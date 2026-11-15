//! 对接本地 llama-server 的上游客户端：共享前缀 KV 预填、槽位对数概率读取与多模态输入。

use super::*;

pub(super) struct UpstreamClient {
    base_url: String,
    http: reqwest::blocking::Client,
    slot_ids: Vec<i64>,
    vision: bool,
    media_marker: String,
    pub(super) lock: Mutex<()>,
}

impl UpstreamClient {
    pub(super) fn connect(base_url: String) -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("创建本地推理客户端失败：{e}"))?;

        let props: Value = http
            .get(format!("{base_url}/props"))
            .send()
            .map_err(|e| format!("读取 llama-server 属性失败：{e}"))?
            .json()
            .unwrap_or_else(|_| json!({}));
        let vision = props
            .pointer("/modalities/vision")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let media_marker = props
            .get("media_marker")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("<__media__>")
            .to_owned();

        let mut client = Self {
            base_url,
            http,
            slot_ids: Vec::new(),
            vision,
            media_marker,
            lock: Mutex::new(()),
        };
        client.slot_ids = client.verify_slots()?;
        Ok(client)
    }

    fn tokenize(&self, content: &str, parse_special: bool) -> Result<Vec<i64>, String> {
        let resp = self
            .http
            .post(format!("{}/tokenize", self.base_url))
            .json(&json!({
                "content": content,
                "add_special": false,
                "parse_special": parse_special,
            }))
            .send()
            .map_err(|e| format!("llama-server 分词请求失败：{e}"))?;
        if !resp.status().is_success() {
            return Err(format!(
                "llama-server /tokenize 返回 HTTP {}",
                resp.status().as_u16()
            ));
        }
        let body: Value = resp.json().map_err(|e| format!("解析分词结果失败：{e}"))?;
        let arr = body
            .get("tokens")
            .and_then(Value::as_array)
            .ok_or_else(|| "分词响应缺少 tokens 数组".to_owned())?;
        arr.iter()
            .map(|v| v.as_i64().ok_or_else(|| "token id 非整数".to_owned()))
            .collect()
    }

    fn verify_slots(&self) -> Result<Vec<i64>, String> {
        let labels = slot_labels();
        let mut ids = Vec::with_capacity(labels.len());
        for label in &labels[..LETTERS.len()] {
            let toks = self.tokenize(label, false)?;
            if toks.len() != 1 {
                return Err(format!(
                    "基础选项槽位 {label} 不是单 token（长度 {}）",
                    toks.len()
                ));
            }
            ids.push(toks[0]);
        }
        let mut extra = Vec::new();
        let mut all_extra_single = true;
        for label in &labels[LETTERS.len()..] {
            let toks = self.tokenize(label, false)?;
            if toks.len() == 1 {
                extra.push(toks[0]);
            } else {
                all_extra_single = false;
                break;
            }
        }
        if all_extra_single {
            ids.extend(extra);
        }
        let uniq: HashSet<i64> = ids.iter().copied().collect();
        if uniq.len() != ids.len() {
            return Err("选项槽位 token id 发生冲突".into());
        }
        Ok(ids)
    }

    fn post_completion(&self, body: &Value) -> Result<Vec<Value>, String> {
        let resp = self
            .http
            .post(format!("{}/completion", self.base_url))
            .json(body)
            .send()
            .map_err(|e| format!("llama-server 推理失败：{e}"))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().unwrap_or_default();
            return Err(format!(
                "llama-server /completion 返回 HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            ));
        }
        let parsed: Value = resp.json().map_err(|e| format!("解析推理输出失败：{e}"))?;
        let mut items = match parsed {
            Value::Array(arr) => arr,
            other => vec![other],
        };
        items.sort_by_key(|item| item.get("index").and_then(Value::as_u64).unwrap_or(0));
        Ok(items)
    }

    pub(super) fn evaluate(
        &self,
        model_name: &str,
        state: &Value,
        questions: &Map<String, Value>,
        media: Option<&Value>,
        abandoned: &dyn Fn() -> bool,
    ) -> Result<(u16, Value), String> {
        let t0 = Instant::now();
        let images = match parse_inline_media(media, self.vision) {
            Ok(imgs) => imgs,
            Err(msg) => return Ok((422, json!({ "detail": msg }))),
        };
        let (branches, choice_orders) = match render_branches(questions, self.slot_ids.len()) {
            Ok(pair) => pair,
            Err(msg) => return Ok((422, json!({ "detail": msg }))),
        };
        if branches.is_empty() {
            return Ok((422, json!({ "detail": "questions 不能为空" })));
        }

        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        // 2026-11-07：网关单槽串行推理。排队期间调用方已放弃（预热被新的中间结果取代、超时）的请求直接丢弃，
        // 不再占用推理，避免正式轮次排在一串作废请求之后超时并触发熔断。
        if abandoned() {
            return Ok((
                CLIENT_GONE_STATUS,
                json!({ "detail": "client disconnected" }),
            ));
        }
        let (prompts, prefix_prompt, total_tokens) = if images.is_empty() {
            let mut full_ids = Vec::with_capacity(branches.len());
            for br in &branches {
                let text = match render_full_prompt(state, &br.suffix, None, 0) {
                    Ok(t) => t,
                    Err(msg) => return Ok((422, json!({ "detail": msg }))),
                };
                full_ids.push(self.tokenize(&text, true)?);
            }
            let marker_text = render_full_prompt(state, "QEV_PREFIX_MARK", None, 0)?;
            let cut = marker_text.find("QEV_PREFIX_MARK").unwrap_or(0);
            let mut prefix_ids = self.tokenize(&marker_text[..cut], true)?;
            while !prefix_ids.is_empty()
                && !full_ids.iter().all(|ids| {
                    ids.len() > prefix_ids.len() && ids[..prefix_ids.len()] == prefix_ids[..]
                })
            {
                prefix_ids.pop();
            }
            let suffix_sum: usize = full_ids
                .iter()
                .map(|ids| ids.len() - prefix_ids.len())
                .sum();
            let longest = full_ids.iter().map(Vec::len).max().unwrap_or(0);
            let total = prefix_ids.len() + suffix_sum;
            if longest > DEFAULT_CTX_TOKENS || total > MAX_REQUEST_TOKENS {
                return Ok((
                    422,
                    json!({ "detail": format!("请求长度超限（最长分支 {longest} tokens，总计 {total} tokens）") }),
                ));
            }
            let prompt_vals: Vec<Value> = full_ids.into_iter().map(|ids| json!(ids)).collect();
            (prompt_vals, json!(prefix_ids), total)
        } else {
            let marker_prompt = match render_full_prompt(
                state,
                "QEV_PREFIX_MARK",
                Some(&self.media_marker),
                images.len(),
            ) {
                Ok(t) => t,
                Err(msg) => return Ok((422, json!({ "detail": msg }))),
            };
            let Some((head, tail)) = marker_prompt.split_once("QEV_PREFIX_MARK") else {
                return Err("内部前缀切分标记丢失".into());
            };
            let prefix_obj = json!({
                "prompt_string": head,
                "multimodal_data": images,
            });
            let prompt_vals: Vec<Value> = branches
                .iter()
                .map(|br| {
                    json!({
                        "prompt_string": format!("{head}{}{tail}", br.suffix),
                        "multimodal_data": images,
                    })
                })
                .collect();
            (prompt_vals, prefix_obj, branches.len() * 256)
        };

        if abandoned() {
            return Ok((
                CLIENT_GONE_STATUS,
                json!({ "detail": "client disconnected" }),
            ));
        }
        let mut pin_slot: Option<i64> = None;
        if prompts.len() > 1 {
            let prefill = self.post_completion(&json!({
                "prompt": [prefix_prompt],
                "n_predict": 0,
                "cache_prompt": true,
            }))?;
            pin_slot = prefill
                .first()
                .and_then(|v| v.get("id_slot"))
                .and_then(Value::as_i64);
        }

        let max_slots_needed = branches.iter().map(|b| b.labels.len()).max().unwrap_or(2);
        let n_top = 64usize.max(2 * max_slots_needed);
        let mut batch_req = json!({
            "prompt": prompts,
            "n_predict": 1,
            "cache_prompt": true,
            "temperature": 0.0,
            "n_probs": n_top,
        });
        if let Some(slot) = pin_slot {
            batch_req["id_slot"] = json!(slot);
        }
        let batch_out = self.post_completion(&batch_req)?;
        if batch_out.len() != branches.len() {
            return Err(format!(
                "llama-server 返回分支数不匹配（{} != {}）",
                batch_out.len(),
                branches.len()
            ));
        }

        let mut branch_probs: Vec<Vec<f64>> = Vec::with_capacity(branches.len());
        for (idx, (br, item)) in branches.iter().zip(batch_out.iter()).enumerate() {
            let slots = &self.slot_ids[..br.labels.len()];
            let top_arr = item
                .pointer("/completion_probabilities/0/top_logprobs")
                .and_then(Value::as_array);
            let mut logprob_by_id = BTreeMap::new();
            if let Some(arr) = top_arr {
                for entry in arr {
                    if let (Some(id), Some(lp)) = (
                        entry.get("id").and_then(Value::as_i64),
                        entry.get("logprob").and_then(Value::as_f64),
                    ) {
                        logprob_by_id.insert(id, lp);
                    }
                }
            }
            let logits = if slots.iter().all(|s| logprob_by_id.contains_key(s)) {
                slots.iter().map(|s| logprob_by_id[s]).collect::<Vec<f64>>()
            } else {
                let bias: Vec<Value> = slots.iter().map(|&s| json!([s, 100.0])).collect();
                let mut fb_req = json!({
                    "prompt": [prompts[idx].clone()],
                    "n_predict": 1,
                    "cache_prompt": true,
                    "n_probs": slots.len(),
                    "post_sampling_probs": true,
                    "samplers": ["temperature"],
                    "temperature": 1.0,
                    "logit_bias": bias,
                });
                if let Some(slot) = pin_slot {
                    fb_req["id_slot"] = json!(slot);
                }
                let fb = self.post_completion(&fb_req)?;
                let mut prob_by_id = BTreeMap::new();
                if let Some(arr) = fb
                    .first()
                    .and_then(|v| v.pointer("/completion_probabilities/0/top_probs"))
                    .and_then(Value::as_array)
                {
                    for entry in arr {
                        if let (Some(id), Some(p)) = (
                            entry.get("id").and_then(Value::as_i64),
                            entry.get("prob").and_then(Value::as_f64),
                        ) {
                            prob_by_id.insert(id, p);
                        }
                    }
                }
                slots
                    .iter()
                    .map(|s| prob_by_id.get(s).copied().unwrap_or(0.0).max(1e-30).ln())
                    .collect::<Vec<f64>>()
            };
            branch_probs.push(softmax(&logits));
        }

        let mut answers = Map::new();
        let mut chunk_accum: BTreeMap<String, (BTreeMap<String, (f64, f64)>, Vec<f64>)> =
            BTreeMap::new();

        for (br, probs) in branches.iter().zip(branch_probs.iter()) {
            if br.chunk_index.is_some() {
                let entry = chunk_accum
                    .entry(br.qid.clone())
                    .or_insert_with(|| (BTreeMap::new(), Vec::new()));
                let none_idx = br
                    .labels
                    .iter()
                    .position(|l| l == NONE_OPTION)
                    .unwrap_or(probs.len() - 1);
                let keep = (1.0 - probs[none_idx]).max(1e-9);
                for (lab, &p) in br.labels.iter().zip(probs.iter()) {
                    if lab != NONE_OPTION {
                        entry.0.insert(lab.clone(), (p / keep, keep));
                    }
                }
                entry.1.push(keep);
                continue;
            }
            let ans = match br.kind {
                "noul" => make_noul_answer(probs[0]),
                "choice" => make_choice_answer(&br.labels, probs),
                _ => make_score_answer(&br.raw_levels, probs),
            };
            answers.insert(br.qid.clone(), ans);
        }

        for (qid, (scores, weights)) in chunk_accum {
            let total_w: f64 = weights.iter().sum::<f64>().max(1e-12);
            let names = choice_orders.get(&qid).cloned().unwrap_or_default();
            let probs: Vec<f64> = names
                .iter()
                .map(|name| {
                    scores
                        .get(name)
                        .map(|&(cond, keep)| cond * (keep / total_w))
                        .unwrap_or(0.0)
                })
                .collect();
            answers.insert(qid, make_choice_answer(&names, &probs));
        }

        let elapsed_s = t0.elapsed().as_secs_f64();
        Ok((
            200,
            json!({
                "id": format!("onejev-{}", t0.elapsed().as_nanos()),
                "model": model_name,
                "answers": answers,
                "usage": {
                    "input_tokens": total_tokens,
                    "output_tokens": questions.len(),
                },
                "qev": {
                    "prompt_version": PROMPT_VERSION,
                    "fork_mode": "llama.cpp",
                    "branches": branches.len(),
                    "timing": { "total_s": elapsed_s },
                }
            }),
        ))
    }
}
