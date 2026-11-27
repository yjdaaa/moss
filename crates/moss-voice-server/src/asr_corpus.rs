//! VB1/VB2 识别（ASR）真实录音语料采集、保留清理与标注存储。
//!
//! 隐私与安全边界：
//! - 默认关闭；仅允许经过来源校验的桌面主窗口（`require_private_settings_window` + `voice-private-main.json`）确认开启。
//! - 仅保存真实小智语音轮次解码后的 16 kHz 单声道 WAV 与识别元数据到 `<runtime>/asr-corpus/<YYYY-MM-DD>/<turn-id>.{wav,json}`；
//!   VA 模拟轮次没有真实录音，一律不保存。
//! - 按条数（默认 500 条）与天数（默认 7 天）双上限“先到者先清理”。
//! - 严禁进入诊断包（`diagnostics_bundle`）或 `ProcessTrace` 附件；绝不参与声纹识别或身份授权。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};

use crate::audio::Resampler;

pub const ASR_CORPUS_DIR_REL: &str = "asr-corpus";
pub const ASR_CORPUS_SAMPLE_RATE: u32 = 16_000;
pub const DEFAULT_MAX_ITEMS: u32 = 500;
pub const DEFAULT_MAX_DAYS: u32 = 7;
pub const MAX_LABEL_CHARS: usize = 500;
pub const MAX_WAV_BYTES: usize = 4 * 1024 * 1024;
pub const VALID_CORPUS_TAGS: &[&str] = &["命令词", "数字", "材料名", "有噪声"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsrCorpusConfig {
    pub enabled: bool,
    pub max_items: u32,
    pub max_days: u32,
}

impl Default for AsrCorpusConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_items: DEFAULT_MAX_ITEMS,
            max_days: DEFAULT_MAX_DAYS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterimEntry {
    pub at_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointTiming {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vad_start_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vad_end_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utterance_end_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_asr_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_wait_ms: Option<u64>,
}

/// 一轮真实语音结束时交给语料库采集的输入。
#[derive(Debug, Clone)]
pub struct CapturedUtterance {
    pub session_id: String,
    pub device_id: String,
    pub turn: u64,
    pub captured_at_ms: u64,
    pub simulated: bool,
    pub pcm: Vec<i16>,
    pub sample_rate: u32,
    pub raw_text: String,
    pub corrected_text: String,
    pub interims: Vec<InterimEntry>,
    pub asr_model: String,
    pub vad_engine: String,
    pub endpoint: EndpointTiming,
    pub outcome: String,
}

/// 落盘到 `<runtime>/asr-corpus/<date>/<turn-id>.json` 的识别元数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsrCorpusMeta {
    pub turn_id: String,
    pub date: String,
    pub captured_at_ms: u64,
    pub device_id: String,
    pub session_id: String,
    pub turn: u64,
    pub simulated: bool,
    pub sample_rate: u32,
    pub duration_ms: u64,
    pub asr_model: String,
    pub vad_engine: String,
    #[serde(default)]
    pub endpoint: EndpointTiming,
    #[serde(default)]
    pub interims: Vec<InterimEntry>,
    pub raw_text: String,
    pub corrected_text: String,
    pub outcome: String,
}

/// 落盘到 `<runtime>/asr-corpus/<date>/<turn-id>.label.json` 的人工标注。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsrCorpusLabel {
    pub turn_id: String,
    pub date: String,
    pub reference_text: String,
    pub verified_correct: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    pub updated_at_ms: u64,
}

/// 语音中心“识别语料”列表展示条目（不含本机绝对路径）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsrCorpusItem {
    #[serde(flatten)]
    pub meta: AsrCorpusMeta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<AsrCorpusLabel>,
}

pub struct AsrCorpusStore {
    root: PathBuf,
    enabled: AtomicBool,
    config: RwLock<AsrCorpusConfig>,
    io_lock: Mutex<()>,
    now_ms_fn: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl AsrCorpusStore {
    pub fn new(root: PathBuf, initial: AsrCorpusConfig) -> Self {
        Self::with_clock(root, initial, Arc::new(crate::unix_now_ms))
    }

    pub fn with_clock(
        root: PathBuf,
        initial: AsrCorpusConfig,
        now_ms_fn: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self {
            root,
            enabled: AtomicBool::new(initial.enabled),
            config: RwLock::new(initial),
            io_lock: Mutex::new(()),
            now_ms_fn,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 零锁热路径判断：关闭时会话不复制音频、不记录中间结果列表。
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn config(&self) -> AsrCorpusConfig {
        self.config
            .read()
            .map(|c| *c)
            .unwrap_or_else(|e| *e.into_inner())
    }

    pub fn set_config(&self, next: AsrCorpusConfig) -> Result<(), String> {
        let clamped = AsrCorpusConfig {
            enabled: next.enabled,
            max_items: next.max_items.clamp(1, 5000),
            max_days: next.max_days.clamp(1, 90),
        };
        if let Ok(mut guard) = self.config.write() {
            *guard = clamped;
        }
        self.enabled.store(clamped.enabled, Ordering::Release);
        let _io = self
            .io_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = self.prune_locked((self.now_ms_fn)());
        Ok(())
    }

    /// 采集一轮真实语音：默认关闭时不落盘；VA 模拟轮次或空音频一律拒绝保存。
    pub fn record(&self, utt: CapturedUtterance) -> Result<Option<AsrCorpusMeta>, String> {
        if !self.is_enabled() || utt.simulated || utt.pcm.is_empty() || utt.sample_rate == 0 {
            return Ok(None);
        }
        let pcm_16k = if utt.sample_rate == ASR_CORPUS_SAMPLE_RATE {
            utt.pcm
        } else {
            let mut resampler = Resampler::new(utt.sample_rate, ASR_CORPUS_SAMPLE_RATE);
            resampler.process(&utt.pcm)
        };
        if pcm_16k.is_empty() {
            return Ok(None);
        }
        let duration_ms = (pcm_16k.len() as u64) * 1000 / u64::from(ASR_CORPUS_SAMPLE_RATE);
        let wav = moss_voice_providers::wav::encode_wav(&pcm_16k, ASR_CORPUS_SAMPLE_RATE);
        if wav.len() > MAX_WAV_BYTES {
            return Ok(None);
        }

        let captured_at_ms = if utt.captured_at_ms > 0 {
            utt.captured_at_ms
        } else {
            (self.now_ms_fn)()
        };
        let date = local_date_from_ms(captured_at_ms);
        let turn_id = sanitize_turn_id(&utt.session_id, utt.turn);
        let meta = AsrCorpusMeta {
            turn_id: turn_id.clone(),
            date: date.clone(),
            captured_at_ms,
            device_id: scrub_meta_text(&utt.device_id, 64),
            session_id: scrub_meta_text(&utt.session_id, 64),
            turn: utt.turn,
            simulated: false,
            sample_rate: ASR_CORPUS_SAMPLE_RATE,
            duration_ms,
            asr_model: scrub_meta_text(&utt.asr_model, 128),
            vad_engine: scrub_meta_text(&utt.vad_engine, 64),
            endpoint: utt.endpoint,
            interims: utt
                .interims
                .into_iter()
                .take(64)
                .map(|entry| InterimEntry {
                    at_ms: entry.at_ms,
                    text: scrub_meta_text(&entry.text, 500),
                })
                .collect(),
            raw_text: scrub_meta_text(&utt.raw_text, 1000),
            corrected_text: scrub_meta_text(&utt.corrected_text, 1000),
            outcome: scrub_meta_text(&utt.outcome, 32),
        };

        let _io = self
            .io_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.is_enabled() {
            return Ok(None);
        }
        ensure_private_dir(&self.root)?;
        let day_dir = self.root.join(&date);
        ensure_private_dir(&day_dir)?;

        let wav_path = day_dir.join(format!("{turn_id}.wav"));
        let meta_path = day_dir.join(format!("{turn_id}.json"));
        write_private_atomic(&wav_path, &wav)?;
        let meta_bytes =
            serde_json::to_vec_pretty(&meta).map_err(|e| format!("序列化语料元数据失败：{e}"))?;
        write_private_atomic(&meta_path, &meta_bytes)?;

        let _ = self.prune_locked((self.now_ms_fn)());
        Ok(Some(meta))
    }

    pub fn list(&self) -> Result<Vec<AsrCorpusItem>, String> {
        let _io = self
            .io_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut items = self.scan_locked()?;
        items.sort_by(|a, b| {
            b.meta
                .captured_at_ms
                .cmp(&a.meta.captured_at_ms)
                .then_with(|| b.meta.date.cmp(&a.meta.date))
                .then_with(|| b.meta.turn_id.cmp(&a.meta.turn_id))
        });
        Ok(items)
    }

    pub fn read_audio_wav(&self, date: &str, turn_id: &str) -> Result<(Vec<u8>, u64), String> {
        validate_date(date)?;
        validate_turn_id(turn_id)?;
        let _io = self
            .io_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let day_dir = self.root.join(date);
        reject_symlink(&self.root)?;
        reject_symlink(&day_dir)?;
        let wav_path = day_dir.join(format!("{turn_id}.wav"));
        reject_symlink(&wav_path)?;
        let meta = fs::metadata(&wav_path).map_err(|_| "找不到对应的识别录音文件".to_owned())?;
        if !meta.is_file() || meta.len() as usize > MAX_WAV_BYTES {
            return Err("识别录音文件无效或超出大小上限".into());
        }
        let bytes = fs::read(&wav_path).map_err(|e| format!("读取识别录音失败：{e}"))?;
        let duration_ms = fs::read_to_string(day_dir.join(format!("{turn_id}.json")))
            .ok()
            .and_then(|s| serde_json::from_str::<AsrCorpusMeta>(&s).ok())
            .map(|m| m.duration_ms)
            .unwrap_or_else(|| {
                bytes
                    .len()
                    .saturating_sub(44)
                    .saturating_div(2)
                    .saturating_mul(1000) as u64
                    / u64::from(ASR_CORPUS_SAMPLE_RATE)
            });
        Ok((bytes, duration_ms))
    }

    pub fn save_label(
        &self,
        date: &str,
        turn_id: &str,
        reference_text: &str,
        verified_correct: bool,
        tags: Vec<String>,
    ) -> Result<AsrCorpusLabel, String> {
        validate_date(date)?;
        validate_turn_id(turn_id)?;
        let normalized_text = validate_reference_text(reference_text)?;
        let normalized_tags = normalize_tags(&tags)?;

        let _io = self
            .io_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let day_dir = self.root.join(date);
        reject_symlink(&self.root)?;
        reject_symlink(&day_dir)?;
        let meta_path = day_dir.join(format!("{turn_id}.json"));
        let wav_path = day_dir.join(format!("{turn_id}.wav"));
        if !meta_path.is_file() || !wav_path.is_file() {
            return Err("该识别语料不存在或已被清理".into());
        }
        let label = AsrCorpusLabel {
            turn_id: turn_id.to_owned(),
            date: date.to_owned(),
            reference_text: normalized_text,
            verified_correct,
            tags: normalized_tags,
            updated_at_ms: (self.now_ms_fn)(),
        };
        let label_path = day_dir.join(format!("{turn_id}.label.json"));
        reject_symlink(&label_path)?;
        let bytes =
            serde_json::to_vec_pretty(&label).map_err(|e| format!("序列化语料标注失败：{e}"))?;
        write_private_atomic(&label_path, &bytes)?;
        Ok(label)
    }

    pub fn delete_item(&self, date: &str, turn_id: &str) -> Result<bool, String> {
        validate_date(date)?;
        validate_turn_id(turn_id)?;
        let _io = self
            .io_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let day_dir = self.root.join(date);
        if !day_dir.is_dir() {
            return Ok(false);
        }
        reject_symlink(&self.root)?;
        reject_symlink(&day_dir)?;
        let mut removed = false;
        for ext in ["wav", "json", "label.json"] {
            let path = day_dir.join(format!("{turn_id}.{ext}"));
            if path.exists() {
                reject_symlink(&path)?;
                fs::remove_file(&path).map_err(|e| format!("删除语料文件失败：{e}"))?;
                removed = true;
            }
        }
        remove_dir_if_empty(&day_dir);
        Ok(removed)
    }

    pub fn clear_all(&self) -> Result<usize, String> {
        let _io = self
            .io_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.root.exists() {
            return Ok(0);
        }
        reject_symlink(&self.root)?;
        let mut removed_items = 0usize;
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(0);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let Some(date_str) = name.to_str() else {
                continue;
            };
            if validate_date(date_str).is_err() || is_symlink(&path) || !path.is_dir() {
                continue;
            }
            if let Ok(files) = fs::read_dir(&path) {
                for file in files.flatten() {
                    let fpath = file.path();
                    if is_symlink(&fpath) || !fpath.is_file() {
                        continue;
                    }
                    let fname = file.file_name();
                    let Some(fname_str) = fname.to_str() else {
                        continue;
                    };
                    if fname_str.ends_with(".wav")
                        || fname_str.ends_with(".json")
                        || fname_str.ends_with(".tmp")
                    {
                        if fname_str.ends_with(".wav") {
                            removed_items += 1;
                        }
                        let _ = fs::remove_file(&fpath);
                    }
                }
            }
            remove_dir_if_empty(&path);
        }
        Ok(removed_items)
    }

    pub fn prune(&self) -> Result<usize, String> {
        let _io = self
            .io_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.prune_locked((self.now_ms_fn)())
    }

    fn prune_locked(&self, now_ms: u64) -> Result<usize, String> {
        if !self.root.is_dir() || is_symlink(&self.root) {
            return Ok(0);
        }
        let cfg = self.config();
        let max_age_ms = u64::from(cfg.max_days).saturating_mul(86_400_000);
        let cutoff_ms = now_ms.saturating_sub(max_age_ms);
        let mut items = self.scan_locked()?;
        let mut removed = 0usize;

        // 1. 先按保留天数清理超期条目
        let mut kept = Vec::with_capacity(items.len());
        for item in items.drain(..) {
            if item.meta.captured_at_ms < cutoff_ms {
                self.remove_item_files_locked(&item.meta.date, &item.meta.turn_id);
                removed += 1;
            } else {
                kept.push(item);
            }
        }

        // 2. 再按保留条数上限清理最早的条目（先到者先清理）
        let max_items = cfg.max_items as usize;
        if kept.len() > max_items {
            kept.sort_by(|a, b| {
                a.meta
                    .captured_at_ms
                    .cmp(&b.meta.captured_at_ms)
                    .then_with(|| a.meta.date.cmp(&b.meta.date))
                    .then_with(|| a.meta.turn_id.cmp(&b.meta.turn_id))
            });
            let excess = kept.len() - max_items;
            for item in kept.into_iter().take(excess) {
                self.remove_item_files_locked(&item.meta.date, &item.meta.turn_id);
                removed += 1;
            }
        }

        // 清理空日期目录
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = entry.file_name().to_str()
                    && validate_date(name).is_ok()
                    && !is_symlink(&path)
                    && path.is_dir()
                {
                    remove_dir_if_empty(&path);
                }
            }
        }
        Ok(removed)
    }

    fn remove_item_files_locked(&self, date: &str, turn_id: &str) {
        let day_dir = self.root.join(date);
        if is_symlink(&day_dir) || !day_dir.is_dir() {
            return;
        }
        for ext in ["wav", "json", "label.json"] {
            let path = day_dir.join(format!("{turn_id}.{ext}"));
            if !is_symlink(&path) && path.is_file() {
                let _ = fs::remove_file(&path);
            }
        }
    }

    fn scan_locked(&self) -> Result<Vec<AsrCorpusItem>, String> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        reject_symlink(&self.root)?;
        let mut out = Vec::new();
        let Ok(days) = fs::read_dir(&self.root) else {
            return Ok(out);
        };
        for day in days.flatten() {
            let day_path = day.path();
            let day_name = day.file_name();
            let Some(date_str) = day_name.to_str() else {
                continue;
            };
            if validate_date(date_str).is_err() || is_symlink(&day_path) || !day_path.is_dir() {
                continue;
            }
            let Ok(files) = fs::read_dir(&day_path) else {
                continue;
            };
            for file in files.flatten() {
                let fpath = file.path();
                let fname = file.file_name();
                let Some(name_str) = fname.to_str() else {
                    continue;
                };
                if !name_str.ends_with(".json")
                    || name_str.ends_with(".label.json")
                    || is_symlink(&fpath)
                    || !fpath.is_file()
                {
                    continue;
                }
                let Some(turn_id) = name_str.strip_suffix(".json") else {
                    continue;
                };
                if validate_turn_id(turn_id).is_err() {
                    continue;
                }
                let wav_path = day_path.join(format!("{turn_id}.wav"));
                if is_symlink(&wav_path) || !wav_path.is_file() {
                    continue;
                }
                let Ok(raw) = fs::read_to_string(&fpath) else {
                    continue;
                };
                let Ok(mut meta) = serde_json::from_str::<AsrCorpusMeta>(&raw) else {
                    continue;
                };
                if meta.simulated {
                    continue;
                }
                meta.date = date_str.to_owned();
                meta.turn_id = turn_id.to_owned();
                let label_path = day_path.join(format!("{turn_id}.label.json"));
                let label = if !is_symlink(&label_path) && label_path.is_file() {
                    fs::read_to_string(&label_path)
                        .ok()
                        .and_then(|s| serde_json::from_str::<AsrCorpusLabel>(&s).ok())
                } else {
                    None
                };
                out.push(AsrCorpusItem { meta, label });
            }
        }
        Ok(out)
    }
}

fn scrub_meta_text(text: &str, max_chars: usize) -> String {
    let scrubbed = moss_voice_providers::scrub_text_with_options(text, false, 0, false).0;
    scrubbed.chars().take(max_chars).collect()
}

fn local_date_from_ms(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    Local
        .timestamp_opt(secs, nanos)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string())
}

fn sanitize_turn_id(session_id: &str, turn: u64) -> String {
    let safe_session: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(48)
        .collect();
    let prefix = if safe_session.is_empty() {
        "session"
    } else {
        safe_session.as_str()
    };
    format!("{prefix}-t{turn}")
}

pub fn validate_date(date: &str) -> Result<(), String> {
    let bytes = date.as_bytes();
    let ok = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err("日期格式无效（须为 YYYY-MM-DD）".into())
    }
}

pub fn validate_turn_id(turn_id: &str) -> Result<(), String> {
    if turn_id.is_empty()
        || turn_id.len() > 96
        || !turn_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err("语料编号无效".into());
    }
    Ok(())
}

fn validate_reference_text(raw: &str) -> Result<String, String> {
    if raw.chars().any(char::is_control) {
        return Err("正确文字不能包含控制字符".into());
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("正确文字不能为空".into());
    }
    if trimmed.chars().count() > MAX_LABEL_CHARS {
        return Err(format!("正确文字最多 {MAX_LABEL_CHARS} 个字符"));
    }
    Ok(trimmed.to_owned())
}

fn normalize_tags(tags: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for raw in tags {
        let tag = raw.trim();
        if tag.is_empty() {
            continue;
        }
        if !VALID_CORPUS_TAGS.contains(&tag) {
            return Err(format!("不支持的语料标签：{tag}"));
        }
        if !out.iter().any(|existing| existing == tag) {
            out.push(tag.to_owned());
        }
    }
    Ok(out)
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    if is_symlink(path) {
        return Err("拒绝访问符号链接路径".into());
    }
    Ok(())
}

fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    reject_symlink(dir)?;
    fs::create_dir_all(dir).map_err(|e| format!("创建识别语料目录失败：{e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    reject_symlink(path)?;
    let tmp = path.with_extension("tmp");
    reject_symlink(&tmp)?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| format!("写入语料临时文件失败：{e}"))?;
        file.write_all(bytes)
            .map_err(|e| format!("写入语料内容失败：{e}"))?;
        file.sync_all()
            .map_err(|e| format!("落盘语料文件失败：{e}"))?;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        fs::write(&tmp, bytes).map_err(|e| format!("写入语料临时文件失败：{e}"))?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("替换语料文件失败：{e}"))?;
    Ok(())
}

fn remove_dir_if_empty(dir: &Path) {
    if let Ok(mut it) = fs::read_dir(dir)
        && it.next().is_none()
    {
        let _ = fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn sample_utterance(
        turn: u64,
        at_ms: u64,
        simulated: bool,
        sample_rate: u32,
    ) -> CapturedUtterance {
        CapturedUtterance {
            session_id: "sess-abc".into(),
            device_id: "aa:bb:cc:11:22:33".into(),
            turn,
            captured_at_ms: at_ms,
            simulated,
            pcm: vec![1200; (sample_rate / 4) as usize],
            sample_rate,
            raw_text: "三毫米段木刻字".into(),
            corrected_text: "三毫米椴木刻字".into(),
            interims: vec![InterimEntry {
                at_ms: 180,
                text: "三毫米".into(),
            }],
            asr_model: "本地 sense_voice".into(),
            vad_engine: "本地判停 Silero v6.2".into(),
            endpoint: EndpointTiming {
                vad_start_ms: Some(100),
                vad_end_ms: Some(900),
                utterance_end_ms: Some(950),
                final_asr_ms: Some(1120),
                endpoint_wait_ms: Some(250),
            },
            outcome: "done".into(),
        }
    }

    #[test]
    fn default_disabled_and_simulated_turns_are_never_saved() {
        let dir = tempfile::tempdir().unwrap();
        let store = AsrCorpusStore::new(dir.path().join("asr-corpus"), AsrCorpusConfig::default());
        assert!(!store.is_enabled(), "识别语料采集必须默认关闭");
        assert!(
            store
                .record(sample_utterance(1, 1_760_000_000_000, false, 16_000))
                .unwrap()
                .is_none(),
            "开关关闭时不保存录音"
        );
        assert!(store.list().unwrap().is_empty());

        // 开启后，VA 模拟轮次仍然一律不保存
        store
            .set_config(AsrCorpusConfig {
                enabled: true,
                max_items: 500,
                max_days: 7,
            })
            .unwrap();
        assert!(
            store
                .record(sample_utterance(2, 1_760_000_001_000, true, 16_000))
                .unwrap()
                .is_none(),
            "模拟轮次绝不能保存进识别语料"
        );
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn real_turn_resamples_to_16k_wav_saves_label_and_supports_delete_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let now = Arc::new(AtomicU64::new(1_760_000_000_000));
        let clock = now.clone();
        let store = AsrCorpusStore::with_clock(
            dir.path().join("asr-corpus"),
            AsrCorpusConfig {
                enabled: true,
                max_items: 500,
                max_days: 7,
            },
            Arc::new(move || clock.load(Ordering::Acquire)),
        );

        // 输入 24 kHz 音频，应自动重采样并存为 16 kHz 单声道 WAV
        let saved = store
            .record(sample_utterance(
                1,
                now.load(Ordering::Acquire),
                false,
                24_000,
            ))
            .unwrap()
            .expect("真实轮次应成功保存");
        assert!(!saved.simulated);
        assert_eq!(saved.sample_rate, 16_000);
        assert_eq!(saved.interims.len(), 1);
        assert_eq!(saved.asr_model, "本地 sense_voice");

        let (wav, _dur) = store.read_audio_wav(&saved.date, &saved.turn_id).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            16_000
        );
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);

        // 保存标注并回读
        let label = store
            .save_label(
                &saved.date,
                &saved.turn_id,
                "三毫米椴木刻字",
                false,
                vec!["材料名".into(), "数字".into(), "材料名".into()],
            )
            .unwrap();
        assert_eq!(label.tags, vec!["材料名", "数字"]);
        assert!(
            dir.path()
                .join("asr-corpus")
                .join(&saved.date)
                .join(format!("{}.label.json", saved.turn_id))
                .is_file()
        );

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].label.as_ref().unwrap().reference_text,
            "三毫米椴木刻字"
        );

        // 拒绝非法路径与不支持标签
        assert!(store.read_audio_wav("../etc", &saved.turn_id).is_err());
        assert!(
            store
                .save_label(
                    &saved.date,
                    &saved.turn_id,
                    "ok",
                    true,
                    vec!["非法标签".into()]
                )
                .is_err()
        );

        // 删除单条与全量清空
        assert!(store.delete_item(&saved.date, &saved.turn_id).unwrap());
        assert!(store.list().unwrap().is_empty());

        store
            .record(sample_utterance(
                2,
                now.load(Ordering::Acquire),
                false,
                16_000,
            ))
            .unwrap();
        assert_eq!(store.clear_all().unwrap(), 1);
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn retention_enforces_both_max_items_and_max_days_limits() {
        let dir = tempfile::tempdir().unwrap();
        let day_ms = 86_400_000u64;
        let base_ms = 1_760_000_000_000u64;
        let now = Arc::new(AtomicU64::new(base_ms));
        let clock = now.clone();
        let store = AsrCorpusStore::with_clock(
            dir.path().join("asr-corpus"),
            AsrCorpusConfig {
                enabled: true,
                max_items: 3,
                max_days: 7,
            },
            Arc::new(move || clock.load(Ordering::Acquire)),
        );

        // 连续采集 5 条，条数上限 3 生效：保留最新 3 条（turn 3, 4, 5）
        for t in 1..=5u64 {
            let ts = base_ms + t * 1_000;
            now.store(ts, Ordering::Release);
            store
                .record(sample_utterance(t, ts, false, 16_000))
                .unwrap();
        }
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 3);
        let turns: Vec<u64> = listed.iter().map(|i| i.meta.turn).collect();
        assert_eq!(turns, vec![5, 4, 3]);

        // 时间推进 8 天后新增 1 条：超过 7 天的 3 条被清理，仅留新采的 1 条
        let future = base_ms + 8 * day_ms;
        now.store(future, Ordering::Release);
        store
            .record(sample_utterance(6, future, false, 16_000))
            .unwrap();
        let listed_after_days = store.list().unwrap();
        assert_eq!(listed_after_days.len(), 1);
        assert_eq!(listed_after_days[0].meta.turn, 6);
    }
}
