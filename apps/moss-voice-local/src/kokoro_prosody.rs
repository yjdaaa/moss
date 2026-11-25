// SPDX-License-Identifier: GPL-3.0-or-later
//! Kokoro 中文分词词界与普通话变调词表增强。
//!
//! 背景：上游 `sherpa-onnx 1.13.8` 的 `KokoroMultiLangLexicon` 已移除 `cppjieba`，忽略 `dict_dir`，
//! 仅按 `lexicon-zh.txt`（只含 `pypinyin.phrases_dict` 的约 4.7 万多音词，缺失“毫米、椴木、刻字、注意、
//! 确认、对焦、速度、太快、两遍”等绝大多数日常单音词组）做前向最大匹配；未命中的词逐字拆散，且
//! `ConvertChineseToTokenIDs` 未像英文分支那样在词与词之间插入 Kokoro 训练时使用的词界记号
//! （v1.0 IPA 为空格 `space_id`，v1.1 注音为 `/`），同时 v1.0 词表未做“一 / 不 / 三声连读”变调，
//! 导致合成语音出现一字一顿、词组粘连错位与“切不(bù)透 / 一(yī)般 / 你(nǐ)好”生硬读音。
//!
//! 做法：不改模型目录原文件，基于 `<kokoro_dir>/tokens.txt`、`lexicon-zh.txt` 与 `dict/*.utf8`
//! 派生带单码点非空白词界别名（v1.0 用 `▁ -> space_id`，v1.1 复用已有的 `/`）、补齐高频词组并完成
//! 普通话变调的词表，原子缓存在 `.tts-previews/<model>/prosody-v1`（只读目录自动回退临时目录）。

use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const PROSODY_SCHEMA: &str = "kokoro-prosody-v1";

/// 激光工坊高频词组与日常口语变调词（兜底 `jieba.dict.utf8` 低频项或缺失项）。
const SPOKEN_AND_WORKSHOP_WORDS: &[&str] = &[
    // 口语高频与一/不/三声变调词
    "你好",
    "您好",
    "可以",
    "好的",
    "没问题",
    "一下",
    "一般",
    "一起",
    "一直",
    "一定",
    "一样",
    "一共",
    "一会儿",
    "一遍",
    "一点",
    "一旦",
    "一再",
    "不要",
    "不用",
    "不错",
    "不对",
    "不好",
    "不够",
    "不透",
    "不大",
    "不深",
    "不行",
    "不能",
    "不会",
    "不是",
    "不必",
    "不准",
    "太快",
    "太慢",
    "太高",
    "太低",
    "别太快",
    "两遍",
    "两边",
    "两个",
    "两次",
    "两秒",
    "走两遍",
    "走一遍",
    "分两遍",
    "试一下",
    "看一下",
    "等一下",
    "查一下",
    "问一下",
    "想一下",
    "说一下",
    // 工坊与雕刻领域词组
    "毫米",
    "一毫米",
    "两毫米",
    "三毫米",
    "四毫米",
    "五毫米",
    "六毫米",
    "八毫米",
    "十毫米",
    "厘米",
    "每分钟",
    "每秒",
    "百分之",
    "椴木",
    "椴木板",
    "松木",
    "桦木",
    "竹板",
    "木板",
    "木牌",
    "亚克力",
    "牛皮纸",
    "不锈钢",
    "氧化铝",
    "边角料",
    "刻字",
    "雕刻",
    "切割",
    "切透",
    "切不透",
    "刻不深",
    "烧焦",
    "发黑",
    "对焦",
    "调焦",
    "焦距",
    "走边框",
    "边框",
    "原点",
    "归零",
    "解锁",
    "急停",
    "暂停",
    "待命",
    "空闲",
    "就绪",
    "已就绪",
    "激光",
    "激光机",
    "激光头",
    "雕刻机",
    "工作台",
    "防护盖",
    "护目镜",
    "排风",
    "吹气",
    "气泵",
    "功率",
    "速度",
    "遍数",
    "尺寸",
    "厚度",
    "温度",
    "进度",
    "材料",
    "连接",
    "串口",
    "注意",
    "最需要",
    "建议",
    "确认",
    "检查",
    "必要",
    "必要时",
    "把握",
    "较大",
    "把握较大",
    "把握一般",
    "把握不大",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KokoroAlphabet {
    /// Kokoro v1.0 多语种 IPA（声调箭头 `→ ↗ ↓ ↘`，词间空格 `space_id`）
    V10Ipa,
    /// Kokoro v1.1 中文注音（声调数字 `1..=5`，词间斜杠 `/`）
    V11Zhuyin,
}

impl KokoroAlphabet {
    fn tone_of_token(self, tok: &str) -> Option<u8> {
        match self {
            Self::V10Ipa => match tok {
                "→" => Some(1),
                "↗" => Some(2),
                "↓" => Some(3),
                "↘" => Some(4),
                _ => None,
            },
            Self::V11Zhuyin => match tok {
                "1" => Some(1),
                "2" => Some(2),
                "3" => Some(3),
                "4" => Some(4),
                "5" => Some(5),
                _ => None,
            },
        }
    }

    fn tone_token(self, tone: u8) -> Option<&'static str> {
        match (self, tone) {
            (Self::V10Ipa, 1) => Some("→"),
            (Self::V10Ipa, 2) => Some("↗"),
            (Self::V10Ipa, 3) => Some("↓"),
            (Self::V10Ipa, 4) => Some("↘"),
            (Self::V11Zhuyin, 1) => Some("1"),
            (Self::V11Zhuyin, 2) => Some("2"),
            (Self::V11Zhuyin, 3) => Some("3"),
            (Self::V11Zhuyin, 4) => Some("4"),
            (Self::V11Zhuyin, 5) => Some("5"),
            _ => None,
        }
    }
}

fn is_cjk(ch: char) -> bool {
    ('\u{4E00}'..='\u{9FFF}').contains(&ch)
}

fn is_cjk_word(word: &str, min_chars: usize, max_chars: usize) -> bool {
    let mut count = 0usize;
    for ch in word.chars() {
        if !is_cjk(ch) {
            return false;
        }
        count += 1;
        if count > max_chars {
            return false;
        }
    }
    count >= min_chars
}

fn file_stamp(path: &Path) -> (u64, u128) {
    match fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (meta.len(), mtime)
        }
        Err(_) => (0, 0),
    }
}

fn cache_ready(cache_dir: &Path, fingerprint: &str) -> Option<(PathBuf, PathBuf)> {
    let out_tokens = cache_dir.join("tokens.txt");
    let out_lexicon = cache_dir.join("lexicon-zh.txt");
    let fp_path = cache_dir.join("fingerprint.txt");
    if fs::read_to_string(&fp_path)
        .ok()
        .is_some_and(|saved| saved.trim() == fingerprint)
        && fs::metadata(&out_tokens).is_ok_and(|m| m.len() >= 32)
        && fs::metadata(&out_lexicon).is_ok_and(|m| m.len() >= 64)
    {
        Some((out_tokens, out_lexicon))
    } else {
        None
    }
}

/// 准备派生后的 `(tokens.txt, lexicon-zh.txt)` 路径；若输入目录是测试桩或未知词表格式则返回 `None` 回退原文件。
pub(crate) fn prepare_kokoro_prosody_files(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let tokens_in = dir.join("tokens.txt");
    let lexicon_in = dir.join("lexicon-zh.txt");
    let jieba_in = dir.join("dict").join("jieba.dict.utf8");
    let user_in = dir.join("dict").join("user.dict.utf8");

    let tokens_bytes = fs::read(&tokens_in).ok()?;
    if tokens_bytes.len() < 32 {
        return None;
    }
    let tokens_stamp = file_stamp(&tokens_in);
    let zh_stamp = file_stamp(&lexicon_in);
    if zh_stamp.0 < 64 {
        return None;
    }
    let jieba_stamp = file_stamp(&jieba_in);
    let user_stamp = file_stamp(&user_in);
    let user_bytes = if user_stamp.0 > 0 && user_stamp.0 <= 4 * 1024 * 1024 {
        fs::read(&user_in).ok()
    } else {
        None
    };

    let fingerprint = compute_fingerprint(
        &tokens_bytes,
        tokens_stamp,
        zh_stamp,
        jieba_stamp,
        user_stamp,
        user_bytes.as_deref(),
    );
    let candidates = candidate_cache_dirs(dir);
    for cache_dir in &candidates {
        if let Some(hit) = cache_ready(cache_dir, &fingerprint) {
            return Some(hit);
        }
    }

    let tokens_raw = String::from_utf8(tokens_bytes).ok()?;
    let lexicon_raw = fs::read_to_string(&lexicon_in).ok()?;
    let jieba_raw = fs::read_to_string(&jieba_in).ok();
    let user_raw = match user_bytes {
        Some(bytes) => String::from_utf8(bytes).ok(),
        None => fs::read_to_string(&user_in).ok(),
    };

    let (derived_tokens, derived_lexicon) = build_prosody_assets(
        &tokens_raw,
        &lexicon_raw,
        jieba_raw.as_deref(),
        user_raw.as_deref(),
    )?;

    for cache_dir in &candidates {
        if write_cache_atomic(cache_dir, &fingerprint, &derived_tokens, &derived_lexicon).is_ok() {
            return Some((
                cache_dir.join("tokens.txt"),
                cache_dir.join("lexicon-zh.txt"),
            ));
        }
        // Windows 下主进程与后台试听预热进程同时首次加载时，同伴进程可能刚写完并用 ifstream 打开词表导致 rename 共享冲突：
        // 重查同伴写入的同指纹缓存，命中则直接复用，避免落入临时目录产生孤儿副本
        for _ in 0..3 {
            if let Some(hit) = cache_ready(cache_dir, &fingerprint) {
                return Some(hit);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    None
}

fn compute_fingerprint(
    tokens_bytes: &[u8],
    tokens_stamp: (u64, u128),
    zh_stamp: (u64, u128),
    jieba_stamp: (u64, u128),
    user_stamp: (u64, u128),
    user_bytes: Option<&[u8]>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PROSODY_SCHEMA.as_bytes());
    hasher.update(b"\0");
    hasher.update(tokens_bytes);
    for (len, mtime) in [tokens_stamp, zh_stamp, jieba_stamp, user_stamp] {
        hasher.update(len.to_le_bytes());
        hasher.update(mtime.to_le_bytes());
    }
    if let Some(bytes) = user_bytes {
        hasher.update(b"\x01");
        hasher.update(bytes);
    }
    let mut words_hasher = Sha256::new();
    for &w in SPOKEN_AND_WORKSHOP_WORDS {
        words_hasher.update(w.as_bytes());
        words_hasher.update(b"\n");
    }
    hasher.update(words_hasher.finalize());
    format!("{:x}", hasher.finalize())
}

fn candidate_cache_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(2);
    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if let Some(parent) = dir.parent() {
        let preview_model_id = if dir_name.contains("v1_1") || dir_name == "kokoro_v11" {
            Some("kokoro_v11")
        } else if dir_name.starts_with("kokoro") {
            Some("kokoro")
        } else {
            None
        };
        if let Some(model_id) = preview_model_id {
            out.push(
                parent
                    .join(".tts-previews")
                    .join(model_id)
                    .join("prosody-v1"),
            );
        }
    }
    let mut path_hasher = Sha256::new();
    path_hasher.update(dir.to_string_lossy().as_bytes());
    let digest = format!("{:x}", path_hasher.finalize());
    out.push(
        std::env::temp_dir()
            .join(PROSODY_SCHEMA)
            .join(&digest[..16]),
    );
    out
}

fn write_cache_atomic(
    cache_dir: &Path,
    fingerprint: &str,
    tokens: &str,
    lexicon: &str,
) -> std::io::Result<()> {
    fs::create_dir_all(cache_dir)?;
    let pid = std::process::id();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp_tokens = cache_dir.join(format!("tokens.txt.tmp.{pid}.{nonce}"));
    let tmp_lexicon = cache_dir.join(format!("lexicon-zh.txt.tmp.{pid}.{nonce}"));
    let tmp_fp = cache_dir.join(format!("fingerprint.txt.tmp.{pid}.{nonce}"));

    let write_res = (|| -> std::io::Result<()> {
        fs::write(&tmp_tokens, tokens.as_bytes())?;
        fs::write(&tmp_lexicon, lexicon.as_bytes())?;
        fs::rename(&tmp_tokens, cache_dir.join("tokens.txt"))?;
        fs::rename(&tmp_lexicon, cache_dir.join("lexicon-zh.txt"))?;
        fs::write(&tmp_fp, format!("{fingerprint}\n").as_bytes())?;
        fs::rename(&tmp_fp, cache_dir.join("fingerprint.txt"))?;
        Ok(())
    })();
    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_tokens);
        let _ = fs::remove_file(&tmp_lexicon);
        let _ = fs::remove_file(&tmp_fp);
    }
    write_res
}

/// 纯内存构建派生 `(tokens.txt, lexicon-zh.txt)` 内容。
pub(crate) fn build_prosody_assets(
    tokens_raw: &str,
    lexicon_zh_raw: &str,
    jieba_dict_raw: Option<&str>,
    user_dict_raw: Option<&str>,
) -> Option<(String, String)> {
    let token_map = parse_symbol_table(tokens_raw);
    if token_map.len() < 10 {
        return None;
    }

    let (alphabet, boundary_sym, derived_tokens) = if token_map.contains_key("→")
        && token_map.contains_key("↗")
        && token_map.contains_key("↓")
        && token_map.contains_key("↘")
    {
        let space_id = *token_map.get(" ")?;
        // 选取单个 BMP 码点且非空白的符号映射到 space_id（满足 sherpa-onnx InitTokens 单 UTF-32 码点约束，且不被 SplitUtf8 空白过滤吃掉）
        let boundary = ["▁", "·", "¦"]
            .into_iter()
            .find(|cand| !token_map.contains_key(*cand))?;
        let mut out = tokens_raw.to_owned();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("{boundary} {space_id}\n"));
        (KokoroAlphabet::V10Ipa, boundary, out)
    } else if token_map.contains_key("/")
        && token_map.contains_key("1")
        && token_map.contains_key("2")
        && token_map.contains_key("3")
        && token_map.contains_key("4")
    {
        (KokoroAlphabet::V11Zhuyin, "/", tokens_raw.to_owned())
    } else {
        return None;
    };

    let mut single_chars: HashMap<char, Vec<String>> = HashMap::with_capacity(24_000);
    let mut raw_entries: Vec<(&str, Vec<&str>)> = Vec::with_capacity(50_000);
    let mut seen_words: HashSet<String> = HashSet::with_capacity(120_000);

    for line in lexicon_zh_raw.lines() {
        let mut parts = line.split_whitespace();
        let Some(word) = parts.next() else {
            continue;
        };
        let phones: Vec<&str> = parts.collect();
        if phones.is_empty()
            || !phones
                .iter()
                .all(|&p| p == boundary_sym || token_map.contains_key(p))
        {
            continue;
        }
        if seen_words.insert(word.to_owned()) {
            let mut chars = word.chars();
            if let (Some(ch), None) = (chars.next(), chars.next())
                && is_cjk(ch)
            {
                let mut clean: Vec<String> = phones
                    .iter()
                    .copied()
                    .filter(|&p| p != boundary_sym)
                    .map(str::to_owned)
                    .collect();
                if !clean.is_empty() {
                    single_chars
                        .entry(ch)
                        .or_insert_with(|| std::mem::take(&mut clean));
                }
            }
            raw_entries.push((word, phones));
        }
    }

    if single_chars.is_empty() {
        return None;
    }

    let mut out_lexicon = String::with_capacity(lexicon_zh_raw.len().saturating_mul(2) + 512_000);
    // 记录原词表中多字词（多音字正确读音）的分字音节表，供后续拼装 3~6 字复合词时优先复用，防止被单字默认音覆盖
    let mut known_phrases: HashMap<String, Vec<Vec<String>>> = HashMap::with_capacity(48_000);

    // 1. 保留原 lexicon-zh.txt 全部多音词与单字读音，对可精确对齐字数的词条补齐普通话变调并追加词界记号
    for (word, phones) in raw_entries {
        let clean_phones: Vec<String> = phones
            .into_iter()
            .filter(|&p| p != boundary_sym)
            .map(str::to_owned)
            .collect();
        if clean_phones.is_empty() {
            continue;
        }
        let chars: Vec<char> = word.chars().collect();
        let final_phones = if chars.len() >= 2 && chars.iter().copied().all(is_cjk) {
            if let Some(mut syllables) = split_syllables(&clean_phones, alphabet)
                && syllables.len() == chars.len()
            {
                if chars.len() <= 6 {
                    known_phrases.insert(word.to_owned(), syllables.clone());
                }
                apply_mandarin_sandhi(&chars, &mut syllables, alphabet);
                syllables.into_iter().flatten().collect()
            } else {
                clean_phones
            }
        } else {
            clean_phones
        };
        append_lexicon_line(&mut out_lexicon, word, &final_phones, boundary_sym);
    }

    // 2. 增补工坊高频词与口语变调词
    for &word in SPOKEN_AND_WORKSHOP_WORDS {
        try_append_synthesized_word(
            word,
            &single_chars,
            &known_phrases,
            &mut seen_words,
            alphabet,
            boundary_sym,
            &mut out_lexicon,
        );
    }

    // 3. 增补用户词典 `dict/user.dict.utf8`
    if let Some(user_raw) = user_dict_raw {
        for line in user_raw.lines() {
            let Some(word) = line.split_whitespace().next() else {
                continue;
            };
            if is_cjk_word(word, 2, 6) {
                try_append_synthesized_word(
                    word,
                    &single_chars,
                    &known_phrases,
                    &mut seen_words,
                    alphabet,
                    boundary_sym,
                    &mut out_lexicon,
                );
            }
        }
    }

    // 4. 增补 `dict/jieba.dict.utf8` 中的常用现代汉语词组（剔除人名 `nr*` 与低频噪声，防止前向最大匹配跨界误切）
    if let Some(jieba_raw) = jieba_dict_raw {
        for line in jieba_raw.lines() {
            let mut parts = line.split_whitespace();
            let Some(word) = parts.next() else {
                continue;
            };
            if !is_cjk_word(word, 2, 4) {
                continue;
            }
            let freq: u64 = parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let pos = parts.next().unwrap_or_default();
            if pos.starts_with("nr") {
                continue;
            }
            let char_len = word.chars().count();
            let min_freq = if pos.starts_with("ns") || pos.starts_with("nz") || char_len >= 4 {
                200
            } else {
                30
            };
            if freq < min_freq {
                continue;
            }
            try_append_synthesized_word(
                word,
                &single_chars,
                &known_phrases,
                &mut seen_words,
                alphabet,
                boundary_sym,
                &mut out_lexicon,
            );
        }
    }

    Some((derived_tokens, out_lexicon))
}

fn parse_symbol_table(tokens_raw: &str) -> HashMap<String, i32> {
    let mut map = HashMap::new();
    for line in tokens_raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            [only_id] => {
                if let Ok(id) = only_id.parse::<i32>() {
                    map.entry(" ".to_owned()).or_insert(id);
                }
            }
            [sym, id_str, ..] => {
                if let Ok(id) = id_str.parse::<i32>() {
                    map.entry((*sym).to_owned()).or_insert(id);
                }
            }
            [] => {}
        }
    }
    map
}

fn synthesize_word_syllables(
    word: &str,
    chars: &[char],
    single_chars: &HashMap<char, Vec<String>>,
    known_phrases: &HashMap<String, Vec<Vec<String>>>,
) -> Option<Vec<Vec<String>>> {
    // “调焦”中的“调”单字默认读去声 diào，优先从原词表“调整 / 调节”借阳平 tiáo 音节
    if word == "调焦" {
        let tiao = known_phrases
            .get("调整")
            .or_else(|| known_phrases.get("调节"))
            .and_then(|s| s.first())
            .or_else(|| single_chars.get(&'调'))?;
        let jiao = single_chars.get(&'焦')?;
        return Some(vec![tiao.clone(), jiao.clone()]);
    }

    // 三字词 `X + AB`（如“轻音乐”“这一切”“不了解”）若仅后两字命中原词表多音词、前两字未命中，优先保住后两字多音词
    if chars.len() == 3 {
        let prefix2: String = chars[..2].iter().collect();
        let suffix2: String = chars[1..].iter().collect();
        if !known_phrases.contains_key(&prefix2)
            && let Some(tail_syls) = known_phrases.get(&suffix2)
        {
            let head = single_chars.get(&chars[0])?;
            let mut out = Vec::with_capacity(3);
            out.push(head.clone());
            out.extend(tail_syls.iter().cloned());
            return Some(out);
        }
    }

    let mut out = Vec::with_capacity(chars.len());
    let mut pos = 0usize;
    while pos < chars.len() {
        let max_span = (chars.len() - pos).min(6);
        let mut matched_span = 0usize;
        if max_span >= 2 {
            for span in (2..=max_span).rev() {
                if pos == 0 && span == chars.len() {
                    continue;
                }
                let sub: String = chars[pos..pos + span].iter().collect();
                if let Some(sub_syls) = known_phrases.get(&sub) {
                    out.extend(sub_syls.iter().cloned());
                    matched_span = span;
                    break;
                }
            }
        }
        if matched_span > 0 {
            pos += matched_span;
        } else {
            out.push(single_chars.get(&chars[pos])?.clone());
            pos += 1;
        }
    }
    Some(out)
}

fn try_append_synthesized_word(
    word: &str,
    single_chars: &HashMap<char, Vec<String>>,
    known_phrases: &HashMap<String, Vec<Vec<String>>>,
    seen_words: &mut HashSet<String>,
    alphabet: KokoroAlphabet,
    boundary_sym: &str,
    out_lexicon: &mut String,
) {
    if seen_words.contains(word) {
        return;
    }
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 2 {
        return;
    }
    let Some(mut syllables) = synthesize_word_syllables(word, &chars, single_chars, known_phrases)
    else {
        return;
    };
    seen_words.insert(word.to_owned());
    apply_mandarin_sandhi(&chars, &mut syllables, alphabet);
    let flat: Vec<String> = syllables.into_iter().flatten().collect();
    append_lexicon_line(out_lexicon, word, &flat, boundary_sym);
}

fn append_lexicon_line(out: &mut String, word: &str, phones: &[String], boundary_sym: &str) {
    out.push_str(word);
    for p in phones {
        out.push(' ');
        out.push_str(p);
    }
    if phones.last().map(String::as_str) != Some(boundary_sym) {
        out.push(' ');
        out.push_str(boundary_sym);
    }
    out.push('\n');
}

fn split_syllables(phones: &[String], alphabet: KokoroAlphabet) -> Option<Vec<Vec<String>>> {
    let mut syllables = Vec::new();
    let mut current = Vec::new();
    for tok in phones {
        current.push(tok.clone());
        if alphabet.tone_of_token(tok).is_some() {
            syllables.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        match alphabet {
            // v1.0 词末轻声音节无箭头记号
            KokoroAlphabet::V10Ipa => syllables.push(current),
            KokoroAlphabet::V11Zhuyin => return None,
        }
    }
    (!syllables.is_empty()).then_some(syllables)
}

fn syllable_tone(syllable: &[String], alphabet: KokoroAlphabet) -> u8 {
    syllable
        .last()
        .and_then(|tok| alphabet.tone_of_token(tok))
        .unwrap_or(5)
}

fn set_syllable_tone(syllable: &mut [String], new_tone: u8, alphabet: KokoroAlphabet) {
    if let Some(last) = syllable.last_mut()
        && matches!(alphabet.tone_of_token(last), Some(1..=4))
        && let Some(replacement) = alphabet.tone_token(new_tone)
    {
        *last = replacement.to_owned();
    }
}

fn is_yi_literal_prev(ch: char) -> bool {
    matches!(
        ch,
        '第' | '初'
            | '零'
            | '〇'
            | '一'
            | '二'
            | '三'
            | '四'
            | '五'
            | '六'
            | '七'
            | '八'
            | '九'
            | '十'
            | '百'
            | '千'
            | '万'
            | '亿'
    )
}

fn is_yi_literal_next(ch: char) -> bool {
    matches!(
        ch,
        '零' | '〇'
            | '一'
            | '二'
            | '三'
            | '四'
            | '五'
            | '六'
            | '七'
            | '八'
            | '九'
            | '十'
            | '月'
            | '日'
            | '号'
            | '楼'
            | '路'
            | '世'
    )
}

/// 普通话词内连读变调（先跑“一”变调、再跑“不”变调、最后上声连读变调；对已变调词条保持幂等）。
fn apply_mandarin_sandhi(chars: &[char], syllables: &mut [Vec<String>], alphabet: KokoroAlphabet) {
    let len = chars.len();
    if len < 2 || syllables.len() != len {
        return;
    }

    // 1. “一”变调（须先于“不”变调，确保“不一般 [4,1,1]”先变为 `[4,4,1]` 再联动变为 `bú yì bān [2,4,1]`）
    for i in 0..len - 1 {
        if chars[i] != '一' || syllable_tone(&syllables[i], alphabet) != 1 {
            continue;
        }
        if (i > 0 && is_yi_literal_prev(chars[i - 1])) || is_yi_literal_next(chars[i + 1]) {
            continue;
        }
        match syllable_tone(&syllables[i + 1], alphabet) {
            4 => set_syllable_tone(&mut syllables[i], 2, alphabet),
            1..=3 => set_syllable_tone(&mut syllables[i], 4, alphabet),
            _ => {}
        }
    }

    // 2. “不”变调：去声前变阳平（如“不要 bú yào”“切不透 qiē bú tòu”“不一般 bú yì bān”）
    for i in 0..len - 1 {
        if chars[i] == '不'
            && syllable_tone(&syllables[i], alphabet) == 4
            && syllable_tone(&syllables[i + 1], alphabet) == 4
        {
            set_syllable_tone(&mut syllables[i], 2, alphabet);
        }
    }

    // 3. 上声（三声）连读变调：四字全上声按 2+2 节律变 `2 3 2 3`，其余相邻三声前字变阳平（你好 ní hǎo、走两遍 zóu liǎng biàn）
    if len == 4 && syllables.iter().all(|s| syllable_tone(s, alphabet) == 3) {
        set_syllable_tone(&mut syllables[0], 2, alphabet);
        set_syllable_tone(&mut syllables[2], 2, alphabet);
    } else {
        for i in 0..len - 1 {
            if syllable_tone(&syllables[i], alphabet) == 3
                && syllable_tone(&syllables[i + 1], alphabet) == 3
            {
                set_syllable_tone(&mut syllables[i], 2, alphabet);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_v10_tokens() -> &'static str {
        "$\t0\n,\t3\n.\t4\n \t16\na\t43\ne\t47\ni\t51\nj\t52\nk\t53\nl\t54\nm\t55\nn\t56\no\t57\np\t58\ns\t61\nt\t62\nu\t63\nw\t65\nx\t66\ny\t67\nɕ\t72\nɤ\t75\nə\t76\nɛ\t77\nɨ\t82\nŋ\t86\nʂ\t92\nʈ\t95\nʰ\t99\n→\t101\n↗\t102\n↓\t103\n↘\t104\n"
    }

    fn sample_v10_lexicon() -> &'static str {
        concat!(
            "你 n i ↓\n",
            "好 x a u ↓\n",
            "一 i →\n",
            "第 t i ↘\n",
            "月 y e ↘\n",
            "号 x a u ↘\n",
            "般 p a n →\n",
            "下 ɕ j a ↘\n",
            "不 p u ↘\n",
            "切 t ɕ ʰ j e →\n",
            "透 t ʰ o u ↘\n",
            "三 s a n →\n",
            "毫 x a u ↗\n",
            "米 m i ↓\n",
            "椴 t w a n ↘\n",
            "木 m u ↘\n",
            "刻 k ɤ ↘\n",
            "字 t s ɨ ↘\n",
            "走 t s o u ↓\n",
            "两 l j a ŋ ↓\n",
            "遍 p j ɛ n ↘\n",
            "对 t w e i ↘\n",
            "焦 t ɕ j a u →\n",
            "注 ʈ ʂ u ↘\n",
            "意 i ↘\n",
            "的 t ə\n",
            "展 ʈ ʂ a n ↓\n",
            "览 l a n ↓\n",
            "馆 k w a n ↓\n",
            "美 m e i ↓\n",
            "年 n j ɛ n ↗\n",
            "层 t s ʰ ə ŋ ↗\n",
            "定 t i ŋ ↘\n",
            "这 ʈ ʂ ɤ ↘\n",
            "音 i n →\n",
            "乐 l ɤ ↘\n",
            "会 x w e i ↘\n",
            "轻 t ɕ ʰ i ŋ →\n",
            "调 t j a u ↘\n",
            "整 ʈ ʂ ə ŋ ↓\n",
            // 原词表多音词（“一切”中的“切”为去声，“音乐”中的“乐”为 yuè，“调整”中的“调”为阳平 tiáo）
            "一切 i → t ɕ ʰ j e ↘\n",
            "音乐 i n → y e ↘\n",
            "调整 t ʰ j a u ↗ ʈ ʂ ə ŋ ↓\n",
        )
    }

    fn lookup_line<'a>(lexicon: &'a str, word: &str) -> Option<&'a str> {
        let prefix = format!("{word} ");
        lexicon.lines().find(|line| line.starts_with(&prefix))
    }

    #[test]
    fn v10_ipa_adds_word_boundary_alias_groups_words_and_applies_mandarin_sandhi() {
        let jieba = "展览馆 120 n\n王小二 500 nr\n罕见词 3 n\n";
        let (tokens_out, lexicon_out) =
            build_prosody_assets(sample_v10_tokens(), sample_v10_lexicon(), Some(jieba), None)
                .expect("build v10 prosody assets");

        assert!(
            tokens_out.lines().any(|l| l == "▁ 16"),
            "v1.0 tokens.txt 须追加单码点非空白词界别名映射到 space_id=16: {tokens_out}"
        );
        for sym in parse_symbol_table(&tokens_out).keys() {
            assert_eq!(
                sym.chars().count(),
                1,
                "sherpa-onnx InitTokens 要求每个 token 严格为单个 UTF-32 码点: {sym:?}"
            );
        }

        // 工坊与高频双音/三音词组自动补齐并带词末 `▁` 词界，词内音节紧密相连无空格分隔符
        assert_eq!(
            lookup_line(&lexicon_out, "三毫米"),
            Some("三毫米 s a n → x a u ↗ m i ↓ ▁")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "椴木"),
            Some("椴木 t w a n ↘ m u ↘ ▁")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "刻字"),
            Some("刻字 k ɤ ↘ t s ɨ ↘ ▁")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "对焦"),
            Some("对焦 t w e i ↘ t ɕ j a u → ▁")
        );

        // 普通话变调：三声连读、不、一，且多音词原读音不被单字拼读覆盖
        assert_eq!(
            lookup_line(&lexicon_out, "你好"),
            Some("你好 n i ↗ x a u ↓ ▁")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "走两遍"),
            Some("走两遍 t s o u ↗ l j a ŋ ↓ p j ɛ n ↘ ▁")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "展览馆"),
            Some("展览馆 ʈ ʂ a n ↗ l a n ↗ k w a n ↓ ▁")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "切不透"),
            Some("切不透 t ɕ ʰ j e → p u ↗ t ʰ o u ↘ ▁")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "一般"),
            Some("一般 i ↘ p a n → ▁")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "一下"),
            Some("一下 i ↗ ɕ j a ↘ ▁")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "一切"),
            Some("一切 i ↗ t ɕ ʰ j e ↘ ▁"),
            "原词表多音字（切=去声）保留原声调并补上‘一’的阳平变调"
        );
        assert_eq!(lookup_line(&lexicon_out, "的"), Some("的 t ə ▁"));

        // 序数与日期中的“一”保持本调阴平，数量词“一年 / 一层”正常变去声，“不一般”先“一”后“不”联动变调，且复合词保留子多音词读音
        let (_, ordinal_lex) = build_prosody_assets(
            sample_v10_tokens(),
            sample_v10_lexicon(),
            Some("音乐会 500 n\n轻音乐 300 n\n这一切 600 r\n不一般 200 a\n不一定 500 d\n一年 800 t\n一层 400 q\n第一层 200 q\n"),
            Some("第一 100 m\n一月 100 t\n一号 100 q\n好美展览 100 nz\n"),
        )
        .unwrap();
        assert_eq!(lookup_line(&ordinal_lex, "第一"), Some("第一 t i ↘ i → ▁"));
        assert_eq!(lookup_line(&ordinal_lex, "一月"), Some("一月 i → y e ↘ ▁"));
        assert_eq!(
            lookup_line(&ordinal_lex, "一号"),
            Some("一号 i → x a u ↘ ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "第一层"),
            Some("第一层 t i ↘ i → t s ʰ ə ŋ ↗ ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "一年"),
            Some("一年 i ↘ n j ɛ n ↗ ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "一层"),
            Some("一层 i ↘ t s ʰ ə ŋ ↗ ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "不一般"),
            Some("不一般 p u ↗ i ↘ p a n → ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "不一定"),
            Some("不一定 p u ↘ i ↗ t i ŋ ↘ ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "音乐会"),
            Some("音乐会 i n → y e ↘ x w e i ↘ ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "轻音乐"),
            Some("轻音乐 t ɕ ʰ i ŋ → i n → y e ↘ ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "这一切"),
            Some("这一切 ʈ ʂ ɤ ↘ i ↗ t ɕ ʰ j e ↘ ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "调焦"),
            Some("调焦 t ʰ j a u ↗ t ɕ j a u → ▁")
        );
        assert_eq!(
            lookup_line(&ordinal_lex, "好美展览"),
            Some("好美展览 x a u ↗ m e i ↓ ʈ ʂ a n ↗ l a n ↓ ▁")
        );
        assert!(lookup_line(&lexicon_out, "王小二").is_none());
        assert!(lookup_line(&lexicon_out, "罕见词").is_none());
    }

    #[test]
    fn v11_zhuyin_reuses_slash_boundary_and_stub_files_fall_back_cleanly() {
        let v11_tokens = "$\t0\n,\t3\n.\t4\n \t16\n/\t17\n1\t21\n2\t22\n3\t23\n4\t24\n5\t25\nㄋ\t30\nㄧ\t31\nㄏ\t32\nㄠ\t33\nㄅ\t34\nㄨ\t35\nㄊ\t36\nㄡ\t37\n";
        let v11_lexicon = "你 ㄋ ㄧ 3\n好 ㄏ ㄠ 3\n不 ㄅ ㄨ 4\n透 ㄊ ㄡ 4\n";
        let (tokens_out, lexicon_out) =
            build_prosody_assets(v11_tokens, v11_lexicon, None, None).expect("v11 assets");
        assert_eq!(
            tokens_out, v11_tokens,
            "v1.1 已有 `/` 记号，不重复改写 tokens.txt"
        );
        assert_eq!(
            lookup_line(&lexicon_out, "你好"),
            Some("你好 ㄋ ㄧ 2 ㄏ ㄠ 3 /")
        );
        assert_eq!(
            lookup_line(&lexicon_out, "不透"),
            Some("不透 ㄅ ㄨ 2 ㄊ ㄡ 4 /")
        );

        // 单元测试桩文件安全回退
        assert!(build_prosody_assets("stub", "stub", None, None).is_none());

        // 磁盘缓存命中复用验证
        let unique = format!(
            "moss-kokoro-prosody-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let root = std::env::temp_dir().join(unique);
        let kokoro_dir = root.join("kokoro-multi-lang-v1_0");
        fs::create_dir_all(kokoro_dir.join("dict")).unwrap();
        fs::write(kokoro_dir.join("tokens.txt"), sample_v10_tokens()).unwrap();
        fs::write(kokoro_dir.join("lexicon-zh.txt"), sample_v10_lexicon()).unwrap();

        let (tok1, lex1) = prepare_kokoro_prosody_files(&kokoro_dir).expect("first prepare");
        assert!(tok1.starts_with(root.join(".tts-previews").join("kokoro")));
        let mtime1 = fs::metadata(&lex1).unwrap().modified().unwrap();
        let (tok2, lex2) = prepare_kokoro_prosody_files(&kokoro_dir).expect("cached prepare");
        assert_eq!((&tok1, &lex1), (&tok2, &lex2));
        assert_eq!(fs::metadata(&lex2).unwrap().modified().unwrap(), mtime1);

        // 等长修改 user.dict.utf8 时指纹变更并自动刷新派生词表
        let user_dict = kokoro_dir.join("dict").join("user.dict.utf8");
        fs::write(&user_dict, "好展 100 n\n").unwrap();
        let (_, lex3) = prepare_kokoro_prosody_files(&kokoro_dir).expect("user dict v1");
        assert!(lookup_line(&fs::read_to_string(&lex3).unwrap(), "好展").is_some());
        fs::write(&user_dict, "美馆 100 n\n").unwrap();
        let (_, lex4) = prepare_kokoro_prosody_files(&kokoro_dir).expect("user dict v2");
        let lex4_text = fs::read_to_string(&lex4).unwrap();
        assert!(lookup_line(&lex4_text, "美馆").is_some());
        assert!(lookup_line(&lex4_text, "好展").is_none());

        let _ = fs::remove_dir_all(&root);
    }
}
