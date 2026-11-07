//! 写入 xiaozhi-server 的 `data/.config.yaml`（技术书 §9.4.4 字段表，实施清单 P4-06）。
//!
//! - 只改字段表中的键，按行读-改-写，**保留文件中其他内容与注释**；
//! - 写入前备份为 `data/.config.yaml.bak`，写入后用 YAML 解析器重新解析并核对写入的值；
//! - 某分区留空时不改该模块；密钥留空时保留文件中已有的密钥；
//! - 检测到 `manager-api.url`（全模块部署，配置由智控台管理）时拒绝写入。

use std::path::{Path, PathBuf};

use crate::models::{ResolvedSection, Section};

/// 字段表中的模块名（技术书 §9.4.4）。
pub const LLM_NAME: &str = "MossLLM";
pub const ASR_NAME: &str = "MossASR";
pub const TTS_NAME: &str = "MossTTS";

/// 一个待写入的标量。
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Str(String),
    Num(f64),
}

impl Scalar {
    fn literal(&self) -> String {
        match self {
            // JSON 字符串同时是合法的 YAML 双引号标量
            Self::Str(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into()),
            Self::Num(value) => {
                if value.fract() == 0.0 && value.abs() < 1e9 {
                    format!("{}", *value as i64)
                } else {
                    format!("{value}")
                }
            }
        }
    }

    fn matches(&self, value: &serde_yaml::Value) -> bool {
        match self {
            Self::Str(expected) => value.as_str() == Some(expected.as_str()),
            Self::Num(expected) => value
                .as_f64()
                .map(|actual| (actual - expected).abs() < 1e-9)
                .unwrap_or(false),
        }
    }
}

/// 一次写入的全部改动：`(路径, 值)`。
pub type Edits = Vec<(Vec<String>, Scalar)>;

fn path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

/// 由各分区生成字段表中的改动。`mcp_endpoint` 为自建接入点（写入顶层 `mcp_endpoint`）。
pub fn edits_for(sections: &[ResolvedSection], mcp_endpoint: Option<&str>) -> Edits {
    let mut edits: Edits = Vec::new();
    let s = |value: &str| Scalar::Str(value.to_owned());
    for resolved in sections {
        match resolved.section {
            Section::Llm => {
                edits.push((path(&["LLM", LLM_NAME, "type"]), s("openai")));
                edits.push((path(&["LLM", LLM_NAME, "url"]), s(&resolved.url)));
                if !resolved.api_key.is_empty() {
                    edits.push((path(&["LLM", LLM_NAME, "api_key"]), s(&resolved.api_key)));
                }
                edits.push((path(&["LLM", LLM_NAME, "model_name"]), s(&resolved.model)));
                edits.push((path(&["selected_module", "LLM"]), s(LLM_NAME)));
            }
            Section::Asr => {
                edits.push((path(&["ASR", ASR_NAME, "type"]), s("openai")));
                edits.push((path(&["ASR", ASR_NAME, "base_url"]), s(&resolved.url)));
                if !resolved.api_key.is_empty() {
                    edits.push((path(&["ASR", ASR_NAME, "api_key"]), s(&resolved.api_key)));
                }
                edits.push((path(&["ASR", ASR_NAME, "model_name"]), s(&resolved.model)));
                edits.push((path(&["ASR", ASR_NAME, "output_dir"]), s("tmp/")));
                edits.push((path(&["selected_module", "ASR"]), s(ASR_NAME)));
            }
            Section::Tts => {
                edits.push((path(&["TTS", TTS_NAME, "type"]), s("openai")));
                edits.push((path(&["TTS", TTS_NAME, "api_url"]), s(&resolved.url)));
                if !resolved.api_key.is_empty() {
                    edits.push((path(&["TTS", TTS_NAME, "api_key"]), s(&resolved.api_key)));
                }
                edits.push((path(&["TTS", TTS_NAME, "model"]), s(&resolved.model)));
                edits.push((path(&["TTS", TTS_NAME, "voice"]), s(&resolved.voice)));
                edits.push((
                    path(&["TTS", TTS_NAME, "speed"]),
                    Scalar::Num(resolved.speed),
                ));
                edits.push((path(&["TTS", TTS_NAME, "output_dir"]), s("tmp/")));
                edits.push((path(&["selected_module", "TTS"]), s(TTS_NAME)));
            }
        }
    }
    if !sections.is_empty() {
        edits.push((path(&["selected_module", "Intent"]), s("function_call")));
    }
    if let Some(endpoint) = mcp_endpoint
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        edits.push((path(&["mcp_endpoint"]), s(endpoint)));
    }
    edits
}

// ---------- 按行编辑 ----------

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_content(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && !trimmed.starts_with('#')
}

/// 行首的映射键（去掉引号）；列表项、无冒号的行返回 `None`。
fn key_of(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed == "-" || trimmed.starts_with('#') {
        return None;
    }
    let (raw, quoted) = if let Some(rest) = trimmed.strip_prefix('"') {
        (rest.split_once('"')?.0, true)
    } else if let Some(rest) = trimmed.strip_prefix('\'') {
        (rest.split_once('\'')?.0, true)
    } else {
        let mut end = None;
        let bytes = trimmed.as_bytes();
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b':'
                && (index + 1 == bytes.len()
                    || bytes[index + 1] == b' '
                    || bytes[index + 1] == b'\t')
            {
                end = Some(index);
                break;
            }
        }
        (&trimmed[..end?], false)
    };
    if quoted {
        // 引号后必须紧跟冒号
        let after = &trimmed[raw.len() + 2..];
        if !after.starts_with(':') {
            return None;
        }
    }
    Some(raw.trim().to_owned())
}

/// 键行冒号之后的内联值（去掉注释与空白）。
fn inline_value(line: &str) -> String {
    let trimmed = line.trim_start();
    let after = if trimmed.starts_with('"') || trimmed.starts_with('\'') {
        let quote = trimmed.as_bytes()[0] as char;
        trimmed[1..]
            .split_once(quote)
            .map(|(_, rest)| rest.trim_start_matches(':'))
            .unwrap_or("")
    } else {
        trimmed
            .split_once(": ")
            .map(|(_, rest)| rest)
            .or_else(|| trimmed.strip_suffix(':').map(|_| ""))
            .unwrap_or("")
    };
    let value = after.trim();
    if value.starts_with('#') {
        String::new()
    } else {
        value.split(" #").next().unwrap_or("").trim().to_owned()
    }
}

struct Doc {
    lines: Vec<String>,
    unit: usize,
}

impl Doc {
    fn parse(text: &str) -> Self {
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let unit = lines
            .iter()
            .filter(|line| is_content(line))
            .map(|line| indent_of(line))
            .filter(|indent| *indent > 0)
            .min()
            .unwrap_or(2);
        Self { lines, unit }
    }

    /// 键 `index` 的值块的结束位置（不含）：之后第一条缩进不大于它的内容行。
    fn block_end(&self, index: usize) -> usize {
        let own = indent_of(&self.lines[index]);
        let mut cursor = index + 1;
        while cursor < self.lines.len() {
            let line = &self.lines[cursor];
            if is_content(line) {
                let indent = indent_of(line);
                let is_item = line.trim_start().starts_with('-');
                if indent < own || (indent == own && !is_item) {
                    break;
                }
            }
            cursor += 1;
        }
        cursor
    }

    /// 值块中最后一条内容行之后的位置（插入新子键的位置，避开属于下一个键的注释）。
    fn insert_point(&self, parent: Option<usize>) -> usize {
        let (start, end) = match parent {
            None => (0, self.lines.len()),
            Some(index) => (index, self.block_end(index)),
        };
        (start..end)
            .rev()
            .find(|cursor| is_content(&self.lines[*cursor]))
            .map(|cursor| cursor + 1)
            .unwrap_or(end)
    }

    fn child_indent(&self, parent: Option<usize>) -> usize {
        let Some(index) = parent else { return 0 };
        let own = indent_of(&self.lines[index]);
        (index + 1..self.block_end(index))
            .find(|cursor| is_content(&self.lines[*cursor]))
            .map(|cursor| indent_of(&self.lines[cursor]))
            .filter(|indent| *indent > own)
            .unwrap_or(own + self.unit)
    }

    fn find_child(&self, parent: Option<usize>, key: &str) -> Option<usize> {
        let indent = self.child_indent(parent);
        let (start, end) = match parent {
            None => (0, self.lines.len()),
            Some(index) => (index + 1, self.block_end(index)),
        };
        (start..end).find(|cursor| {
            let line = &self.lines[*cursor];
            is_content(line) && indent_of(line) == indent && key_of(line).as_deref() == Some(key)
        })
    }

    fn set(&mut self, path: &[String], value: &Scalar) -> Result<(), String> {
        let mut parent: Option<usize> = None;
        for (depth, key) in path.iter().enumerate() {
            let last = depth + 1 == path.len();
            match self.find_child(parent, key) {
                Some(index) => {
                    let indent = indent_of(&self.lines[index]);
                    if last {
                        // 替换值；删除旧值的多行内容
                        let end = self.block_end(index);
                        let content_end = (index + 1..end)
                            .rev()
                            .find(|cursor| is_content(&self.lines[*cursor]))
                            .map(|cursor| cursor + 1)
                            .unwrap_or(index + 1);
                        self.lines.drain(index + 1..content_end);
                        self.lines[index] =
                            format!("{}{key}: {}", " ".repeat(indent), value.literal());
                    } else {
                        let inline = inline_value(&self.lines[index]);
                        if !inline.is_empty() {
                            if matches!(inline.as_str(), "{}" | "null" | "~") {
                                self.lines[index] = format!("{}{key}:", " ".repeat(indent));
                            } else {
                                return Err(format!(
                                    "{} 使用了单行写法，无法自动修改，请改成多行后再保存",
                                    path[..=depth].join(".")
                                ));
                            }
                        }
                        parent = Some(index);
                    }
                }
                None => {
                    let indent = self.child_indent(parent);
                    let at = self.insert_point(parent);
                    let line = if last {
                        format!("{}{key}: {}", " ".repeat(indent), value.literal())
                    } else {
                        format!("{}{key}:", " ".repeat(indent))
                    };
                    self.lines.insert(at, line);
                    if last {
                        return Ok(());
                    }
                    parent = Some(at);
                }
            }
        }
        Ok(())
    }

    fn render(&self) -> String {
        let mut text = self.lines.join("\n");
        text.push('\n');
        text
    }
}

fn lookup<'a>(value: &'a serde_yaml::Value, path: &[String]) -> Option<&'a serde_yaml::Value> {
    path.iter().try_fold(value, |current, key| {
        current.as_mapping()?.get(key.as_str())
    })
}

/// 是否为全模块部署（`manager-api.url` 非空）：此时配置由智控台管理，向导只测试不写。
pub fn is_full_module(text: &str) -> bool {
    serde_yaml::from_str::<serde_yaml::Value>(text)
        .ok()
        .and_then(|value| {
            lookup(&value, &["manager-api".to_owned(), "url".to_owned()])
                .and_then(serde_yaml::Value::as_str)
                .map(|url| !url.trim().is_empty())
        })
        .unwrap_or(false)
}

/// 在内存中应用改动并校验，返回新文本。原文本无法解析时拒绝修改。
pub fn apply(original: &str, edits: &Edits) -> Result<String, String> {
    if !original.trim().is_empty() {
        let parsed: serde_yaml::Value = serde_yaml::from_str(original)
            .map_err(|_| "现有 data/.config.yaml 无法解析，为避免损坏未做修改".to_owned())?;
        if !(parsed.is_mapping() || parsed.is_null()) {
            return Err("现有 data/.config.yaml 顶层不是映射，未做修改".into());
        }
    }
    if is_full_module(original) {
        return Err(
            "检测到 manager-api.url（全模块部署），模型配置由智控台管理；向导只测试连接，不写文件"
                .into(),
        );
    }
    let mut doc = Doc::parse(original);
    for (path, value) in edits {
        doc.set(path, value)?;
    }
    let text = doc.render();
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|_| "写入后的配置无法解析，已放弃写入".to_owned())?;
    for (path, value) in edits {
        if !lookup(&parsed, path)
            .map(|actual| value.matches(actual))
            .unwrap_or(false)
        {
            return Err(format!("写入后核对失败：{}，已放弃写入", path.join(".")));
        }
    }
    Ok(text)
}

/// 写入结果（不含密钥）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WriteOutcome {
    pub created: bool,
    pub backup: Option<PathBuf>,
    pub changed_keys: Vec<String>,
}

/// 读-改-写 `config_path`：先备份为 `.bak`，临时文件写好后替换。
pub fn write(config_path: &Path, edits: &Edits) -> Result<WriteOutcome, String> {
    let existing = match std::fs::read(config_path) {
        Ok(bytes) => Some(
            String::from_utf8(bytes)
                .map_err(|_| "现有 data/.config.yaml 不是 UTF-8 文本，未做修改".to_owned())?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("无法读取 data/.config.yaml".into()),
    };
    let original = existing.clone().unwrap_or_default();
    let text = apply(&original, edits)?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "无法创建 data 目录".to_owned())?;
    }
    let backup = if existing.is_some() {
        let mut name = config_path.as_os_str().to_owned();
        name.push(".bak");
        let backup = PathBuf::from(name);
        std::fs::copy(config_path, &backup)
            .map_err(|_| "备份 data/.config.yaml.bak 失败，未做修改".to_owned())?;
        Some(backup)
    } else {
        None
    };
    let mut temp_name = config_path.as_os_str().to_owned();
    temp_name.push(".tmp");
    let temp = PathBuf::from(temp_name);
    std::fs::write(&temp, text.as_bytes()).map_err(|_| "写入临时文件失败".to_owned())?;
    std::fs::rename(&temp, config_path).map_err(|_| {
        let _ = std::fs::remove_file(&temp);
        "替换 data/.config.yaml 失败".to_owned()
    })?;
    // 写入后从磁盘重新读取并解析一次
    let reread =
        std::fs::read_to_string(config_path).map_err(|_| "写入后无法重新读取配置".to_owned())?;
    serde_yaml::from_str::<serde_yaml::Value>(&reread)
        .map_err(|_| "写入后的配置无法解析，请用 .bak 恢复".to_owned())?;
    let mut changed_keys: Vec<String> = edits.iter().map(|(path, _)| path.join(".")).collect();
    changed_keys.dedup();
    Ok(WriteOutcome {
        created: existing.is_none(),
        backup,
        changed_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SectionInput, resolve};

    const SAMPLE: &str = "# 自定义配置：只写需要覆盖的项\nserver:\n  ip: 0.0.0.0\n  port: 8000 # 服务端口\n\nselected_module:\n  VAD: SileroVAD\n  ASR: FunASR\n  LLM: ChatGLMLLM\n\n# 大模型\nLLM:\n  ChatGLMLLM:\n    type: openai\n    model_name: glm-4-flash\n    api_key: old-glm-key\n  MossLLM:\n    type: openai\n    url: https://old.example/v1\n    api_key: sk-old-llm-key\n    model_name: old-model\n    temperature: 0.7\n\nprompt: |\n  你是小智。\n  请简短回答。\n";

    fn resolved(section: Section, url: &str, key: &str, model: &str) -> ResolvedSection {
        let input = SectionInput {
            url: url.into(),
            api_key: key.into(),
            model: model.into(),
            voice: "alloy".into(),
            speed: Some(1.25),
            ..Default::default()
        };
        resolve(section, &input).unwrap().unwrap()
    }

    fn get(text: &str, path: &[&str]) -> serde_yaml::Value {
        let value: serde_yaml::Value = serde_yaml::from_str(text).unwrap();
        lookup(
            &value,
            &path.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        )
        .cloned()
        .unwrap_or(serde_yaml::Value::Null)
    }

    #[test]
    fn llm_only_update_keeps_comments_other_modules_and_extra_fields() {
        let llm = resolved(
            Section::Llm,
            "https://new.example/v1/",
            "sk-new-llm-key",
            "new-model",
        );
        let text = apply(SAMPLE, &edits_for(&[llm], None)).unwrap();
        assert!(text.contains("# 自定义配置：只写需要覆盖的项"));
        assert!(text.contains("port: 8000 # 服务端口"));
        assert!(text.contains("# 大模型"));
        assert_eq!(
            get(&text, &["LLM", "MossLLM", "url"]).as_str(),
            Some("https://new.example/v1")
        );
        assert_eq!(
            get(&text, &["LLM", "MossLLM", "api_key"]).as_str(),
            Some("sk-new-llm-key")
        );
        assert_eq!(
            get(&text, &["LLM", "MossLLM", "model_name"]).as_str(),
            Some("new-model")
        );
        assert_eq!(
            get(&text, &["LLM", "MossLLM", "temperature"]).as_f64(),
            Some(0.7)
        );
        assert_eq!(
            get(&text, &["LLM", "ChatGLMLLM", "api_key"]).as_str(),
            Some("old-glm-key")
        );
        assert_eq!(
            get(&text, &["selected_module", "LLM"]).as_str(),
            Some("MossLLM")
        );
        assert_eq!(
            get(&text, &["selected_module", "Intent"]).as_str(),
            Some("function_call")
        );
        // 留空的分区不改
        assert_eq!(
            get(&text, &["selected_module", "ASR"]).as_str(),
            Some("FunASR")
        );
        assert_eq!(
            get(&text, &["prompt"]).as_str(),
            Some("你是小智。\n请简短回答。\n")
        );
        assert!(!text.contains("sk-old-llm-key"));
    }

    #[test]
    fn blank_key_keeps_existing_key() {
        let llm = resolved(Section::Llm, "https://new.example/v1", "", "m2");
        let text = apply(SAMPLE, &edits_for(&[llm], None)).unwrap();
        assert_eq!(
            get(&text, &["LLM", "MossLLM", "api_key"]).as_str(),
            Some("sk-old-llm-key")
        );
        assert_eq!(
            get(&text, &["LLM", "MossLLM", "model_name"]).as_str(),
            Some("m2")
        );
    }

    #[test]
    fn missing_sections_and_new_file_are_created() {
        let sections = [
            resolved(Section::Llm, "https://h/v1", "k1", "m"),
            resolved(Section::Asr, "https://h/v1", "k2", "whisper-1"),
            resolved(Section::Tts, "https://h/v1", "k3", "tts-1"),
        ];
        let edits = edits_for(
            &sections,
            Some("ws://127.0.0.1:8004/mcp_endpoint/mcp/?token=t"),
        );
        for original in ["", SAMPLE, "server:\n    port: 8000\nASR: {}\n"] {
            let text = apply(original, &edits).unwrap();
            assert_eq!(
                get(&text, &["ASR", "MossASR", "base_url"]).as_str(),
                Some("https://h/v1/audio/transcriptions")
            );
            assert_eq!(
                get(&text, &["ASR", "MossASR", "output_dir"]).as_str(),
                Some("tmp/")
            );
            assert_eq!(
                get(&text, &["TTS", "MossTTS", "api_url"]).as_str(),
                Some("https://h/v1/audio/speech")
            );
            assert_eq!(
                get(&text, &["TTS", "MossTTS", "model"]).as_str(),
                Some("tts-1")
            );
            assert_eq!(
                get(&text, &["TTS", "MossTTS", "voice"]).as_str(),
                Some("alloy")
            );
            assert_eq!(
                get(&text, &["TTS", "MossTTS", "speed"]).as_f64(),
                Some(1.25)
            );
            assert_eq!(
                get(&text, &["selected_module", "TTS"]).as_str(),
                Some("MossTTS")
            );
            assert_eq!(
                get(&text, &["mcp_endpoint"]).as_str(),
                Some("ws://127.0.0.1:8004/mcp_endpoint/mcp/?token=t")
            );
        }
        // 四空格缩进的文件沿用四空格
        let text = apply("server:\n    port: 8000\n", &edits).unwrap();
        assert!(
            text.contains("\n    MossLLM:\n        type: \"openai\""),
            "{text}"
        );
    }

    #[test]
    fn special_characters_are_quoted_safely() {
        let llm = resolved(Section::Llm, "https://h/v1", "k:#\"'\\ x", "m: #1");
        let text = apply("", &edits_for(&[llm], None)).unwrap();
        assert_eq!(
            get(&text, &["LLM", "MossLLM", "api_key"]).as_str(),
            Some("k:#\"'\\ x")
        );
        assert_eq!(
            get(&text, &["LLM", "MossLLM", "model_name"]).as_str(),
            Some("m: #1")
        );
    }

    #[test]
    fn full_module_and_unparseable_files_are_refused() {
        let llm = resolved(Section::Llm, "https://h/v1", "k", "m");
        let edits = edits_for(&[llm], None);
        let full = "manager-api:\n  url: http://127.0.0.1:8002/xiaozhi\n  secret: s\n";
        assert!(is_full_module(full));
        assert!(apply(full, &edits).unwrap_err().contains("智控台"));
        assert!(!is_full_module("manager-api:\n  url: \"\"\n"));
        assert!(
            apply("a: [1, 2\n", &edits)
                .unwrap_err()
                .contains("无法解析")
        );
        assert!(
            apply("LLM: {MossLLM: {url: x}}\n", &edits)
                .unwrap_err()
                .contains("单行")
        );
    }

    #[test]
    fn write_creates_backup_and_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("data").join(".config.yaml");
        let llm = resolved(Section::Llm, "https://h/v1", "sk-fake-write", "m");
        let outcome = write(&config, &edits_for(&[llm.clone()], None)).unwrap();
        assert!(outcome.created);
        assert_eq!(outcome.backup, None);
        let first = std::fs::read_to_string(&config).unwrap();
        let outcome = write(
            &config,
            &edits_for(&[resolved(Section::Llm, "https://h2/v1", "", "m2")], None),
        )
        .unwrap();
        assert!(!outcome.created);
        let backup = outcome.backup.unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), first);
        let second = std::fs::read_to_string(&config).unwrap();
        assert!(second.contains("sk-fake-write"), "空密钥保留原值");
        assert!(second.contains("https://h2/v1"));
        assert!(!dir.path().join("data").join(".config.yaml.tmp").exists());
        assert!(
            !serde_json::to_string(&outcome.changed_keys)
                .unwrap()
                .contains("sk-fake")
        );
        // 全模块部署：文件不变
        std::fs::write(&config, "manager-api:\n  url: http://x/xiaozhi\n").unwrap();
        assert!(write(&config, &edits_for(&[llm], None)).is_err());
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "manager-api:\n  url: http://x/xiaozhi\n"
        );
    }
}
