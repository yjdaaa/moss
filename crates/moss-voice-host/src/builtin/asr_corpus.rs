//! VB1/VB2 识别语料采集与标注的宿主管理入口（仅经 `voice_asr_corpus_manage` 供受信任主窗口调用）。
//!
//! 隐私与安全边界：
//! - 开关 `asr_corpus_capture_enabled` 默认关闭，开启必须携带 `confirmed: true`；
//! - 关闭开关时支持通过 `delete_existing: true` 同步删除已采集语料，关闭后仍允许调用 `delete` / `clear` 清理旧语料；
//! - 音频读取与标注写入仅在开关开启时可用，且返回 WAV Data URL，绝不回传本机绝对路径；
//! - 绝不进入 `diagnostics_bundle()`、`ProcessTrace` 或声纹识别链路。

use moss_voice::CenterError;
use moss_voice_server::asr_corpus::{ASR_CORPUS_DIR_REL, AsrCorpusConfig, VALID_CORPUS_TAGS};
use serde_json::{Value, json};

use super::*;

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
                let mut payload = self.asr_corpus_list_payload(Some(cleared))?;
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("cleared".into(), Value::from(cleared));
                }
                Ok(payload)
            }
            _ => Err(invalid("不支持的识别语料操作")),
        }
    }

    fn asr_corpus_list_payload(&self, deleted_count: Option<usize>) -> Result<Value, CenterError> {
        let cfg = self.asr_corpus.config();
        let all_items = self.asr_corpus.list().map_err(internal)?;
        let total_count = all_items.len();
        let labeled_count = all_items.iter().filter(|item| item.label.is_some()).count();
        // 关闭开关时仅返回统计数字与空列表，不把录音元数据下发到界面
        let items = if cfg.enabled { all_items } else { Vec::new() };
        let mut out = json!({
            "enabled": cfg.enabled,
            "max_items": cfg.max_items,
            "max_days": cfg.max_days,
            "corpus_dir_rel": ASR_CORPUS_DIR_REL,
            "valid_tags": VALID_CORPUS_TAGS,
            "total_count": total_count,
            "labeled_count": labeled_count,
            "items": items,
        });
        if let (Some(count), Some(obj)) = (deleted_count, out.as_object_mut()) {
            obj.insert("deleted_count".into(), Value::from(count));
        }
        Ok(out)
    }
}
