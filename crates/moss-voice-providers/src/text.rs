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
    let chars: Vec<char> = speakable_numbers(&input.chars().collect::<Vec<_>>());
    let len = chars.len();
    let mut out = String::with_capacity(input.len() + 16);
    let mut i = 0;
    while i < len {
        let c = chars[i];
        // 百分号：80% / 40到60% -> 百分之80 / 百分之40到60（合成模型遇到 % 会按英文念或直接跳过）
        if matches!(c, '%' | '％') {
            let run: Vec<char> = out
                .chars()
                .rev()
                .take_while(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '到'))
                .collect();
            // 这串“数字 / 小数点 / 到”必须以数字开头（“调到80%”里的“到”不属于它）
            let digits = run
                .iter()
                .rposition(char::is_ascii_digit)
                .map_or(0, |last| last + 1);
            if digits > 0 {
                let at = out
                    .char_indices()
                    .rev()
                    .nth(digits - 1)
                    .map_or(0, |(index, _)| index);
                out.insert_str(at, "百分之");
                i += 1;
                continue;
            }
        }
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

/// 数字里只为了给人看的符号换成念得出来的写法：
/// - 千分位 `1,200` → `1200`（合成模型会把逗号当成停顿，念成“一，二百”）；
/// - 时间 `14:30` → `14点30`、`9:00` → `9点`（冒号同样会被当成停顿）。
///   只认“1–2 位数字 : 2 位数字”且是合法时刻的写法，`16:9`、`1:2` 这类比例不动。
fn speakable_numbers(chars: &[char]) -> Vec<char> {
    let digit = |index: usize| chars.get(index).is_some_and(char::is_ascii_digit);
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ','
            && i > 0
            && digit(i - 1)
            && digit(i + 1)
            && digit(i + 2)
            && digit(i + 3)
            && !digit(i + 4)
        {
            i += 1;
            continue;
        }
        if matches!(c, ':' | '：')
            && i > 0
            && digit(i - 1)
            && digit(i + 1)
            && digit(i + 2)
            && !digit(i + 3)
        {
            let hour_start = if i >= 2 && digit(i - 2) { i - 2 } else { i - 1 };
            let standalone = hour_start == 0 || !digit(hour_start - 1);
            let hour: u32 = chars[hour_start..i]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(99);
            let minute: u32 = chars[i + 1..i + 3]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(99);
            if standalone && hour <= 24 && minute < 60 {
                out.push('点');
                if minute == 0 {
                    i += 3;
                } else {
                    i += 1;
                }
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn is_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '!' | '?' | '\n')
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClauseTailState {
    /// 后续尚未满 `max_speakable` 字且未见句末或新分句停顿：流式 SSE 还在吐字，可暂缓切分观察短尾
    Undecided,
    /// 在 `max_speakable` 字内已见句末标点：跳过当前逗号，留到该句末整句送出
    NearSentenceEnd,
    /// 后续超过 `max_speakable` 字或出现新分句停顿/换行：正常在当前逗号切开
    LongOrMultiClause,
}

/// 观察逗号后的缓冲区尾部状态，用于消除 4~8 字孤立句末短尾（兼容整块推入与逐字 SSE 流式推入）。
fn inspect_clause_tail(rest: &str, max_speakable: usize) -> ClauseTailState {
    let mut speakable_seen = 0usize;
    let mut chars = rest.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\n' || is_clause_pause(ch) {
            return ClauseTailState::LongOrMultiClause;
        }
        if matches!(ch, '。' | '！' | '？' | '!' | '?')
            || (ch == '.' && chars.peek().is_some_and(|next| next.is_whitespace()))
        {
            return if speakable_seen > 0 && speakable_seen <= max_speakable {
                ClauseTailState::NearSentenceEnd
            } else {
                ClauseTailState::LongOrMultiClause
            };
        }
        if ch.is_alphanumeric() {
            speakable_seen += 1;
            if speakable_seen > max_speakable {
                return ClauseTailState::LongOrMultiClause;
            }
        }
    }
    ClauseTailState::Undecided
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D | 0x2B50 | 0x2B55)
}

/// 把一句话切成设备字幕一行放得下的几段（拼回去与原文逐字相同）。
///
/// 官方固件底部字幕默认是单行跑马灯：超过一行的句子要等 1 秒才开始以约 3 字/秒滚动，下一句一到又从头来，
/// 句子后半截根本露不出来。`max_units` 为一行的宽度（半角字符记 1，其余记 2）；优先在标点或空格处断开
/// （断点不早于行宽的四成），标点不落在段首。
pub fn subtitle_segments(text: &str, max_units: usize) -> Vec<String> {
    let unit = |c: char| if c.is_ascii() { 1 } else { 2 };
    let total: usize = text.chars().map(unit).sum();
    if total <= max_units || max_units < 4 {
        return vec![text.to_owned()];
    }
    let limit = max_units;
    let hangs =
        |c: char| is_pause(c) || matches!(c, '、' | '”' | '’' | '）' | ')' | '》' | '」' | ' ');
    let mut out = Vec::new();
    let mut current = String::new();
    let mut width = 0;
    // 当前段里最近一个可断点：（字节长度, 到此为止的宽度）
    let mut last_break: Option<(usize, usize)> = None;
    for c in text.chars() {
        if width + unit(c) > limit && !current.is_empty() && !hangs(c) {
            match last_break.take() {
                Some((at, upto)) if upto * 5 >= width * 2 => {
                    let rest = current.split_off(at);
                    out.push(std::mem::replace(&mut current, rest));
                    width -= upto;
                }
                _ => {
                    out.push(std::mem::take(&mut current));
                    width = 0;
                }
            }
        }
        current.push(c);
        width += unit(c);
        if hangs(c) {
            last_break = Some((current.len(), width));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// 推理模型把思考过程用 `<think>…</think>` 夹在正文里（经 Ollama 等 OpenAI 兼容服务、关不掉思考时常见）：
/// 流式去掉，不念出来、不进历史（n114）。标签可能被拆在两段流式文字里，结尾像半个标签的部分先留着。
#[derive(Debug, Default)]
pub struct ThinkFilter {
    inside: bool,
    pending: String,
}

impl ThinkFilter {
    const OPEN: &'static str = "<think>";
    const CLOSE: &'static str = "</think>";

    /// 送入一段流式文字，返回去掉思考内容后能放出去的部分。
    pub fn push(&mut self, delta: &str) -> String {
        self.pending.push_str(delta);
        let mut out = String::new();
        loop {
            let tag = if self.inside { Self::CLOSE } else { Self::OPEN };
            if let Some(at) = self.pending.find(tag) {
                if !self.inside {
                    out.push_str(&self.pending[..at]);
                }
                self.pending.drain(..at + tag.len());
                self.inside = !self.inside;
                continue;
            }
            // 结尾可能是半个标签（标签全是 ASCII，按字节切不会切坏汉字）
            let keep = (1..tag.len())
                .rev()
                .find(|&n| self.pending.ends_with(&tag[..n]))
                .unwrap_or(0);
            let split = self.pending.len() - keep;
            if !self.inside {
                out.push_str(&self.pending[..split]);
            }
            self.pending.drain(..split);
            return out;
        }
    }

    /// 流结束：留着的半个标签其实是正文就放出来；没闭合的思考内容丢掉。
    pub fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        if self.inside { String::new() } else { rest }
    }
}

/// 一次性去掉整段文字里的 `<think>…</think>`（见 [`ThinkFilter`]）。
pub fn strip_think(text: &str) -> String {
    if !text.contains(ThinkFilter::OPEN) {
        return text.to_owned();
    }
    let mut filter = ThinkFilter::default();
    let mut out = filter.push(text);
    out.push_str(&filter.finish());
    out.trim_start().to_owned()
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
                    // 首句已送出（喇叭正在播放）时，若逗号后还没流够 8 个字就暂缓切分观察；一旦 ≤8 字内即见句号便合并整句送出，
                    // 超过 8 字或遇到新逗号则立即在当前逗号切开。首句未送出时保持 6 字首逗快切，保证冷启动首声延迟最低。
                    if self.sent_any && is_clause_pause(c) {
                        match inspect_clause_tail(&self.buffer[end..], 8) {
                            ClauseTailState::NearSentenceEnd => continue,
                            ClauseTailState::Undecided
                                if self.buffer.chars().count() <= MAX_CHARS =>
                            {
                                return None;
                            }
                            _ => {}
                        }
                    }
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
    #[test]
    fn subtitle_segments_fit_one_line_and_rejoin_to_the_original() {
        assert_eq!(
            subtitle_segments("好的，马上开始。", 28),
            vec!["好的，马上开始。"]
        );
        let text = "椴木三毫米建议用百分之八十功率，速度每分钟六百毫米，分两遍切透，记得打开吹气。";
        let segments = subtitle_segments(text, 28);
        assert_eq!(segments.len(), 3, "{segments:?}");
        assert_eq!(segments.concat(), text);
        for segment in &segments {
            let width: usize = segment
                .chars()
                .map(|c| if c.is_ascii() { 1 } else { 2 })
                .sum();
            // 悬挂标点允许多出一个字
            assert!(width <= 30, "{segment}");
            assert!(!segment.starts_with(['，', '。', '、']), "{segment}");
        }
        // 优先在标点处断开
        assert_eq!(
            subtitle_segments("先把材料放平，再关上防护盖，然后按开始键。", 28),
            vec!["先把材料放平，再关上防护盖，", "然后按开始键。"]
        );
        // 没有标点时按宽度硬切
        let plain = "一二三四五六七八九十一二三四五六七八九十";
        let segments = subtitle_segments(plain, 28);
        assert_eq!(segments.concat(), plain);
        assert_eq!(
            segments
                .iter()
                .map(|s| s.chars().count())
                .collect::<Vec<_>>(),
            vec![14, 6]
        );
        // 英文在空格处断开
        let english = "Laser power is set to eighty percent now";
        assert_eq!(subtitle_segments(english, 28).concat(), english);
    }

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
                "提示：请确认材料、厚度与功率在 百分之40到60 之间，",
                "速度 1200毫米每分钟，木牌尺寸 100乘80毫米，",
                "厚度 3毫米，当前时间 14点30。"
            ]
        );
        // 百分号、千分位、时间写成念得出来的样子；比例与非时刻的冒号不动
        assert_eq!(
            clean_for_speech("功率调到80%，大概9:00开始，画面比例16:9，一共12,000步。"),
            "功率调到百分之80，大概9点开始，画面比例16:9，一共12000步。"
        );
        assert_eq!(clean_for_speech("进度 99.5% 了"), "进度 百分之99.5 了");
        // 2. Markdown 链接与斜体星号被清洗，保留可读正文
        assert_eq!(
            clean_for_speech("查看 [材料库说明](https://example.com/docs) 中的 *3cm* 焦距。"),
            "查看 材料库说明 中的 3厘米 焦距。"
        );
        // 3. 首句已开播后，后续复合句末尾紧跟 ≤8 字短收尾时不从逗号硬拆出孤立尾巴（含整块推入与逐字模拟真实 SSE 流式到达）
        let smooth = run(&[
            "三毫米椴木刻字把握较大。",
            "最需要注意的是切不透，建议先确认对焦，速度别太快，必要时走两遍。",
        ]);
        assert_eq!(
            smooth,
            vec![
                "三毫米椴木刻字把握较大。",
                "最需要注意的是切不透，建议先确认对焦，速度别太快，必要时走两遍。"
            ]
        );
        let sse_tokens: Vec<String> =
            "收到，激光机空闲。最需要注意的是切不透，建议先确认对焦，速度别太快，必要时走两遍。"
                .chars()
                .map(|ch| ch.to_string())
                .collect();
        let sse_refs: Vec<&str> = sse_tokens.iter().map(String::as_str).collect();
        assert_eq!(
            run(&sse_refs),
            vec![
                "收到，激光机空闲。",
                "最需要注意的是切不透，建议先确认对焦，速度别太快，必要时走两遍。"
            ]
        );
    }

    #[test]
    fn think_blocks_are_removed_even_when_tags_are_split_across_chunks() {
        let mut filter = ThinkFilter::default();
        let mut out = String::new();
        for chunk in [
            "<th",
            "ink>用户想",
            "知道状态</thi",
            "nk>好，",
            "现在空闲。",
        ] {
            out.push_str(&filter.push(chunk));
        }
        out.push_str(&filter.finish());
        assert_eq!(out, "好，现在空闲。");
        // 没有标签的正文原样放出，结尾的“<”不是标签时流结束照样放出
        let mut plain = ThinkFilter::default();
        assert_eq!(plain.push("一加一等于二，"), "一加一等于二，");
        assert_eq!(plain.push("a<"), "a");
        assert_eq!(plain.finish(), "<");
        // 没闭合的思考内容不念
        let mut open = ThinkFilter::default();
        assert_eq!(open.push("<think>还在想"), "");
        assert_eq!(open.finish(), "");
        assert_eq!(strip_think("<think>想一想</think>\n好的。"), "好的。");
        assert_eq!(strip_think("好的。"), "好的。");
    }
}
