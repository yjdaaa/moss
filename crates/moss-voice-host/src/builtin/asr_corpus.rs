//! VB1/VB2 识别语料采集与标注的宿主管理入口（仅经 `voice_asr_corpus_manage` 供受信任主窗口调用）。
//!
//! 隐私与安全边界：
//! - 开关 `asr_corpus_capture_enabled` 默认关闭，开启必须携带 `confirmed: true`；
//! - 关闭开关时支持通过 `delete_existing: true` 同步删除已采集语料，关闭后仍允许调用 `delete` / `clear` 清理旧语料；
//! - 音频读取与标注写入仅在开关开启时可用，且返回 WAV Data URL，绝不回传本机绝对路径；
//! - 绝不进入 `diagnostics_bundle()`、`ProcessTrace` 或声纹识别链路。

use std::path::{Path, PathBuf};

use moss_voice::CenterError;
use moss_voice_server::asr_corpus::{ASR_CORPUS_DIR_REL, AsrCorpusConfig, VALID_CORPUS_TAGS};
use serde_json::{Value, json};

use super::*;

const MAX_TRACE_WAV_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LABEL_JSON_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone)]
pub(super) struct TraceVoiceEntry {
    pub stem: String,
    pub rel_path: String,
    pub path: PathBuf,
    pub modified_ms: u64,
}

fn check_trace_ancestry(
    trace_root: &Path,
    runtime_root: &Path,
    target: &Path,
) -> Result<(), String> {
    if trace_root.starts_with(runtime_root) {
        super::bench_recordings::ensure_safe_existing_ancestry(runtime_root, target)
    } else {
        super::bench_recordings::ensure_not_symlink(trace_root)?;
        super::bench_recordings::ensure_safe_existing_ancestry(trace_root, target)
    }
}

/// 扫描某日 `<trace_root>/<date>/voice-files/` 下的全部普通（非软链）WAV 附件，供状态查询、试听与真值标注复用。
pub(super) fn index_trace_voice_wavs(
    trace_root: &Path,
    runtime_root: &Path,
    date: &str,
) -> Result<Vec<TraceVoiceEntry>, String> {
    if date.len() != 10 || !date.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err("过程记录日期格式非法".into());
    }
    let voice_files_dir = trace_root.join(date).join("voice-files");
    check_trace_ancestry(trace_root, runtime_root, &voice_files_dir)?;
    let Ok(dir_meta) = std::fs::symlink_metadata(&voice_files_dir) else {
        return Ok(Vec::new());
    };
    if super::bench_recordings::is_link_or_reparse(&dir_meta) || !dir_meta.is_dir() {
        return Ok(Vec::new());
    }
    let read_dir = std::fs::read_dir(&voice_files_dir)
        .map_err(|e| format!("读取过程记录录音目录失败：{e}"))?;
    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if super::bench_recordings::is_link_or_reparse(&meta) || !meta.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".wav") else {
            continue;
        };
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        entries.push(TraceVoiceEntry {
            stem: stem.to_owned(),
            rel_path: format!("{date}/voice-files/{file_name}"),
            path,
            modified_ms,
        });
    }
    Ok(entries)
}

/// 在已扫描的当日过程记录 WAV 列表中定位某一轮用户录音：
/// 优先精确命中 `preferred_rel`（`ProcessTrace::save_file` 返回的 `date/voice-files/{HHMMSSmmm}-{seq:06}-{dev_short}-t{turn}-user.wav`）；
/// 否则匹配 `{HHMMSSmmm}-{seq:06}-{dev_short}-t{turn}-user.wav` 或无前缀 `{dev_short}-t{turn}-user.wav`，同日多会话按最接近 `at_ms_hint` 消歧。
pub(super) fn match_trace_voice_wav<'a>(
    entries: &'a [TraceVoiceEntry],
    dev_short: &str,
    turn: u64,
    at_ms_hint: u64,
    preferred_rel: Option<&str>,
) -> Option<&'a TraceVoiceEntry> {
    if let Some(rel) = preferred_rel.map(str::trim).filter(|s| !s.is_empty()) {
        let normalized = rel.replace('\\', "/");
        if let Some(hit) = entries.iter().find(|e| e.rel_path == normalized) {
            return Some(hit);
        }
    }
    let bare_stem = format!("{dev_short}-t{turn}-user");
    let suffix = format!("-{bare_stem}");
    let mut candidates: Vec<&TraceVoiceEntry> = entries
        .iter()
        .filter(|e| e.stem == bare_stem || e.stem.ends_with(&suffix))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    if at_ms_hint > 0 {
        candidates.sort_by_key(|e| (e.modified_ms.abs_diff(at_ms_hint), e.stem.as_str()));
        return Some(candidates[0]);
    }
    candidates.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then_with(|| b.stem.cmp(&a.stem))
    });
    Some(candidates[0])
}

/// 安全读取单个 `.label.json`（校验祖先非软链/重解析点且文件体积不超过 256 KiB）。
pub(super) fn read_label_json_safe(
    base_root: &Path,
    runtime_root: &Path,
    path: &Path,
) -> Option<Value> {
    check_trace_ancestry(base_root, runtime_root, path).ok()?;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if super::bench_recordings::is_link_or_reparse(&meta)
        || !meta.is_file()
        || meta.len() > MAX_LABEL_JSON_BYTES
    {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&raw).ok()
}

impl BuiltinVoice {
    pub(super) fn asr_corpus_summary(&self) -> Value {
        let cfg = self.asr_corpus.config();
        let (total_count, labeled_count) = self
            .asr_corpus
            .list()
            .map(|items| {
                let total = items.len();
                let labeled = items.iter().filter(|item| item.label.is_some()).count();
                (total, labeled)
            })
            .unwrap_or((0, 0));
        json!({
            "enabled": cfg.enabled,
            "max_items": cfg.max_items,
            "max_days": cfg.max_days,
            "corpus_dir_rel": ASR_CORPUS_DIR_REL,
            "total_count": total_count,
            "labeled_count": labeled_count,
        })
    }

    pub fn asr_corpus_manage(&self, input: &Value) -> Result<Value, CenterError> {
        let invalid = |msg: &str| CenterError::new("asr_corpus_invalid", 400, msg);
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("list");

        match action {
            "status" | "list" => self.asr_corpus_list_payload(None),
            "set_config" => {
                let current = self.asr_corpus.config();
                let enabled = match input.get("enabled") {
                    Some(v) => v.as_bool().ok_or_else(|| invalid("采集开关须为布尔值"))?,
                    None => current.enabled,
                };
                let max_items = match input.get("max_items") {
                    Some(v) => {
                        let raw = v
                            .as_u64()
                            .ok_or_else(|| invalid("最多保留条数须为正整数"))?;
                        moss_voice::settings::validate_asr_corpus_max_items(raw)
                            .map_err(|msg| invalid(&msg))?
                    }
                    None => current.max_items,
                };
                let max_days = match input.get("max_days") {
                    Some(v) => {
                        let raw = v
                            .as_u64()
                            .ok_or_else(|| invalid("最多保留天数须为正整数"))?;
                        moss_voice::settings::validate_asr_corpus_max_days(raw)
                            .map_err(|msg| invalid(&msg))?
                    }
                    None => current.max_days,
                };
                let confirmed = input
                    .get("confirmed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if enabled && !current.enabled && !confirmed {
                    return Err(CenterError::new(
                        "confirmation_required",
                        400,
                        "开启识别录音语料采集前需要明确确认",
                    ));
                }
                let delete_existing = input
                    .get("delete_existing")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                moss_voice::settings::update_voice(&self.settings_file(), |s| {
                    s.builtin_local.asr_corpus_capture_enabled = enabled;
                    s.builtin_local.asr_corpus_max_items = max_items;
                    s.builtin_local.asr_corpus_max_days = max_days;
                    Ok(())
                })
                .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;

                self.asr_corpus
                    .set_config(AsrCorpusConfig {
                        enabled,
                        max_items,
                        max_days,
                    })
                    .map_err(internal)?;

                let deleted = if !enabled && delete_existing {
                    Some(self.asr_corpus.clear_all().map_err(internal)?)
                } else {
                    None
                };
                self.invalidate_status_probe_cache();
                self.asr_corpus_list_payload(deleted)
            }
            "audio" => {
                if !self.asr_corpus.is_enabled() {
                    return Err(CenterError::new(
                        "asr_corpus_disabled",
                        403,
                        "请先开启“保存最近的识别录音用于测评”开关",
                    ));
                }
                let date = input
                    .get("date")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少语料日期"))?;
                let turn_id = input
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少语料编号"))?;
                let (wav, duration_ms) = self
                    .asr_corpus
                    .read_audio_wav(date, turn_id)
                    .map_err(|msg| invalid(&msg))?;
                let audio_data_url = moss_voice::models::audio_data_url(&wav, "audio/wav");
                Ok(json!({
                    "date": date,
                    "turn_id": turn_id,
                    "sample_rate": moss_voice_server::asr_corpus::ASR_CORPUS_SAMPLE_RATE,
                    "duration_ms": duration_ms,
                    "audio_data_url": audio_data_url,
                }))
            }
            "save_label" => {
                if !self.asr_corpus.is_enabled() {
                    return Err(CenterError::new(
                        "asr_corpus_disabled",
                        403,
                        "请先开启“保存最近的识别录音用于测评”开关",
                    ));
                }
                let date = input
                    .get("date")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少语料日期"))?;
                let turn_id = input
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少语料编号"))?;
                let reference_text = input
                    .get("reference_text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少正确文字"))?;
                let verified_correct = input
                    .get("verified_correct")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let tags: Vec<String> = match input.get("tags") {
                    Some(v) => {
                        serde_json::from_value(v.clone()).map_err(|_| invalid("标签格式不正确"))?
                    }
                    None => Vec::new(),
                };
                let label = self
                    .asr_corpus
                    .save_label(date, turn_id, reference_text, verified_correct, tags)
                    .map_err(|msg| invalid(&msg))?;
                self.invalidate_status_probe_cache();
                let items = self.asr_corpus.list().map_err(internal)?;
                let labeled_count = items.iter().filter(|item| item.label.is_some()).count();
                Ok(json!({
                    "label": label,
                    "total_count": items.len(),
                    "labeled_count": labeled_count,
                }))
            }
            "delete" => {
                let date = input
                    .get("date")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少语料日期"))?;
                let turn_id = input
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("缺少语料编号"))?;
                let deleted = self
                    .asr_corpus
                    .delete_item(date, turn_id)
                    .map_err(|msg| invalid(&msg))?;
                self.invalidate_status_probe_cache();
                let mut payload = self.asr_corpus_list_payload(None)?;
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("deleted".into(), Value::Bool(deleted));
                }
                Ok(payload)
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
                        "清空全部识别语料前需要明确确认",
                    ));
                }
                let cleared = self.asr_corpus.clear_all().map_err(internal)?;
                self.invalidate_status_probe_cache();
                let mut payload = self.asr_corpus_list_payload(Some(cleared))?;
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("cleared".into(), Value::from(cleared));
                }
                Ok(payload)
            }
            "set_process_trace" => {
                let enabled = input
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("完整过程记录开关须为布尔值"))?;
                let ctrl = super::debug_inject::DesktopProcessTraceControl {
                    settings_file: self.settings_file(),
                    trace: self.host.api_service.process_trace().cloned(),
                };
                use moss_voice_server::debug_api::ProcessTraceControl as _;
                let res = ctrl.set_enabled(enabled).map_err(internal)?;
                self.invalidate_status_probe_cache();
                Ok(json!({
                    "process_trace_enabled": res.enabled,
                    "previous_enabled": res.previous_enabled,
                }))
            }
            "trace_audio" => {
                let date = input
                    .get("date")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| s.len() == 10 && s.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
                    .ok_or_else(|| invalid("缺少有效的记录日期"))?;
                let device_id = input
                    .get("device_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let session_id = input
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let turn = input.get("turn").and_then(Value::as_u64).unwrap_or(0);
                let at_ms_hint = input.get("at_ms").and_then(Value::as_u64).unwrap_or(0);
                let preferred_rel = input
                    .get("trace_audio_file")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let dev_short = moss_voice_server::device_short(device_id);
                let trace_root = self
                    .host
                    .api_service
                    .process_trace()
                    .map(|t| t.directory_status().dir)
                    .unwrap_or_else(|| {
                        self.host
                            .runtime_root
                            .path
                            .join(laser_runtime::process_trace::TRACE_DIR)
                    });
                let trace_entries =
                    index_trace_voice_wavs(&trace_root, &self.host.runtime_root.path, date)
                        .map_err(internal)?;
                if let Some(hit) = match_trace_voice_wav(
                    &trace_entries,
                    &dev_short,
                    turn,
                    at_ms_hint,
                    preferred_rel,
                ) {
                    check_trace_ancestry(&trace_root, &self.host.runtime_root.path, &hit.path)
                        .map_err(internal)?;
                    let meta = std::fs::symlink_metadata(&hit.path)
                        .map_err(|e| internal(e.to_string()))?;
                    if super::bench_recordings::is_link_or_reparse(&meta) || !meta.is_file() {
                        return Err(invalid("拒绝读取符号链接录音文件"));
                    }
                    if meta.len() > MAX_TRACE_WAV_BYTES {
                        return Err(invalid("录音文件超过 16 MB 上限，拒绝读取"));
                    }
                    let wav = std::fs::read(&hit.path).map_err(|e| internal(e.to_string()))?;
                    let audio_data_url = moss_voice::models::audio_data_url(&wav, "audio/wav");
                    return Ok(json!({
                        "date": date,
                        "turn": turn,
                        "trace_audio_file": hit.rel_path,
                        "audio_data_url": audio_data_url,
                    }));
                }
                if !self.asr_corpus.is_enabled() {
                    return Err(CenterError::new(
                        "asr_corpus_disabled",
                        403,
                        "未找到过程记录录音，且独立识别语料采集已关闭",
                    ));
                }
                let turn_id = moss_voice_server::asr_corpus::sanitize_turn_id(session_id, turn);
                let (wav, duration_ms) = self
                    .asr_corpus
                    .read_audio_wav(date, &turn_id)
                    .map_err(|msg| invalid(&msg))?;
                let audio_data_url = moss_voice::models::audio_data_url(&wav, "audio/wav");
                Ok(json!({
                    "date": date,
                    "turn": turn,
                    "turn_id": turn_id,
                    "duration_ms": duration_ms,
                    "audio_data_url": audio_data_url,
                }))
            }
            "trace_label" => {
                let date = input
                    .get("date")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| s.len() == 10 && s.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
                    .ok_or_else(|| invalid("缺少有效的记录日期"))?;
                let reference_text = input
                    .get("reference_text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.chars().count() <= 500)
                    .ok_or_else(|| invalid("正确文字不能为空且不超过 500 字"))?;
                let device_id = input
                    .get("device_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let session_id = input
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let turn = input.get("turn").and_then(Value::as_u64).unwrap_or(0);
                let at_ms_hint = input.get("at_ms").and_then(Value::as_u64).unwrap_or(0);
                let preferred_rel = input
                    .get("trace_audio_file")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let raw_text = input
                    .get("raw_text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let corrected_text = input
                    .get("corrected_text")
                    .and_then(Value::as_str)
                    .unwrap_or(raw_text);
                let input_source = input
                    .get("input_source")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("live_mic");
                let verified_correct = input
                    .get("verified_correct")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let tags: Vec<String> = input
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .filter(|t| VALID_CORPUS_TAGS.contains(t))
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let dev_short = moss_voice_server::device_short(device_id);
                let bare_stem = format!("{dev_short}-t{turn}-user");
                let turn_id = moss_voice_server::asr_corpus::sanitize_turn_id(session_id, turn);
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let label_source = if input_source == "e2e_playback" {
                    "e2e-playback"
                } else {
                    "manual"
                };
                let label_json = json!({
                    "turn_id": turn_id,
                    "turn": turn,
                    "device_id": device_id,
                    "session_id": session_id,
                    "date": date,
                    "input_source": input_source,
                    "raw_text": raw_text,
                    "corrected_text": corrected_text,
                    "reference_text": reference_text,
                    "verified_correct": verified_correct,
                    "tags": tags,
                    "updated_at_ms": now_ms,
                    "label_source": label_source,
                });
                let trace_root = self
                    .host
                    .api_service
                    .process_trace()
                    .map(|t| t.directory_status().dir)
                    .unwrap_or_else(|| {
                        self.host
                            .runtime_root
                            .path
                            .join(laser_runtime::process_trace::TRACE_DIR)
                    });
                let trace_entries =
                    index_trace_voice_wavs(&trace_root, &self.host.runtime_root.path, date)
                        .map_err(internal)?;
                let matched_stem = match_trace_voice_wav(
                    &trace_entries,
                    &dev_short,
                    turn,
                    at_ms_hint,
                    preferred_rel,
                )
                .map(|e| e.stem.clone())
                .unwrap_or(bare_stem);
                let voice_files_dir = trace_root.join(date).join("voice-files");
                if trace_root.starts_with(&self.host.runtime_root.path) {
                    super::bench_recordings::ensure_safe_dir_under(
                        &self.host.runtime_root.path,
                        &voice_files_dir,
                    )
                    .map_err(internal)?;
                } else {
                    super::bench_recordings::ensure_not_symlink(&trace_root).map_err(internal)?;
                    if !trace_root.exists() {
                        std::fs::create_dir_all(&trace_root)
                            .map_err(|e| internal(e.to_string()))?;
                    }
                    super::bench_recordings::ensure_safe_dir_under(&trace_root, &voice_files_dir)
                        .map_err(internal)?;
                }
                let trace_label_path = voice_files_dir.join(format!("{matched_stem}.label.json"));
                let serialized = format!(
                    "{}\n",
                    serde_json::to_string_pretty(&label_json).unwrap_or_default()
                );
                super::bench_recordings::write_private_atomic(
                    &trace_label_path,
                    serialized.as_bytes(),
                )
                .map_err(internal)?;

                if self.asr_corpus.is_enabled() {
                    let corpus_wav = self
                        .asr_corpus
                        .root()
                        .join(date)
                        .join(format!("{turn_id}.wav"));
                    if super::bench_recordings::ensure_safe_existing_ancestry(
                        &self.host.runtime_root.path,
                        &corpus_wav,
                    )
                    .is_ok()
                        && std::fs::symlink_metadata(&corpus_wav).is_ok_and(|m| {
                            !super::bench_recordings::is_link_or_reparse(&m) && m.is_file()
                        })
                    {
                        let _ = self.asr_corpus.save_label(
                            date,
                            &turn_id,
                            reference_text,
                            verified_correct,
                            tags.clone(),
                        );
                    }
                }
                if let Some(trace) = self
                    .host
                    .api_service
                    .process_trace()
                    .filter(|t| t.enabled())
                {
                    let flow = format!("语音·{dev_short}·第{turn}轮");
                    let step = format!(
                        "记录识别真值[音源：{input_source}]：原识别「{raw_text}」→ 正确文字「{reference_text}」"
                    );
                    trace.record("voice", &flow, "user_asr_label", &step, label_json.clone());
                }
                self.invalidate_status_probe_cache();
                Ok(json!({
                    "ok": true,
                    "label": label_json,
                }))
            }
            _ => Err(invalid("不支持的识别语料操作")),
        }
    }

    fn asr_corpus_list_payload(&self, deleted_count: Option<usize>) -> Result<Value, CenterError> {
        let cfg = self.asr_corpus.config();
        let all_items = self.asr_corpus.list().map_err(internal)?;
        let total_count = all_items.len();
        let labeled_count = all_items.iter().filter(|item| item.label.is_some()).count();
        // 关闭开关时 items 保持空数组，仅在 retained_items 返回最小只删索引（不含原话与录音）供逐条删除（n137 VB1）
        let (items, retained_items) = if cfg.enabled {
            (all_items, Vec::new())
        } else {
            let retained: Vec<Value> = all_items
                .iter()
                .map(|item| {
                    json!({
                        "date": item.meta.date,
                        "turn_id": item.meta.turn_id,
                        "captured_at_ms": item.meta.captured_at_ms,
                        "duration_ms": item.meta.duration_ms,
                        "labeled": item.label.is_some(),
                    })
                })
                .collect();
            (Vec::new(), retained)
        };
        let mut out = json!({
            "enabled": cfg.enabled,
            "max_items": cfg.max_items,
            "max_days": cfg.max_days,
            "corpus_dir_rel": ASR_CORPUS_DIR_REL,
            "valid_tags": VALID_CORPUS_TAGS,
            "total_count": total_count,
            "labeled_count": labeled_count,
            "items": items,
            "retained_items": retained_items,
        });
        if let (Some(count), Some(obj)) = (deleted_count, out.as_object_mut()) {
            obj.insert("deleted_count".into(), Value::from(count));
        }
        Ok(out)
    }
}
