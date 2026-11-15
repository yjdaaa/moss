//! OneJevGateway：托管 llama-server 子进程并在 127.0.0.1:18090 暴露 /v1/systemone，含孤儿进程清理。

use super::*;

/// 从 `ps -axo pid=,ppid=,command=` 的输出里挑出 MOSS 自己拉起、父进程已不在（被 init/launchd 收养）
/// 的 OneJev 推理进程：命令行同时含 `llama-server`、`OneJev1.6-` 权重名与 MOSS 的模型目录。
/// 只在 macOS / Linux 的清理里调用；单元测试在各平台都跑。
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(super) fn orphan_llama_pids(ps_output: &str, model_roots: &[String]) -> Vec<u32> {
    ps_output
        .lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (ppid, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let ppid: u32 = ppid.parse().ok()?;
            let command = command.trim_start();
            let ours = command.contains("llama-server")
                && command.contains("OneJev1.6-")
                && model_roots
                    .iter()
                    .any(|root| !root.is_empty() && command.contains(root.as_str()));
            (ppid == 1 && ours).then_some(pid)
        })
        .collect()
}

/// 清理上次 MOSS 崩溃或被强制结束后遗留的 OneJev 推理进程，返回结束的个数。
/// Windows 由 Job Object 随 MOSS 一起结束，这里只处理 macOS / Linux；只认 MOSS 模型目录里的 OneJev 权重，
/// 绝不碰用户自己运行的其他 `llama-server`。
pub fn reap_orphaned_llama_servers(model_roots: &[PathBuf]) -> usize {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let Ok(out) = Command::new("ps")
            .args(["-axo", "pid=,ppid=,command="])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return 0;
        };
        let roots: Vec<String> = model_roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect();
        let pids = orphan_llama_pids(&String::from_utf8_lossy(&out.stdout), &roots);
        let mut killed = 0;
        for pid in pids {
            let ok = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if ok {
                killed += 1;
            }
        }
        killed
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = model_roots;
        0
    }
}

pub struct OneJevGateway {
    model_id: String,
    accelerator: DetectedAccelerator,
    listen_addr: SocketAddr,
    stop_flag: Arc<AtomicBool>,
    server_thread: Option<std::thread::JoinHandle<()>>,
    llama_child: Arc<Mutex<Option<Child>>>,
    _job: Option<JobHandle>,
    log: LlamaLog,
}

/// `llama-server` 错误输出的最近几行（只在内存里，按行数与长度封顶），用于启动失败与意外退出时说明原因。
#[derive(Clone, Default)]
pub struct LlamaLog(pub(super) Arc<Mutex<std::collections::VecDeque<String>>>);

pub(super) const LLAMA_LOG_LINES: usize = 40;
pub(super) const LLAMA_LOG_LINE_CHARS: usize = 240;

impl LlamaLog {
    pub(super) fn push(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(mut lines) = self.0.lock() else { return };
        if lines.len() >= LLAMA_LOG_LINES {
            lines.pop_front();
        }
        lines.push_back(line.chars().take(LLAMA_LOG_LINE_CHARS).collect());
    }

    /// 持续读完子进程 stderr（读到 EOF 即子进程已退出），避免管道写满阻塞推理进程。
    pub(crate) fn drain<R: Read + Send + 'static>(&self, pipe: R) {
        let log = self.clone();
        let _ = std::thread::Builder::new()
            .name("onejev-llama-log".into())
            .spawn(move || {
                let mut reader = BufReader::new(pipe);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => log.push(&String::from_utf8_lossy(&buf)),
                    }
                }
            });
    }

    /// 最近几行里最能说明问题的：优先带 error / failed 字样的行，否则取最后几行。
    pub fn summary(&self, max_lines: usize) -> String {
        let Ok(lines) = self.0.lock() else {
            return String::new();
        };
        let lower_hit = |l: &String| {
            let l = l.to_ascii_lowercase();
            l.contains("error")
                || l.contains("failed")
                || l.contains("unable")
                || l.contains("out of memory")
        };
        let picked: Vec<&String> = {
            let hits: Vec<&String> = lines.iter().filter(|l| lower_hit(l)).collect();
            if hits.is_empty() {
                lines.iter().collect()
            } else {
                hits
            }
        };
        let start = picked.len().saturating_sub(max_lines);
        picked[start..]
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

pub(crate) fn with_log_hint(message: String, log: &LlamaLog) -> String {
    let tail = log.summary(3);
    if tail.is_empty() {
        message
    } else {
        format!("{message}。推理进程输出：{tail}")
    }
}

impl OneJevGateway {
    pub fn start(
        model_id: &str,
        model_dir: &Path,
        models_root: &Path,
        listen_addr: SocketAddr,
        mirror_hint: Option<&str>,
    ) -> Result<Self, String> {
        let cancel = AtomicBool::new(false);
        Self::start_with_cancel(
            model_id,
            model_dir,
            models_root,
            listen_addr,
            mirror_hint,
            &cancel,
        )
    }

    pub fn start_with_cancel(
        model_id: &str,
        model_dir: &Path,
        models_root: &Path,
        listen_addr: SocketAddr,
        mirror_hint: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        if cancel.load(Ordering::SeqCst) {
            return Err("本地 OneJev 启动已取消".into());
        }
        let (gguf_name, mmproj_name) = match model_id {
            "onejev_1_6_0_8b" => (
                "OneJev1.6-0.8B.Q8_0.gguf",
                "OneJev1.6-0.8B.mmproj-Q8_0.gguf",
            ),
            "onejev_1_6_4b" => ("OneJev1.6-4B.Q4_K_M.gguf", "OneJev1.6-4B.mmproj-Q8_0.gguf"),
            "onejev_1_6_9b" => ("OneJev1.6-9B.Q4_K_M.gguf", "OneJev1.6-9B.mmproj-Q8_0.gguf"),
            other => return Err(format!("未知的 OneJev 模型编号：{other}")),
        };
        let gguf_path = model_dir.join(gguf_name);
        let mmproj_path = model_dir.join(mmproj_name);
        if !gguf_path.is_file() {
            return Err(format!("缺少模型权重文件：{}", gguf_path.display()));
        }

        // 先做一次瞬时回环端口探测并立即释放：若 18090 已被外部服务占用可立刻报错，
        // 同时不在下载/加载数 GB 权重期间挂着未 accept 的 LISTEN 队列（否则请求会卡满超时并触发熔断，而非 <1ms ECONNREFUSED 放行）
        if listen_addr.port() != 0 {
            drop(
                TcpListener::bind(listen_addr)
                    .map_err(|err| format!("绑定本地 Jev 端口 {listen_addr} 失败：{err}"))?,
            );
        }

        let llama_bin = ensure_llama_runtime(models_root, mirror_hint, cancel, &|_| {})?;
        if cancel.load(Ordering::SeqCst) {
            return Err("本地 OneJev 启动已取消".into());
        }
        let accel = probe_llama_accelerator_with_cancel(&llama_bin, true, Some(cancel));
        if cancel.load(Ordering::SeqCst) {
            return Err("本地 OneJev 启动已取消".into());
        }

        let llama_port = {
            let sock = TcpListener::bind(("127.0.0.1", 0))
                .map_err(|e| format!("分配本地推理端口失败：{e}"))?;
            sock.local_addr()
                .map_err(|e| format!("读取本地推理端口失败：{e}"))?
                .port()
        };

        let mut cmd = Command::new(&llama_bin);
        cmd.args([
            "--host",
            "127.0.0.1",
            "--port",
            &llama_port.to_string(),
            "-c",
            &DEFAULT_CTX_TOKENS.to_string(),
            "-np",
            "1",
            "--image-min-tokens",
            "64",
            "--image-max-tokens",
            "4096",
            "-m",
        ])
        .arg(&gguf_path);

        if mmproj_path.is_file() {
            cmd.arg("--mmproj").arg(&mmproj_path);
        }
        if accel.kind.uses_gpu() {
            cmd.args(["-ngl", "all", "-fa", "auto"]);
        } else {
            cmd.args(["-ngl", "0"]);
            if mmproj_path.is_file() {
                cmd.arg("--no-mmproj-offload");
            }
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        configure_no_window(&mut cmd);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动 OneJev llama-server 失败：{e}"))?;
        let job = JobHandle::for_child(&child);
        let log = LlamaLog::default();
        if let Some(stderr) = child.stderr.take() {
            log.drain(stderr);
        }

        let upstream_addr = SocketAddr::from(([127, 0, 0, 1], llama_port));
        let load_timeout = if model_id == "onejev_1_6_9b" {
            Duration::from_secs(60)
        } else if model_id == "onejev_1_6_4b" {
            Duration::from_secs(45)
        } else {
            Duration::from_secs(30)
        };
        let deadline = Instant::now() + load_timeout;
        let mut ready = false;
        while Instant::now() < deadline {
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("本地 OneJev 启动已取消".into());
            }
            if let Ok(Some(status)) = child.try_wait() {
                // 给日志线程一点时间读完最后的错误输出
                std::thread::sleep(Duration::from_millis(100));
                return Err(with_log_hint(
                    format!(
                        "OneJev 底层推理进程提前退出（状态码 {:?}，加速模式 {}）",
                        status.code(),
                        accel.display_text()
                    ),
                    &log,
                ));
            }
            if probe_http_health(upstream_addr, Duration::from_millis(150)) {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(60));
        }
        if !ready {
            let _ = child.kill();
            let _ = child.wait();
            return Err(with_log_hint(
                "OneJev 模型加载超时（首次加载权重到显存/统一内存较慢，请稍后重试）".into(),
                &log,
            ));
        }
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("本地 OneJev 启动已取消".into());
        }

        let upstream = match UpstreamClient::connect(format!("http://127.0.0.1:{llama_port}")) {
            Ok(client) => Arc::new(client),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err);
            }
        };

        let listener = match TcpListener::bind(listen_addr) {
            Ok(sock) => sock,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("绑定本地 Jev 端口 {listen_addr} 失败：{err}"));
            }
        };
        let bound_addr = listener.local_addr().unwrap_or(listen_addr);

        let child_arc = Arc::new(Mutex::new(Some(child)));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let mut gateway = Self::spawn_with_upstream(
            model_id.to_owned(),
            accel,
            listener,
            bound_addr,
            upstream,
            child_arc,
            job,
            stop_flag,
        )?;
        gateway.log = log;
        Ok(gateway)
    }

    pub(super) fn spawn_with_upstream(
        model_id: String,
        accelerator: DetectedAccelerator,
        listener: TcpListener,
        bound_addr: SocketAddr,
        upstream: Arc<UpstreamClient>,
        llama_child: Arc<Mutex<Option<Child>>>,
        job: Option<JobHandle>,
        stop_flag: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let thread_stop = stop_flag.clone();
        let thread_child = llama_child.clone();
        let cleanup_child = llama_child.clone();
        let thread_model = model_id.clone();
        let thread_accel = accelerator.display_text();
        let handle = match std::thread::Builder::new()
            .name(format!("onejev-gateway-{model_id}"))
            .spawn(move || {
                run_gateway_loop(
                    listener,
                    upstream,
                    thread_child,
                    thread_stop,
                    &thread_model,
                    &thread_accel,
                );
            }) {
            Ok(h) => h,
            Err(e) => {
                if let Ok(mut guard) = cleanup_child.lock()
                    && let Some(mut child) = guard.take()
                {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(format!("启动 OneJev 网关线程失败：{e}"));
            }
        };

        Ok(Self {
            model_id,
            accelerator,
            listen_addr: bound_addr,
            stop_flag,
            server_thread: Some(handle),
            llama_child,
            _job: job,
            log: LlamaLog::default(),
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn accelerator(&self) -> &DetectedAccelerator {
        &self.accelerator
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    /// 推理进程最近的错误输出摘要（意外退出后用来说明原因；没有输出时为空）。
    pub fn log_summary(&self) -> String {
        self.log.summary(3)
    }

    pub fn is_alive(&self) -> bool {
        if self.stop_flag.load(Ordering::SeqCst) {
            return false;
        }
        let Ok(mut guard) = self.llama_child.lock() else {
            return false;
        };
        match guard.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => true,
        }
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.listen_addr, Duration::from_millis(80));
        if let Some(handle) = self.server_thread.take() {
            let _ = handle.join();
        }
        if let Ok(mut guard) = self.llama_child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        self._job = None;
    }
}

impl Drop for OneJevGateway {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn probe_http_health(addr: SocketAddr, timeout: Duration) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 512];
    let Ok(n) = stream.read(&mut buf) else {
        return false;
    };
    let text = String::from_utf8_lossy(&buf[..n]);
    text.lines()
        .next()
        .is_some_and(|l| l.starts_with("HTTP/1.1 200") || l.starts_with("HTTP/1.0 200"))
        && text.contains("\"ok\"")
}

pub fn health_payload(model_id: &str, accel_label: &str) -> Value {
    json!({
        "status": "ok",
        "engine": "onejev",
        "model": model_id,
        "fork_mode": "llama.cpp",
        "prompt_version": PROMPT_VERSION,
        "accelerator": accel_label,
    })
}

struct ActiveConnGuard(Arc<AtomicUsize>);

impl Drop for ActiveConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn is_allowed_loopback_host(raw_host: &str) -> bool {
    let trimmed = raw_host.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Some(rest) = trimmed.strip_prefix("[::1]") {
        return rest.is_empty()
            || rest
                .strip_prefix(':')
                .is_some_and(|port| port.parse::<u16>().is_ok());
    }
    let (host_part, port_ok) = match trimmed.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().is_ok()),
        None => (trimmed, true),
    };
    port_ok && (host_part == "127.0.0.1" || host_part.eq_ignore_ascii_case("localhost"))
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    max_bytes: usize,
) -> Result<Option<String>, &'static str> {
    let mut buf = Vec::with_capacity(128);
    loop {
        let available = reader.fill_buf().map_err(|_| "header read error")?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            if buf.len() + pos + 1 > max_bytes {
                return Err("header line too long");
            }
            buf.extend_from_slice(&available[..=pos]);
            reader.consume(pos + 1);
            break;
        }
        if buf.len() + available.len() > max_bytes {
            return Err("header line too long");
        }
        buf.extend_from_slice(available);
        let len = available.len();
        reader.consume(len);
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| "invalid header encoding")
}

fn run_gateway_loop(
    listener: TcpListener,
    upstream: Arc<UpstreamClient>,
    llama_child: Arc<Mutex<Option<Child>>>,
    stop_flag: Arc<AtomicBool>,
    model_id: &str,
    accel_label: &str,
) {
    let active_conns = Arc::new(AtomicUsize::new(0));
    while !stop_flag.load(Ordering::SeqCst) {
        let Ok((mut stream, _)) = listener.accept() else {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }
            continue;
        };
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }
        let mut admitted = false;
        let mut cur = active_conns.load(Ordering::Acquire);
        while cur < MAX_ACTIVE_GATEWAY_CONNS {
            match active_conns.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    admitted = true;
                    break;
                }
                Err(next) => cur = next,
            }
        }
        if !admitted {
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            write_json_response(
                &mut stream,
                503,
                &json!({ "status": "busy", "detail": "too many concurrent requests" }),
            );
            continue;
        }
        let guard = ActiveConnGuard(active_conns.clone());
        let upstream = upstream.clone();
        let llama_child = llama_child.clone();
        let model_id = model_id.to_owned();
        let accel_label = accel_label.to_owned();
        std::thread::spawn(move || {
            let _guard = guard;
            handle_gateway_conn(stream, &upstream, &llama_child, &model_id, &accel_label);
        });
    }
}

fn handle_gateway_conn(
    mut stream: TcpStream,
    upstream: &UpstreamClient,
    llama_child: &Mutex<Option<Child>>,
    model_id: &str,
    accel_label: &str,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let Ok(cloned) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(cloned);
    let request_line = match read_bounded_line(&mut reader, 4096) {
        Ok(Some(line)) => line,
        Ok(None) => return,
        Err(detail) => {
            write_json_response(&mut stream, 400, &json!({ "detail": detail }));
            return;
        }
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or(raw_path);

    let mut header_bytes = request_line.len();
    let mut header_lines = 0usize;
    let mut content_length = 0usize;
    let mut host_header: Option<String> = None;
    let mut has_origin = false;
    let mut sec_fetch_site: Option<String> = None;
    let mut has_transfer_encoding = false;

    loop {
        let line = match read_bounded_line(&mut reader, 4096) {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(detail) => {
                write_json_response(&mut stream, 400, &json!({ "detail": detail }));
                return;
            }
        };
        if line == "\r\n" || line == "\n" {
            break;
        }
        header_lines += 1;
        header_bytes += line.len();
        if header_lines > MAX_HTTP_HEADER_LINES || header_bytes > MAX_HTTP_HEADER_BYTES {
            write_json_response(
                &mut stream,
                431,
                &json!({ "detail": "request headers too large" }),
            );
            return;
        }
        if let Some((name, val)) = line.split_once(':') {
            let key = name.trim();
            let value = val.trim();
            if key.eq_ignore_ascii_case("content-length") {
                let Ok(parsed) = value.parse::<usize>() else {
                    write_json_response(
                        &mut stream,
                        400,
                        &json!({ "detail": "invalid content-length" }),
                    );
                    return;
                };
                content_length = parsed;
            } else if key.eq_ignore_ascii_case("host") {
                host_header = Some(value.to_owned());
            } else if key.eq_ignore_ascii_case("origin") && !value.is_empty() {
                has_origin = true;
            } else if key.eq_ignore_ascii_case("sec-fetch-site") {
                sec_fetch_site = Some(value.to_ascii_lowercase());
            } else if key.eq_ignore_ascii_case("transfer-encoding") {
                has_transfer_encoding = true;
            }
        }
    }

    // 回环免鉴权安全防护（对齐 safety.md §2）：在路由与读取请求体前拦截 DNS 重绑定与跨站浏览器请求
    if method.eq_ignore_ascii_case("OPTIONS") {
        write_json_response(
            &mut stream,
            403,
            &json!({ "detail": "preflight forbidden" }),
        );
        return;
    }
    if !host_header.as_deref().is_some_and(is_allowed_loopback_host) {
        write_json_response(&mut stream, 403, &json!({ "detail": "forbidden host" }));
        return;
    }
    if has_origin
        || sec_fetch_site
            .as_deref()
            .is_some_and(|site| !matches!(site, "none" | "same-origin"))
    {
        write_json_response(
            &mut stream,
            403,
            &json!({ "detail": "cross-origin requests forbidden" }),
        );
        return;
    }
    if has_transfer_encoding {
        write_json_response(
            &mut stream,
            400,
            &json!({ "detail": "transfer-encoding unsupported" }),
        );
        return;
    }

    if content_length > MAX_HTTP_BODY_BYTES {
        write_json_response(
            &mut stream,
            413,
            &json!({ "detail": "request body too large" }),
        );
        return;
    }

    let mut body = Vec::with_capacity(content_length.min(64 * 1024));
    if content_length > 0 {
        let mut limited = (&mut reader).take(content_length as u64);
        if limited.read_to_end(&mut body).is_err() || body.len() != content_length {
            write_json_response(
                &mut stream,
                400,
                &json!({ "detail": "incomplete request body" }),
            );
            return;
        }
    }

    let child_alive = llama_child
        .lock()
        .ok()
        .is_some_and(|mut g| match g.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => true,
        });
    if !child_alive {
        write_json_response(
            &mut stream,
            503,
            &json!({ "status": "error", "detail": "llama-server exited" }),
        );
        return;
    }

    match (method, path) {
        ("GET", "/health") => {
            write_json_response(&mut stream, 200, &health_payload(model_id, accel_label));
        }
        ("GET", "/v1/models") => {
            write_json_response(
                &mut stream,
                200,
                &json!({
                    "models": [
                        { "name": model_id, "description": "OneJev 1.6 local multimodal decision model", "release_date": "2026-10-17" },
                        { "name": "jev-latest", "description": format!("alias for {model_id}"), "release_date": "2026-10-17" },
                        { "name": "jev-preview", "description": format!("alias for {model_id}"), "release_date": "2026-10-17" },
                        { "name": "qev-latest", "description": format!("alias for {model_id}"), "release_date": "2026-10-17" },
                    ]
                }),
            );
        }
        ("POST", "/v1/systemone") => {
            let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
                write_json_response(&mut stream, 422, &json!({ "detail": "invalid JSON" }));
                return;
            };
            let requested_model = payload
                .get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("jev-latest");
            let allowed = matches!(requested_model, "jev-latest" | "jev-preview" | "qev-latest")
                || requested_model == model_id
                || crate::settings::canonical_local_jev_model(requested_model) == Some(model_id);
            if !allowed {
                write_json_response(
                    &mut stream,
                    404,
                    &json!({ "detail": format!("model {requested_model:?} is not served by {model_id}") }),
                );
                return;
            }
            let state = payload.get("state").unwrap_or(&Value::Null);
            let Some(questions) = payload.get("questions").and_then(Value::as_object) else {
                write_json_response(
                    &mut stream,
                    422,
                    &json!({ "detail": "questions must be an object" }),
                );
                return;
            };
            let media = payload.get("media");
            let abandoned = || client_disconnected(&stream);
            match upstream.evaluate(model_id, state, questions, media, &abandoned) {
                Ok((CLIENT_GONE_STATUS, _)) => {}
                Ok((status, resp_json)) => write_json_response(&mut stream, status, &resp_json),
                Err(err) => {
                    write_json_response(&mut stream, 502, &json!({ "error": err, "detail": err }))
                }
            }
        }
        _ => {
            write_json_response(&mut stream, 404, &json!({ "detail": "not found" }));
        }
    }
}

/// 内部状态码：调用方已断开，不写响应。
pub(super) const CLIENT_GONE_STATUS: u16 = 499;

/// 请求已完整读完后，调用方若已关闭连接（放弃等待），套接字上会读到 EOF 或连接重置。
/// 只做非阻塞 peek，不消费数据；无法判断时按“仍在等待”处理。
fn client_disconnected(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return false;
    }
    let mut buf = [0u8; 1];
    let gone = match stream.peek(&mut buf) {
        Ok(0) => true,
        Ok(_) => false,
        Err(err) => !matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ),
    };
    let _ = stream.set_nonblocking(false);
    gone
}

pub(super) fn write_json_response(stream: &mut TcpStream, status: u16, body: &Value) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let payload = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&payload);
    let _ = stream.flush();
}
