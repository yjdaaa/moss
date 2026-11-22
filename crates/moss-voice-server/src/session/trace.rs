//! 完整过程记录（n105，默认关）：会话这一层负责带上设备、会话与轮次，写入 `logs/process-trace/`。
//!
//! 每轮依次可能写：`user_audio`（用户这段话的录音 WAV，上行采样率）→ `asr`（识别原文与纠正后）→
//! `llm_round`（每轮大模型请求与回复，来自对话任务）→ `tool_call`（工具参数与完整结果）→
//! `turn_end`（结果、实际播出的句子、完整回复、耗时，以及合成语音 WAV，下行采样率）。
//! 写入前去掉疑似密钥 / 令牌（不遮 IP、不截断），多模态图片的 data URL 只留长度。

use super::*;

/// 一轮合成语音最多缓存的样本数（约 20 分钟 24 kHz），超出部分不再记录。
const MAX_TRACE_TTS_SAMPLES: usize = 24_000 * 60 * 20;

/// 去掉凭据并省略图片数据（递归处理字符串）。
pub(crate) fn scrub_trace_value(value: &mut Value) {
    match value {
        Value::String(text) => {
            if text.starts_with("data:") && text.len() > 256 {
                *text = format!("[图片数据已省略，{} 字节]", text.len());
            } else {
                *text = moss_voice_providers::scrub_text_with_options(text, false, 0, false).0;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub_trace_value),
        Value::Object(map) => map.values_mut().for_each(scrub_trace_value),
        _ => {}
    }
}

impl<'a> Session<'a> {
    /// 打开时返回记录器；关着（或回声模式没有对话）时为 `None`。
    pub(super) fn trace_handle(&self) -> Option<laser_runtime::ProcessTrace> {
        self.conversation
            .as_ref()?
            .process_trace
            .clone()
            .filter(laser_runtime::ProcessTrace::enabled)
    }

    /// 写一条语音事件，自动带上会话、设备与当前轮次。
    pub(super) fn trace_record(&self, kind: &str, mut data: Value) {
        let Some(trace) = self.trace_handle() else {
            return;
        };
        scrub_trace_value(&mut data);
        let mut line = serde_json::Map::new();
        line.insert("session_id".into(), Value::from(self.session_id));
        line.insert("device_id".into(), Value::from(self.device_id));
        line.insert("turn".into(), Value::from(self.turn_number));
        match data {
            Value::Object(map) => line.extend(map),
            other => {
                line.insert("data".into(), other);
            }
        }
        trace.record("voice", kind, Value::Object(line));
    }

    /// 把一段 PCM 存成 WAV，返回相对 `logs/process-trace/` 的路径。
    pub(super) fn trace_audio(&self, label: &str, pcm: &[i16], sample_rate: u32) -> Option<String> {
        if pcm.is_empty() {
            return None;
        }
        let trace = self.trace_handle()?;
        let device: String = self
            .device_id
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        let stem = format!("{device}-t{}-{label}", self.turn_number);
        trace.save_file(
            "voice",
            &stem,
            "wav",
            &moss_voice_providers::wav::encode_wav(pcm, sample_rate),
        )
    }

    /// 轮次开始：记下这一轮的输入（用户录音 / 快路径识别文字 / 固定话术 / 后台播报）。
    pub(super) fn trace_turn_input(&self, input: &TurnInput, kind: TurnKind) {
        if self.trace_handle().is_none() {
            return;
        }
        let kind = format!("{kind:?}");
        match input {
            TurnInput::Utterance {
                pcm, sample_rate, ..
            } => {
                let file = self.trace_audio("user", pcm, *sample_rate);
                self.trace_record(
                    "user_audio",
                    serde_json::json!({
                        "turn_kind": kind,
                        "sample_rate": sample_rate,
                        "duration_ms": (pcm.len() as u64) * 1000 / u64::from((*sample_rate).max(1)),
                        "file": file,
                    }),
                );
            }
            TurnInput::FastStop { text } => self.trace_record(
                "fast_stop",
                serde_json::json!({"turn_kind": kind, "text": text}),
            ),
            TurnInput::Say { text, close } => self.trace_record(
                "say",
                serde_json::json!({"turn_kind": kind, "text": text, "close": close}),
            ),
            TurnInput::Report { question, results } => self.trace_record(
                "report_input",
                serde_json::json!({"turn_kind": kind, "question": question, "results": results}),
            ),
        }
    }

    /// 合成语音（已重采样到下行采样率）追加到本轮缓存。
    pub(super) fn trace_tts_push(&mut self, samples: &[i16]) {
        if let Some(turn) = self.turn.as_mut()
            && let Some(buffer) = turn.trace_tts.as_mut()
        {
            let room = MAX_TRACE_TTS_SAMPLES.saturating_sub(buffer.len());
            buffer.extend_from_slice(&samples[..samples.len().min(room)]);
        }
    }

    /// 轮次结束：结果、播出的句子、完整回复、耗时与合成语音 WAV（每轮只写一次）。
    pub(super) fn trace_turn_end(&mut self, outcome: &str) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        let Some(tts) = turn.trace_tts.take() else {
            return;
        };
        let reply = turn
            .pending_messages
            .iter()
            .rev()
            .find(|m| m.role == moss_voice_providers::Role::Assistant)
            .and_then(|m| m.content.clone());
        let data = serde_json::json!({
            "turn_kind": format!("{:?}", turn.kind),
            "outcome": outcome,
            "user_text": turn.user,
            "spoken": turn.spoken,
            "reply": reply,
            "text_reply": turn.debug_text_reply,
            "stages_ms": turn.timing.as_ref().map(|t| &t.stages),
            "tools_ms": turn.timing.as_ref().map(|t| &t.tools),
            "elapsed_ms": turn.started.elapsed().as_millis() as u64,
            "frames_sent": turn.frames_sent,
        });
        let downlink = self.shared.config.downlink_sample_rate;
        let file = self.trace_audio("moss", &tts, downlink);
        let mut data = data;
        data["tts_file"] = serde_json::json!(file);
        data["tts_sample_rate"] = serde_json::json!(downlink);
        self.trace_record("turn_end", data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_keeps_text_but_drops_secrets_and_image_data() {
        let mut value = serde_json::json!({
            "messages": [
                {"role": "user", "content": "帮我刻“你好”，api_key=sk-abcdefghijklmnopqrstuv 别外传"},
                {"role": "user", "content": [{"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", "A".repeat(400))}}]},
            ],
            "ip": "192.168.1.5",
        });
        scrub_trace_value(&mut value);
        let text = value.to_string();
        assert!(text.contains("帮我刻“你好”"), "原话保留：{text}");
        assert!(
            !text.contains("sk-abcdefghijklmnopqrstuv"),
            "密钥去掉：{text}"
        );
        assert!(text.contains("图片数据已省略"), "{text}");
        assert!(text.contains("192.168.1.5"), "完整记录不遮 IP：{text}");
    }
}
