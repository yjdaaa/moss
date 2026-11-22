//! 完整过程记录（n105，默认关）：会话这一层负责带上设备、会话与轮次，按发生先后写进当天时间线
//! （`logs/process-trace/<日期>/timeline.jsonl` 与可直接阅读的 `timeline.txt`）。
//!
//! 一轮对话里依次可能出现：设备开始拾音 → 检测到用户开口 → 设备停止拾音 → 用户说完一句话（录音 WAV，
//! 上行采样率）→ 识别结果 → 大模型第 N 轮请求与回复（来自对话任务）→ 工具执行结果 → 开始播放第 N 句 →
//! 本轮结束（播出的句子、完整回复、合成语音 WAV，下行采样率）→ 本轮耗时；中间穿插会话里的诊断事件与打断。
//! 写入前去掉疑似密钥 / 令牌（不遮 IP、不截断原话），多模态图片的 data URL 只留长度。

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

/// 设备短号：设备编号里最后 4 个字母或数字（时间线里区分多台小智）。
fn device_short(device_id: &str) -> String {
    let chars: Vec<char> = device_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let tail: String = chars[chars.len().saturating_sub(4)..].iter().collect();
    if tail.is_empty() {
        "设备".to_owned()
    } else {
        tail
    }
}

/// 这台设备某一轮的流程标签，如“语音·3344·第2轮”。
pub(crate) fn turn_flow(device_id: &str, turn: u64) -> String {
    format!("语音·{}·第{turn}轮", device_short(device_id))
}

/// 两轮之间（拾音、开口、连接等）的流程标签，如“语音·3344”。
pub(crate) fn device_flow(device_id: &str) -> String {
    format!("语音·{}", device_short(device_id))
}

fn clip(text: &str, max: usize) -> String {
    let flat = text.replace(['\r', '\n'], " ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

fn text_of<'a>(data: &'a Value, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or("")
}

fn outcome_label(outcome: &str) -> &str {
    match outcome {
        "done" => "正常播完",
        "interrupted" => "被打断",
        "ignored" => "没听到有效内容，已忽略",
        "not_for_me" => "判断不是对小智说的，不回应",
        "music_started" => "开始放音乐",
        "disconnected" => "设备断开",
        "replaced" => "被下一轮接替",
        other => other,
    }
}

/// 一步语音事件的中文说明（`kind` 为步骤类型，`data` 为这一步的细节）。
pub(crate) fn voice_step_text(kind: &str, data: &Value) -> String {
    match kind {
        "listen_start" => format!("设备开始拾音（{}）", text_of(data, "mode")),
        "listen_stop" => "设备停止拾音".to_owned(),
        "speech_began" => "检测到用户开口".to_owned(),
        "user_audio" => {
            let seconds = data["duration_ms"].as_u64().unwrap_or(0) as f64 / 1000.0;
            match data["file"].as_str() {
                Some(file) => format!("用户说完一句话（{seconds:.1} 秒），录音：{file}"),
                None => format!("用户说完一句话（{seconds:.1} 秒），录音未保存"),
            }
        }
        "asr" => {
            let raw = text_of(data, "raw");
            let corrected = text_of(data, "corrected");
            if raw.trim().is_empty() {
                "识别结果为空（没听清）".to_owned()
            } else if corrected.is_empty() || corrected == raw {
                format!("识别结果：「{}」", clip(raw, 120))
            } else {
                format!(
                    "识别结果：「{}」，纠正为「{}」",
                    clip(raw, 120),
                    clip(corrected, 120)
                )
            }
        }
        "fast_stop" => format!(
            "边说边识别命中停止快路径：「{}」",
            clip(text_of(data, "text"), 60)
        ),
        "say" => format!("播报固定话术：「{}」", clip(text_of(data, "text"), 80)),
        "report_input" => format!(
            "后台查询有结果，准备主动播报：{}",
            clip(text_of(data, "question"), 60)
        ),
        "llm_round" => {
            let head = format!(
                "大模型第{}轮（{} ms）",
                data["round"].as_u64().unwrap_or(1),
                data["elapsed_ms"].as_u64().unwrap_or(0)
            );
            if let Some(error) = data["error"].as_str() {
                return format!("{head}：请求失败：{}", clip(error, 120));
            }
            let calls: Vec<&str> = data["reply_tool_calls"]
                .as_array()
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|c| c["function"]["name"].as_str())
                        .collect()
                })
                .unwrap_or_default();
            let reply = text_of(data, "reply_text").trim();
            match (calls.is_empty(), reply.is_empty()) {
                (false, true) => format!("{head}：要求调用工具 {}", calls.join("、")),
                (false, false) => format!(
                    "{head}：要求调用工具 {}，并说「{}」",
                    calls.join("、"),
                    clip(reply, 60)
                ),
                (true, false) => format!("{head}：回复「{}」", clip(reply, 120)),
                (true, true) => format!("{head}：没有回复内容"),
            }
        }
        "tool_call" => format!(
            "工具 {} 执行完（{} ms）：参数 {}，结果「{}」",
            text_of(data, "name"),
            data["elapsed_ms"].as_u64().unwrap_or(0),
            clip(text_of(data, "arguments"), 80),
            clip(text_of(data, "result"), 100)
        ),
        "jev" => {
            if data["applied"].as_bool() != Some(true) {
                return "Jev 前置筛选没有结果（超时或未启用），按全量工具继续".to_owned();
            }
            let fast = data["fast_read"]
                .as_str()
                .map(|intent| format!("，判断为只读快答（{intent}）"))
                .unwrap_or_default();
            format!(
                "Jev 前置筛选：保留 {} / {} 个工具{fast}{}",
                data["tools_after"].as_u64().unwrap_or(0),
                data["tools_before"].as_u64().unwrap_or(0),
                if data["cached"].as_bool() == Some(true) {
                    "（命中缓存）"
                } else {
                    ""
                }
            )
        }
        "sentence_play" => format!(
            "开始播放第{}句：「{}」",
            data["index"].as_u64().unwrap_or(1),
            clip(text_of(data, "text"), 100)
        ),
        "interrupt" => format!("打断当前回答：{}", text_of(data, "reason")),
        "turn_end" => {
            let spoken: Vec<&str> = data["spoken"]
                .as_array()
                .map(|s| s.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let said = if spoken.is_empty() {
                "没有播出内容".to_owned()
            } else {
                format!("播出「{}」", clip(&spoken.join(""), 100))
            };
            let audio = match data["tts_file"].as_str() {
                Some(file) => format!("，合成语音：{file}"),
                None => String::new(),
            };
            format!(
                "本轮结束（{}）：{said}{audio}",
                outcome_label(text_of(data, "outcome"))
            )
        }
        other => other.to_owned(),
    }
}

/// 诊断事件的中文说明：级别 + 原文（+ 下一步建议）。
pub(crate) fn event_step_text(event: &Value) -> String {
    let level = match text_of(event, "level") {
        "warn" => "提醒",
        "error" => "错误",
        _ => "事件",
    };
    let hint = text_of(event, "hint");
    if hint.is_empty() {
        format!("{level}：{}", clip(text_of(event, "text"), 160))
    } else {
        format!(
            "{level}：{}（{}）",
            clip(text_of(event, "text"), 160),
            clip(hint, 80)
        )
    }
}

/// 不属于某台设备会话的诊断事件按来源归类。
pub(crate) fn event_flow(event: &Value) -> String {
    match text_of(event, "source") {
        "laser" => "诊断·激光".to_owned(),
        "moss" => "诊断·MOSS".to_owned(),
        _ => "诊断·语音".to_owned(),
    }
}

/// 一轮耗时的中文说明：列出记到的各阶段（毫秒，从开始录音算起）。
pub(crate) fn timing_step_text(timing: &Value) -> String {
    let stages = timing["stages"]
        .as_object()
        .map(|stages| {
            let mut list: Vec<(&String, u64)> = stages
                .iter()
                .filter_map(|(k, v)| v.as_u64().map(|ms| (k, ms)))
                .collect();
            list.sort_by_key(|(_, ms)| *ms);
            list.iter()
                .map(|(k, ms)| format!("{k} {ms}"))
                .collect::<Vec<_>>()
                .join("、")
        })
        .unwrap_or_default();
    format!(
        "本轮耗时（{}，毫秒）：{stages}",
        outcome_label(text_of(timing, "outcome"))
    )
}

/// 后台任务（记忆整理、长期记忆提取、按人记忆）调用的大模型：完整过程记录打开时包一层，
/// 每次请求与回复都记进时间线；关着时原样返回，不多一层调用。
pub(crate) fn background_llm(
    conversation: &Conversation,
    device_id: &str,
    purpose: &'static str,
) -> Arc<dyn moss_voice_providers::Llm> {
    match conversation
        .process_trace
        .clone()
        .filter(laser_runtime::ProcessTrace::enabled)
    {
        Some(trace) => Arc::new(TracingLlm {
            inner: conversation.llm.clone(),
            trace,
            flow: format!("{}·后台", device_flow(device_id)),
            purpose,
        }),
        None => conversation.llm.clone(),
    }
}

struct TracingLlm {
    inner: Arc<dyn moss_voice_providers::Llm>,
    trace: laser_runtime::ProcessTrace,
    flow: String,
    purpose: &'static str,
}

impl moss_voice_providers::Llm for TracingLlm {
    fn chat<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [moss_voice_providers::ToolSpec],
        deltas: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> moss_voice_providers::BoxFuture<
        'a,
        Result<moss_voice_providers::LlmReply, moss_voice_providers::ProviderError>,
    > {
        Box::pin(async move {
            let began = Instant::now();
            let reply = self.inner.chat(messages, tools, deltas).await;
            let elapsed_ms = began.elapsed().as_millis() as u64;
            let mut data = serde_json::json!({
                "purpose": self.purpose,
                "elapsed_ms": elapsed_ms,
                "messages": messages,
                "reply_text": reply.as_ref().ok().map(|r| r.text.clone()),
                "error": reply.as_ref().err().map(ToString::to_string),
            });
            scrub_trace_value(&mut data);
            let step = match (data["reply_text"].as_str(), data["error"].as_str()) {
                (Some(text), None) => format!(
                    "后台{}调用大模型（{elapsed_ms} ms）：回复「{}」",
                    self.purpose,
                    clip(text, 100)
                ),
                (_, Some(error)) => format!(
                    "后台{}调用大模型失败（{elapsed_ms} ms）：{}",
                    self.purpose,
                    clip(error, 100)
                ),
                _ => format!("后台{}调用大模型（{elapsed_ms} ms）", self.purpose),
            };
            self.trace
                .record("voice", &self.flow, "background_llm", &step, data);
            reply
        })
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

    /// 当前活跃轮次的编号（暂停插话切回原轮次时读原轮次编号，不串到插话轮）。
    pub(super) fn active_turn_number(&self) -> u64 {
        self.turn
            .as_ref()
            .map(|t| t.turn_number)
            .unwrap_or(self.turn_number)
    }

    /// 当前流程标签：有进行中的轮次时带该轮编号，否则只带设备。
    pub(super) fn trace_flow(&self) -> String {
        if let Some(turn) = self.turn.as_ref() {
            turn_flow(self.device_id, turn.turn_number)
        } else {
            device_flow(self.device_id)
        }
    }

    /// 记一步语音事件，自动带上会话、设备、当前轮次与中文说明。
    pub(super) fn trace_record(&self, kind: &str, data: Value) {
        let flow = self.trace_flow();
        self.trace_record_in(&flow, self.active_turn_number(), kind, data);
    }

    fn trace_record_in(&self, flow: &str, turn: u64, kind: &str, mut data: Value) {
        let Some(trace) = self.trace_handle() else {
            return;
        };
        scrub_trace_value(&mut data);
        let step = voice_step_text(kind, &data);
        let mut line = serde_json::Map::new();
        line.insert("session_id".into(), Value::from(self.session_id));
        line.insert("device_id".into(), Value::from(self.device_id));
        line.insert("turn".into(), Value::from(turn));
        match data {
            Value::Object(map) => line.extend(map),
            other => {
                line.insert("data".into(), other);
            }
        }
        trace.record("voice", flow, kind, &step, Value::Object(line));
    }

    /// 来自对话任务的一步（大模型轮次要编号）。
    pub(super) fn trace_turn_event(&mut self, kind: &str, mut data: Value) {
        if self.trace_handle().is_none() {
            return;
        }
        if kind == "llm_round"
            && let Some(turn) = self.turn.as_mut()
        {
            turn.trace_rounds += 1;
            data["round"] = Value::from(turn.trace_rounds);
        }
        self.trace_record(kind, data);
    }

    /// 把一段 PCM 存成 WAV，返回相对 `logs/process-trace/` 的路径。
    pub(super) fn trace_audio_for(
        &self,
        turn_number: u64,
        label: &str,
        pcm: &[i16],
        sample_rate: u32,
    ) -> Option<String> {
        if pcm.is_empty() {
            return None;
        }
        let trace = self.trace_handle()?;
        let stem = format!("{}-t{turn_number}-{label}", device_short(self.device_id));
        trace.save_file(
            "voice",
            &stem,
            "wav",
            &moss_voice_providers::wav::encode_wav(pcm, sample_rate),
        )
    }

    /// 轮次开始：记下这一轮的输入（用户录音 / 快路径识别文字 / 固定话术 / 后台播报）。
    /// `start_turn` 调用时 `self.turn` 尚未写入新轮次，这里显式归到新的 `self.turn_number`。
    pub(super) fn trace_turn_input(&self, input: &TurnInput, kind: TurnKind) {
        if self.trace_handle().is_none() {
            return;
        }
        let turn_no = self.turn_number;
        let flow = turn_flow(self.device_id, turn_no);
        let kind = format!("{kind:?}");
        match input {
            TurnInput::Utterance {
                pcm, sample_rate, ..
            } => {
                let file = self.trace_audio_for(turn_no, "user", pcm, *sample_rate);
                self.trace_record_in(
                    &flow,
                    turn_no,
                    "user_audio",
                    serde_json::json!({
                        "turn_kind": kind,
                        "sample_rate": sample_rate,
                        "duration_ms": (pcm.len() as u64) * 1000 / u64::from((*sample_rate).max(1)),
                        "file": file,
                    }),
                );
            }
            TurnInput::FastStop { text } => self.trace_record_in(
                &flow,
                turn_no,
                "fast_stop",
                serde_json::json!({"turn_kind": kind, "text": text}),
            ),
            TurnInput::Say { text, close } => self.trace_record_in(
                &flow,
                turn_no,
                "say",
                serde_json::json!({"turn_kind": kind, "text": text, "close": close}),
            ),
            TurnInput::Report { question, results } => self.trace_record_in(
                &flow,
                turn_no,
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
        let turn_no = turn.turn_number;
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
        let file = self.trace_audio_for(turn_no, "moss", &tts, downlink);
        let mut data = data;
        data["tts_file"] = serde_json::json!(file);
        data["tts_sample_rate"] = serde_json::json!(downlink);
        let flow = turn_flow(self.device_id, turn_no);
        self.trace_record_in(&flow, turn_no, "turn_end", data);
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

    #[test]
    fn step_texts_say_what_each_step_does() {
        assert_eq!(turn_flow("aa:bb:cc:11:22:33", 3), "语音·2233·第3轮");
        assert_eq!(device_flow(""), "语音·设备");
        assert_eq!(
            voice_step_text(
                "user_audio",
                &serde_json::json!({"duration_ms": 3200, "file": "2026-11-22/voice-files/a.wav"})
            ),
            "用户说完一句话（3.2 秒），录音：2026-11-22/voice-files/a.wav"
        );
        assert_eq!(
            voice_step_text(
                "asr",
                &serde_json::json!({"raw": "段木", "corrected": "椴木"})
            ),
            "识别结果：「段木」，纠正为「椴木」"
        );
        assert_eq!(
            voice_step_text(
                "asr",
                &serde_json::json!({"raw": "你好", "corrected": "你好"})
            ),
            "识别结果：「你好」"
        );
        assert_eq!(
            voice_step_text(
                "llm_round",
                &serde_json::json!({"round": 1, "elapsed_ms": 820, "reply_text": "",
                    "reply_tool_calls": [{"function": {"name": "get_time"}}]})
            ),
            "大模型第1轮（820 ms）：要求调用工具 get_time"
        );
        assert_eq!(
            voice_step_text(
                "llm_round",
                &serde_json::json!({"round": 2, "elapsed_ms": 300, "reply_text": "现在十二点。", "reply_tool_calls": []})
            ),
            "大模型第2轮（300 ms）：回复「现在十二点。」"
        );
        assert!(
            voice_step_text(
                "llm_round",
                &serde_json::json!({"round": 1, "elapsed_ms": 5, "error": "429 限流"})
            )
            .ends_with("请求失败：429 限流")
        );
        assert_eq!(
            voice_step_text(
                "turn_end",
                &serde_json::json!({"outcome": "interrupted", "spoken": ["好的，", "马上"], "tts_file": "x.wav"})
            ),
            "本轮结束（被打断）：播出「好的，马上」，合成语音：x.wav"
        );
        assert_eq!(
            event_step_text(
                &serde_json::json!({"level": "warn", "text": "识别超时", "hint": "检查网络"})
            ),
            "提醒：识别超时（检查网络）"
        );
        assert_eq!(
            event_flow(&serde_json::json!({"source": "laser"})),
            "诊断·激光"
        );
        assert_eq!(
            timing_step_text(&serde_json::json!({"outcome": "done",
                "stages": {"first_audio": 900, "utterance_end": 0, "final_asr": 320}})),
            "本轮耗时（正常播完，毫秒）：utterance_end 0、final_asr 320、first_audio 900"
        );
    }

    #[test]
    fn tracing_llm_scrubs_secrets_before_formatting_step_text() {
        use moss_voice_providers::Llm;
        struct SecretLlm;
        impl moss_voice_providers::Llm for SecretLlm {
            fn chat<'a>(
                &'a self,
                _messages: &'a [ChatMessage],
                _tools: &'a [moss_voice_providers::ToolSpec],
                _deltas: tokio::sync::mpsc::UnboundedSender<String>,
            ) -> moss_voice_providers::BoxFuture<
                'a,
                Result<moss_voice_providers::LlmReply, moss_voice_providers::ProviderError>,
            > {
                Box::pin(async {
                    Ok(moss_voice_providers::LlmReply {
                        text: "记住密钥 sk-abcdefghijklmnopqrstuv 不要外泄".into(),
                        tool_calls: Vec::new(),
                    })
                })
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        laser_runtime::settings_store::update(
            &laser_runtime::settings_store::settings_path(tmp.path()),
            |map| {
                map.insert(
                    laser_runtime::process_trace::SETTING_KEY.into(),
                    serde_json::json!(true),
                );
                Ok(())
            },
        )
        .unwrap();
        let trace = laser_runtime::ProcessTrace::new(tmp.path());
        let llm = TracingLlm {
            inner: Arc::new(SecretLlm),
            trace: trace.clone(),
            flow: "语音·1234·后台".into(),
            purpose: "记忆整理",
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        rt.block_on(async {
            llm.chat(&[], &[], tx).await.unwrap();
        });
        trace.flush();
        let day = std::fs::read_dir(trace.dir())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        for file in [
            laser_runtime::process_trace::TIMELINE_JSONL,
            laser_runtime::process_trace::TIMELINE_TXT,
        ] {
            let content = std::fs::read_to_string(day.join(file)).unwrap();
            assert!(
                !content.contains("sk-abcdefghijklmnopqrstuv"),
                "{file} 的 step 与数据不得泄露密钥：\n{content}"
            );
        }
    }
}
