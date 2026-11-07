//! 日志脱敏（技术书 §9.4.3，验收 A-66）：形如密钥的字段值替换为 `***`。
//!
//! 识别 `api_key` / `api-key` / `apikey` / `authorization` / `token` / `access_token` /
//! `secret` / `password` 后紧跟（可带引号的）`:` 或 `=` 的写法，以及 `Bearer` / `Basic` 前缀；
//! 只替换值，保留字段名，便于排查。`tokens_used` 这类以关键词开头但不是该字段的名字不受影响。

const MARKERS: [&str; 8] = [
    "access_token",
    "api_key",
    "api-key",
    "apikey",
    "authorization",
    "password",
    "secret",
    "token",
];

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn is_value_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | ',' | '&' | '}' | ']' | ')' | ';')
}

/// 对一行文本脱敏。
pub fn redact_line(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let lower: Vec<char> = line.to_lowercase().chars().collect();
    if lower.len() != chars.len() {
        // 极少数字符小写后长度变化：按原样逐字匹配 ASCII 关键词即可
        return redact_ascii_case(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    while index < chars.len() {
        if let Some((value_start, value_end)) = match_standalone_auth(&chars, &lower, index) {
            out.extend(&chars[index..value_start]);
            out.push_str("***");
            index = value_end;
            continue;
        }
        if let Some((value_start, value_end)) = match_marker(&chars, &lower, index) {
            out.extend(&chars[index..value_start]);
            out.push_str("***");
            index = value_end;
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn redact_ascii_case(line: &str) -> String {
    let lower: String = line.chars().map(|c| c.to_ascii_lowercase()).collect();
    let chars: Vec<char> = line.chars().collect();
    let lower: Vec<char> = lower.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    while index < chars.len() {
        if let Some((value_start, value_end)) = match_standalone_auth(&chars, &lower, index) {
            out.extend(&chars[index..value_start]);
            out.push_str("***");
            index = value_end;
            continue;
        }
        if let Some((value_start, value_end)) = match_marker(&chars, &lower, index) {
            out.extend(&chars[index..value_start]);
            out.push_str("***");
            index = value_end;
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// Also redact bearer/basic authorization headers emitted without a field name.
fn match_standalone_auth(chars: &[char], lower: &[char], index: usize) -> Option<(usize, usize)> {
    if index > 0 && is_name_char(chars[index - 1]) {
        return None;
    }
    for prefix in ["bearer ", "basic "] {
        let mark: Vec<char> = prefix.chars().collect();
        let end = index + mark.len();
        if end > lower.len() || lower[index..end] != mark[..] {
            continue;
        }
        let mut cursor = end;
        while cursor < chars.len() && chars[cursor] == ' ' {
            cursor += 1;
        }
        let start = cursor;
        while cursor < chars.len() && !is_value_end(chars[cursor]) {
            cursor += 1;
        }
        if cursor > start && !chars[start..cursor].iter().all(|c| *c == '*') {
            return Some((start, cursor));
        }
    }
    None
}

/// 在 `index` 处匹配一个关键词；返回需要替换的值区间（字符下标）。
fn match_marker(chars: &[char], lower: &[char], index: usize) -> Option<(usize, usize)> {
    if index > 0
        && is_name_char(chars[index - 1])
        && chars[index - 1] != '_'
        && chars[index - 1] != '-'
    {
        return None;
    }
    for marker in MARKERS {
        let marker: Vec<char> = marker.chars().collect();
        let end = index + marker.len();
        if end > lower.len() || lower[index..end] != marker[..] {
            continue;
        }
        // 关键词之后不能紧跟名字字符（排除 tokens_used、tokenizer 等）
        let mut cursor = end;
        if cursor < chars.len() && is_name_char(chars[cursor]) {
            continue;
        }
        while cursor < chars.len() && matches!(chars[cursor], '"' | '\'') {
            cursor += 1;
        }
        while cursor < chars.len() && chars[cursor] == ' ' {
            cursor += 1;
        }
        if cursor >= chars.len() || !matches!(chars[cursor], ':' | '=') {
            continue;
        }
        cursor += 1;
        while cursor < chars.len() && matches!(chars[cursor], ' ' | '"' | '\'') {
            cursor += 1;
        }
        // Bearer / Basic 前缀保留
        for scheme in ["bearer ", "basic "] {
            let scheme: Vec<char> = scheme.chars().collect();
            let scheme_end = cursor + scheme.len();
            if scheme_end <= lower.len() && lower[cursor..scheme_end] == scheme[..] {
                cursor = scheme_end;
                while cursor < chars.len() && chars[cursor] == ' ' {
                    cursor += 1;
                }
                break;
            }
        }
        let value_start = cursor;
        while cursor < chars.len() && !is_value_end(chars[cursor]) {
            cursor += 1;
        }
        if cursor == value_start || chars[value_start..cursor].iter().all(|c| *c == '*') {
            return None;
        }
        return Some((value_start, cursor));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::redact_line;

    /// A-66
    #[test]
    fn secrets_are_replaced_and_names_kept() {
        let cases = [
            ("api_key: sk-abc123", "api_key: ***"),
            ("  api_key: \"sk-abc123\"", "  api_key: \"***\""),
            (
                "{\"api_key\": \"sk-abc\", \"model\": \"x\"}",
                "{\"api_key\": \"***\", \"model\": \"x\"}",
            ),
            (
                "Authorization: Bearer eyJhbGciOi.abc",
                "Authorization: Bearer ***",
            ),
            ("authorization=Basic dXNlcjpw", "authorization=Basic ***"),
            (
                "wss://example.test/mcp/?token=abc.def&x=1",
                "wss://example.test/mcp/?token=***&x=1",
            ),
            ("ACCESS_TOKEN=xyz", "ACCESS_TOKEN=***"),
            ("password = hunter2 ok", "password = *** ok"),
            ("LLM_API-KEY: k1", "LLM_API-KEY: ***"),
            ("x_api_key=abc", "x_api_key=***"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_line(input), expected, "{input}");
        }
    }

    #[test]
    fn nv15_standalone_auth_and_legacy_log_tail_secrets_are_redacted() {
        for source in [
            "Bearer synthetic-old-token",
            "Basic synthetic-credential",
            "refresh Bearer synthetic-old-token now",
        ] {
            let sanitized = redact_line(source);
            assert!(
                !sanitized.contains("synthetic-old-token")
                    && !sanitized.contains("synthetic-credential")
            );
            assert!(sanitized.contains("***"));
        }
        assert_eq!(
            redact_line("Authorization: Bearer synthetic-old-token"),
            "Authorization: Bearer ***"
        );
    }
    #[test]
    fn ordinary_text_is_untouched() {
        for line in [
            "tokens_used: 120",
            "tokenizer loaded",
            "收到 token 统计",
            "api_key:",
            "api_key: ***",
            "2026-10-06 INFO server started on 0.0.0.0:8000",
        ] {
            assert_eq!(redact_line(line), line);
        }
    }
}
