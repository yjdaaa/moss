//! 单说话人中文 VITS（`TtsEngine::Vits`，首个为 MOSS 原声 `moss_vits`，内置语音技术书 n122）。
//!
//! 模型目录里是下载校验过的原文件：`moss.onnx`（CjangCjengh `chinese_cleaners` 训练、Piper 式导出，
//! 输入 `input` / `input_lengths` / `scales`，不带 sherpa 元数据）与 sherpa-onnx 官方中文 VITS 的
//! `tokens.txt` / `lexicon.txt`（同一套 50 个注音符号，顺序一致）。这里派生 sherpa 能直接加载的三份文件：
//!
//! - `model.onnx`：原模型末尾追加 ONNX `metadata_props`（protobuf 第 14 号字段可重复，追加即生效，权重不动）；
//! - `tokens.txt`：把空格符号（第 49 号）改名为 `▁`；
//! - `lexicon.txt`：每个词前加 `▁`，还原原版前端“词与词之间一个空格”的输入
//!   （sherpa 的词表前端会丢掉空格；2026-11-25 噪声关掉对照，与原版前端逐采样一致，只多句首一个空格）。
//!
//! 派生文件放在模型目录的 `derived-v1/`，删模型时一起删；目录不可写时退到系统临时目录。

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const SCHEMA: &str = "moss-vits-derived-v1";
/// 原版前端的词间空格在 `tokens.txt` 里的编号（`chinese_cleaners` 符号表最后一个）
const SPACE_ID: &str = "49";
const BOUNDARY: &str = "▁";

/// sherpa 读取的模型元数据：`comment=piper` 走 `input` / `input_lengths` / `scales` 三输入；
/// `jieba=1` 选词表最长匹配前端（不需要 `dict_dir`）；`add_blank=1` 与训练配置一致。
const METADATA: &[(&str, &str)] = &[
    ("model_type", "vits"),
    ("comment", "piper"),
    ("language", "Chinese"),
    ("add_blank", "1"),
    ("n_speakers", "1"),
    ("sample_rate", "16000"),
    ("jieba", "1"),
    ("punctuation", "， 。 ！ ？ — …"),
];

#[derive(Debug)]
pub(crate) struct VitsFiles {
    pub model: PathBuf,
    pub tokens: PathBuf,
    pub lexicon: PathBuf,
}

/// 准备派生文件（已是最新则直接复用）。
pub(crate) fn prepare_vits_files(dir: &Path) -> Result<VitsFiles, String> {
    let model_in = dir.join("moss.onnx");
    let tokens_in = dir.join("tokens.txt");
    let lexicon_in = dir.join("lexicon.txt");
    for path in [&model_in, &tokens_in, &lexicon_in] {
        if !path.is_file() {
            return Err(format!("模型文件不存在：{}", path.display()));
        }
    }
    let fingerprint = fingerprint(&[&model_in, &tokens_in, &lexicon_in]);
    let candidates = cache_dirs(dir);
    for cache in &candidates {
        if let Some(files) = cache_ready(cache, &fingerprint) {
            return Ok(files);
        }
    }
    let tokens = derive_tokens(
        &fs::read_to_string(&tokens_in).map_err(|e| format!("读取 tokens.txt 失败：{e}"))?,
    )?;
    let lexicon = derive_lexicon(
        &fs::read_to_string(&lexicon_in).map_err(|e| format!("读取 lexicon.txt 失败：{e}"))?,
    );
    let mut last_error = String::new();
    for cache in &candidates {
        match write_cache(cache, &fingerprint, &model_in, &tokens, &lexicon) {
            Ok(()) => return Ok(files_in(cache)),
            Err(error) => {
                // 另一个进程（如后台试听预热）同时在写：它写完的同指纹缓存直接用
                for _ in 0..3 {
                    if let Some(files) = cache_ready(cache, &fingerprint) {
                        return Ok(files);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                last_error = error.to_string();
            }
        }
    }
    Err(format!("准备 MOSS 原声模型失败：{last_error}"))
}

fn files_in(cache: &Path) -> VitsFiles {
    VitsFiles {
        model: cache.join("model.onnx"),
        tokens: cache.join("tokens.txt"),
        lexicon: cache.join("lexicon.txt"),
    }
}

fn fingerprint(inputs: &[&Path]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SCHEMA.as_bytes());
    for path in inputs {
        let meta = fs::metadata(path).ok();
        let len = meta.as_ref().map_or(0, |m| m.len());
        let mtime = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        hasher.update(len.to_le_bytes());
        hasher.update(mtime.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn cache_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut path_hasher = Sha256::new();
    path_hasher.update(dir.to_string_lossy().as_bytes());
    let digest = format!("{:x}", path_hasher.finalize());
    vec![
        dir.join("derived-v1"),
        std::env::temp_dir().join(SCHEMA).join(&digest[..16]),
    ]
}

fn cache_ready(cache: &Path, fingerprint: &str) -> Option<VitsFiles> {
    let saved = fs::read_to_string(cache.join("fingerprint.txt")).ok()?;
    let files = files_in(cache);
    (saved.trim() == fingerprint
        && [&files.model, &files.tokens, &files.lexicon]
            .iter()
            .all(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() > 0)))
    .then_some(files)
}

fn write_cache(
    cache: &Path,
    fingerprint: &str,
    model_in: &Path,
    tokens: &str,
    lexicon: &str,
) -> io::Result<()> {
    fs::create_dir_all(cache)?;
    let nonce = format!(
        "{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let tmp = |name: &str| cache.join(format!("{name}.tmp.{nonce}"));
    let names = ["model.onnx", "tokens.txt", "lexicon.txt", "fingerprint.txt"];
    let result = (|| {
        // 先让旧指纹失效，避免写到一半时被当成完整缓存
        let _ = fs::remove_file(cache.join("fingerprint.txt"));
        write_model_with_metadata(model_in, &tmp("model.onnx"))?;
        fs::write(tmp("tokens.txt"), tokens)?;
        fs::write(tmp("lexicon.txt"), lexicon)?;
        fs::write(tmp("fingerprint.txt"), format!("{fingerprint}\n"))?;
        for name in names {
            fs::rename(tmp(name), cache.join(name))?;
        }
        Ok(())
    })();
    if result.is_err() {
        for name in names {
            let _ = fs::remove_file(tmp(name));
        }
    }
    result
}

fn write_model_with_metadata(src: &Path, dst: &Path) -> io::Result<()> {
    let mut input = fs::File::open(src)?;
    let mut output = io::BufWriter::new(fs::File::create(dst)?);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let read = input.read(&mut buf)?;
        if read == 0 {
            break;
        }
        output.write_all(&buf[..read])?;
    }
    output.write_all(&onnx_metadata(METADATA))?;
    output.flush()?;
    output.get_ref().sync_all()
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_bytes(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
    out.push(tag);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

/// 编码 `ModelProto.metadata_props`（第 14 号字段，`StringStringEntryProto { key = 1; value = 2; }`）。
fn onnx_metadata(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (key, value) in pairs {
        let mut entry = Vec::new();
        put_bytes(&mut entry, 0x0a, key.as_bytes());
        put_bytes(&mut entry, 0x12, value.as_bytes());
        put_bytes(&mut out, 0x72, &entry);
    }
    out
}

/// `tokens.txt` 里只有编号的那一行是空格符号，改名为 `▁`（词表里才写得进去）。
fn derive_tokens(raw: &str) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len() + 4);
    let mut renamed = false;
    for line in raw.lines() {
        if line.trim() == SPACE_ID && line.starts_with(' ') {
            out.push_str(BOUNDARY);
            out.push(' ');
            out.push_str(SPACE_ID);
            renamed = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !renamed
        || raw
            .lines()
            .any(|line| line.split_whitespace().next() == Some(BOUNDARY))
    {
        return Err("tokens.txt 不是预期的中文注音符号表".into());
    }
    Ok(out)
}

/// 每个词的注音前加词界 `▁`。
fn derive_lexicon(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + raw.len() / 4);
    for line in raw.lines() {
        let mut parts = line.split_whitespace();
        let Some(word) = parts.next() else { continue };
        let phones: Vec<&str> = parts.collect();
        if phones.is_empty() {
            continue;
        }
        out.push_str(word);
        out.push(' ');
        out.push_str(BOUNDARY);
        for phone in phones {
            out.push(' ');
            out.push_str(phone);
        }
        out.push('\n');
    }
    out
}

/// 字母的中文读法（缩写逐个字母念，如 G 代码、ESP32）。
const LETTERS: [&str; 26] = [
    "诶",
    "比",
    "西",
    "迪",
    "伊",
    "艾弗",
    "吉",
    "艾尺",
    "艾",
    "杰",
    "开",
    "艾勒",
    "艾姆",
    "恩",
    "欧",
    "批",
    "丘",
    "阿",
    "艾斯",
    "提",
    "优",
    "维",
    "达不溜",
    "艾克斯",
    "歪",
    "贼",
];
/// 紧跟数字的单位
const UNITS: &[(&str, &str)] = &[
    ("mm", "毫米"),
    ("cm", "厘米"),
    ("km", "千米"),
    ("ms", "毫秒"),
    ("min", "分钟"),
    ("kg", "千克"),
    ("kW", "千瓦"),
    ("W", "瓦"),
    ("m", "米"),
    ("s", "秒"),
];
/// 拼成一个缩写逐个字母念的最长长度；更长的英文单词这个模型念不了，直接跳过
const MAX_SPELLED: usize = 6;

/// 送进模型前的文字整理：这个模型只认中文与中文标点，数字交给 `number.fst` 等读法规则，
/// 这里只处理规则不管的：百分号、紧跟数字的单位、MOSS 读作“莫斯”、大写缩写逐个字母念、半角标点换全角。
pub(crate) fn normalize_text(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || (chars[i] == '.'
                        && i + 1 < chars.len()
                        && chars[i + 1].is_ascii_digit()
                        && i > start))
            {
                i += 1;
            }
            let number: String = chars[start..i].iter().collect();
            if i < chars.len() && (chars[i] == '%' || chars[i] == '％') {
                out.push_str("百分之");
                out.push_str(&number);
                i += 1;
                continue;
            }
            out.push_str(&number);
            let rest: String = chars[i..].iter().take(4).collect();
            if let Some((unit, spoken)) = UNITS.iter().find(|(unit, _)| {
                rest.starts_with(unit)
                    && !rest[unit.len()..]
                        .chars()
                        .next()
                        .is_some_and(|n| n.is_ascii_alphabetic())
            }) {
                out.push_str(spoken);
                i += unit.chars().count();
            }
            continue;
        }
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if word.eq_ignore_ascii_case("moss") {
                out.push_str("莫斯");
            } else if word.len() <= MAX_SPELLED
                && (word.len() == 1 || word.chars().all(|ch| ch.is_ascii_uppercase()))
            {
                for ch in word.chars() {
                    out.push_str(LETTERS[(ch.to_ascii_uppercase() as u8 - b'A') as usize]);
                }
            }
            continue;
        }
        match c {
            '℃' => out.push_str("摄氏度"),
            '°' if chars.get(i + 1).is_some_and(|n| *n == 'C') => {
                out.push_str("摄氏度");
                i += 1;
            }
            ',' | '、' | ';' | '；' | ':' | '：' => out.push('，'),
            '.' => out.push('。'),
            '!' => out.push('！'),
            '?' => out.push('？'),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_appended_as_onnx_model_proto_field_14() {
        let bytes = onnx_metadata(&[("comment", "piper")]);
        // 0x72 = 字段 14、长度定界；条目里 key = 1（0x0a）、value = 2（0x12）
        assert_eq!(
            bytes,
            [
                &[0x72, 16, 0x0a, 7][..],
                b"comment",
                &[0x12, 5][..],
                b"piper"
            ]
            .concat()
        );
        let mut long = Vec::new();
        put_varint(&mut long, 300);
        assert_eq!(long, [0xac, 0x02]);
    }

    #[test]
    fn tokens_rename_space_and_lexicon_gets_word_boundaries() {
        let tokens = "_ 0\n， 1\nㄋ 13\nˇ 46\n  49\n";
        let derived = derive_tokens(tokens).unwrap();
        assert!(derived.ends_with("▁ 49\n"), "{derived}");
        assert!(
            derive_tokens("_ 0\nㄋ 13\n").is_err(),
            "没有空格符号的表不认"
        );
        assert_eq!(
            derive_lexicon("你好 ㄋ ㄧ ˇ ㄏ ㄠ ˇ\n\n坏行\n的 ㄉ ㄜ ˙\n"),
            "你好 ▁ ㄋ ㄧ ˇ ㄏ ㄠ ˇ\n的 ▁ ㄉ ㄜ ˙\n"
        );
    }

    #[test]
    fn normalize_handles_percent_units_moss_and_acronyms() {
        assert_eq!(
            normalize_text("功率30%，3mm椴木，用时12分钟"),
            "功率百分之30，3毫米椴木，用时12分钟"
        );
        assert_eq!(
            normalize_text("温度36.5℃，延迟200ms。"),
            "温度36.5摄氏度，延迟200毫秒。"
        );
        assert_eq!(normalize_text("你好，我是 MOSS。"), "你好，我是 莫斯。");
        assert_eq!(normalize_text("发G代码给ESP32"), "发吉代码给伊艾斯批32");
        // 太长的英文单词念不了：跳过；小数点保留给数字读法规则，句号换全角
        assert_eq!(normalize_text("连上FluidNC了.好!"), "连上了。好！");
        assert_eq!(normalize_text("速度1.5倍,对吗?"), "速度1.5倍，对吗？");
        assert_eq!(normalize_text("50min后"), "50分钟后");
        // 单位后面还连着字母就不是单位（这里 `mmx` 被当成念不了的英文跳过）
        assert_eq!(normalize_text("3mmx"), "3");
    }

    #[test]
    fn prepare_writes_derived_files_once_and_reuses_cache() {
        let root = std::env::temp_dir().join(format!(
            "moss-vits-derive-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let model_dir = root.join("moss-vits-zh");
        fs::create_dir_all(&model_dir).unwrap();
        fs::write(model_dir.join("moss.onnx"), b"\x08\x07fake-model").unwrap();
        fs::write(model_dir.join("tokens.txt"), "_ 0\nㄋ 13\n  49\n").unwrap();
        fs::write(model_dir.join("lexicon.txt"), "你 ㄋ ㄧ ˇ\n").unwrap();
        let files = prepare_vits_files(&model_dir).unwrap();
        assert!(files.model.starts_with(model_dir.join("derived-v1")));
        let model = fs::read(&files.model).unwrap();
        assert!(model.starts_with(b"\x08\x07fake-model"), "原模型字节不动");
        assert!(model.ends_with(&onnx_metadata(METADATA)));
        assert_eq!(
            fs::read_to_string(&files.lexicon).unwrap(),
            "你 ▁ ㄋ ㄧ ˇ\n"
        );
        // 第二次直接命中缓存（不重写）
        let stamp = fs::metadata(&files.model).unwrap().modified().unwrap();
        let again = prepare_vits_files(&model_dir).unwrap();
        assert_eq!(
            fs::metadata(&again.model).unwrap().modified().unwrap(),
            stamp
        );
        // 缺文件时报清楚
        fs::remove_file(model_dir.join("lexicon.txt")).unwrap();
        assert!(
            prepare_vits_files(&model_dir)
                .unwrap_err()
                .contains("lexicon.txt")
        );
        let _ = fs::remove_dir_all(&root);
    }
}
