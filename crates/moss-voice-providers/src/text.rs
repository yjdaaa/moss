//! 大模型输出 → 合成用的句子。
//!
//! 规则合并 xiaozhi-esp32-server（`7a260ee`，首句遇逗号即切，降低首包延迟）与海门
//! `src/xiaozhi_asr_llm_tts.rs`（`513d9e6`，清洗 Markdown，超过 180 字或 6 秒强制送出）：
//! - 第一句遇到任何停顿标点（含逗号）就切；之后只在句末标点处切；
//! - 英文句点只在后面跟空白时算句末（避免把 `3.5` 切开）；
//! - 累计超过 180 字，或从这一句第一个字算起超过 6 秒，在最后一个标点处（没有就整段）强制送出；
//! - 送出前清洗 Markdown 与表情，没有可读内容的片段丢弃。

use std::time::{Duration, Instant};

const MAX_CHARS: usize = 180;
const MAX_WAIT: Duration = Duration::from_secs(6);

fn is_pause(c: char) -> bool {
    matches!(c, '，' | ',' | '、' | '；' | ';' | '：' | ':' | '…' | '~' | '～') || is_end(c)
}

fn is_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '!' | '?' | '\n')
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D | 0x2B50 | 0x2B55)
}

/// 首个表情对应的设备表情名（固件支持的名称）。
pub fn emotion_of(text: &str) -> Option<&'static str> {
    text.chars().find(|c| is_emoji(*c) && *c != '\u{FE0F}' && *c != '\u{200D}').map(|c| match c {
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

/// 清洗为可朗读的文字：去掉 Markdown 标记（加粗、代码、标题、列表、引用）与表情，合并空白。
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
        let cleaned: String = line
            .replace("**", "")
            .replace("__", "")
            .chars()
            .filter(|c| *c != '`' && !is_emoji(*c))
            .collect();
        let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        if !cleaned.is_empty() {
            lines.push(cleaned);
        }
    }
    lines.join("，").trim_start_matches(|c: char| is_pause(c) || c.is_whitespace()).to_owned()
}

/// 是否有可读内容（字母、数字或汉字）。
pub fn speakable(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

/// 流式切句器。
#[derive(Default)]
pub struct Segmenter {
    buffer: String,
    sent_any: bool,
    since: Option<Instant>,
}

impl Segmenter {
    pub fn new() -> Self {
        Self::default()
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
        let waited = self.since.map(|since| now.duration_since(since) >= MAX_WAIT).unwrap_or(false);
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
        self.since = if self.buffer.is_empty() { None } else { Some(now) };
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
            let boundary = if self.sent_any { is_end(c) } else { is_pause(c) };
            if boundary {
                // 有内容才切（避免开头的孤立标点单独成句）
                if speakable(&self.buffer[..end]) || c == '\n' {
                    return Some(end);
                }
                continue;
            }
            if c == '.' {
                match chars.peek() {
                    Some((_, next)) if next.is_whitespace() => return Some(end),
                    _ => {}
                }
            }
        }
        None
    }

    fn last_pause(&self) -> Option<usize> {
        self.buffer.char_indices().rev().find(|(_, c)| is_pause(*c)).map(|(at, c)| at + c.len_utf8())
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
    fn first_sentence_cuts_at_comma_then_at_sentence_end() {
        let out = run(&["好的", "，我来", "帮你看看，", "激光机现在空闲。", "温度正常！还有别的吗"]);
        assert_eq!(out, vec!["好的，", "我来帮你看看，激光机现在空闲。", "温度正常！", "还有别的吗"]);
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
        let mut segmenter = Segmenter::new();
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
        let mut segmenter = Segmenter::new();
        let start = Instant::now();
        assert_eq!(segmenter.push("嗯，", start), vec!["嗯，"]);
        assert!(segmenter.push("让我想一想这个问题", start).is_empty());
        let out = segmenter.push("的答案", start + Duration::from_secs(7));
        assert_eq!(out, vec!["让我想一想这个问题的答案"]);
    }

    #[test]
    fn punctuation_only_pieces_are_dropped() {
        assert_eq!(run(&["，。", "你好。", "……"]), vec!["你好。"], "开头的孤立标点被去掉");
        assert!(run(&["😊"]).is_empty());
    }
}
