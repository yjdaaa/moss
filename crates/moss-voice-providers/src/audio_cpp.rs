//! `audio.cpp`（Qwen3-TTS 1.7B Base 零样本声音克隆）本机回环常驻服务端管理与 SSE 流式合成提供方（n131）。
//!
//! - MOSS 按需在 `127.0.0.1` 随机空闲端口拉起 `audiocpp_server --no-ui` 常驻子进程，并纳入 `JobHandle`
//!   进程树守卫，避免每次合成重新加载 2.7 GB 权重；
//! - 通过 `voice_dir` 下的代次文件 `ref_<gen>.wav` 配合 `qwen3_tts.voice_prompt_cache_slots = 1`，
//!   同一参考音色首声延迟约 150～350 ms，更换参考录音或修改台词后无需重启进程即可即时生效；
//! - 参考台词非空时走 ICL 音频码本提示（`options.reference_text`），留空时自动退回声纹模式
//!   （`options.x_vector_only_mode = true`）；
//! - 所有本机回环请求强制 `.no_proxy()` 与 `redirect(Policy::none())`，打断对话（`chunks.closed()`）时
//!   立即断开连接以释放单模型推理槽位，错误信息全部脱敏。

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use moss_process::JobHandle;
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::openai::{MAX_AUDIO_BYTES, MAX_ERROR_BYTES, SseDecoder, read_body};
use crate::{BoxFuture, DiagCategory, Pcm, ProviderError, Tts, scrub_diagnostic_text};

pub const AUDIO_CPP_SAMPLE_RATE: u32 = 24_000;
const MODEL_SLOT_ID: &str = "qwen3-clone";
const READY_TIMEOUT: Duration = Duration::from_secs(95);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_SSE_STREAM_BYTES: usize = MAX_AUDIO_BYTES * 2;
const STDERR_TAIL_LIMIT: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCppVoiceRef {
    pub voice_name: String,
    pub transcript: String,
}

pub type VoiceRefResolver = Arc<dyn Fn() -> Result<AudioCppVoiceRef, ProviderError> + Send + Sync>;

struct WorkerInner {
    base_url: String,
    model_path: PathBuf,
    voice_dir: PathBuf,
    config_dir: Mutex<Option<PathBuf>>,
    role: &'static str,
    alive: AtomicBool,
    child: Mutex<Option<Child>>,
    job: Mutex<Option<JobHandle>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    http_client: reqwest::Client,
    synth_lock: Arc<tokio::sync::Mutex<()>>,
}

impl WorkerInner {
    fn kill_child_quiet(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.reap_child_and_config();
    }

    fn kill_child_with_reason(&self, reason: &str) {
        if self.alive.swap(false, Ordering::SeqCst)
            && self.child.lock().ok().is_some_and(|c| c.is_some())
            && let Some(observer) = crate::local::helper_observer()
        {
            observer.exited(self.role, reason);
        }
        self.reap_child_and_config();
    }

    fn reap_child_and_config(&self) {
        if let Ok(mut job) = self.job.lock()
            && let Some(job) = job.take()
        {
            job.terminate();
        }
        if let Ok(mut slot) = self.child.lock()
            && let Some(mut child) = slot.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Ok(mut cfg) = self.config_dir.lock()
            && let Some(dir) = cfg.take()
        {
            let _ = fs::remove_dir_all(dir);
        }
    }

    fn stderr_hint(&self) -> String {
        let Ok(lines) = self.stderr_tail.lock() else {
            return String::new();
        };
        let joined: Vec<&str> = lines.iter().rev().take(4).map(String::as_str).collect();
        if joined.is_empty() {
            String::new()
        } else {
            let ordered: Vec<&str> = joined.into_iter().rev().collect();
            scrub_diagnostic_text(&ordered.join(" | "))
        }
    }
}

impl Drop for WorkerInner {
    fn drop(&mut self) {
        self.kill_child_with_reason("MOSS 不再使用它，已结束");
    }
}

fn model_err(message: impl Into<String>) -> ProviderError {
    ProviderError::with_category(DiagCategory::Model, message)
}

fn build_loopback_client() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| ProviderError::with_category(DiagCategory::Internal, "无法创建本地推理客户端"))
}

fn prepend_library_search_path(cmd: &mut Command, exe: &Path) {
    let Some(dir) = exe.parent() else {
        return;
    };
    let var = if cfg!(target_os = "macos") {
        Some("DYLD_LIBRARY_PATH")
    } else if cfg!(target_os = "linux") {
        Some("LD_LIBRARY_PATH")
    } else if cfg!(windows) {
        Some("PATH")
    } else {
        None
    };
    let Some(var) = var else {
        return;
    };
    let mut paths = vec![dir.to_path_buf()];
    if let Some(existing) = std::env::var_os(var) {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        cmd.env(var, joined);
    }
}

fn pick_ephemeral_port() -> Result<u16, ProviderError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| model_err(format!("无法分配本地回环端口：{e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| model_err(format!("无法读取本地回环端口：{e}")))?
        .port();
    Ok(port)
}

/// 解析 `audiocpp_server --backend <backend> --list-devices --json` 输出，双显卡机器优先选独立显卡（`type == "gpu"`）而非核显（`type == "igpu"`）。
fn select_preferred_device_index(list_json: &str, backend: &str) -> u32 {
    let trimmed = list_json
        .find('{')
        .map(|idx| &list_json[idx..])
        .unwrap_or(list_json);
    let Ok(parsed) = serde_json::from_str::<Value>(trimmed) else {
        return 0;
    };
    let Some(devices) = parsed.get("devices").and_then(Value::as_array) else {
        return 0;
    };
    devices
        .iter()
        .filter(|d| {
            d.get("backend")
                .and_then(Value::as_str)
                .is_some_and(|b| b.eq_ignore_ascii_case(backend))
        })
        .max_by_key(|d| {
            let dev_type = d.get("type").and_then(Value::as_str).unwrap_or("");
            let has_pci = d
                .get("pci_bus_id")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            let is_discrete = dev_type.eq_ignore_ascii_case("gpu")
                || (!dev_type.eq_ignore_ascii_case("igpu")
                    && !dev_type.eq_ignore_ascii_case("cpu")
                    && has_pci);
            let total_bytes = d
                .pointer("/memory/total_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (is_discrete, total_bytes)
        })
        .and_then(|d| d.get("index").and_then(Value::as_u64))
        .and_then(|idx| u32::try_from(idx).ok())
        .unwrap_or(0)
}

fn pick_preferred_backend_device(exe: &Path, backend: &str) -> u32 {
    if let Ok(raw) = std::env::var("MOSS_AUDIOCPP_DEVICE")
        && let Ok(explicit) = raw.trim().parse::<u32>()
    {
        return explicit;
    }
    if matches!(backend, "cpu" | "metal") {
        return 0;
    }
    let mut cmd = Command::new(exe);
    if let Some(exe_dir) = exe.parent() {
        cmd.current_dir(exe_dir);
    }
    cmd.args(["--backend", backend, "--list-devices", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    prepend_library_search_path(&mut cmd, exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let Ok(mut child) = cmd.spawn() else {
        return 0;
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let Some(mut stdout) = child.stdout.take() else {
                    return 0;
                };
                let mut buf = Vec::new();
                if (&mut stdout).take(64 * 1024).read_to_end(&mut buf).is_err() {
                    return 0;
                }
                return select_preferred_device_index(&String::from_utf8_lossy(&buf), backend);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return 0;
            }
        }
    }
}

fn probe_models_ready(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    if stream
        .write_all(
            format!(
                "GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 4096];
    let mut raw = Vec::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() >= 16 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return false;
    }
    body.contains(MODEL_SLOT_ID) && body.contains("\"loaded\":true")
}

fn recommended_cpu_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(2, 6))
        .unwrap_or(4)
}

fn decode_pcm_base64(encoded: &str) -> Result<Pcm, ProviderError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| model_err("本地克隆引擎返回了无效的音频编码"))?;
    if !bytes.len().is_multiple_of(2) {
        return Err(model_err("本地克隆引擎返回的 PCM 字节长度无效"));
    }
    let samples = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| i16::from_le_bytes(pair))
        .collect();
    Ok(Pcm {
        samples,
        sample_rate: AUDIO_CPP_SAMPLE_RATE,
    })
}

/// 运行中的 `audiocpp_server` 子进程句柄；克隆共享同一底层进程与推理互斥锁。
#[derive(Clone)]
pub struct AudioCppWorker {
    inner: Arc<WorkerInner>,
}

impl AudioCppWorker {
    pub fn spawn(
        exe: &Path,
        model_path: &Path,
        voice_dir: &Path,
        backend: &str,
    ) -> Result<Self, ProviderError> {
        Self::spawn_as(
            exe,
            model_path,
            voice_dir,
            backend,
            "Qwen3-TTS 克隆推理程序",
        )
    }

    pub fn spawn_as(
        exe: &Path,
        model_path: &Path,
        voice_dir: &Path,
        backend: &str,
        role: &'static str,
    ) -> Result<Self, ProviderError> {
        Self::spawn_as_cancellable(exe, model_path, voice_dir, backend, role, &|| false)
    }

    pub fn spawn_as_cancellable(
        exe: &Path,
        model_path: &Path,
        voice_dir: &Path,
        backend: &str,
        role: &'static str,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, ProviderError> {
        if !exe.is_file() {
            return Err(model_err(
                "未找到 audio.cpp 推理程序，请删除并重新下载 Qwen3-TTS 声音克隆模型",
            ));
        }
        if !model_path.is_file() {
            return Err(model_err("Qwen3-TTS 声音克隆模型文件缺失，请重新下载"));
        }
        fs::create_dir_all(voice_dir)
            .map_err(|e| model_err(format!("创建本地参考音色目录失败：{e}")))?;

        let http_client = build_loopback_client()?;
        let device_id = pick_preferred_backend_device(exe, backend);
        let mut last_err = None;
        for attempt in 0..2 {
            if is_cancelled() {
                return Err(model_err("已取消加载声音克隆模型"));
            }
            let port = pick_ephemeral_port()?;
            match Self::spawn_once(
                exe,
                model_path,
                voice_dir,
                backend,
                device_id,
                role,
                port,
                http_client.clone(),
                is_cancelled,
            ) {
                Ok(worker) => return Ok(worker),
                Err((retryable, err)) if retryable && attempt == 0 => {
                    last_err = Some(err);
                }
                Err((_, err)) => return Err(err),
            }
        }
        Err(last_err.unwrap_or_else(|| model_err("启动 Qwen3-TTS 声音克隆引擎失败")))
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_once(
        exe: &Path,
        model_path: &Path,
        voice_dir: &Path,
        backend: &str,
        device_id: u32,
        role: &'static str,
        port: u16,
        http_client: reqwest::Client,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, (bool, ProviderError)> {
        let state_parent = voice_dir.parent().unwrap_or(voice_dir);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let config_dir = state_parent.join(format!(
            ".audiocpp-srv-{}-{port}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&config_dir)
            .map_err(|e| (false, model_err(format!("创建本地推理配置目录失败：{e}"))))?;
        let config_path = config_dir.join("server.json");
        let cfg = json!({
            "host": "127.0.0.1",
            "port": port,
            "backend": backend,
            "device": device_id,
            "threads": recommended_cpu_threads(),
            "lazy_load": false,
            "max_loaded_models": 1,
            "ui": false,
            "ui_management": false,
            "voice_dir": voice_dir,
            "models": [{
                "id": MODEL_SLOT_ID,
                "family": "qwen3_tts",
                "path": model_path,
                "task": "tts",
                "mode": "streaming",
                "session_options": {
                    "qwen3_tts.voice_prompt_cache_slots": 1,
                },
                "default_request_options": {
                    "language": "zh",
                    "temperature": 0.7,
                },
            }],
        });
        let cfg_bytes = serde_json::to_vec_pretty(&cfg)
            .map_err(|e| (false, model_err(format!("生成本地推理配置失败：{e}"))))?;
        if let Err(e) = fs::write(&config_path, cfg_bytes) {
            let _ = fs::remove_dir_all(&config_dir);
            return Err((false, model_err(format!("写入本地推理配置失败：{e}"))));
        }

        let observer = crate::local::helper_observer();
        let mut cmd = Command::new(exe);
        if let Some(exe_dir) = exe.parent() {
            cmd.current_dir(exe_dir);
        }
        cmd.arg("--config")
            .arg(&config_path)
            .arg("--backend")
            .arg(backend)
            .arg("--device")
            .arg(device_id.to_string())
            .arg("--no-ui")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        prepend_library_search_path(&mut cmd, exe);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(err) => {
                let _ = fs::remove_dir_all(&config_dir);
                return Err((
                    false,
                    model_err(format!("启动 Qwen3-TTS 声音克隆进程失败：{err}")),
                ));
            }
        };
        let pid = child.id();
        if let Some(ref obs) = observer {
            obs.started(role, Some(pid));
        }
        let job = JobHandle::for_child(&child);
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LIMIT)));
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            let obs_clone = observer.clone();
            std::thread::Builder::new()
                .name("audiocpp-stderr".into())
                .spawn(move || {
                    let mut reader = BufReader::new(stderr);
                    let mut chunk = Vec::new();
                    loop {
                        chunk.clear();
                        let mut limited = (&mut reader).take(crate::local::STDERR_CHUNK as u64);
                        match limited.read_until(b'\n', &mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                let text = String::from_utf8_lossy(&chunk);
                                let text = text.trim_end_matches(['\r', '\n']);
                                if !text.trim().is_empty() {
                                    if let Some(ref obs) = obs_clone {
                                        obs.stderr_line(role, text);
                                    }
                                    if let Ok(mut buf) = tail.lock() {
                                        if buf.len() >= STDERR_TAIL_LIMIT {
                                            buf.pop_front();
                                        }
                                        buf.push_back(text.trim().to_owned());
                                    }
                                }
                            }
                        }
                    }
                })
                .ok();
        }

        let worker = Self {
            inner: Arc::new(WorkerInner {
                base_url: format!("http://127.0.0.1:{port}"),
                model_path: model_path.to_path_buf(),
                voice_dir: voice_dir.to_path_buf(),
                config_dir: Mutex::new(Some(config_dir)),
                role,
                alive: AtomicBool::new(true),
                child: Mutex::new(Some(child)),
                job: Mutex::new(job),
                stderr_tail,
                http_client,
                synth_lock: Arc::new(tokio::sync::Mutex::new(())),
            }),
        };

        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if is_cancelled() {
                worker.terminate_with_reason("操作已取消，MOSS 结束了它");
                return Err((false, model_err("已取消加载声音克隆模型")));
            }
            if !worker.is_alive() {
                let hint = worker.inner.stderr_hint();
                let port_conflict = hint.to_ascii_lowercase().contains("address already in use")
                    || hint.to_ascii_lowercase().contains("bind");
                worker.terminate_with_reason("启动时提前退出");
                let err = model_err("Qwen3-TTS 声音克隆引擎启动时提前退出，请确认内存充足后重试");
                return Err((
                    port_conflict,
                    if hint.is_empty() {
                        err
                    } else {
                        err.detail(hint)
                    },
                ));
            }
            if probe_models_ready(port) {
                return Ok(worker);
            }
            if Instant::now() >= deadline {
                let hint = worker.inner.stderr_hint();
                worker.terminate_with_reason("模型加载超时，MOSS 结束了它");
                let err = model_err("Qwen3-TTS 声音克隆模型加载超时，请确认内存/显存充足后重试");
                return Err((
                    false,
                    if hint.is_empty() {
                        err
                    } else {
                        err.detail(hint)
                    },
                ));
            }
            std::thread::sleep(Duration::from_millis(120));
        }
    }

    /// 供单元测试连接自定义回环模拟服务端使用。
    pub fn from_loopback_url(
        base_url: String,
        model_path: PathBuf,
        voice_dir: PathBuf,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            inner: Arc::new(WorkerInner {
                base_url: base_url.trim_end_matches('/').to_owned(),
                model_path,
                voice_dir,
                config_dir: Mutex::new(None),
                role: "Qwen3-TTS 克隆推理程序",
                alive: AtomicBool::new(true),
                child: Mutex::new(None),
                job: Mutex::new(None),
                stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
                http_client: build_loopback_client()?,
                synth_lock: Arc::new(tokio::sync::Mutex::new(())),
            }),
        })
    }

    pub fn model_path(&self) -> &Path {
        &self.inner.model_path
    }

    pub fn voice_dir(&self) -> &Path {
        &self.inner.voice_dir
    }

    pub fn is_alive(&self) -> bool {
        if !self.inner.alive.load(Ordering::SeqCst) {
            return false;
        }
        if let Ok(mut guard) = self.inner.child.lock()
            && let Some(ref mut child) = *guard
            && let Ok(Some(status)) = child.try_wait()
        {
            if self.inner.alive.swap(false, Ordering::SeqCst)
                && let Some(obs) = crate::local::helper_observer()
            {
                let text = crate::local::exit_status_text(status);
                obs.exited(self.inner.role, &format!("意外退出（{text}）"));
            }
            return false;
        }
        true
    }

    pub fn terminate(&self) {
        self.inner.kill_child_quiet();
    }

    pub fn terminate_with_reason(&self, reason: &str) {
        self.inner.kill_child_with_reason(reason);
    }

    pub async fn synthesize_stream_with_ref(
        &self,
        text: &str,
        voice_ref: &AudioCppVoiceRef,
        chunks: UnboundedSender<Pcm>,
    ) -> Result<(), ProviderError> {
        let trimmed = text.trim();
        if trimmed.is_empty() || chunks.is_closed() {
            return Ok(());
        }
        if !self.is_alive() {
            let hint = self.inner.stderr_hint();
            let err = model_err("Qwen3-TTS 声音克隆推理进程已退出");
            return Err(if hint.is_empty() {
                err
            } else {
                err.detail(hint)
            });
        }

        let deadline = tokio::time::Instant::now() + STREAM_TOTAL_TIMEOUT;
        let _guard = tokio::select! {
            biased;
            _ = chunks.closed() => return Ok(()),
            res = tokio::time::timeout_at(deadline, self.inner.synth_lock.lock()) => {
                res.map_err(|_| model_err("等待本地声音克隆推理槽位超时"))?
            }
        };
        if chunks.is_closed() {
            return Ok(());
        }
        if !self.is_alive() {
            return Err(model_err("Qwen3-TTS 声音克隆推理进程已退出"));
        }

        let transcript = voice_ref.transcript.trim();
        let options = if transcript.is_empty() {
            json!({
                "language": "zh",
                "temperature": 0.7,
                "x_vector_only_mode": true,
            })
        } else {
            json!({
                "language": "zh",
                "temperature": 0.7,
                "reference_text": transcript,
            })
        };
        let payload = json!({
            "model": MODEL_SLOT_ID,
            "input": trimmed,
            "voice": voice_ref.voice_name,
            "stream_format": "sse",
            "options": options,
        });
        let url = format!("{}/v1/audio/speech", self.inner.base_url);
        let send_wait = STREAM_IDLE_TIMEOUT
            .min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        if send_wait.is_zero() {
            return Err(model_err("本地声音克隆请求超时"));
        }
        let mut response = tokio::select! {
            biased;
            _ = chunks.closed() => return Ok(()),
            res = tokio::time::timeout(
                send_wait,
                self.inner
                    .http_client
                    .post(&url)
                    .header("Connection", "close")
                    .json(&payload)
                    .send(),
            ) => res,
        }
        .map_err(|_| model_err("本地声音克隆首包等待超时"))?
        .map_err(|_| {
            let hint = self.inner.stderr_hint();
            let err = model_err("无法连接本地声音克隆推理进程");
            if hint.is_empty() {
                err
            } else {
                err.detail(hint)
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            let body = read_body("本地声音克隆", response, MAX_ERROR_BYTES)
                .await
                .unwrap_or_default();
            let detail = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .or_else(|| v.get("error"))
                        .or_else(|| v.get("message"))
                        .and_then(Value::as_str)
                        .map(scrub_diagnostic_text)
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| scrub_diagnostic_text(&String::from_utf8_lossy(&body)));
            let err = model_err(format!("本地声音克隆返回 HTTP {}", status.as_u16()));
            return Err(if detail.is_empty() {
                err
            } else {
                err.detail(detail)
            });
        }

        let mut decoder = SseDecoder::default();
        let mut stream_bytes = 0usize;
        let mut saw_audio = false;
        let mut saw_done = false;

        enum Step {
            Continue,
            Done,
            ReceiverClosed,
        }

        let handle_event = |data: &str, saw_audio: &mut bool| -> Result<Step, ProviderError> {
            let trimmed_data = data.trim();
            if trimmed_data.is_empty() {
                return Ok(Step::Continue);
            }
            if trimmed_data == "[DONE]" {
                return Ok(Step::Done);
            }
            let val: Value = serde_json::from_str(trimmed_data)
                .map_err(|_| model_err("本地声音克隆返回了无效的流式事件"))?;
            match val.get("type").and_then(Value::as_str) {
                Some("speech.audio.delta") => {
                    if let Some(b64) = val.get("audio").and_then(Value::as_str)
                        && !b64.trim().is_empty()
                    {
                        let pcm = decode_pcm_base64(b64)?;
                        if !pcm.samples.is_empty() {
                            *saw_audio = true;
                            if chunks.send(pcm).is_err() {
                                return Ok(Step::ReceiverClosed);
                            }
                        }
                    }
                    Ok(Step::Continue)
                }
                Some("speech.audio.done") => Ok(Step::Done),
                Some("error") => {
                    let msg = val
                        .pointer("/error/message")
                        .or_else(|| val.get("message"))
                        .and_then(Value::as_str)
                        .map(scrub_diagnostic_text)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "本地声音克隆流式合成出错".into());
                    Err(model_err(msg))
                }
                _ => Ok(Step::Continue),
            }
        };

        loop {
            let wait = if saw_done {
                Duration::from_millis(60)
            } else {
                STREAM_IDLE_TIMEOUT
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now()))
            };
            if wait.is_zero() {
                if saw_done {
                    break;
                }
                return Err(model_err("本地声音克隆合成总耗时超时"));
            }
            let next = tokio::select! {
                biased;
                _ = chunks.closed() => return Ok(()),
                res = tokio::time::timeout(wait, response.chunk()) => res,
            };
            let chunk = match next {
                Ok(Ok(Some(c))) => c,
                Ok(Ok(None)) => break,
                Err(_) if saw_done => break,
                Err(_) => return Err(model_err("本地声音克隆流式接收超时")),
                Ok(Err(_)) if saw_done && saw_audio => break,
                Ok(Err(_)) => return Err(model_err("本地声音克隆音频流意外中断")),
            };
            stream_bytes = stream_bytes.saturating_add(chunk.len());
            if stream_bytes > MAX_SSE_STREAM_BYTES {
                return Err(ProviderError::with_category(
                    DiagCategory::Audio,
                    "本地声音克隆返回的数据超过大小上限",
                ));
            }
            for event_data in decoder.push(&chunk) {
                match handle_event(&event_data, &mut saw_audio)? {
                    Step::Continue => {}
                    Step::Done => saw_done = true,
                    Step::ReceiverClosed => return Ok(()),
                }
            }
        }
        if let Some(event_data) = decoder.finish()
            && matches!(
                handle_event(&event_data, &mut saw_audio)?,
                Step::ReceiverClosed
            )
        {
            return Ok(());
        }
        if !saw_audio && !chunks.is_closed() {
            return Err(model_err("本地声音克隆未生成有效音频"));
        }
        Ok(())
    }

    pub async fn synthesize_with_ref(
        &self,
        text: &str,
        voice_ref: &AudioCppVoiceRef,
    ) -> Result<Pcm, ProviderError> {
        let (tx, mut rx) = unbounded_channel();
        self.synthesize_stream_with_ref(text, voice_ref, tx).await?;
        let mut samples = Vec::new();
        while let Some(chunk) = rx.recv().await {
            samples.extend_from_slice(&chunk.samples);
        }
        Ok(Pcm {
            samples,
            sample_rate: AUDIO_CPP_SAMPLE_RATE,
        })
    }
}

/// 基于 `audio.cpp`（Qwen3-TTS 1.7B Base）的本地零样本声音克隆合成提供方。
pub struct AudioCppTts {
    pub worker: AudioCppWorker,
    pub resolve_ref: VoiceRefResolver,
}

impl Tts for AudioCppTts {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        Box::pin(async move {
            if text.trim().is_empty() {
                return Ok(Pcm {
                    samples: Vec::new(),
                    sample_rate: AUDIO_CPP_SAMPLE_RATE,
                });
            }
            let voice_ref = (self.resolve_ref)()?;
            self.worker.synthesize_with_ref(text, &voice_ref).await
        })
    }

    fn synthesize_stream<'a>(
        &'a self,
        text: &'a str,
        chunks: UnboundedSender<Pcm>,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            if text.trim().is_empty() || chunks.is_closed() {
                return Ok(());
            }
            let voice_ref = (self.resolve_ref)()?;
            self.worker
                .synthesize_stream_with_ref(text, &voice_ref, chunks)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener as AsyncTcpListener;

    async fn read_http_json(stream: &mut tokio::net::TcpStream) -> Value {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let header_end = loop {
            let n = stream.read(&mut tmp).await.unwrap();
            assert!(n > 0);
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..header_end]);
        let content_len = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        while buf.len() < header_end + content_len {
            let n = stream.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        serde_json::from_slice(&buf[header_end..header_end + content_len]).unwrap()
    }

    #[tokio::test]
    async fn audiocpp_tts_streams_icl_and_xvector_modes_and_aborts_on_barge_in() {
        let listener = AsyncTcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // 1. ICL mode request
            let (mut s1, _) = listener.accept().await.unwrap();
            let req1 = read_http_json(&mut s1).await;
            assert_eq!(req1["model"], "qwen3-clone");
            assert_eq!(req1["voice"], "ref_1");
            assert_eq!(req1["stream_format"], "sse");
            assert_eq!(
                req1["options"]["reference_text"],
                "让人类永远保持理智，确实是一种奢求。"
            );
            assert!(req1["options"].get("x_vector_only_mode").is_none());
            let pcm_bytes: Vec<u8> = [1200i16, -1200i16, 2400i16]
                .into_iter()
                .flat_map(i16::to_le_bytes)
                .collect();
            let b64 = base64::engine::general_purpose::STANDARD.encode(pcm_bytes);
            let sse = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata:\n\ndata: {{\"type\":\"speech.audio.delta\",\"audio\":\"{b64}\"}}\n\ndata: {{\"type\":\"speech.audio.done\",\"timing\":{{\"ttft_ms\":142.0}}}}\n\n"
            );
            s1.write_all(sse.as_bytes()).await.unwrap();
            drop(s1);

            // 2. x_vector_only_mode when transcript is empty
            let (mut s2, _) = listener.accept().await.unwrap();
            let req2 = read_http_json(&mut s2).await;
            assert_eq!(req2["voice"], "ref_2");
            assert_eq!(req2["options"]["x_vector_only_mode"], true);
            assert!(req2["options"].get("reference_text").is_none());
            let b64_2 = base64::engine::general_purpose::STANDARD.encode(500i16.to_le_bytes());
            let sse2 = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {{\"type\":\"speech.audio.delta\",\"audio\":\"{b64_2}\"}}\n\ndata: {{\"type\":\"speech.audio.done\"}}\n\n"
            );
            s2.write_all(sse2.as_bytes()).await.unwrap();
            drop(s2);

            // 3. Barge-in cancellation: send 1 chunk, then stall — client must disconnect immediately on receiver drop
            let (mut s3, _) = listener.accept().await.unwrap();
            let _ = read_http_json(&mut s3).await;
            let head3 = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {{\"type\":\"speech.audio.delta\",\"audio\":\"{b64_2}\"}}\n\n"
            );
            s3.write_all(head3.as_bytes()).await.unwrap();
            let mut eof_buf = [0u8; 16];
            let n = tokio::time::timeout(Duration::from_secs(2), s3.read(&mut eof_buf))
                .await
                .expect("client should close socket promptly on barge-in")
                .unwrap_or(0);
            assert_eq!(n, 0);
        });

        let worker = AudioCppWorker::from_loopback_url(
            format!("http://{addr}"),
            PathBuf::from("/tmp/fake.gguf"),
            PathBuf::from("/tmp/voices"),
        )
        .unwrap();

        let state = Arc::new(Mutex::new(Some(AudioCppVoiceRef {
            voice_name: "ref_1".into(),
            transcript: "让人类永远保持理智，确实是一种奢求。".into(),
        })));
        let state_for_tts = state.clone();
        let tts = AudioCppTts {
            worker,
            resolve_ref: Arc::new(move || {
                state_for_tts.lock().unwrap().clone().ok_or_else(|| {
                    ProviderError::with_category(DiagCategory::Config, "未上传参考录音")
                })
            }),
        };

        // 1. ICL mode
        let pcm1 = tts.synthesize("你好，我是莫斯。").await.unwrap();
        assert_eq!(pcm1.sample_rate, 24_000);
        assert_eq!(pcm1.samples, vec![1200, -1200, 2400]);

        // 2. Hot-switched to empty transcript -> x_vector_only_mode
        *state.lock().unwrap() = Some(AudioCppVoiceRef {
            voice_name: "ref_2".into(),
            transcript: String::new(),
        });
        let pcm2 = tts.synthesize("声纹模式测试。").await.unwrap();
        assert_eq!(pcm2.samples, vec![500]);

        // 3. Barge-in cancellation
        let (tx, mut rx) = unbounded_channel();
        let stream_task = tokio::spawn(async move {
            tts.synthesize_stream("这是一段会被用户插话打断的长文本。", tx)
                .await
        });
        let first = rx.recv().await.unwrap();
        assert_eq!(first.samples, vec![500]);
        drop(rx);
        let res = tokio::time::timeout(Duration::from_secs(2), stream_task)
            .await
            .expect("synthesize_stream should exit immediately after receiver drop")
            .unwrap();
        assert!(res.is_ok());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn missing_reference_returns_config_error_without_touching_network() {
        let worker = AudioCppWorker::from_loopback_url(
            "http://127.0.0.1:9".into(),
            PathBuf::from("/tmp/fake.gguf"),
            PathBuf::from("/tmp/voices"),
        )
        .unwrap();
        let tts = AudioCppTts {
            worker,
            resolve_ref: Arc::new(|| {
                Err(ProviderError::with_category(
                    DiagCategory::Config,
                    "尚未上传本地声音克隆参考录音",
                ))
            }),
        };
        let err = tts.synthesize("测试").await.unwrap_err();
        assert_eq!(err.category, DiagCategory::Config);
        assert!(err.to_string().contains("尚未上传"));
    }

    #[test]
    fn select_preferred_device_index_prefers_discrete_gpu_over_igpu() {
        // 真实双显卡笔记本（Intel 核显共享内存 8.39 GB > RTX 4060 独显 8.34 GB）：必须选 index=1 独显而非 index=0 核显
        let dual_gpu_json = r#"{"schema_version":1,"devices":[
            {"backend":"vulkan","registry":"Vulkan","index":0,"name":"Vulkan0","description":"Intel(R) RaptorLake-S Mobile Graphics Controller","type":"igpu","pci_bus_id":null,"memory":{"free_bytes":7702179943,"total_bytes":8390121472}},
            {"backend":"vulkan","registry":"Vulkan","index":1,"name":"Vulkan1","description":"NVIDIA GeForce RTX 4060 Laptop GPU","type":"gpu","pci_bus_id":"0000:01:00.0","memory":{"free_bytes":7537164288,"total_bytes":8342470656}},
            {"backend":"cpu","registry":"CPU","index":0,"name":"CPU","description":"13th Gen Intel(R) Core(TM) i7-13650HX","type":"cpu","pci_bus_id":null,"memory":{"free_bytes":5064302592,"total_bytes":16780242944}}
        ]}"#;
        assert_eq!(select_preferred_device_index(dual_gpu_json, "vulkan"), 1);
        assert_eq!(select_preferred_device_index("invalid", "vulkan"), 0);
    }
}
