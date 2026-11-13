//! 大模型输出 → 合成用的句子。
//!
//! 规则兼顾首响延迟、播放连续性与自然韵律（合并 xiaozhi-esp32-server `7a260ee` 与海门 `513d9e6` 并消除碎句断流）：
//! - 首句在停顿标点（逗号、分号、冒号等）满 6 个可读字才切，避免“好的，”“收到，”等 2 字碎句播完即断流；句末标点满 2 字即切（流结束时任意长度兜底冲刷）；
//! - 后续句子在句末标点（满 2 字）切分，长复合句满 18 个可读字遇到逗号/分号/冒号也按自然气口切分，避免死等句号拖成大段空白；
//! - 英文句点只在后面跟空白时算句末（避免把 `3.5` 切开）；
//! - 累计超过 180 字，或从这一句第一个字算起超过 6 秒，在最后一个标点处（没有就整段）强制送出；
//! - 送出前清洗 Markdown 与表情，没有可读内容的片段丢弃。

use std::time::{Duration, Instant};

const MAX_CHARS: usize = 180;
const MAX_WAIT: Duration = Duration::from_secs(6);
/// 首句在逗号等非句末停顿处切分的默认最小可读字数（约 1.3~1.6 秒音频，足以掩盖下一句预取耗时）。
pub const DEFAULT_FIRST_PAUSE_MIN_CHARS: usize = 6;
/// 首句在句末标点（。！？）切分的默认最小可读字数。
const DEFAULT_FIRST_END_MIN_CHARS: usize = 2;
/// 后续句子在句末标点切分的默认最小可读字数（合并“嗯。”“对！”等单字感叹）。
pub const DEFAULT_NEXT_END_MIN_CHARS: usize = 2;
/// 后续长复合句在逗号/分号/冒号处按气口切分的默认最小可读字数（约 3.8 秒语音，自然换气不拖沓）。
pub const DEFAULT_CLAUSE_SPLIT_CHARS: usize = 18;

fn is_pause(c: char) -> bool {
    is_clause_pause(c) || c == '…' || is_end(c)
}

fn is_clause_pause(c: char) -> bool {
    matches!(c, '，' | ',' | '；' | ';' | '：' | ':')
}

fn is_cjk(c: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

/// 将 Markdown 行内链接 `[文字](url)` 还原为纯 `文字`。
fn strip_markdown_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(close) = after_open.find("](") {
            let label = &after_open[..close];
            let after_mid = &after_open[close + 2..];
            if let Some(end_paren) = after_mid.find(')') {
                out.push_str(label);
                rest = &after_mid[end_paren + 1..];
                continue;
            }
        }
        out.push('[');
        rest = after_open;
    }
    out.push_str(rest);
    out
}

/// 将中文语境下的加工参数符号（范围 `~`、乘号 `×`、速度/尺寸单位 `mm/min`、`mm/s`、`mm`、`cm`）转成顺畅口语。
fn normalize_spoken_terms(input: &str) -> String {
    if !input.chars().any(is_cjk) {
        return input.to_owned();
    }
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(input.len() + 16);
    let mut i = 0;
    while i < len {
        let c = chars[i];
        // 数字范围：40~60% / 3～5 -> 40到60% / 3到5
        if matches!(c, '~' | '～') {
            let prev_ok = i > 0 && (chars[i - 1].is_ascii_digit() || chars[i - 1] == '%');
            let next_ok = i + 1 < len && chars[i + 1].is_ascii_digit();
            if prev_ok && next_ok {
                out.push('到');
                i += 1;
                continue;
            }
        }
        // 尺寸乘号：100×80 -> 100乘80
        if c == '×'
            && i > 0
            && i + 1 < len
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit()
        {
            out.push('乘');
            i += 1;
            continue;
        }
        // 紧跟在数字或空格后的单位：mm/min、mm/s、mm、cm
        let prev_is_num = i > 0
            && (chars[i - 1].is_ascii_digit()
                || (chars[i - 1] == ' ' && i >= 2 && chars[i - 2].is_ascii_digit()));
        if prev_is_num
            && (c == 'm' || c == 'M')
            && i + 1 < len
            && (chars[i + 1] == 'm' || chars[i + 1] == 'M')
        {
            let tail: String = chars[i + 2..].iter().take(5).collect();
            let tail_lower = tail.to_ascii_lowercase();
            if tail_lower.starts_with("/min")
                && chars.get(i + 6).is_none_or(|ch| !ch.is_ascii_alphabetic())
            {
                if out.ends_with(' ') {
                    out.pop();
                }
                out.push_str("毫米每分钟");
                i += 6;
                continue;
            }
            if tail_lower.starts_with("/s")
                && chars.get(i + 4).is_none_or(|ch| !ch.is_ascii_alphabetic())
            {
                if out.ends_with(' ') {
                    out.pop();
                }
                out.push_str("毫米每秒");
                i += 4;
                continue;
            }
            if chars
                .get(i + 2)
                .is_none_or(|ch| !ch.is_ascii_alphabetic() && *ch != '/')
            {
                if out.ends_with(' ') {
                    out.pop();
                }
                out.push_str("毫米");
                i += 2;
                continue;
            }
        }
        if prev_is_num
            && (c == 'c' || c == 'C')
            && i + 1 < len
            && (chars[i + 1] == 'm' || chars[i + 1] == 'M')
            && chars
                .get(i + 2)
                .is_none_or(|ch| !ch.is_ascii_alphabetic() && *ch != '/')
        {
            if out.ends_with(' ') {
                out.pop();
            }
            out.push_str("厘米");
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn is_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '!' | '?' | '\n')
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D | 0x2B50 | 0x2B55)
}

/// 首个表情对应的设备表情名（固件支持的名称）。
pub fn emotion_of(text: &str) -> Option<&'static str> {
    text.chars()
        .find(|c| is_emoji(*c) && *c != '\u{FE0F}' && *c != '\u{200D}')
        .map(|c| match c {
            '😂' | '🤣' | '😆' => "laughing",
            '😊' | '😄' | '😁' | '🙂' | '😀' | '😃' => "happy",
            '😢' | '😞' | '😔' | '☹' => "sad",
            '😭' => "crying",
            '😠' | '😡' => "angry",
            '😍' | '🥰' | '❤' | '💕' => "loving",
            '😳' | '😅' => "embarrassed",
            '😮' | '😯' => "surprised",
            '😱' => "shocked",
            '🤔' => "thinking",
            '😉' => "winking",
            '😎' => "cool",
            '😌' => "relaxed",
            '😋' => "delicious",
            '😘' => "kissy",
            '😏' => "confident",
            '😴' => "sleepy",
            '🤪' | '😜' => "silly",
            '😕' | '😵' => "confused",
            _ => "happy",
        })
}

/// 清洗为可朗读的文字：去掉 Markdown 标记（加粗、代码、标题、列表、引用、行内链接）与表情，并将中文加工参数符号转为口语表达。
pub fn clean_for_speech(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let mut line = line.trim();
        line = line.trim_start_matches('#').trim_start();
        line = line.trim_start_matches('>').trim_start();
        for marker in ["- ", "* ", "+ "] {
            if let Some(rest) = line.strip_prefix(marker) {
                line = rest;
            }
        }
        let without_links = strip_markdown_links(line);
        let cleaned: String = without_links
            .replace("**", "")
            .replace("__", "")
            .chars()
            .filter(|c| *c != '`' && *c != '*' && !is_emoji(*c))
            .collect();
        let cleaned = normalize_spoken_terms(&cleaned);
        let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        if !cleaned.is_empty() {
            lines.push(cleaned);
        }
    }
    lines
        .join("，")
        .trim_start_matches(|c: char| is_pause(c) || c == '、' || c.is_whitespace())
        .to_owned()
}

/// 是否有可读内容（字母、数字或汉字）。
pub fn speakable(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

/// 统计可读字符数（字母、数字或汉字，不含标点与空白），用于碎句防抖合并（TTS Coalesce）。
pub fn speakable_char_count(text: &str) -> usize {
    text.chars().filter(|c| c.is_alphanumeric()).count()
}

/// 流式切句器（含极短碎句防抖合并与长复合句气口切分）。
pub struct Segmenter {
    buffer: String,
    sent_any: bool,
    since: Option<Instant>,
    min_first_chars: usize,
    min_first_end_chars: usize,
    min_next_chars: usize,
    clause_split_chars: usize,
}

impl Default for Segmenter {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            sent_any: false,
            since: None,
            min_first_chars: DEFAULT_FIRST_PAUSE_MIN_CHARS,
            min_first_end_chars: DEFAULT_FIRST_END_MIN_CHARS,
            min_next_chars: DEFAULT_NEXT_END_MIN_CHARS,
            clause_split_chars: DEFAULT_CLAUSE_SPLIT_CHARS,
        }
    }
}

impl Segmenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 自定义首句与后续句子的最小可读字符合并阈值（不足阈值时暂存与后句合并，流结束 `finish` 时兜底冲刷）。
    pub fn with_min_chars(mut self, first_min: usize, next_min: usize) -> Self {
        let first = first_min.max(1);
        self.min_first_chars = first;
        self.min_first_end_chars = first.min(DEFAULT_FIRST_END_MIN_CHARS);
        self.min_next_chars = next_min.max(1);
        self
    }

    /// 自定义后续长复合句在逗号/分号/冒号处按气口切分的最小可读字符阈值。
    pub fn with_clause_chars(mut self, clause_min: usize) -> Self {
        self.clause_split_chars = clause_min.max(1);
        self
    }

    /// 送入一段增量，返回可以送合成的句子（已清洗）。
    pub fn push(&mut self, delta: &str, now: Instant) -> Vec<String> {
        if self.buffer.is_empty() && !delta.is_empty() {
            self.since = Some(now);
        }
        self.buffer.push_str(delta);
        let mut out = Vec::new();
        while let Some(cut) = self.find_cut() {
            self.emit(cut, now, &mut out);
        }
        let waited = self
            .since
            .map(|since| now.duration_since(since) >= MAX_WAIT)
            .unwrap_or(false);
        if !self.buffer.is_empty() && (self.buffer.chars().count() > MAX_CHARS || waited) {
            let cut = self.last_pause().unwrap_or(self.buffer.len());
            self.emit(cut, now, &mut out);
        }
        out
    }

    /// 上游结束：送出剩余内容。
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buffer);
        self.since = None;
        let cleaned = clean_for_speech(&rest);
        speakable(&cleaned).then_some(cleaned)
    }

    fn emit(&mut self, cut: usize, now: Instant, out: &mut Vec<String>) {
        let piece: String = self.buffer.drain(..cut).collect();
        self.since = if self.buffer.is_empty() {
            None
        } else {
            Some(now)
        };
        let cleaned = clean_for_speech(&piece);
        if speakable(&cleaned) {
            self.sent_any = true;
            out.push(cleaned);
        }
    }

    /// 下一个切点（字节位置，含标点本身）。
    fn find_cut(&self) -> Option<usize> {
        let mut chars = self.buffer.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            let end = at + c.len_utf8();
            let (boundary, min_chars) = if self.sent_any {
                if is_end(c) {
                    (true, self.min_next_chars)
                } else if is_clause_pause(c) {
                    (true, self.clause_split_chars.max(self.min_next_chars))
                } else {
                    (false, self.min_next_chars)
                }
            } else if is_end(c) {
                (true, self.min_first_end_chars)
            } else if is_pause(c) {
                let first_pause_min = if matches!(c, '：' | ':') {
                    self.min_first_chars.max(6)
                } else {
                    self.min_first_chars
                };
                (true, first_pause_min)
            } else {
                (false, self.min_first_chars)
            };
            if boundary {
                // 保护数字千分位（1,200）与时间（14:30）：紧跟在数字后的半角逗号/冒号若后续是数字或尚未收齐则不切
                if matches!(c, ',' | ':')
                    && self.buffer[..at].ends_with(|ch: char| ch.is_ascii_digit())
                    && chars.peek().is_none_or(|(_, next)| next.is_ascii_digit())
                {
                    continue;
                }
                // 有足够可读内容才切（避免开头的孤立标点或句首/句中 1~2 字碎句单独发起 TTS 导致播完断流；换行符保持分行）
                let count = speakable_char_count(&self.buffer[..end]);
                if count >= min_chars
                    || (c == '\n' && count >= 1)
                    || (c == '\n' && !self.buffer[..at].trim().is_empty())
                {
                    return Some(end);
                }
                if count == 0 && c == '\n' {
                    return Some(end);
                }
                continue;
            }
            if c == '.' {
                let period_min = if self.sent_any {
                    self.min_next_chars
                } else {
                    self.min_first_end_chars
                };
                match chars.peek() {
                    Some((_, next))
                        if next.is_whitespace()
                            && speakable_char_count(&self.buffer[..end]) >= period_min =>
                    {
                        return Some(end);
                    }
                    _ => {}
                }
            }
        }
        None
    }

    fn last_pause(&self) -> Option<usize> {
        self.buffer
            .char_indices()
            .rev()
            .find(|&(at, c)| {
                if !is_pause(c) {
                    return false;
                }
                if matches!(c, ',' | ':')
                    && self.buffer[..at].ends_with(|ch: char| ch.is_ascii_digit())
                    && self.buffer[at + c.len_utf8()..]
                        .chars()
                        .next()
                        .is_none_or(|next| next.is_ascii_digit())
                {
                    return false;
                }
                true
            })
            .map(|(at, c)| at + c.len_utf8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(deltas: &[&str]) -> Vec<String> {
        let mut segmenter = Segmenter::new();
        let now = Instant::now();
        let mut out = Vec::new();
        for delta in deltas {
            out.extend(segmenter.push(delta, now));
        }
        out.extend(segmenter.finish());
        out
    }

    #[test]
    fn first_sentence_coalesces_short_interjection_and_cuts_at_substantial_clause() {
        // 首句“好的，”仅 2 字，不再单独切出导致 180ms 播完断流，而是合并至满 6 字的自然停顿“好的，我来帮你看看，”
        let out = run(&[
            "好的",
            "，我来",
            "帮你看看，",
            "激光机现在空闲。",
            "温度正常！还有别的吗",
        ]);
        assert_eq!(
            out,
            vec![
                "好的，我来帮你看看，",
                "激光机现在空闲。",
                "温度正常！",
                "还有别的吗"
            ]
        );
    }

    #[test]
    fn subsequent_long_compound_sentence_splits_at_clause_pause_without_waiting_for_period() {
        let out = run(&[
            "收到，激光雕刻机目前处于空闲状态，",
            "工作台已经完成归零并且排风系统运转正常，随时可以开始下一步雕刻任务。",
        ]);
        assert_eq!(
            out,
            vec![
                "收到，激光雕刻机目前处于空闲状态，",
                "工作台已经完成归零并且排风系统运转正常，",
                "随时可以开始下一步雕刻任务。"
            ]
        );
    }

    #[test]
    fn english_period_needs_following_space() {
        let out = run(&["Power is 3.5 watts", ". Speed is 1000", ". Done"]);
        assert_eq!(out, vec!["Power is 3.5 watts.", "Speed is 1000.", "Done"]);
    }

    #[test]
    fn markdown_and_emoji_are_cleaned() {
        let out = run(&["😊 **好的**，", "\n## 步骤\n- 打开 `电源`\n- 放好材料。"]);
        // 换行算句末：每行各成一句
        assert_eq!(out, vec!["好的，", "步骤", "打开 电源", "放好材料。"]);
        assert_eq!(emotion_of("好的😊"), Some("happy"));
        assert_eq!(emotion_of("嗯🤔"), Some("thinking"));
        assert_eq!(emotion_of("没有表情"), None);
        assert!(!speakable("，。！ 😊"));
    }

    #[test]
    fn long_text_is_forced_out() {
        let long = "很".repeat(200);
        let mut segmenter = Segmenter::new().with_min_chars(1, 2);
        let now = Instant::now();
        assert_eq!(segmenter.push("好，", now), vec!["好，"]);
        let out = segmenter.push(&format!("一二三，{long}"), now);
        assert_eq!(out.len(), 1, "超过 180 字强制送出");
        assert_eq!(out[0], "一二三，", "在最后一个标点处切");
        let out = segmenter.push("", now);
        assert_eq!(out.len(), 1, "剩下的 200 字没有标点，整段送出");
        assert!(segmenter.finish().is_none());
    }

    #[test]
    fn slow_stream_is_forced_out_after_six_seconds() {
        let mut segmenter = Segmenter::new().with_min_chars(1, 2);
        let start = Instant::now();
        assert_eq!(segmenter.push("嗯，", start), vec!["嗯，"]);
        assert!(segmenter.push("让我想一想这个问题", start).is_empty());
        let out = segmenter.push("的答案", start + Duration::from_secs(7));
        assert_eq!(out, vec!["让我想一想这个问题的答案"]);
    }

    #[test]
    fn punctuation_only_pieces_are_dropped() {
        assert_eq!(
            run(&["，。", "你好。", "……"]),
            vec!["你好。"],
            "开头的孤立标点被去掉"
        );
        assert!(run(&["😊"]).is_empty());
    }

    #[test]
    fn short_mid_stream_fragments_coalesce_with_following_clause() {
        // 首句满 6 字切出保证连续缓冲；后续仅有 1 个可读字的碎句（“嗯。”、“对！”）自动与下一句合并，避免浪费一次 TTS 往返
        let out = run(&[
            "好的，马上检查，",
            "嗯。",
            "激光机目前处于空闲状态。",
            "对！",
            "随时可以开始。",
        ]);
        assert_eq!(
            out,
            vec![
                "好的，马上检查，",
                "嗯。激光机目前处于空闲状态。",
                "对！随时可以开始。"
            ]
        );

        // 自定义合并阈值（如首句至少 4 字、后续至少 6 字）：不足阈值的碎句自动合并，流结束兜底冲刷
        let mut custom = Segmenter::new().with_min_chars(4, 6);
        let now = Instant::now();
        assert!(custom.push("好，", now).is_empty(), "不足 4 字暂存不切");
        assert_eq!(
            custom.push("马上为您查询，", now),
            vec!["好，马上为您查询，"]
        );
        assert!(custom.push("正常。", now).is_empty(), "后续不足 6 字暂存");
        assert_eq!(
            custom.push("可以开始雕刻。", now),
            vec!["正常。可以开始雕刻。"]
        );
        assert_eq!(custom.push("尾句", now), Vec::<String>::new());
        assert_eq!(custom.finish().as_deref(), Some("尾句"));
    }

    #[test]
    fn ranges_lists_colons_and_laser_units_stay_fluent() {
        // 1. 提示前缀、顿号并列词、数值范围与时间不被首句逗号切碎；超长参数串在满 22 字的自然逗号处分段避免长句死等
        let out = run(&[
            "提示：",
            "请确认材料、厚度与功率在 40~60% 之间，",
            "速度 1,200mm/min，木牌尺寸 100×80mm，厚度 3mm，当前时间 14:30。",
        ]);
        assert_eq!(
            out,
            vec![
                "提示：请确认材料、厚度与功率在 40到60% 之间，",
                "速度 1,200毫米每分钟，木牌尺寸 100乘80毫米，",
                "厚度 3毫米，当前时间 14:30。"
            ]
        );
        // 2. Markdown 链接与斜体星号被清洗，保留可读正文
        assert_eq!(
            clean_for_speech("查看 [材料库说明](https://example.com/docs) 中的 *3cm* 焦距。"),
            "查看 材料库说明 中的 3厘米 焦距。"
        );
    }
}
