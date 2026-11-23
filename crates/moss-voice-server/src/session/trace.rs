//! 完整过程记录（n105，默认关）：会话这一层负责带上设备、会话与轮次，按发生先后写进当天时间线
//! （`logs/process-trace/<日期>/timeline.jsonl` 与可直接阅读的 `timeline.txt`）。
//!
//! 一轮对话里依次可能出现：设备开始拾音 → 判停方式与降噪情况 → 检测到用户开口（哪个判停判的）→ 边说边识别的
//! 中间文字 → 命令词命中 → 检测到用户停止出声 → 用户说完一句话（录音 WAV，上行采样率）→ 识别结果（原文 → 同音纠正
//! → 命令词改判，分开写）或被忽略的原因 → 语义判停概率与开口前要不要多等 → 大模型第 N 轮请求 → 第 N 轮回复（纯大模型
//! 耗时）→ 工具执行结果（含快路径直接调用的工具）→ 第 N 句开始合成 / 合成出音频 / 合成失败 → 开始播放第 N 句 →
//! 本轮结束（播出的句子、完整回复、合成语音 WAV，下行采样率）→ 本轮耗时；中间穿插会话里的诊断事件与打断。
//!
//! 2026-11-23 n110：每一步的 `step` 是一句普通人和 AI 都看得懂的中文；`kind` 保持英文蛇形（机器读），
//! JSON 细节保留原始数据、不截断，合成语音不限时长。写入前去掉疑似密钥 / 令牌（不遮 IP、不截断原话），
//! 多模态图片的 data URL 只留长度。开关关着时所有记录入口第一步就返回，不做格式化、不复制音频。

use super::*;

const VOICE_SECRET_WORDS: &[&str] = &[
    "key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "passcode",
    "pin",
    "authorization",
    "auth",
    "credential",
    "cookie",
    "sig",
    "signature",
    "jwt",
    "otp",
];

fn is_voice_secret_key(key: &str) -> bool {
    let chars: Vec<char> = key.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_ascii_uppercase() && !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower)
            {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words.iter().any(|w| {
        let singular = w.strip_suffix('s').unwrap_or(w);
        VOICE_SECRET_WORDS.contains(&w.as_str()) || VOICE_SECRET_WORDS.contains(&singular)
    })
}

/// 去掉凭据并省略图片数据（递归处理字符串与敏感字段名，保留工具 JSON Schema 的字段定义对象）。
pub(crate) fn scrub_trace_value(value: &mut Value) {
    scrub_trace_value_inner(value, false);
}

fn scrub_trace_value_inner(value: &mut Value, in_schema_properties: bool) {
    match value {
        Value::String(text) => {
            if text.starts_with("data:") && text.len() > 256 {
                *text = format!("[图片数据已省略，{} 字节]", text.len());
            } else {
                *text = moss_voice_providers::scrub_text_with_options(text, false, 0, false).0;
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| scrub_trace_value_inner(item, false)),
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if in_schema_properties && v.is_object() {
                    scrub_trace_value_inner(v, false);
                    continue;
                }
                let non_empty = match v {
                    Value::String(s) => !s.is_empty(),
                    Value::Number(_) => true,
                    Value::Array(a) => !a.is_empty(),
                    Value::Object(m) => !m.is_empty(),
                    _ => false,
                };
                if non_empty && is_voice_secret_key(k) {
                    *v = Value::String("[REDACTED]".into());
                } else {
                    scrub_trace_value_inner(v, k == "properties");
                }
            }
        }
        _ => {}
    }
}

/// 直接把一步语音记录写进指定的 `ProcessTrace`（会话与后台 `Turn` 任务共用，确保抢跑等待或插话暂停期间不经通道积压、不丢记录）。
pub(crate) fn write_voice_trace(
    trace: &laser_runtime::ProcessTrace,
    session_id: &str,
    device_id: &str,
    flow: &str,
    turn: u64,
    kind: &str,
    mut data: Value,
) {
    scrub_trace_value(&mut data);
    let step = voice_step_text(kind, &data);
    let mut line = serde_json::Map::new();
    line.insert("session_id".into(), Value::from(session_id));
    line.insert("device_id".into(), Value::from(device_id));
    line.insert("turn".into(), Value::from(turn));
    match data {
        Value::Object(map) => line.extend(map),
        other => {
            line.insert("data".into(), other);
        }
    }
    trace.record("voice", flow, kind, &step, Value::Object(line));
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

/// 各阶段在一轮里的先后（同一毫秒内按这个顺序列出）。
const STAGE_ORDER: &[&str] = &[
    "recording_start",
    "vad_start",
    "barge_in",
    "vad_end",
    "utterance_end",
    "interim_stop",
    "final_asr",
    "smart_turn",
    "fast_route",
    "jev_filter",
    "first_llm_delta",
    "first_sentence",
    "first_synthesis_start",
    "first_synthesis_ready",
    "first_audio",
    "first_audio_frame",
    "turn_end",
];

/// 一轮耗时里各阶段键的中文名（JSON 里保留原键）。
fn stage_label(key: &str) -> &str {
    match key {
        "recording_start" => "开始拾音",
        "vad_start" => "判停确认开口",
        "vad_end" => "判停确认说完",
        "barge_in" => "插话开头",
        "utterance_end" => "这句话交给处理",
        "interim_stop" => "边说边识别命中停止",
        "final_asr" => "识别出最终文字",
        "smart_turn" => "语义判停给出概率",
        "fast_route" => "走快路径",
        "jev_filter" => "Jev 前置筛选完成",
        "first_llm_delta" => "大模型吐出第一个字",
        "first_sentence" => "切出第一句",
        "first_synthesis_start" => "开始合成第一句",
        "first_synthesis_ready" => "第一句音频就绪",
        "first_audio_frame" => "第一帧语音发给设备",
        "first_audio" => "第一段语音就绪",
        "turn_end" => "本轮结束",
        other => other,
    }
}

/// 轮次种类（`TurnKind` 的 Debug 名）的中文名。
pub(crate) fn turn_kind_label(kind: &str) -> &str {
    match kind {
        "Answer" => "回答用户",
        "Greeting" => "唤醒回应",
        "Goodbye" => "空闲再见",
        "Notice" => "加工结束播报",
        other => other,
    }
}

/// Jev 只读快答意图（`ReadIntent` 的 Debug 名）的中文名。
fn read_intent_label(intent: &str) -> &str {
    match intent {
        "Status" => "查激光机状态",
        "Connection" => "查激光机连接",
        "Progress" => "查加工进度",
        "Remaining" => "查剩余时间",
        "Material" => "查当前材料参数",
        other => other,
    }
}

/// 常用工具的中文用途（没有登记的工具用调用方附带的 `description` 首句）。
fn tool_label(data: &Value) -> String {
    let name = text_of(data, "name");
    let known = match name {
        "laser_safe_action_tool" => "激光机安全动作：状态、停止、急停等",
        "check_laser_connection_tool" => "检查激光机连接",
        "laser_workflow_tool" => "加工任务：查询、生成、发送",
        "material_params_tool" => "材料参数库",
        "end_conversation" => "结束这次对话",
        "stay_silent" => "判断不是对小智说的、不回应",
        "play_music" => "播放本地音乐",
        _ => "",
    };
    let described = if known.is_empty() {
        let description = text_of(data, "description");
        description
            .split(['。', '\n', '；', ';'])
            .next()
            .unwrap_or("")
            .trim()
            .to_owned()
    } else {
        known.to_owned()
    };
    if described.is_empty() {
        name.to_owned()
    } else {
        format!("{name}（{}）", clip(&described, 40))
    }
}

fn denoise_engine_label(engine: &str) -> &str {
    match engine {
        "gtcrn" => "GTCRN",
        "dpdfnet" => "DPDFNet",
        "" => "降噪模型",
        other => other,
    }
}

fn ms_of(data: &Value, key: &str) -> u64 {
    data[key].as_u64().unwrap_or(0)
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
        "speech_began" => {
            let engine = text_of(data, "engine");
            let head = if data["resumed"].as_bool() == Some(true) {
                "用户停顿后接着说"
            } else {
                "检测到用户开口"
            };
            if engine.is_empty() {
                head.to_owned()
            } else {
                format!("{head}（由{engine}判定）")
            }
        }
        "speech_end" => format!("检测到用户停止出声（由{}判定）", text_of(data, "engine")),
        "vad_engine" => {
            let engine = text_of(data, "engine");
            let reason = text_of(data, "fallback_reason");
            match (reason.is_empty(), data["previous"].as_str()) {
                (true, None) => format!("这次连接判断用户开口和说完用的是{engine}"),
                (true, Some(previous)) => format!("判停方式从{previous}换成{engine}"),
                (false, None) => format!(
                    "本地判停模型没能用上（{}），这次连接改用{engine}",
                    clip(reason, 120)
                ),
                (false, Some(_)) => {
                    format!("本地判停出错（{}），之后改用{engine}", clip(reason, 120))
                }
            }
        }
        "vad_skipped" => format!(
            "判停处理跟不上，跳过了 {} 帧没判（约 {} ms 音频，排队超过 {} ms）",
            ms_of(data, "frames"),
            ms_of(data, "audio_ms"),
            ms_of(data, "deadline_ms")
        ),
        "denoise" => {
            let stage = text_of(data, "stage");
            let engine = denoise_engine_label(text_of(data, "engine"));
            let model = text_of(data, "model");
            if data["applied"].as_bool() == Some(true) {
                format!("这句话的音频先经 {engine} 降噪（{model}）再送{stage}")
            } else if let Some(error) = data["error"].as_str() {
                format!(
                    "{engine} 降噪器没建起来（{}），这句话{stage}用原始音频",
                    clip(error, 80)
                )
            } else {
                format!("没开降噪，这句话{stage}用原始音频")
            }
        }
        "kws_hit" => {
            let when = text_of(data, "when");
            if when.is_empty() {
                format!("命令词模型听到{}命令", text_of(data, "keyword"))
            } else {
                format!("命令词模型听到{}命令（{when}）", text_of(data, "keyword"))
            }
        }
        "interim" => {
            let text = text_of(data, "text");
            let corrected = text_of(data, "corrected");
            if corrected.is_empty() || corrected == text {
                format!("边说边识别：「{}」", clip(text, 120))
            } else {
                format!(
                    "边说边识别：「{}」，纠正为「{}」",
                    clip(text, 120),
                    clip(corrected, 120)
                )
            }
        }
        "follow_probe" => match text_of(data, "phase") {
            "start" => format!(
                "思考中听到一阵声音（{} ms），先交给判停模型确认是不是用户接着说",
                ms_of(data, "head_ms")
            ),
            "confirmed" => {
                "判停模型确认是人声：用户接着说，放弃还没开口的回答、与前半句合并重听".to_owned()
            }
            _ => "判停模型认为那阵声音不是人声，照常回答".to_owned(),
        },
        "jev_hold_check" => format!(
            "静音判停后先请 Jev 判断「{}」说完没有，期间继续拾音",
            clip(text_of(data, "text"), 80)
        ),
        "jev_hold" => match text_of(data, "decision") {
            "extend" => "Jev 判断这句话还没说完，延长拾音等用户接着说（最多 1.2 秒）".to_owned(),
            "expired" => "延长的拾音到期，用户没有接着说，按说完处理".to_owned(),
            "late" => "Jev 判断回来时已过续听期限，按说完处理".to_owned(),
            _ => "Jev 判断这句话已经说完，按说完处理".to_owned(),
        },
        "smart_turn" => {
            if let Some(prob) = data["probability"].as_f64() {
                format!(
                    "语义判停：这句话已说完的概率 {prob:.2}（{} ms 算出）",
                    ms_of(data, "elapsed_ms")
                )
            } else {
                format!(
                    "语义判停没用上：{}，只按句子本身判断",
                    text_of(data, "reason")
                )
            }
        }
        "hold_decision" => {
            let text_wait = ms_of(data, "text_wait_ms");
            let basis = match data["smart_turn_prob"].as_f64() {
                Some(prob) => format!("句尾规则要等 {text_wait} ms，语义判停说完概率 {prob:.2}"),
                None => format!("句尾规则要等 {text_wait} ms，没有语义判停结果"),
            };
            if data["held"].as_bool() == Some(true) {
                format!(
                    "这句话像是还没说完（{basis}），开口前再等 {} ms，期间接着说会合并",
                    ms_of(data, "extra_wait_ms")
                )
            } else {
                format!("判定这句话已说完（{basis}），马上处理")
            }
        }
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
            let Some(homophone) = data["homophone"].as_str() else {
                // 旧记录（n110 之前）没有分开同音纠正与命令词改判
                return if raw.trim().is_empty() {
                    "识别结果为空（没听清）".to_owned()
                } else if corrected.is_empty() || corrected == raw {
                    format!("识别结果：「{}」", clip(raw, 120))
                } else {
                    format!(
                        "识别结果：「{}」，纠正为「{}」",
                        clip(raw, 120),
                        clip(corrected, 120)
                    )
                };
            };
            let mut text = if raw.trim().is_empty() {
                "识别结果为空（没听清）".to_owned()
            } else {
                format!("识别结果：「{}」", clip(raw, 120))
            };
            if homophone != raw {
                text.push_str(&format!("，同音纠正为「{}」", clip(homophone, 120)));
            }
            if data["kws_fused"].as_bool() == Some(true) {
                text.push_str(&format!(
                    "，命令词模型听到{}命令、改判为「{}」",
                    text_of(data, "kws_keyword"),
                    clip(corrected, 40)
                ));
            }
            text
        }
        "asr_ignored" => {
            let raw = text_of(data, "raw");
            if raw.trim().is_empty() {
                format!("没识别出文字（{}），不回应", text_of(data, "reason"))
            } else {
                format!(
                    "识别结果「{}」{}，不回应",
                    clip(raw, 60),
                    text_of(data, "reason")
                )
            }
        }
        "fast_stop" => match text_of(data, "source") {
            "kws" => "思考中命令词模型听到“停”，不等识别直接走停止快路径".to_owned(),
            "recheck" => format!(
                "思考中那阵声音单独识别出停止说法「{}」，打断回答走停止快路径",
                clip(text_of(data, "text"), 60)
            ),
            _ => format!(
                "边说边识别整句命中停止词「{}」，立即走停止快路径",
                clip(text_of(data, "text"), 60)
            ),
        },
        "say" => format!("播报固定话术：「{}」", clip(text_of(data, "text"), 80)),
        "report_input" => format!(
            "后台查询有结果，准备主动播报：{}",
            clip(text_of(data, "question"), 60)
        ),
        "llm_request" => format!(
            "向大模型发出第{}轮请求（{} 条消息，{} 个工具）",
            data["round"].as_u64().unwrap_or(1),
            ms_of(data, "message_count"),
            ms_of(data, "tool_count")
        ),
        "llm_round" => {
            let head = match (data.get("first_token_ms"), data["first_token_ms"].as_u64()) {
                (_, Some(first)) => format!(
                    "大模型第{}轮回复完（首字 {first} ms，完整 {} ms）",
                    data["round"].as_u64().unwrap_or(1),
                    ms_of(data, "elapsed_ms")
                ),
                // 回复一次性到达（没有单独的首字时刻）
                (Some(_), None) => format!(
                    "大模型第{}轮回复完（{} ms）",
                    data["round"].as_u64().unwrap_or(1),
                    ms_of(data, "elapsed_ms")
                ),
                (None, None) => format!(
                    "大模型第{}轮（{} ms）",
                    data["round"].as_u64().unwrap_or(1),
                    ms_of(data, "elapsed_ms")
                ),
            };
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
        "tool_call" => {
            let label = tool_label(data);
            let gap = if label.ends_with('）') { "" } else { " " };
            format!(
                "{}工具 {label}{gap}执行完（{} ms）：参数 {}，结果「{}」",
                if data["fast_route"].as_bool() == Some(true) {
                    "快路径直接调用"
                } else {
                    ""
                },
                ms_of(data, "elapsed_ms"),
                clip(text_of(data, "arguments"), 80),
                clip(text_of(data, "result"), 100)
            )
        }
        "tts_start" => format!(
            "{}第{}句{}：「{}」",
            if data["fallback"].as_bool() == Some(true) {
                "主合成失败，改用本地合成重新合成"
            } else {
                "开始合成"
            },
            data["index"].as_u64().unwrap_or(1),
            if data["fixed"].as_bool() == Some(true) {
                "（固定话术，可能直接用缓存）"
            } else {
                ""
            },
            clip(text_of(data, "text"), 100)
        ),
        "tts_ready" => format!(
            "第{}句合成出第一段音频（{} ms）",
            data["index"].as_u64().unwrap_or(1),
            ms_of(data, "elapsed_ms")
        ),
        "tts_failed" => format!(
            "第{}句合成失败（{} ms）：{}{}",
            data["index"].as_u64().unwrap_or(1),
            ms_of(data, "elapsed_ms"),
            clip(text_of(data, "error"), 120),
            if data["fallback"].as_bool() == Some(true) {
                "，改用本地合成"
            } else {
                ""
            }
        ),
        "jev" => {
            if data["applied"].as_bool() != Some(true) {
                return "Jev 前置筛选没有结果（超时或未启用），按全量工具继续".to_owned();
            }
            let fast = data["fast_read"]
                .as_str()
                .map(|intent| format!("，判断为只读快答（{}）", read_intent_label(intent)))
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

/// 一轮耗时的中文说明：按先后列出记到的各阶段（毫秒，从开始拾音算起；JSON 里保留原键）。
pub(crate) fn timing_step_text(timing: &Value) -> String {
    let stages = timing["stages"]
        .as_object()
        .map(|stages| {
            let mut list: Vec<(&String, u64)> = stages
                .iter()
                .filter_map(|(k, v)| v.as_u64().map(|ms| (k, ms)))
                .collect();
            list.sort_by_key(|(k, ms)| {
                let rank = STAGE_ORDER
                    .iter()
                    .position(|known| known == k)
                    .unwrap_or(STAGE_ORDER.len());
                (*ms, rank)
            });
            list.iter()
                .map(|(k, ms)| format!("{} {ms}", stage_label(k)))
                .collect::<Vec<_>>()
                .join("、")
        })
        .unwrap_or_default();
    format!(
        "本轮各阶段耗时（{}，从开始拾音算起，毫秒）：{stages}",
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
            let trace = self.trace.scoped();
            let began = Instant::now();
            // 先记“开始请求”：后台整理可能要好几秒，时间线上要看得出它和对话的真实先后
            let mut request = serde_json::json!({
                "purpose": self.purpose,
                "message_count": messages.len(),
            });
            scrub_trace_value(&mut request);
            trace.record(
                "voice",
                &self.flow,
                "background_llm_start",
                &format!(
                    "后台{}开始调用大模型（{} 条消息）",
                    self.purpose,
                    messages.len()
                ),
                request,
            );
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
            trace.record("voice", &self.flow, "background_llm", &step, data);
            reply
        })
    }
}

impl<'a> Session<'a> {
    /// 打开时返回记录器；有进行中的轮次且本轮已固定目录时优先沿用本轮句柄，避免中途换目录拆散同轮记录与附件。
    pub(super) fn trace_handle(&self) -> Option<laser_runtime::ProcessTrace> {
        if let Some(turn_trace) = self
            .turn
            .as_ref()
            .and_then(|t| t.trace.as_ref())
            .filter(|t| t.enabled())
        {
            return Some(turn_trace.clone());
        }
        self.conversation
            .as_ref()?
            .process_trace
            .clone()
            .filter(laser_runtime::ProcessTrace::enabled)
            .map(|trace| trace.scoped())
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
        let Some(trace) = self.trace_handle() else {
            return;
        };
        let flow = self.trace_flow();
        self.trace_record_in(&trace, &flow, self.active_turn_number(), kind, data);
    }

    fn trace_record_in(
        &self,
        trace: &laser_runtime::ProcessTrace,
        flow: &str,
        turn: u64,
        kind: &str,
        data: Value,
    ) {
        write_voice_trace(
            trace,
            self.session_id,
            self.device_id,
            flow,
            turn,
            kind,
            data,
        );
    }

    /// 来自对话任务的一步（大模型轮次由对话任务编号）。
    pub(super) fn trace_turn_event(&mut self, kind: &str, data: Value) {
        if self.trace_handle().is_none() {
            return;
        }
        self.trace_record(kind, data);
    }

    /// 把当前进行中的轮次编号同步给 `device_requests`（没有轮次时为 0），看图等旁路请求据此归到正确的轮次。
    /// 每次会话循环后调用，只在编号变化时加锁写一次。
    pub(super) fn sync_trace_turn(&mut self) {
        let current = self.turn.as_ref().map_or(0, |turn| turn.turn_number);
        if current == self.synced_turn {
            return;
        }
        self.synced_turn = current;
        if let Ok(mut map) = self.shared.device_requests.lock()
            && let Some(handle) = map.get_mut(self.device_id)
            && handle.session_id == self.session_id
        {
            handle.turn = current;
        }
    }

    /// 判停线程交来的事实（判停方式、跳过的帧、降噪情况）：记进当前流程，并更新会话记住的判停方式。
    pub(super) fn trace_vad_notes(&mut self, notes: Vec<(&'static str, Value)>) {
        let tracing_now = self.trace_handle().is_some();
        for (kind, data) in notes {
            if kind == "vad_engine" {
                self.vad_worker_reported = true;
                if let Some(engine) = data["engine"].as_str() {
                    self.vad_engine = engine.to_owned();
                }
                self.vad_fallback_reason = data["fallback_reason"].as_str().map(str::to_owned);
                if tracing_now && !self.listen.listen_manual {
                    self.vad_engine_traced = true;
                } else if self.listen.listen_manual {
                    // 按键说话期间不把自动判停引擎混进当前拾音，留待下次自动拾音时写出
                    continue;
                }
            }
            self.trace_record(kind, data);
        }
    }

    /// 命令词模型的一次命中（`when` 说明是在什么时候听到的）。
    pub(super) fn trace_keyword(
        &self,
        kind: moss_voice_providers::local::CommandKeyword,
        when: &str,
    ) {
        if self.trace_handle().is_none() {
            return;
        }
        let keyword = match kind {
            moss_voice_providers::local::CommandKeyword::Stop => "停止",
            moss_voice_providers::local::CommandKeyword::Exit => "退出",
        };
        self.trace_record(
            "kws_hit",
            serde_json::json!({"keyword": keyword, "when": when}),
        );
    }

    /// 把一段 PCM 存成 WAV，返回相对 `logs/process-trace/` 的路径。
    pub(super) fn trace_audio_for(
        &self,
        trace: &laser_runtime::ProcessTrace,
        turn_number: u64,
        label: &str,
        pcm: &[i16],
        sample_rate: u32,
    ) -> Option<String> {
        if pcm.is_empty() {
            return None;
        }
        let stem = format!("{}-t{turn_number}-{label}", device_short(self.device_id));
        trace.save_file(
            "voice",
            &stem,
            "wav",
            &moss_voice_providers::wav::encode_wav(pcm, sample_rate),
        )
    }

    /// 轮次开始：记下这一轮的输入（用户录音 / 快路径识别文字 / 固定话术 / 后台播报）。
    /// `start_turn` 调用时 `self.turn` 尚未写入新轮次，这里显式使用本轮固定的 `trace` 与新的 `self.turn_number`。
    pub(super) fn trace_turn_input(
        &self,
        input: &TurnInput,
        kind: TurnKind,
        trace: Option<&laser_runtime::ProcessTrace>,
    ) {
        let Some(trace) = trace else {
            return;
        };
        let turn_no = self.turn_number;
        let flow = turn_flow(self.device_id, turn_no);
        let kind = format!("{kind:?}");
        let kind_label = turn_kind_label(&kind).to_owned();
        match input {
            TurnInput::Utterance {
                pcm, sample_rate, ..
            } => {
                let file = self.trace_audio_for(trace, turn_no, "user", pcm, *sample_rate);
                self.trace_record_in(
                    trace,
                    &flow,
                    turn_no,
                    "user_audio",
                    serde_json::json!({
                        "turn_kind": kind,
                        "turn_kind_label": kind_label,
                        "sample_rate": sample_rate,
                        "duration_ms": (pcm.len() as u64) * 1000 / u64::from((*sample_rate).max(1)),
                        "file": file,
                    }),
                );
            }
            TurnInput::FastStop { text, source } => self.trace_record_in(
                trace,
                &flow,
                turn_no,
                "fast_stop",
                serde_json::json!({
                    "turn_kind": kind,
                    "turn_kind_label": kind_label,
                    "text": text,
                    "source": source.key(),
                }),
            ),
            TurnInput::Say { text, close } => self.trace_record_in(
                trace,
                &flow,
                turn_no,
                "say",
                serde_json::json!({"turn_kind": kind, "turn_kind_label": kind_label, "text": text, "close": close}),
            ),
            TurnInput::Report { question, results } => self.trace_record_in(
                trace,
                &flow,
                turn_no,
                "report_input",
                serde_json::json!({"turn_kind": kind, "turn_kind_label": kind_label, "question": question, "results": results}),
            ),
        }
    }

    /// 合成语音（已重采样到下行采样率）追加到本轮缓存（n110 起不限时长；开关关着时缓存为 `None`，不复制）。
    pub(super) fn trace_tts_push(&mut self, samples: &[i16]) {
        if let Some(turn) = self.turn.as_mut()
            && let Some(buffer) = turn.trace_tts.as_mut()
        {
            buffer.extend_from_slice(samples);
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
            "turn_kind_label": turn_kind_label(&format!("{:?}", turn.kind)),
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
        let pinned = self
            .turn
            .as_ref()
            .and_then(|t| t.trace.as_ref())
            .filter(|t| t.enabled())
            .cloned();
        let Some(trace) = pinned.or_else(|| self.trace_handle()) else {
            return;
        };
        let file = self.trace_audio_for(&trace, turn_no, "moss", &tts, downlink);
        let mut data = data;
        data["tts_file"] = serde_json::json!(file);
        data["tts_sample_rate"] = serde_json::json!(downlink);
        let flow = turn_flow(self.device_id, turn_no);
        self.trace_record_in(&trace, &flow, turn_no, "turn_end", data);
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
            "apiKey": "plain-secret-key-without-prefix",
            "lan_static_pin": 654321,
            "tools": [
                {"name": "set_config", "parameters": {"properties": {"key": {"type": "string"}}}}
            ],
            "ip": "192.168.1.5",
        });
        scrub_trace_value(&mut value);
        assert_eq!(value["apiKey"], "[REDACTED]");
        assert_eq!(value["lan_static_pin"], "[REDACTED]");
        assert_eq!(
            value["tools"][0]["parameters"]["properties"]["key"]["type"], "string",
            "工具 Schema 里的属性名不被当成密钥覆盖"
        );
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
                "stages": {"first_audio_frame": 900, "utterance_end": 0, "final_asr": 320,
                    "first_synthesis_start": 610, "vad_start": 0, "mystery_stage": 950}})),
            "本轮各阶段耗时（正常播完，从开始拾音算起，毫秒）：判停确认开口 0、这句话交给处理 0、\
             识别出最终文字 320、开始合成第一句 610、第一帧语音发给设备 900、mystery_stage 950"
        );
    }

    /// n110：新增步骤（判停方式、开口 / 说完、降噪、命令词、识别三步、被忽略的识别、语义判停、
    /// 开口前等待、边说边识别、接着说确认、大模型请求 / 回复、快路径工具、逐句合成）各有一句中文说明。
    #[test]
    fn pipeline_step_texts_are_plain_chinese() {
        let text = |kind: &str, data: Value| voice_step_text(kind, &data);
        assert_eq!(
            text(
                "vad_engine",
                serde_json::json!({"engine": "本地判停 Silero v6.2（silero_v6）"})
            ),
            "这次连接判断用户开口和说完用的是本地判停 Silero v6.2（silero_v6）"
        );
        assert_eq!(
            text(
                "vad_engine",
                serde_json::json!({"engine": "能量判停（按音量）",
                "fallback_reason": "设备上行音频是 24000 Hz，本地判停只支持 16 kHz"})
            ),
            "本地判停模型没能用上（设备上行音频是 24000 Hz，本地判停只支持 16 kHz），这次连接改用能量判停（按音量）"
        );
        assert_eq!(
            text(
                "speech_began",
                serde_json::json!({"engine": "能量判停（按音量）", "resumed": false})
            ),
            "检测到用户开口（由能量判停（按音量）判定）"
        );
        assert_eq!(
            text("speech_end", serde_json::json!({"engine": "本地判停"})),
            "检测到用户停止出声（由本地判停判定）"
        );
        assert_eq!(
            text(
                "vad_skipped",
                serde_json::json!({"frames": 3, "audio_ms": 180, "deadline_ms": 1500})
            ),
            "判停处理跟不上，跳过了 3 帧没判（约 180 ms 音频，排队超过 1500 ms）"
        );
        assert_eq!(
            text(
                "denoise",
                serde_json::json!({"stage": "识别", "applied": true,
                "model": "gtcrn_simple.onnx", "engine": "gtcrn"})
            ),
            "这句话的音频先经 GTCRN 降噪（gtcrn_simple.onnx）再送识别"
        );
        assert_eq!(
            text(
                "denoise",
                serde_json::json!({"stage": "判停", "applied": false,
                "model": "gtcrn_simple.onnx", "engine": "gtcrn", "error": "降噪器没建起来"})
            ),
            "GTCRN 降噪器没建起来（降噪器没建起来），这句话判停用原始音频"
        );
        assert_eq!(
            text(
                "denoise",
                serde_json::json!({"stage": "判停", "applied": false, "model": "", "engine": ""})
            ),
            "没开降噪，这句话判停用原始音频"
        );
        assert_eq!(
            text(
                "kws_hit",
                serde_json::json!({"keyword": "退出", "when": "用户说话中"})
            ),
            "命令词模型听到退出命令（用户说话中）"
        );
        assert_eq!(
            text(
                "asr",
                serde_json::json!({"raw": "问一夏", "homophone": "问一下",
                "corrected": "退下", "kws_fused": true, "kws_keyword": "退出"})
            ),
            "识别结果：「问一夏」，同音纠正为「问一下」，命令词模型听到退出命令、改判为「退下」"
        );
        assert_eq!(
            text(
                "asr",
                serde_json::json!({"raw": "你好", "homophone": "你好",
                "corrected": "你好", "kws_fused": false})
            ),
            "识别结果：「你好」"
        );
        assert_eq!(
            text(
                "asr_ignored",
                serde_json::json!({"raw": "嗯", "reason": "只是语气词或噪声词"})
            ),
            "识别结果「嗯」只是语气词或噪声词，不回应"
        );
        assert_eq!(
            text(
                "smart_turn",
                serde_json::json!({"probability": 0.875, "elapsed_ms": 40})
            ),
            "语义判停：这句话已说完的概率 0.88（40 ms 算出）"
        );
        assert_eq!(
            text(
                "smart_turn",
                serde_json::json!({"reason": "识别完后再等 250 ms 仍没算完，超出时间预算已放弃"})
            ),
            "语义判停没用上：识别完后再等 250 ms 仍没算完，超出时间预算已放弃，只按句子本身判断"
        );
        assert_eq!(
            text(
                "hold_decision",
                serde_json::json!({"held": true, "text_wait_ms": 1300,
                "smart_turn_prob": 0.25, "extra_wait_ms": 900})
            ),
            "这句话像是还没说完（句尾规则要等 1300 ms，语义判停说完概率 0.25），开口前再等 900 ms，期间接着说会合并"
        );
        assert_eq!(
            text(
                "hold_decision",
                serde_json::json!({"held": false, "text_wait_ms": 250,
                "smart_turn_prob": null, "extra_wait_ms": 0})
            ),
            "判定这句话已说完（句尾规则要等 250 ms，没有语义判停结果），马上处理"
        );
        assert_eq!(
            text(
                "interim",
                serde_json::json!({"text": "帮我刻", "corrected": "帮我刻"})
            ),
            "边说边识别：「帮我刻」"
        );
        assert!(
            text(
                "follow_probe",
                serde_json::json!({"phase": "start", "head_ms": 420})
            )
            .starts_with("思考中听到一阵声音（420 ms）")
        );
        assert_eq!(
            text(
                "llm_request",
                serde_json::json!({"round": 2, "message_count": 5, "tool_count": 12})
            ),
            "向大模型发出第2轮请求（5 条消息，12 个工具）"
        );
        assert_eq!(
            text(
                "llm_round",
                serde_json::json!({"round": 1, "first_token_ms": 210,
                "elapsed_ms": 820, "reply_text": "好的。", "reply_tool_calls": []})
            ),
            "大模型第1轮回复完（首字 210 ms，完整 820 ms）：回复「好的。」"
        );
        assert_eq!(
            text(
                "tool_call",
                serde_json::json!({"name": "laser_safe_action_tool", "fast_route": true,
                "elapsed_ms": 15, "arguments": "{\"action\":\"status\"}", "result": "空闲"})
            ),
            "快路径直接调用工具 laser_safe_action_tool（激光机安全动作：状态、停止、急停等）执行完（15 ms）：\
             参数 {\"action\":\"status\"}，结果「空闲」"
        );
        assert_eq!(
            text(
                "tool_call",
                serde_json::json!({"name": "get_time", "description": "查询时间。返回本地时间",
                "elapsed_ms": 3, "arguments": "{}", "result": "12:00"})
            ),
            "工具 get_time（查询时间）执行完（3 ms）：参数 {}，结果「12:00」"
        );
        assert_eq!(
            text(
                "tts_start",
                serde_json::json!({"index": 2, "text": "马上好。", "fixed": false, "fallback": false})
            ),
            "开始合成第2句：「马上好。」"
        );
        assert_eq!(
            text(
                "tts_ready",
                serde_json::json!({"index": 2, "elapsed_ms": 180})
            ),
            "第2句合成出第一段音频（180 ms）"
        );
        assert_eq!(
            text(
                "tts_failed",
                serde_json::json!({"index": 1, "elapsed_ms": 30, "error": "401", "fallback": true})
            ),
            "第1句合成失败（30 ms）：401，改用本地合成"
        );
        assert_eq!(
            text(
                "jev",
                serde_json::json!({"applied": true, "tools_before": 9, "tools_after": 3,
                "fast_read": "Progress"})
            ),
            "Jev 前置筛选：保留 3 / 9 个工具，判断为只读快答（查加工进度）"
        );
        assert_eq!(turn_kind_label("Greeting"), "唤醒回应");
        // n110 复审 12①：停止快路径按来源分别说明
        assert_eq!(
            text(
                "fast_stop",
                serde_json::json!({"text": "停止", "source": "interim"})
            ),
            "边说边识别整句命中停止词「停止」，立即走停止快路径"
        );
        assert_eq!(
            text(
                "fast_stop",
                serde_json::json!({"text": "停", "source": "kws"})
            ),
            "思考中命令词模型听到“停”，不等识别直接走停止快路径"
        );
        assert_eq!(
            text(
                "fast_stop",
                serde_json::json!({"text": "停下", "source": "recheck"})
            ),
            "思考中那阵声音单独识别出停止说法「停下」，打断回答走停止快路径"
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
