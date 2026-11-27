//! VD2 本机整体测试真人录音语料的存储与主窗口管理入口（仅经 `voice_bench_recordings_manage` 供受信任主窗口调用）。
//!
//! 隐私与安全边界：
//! - 录音统一重采样并落盘为 16 kHz 单声道 WAV，保存在 `<runtime>/voice-bench/recordings/<speaker>/<corpus-id>.{wav,json}`；
//! - 仅保存说话人标签，不涉及身份识别、绝不用于声纹识别；
//! - 绝不进入 `diagnostics_bundle()`、`ProcessTrace` 附件，不上传、不联网；
//! - 接口绝不回传本机绝对路径；删除全部录音与删除整个说话人必须携带 `confirmed: true`。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use moss_voice::CenterError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::*;

pub const BENCH_RECORDINGS_DIR_REL: &str = "voice-bench/recordings";
pub const BENCH_RECORDING_SAMPLE_RATE: u32 = 16_000;
pub const MIN_DURATION_MS: u64 = 500;
pub const MAX_DURATION_MS: u64 = 15_000;
const MAX_UPLOAD_WAV_BYTES: usize = 4 * 1024 * 1024;
/// 电平过低门限（峰值振幅 < 0.03 或均方根 < 0.005）
const MIN_PEAK_AMPLITUDE: f32 = 0.03;
const MIN_RMS_AMPLITUDE: f32 = 0.005;
/// 削顶门限：|sample| >= 32700 占比 >= 0.5% 或连续/累计超过 80 个采样
const CLIP_SAMPLE_THRESHOLD: i16 = 32_700;
const MAX_CLIPPED_RATIO: f32 = 0.005;
const MAX_CLIPPED_SAMPLES: usize = 80;

const DEFAULT_E2E_CORPUS_JSON: &str =
    include_str!("../../../../scripts/voice-bench/e2e-corpus.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusSegment {
    pub text: String,
    #[serde(default)]
    pub pause_ms_after: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct RawCorpusTurn {
    id: String,
    #[serde(default)]
    label: String,
    text: String,
    #[serde(default)]
    segments: Vec<CorpusSegment>,
    #[serde(default)]
    key_terms: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    expect_route: Option<String>,
    #[serde(default)]
    expect_tool: Option<String>,
    #[serde(default)]
    expect_simulation_gate: bool,
    #[serde(default)]
    require_idle_laser: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RawCorpusFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    turns: Vec<RawCorpusTurn>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchCorpusItem {
    pub id: String,
    pub label: String,
    pub text: String,
    pub segments: Vec<CorpusSegment>,
    pub pause_hint: Option<String>,
    pub key_terms: Vec<String>,
    pub tags: Vec<String>,
    pub expect_route: Option<String>,
    pub expect_tool: Option<String>,
    pub expect_simulation_gate: bool,
    pub require_idle_laser: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchRecordingMeta {
    pub corpus_id: String,
    pub text: String,
    pub speaker: String,
    pub speaker_dir: String,
    pub recorded_at_ms: u64,
    pub duration_ms: u64,
    pub sample_rate: u32,
    pub peak_level: u8,
    pub rms_level: u8,
    pub peak_amplitude: f32,
    pub rms_amplitude: f32,
    pub clipped_ratio: f32,
}

#[derive(Debug, Clone, Serialize)]
struct SpeakerSummary {
    speaker: String,
    speaker_dir: String,
    recorded_count: usize,
    corpus_count: usize,
    complete: bool,
    updated_at_ms: u64,
}

fn load_default_corpus() -> &'static (String, Vec<BenchCorpusItem>) {
    static CACHE: std::sync::OnceLock<(String, Vec<BenchCorpusItem>)> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let parsed: RawCorpusFile =
            serde_json::from_str(DEFAULT_E2E_CORPUS_JSON).unwrap_or_else(|_| RawCorpusFile {
                name: "xiaozhi-e2e-default".into(),
                turns: Vec::new(),
            });
        let items = parsed
            .turns
            .into_iter()
            .map(|t| {
                let pause_hint = t.segments.iter().find(|s| s.pause_ms_after > 0).map(|s| {
                    let secs = (s.pause_ms_after as f64) / 1000.0;
                    format!("在「{}」后停顿约 {secs:.1} 秒，再继续说后半句", s.text)
                });
                BenchCorpusItem {
                    label: if t.label.trim().is_empty() {
                        t.id.clone()
                    } else {
                        t.label
                    },
                    id: t.id,
                    text: t.text,
                    segments: t.segments,
                    pause_hint,
                    key_terms: t.key_terms,
                    tags: t.tags,
                    expect_route: t.expect_route,
                    expect_tool: t.expect_tool,
                    expect_simulation_gate: t.expect_simulation_gate,
                    require_idle_laser: t.require_idle_laser,
                }
            })
            .collect();
        (parsed.name, items)
    })
}

fn is_windows_reserved_name(name: &str) -> bool {
    let upper = name.trim().to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or(&upper);
    matches!(
        stem,
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

pub(crate) fn sanitize_speaker_label(raw: &str) -> Result<(String, String), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("说话人标签不能为空（例如“我”“家人 A”）".into());
    }
    if trimmed.chars().count() > 32 {
        return Err("说话人标签最多 32 个字符".into());
    }
    if trimmed == "." || trimmed == ".." || trimmed.contains("..") {
        return Err("说话人标签不能包含路径符号".into());
    }
    if trimmed.ends_with('.') || trimmed.ends_with(' ') {
        return Err("说话人标签不能以点号结尾".into());
    }
    for ch in trimmed.chars() {
        if ch.is_control()
            || matches!(
                ch,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
            )
        {
            return Err("说话人标签包含不允许的文件名符号".into());
        }
    }
    if is_windows_reserved_name(trimmed) {
        return Err("说话人标签不能使用系统保留名称".into());
    }
    Ok((trimmed.to_owned(), trimmed.to_owned()))
}

pub(crate) fn sanitize_corpus_id(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err("语料编号须为 1~64 位字母、数字、下划线或连字符".into());
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("语料编号仅允许字母、数字、下划线或连字符".into());
    }
    Ok(trimmed.to_owned())
}

const MAX_META_JSON_BYTES: u64 = 256 * 1024;

pub(super) fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

/// 检查从 `base_root` 向下到 `target` 的每一级已有目录/文件均非符号链接或重解析点（只读检查，不创建目录）。
pub(super) fn ensure_safe_existing_ancestry(base_root: &Path, target: &Path) -> Result<(), String> {
    let rel = target
        .strip_prefix(base_root)
        .map_err(|_| "路径越界".to_owned())?;
    let mut cur = base_root.to_path_buf();
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(part) => cur.push(part),
            _ => return Err("路径包含非法路径段".into()),
        }
        match fs::symlink_metadata(&cur) {
            Ok(meta) => {
                if is_link_or_reparse(&meta) {
                    return Err("路径不能包含符号链接".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(format!("检查路径失败：{e}")),
        }
    }
    Ok(())
}

/// 逐级校验并创建位于 `base_root` 之下的私有目录（Unix `0700`，拒软链与重解析点）。
pub(super) fn ensure_safe_dir_under(base_root: &Path, target: &Path) -> Result<(), String> {
    let rel = target
        .strip_prefix(base_root)
        .map_err(|_| "目录路径越界".to_owned())?;
    let mut cur = base_root.to_path_buf();
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(part) => cur.push(part),
            _ => return Err("目录包含非法路径段".into()),
        }
        match fs::symlink_metadata(&cur) {
            Ok(meta) => {
                if is_link_or_reparse(&meta) {
                    return Err("目录不能是符号链接".into());
                }
                if !meta.is_dir() {
                    return Err("目录路径不是文件夹".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&cur).map_err(|err| format!("创建目录失败：{err}"))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&cur, fs::Permissions::from_mode(0o700))
                        .map_err(|err| format!("设置目录权限失败：{err}"))?;
                }
            }
            Err(e) => return Err(format!("检查目录失败：{e}")),
        }
    }
    Ok(())
}

pub(super) fn ensure_not_symlink(path: &Path) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && is_link_or_reparse(&meta)
    {
        return Err("拒绝访问符号链接文件".into());
    }
    Ok(())
}

fn unique_tmp_path(path: &Path) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rnd = [0u8; 8];
    let _ = getrandom::fill(&mut rnd);
    let rand_u64 = u64::from_le_bytes(rnd);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("recording");
    path.with_file_name(format!(
        ".{file_name}.tmp.{}.{seq}.{rand_u64:016x}",
        std::process::id()
    ))
}

pub(super) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    ensure_not_symlink(path)?;
    let tmp = unique_tmp_path(path);
    ensure_not_symlink(&tmp)?;
    let write_res: Result<(), String> = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&tmp)
            .map_err(|e| format!("创建测试录音临时文件失败：{e}"))?;
        file.write_all(bytes)
            .map_err(|e| format!("写入测试录音文件失败：{e}"))?;
        file.sync_all()
            .map_err(|e| format!("落盘测试录音文件失败：{e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("设置测试录音文件权限失败：{e}"))?;
        }
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("提交测试录音文件失败：{err}"));
    }
    Ok(())
}

fn resample_linear_i16(samples: &[i16], src_rate: u32, dst_rate: u32) -> Vec<i16> {
    if samples.is_empty() || src_rate == 0 || src_rate == dst_rate {
        return samples.to_vec();
    }
    let out_len =
        ((samples.len() as u64 * u64::from(dst_rate)) / u64::from(src_rate)).max(1) as usize;
    let mut out = Vec::with_capacity(out_len);
    let last = samples.len() - 1;
    for i in 0..out_len {
        let pos = (i as f64 * f64::from(src_rate)) / f64::from(dst_rate);
        let idx = (pos.floor() as usize).min(last);
        let next = (idx + 1).min(last);
        let frac = pos - (idx as f64);
        let a = f64::from(samples[idx]);
        let b = f64::from(samples[next]);
        let v = (a + (b - a) * frac).round().clamp(-32768.0, 32767.0) as i16;
        out.push(v);
    }
    out
}

pub(crate) struct RecordingQuality {
    pub duration_ms: u64,
    pub peak_amplitude: f32,
    pub rms_amplitude: f32,
    pub peak_level: u8,
    pub rms_level: u8,
    pub clipped_ratio: f32,
}

pub(crate) fn analyze_and_validate_recording(
    samples_16k: &[i16],
) -> Result<RecordingQuality, String> {
    let duration_ms = ((samples_16k.len() as u64) * 1000) / u64::from(BENCH_RECORDING_SAMPLE_RATE);
    if duration_ms < MIN_DURATION_MS {
        return Err(format!(
            "录音时长过短（{:.2} 秒），需在 0.5–15 秒之间，请重录",
            duration_ms as f64 / 1000.0
        ));
    }
    if duration_ms > MAX_DURATION_MS {
        return Err(format!(
            "录音时长过长（{:.1} 秒），需在 0.5–15 秒之间，请重录",
            duration_ms as f64 / 1000.0
        ));
    }
    let mut peak_abs: i32 = 0;
    let mut sum_sq: f64 = 0.0;
    let mut clipped: usize = 0;
    for &s in samples_16k {
        let a = i32::from(s).abs();
        if a > peak_abs {
            peak_abs = a;
        }
        if a >= i32::from(CLIP_SAMPLE_THRESHOLD) {
            clipped += 1;
        }
        let norm = f64::from(s) / 32768.0;
        sum_sq += norm * norm;
    }
    let peak_amplitude = (peak_abs as f32 / 32768.0).clamp(0.0, 1.0);
    let rms_amplitude = ((sum_sq / samples_16k.len().max(1) as f64).sqrt() as f32).clamp(0.0, 1.0);
    let clipped_ratio = (clipped as f32) / (samples_16k.len().max(1) as f32);
    let peak_level = (peak_amplitude * 100.0).round().clamp(0.0, 100.0) as u8;
    let rms_level = (rms_amplitude * 100.0).round().clamp(0.0, 100.0) as u8;

    if peak_amplitude < MIN_PEAK_AMPLITUDE || rms_amplitude < MIN_RMS_AMPLITUDE {
        return Err(format!(
            "录音音量过低（峰值电平 {peak_level}%），请靠近电脑麦克风或调高系统输入音量后重录"
        ));
    }
    if clipped >= MAX_CLIPPED_SAMPLES || clipped_ratio >= MAX_CLIPPED_RATIO {
        return Err(format!(
            "录音电平过高发生削顶失真（削顶占比 {:.1}%），请离麦克风稍远或调低输入音量后重录",
            clipped_ratio * 100.0
        ));
    }
    Ok(RecordingQuality {
        duration_ms,
        peak_amplitude: (peak_amplitude * 10_000.0).round() / 10_000.0,
        rms_amplitude: (rms_amplitude * 10_000.0).round() / 10_000.0,
        peak_level,
        rms_level,
        clipped_ratio: (clipped_ratio * 10_000.0).round() / 10_000.0,
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl BuiltinVoice {
    pub(crate) fn bench_recordings_root(&self) -> PathBuf {
        self.host.runtime_root.path.join(BENCH_RECORDINGS_DIR_REL)
    }

    fn scan_bench_recordings(&self) -> Result<Vec<BenchRecordingMeta>, String> {
        let root = self.bench_recordings_root();
        ensure_safe_existing_ancestry(&self.host.runtime_root.path, &root)?;
        let Ok(root_meta) = fs::symlink_metadata(&root) else {
            return Ok(Vec::new());
        };
        if is_link_or_reparse(&root_meta) {
            return Err("测试录音目录不能是符号链接".into());
        }
        if !root_meta.is_dir() {
            return Err("测试录音目录路径不是文件夹".into());
        }
        let mut items = Vec::new();
        let entries = fs::read_dir(&root).map_err(|e| format!("读取测试录音目录失败：{e}"))?;
        for entry in entries.flatten() {
            let speaker_path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&speaker_path) else {
                continue;
            };
            if is_link_or_reparse(&meta) || !meta.is_dir() {
                continue;
            }
            let Some(speaker_dir_name) = speaker_path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if sanitize_speaker_label(speaker_dir_name).is_err() {
                continue;
            }
            let Ok(files) = fs::read_dir(&speaker_path) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                let Ok(fmeta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if is_link_or_reparse(&fmeta) || !fmeta.is_file() {
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("wav") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Ok(corpus_id) = sanitize_corpus_id(stem) else {
                    continue;
                };
                let json_path = speaker_path.join(format!("{corpus_id}.json"));
                let json_safe_ok = fs::symlink_metadata(&json_path).ok().is_some_and(|m| {
                    !is_link_or_reparse(&m) && m.is_file() && m.len() <= MAX_META_JSON_BYTES
                });
                let record = if json_safe_ok
                    && let Ok(raw) = fs::read_to_string(&json_path)
                    && let Ok(mut parsed) = serde_json::from_str::<BenchRecordingMeta>(&raw)
                {
                    parsed.corpus_id = corpus_id;
                    parsed.speaker_dir = speaker_dir_name.to_owned();
                    if parsed.speaker.trim().is_empty() {
                        parsed.speaker = speaker_dir_name.to_owned();
                    }
                    parsed
                } else {
                    // 兜底：存在裸 WAV 时按文件头推算基本信息（限 4 MB 内读取）
                    let duration_ms = if fmeta.len() <= MAX_UPLOAD_WAV_BYTES as u64 {
                        fs::read(&path)
                            .ok()
                            .and_then(|bytes| moss_voice_providers::wav::decode_wav(&bytes).ok())
                            .map(|pcm| {
                                ((pcm.samples.len() as u64) * 1000)
                                    / u64::from(pcm.sample_rate.max(1))
                            })
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    BenchRecordingMeta {
                        corpus_id,
                        text: String::new(),
                        speaker: speaker_dir_name.to_owned(),
                        speaker_dir: speaker_dir_name.to_owned(),
                        recorded_at_ms: 0,
                        duration_ms,
                        sample_rate: BENCH_RECORDING_SAMPLE_RATE,
                        peak_level: 0,
                        rms_level: 0,
                        peak_amplitude: 0.0,
                        rms_amplitude: 0.0,
                        clipped_ratio: 0.0,
                    }
                };
                items.push(record);
            }
        }
        items.sort_by(|a, b| {
            a.speaker
                .cmp(&b.speaker)
                .then_with(|| a.corpus_id.cmp(&b.corpus_id))
        });
        Ok(items)
    }

    fn summarize_speakers(
        corpus_items: &[BenchCorpusItem],
        recordings: &[BenchRecordingMeta],
    ) -> Vec<SpeakerSummary> {
        let corpus_ids: BTreeSet<&str> = corpus_items.iter().map(|c| c.id.as_str()).collect();
        let corpus_count = corpus_items.len();
        let mut by_speaker: BTreeMap<String, (String, BTreeSet<String>, u64)> = BTreeMap::new();
        for rec in recordings {
            let entry = by_speaker
                .entry(rec.speaker.clone())
                .or_insert_with(|| (rec.speaker_dir.clone(), BTreeSet::new(), 0));
            if corpus_ids.contains(rec.corpus_id.as_str()) {
                entry.1.insert(rec.corpus_id.clone());
            }
            if rec.recorded_at_ms > entry.2 {
                entry.2 = rec.recorded_at_ms;
            }
        }
        by_speaker
            .into_iter()
            .map(|(speaker, (speaker_dir, matched, updated_at_ms))| {
                let recorded_count = matched.len();
                SpeakerSummary {
                    speaker,
                    speaker_dir,
                    recorded_count,
                    corpus_count,
                    complete: corpus_count > 0 && recorded_count >= corpus_count,
                    updated_at_ms,
                }
            })
            .collect()
    }

    /// 供 `voice_builtin_status` 返回的无音频、无绝对路径轻量统计（绝不进入 `diagnostics_bundle`）。
    pub(super) fn bench_recordings_summary(&self) -> Value {
        let (_name, corpus_items) = load_default_corpus();
        let corpus_count = corpus_items.len();
        let recordings = self.scan_bench_recordings().unwrap_or_default();
        let speakers = Self::summarize_speakers(corpus_items, &recordings);
        let complete_speaker_count = speakers.iter().filter(|s| s.complete).count();
        let speaker_briefs: Vec<Value> = speakers
            .iter()
            .map(|s| {
                json!({
                    "speaker": s.speaker,
                    "recorded_count": s.recorded_count,
                    "corpus_count": s.corpus_count,
                    "complete": s.complete,
                })
            })
            .collect();
        json!({
            "recordings_dir_rel": BENCH_RECORDINGS_DIR_REL,
            "corpus_count": corpus_count,
            "total_recordings": recordings.len(),
            "speaker_count": speakers.len(),
            "complete_speaker_count": complete_speaker_count,
            "speakers": speaker_briefs,
        })
    }

    fn bench_recordings_list_payload(
        &self,
        extra: Option<(&str, Value)>,
    ) -> Result<Value, CenterError> {
        let (corpus_name, corpus_items) = load_default_corpus();
        let recordings = self.scan_bench_recordings().map_err(internal)?;
        let speakers = Self::summarize_speakers(corpus_items, &recordings);
        let complete_speakers: Vec<String> = speakers
            .iter()
            .filter(|s| s.complete)
            .map(|s| s.speaker.clone())
            .collect();
        let mut out = json!({
            "recordings_dir_rel": BENCH_RECORDINGS_DIR_REL,
            "corpus_name": corpus_name,
            "corpus_count": corpus_items.len(),
            "corpus_items": corpus_items,
            "total_recordings": recordings.len(),
            "speaker_count": speakers.len(),
            "complete_speaker_count": complete_speakers.len(),
            "complete_speakers": complete_speakers,
            "speakers": speakers,
            "recordings": recordings,
        });
        if let (Some((k, v)), Some(obj)) = (extra, out.as_object_mut()) {
            obj.insert(k.into(), v);
        }
        Ok(out)
    }

    pub fn bench_recordings_manage(&self, input: &Value) -> Result<Value, CenterError> {
        let invalid = |msg: &str| CenterError::new("bench_recordings_invalid", 400, msg);
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("list");

        match action {
            "status" | "list" => self.bench_recordings_list_payload(None),
            "save" => {
                let raw_speaker = input
                    .get("speaker")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("请先选择或填写说话人标签"))?;
                let (speaker, speaker_dir_name) =
                    sanitize_speaker_label(raw_speaker).map_err(|m| invalid(&m))?;
                let raw_corpus_id = input
                    .get("corpus_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少语料编号"))?;
                let corpus_id = sanitize_corpus_id(raw_corpus_id).map_err(|m| invalid(&m))?;
                let (_name, corpus_items) = load_default_corpus();
                let default_text = corpus_items
                    .iter()
                    .find(|c| c.id == corpus_id)
                    .map(|c| c.text.clone())
                    .unwrap_or_default();
                let text = input
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                    .unwrap_or(default_text);
                if text.is_empty() || text.chars().count() > 500 {
                    return Err(invalid("缺少有效的语料原文"));
                }

                let raw_b64 = input
                    .get("audio_wav_base64")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| invalid("缺少录音 WAV 数据"))?;
                let b64_body = raw_b64
                    .split_once(";base64,")
                    .map(|(_, rest)| rest)
                    .unwrap_or(raw_b64);
                let estimated = (b64_body.len() / 4) * 3;
                if estimated > MAX_UPLOAD_WAV_BYTES {
                    return Err(invalid(
                        "录音文件过大（超过 4 MB 上限），请重录 0.5–15 秒的句子",
                    ));
                }
                let wav_bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64_body)
                    .map_err(|_| invalid("录音 WAV Base64 编码无效"))?;
                if wav_bytes.len() > MAX_UPLOAD_WAV_BYTES {
                    return Err(invalid(
                        "录音文件过大（超过 4 MB 上限），请重录 0.5–15 秒的句子",
                    ));
                }
                let decoded = moss_voice_providers::wav::decode_wav(&wav_bytes)
                    .map_err(|_| invalid("录音不是有效的 PCM WAV 格式，请重录"))?;
                if decoded.sample_rate < 8_000 || decoded.sample_rate > 96_000 {
                    return Err(invalid("不支持的录音采样率，请重录"));
                }
                let samples_16k = resample_linear_i16(
                    &decoded.samples,
                    decoded.sample_rate,
                    BENCH_RECORDING_SAMPLE_RATE,
                );
                let quality = analyze_and_validate_recording(&samples_16k)
                    .map_err(|msg| CenterError::new("bench_recording_quality_failed", 400, msg))?;

                let root = self.bench_recordings_root();
                let speaker_dir = root.join(&speaker_dir_name);
                ensure_safe_dir_under(&self.host.runtime_root.path, &speaker_dir)
                    .map_err(internal)?;

                let normalized_wav = moss_voice_providers::wav::encode_wav(
                    &samples_16k,
                    BENCH_RECORDING_SAMPLE_RATE,
                );
                let wav_path = speaker_dir.join(format!("{corpus_id}.wav"));
                let json_path = speaker_dir.join(format!("{corpus_id}.json"));

                let meta = BenchRecordingMeta {
                    corpus_id: corpus_id.clone(),
                    text,
                    speaker,
                    speaker_dir: speaker_dir_name,
                    recorded_at_ms: now_ms(),
                    duration_ms: quality.duration_ms,
                    sample_rate: BENCH_RECORDING_SAMPLE_RATE,
                    peak_level: quality.peak_level,
                    rms_level: quality.rms_level,
                    peak_amplitude: quality.peak_amplitude,
                    rms_amplitude: quality.rms_amplitude,
                    clipped_ratio: quality.clipped_ratio,
                };
                let meta_bytes =
                    serde_json::to_vec_pretty(&meta).map_err(|e| internal(e.to_string()))?;

                write_private_atomic(&wav_path, &normalized_wav).map_err(internal)?;
                write_private_atomic(&json_path, &meta_bytes).map_err(internal)?;
                self.invalidate_status_probe_cache();

                self.bench_recordings_list_payload(Some((
                    "saved",
                    serde_json::to_value(&meta).unwrap_or(Value::Null),
                )))
            }
            "audio" => {
                let raw_speaker = input
                    .get("speaker")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少说话人标签"))?;
                let (speaker, speaker_dir_name) =
                    sanitize_speaker_label(raw_speaker).map_err(|m| invalid(&m))?;
                let raw_corpus_id = input
                    .get("corpus_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少语料编号"))?;
                let corpus_id = sanitize_corpus_id(raw_corpus_id).map_err(|m| invalid(&m))?;

                let root = self.bench_recordings_root();
                let speaker_dir = root.join(&speaker_dir_name);
                let wav_path = speaker_dir.join(format!("{corpus_id}.wav"));
                ensure_safe_existing_ancestry(&self.host.runtime_root.path, &wav_path)
                    .map_err(internal)?;
                let meta = fs::symlink_metadata(&wav_path).map_err(|_| {
                    CenterError::new("bench_recording_not_found", 404, "未找到对应的测试录音文件")
                })?;
                if is_link_or_reparse(&meta)
                    || !meta.is_file()
                    || meta.len() > MAX_UPLOAD_WAV_BYTES as u64
                {
                    return Err(invalid("测试录音文件无效或超出大小上限"));
                }
                let bytes = fs::read(&wav_path).map_err(|_| {
                    CenterError::new("bench_recording_not_found", 404, "未找到对应的测试录音文件")
                })?;
                let decoded = moss_voice_providers::wav::decode_wav(&bytes)
                    .map_err(|_| invalid("测试录音 WAV 已损坏"))?;
                let duration_ms =
                    ((decoded.samples.len() as u64) * 1000) / u64::from(decoded.sample_rate.max(1));
                let audio_data_url = moss_voice::models::audio_data_url(&bytes, "audio/wav");
                Ok(json!({
                    "speaker": speaker,
                    "corpus_id": corpus_id,
                    "sample_rate": decoded.sample_rate,
                    "duration_ms": duration_ms,
                    "audio_data_url": audio_data_url,
                }))
            }
            "delete" => {
                let raw_speaker = input
                    .get("speaker")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少说话人标签"))?;
                let (_speaker, speaker_dir_name) =
                    sanitize_speaker_label(raw_speaker).map_err(|m| invalid(&m))?;
                let raw_corpus_id = input
                    .get("corpus_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少语料编号"))?;
                let corpus_id = sanitize_corpus_id(raw_corpus_id).map_err(|m| invalid(&m))?;

                let root = self.bench_recordings_root();
                let speaker_dir = root.join(&speaker_dir_name);
                ensure_safe_existing_ancestry(&self.host.runtime_root.path, &speaker_dir)
                    .map_err(internal)?;
                let mut deleted = false;
                if let Ok(meta) = fs::symlink_metadata(&speaker_dir)
                    && meta.is_dir()
                {
                    for ext in ["wav", "json"] {
                        let p = speaker_dir.join(format!("{corpus_id}.{ext}"));
                        if let Ok(fm) = fs::symlink_metadata(&p) {
                            if is_link_or_reparse(&fm) {
                                return Err(internal("拒绝删除符号链接文件"));
                            }
                            if fm.is_file() {
                                fs::remove_file(&p)
                                    .map_err(|e| internal(format!("删除录音失败：{e}")))?;
                                if ext == "wav" {
                                    deleted = true;
                                }
                            }
                        }
                    }
                    let is_empty = fs::read_dir(&speaker_dir)
                        .map(|mut it| it.next().is_none())
                        .unwrap_or(false);
                    if is_empty {
                        let _ = fs::remove_dir(&speaker_dir);
                    }
                }
                self.invalidate_status_probe_cache();
                self.bench_recordings_list_payload(Some(("deleted", Value::Bool(deleted))))
            }
            "delete_speaker" => {
                let confirmed = input
                    .get("confirmed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !confirmed {
                    return Err(CenterError::new(
                        "confirmation_required",
                        400,
                        "删除该说话人的全部测试录音前需要明确确认",
                    ));
                }
                let raw_speaker = input
                    .get("speaker")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少说话人标签"))?;
                let (_speaker, speaker_dir_name) =
                    sanitize_speaker_label(raw_speaker).map_err(|m| invalid(&m))?;
                let root = self.bench_recordings_root();
                let speaker_dir = root.join(&speaker_dir_name);
                ensure_safe_existing_ancestry(&self.host.runtime_root.path, &speaker_dir)
                    .map_err(internal)?;
                let mut deleted_count = 0usize;
                if let Ok(meta) = fs::symlink_metadata(&speaker_dir)
                    && meta.is_dir()
                {
                    if let Ok(files) = fs::read_dir(&speaker_dir) {
                        for entry in files.flatten() {
                            let p = entry.path();
                            let Ok(fm) = fs::symlink_metadata(&p) else {
                                continue;
                            };
                            if is_link_or_reparse(&fm) {
                                return Err(internal("拒绝删除符号链接文件"));
                            }
                            if fm.is_file() {
                                if p.extension().and_then(|e| e.to_str()) == Some("wav") {
                                    deleted_count += 1;
                                }
                                fs::remove_file(&p)
                                    .map_err(|e| internal(format!("删除录音文件失败：{e}")))?;
                            }
                        }
                    }
                    let _ = fs::remove_dir(&speaker_dir);
                }
                self.invalidate_status_probe_cache();
                self.bench_recordings_list_payload(Some((
                    "deleted_count",
                    Value::from(deleted_count),
                )))
            }
            "clear" => {
                let confirmed = input
                    .get("confirmed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if !confirmed {
                    return Err(CenterError::new(
                        "confirmation_required",
                        400,
                        "删除全部测试录音前需要明确确认",
                    ));
                }
                let root = self.bench_recordings_root();
                ensure_safe_existing_ancestry(&self.host.runtime_root.path, &root)
                    .map_err(internal)?;
                let mut deleted_count = 0usize;
                if let Ok(root_meta) = fs::symlink_metadata(&root)
                    && root_meta.is_dir()
                {
                    let entries = fs::read_dir(&root)
                        .map_err(|e| internal(format!("扫描测试录音失败：{e}")))?;
                    for entry in entries.flatten() {
                        let speaker_dir = entry.path();
                        let Ok(meta) = fs::symlink_metadata(&speaker_dir) else {
                            continue;
                        };
                        if is_link_or_reparse(&meta) {
                            continue;
                        }
                        if meta.is_dir() {
                            if let Ok(files) = fs::read_dir(&speaker_dir) {
                                for file in files.flatten() {
                                    let p = file.path();
                                    if let Ok(fm) = fs::symlink_metadata(&p)
                                        && !is_link_or_reparse(&fm)
                                        && fm.is_file()
                                    {
                                        if p.extension().and_then(|e| e.to_str()) == Some("wav") {
                                            deleted_count += 1;
                                        }
                                        let _ = fs::remove_file(&p);
                                    }
                                }
                            }
                            let _ = fs::remove_dir(&speaker_dir);
                        }
                    }
                }
                self.invalidate_status_probe_cache();
                self.bench_recordings_list_payload(Some((
                    "deleted_count",
                    Value::from(deleted_count),
                )))
            }
            _ => Err(invalid("不支持的测试录音操作")),
        }
    }
}
