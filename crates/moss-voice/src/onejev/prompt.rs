//! OneJev System One 协议渲染与答案计算（对齐 OmniJev/OneJev qev-labels-v2）。

use super::*;

pub(super) fn slot_labels() -> &'static Vec<String> {
    static SLOTS: OnceLock<Vec<String>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let mut out: Vec<String> = LETTERS.chars().map(|c| c.to_string()).collect();
        for a in LETTERS.chars() {
            for b in LETTERS.chars() {
                if out.len() >= 256 {
                    break;
                }
                let pair = format!("{a}{b}");
                if !NOT_ONE_TOKEN.contains(&pair.as_str()) {
                    out.push(pair);
                }
            }
        }
        out
    })
}

#[derive(Debug, Clone)]
enum ParsedQuestion {
    Noul {
        instructions: String,
        true_desc: String,
        false_desc: String,
    },
    Choice {
        instructions: String,
        options: Vec<(String, String)>,
    },
    Score {
        instructions: String,
        levels: Vec<(Value, String)>,
    },
}

#[derive(Debug, Clone)]
pub(super) struct RenderedBranch {
    pub(super) qid: String,
    pub(super) kind: &'static str,
    pub(super) labels: Vec<String>,
    pub(super) raw_levels: Vec<Value>,
    pub(super) suffix: String,
    pub(super) chunk_index: Option<usize>,
}

fn render_entry(val: Option<&Value>) -> String {
    match val {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn render_state(state: &Value) -> String {
    match state {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn parse_question(raw: &Value) -> Result<ParsedQuestion, String> {
    let obj = raw
        .as_object()
        .ok_or_else(|| "question 必须是 JSON 对象".to_owned())?;
    let qtype = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "question 缺少 type 字段".to_owned())?;
    let inst_raw = render_entry(obj.get("instructions"));
    match qtype {
        "noul" => {
            let instructions = if inst_raw.is_empty() {
                NOUL_DEFAULT_INSTRUCTIONS.to_owned()
            } else {
                inst_raw
            };
            let crit = obj.get("criteria").and_then(Value::as_object);
            let t = crit
                .and_then(|c| c.get("true"))
                .map(|v| render_entry(Some(v)))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| NOUL_DEFAULT_TRUE.to_owned());
            let f = crit
                .and_then(|c| c.get("false"))
                .map(|v| render_entry(Some(v)))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| NOUL_DEFAULT_FALSE.to_owned());
            Ok(ParsedQuestion::Noul {
                instructions,
                true_desc: t,
                false_desc: f,
            })
        }
        "choice" => {
            let instructions = if inst_raw.is_empty() {
                "Which option applies to the state?".to_owned()
            } else {
                inst_raw
            };
            let crit = obj
                .get("criteria")
                .and_then(Value::as_object)
                .ok_or_else(|| "choice 问题必须提供对象类型的 criteria".to_owned())?;
            if crit.is_empty() {
                return Err("choice 问题的 criteria 不能为空".into());
            }
            let options = crit
                .iter()
                .map(|(k, v)| (k.clone(), render_entry(Some(v))))
                .collect();
            Ok(ParsedQuestion::Choice {
                instructions,
                options,
            })
        }
        "score" => {
            let instructions = if inst_raw.is_empty() {
                "Which level describes the state?".to_owned()
            } else {
                inst_raw
            };
            let arr = obj
                .get("criteria")
                .and_then(Value::as_array)
                .ok_or_else(|| "score 问题必须提供数组类型的 criteria".to_owned())?;
            if arr.is_empty() {
                return Err("score 问题的 criteria 不能为空".into());
            }
            let levels = arr
                .iter()
                .map(|v| (v.clone(), render_entry(Some(v))))
                .collect();
            Ok(ParsedQuestion::Score {
                instructions,
                levels,
            })
        }
        other => Err(format!("不支持的 question 类型：{other}")),
    }
}

fn format_options_block(names: &[String], descs: &[String]) -> String {
    let slots = slot_labels();
    names
        .iter()
        .zip(descs.iter())
        .zip(slots.iter())
        .map(|((name, desc), slot)| {
            if desc.is_empty() {
                format!("{slot}. {name}")
            } else {
                format!("{slot}. {name}: {desc}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_suffix(
    kind: &'static str,
    instructions: &str,
    names: &[String],
    descs: &[String],
) -> String {
    let slots = slot_labels();
    let letters: Vec<&str> = slots[..names.len()].iter().map(String::as_str).collect();
    let header = if kind == "score" {
        "Rate the state:"
    } else {
        "Question:"
    };
    let answer_line = if letters.len() <= LETTERS.len() {
        format!("Answer with one letter: {}.", letters.join(", "))
    } else {
        format!("Answer with one label: {}.", letters.join(", "))
    };
    format!(
        "{header} {instructions}\n\nOptions:\n{}\n\n{answer_line}",
        format_options_block(names, descs)
    )
}

pub(super) fn render_branches(
    questions: &Map<String, Value>,
    max_slots: usize,
) -> Result<(Vec<RenderedBranch>, BTreeMap<String, Vec<String>>), String> {
    let mut branches = Vec::new();
    let mut choice_orders = BTreeMap::new();
    for (qid, raw_q) in questions {
        match parse_question(raw_q)? {
            ParsedQuestion::Noul {
                instructions,
                true_desc,
                false_desc,
            } => {
                let names = vec!["yes".to_owned(), "no".to_owned()];
                let descs = vec![true_desc, false_desc];
                let suffix = build_suffix("noul", &instructions, &names, &descs);
                branches.push(RenderedBranch {
                    qid: qid.clone(),
                    kind: "noul",
                    labels: names,
                    raw_levels: Vec::new(),
                    suffix,
                    chunk_index: None,
                });
            }
            ParsedQuestion::Choice {
                instructions,
                options,
            } => {
                let all_keys: Vec<String> = options.iter().map(|(k, _)| k.clone()).collect();
                choice_orders.insert(qid.clone(), all_keys);
                if options.len() <= max_slots {
                    let names: Vec<String> = options.iter().map(|(k, _)| k.clone()).collect();
                    let descs: Vec<String> = options.iter().map(|(_, d)| d.clone()).collect();
                    let suffix = build_suffix("choice", &instructions, &names, &descs);
                    branches.push(RenderedBranch {
                        qid: qid.clone(),
                        kind: "choice",
                        labels: names,
                        raw_levels: Vec::new(),
                        suffix,
                        chunk_index: None,
                    });
                } else {
                    let cap = max_slots.saturating_sub(1).max(1);
                    let n_chunks = options.len().div_ceil(cap);
                    let chunk_size = options.len().div_ceil(n_chunks);
                    for (ci, chunk) in options.chunks(chunk_size).enumerate() {
                        let mut names: Vec<String> = chunk.iter().map(|(k, _)| k.clone()).collect();
                        let mut descs: Vec<String> = chunk.iter().map(|(_, d)| d.clone()).collect();
                        names.push(NONE_OPTION.to_owned());
                        descs.push(NONE_DESCRIPTION.to_owned());
                        let suffix = build_suffix("choice", &instructions, &names, &descs);
                        branches.push(RenderedBranch {
                            qid: qid.clone(),
                            kind: "choice",
                            labels: names,
                            raw_levels: Vec::new(),
                            suffix,
                            chunk_index: Some(ci),
                        });
                    }
                }
            }
            ParsedQuestion::Score {
                instructions,
                levels,
            } => {
                if levels.len() > max_slots {
                    return Err(format!(
                        "score 等级数 {} 超过上限 {max_slots}",
                        levels.len()
                    ));
                }
                let names: Vec<String> = (0..levels.len()).map(|i| format!("level {i}")).collect();
                let descs: Vec<String> = levels.iter().map(|(_, d)| d.clone()).collect();
                let raw_levels: Vec<Value> = levels.into_iter().map(|(v, _)| v).collect();
                let labels: Vec<String> = (0..names.len()).map(|i| i.to_string()).collect();
                let suffix = build_suffix("score", &instructions, &names, &descs);
                branches.push(RenderedBranch {
                    qid: qid.clone(),
                    kind: "score",
                    labels,
                    raw_levels,
                    suffix,
                    chunk_index: None,
                });
            }
        }
    }
    Ok((branches, choice_orders))
}

fn render_chatml_prompt(user_content: &str) -> String {
    format!(
        "<|im_start|>system\n{SYSTEM_PROMPT}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n",
        user_content.trim()
    )
}

pub(super) fn render_full_prompt(
    state: &Value,
    suffix: &str,
    media_marker: Option<&str>,
    media_len: usize,
) -> Result<String, String> {
    let state_str = render_state(state);
    let raw_user = format!("<state>\n{state_str}\n</state>\n\n{suffix}");
    if media_len == 0 {
        return Ok(render_chatml_prompt(&raw_user));
    }
    let marker = media_marker.unwrap_or("<__media__>");
    let mut replaced = String::with_capacity(raw_user.len() + media_len * marker.len());
    let mut seen = Vec::new();
    let mut rest = raw_user.as_str();
    while let Some(idx) = rest.find("<image:") {
        replaced.push_str(&rest[..idx]);
        let after = &rest[idx + "<image:".len()..];
        let Some(end) = after.find('>') else {
            return Err("未闭合的 <image:N> 占位符".into());
        };
        let n: usize = after[..end]
            .parse()
            .map_err(|_| format!("无效的图片占位符序号：{}", &after[..end]))?;
        if n < 1 || n > media_len {
            return Err(format!("<image:{n}> 超出 media 列表长度 {media_len}"));
        }
        seen.push(n);
        replaced.push_str(marker);
        rest = &after[end + 1..];
    }
    replaced.push_str(rest);
    let expected: Vec<usize> = (1..=media_len).collect();
    if seen != expected {
        return Err(format!(
            "media 图片须在 state 中按顺序由 <image:1>..<image:{media_len}> 各引用一次"
        ));
    }
    Ok(render_chatml_prompt(&replaced))
}

pub(super) fn softmax(logits: &[f64]) -> Vec<f64> {
    if logits.is_empty() {
        return Vec::new();
    }
    let max_z = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps: Vec<f64> = logits.iter().map(|&x| (x - max_z).exp()).collect();
    let sum: f64 = exps.iter().sum();
    if sum <= 0.0 || !sum.is_finite() {
        return vec![1.0 / (logits.len() as f64); logits.len()];
    }
    exps.into_iter().map(|w| w / sum).collect()
}

fn normalize_probs(probs: &[f64]) -> Vec<f64> {
    let sum: f64 = probs.iter().sum();
    if sum <= 0.0 || !sum.is_finite() {
        return vec![1.0 / (probs.len() as f64); probs.len()];
    }
    probs.iter().map(|&p| p / sum).collect()
}

fn round6(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

fn choice_confidence(probs: &[f64]) -> f64 {
    if probs.len() <= 1 {
        return 1.0;
    }
    let p = normalize_probs(probs);
    let uniform = 1.0 / (p.len() as f64);
    let peak = p.iter().copied().fold(0.0_f64, f64::max);
    ((peak - uniform) / (1.0 - uniform)).clamp(0.0, 1.0)
}

fn score_confidence(probs: &[f64]) -> f64 {
    if probs.len() <= 1 {
        return 1.0;
    }
    let p = normalize_probs(probs);
    let k = p.len();
    let mut mode = 0usize;
    for i in 1..k {
        if p[i] > p[mode] {
            mode = i;
        }
    }
    let distance: f64 = p
        .iter()
        .enumerate()
        .map(|(i, &pi)| pi * ((i as f64) - (mode as f64)).abs())
        .sum();
    let center = ((k - 1) as f64) / 2.0;
    let uniform_mad: f64 = (0..k).map(|i| ((i as f64) - center).abs()).sum::<f64>() / (k as f64);
    if uniform_mad <= 0.0 {
        1.0
    } else {
        (1.0 - distance / uniform_mad).clamp(0.0, 1.0)
    }
}

pub(super) fn make_noul_answer(p_yes: f64) -> Value {
    let p = round6(p_yes.clamp(0.0, 1.0));
    let conf = round6((2.0 * (p - 0.5).abs()).clamp(0.0, 1.0));
    json!({
        "type": "noul",
        "noul": p,
        "confidence": conf,
        "x_jev_confidence": conf,
    })
}

pub(super) fn make_choice_answer(options: &[String], probs: &[f64]) -> Value {
    let p = normalize_probs(probs);
    let mut best = 0usize;
    for i in 1..p.len() {
        if p[i] > p[best] {
            best = i;
        }
    }
    let mut prob_map = Map::new();
    for (opt, &pi) in options.iter().zip(p.iter()) {
        prob_map.insert(opt.clone(), json!(round6(pi)));
    }
    let conf = round6(choice_confidence(&p));
    json!({
        "type": "choice",
        "choice": options[best],
        "probabilities": prob_map,
        "confidence": conf,
        "x_jev_confidence": conf,
    })
}

pub(super) fn make_score_answer(levels: &[Value], probs: &[f64]) -> Value {
    let p = normalize_probs(probs);
    let expected: f64 = p.iter().enumerate().map(|(i, &pi)| (i as f64) * pi).sum();
    let mut legend = Map::new();
    let mut prob_map = Map::new();
    for (i, (lvl, &pi)) in levels.iter().zip(p.iter()).enumerate() {
        let key = i.to_string();
        legend.insert(key.clone(), lvl.clone());
        prob_map.insert(key, json!(round6(pi)));
    }
    let conf = round6(score_confidence(&p));
    json!({
        "type": "score",
        "score": round6(expected),
        "legend": legend,
        "probabilities": prob_map,
        "confidence": conf,
        "x_jev_confidence": conf,
    })
}

pub(super) fn parse_inline_media(
    media_val: Option<&Value>,
    vision_supported: bool,
) -> Result<Vec<String>, String> {
    let Some(val) = media_val else {
        return Ok(Vec::new());
    };
    if val.is_null() {
        return Ok(Vec::new());
    }
    let items = val
        .as_array()
        .ok_or_else(|| "media 必须是数组".to_owned())?;
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if !vision_supported {
        return Err("当前加载的模型未启用视觉投影器（mmproj），不支持输入图片".into());
    }
    let mut images = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| format!("media[{idx}] 必须是对象"))?;
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
        if kind != "image" {
            return Err(format!(
                "本地 OneJev 网关仅支持 image 类型 media（收到 {kind:?}）"
            ));
        }
        let raw_data = obj.get("data").and_then(Value::as_str).ok_or_else(|| {
            format!("media[{idx}] 须提供内联 base64 data 字段（禁止外发未知 URL）")
        })?;
        let b64 = match raw_data.split_once(',') {
            Some((head, rest)) if head.starts_with("data:") => rest.trim(),
            _ => raw_data.trim(),
        };
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| format!("media[{idx}] 的 base64 图片数据无效"))?;
        if decoded.is_empty() {
            return Err(format!("media[{idx}] 图片数据为空"));
        }
        images.push(base64::engine::general_purpose::STANDARD.encode(decoded));
    }
    Ok(images)
}
