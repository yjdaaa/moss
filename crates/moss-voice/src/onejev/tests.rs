//! OneJev 运行时、协议渲染、解压与网关的单元测试。

use super::*;

#[test]
fn slot_labels_match_onejev_256_single_token_table() {
    let slots = slot_labels();
    assert_eq!(slots.len(), 256);
    assert_eq!(&slots[..3], &["A", "B", "C"]);
    assert_eq!(slots[25], "Z");
    assert_eq!(slots[26], "AA");
    assert!(!slots.iter().any(|s| s == "BQ" || s == "CQ" || s == "JQ"));
}

#[test]
fn prompt_rendering_and_confidence_match_onejev_reference() {
    let state = json!({"utterance": "检查激光雕刻机连接状态"});
    let mut questions = Map::new();
    questions.insert(
        "probe".into(),
        json!({
            "type": "noul",
            "instructions": "Does state.utterance ask to check connection status?"
        }),
    );
    let (branches, _) = render_branches(&questions, 256).unwrap();
    assert_eq!(branches.len(), 1);
    let full = render_full_prompt(&state, &branches[0].suffix, None, 0).unwrap();
    assert!(full.starts_with("<|im_start|>system\nApply the question to the state."));
    assert!(full.contains("<state>\n{\n  \"utterance\": \"检查激光雕刻机连接状态\"\n}\n</state>"));
    assert!(full.contains("Question: Does state.utterance ask to check connection status?\n\nOptions:\nA. yes: the statement is true / the answer is yes\nB. no: the statement is false / the answer is no\n\nAnswer with one letter: A, B."));
    assert!(full.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));

    let choice = make_choice_answer(&["a".into(), "b".into(), "c".into()], &[0.85, 0.10, 0.05]);
    assert_eq!(choice["choice"], "a");
    assert!((choice["confidence"].as_f64().unwrap() - 0.775).abs() < 1e-6);

    let score = make_score_answer(
        &[json!("bad"), json!("ok"), json!("great")],
        &[0.0, 0.25, 0.75],
    );
    assert!((score["score"].as_f64().unwrap() - 1.75).abs() < 1e-6);
    assert!(score["confidence"].as_f64().unwrap() > 0.6);
}

#[test]
fn parse_list_devices_distinguishes_metal_cuda_and_vulkan() {
    let mac_out = "Available devices:\n  MTL0: Apple M6 (12124 MiB, 12123 MiB free)\n  BLAS: Accelerate (0 MiB, 0 MiB free)\n";
    let det_mac = parse_list_devices_output(mac_out).unwrap();
    assert_eq!(det_mac.kind, AcceleratorKind::Metal);
    assert_eq!(det_mac.device_detail.as_deref(), Some("Apple M6"));
    // Intel Mac（x86_64）即使输出含 MTL0 也固定回退 CPU（-ngl 0）
    let det_intel_mac = parse_list_devices_output_for_arch(mac_out, false).unwrap();
    assert_eq!(det_intel_mac.kind, AcceleratorKind::Cpu);
    assert!(det_intel_mac.device_detail.is_none());

    let win_cuda =
        "Available devices:\n  CUDA0: NVIDIA GeForce RTX 4060 (8188 MiB, 7100 MiB free)\n";
    let det_cuda = parse_list_devices_output(win_cuda).unwrap();
    assert_eq!(det_cuda.kind, AcceleratorKind::Cuda);
    assert_eq!(
        det_cuda.device_detail.as_deref(),
        Some("NVIDIA GeForce RTX 4060")
    );

    let win_vk =
        "Available devices:\n  Vulkan0: AMD Radeon RX 7800 XT (16384 MiB, 15000 MiB free)\n";
    let det_vk = parse_list_devices_output(win_vk).unwrap();
    assert_eq!(det_vk.kind, AcceleratorKind::Vulkan);
    assert_eq!(
        det_vk.device_detail.as_deref(),
        Some("AMD Radeon RX 7800 XT")
    );
}

fn make_test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut cd = Vec::new();
    for &(name, content) in entries {
        let local_offset = buf.len() as u32;
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(content).unwrap();
        let comp = enc.finish().unwrap();
        let name_bytes = name.as_bytes();

        buf.extend_from_slice(b"PK\x03\x04");
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&8u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(content.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(name_bytes);
        buf.extend_from_slice(&comp);

        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&8u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(content.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&local_offset.to_le_bytes());
        cd.extend_from_slice(name_bytes);
    }
    let cd_offset = buf.len() as u32;
    let cd_size = cd.len() as u32;
    buf.extend_from_slice(&cd);
    buf.extend_from_slice(b"PK\x05\x06");
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    buf.extend_from_slice(&cd_size.to_le_bytes());
    buf.extend_from_slice(&cd_offset.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf
}

/// 2026-11-07：多档 OneJev 同时下载时，后来的排队等推理引擎并报告“等待”，等待中可取消。
#[test]
fn runtime_install_wait_reports_waiting_and_is_cancellable() {
    let held = RUNTIME_INSTALL_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let cancel = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let waiter = {
        let cancel = cancel.clone();
        let seen = seen.clone();
        std::thread::spawn(move || {
            lock_runtime_install(&cancel, &|progress| seen.lock().unwrap().push(progress)).map(drop)
        })
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "未报告等待");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(seen.lock().unwrap().as_slice(), &[RuntimeProgress::Waiting]);
    cancel.store(true, Ordering::SeqCst);
    assert_eq!(waiter.join().unwrap(), Err("已取消".to_owned()));
    drop(held);
    assert!(
        lock_runtime_install(&AtomicBool::new(false), &|_| {}).is_ok(),
        "释放后立即可拿到"
    );
}

#[test]
fn zip_extractor_unpacks_deflate_and_blocks_path_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let ok_zip = make_test_zip(&[
        ("llama-server.exe", b"MZ-test-binary"),
        ("sub/mtmd.dll", b"dll-bytes"),
    ]);
    extract_zip_bytes(&ok_zip, tmp.path(), &cancel).unwrap();
    assert_eq!(
        fs::read(tmp.path().join("llama-server.exe")).unwrap(),
        b"MZ-test-binary"
    );
    assert_eq!(
        fs::read(tmp.path().join("sub").join("mtmd.dll")).unwrap(),
        b"dll-bytes"
    );

    let evil_zip = make_test_zip(&[("../escape.exe", b"bad")]);
    let out_dir = tmp.path().join("evil");
    fs::create_dir_all(&out_dir).unwrap();
    assert!(extract_zip_bytes(&evil_zip, &out_dir, &cancel).is_err());
    assert!(!tmp.path().join("escape.exe").exists());
}

struct MockLlama {
    port: u16,
    stop: Arc<AtomicBool>,
    completions: Arc<AtomicUsize>,
    thread: std::thread::JoinHandle<()>,
}

impl MockLlama {
    fn shutdown(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        let _ = self.thread.join();
    }
}

fn spawn_mock_llama() -> MockLlama {
    let mock_llama = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mock_port = mock_llama.local_addr().unwrap().port();
    let stop_mock = Arc::new(AtomicBool::new(false));
    let stop_mock_t = stop_mock.clone();
    let completions = Arc::new(AtomicUsize::new(0));
    let completions_t = completions.clone();

    let mock_thread = std::thread::spawn(move || {
        while !stop_mock_t.load(Ordering::SeqCst) {
            let Ok((mut s, _)) = mock_llama.accept() else {
                break;
            };
            if stop_mock_t.load(Ordering::SeqCst) {
                break;
            }
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut req_line = String::new();
            if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                continue;
            }
            let path = req_line.split_whitespace().nth(1).unwrap_or("");
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':')
                    && k.trim().eq_ignore_ascii_case("content-length")
                {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            if len > 0 {
                let _ = reader.read_exact(&mut body);
            }
            if path == "/props" {
                write_json_response(
                    &mut s,
                    200,
                    &json!({"modalities": {"vision": true}, "media_marker": "<__media__>"}),
                );
            } else if path == "/tokenize" {
                let v: Value = serde_json::from_slice(&body).unwrap();
                let content = v["content"].as_str().unwrap_or("");
                let slots = slot_labels();
                let tokens: Vec<i64> = if let Some(idx) = slots.iter().position(|l| l == content) {
                    vec![1000 + idx as i64]
                } else {
                    content.bytes().map(|b| b as i64).collect()
                };
                write_json_response(&mut s, 200, &json!({ "tokens": tokens }));
            } else if path == "/completion" {
                completions_t.fetch_add(1, Ordering::SeqCst);
                let v: Value = serde_json::from_slice(&body).unwrap();
                let n_predict = v["n_predict"].as_i64().unwrap_or(1);
                let prompts = v["prompt"].as_array().unwrap();
                if n_predict == 0 {
                    write_json_response(&mut s, 200, &json!([{"index": 0, "id_slot": 0}]));
                } else {
                    let out: Vec<Value> = (0..prompts.len())
                        .map(|idx| {
                            json!({
                                "index": idx,
                                "id_slot": 0,
                                "completion_probabilities": [{
                                    "top_logprobs": [
                                        {"id": 1000, "logprob": 3.0},
                                        {"id": 1001, "logprob": 0.5},
                                        {"id": 1002, "logprob": -1.0}
                                    ]
                                }]
                            })
                        })
                        .collect();
                    write_json_response(&mut s, 200, &Value::Array(out));
                }
            }
        }
    });

    MockLlama {
        port: mock_port,
        stop: stop_mock,
        completions,
        thread: mock_thread,
    }
}

fn spawn_test_gateway(upstream: Arc<UpstreamClient>) -> (OneJevGateway, SocketAddr) {
    let gw_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let gw_addr = gw_listener.local_addr().unwrap();
    let gw = OneJevGateway::spawn_with_upstream(
        "onejev_1_6_0_8b".into(),
        DetectedAccelerator {
            kind: AcceleratorKind::Metal,
            device_detail: Some("Apple M6".into()),
        },
        gw_listener,
        gw_addr,
        upstream,
        Arc::new(Mutex::new(None)),
        None,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    (gw, gw_addr)
}

#[test]
fn gateway_round_trips_systemone_over_mock_llama_server() {
    let mock = spawn_mock_llama();
    let mock_port = mock.port;
    let upstream =
        Arc::new(UpstreamClient::connect(format!("http://127.0.0.1:{mock_port}")).unwrap());
    let (mut gw, gw_addr) = spawn_test_gateway(upstream);

    let client = reqwest::blocking::Client::new();
    let health: Value = client
        .get(format!("http://{gw_addr}/health"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["engine"], "onejev");
    assert_eq!(health["model"], "onejev_1_6_0_8b");

    // 跨站 Origin、Sec-Fetch-Site: cross-site 与非回环 Host（DNS 重绑定）一律返回 403
    let cross_origin = client
        .post(format!("http://{gw_addr}/v1/systemone"))
        .header("Origin", "https://evil.example")
        .json(&json!({ "state": {}, "questions": { "q": { "type": "noul" } } }))
        .send()
        .unwrap();
    assert_eq!(cross_origin.status().as_u16(), 403);

    let cross_site = client
        .get(format!("http://{gw_addr}/health"))
        .header("Sec-Fetch-Site", "cross-site")
        .send()
        .unwrap();
    assert_eq!(cross_site.status().as_u16(), 403);

    let mut rebind_stream = TcpStream::connect(gw_addr).unwrap();
    rebind_stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: evil.example:18090\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut rebind_resp = String::new();
    rebind_stream.read_to_string(&mut rebind_resp).unwrap();
    assert!(
        rebind_resp.starts_with("HTTP/1.1 403"),
        "got: {rebind_resp}"
    );

    let resp: Value = client
        .post(format!("http://{gw_addr}/v1/systemone"))
        .json(&json!({
            "model": "jev-latest",
            "state": { "utterance": "查看雕刻机状态" },
            "questions": {
                "unfinished": { "type": "noul", "instructions": "unfinished?" },
                "route": {
                    "type": "choice",
                    "instructions": "pick route",
                    "criteria": { "status": "check status", "none": "other" }
                }
            }
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(resp["answers"]["unfinished"]["type"], "noul");
    assert!(resp["answers"]["unfinished"]["noul"].as_f64().unwrap() > 0.9);
    assert_eq!(resp["answers"]["route"]["type"], "choice");
    assert!(resp["answers"]["route"]["confidence"].as_f64().unwrap() > 0.8);

    let not_found = client
        .post(format!("http://{gw_addr}/v1/systemone"))
        .json(&json!({
            "model": "multilingual",
            "state": {},
            "questions": { "q": { "type": "noul" } }
        }))
        .send()
        .unwrap();
    assert_eq!(not_found.status().as_u16(), 404);

    gw.stop();
    mock.shutdown();
}

#[test]
fn gateway_drops_requests_abandoned_while_queued_behind_the_single_slot() {
    let mock = spawn_mock_llama();
    let upstream =
        Arc::new(UpstreamClient::connect(format!("http://127.0.0.1:{}", mock.port)).unwrap());
    let (mut gw, gw_addr) = spawn_test_gateway(upstream.clone());
    let body = serde_json::to_vec(&json!({
        "model": "jev-latest",
        "state": { "utterance": "帮我看下" },
        "questions": { "unfinished": { "type": "noul", "instructions": "unfinished?" } }
    }))
    .unwrap();
    let request = |close: bool| {
        let mut stream = TcpStream::connect(gw_addr).unwrap();
        let head = format!(
            "POST /v1/systemone HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            gw_addr.port(),
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        if close {
            drop(stream);
            None
        } else {
            Some(stream)
        }
    };

    // 模拟推理正忙：占住单槽锁，期间到达的请求排队；调用方等不及先断开
    let busy = upstream.lock.lock().unwrap();
    request(true);
    request(true);
    std::thread::sleep(Duration::from_millis(300));
    drop(busy);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        mock.completions.load(Ordering::SeqCst),
        0,
        "已放弃的请求不应再推理"
    );

    // 仍在等待的请求照常推理并返回结果
    let mut alive = request(false).unwrap();
    let mut resp = String::new();
    alive.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200"), "got: {resp}");
    assert!(resp.contains("\"unfinished\""), "got: {resp}");
    assert_eq!(mock.completions.load(Ordering::SeqCst), 1);

    gw.stop();
    mock.shutdown();
}

#[test]
fn local_jev_budget_keeps_hold_inside_listen_window_and_flags_slow_cpu() {
    for id in ["onejev_1_6_0_8b", "onejev_1_6_4b", "onejev_1_6_9b"] {
        for gpu in [true, false] {
            let budget = local_jev_budget(id, gpu).unwrap();
            assert!(
                budget.hold_ms.is_none_or(|ms| ms <= 800),
                "{id} gpu={gpu}: {budget:?}"
            );
            assert!(
                budget.turn_ms >= 1500 && budget.turn_ms <= 3500,
                "{id} gpu={gpu}: {budget:?}"
            );
        }
    }
    assert_eq!(
        local_jev_budget("onejev_1_6_0_8b", true).unwrap().hold_ms,
        Some(600)
    );
    assert_eq!(
        local_jev_budget("onejev_1_6_9b", true).unwrap().hold_ms,
        None
    );
    assert_eq!(
        local_jev_budget("onejev_1_6_9b", true).unwrap().notice,
        Some(NOTICE_9B_NO_HOLD)
    );
    assert_eq!(
        local_jev_budget("onejev_1_6_4b", false).unwrap().notice,
        Some(NOTICE_SLOW_ON_CPU)
    );
    assert_eq!(
        local_jev_budget("onejev_1_6_4b", false).unwrap().hold_ms,
        None
    );
    assert!(
        local_jev_budget("onejev_1_6_0_8b", false)
            .unwrap()
            .notice
            .is_none()
    );
    assert!(local_jev_budget("laya_multilingual", true).is_none());
}

#[test]
fn orphan_reaper_only_matches_moss_onejev_processes_adopted_by_init() {
    let roots = vec!["/Users/u/Library/Application Support/moss/models".to_owned()];
    let ps = "\
  101     1 /Users/u/Library/Application Support/moss/models/.runtime/llama-b12167/llama-server --host 127.0.0.1 --port 5000 -m /Users/u/Library/Application Support/moss/models/onejev_1_6_4b/OneJev1.6-4B.Q4_K_M.gguf
  102   900 /Users/u/Library/Application Support/moss/models/.runtime/llama-b12167/llama-server -m /Users/u/Library/Application Support/moss/models/onejev_1_6_0_8b/OneJev1.6-0.8B.Q8_0.gguf
  103     1 /opt/homebrew/bin/llama-server -m /Users/u/models/qwen.gguf
  104     1 /opt/homebrew/bin/llama-server -m /Users/u/Library/Application Support/moss/models/other.gguf
  105     1 /usr/bin/python3 -m laya.serve
garbage line
";
    assert_eq!(orphan_llama_pids(ps, &roots), vec![101]);
    assert!(orphan_llama_pids(ps, &[String::new()]).is_empty());
}

#[test]
fn llama_log_summary_prefers_error_lines_and_stays_bounded() {
    let log = LlamaLog::default();
    assert_eq!(log.summary(3), "");
    for i in 0..100 {
        log.push(&format!("info line {i}"));
    }
    assert_eq!(log.0.lock().unwrap().len(), LLAMA_LOG_LINES);
    assert!(log.summary(2).ends_with("info line 99"));
    log.push("llama_model_load: error loading model: failed to open file");
    log.push(&"x".repeat(1000));
    assert!(log.summary(3).contains("error loading model"));
    assert!(log.0.lock().unwrap().back().unwrap().chars().count() <= LLAMA_LOG_LINE_CHARS);
    assert!(with_log_hint("启动失败".into(), &log).contains("推理进程输出"));
    assert_eq!(
        with_log_hint("启动失败".into(), &LlamaLog::default()),
        "启动失败"
    );
}

#[test]
fn start_with_cancel_checks_port_conflict_and_releases_port_before_ready() {
    let dir = tempfile::tempdir().unwrap();
    let gguf = dir.path().join("OneJev1.6-0.8B.Q8_0.gguf");
    std::fs::write(&gguf, b"stub-gguf").unwrap();

    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let occupied_addr = occupied.local_addr().unwrap();
    let cancel = AtomicBool::new(false);
    let err = match OneJevGateway::start_with_cancel(
        "onejev_1_6_0_8b",
        dir.path(),
        dir.path(),
        occupied_addr,
        None,
        &cancel,
    ) {
        Ok(mut gw) => {
            gw.stop();
            panic!("expected occupied port error");
        }
        Err(err) => err,
    };
    assert!(err.contains("绑定本地 Jev 端口"), "unexpected err: {err}");
    drop(occupied);

    // 当 cancel 已置位时，探测端口后立即退出，且端口保持可重新绑定（不残留 LISTEN 句柄）
    let cancelled = AtomicBool::new(true);
    let res = OneJevGateway::start_with_cancel(
        "onejev_1_6_0_8b",
        dir.path(),
        dir.path(),
        occupied_addr,
        None,
        &cancelled,
    );
    assert!(res.is_err());
    let rebound = TcpListener::bind(occupied_addr);
    assert!(
        rebound.is_ok(),
        "port should remain unbound after cancelled start"
    );
}
