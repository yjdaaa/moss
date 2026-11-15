//! 被打断时用户实际听到了哪半句（2026-11-15 n100）。
//!
//! 官方小智固件不回报播放进度。服务端按绝对时钟每 60 ms 发一帧、设备只多缓冲几帧，
//! 所以“已发出的帧数 − 设备里还没播的帧数”就是实际播到的位置（误差约一个缓冲余量，≈0.3 秒）。
//! 每段字幕文字记下它从第几帧开始播；正在播的那段按已播帧数占这段总帧数的比例截断文字。
//! 打断时只把真正播出来的部分记进历史，没说出口的单独标出，供下一轮“捡回来接着说”。

use crate::conversation::INTERRUPTED_MARK;

/// 估算每个字的播放时长（与字幕时间轴的约 5.5 字/秒一致，略快于实际语速，宁可少算）。
const MS_PER_CHAR: u32 = 182;
/// 没说出口的内容最多记这么多字（够大模型接上话头，不把整段长回答塞进历史）。
const MAX_UNSAID_CHARS: usize = 80;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct HeardReply {
    /// 用户实际听到的文字
    pub heard: String,
    /// 最后一段只听到一半
    pub cut: bool,
    /// 已经合成或已经发出、但还没播出来的文字
    pub unsaid: String,
}

/// 算出实际听到的部分。
/// - `pieces` / `starts`：已发出的各段文字与各自开始的帧序号（一一对应、不减）；
/// - `frames_sent`：这一轮已发出的帧数；`unplayed`：设备里还没播的帧数；
/// - `tail_frames`：正在播的最后一段在播放泵里还没发出的帧数；`tail_complete`：这段音频已全部推入
///   （否则流式合成还在出，按字数估算它至少多长）；
/// - `pending`：还在播放泵里、一帧都没发出的后续各段文字。
pub(super) fn heard_reply(
    pieces: &[String],
    starts: &[u32],
    frames_sent: u32,
    unplayed: u32,
    tail_frames: u32,
    tail_complete: bool,
    pending: &[String],
) -> HeardReply {
    let played = frames_sent.saturating_sub(unplayed);
    let mut out = HeardReply::default();
    for (index, (text, &start)) in pieces.iter().zip(starts).enumerate() {
        let chars = text.chars().count() as u32;
        let end = match starts.get(index + 1) {
            Some(&next) => next,
            // 最后一段：已发出的 + 泵里还没发的；合成还没出完时至少按字数估算的时长
            None if tail_complete => frames_sent + tail_frames,
            None => (frames_sent + tail_frames).max(start + estimated_frames(text)),
        };
        if played >= end {
            out.heard.push_str(text);
        } else if played <= start || out.cut {
            out.unsaid.push_str(text);
        } else {
            let keep = (chars * (played - start) / (end - start).max(1)) as usize;
            out.heard.extend(text.chars().take(keep));
            out.unsaid.extend(text.chars().skip(keep));
            // 一个字都还没播完时整段算没说出口
            out.cut = keep > 0;
        }
    }
    for text in pending {
        out.unsaid.push_str(text);
    }
    out
}

/// 按字数估算一段话要播几帧（只数文字，不数标点）。
fn estimated_frames(text: &str) -> u32 {
    let chars = text.chars().filter(|c| c.is_alphanumeric()).count() as u32;
    (chars * MS_PER_CHAR).div_ceil(super::FRAME_MS as u32)
}

/// 记进历史的助手话：只有实际听到的部分；被截断或还有没说的，在后面用标记写明没说出口的内容。
/// 什么都没播出来也没有待说内容时返回 `None`（不记助手消息）。
pub(super) fn history_text(reply: &HeardReply) -> Option<String> {
    let unsaid = reply.unsaid.trim();
    if reply.heard.is_empty() && unsaid.is_empty() {
        return None;
    }
    let mut text = reply.heard.clone();
    if reply.cut {
        text.push_str("……");
    }
    if !unsaid.is_empty() {
        let mut short: String = unsaid.chars().take(MAX_UNSAID_CHARS).collect();
        if unsaid.chars().count() > MAX_UNSAID_CHARS {
            short.push_str("……");
        }
        text.push_str(&format!("{INTERRUPTED_MARK}，没说出口的：{short}】"));
    } else if reply.cut {
        text.push_str(&format!("{INTERRUPTED_MARK}】"));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|text| (*text).to_owned()).collect()
    }

    #[test]
    fn only_the_part_that_actually_came_out_of_the_speaker_counts_as_heard() {
        // 两段各 10 帧，已发 15 帧但设备里还有 5 帧没播：第一段播完，第二段一个字也没播完
        let pieces = owned(&["好的，激光机", "现在空闲。"]);
        let reply = heard_reply(&pieces, &[0, 10], 15, 5, 5, true, &owned(&["要我对焦吗？"]));
        assert_eq!(reply.heard, "好的，激光机");
        assert!(!reply.cut);
        assert_eq!(reply.unsaid, "现在空闲。要我对焦吗？");
        assert_eq!(
            history_text(&reply).unwrap(),
            "好的，激光机【被打断，没说出口的：现在空闲。要我对焦吗？】"
        );
    }

    #[test]
    fn a_sentence_cut_in_the_middle_keeps_the_heard_half() {
        // 一段 10 个字、20 帧，播到第 10 帧（一半）
        let pieces = owned(&["一二三四五六七八九十"]);
        let reply = heard_reply(&pieces, &[0], 12, 2, 8, true, &[]);
        assert_eq!(reply.heard, "一二三四五");
        assert!(reply.cut);
        assert_eq!(reply.unsaid, "六七八九十");
        assert_eq!(
            history_text(&reply).unwrap(),
            "一二三四五……【被打断，没说出口的：六七八九十】"
        );
    }

    #[test]
    fn a_fully_played_answer_is_recorded_as_is() {
        let pieces = owned(&["好的。", "已经暂停。"]);
        let reply = heard_reply(&pieces, &[0, 4], 12, 0, 0, true, &[]);
        assert_eq!(history_text(&reply).unwrap(), "好的。已经暂停。");
    }

    #[test]
    fn nothing_played_and_nothing_pending_records_no_assistant_message() {
        assert_eq!(
            history_text(&heard_reply(&[], &[], 0, 0, 0, true, &[])),
            None
        );
        // 还没开口就被打断，但下一句已经合成好：只记没说出口的
        let reply = heard_reply(&[], &[], 0, 0, 0, true, &owned(&["我来查一下。"]));
        assert_eq!(
            history_text(&reply).unwrap(),
            "【被打断，没说出口的：我来查一下。】"
        );
    }

    #[test]
    fn the_last_piece_is_never_shorter_than_its_text_needs() {
        // 流式合成还在出：最后一段才刚发了 2 帧、泵里也没有更多，按 6 个字约 1.1 秒估算，不会算成已播完
        let pieces = owned(&["六个字的句子"]);
        let reply = heard_reply(&pieces, &[0], 2, 0, 0, false, &[]);
        assert!(reply.heard.chars().count() < 6, "{reply:?}");
        // 这段已全部推入并发完：按实际帧数算，已经播完
        let reply = heard_reply(&pieces, &[0], 2, 0, 0, true, &[]);
        assert_eq!(reply.heard, "六个字的句子");
    }

    #[test]
    fn long_unsaid_text_is_capped() {
        let long = "很".repeat(200);
        let reply = heard_reply(&[], &[], 0, 0, 0, true, &[long]);
        let text = history_text(&reply).unwrap();
        assert!(text.chars().count() < 100, "{text}");
        assert!(text.ends_with("……】"));
    }
}
