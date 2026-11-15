//! `speech.cpp`（Qwen3-TTS 0.6B CustomVoice）常驻 worker 客户端（JSON Lines over stdin/stdout）。
//!
//! - MOSS 懒加载 `speech worker <gguf>` 常驻子进程，避免每次合成重新加载 1.2 GB 权重；
//! - 输出严格按 24 kHz i16le 单声道 base64 `chunk` 流式推送；打断或取消时立即下发
//!   `cancel` 并抽空终态消息以保持管道对齐，异常时直接终止子进程避免半帧错位；
//! - Qwen3-TTS 不支持 `speed` 参数（`speech.cpp v0.10.1` 传入 `speed` 会报错拒绝），请求体严禁附带 `speed`。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::{BoxFuture, DiagCategory, Pcm, ProviderError, Tts, scrub_diagnostic_text};

pub const SPEECH_CPP_SAMPLE_RATE: u32 = 24_000;
const READY_TIMEOUT: Duration = Duration::from_secs(90);
const SYNTHESIZE_TIMEOUT: Duration = Duration::from_secs(60);
const CANCEL_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const STDERR_TAIL_LIMIT: usize = 24;

#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    pub struct JobHandle(HANDLE);

    unsafe impl Send for JobHandle {}

    impl JobHandle {
        pub fn for_child(child: &std::process::Child) -> Option<Self> {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job == 0 as HANDLE {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let configured = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if configured == 0
                    || AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0
                {
                    CloseHandle(job);
                    return None;
                }
                Some(Self(job))
            }
        }
    }

    impl Drop for JobHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod job {
    pub struct JobHandle;

    impl JobHandle {
        pub fn for_child(_child: &std::process::Child) -> Option<Self> {
            None
        }
    }
}

use job::JobHandle;

struct WorkerIo {
    writer: Box<dyn Write + Send>,
    rx: Receiver<Result<Value, String>>,
}

struct WorkerInner {
    model_path: PathBuf,
    alive: AtomicBool,
    next_id: AtomicU64,
    io: Mutex<Option<WorkerIo>>,
    child: Mutex<Option<Child>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    _job: Option<JobHandle>,
}

impl WorkerInner {
    fn kill_child(&self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut slot) = self.child.lock()
            && let Some(mut child) = slot.take()
        {
            let _ = child.kill();
            let _ = child.wait();
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
        self.kill_child();
    }
}

fn model_err(message: impl Into<String>) -> ProviderError {
    ProviderError::with_category(DiagCategory::Model, message)
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

fn extract_worker_error(msg: &Value, fallback: &str) -> String {
    msg.get("error")
        .and_then(|err| err.get("message").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(scrub_diagnostic_text)
        .unwrap_or_else(|| fallback.to_owned())
}

fn decode_pcm_base64(encoded: &str) -> Result<Pcm, ProviderError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| model_err("Qwen3-TTS 返回了无效的音频编码"))?;
    if !bytes.len().is_multiple_of(2) {
        return Err(model_err("Qwen3-TTS 返回的 PCM 字节长度无效"));
    }
    let samples = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| i16::from_le_bytes(pair))
        .collect();
    Ok(Pcm {
        samples,
        sample_rate: SPEECH_CPP_SAMPLE_RATE,
    })
}

/// 运行中的 `speech worker <model.gguf>` 子进程句柄；克隆共享同一进程。
#[derive(Clone)]
pub struct SpeechCppWorker {
    inner: Arc<WorkerInner>,
}

enum ChunkSink<'a> {
    Collect(&'a mut Vec<i16>),
    Stream(&'a UnboundedSender<Pcm>),
}

impl SpeechCppWorker {
    /// 启动 `speech worker <model_path>` 并等待 `"ready"` 握手完成。
    pub fn spawn(exe: &Path, model_path: &Path) -> Result<Self, ProviderError> {
        if !exe.is_file() {
            return Err(model_err(
                "未找到 speech.cpp 推理程序，请先下载 Qwen3-TTS 模型",
            ));
        }
        if !model_path.is_file() {
            return Err(model_err("Qwen3-TTS 模型文件缺失，请重新下载"));
        }
        let mut cmd = Command::new(exe);
        cmd.arg("worker")
            .arg(model_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        prepend_library_search_path(&mut cmd, exe);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|err| model_err(format!("启动 Qwen3-TTS 推理进程失败：{err}")))?;
        let job = JobHandle::for_child(&child);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| model_err("Qwen3-TTS 推理进程缺少标准输入"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| model_err("Qwen3-TTS 推理进程缺少标准输出"))?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LIMIT)));
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            std::thread::Builder::new()
                .name("speech-cpp-stderr".into())
                .spawn(move || {
                    let mut reader = BufReader::new(stderr);
                    let mut line = String::new();
                    while let Ok(read) = reader.read_line(&mut line) {
                        if read == 0 {
                            break;
                        }
                        let trimmed = line.trim();
                        if !trimmed.is_empty()
                            && let Ok(mut buf) = tail.lock()
                        {
                            if buf.len() >= STDERR_TAIL_LIMIT {
                                buf.pop_front();
                            }
                            buf.push_back(trimmed.to_owned());
                        }
                        line.clear();
                    }
                })
                .ok();
        }
        let worker = Self::from_streams_internal(
            Box::new(stdout),
            Box::new(stdin),
            model_path.to_path_buf(),
            Some(child),
            job,
            stderr_tail,
        )?;
        Ok(worker)
    }

    /// 用自定义输入/输出流建立 worker 客户端并完成 `"ready"` 握手（单测使用）。
    pub fn from_streams(
        reader: Box<dyn std::io::Read + Send>,
        writer: Box<dyn Write + Send>,
        model_path: PathBuf,
    ) -> Result<Self, ProviderError> {
        Self::from_streams_internal(
            reader,
            writer,
            model_path,
            None,
            None,
            Arc::new(Mutex::new(VecDeque::new())),
        )
    }

    fn from_streams_internal(
        reader: Box<dyn std::io::Read + Send>,
        writer: Box<dyn Write + Send>,
        model_path: PathBuf,
        child: Option<Child>,
        job: Option<JobHandle>,
        stderr_tail: Arc<Mutex<VecDeque<String>>>,
    ) -> Result<Self, ProviderError> {
        let (tx, rx) = channel::<Result<Value, String>>();
        std::thread::Builder::new()
            .name("speech-cpp-stdout".into())
            .spawn(move || {
                let mut reader = BufReader::new(reader);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => {
                            let _ = tx.send(Err("Qwen3-TTS 推理进程已退出".to_owned()));
                            return;
                        }
                        Ok(_) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<Value>(trimmed) {
                                Ok(val) => {
                                    if tx.send(Ok(val)).is_err() {
                                        return;
                                    }
                                }
                                Err(_) => {
                                    let _ = tx.send(Err("Qwen3-TTS 输出了无效的协议行".to_owned()));
                                    return;
                                }
                            }
                        }
                        Err(_) => {
                            let _ = tx.send(Err("读取 Qwen3-TTS 输出失败".to_owned()));
                            return;
                        }
                    }
                }
            })
            .map_err(|err| model_err(format!("创建 Qwen3-TTS 读线程失败：{err}")))?;

        let ready = rx.recv_timeout(READY_TIMEOUT);
        let worker = Self {
            inner: Arc::new(WorkerInner {
                model_path,
                alive: AtomicBool::new(true),
                next_id: AtomicU64::new(1),
                io: Mutex::new(Some(WorkerIo { writer, rx })),
                child: Mutex::new(child),
                stderr_tail,
                _job: job,
            }),
        };

        match ready {
            Ok(Ok(msg)) => match msg.get("type").and_then(Value::as_str) {
                Some("ready") => Ok(worker),
                Some("fatal") => {
                    let detail = extract_worker_error(&msg, "模型加载失败");
                    worker.terminate();
                    Err(model_err(format!("Qwen3-TTS 启动失败：{detail}")))
                }
                other => {
                    worker.terminate();
                    Err(model_err(format!(
                        "Qwen3-TTS 握手响应异常（{}）",
                        other.unwrap_or("unknown")
                    )))
                }
            },
            Ok(Err(reason)) => {
                let hint = worker.inner.stderr_hint();
                worker.terminate();
                Err(if hint.is_empty() {
                    model_err(reason)
                } else {
                    model_err(reason).detail(hint)
                })
            }
            Err(RecvTimeoutError::Timeout) => {
                let hint = worker.inner.stderr_hint();
                worker.terminate();
                let err = model_err("Qwen3-TTS 模型加载超时，请确认显存/内存充足后重试");
                Err(if hint.is_empty() {
                    err
                } else {
                    err.detail(hint)
                })
            }
            Err(RecvTimeoutError::Disconnected) => {
                let hint = worker.inner.stderr_hint();
                worker.terminate();
                let err = model_err("Qwen3-TTS 推理进程启动时提前退出");
                Err(if hint.is_empty() {
                    err
                } else {
                    err.detail(hint)
                })
            }
        }
    }

    pub fn model_path(&self) -> &Path {
        &self.inner.model_path
    }

    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    pub fn terminate(&self) {
        self.inner.kill_child();
        if let Ok(mut slot) = self.inner.io.lock() {
            *slot = None;
        }
    }

    pub fn synthesize_blocking(&self, text: &str, voice: &str) -> Result<Pcm, ProviderError> {
        let mut samples = Vec::new();
        self.run_synthesize(text, voice, ChunkSink::Collect(&mut samples))?;
        Ok(Pcm {
            samples,
            sample_rate: SPEECH_CPP_SAMPLE_RATE,
        })
    }

    pub fn synthesize_stream_blocking(
        &self,
        text: &str,
        voice: &str,
        chunks: UnboundedSender<Pcm>,
    ) -> Result<(), ProviderError> {
        self.run_synthesize(text, voice, ChunkSink::Stream(&chunks))
    }

    fn run_synthesize(
        &self,
        text: &str,
        voice: &str,
        mut sink: ChunkSink<'_>,
    ) -> Result<(), ProviderError> {
        if text.trim().is_empty() {
            return Ok(());
        }
        if !self.is_alive() {
            return Err(model_err("Qwen3-TTS 推理进程已退出"));
        }
        let mut io_guard = self
            .inner
            .io
            .lock()
            .map_err(|_| model_err("Qwen3-TTS 推理通道不可用"))?;
        let Some(io) = io_guard.as_mut() else {
            return Err(model_err("Qwen3-TTS 推理进程已退出"));
        };

        let id = self
            .inner
            .next_id
            .fetch_add(1, Ordering::SeqCst)
            .to_string();
        // speech.cpp v0.10.1 的 Qwen3-TTS 不支持 speed 字段，传入会被 worker 拒绝
        let req = json!({
            "type": "synthesize",
            "id": id,
            "text": text,
            "voice": voice,
        });
        let mut line = serde_json::to_string(&req)
            .map_err(|err| model_err(format!("序列化 Qwen3-TTS 请求失败：{err}")))?;
        line.push('\n');
        if let Err(err) = io
            .writer
            .write_all(line.as_bytes())
            .and_then(|()| io.writer.flush())
        {
            Self::drop_io_and_kill(io_guard, &self.inner);
            return Err(model_err(format!("向 Qwen3-TTS 发送请求失败：{err}")));
        }

        let deadline = Instant::now() + SYNTHESIZE_TIMEOUT;
        loop {
            if let ChunkSink::Stream(tx) = &sink
                && tx.is_closed()
            {
                if Self::send_cancel_and_drain(io, &id).is_err() {
                    Self::drop_io_and_kill(io_guard, &self.inner);
                }
                return Ok(());
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                Self::drop_io_and_kill(io_guard, &self.inner);
                return Err(model_err("Qwen3-TTS 合成超时"));
            }
            match io
                .rx
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(Ok(msg)) => {
                    match msg.get("id").and_then(Value::as_str) {
                        Some(got_id) if got_id != id => continue,
                        Some(_) => {}
                        None => {
                            let reason = if matches!(
                                msg.get("type").and_then(Value::as_str),
                                Some("fatal" | "error")
                            ) {
                                extract_worker_error(&msg, "Qwen3-TTS 推理进程异常")
                            } else {
                                "Qwen3-TTS 返回了缺少请求编号的协议消息".to_owned()
                            };
                            Self::drop_io_and_kill(io_guard, &self.inner);
                            return Err(model_err(reason));
                        }
                    }
                    match msg.get("type").and_then(Value::as_str) {
                        Some("chunk") => {
                            let decoded = msg
                                .get("pcm")
                                .and_then(Value::as_str)
                                .ok_or_else(|| model_err("Qwen3-TTS 缺少音频数据"))
                                .and_then(decode_pcm_base64);
                            let pcm = match decoded {
                                Ok(pcm) => pcm,
                                Err(err) => {
                                    Self::drop_io_and_kill(io_guard, &self.inner);
                                    return Err(err);
                                }
                            };
                            match &mut sink {
                                ChunkSink::Collect(buf) => buf.extend_from_slice(&pcm.samples),
                                ChunkSink::Stream(tx) => {
                                    if tx.send(pcm).is_err() {
                                        if Self::send_cancel_and_drain(io, &id).is_err() {
                                            Self::drop_io_and_kill(io_guard, &self.inner);
                                        }
                                        return Ok(());
                                    }
                                }
                            }
                        }
                        Some("progress") => continue,
                        Some("end") => break Ok(()),
                        Some("cancelled") => break Ok(()),
                        Some("error") => {
                            let reason = extract_worker_error(&msg, "Qwen3-TTS 合成失败");
                            break Err(model_err(reason));
                        }
                        other => {
                            Self::drop_io_and_kill(io_guard, &self.inner);
                            return Err(model_err(format!(
                                "Qwen3-TTS 返回了意外的协议消息（{}）",
                                other.unwrap_or("unknown")
                            )));
                        }
                    }
                }
                Ok(Err(reason)) => {
                    let hint = self.inner.stderr_hint();
                    Self::drop_io_and_kill(io_guard, &self.inner);
                    let err = model_err(reason);
                    return Err(if hint.is_empty() {
                        err
                    } else {
                        err.detail(hint)
                    });
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    Self::drop_io_and_kill(io_guard, &self.inner);
                    return Err(model_err("Qwen3-TTS 推理进程已退出"));
                }
            }
        }
    }

    fn drop_io_and_kill(
        mut io_guard: std::sync::MutexGuard<'_, Option<WorkerIo>>,
        inner: &WorkerInner,
    ) {
        *io_guard = None;
        drop(io_guard);
        inner.kill_child();
    }

    fn send_cancel_and_drain(io: &mut WorkerIo, id: &str) -> Result<(), ()> {
        let cancel_line = format!("{}\n", json!({"type": "cancel", "id": id}));
        if io
            .writer
            .write_all(cancel_line.as_bytes())
            .and_then(|()| io.writer.flush())
            .is_err()
        {
            return Err(());
        }
        let deadline = Instant::now() + CANCEL_DRAIN_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(());
            }
            match io.rx.recv_timeout(remaining) {
                Ok(Ok(msg)) => {
                    match msg.get("id").and_then(Value::as_str) {
                        Some(got_id) if got_id == id => {}
                        Some(_) => continue,
                        None => return Err(()),
                    }
                    if matches!(
                        msg.get("type").and_then(Value::as_str),
                        Some("end" | "error" | "cancelled")
                    ) {
                        return Ok(());
                    }
                }
                _ => return Err(()),
            }
        }
    }
}

/// 基于 `speech.cpp`（Qwen3-TTS 0.6B CustomVoice）的本地语音合成提供方。
pub struct SpeechCppTts {
    pub worker: SpeechCppWorker,
    pub voice: String,
}

impl Tts for SpeechCppTts {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        let worker = self.worker.clone();
        let text = text.to_owned();
        let voice = self.voice.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || worker.synthesize_blocking(&text, &voice))
                .await
                .map_err(|_| model_err("Qwen3-TTS 合成任务异常结束"))?
        })
    }

    fn synthesize_stream<'a>(
        &'a self,
        text: &'a str,
        chunks: UnboundedSender<Pcm>,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        let worker = self.worker.clone();
        let text = text.to_owned();
        let voice = self.voice.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                worker.synthesize_stream_blocking(&text, &voice, chunks)
            })
            .await
            .map_err(|_| model_err("Qwen3-TTS 流式合成任务异常结束"))?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::sync::mpsc;

    fn memory_pipe() -> (ChannelReader, ChannelWriter) {
        let (tx, rx) = mpsc::channel();
        (
            ChannelReader {
                rx,
                buf: VecDeque::new(),
            },
            ChannelWriter { tx },
        )
    }

    struct ChannelReader {
        rx: Receiver<Vec<u8>>,
        buf: VecDeque<u8>,
    }

    impl std::io::Read for ChannelReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            while self.buf.is_empty() {
                match self.rx.recv() {
                    Ok(chunk) => self.buf.extend(chunk),
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.buf.len());
            for slot in out.iter_mut().take(n) {
                *slot = self.buf.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    struct ChannelWriter {
        tx: mpsc::Sender<Vec<u8>>,
    }

    impl Write for ChannelWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.tx
                .send(buf.to_vec())
                .map_err(|_| std::io::Error::new(ErrorKind::BrokenPipe, "closed"))?;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn encode_samples(samples: &[i16]) -> String {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn worker_streams_24khz_pcm_and_omits_speed_field() {
        let (client_in_r, mut server_out_w) = memory_pipe();
        let (mut server_in_r, client_out_w) = memory_pipe();

        let server = std::thread::spawn(move || {
            writeln!(
                server_out_w,
                "{}",
                json!({"type": "ready", "protocol": 4, "version": "0.10.1"})
            )
            .unwrap();
            let mut reader = BufReader::new(&mut server_in_r);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(req["type"], "synthesize");
            assert_eq!(req["voice"], "vivian");
            assert_eq!(req["text"], "你好，工作台已就绪");
            assert!(
                req.get("speed").is_none(),
                "Qwen3-TTS 不得传入 speed 字段：{req}"
            );
            let id = req["id"].as_str().unwrap();
            writeln!(
                server_out_w,
                "{}",
                json!({"type": "progress", "id": id, "done": 0.5})
            )
            .unwrap();
            writeln!(
                server_out_w,
                "{}",
                json!({"type": "chunk", "id": id, "seq": 0, "pcm": encode_samples(&[100, -200])})
            )
            .unwrap();
            writeln!(
                server_out_w,
                "{}",
                json!({"type": "chunk", "id": id, "seq": 1, "pcm": encode_samples(&[300])})
            )
            .unwrap();
            writeln!(
                server_out_w,
                "{}",
                json!({"type": "end", "id": id, "samples": 3, "stop": "complete"})
            )
            .unwrap();
        });

        let worker = SpeechCppWorker::from_streams(
            Box::new(client_in_r),
            Box::new(client_out_w),
            PathBuf::from("qwen3.gguf"),
        )
        .unwrap();
        assert!(worker.is_alive());

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let tts = SpeechCppTts {
            worker: worker.clone(),
            voice: "vivian".into(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        rt.block_on(tts.synthesize_stream("你好，工作台已就绪", tx))
            .unwrap();
        let mut received = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            assert_eq!(chunk.sample_rate, 24_000);
            received.extend(chunk.samples);
        }
        assert_eq!(received, vec![100, -200, 300]);
        server.join().unwrap();
    }

    #[test]
    fn worker_cancels_and_drains_on_dropped_receiver_without_losing_alignment() {
        let (client_in_r, mut server_out_w) = memory_pipe();
        let (mut server_in_r, client_out_w) = memory_pipe();

        let server = std::thread::spawn(move || {
            writeln!(server_out_w, "{}", json!({"type": "ready", "protocol": 4})).unwrap();
            let mut reader = BufReader::new(&mut server_in_r);

            let mut line1 = String::new();
            reader.read_line(&mut line1).unwrap();
            let req1: Value = serde_json::from_str(&line1).unwrap();
            let id1 = req1["id"].as_str().unwrap().to_owned();
            writeln!(
                server_out_w,
                "{}",
                json!({"type": "chunk", "id": id1, "seq": 0, "pcm": encode_samples(&[11, 22])})
            )
            .unwrap();

            // 客户端放弃接收后应收到 cancel，并回传 cancelled 后才能对齐下一句
            let mut line2 = String::new();
            reader.read_line(&mut line2).unwrap();
            let req2: Value = serde_json::from_str(&line2).unwrap();
            assert_eq!(req2["type"], "cancel");
            assert_eq!(req2["id"], id1);
            writeln!(server_out_w, "{}", json!({"type": "cancelled", "id": id1})).unwrap();

            let mut line3 = String::new();
            reader.read_line(&mut line3).unwrap();
            let req3: Value = serde_json::from_str(&line3).unwrap();
            assert_eq!(req3["type"], "synthesize");
            let id3 = req3["id"].as_str().unwrap().to_owned();
            writeln!(
                server_out_w,
                "{}",
                json!({"type": "chunk", "id": id3, "seq": 0, "pcm": encode_samples(&[777])})
            )
            .unwrap();
            writeln!(server_out_w, "{}", json!({"type": "end", "id": id3})).unwrap();
        });

        let worker = SpeechCppWorker::from_streams(
            Box::new(client_in_r),
            Box::new(client_out_w),
            PathBuf::from("qwen3.gguf"),
        )
        .unwrap();

        let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
        drop(rx1);
        worker
            .synthesize_stream_blocking("被打断的句子", "vivian", tx1)
            .unwrap();

        let next = worker.synthesize_blocking("恢复下一句", "serena").unwrap();
        assert_eq!(
            next,
            Pcm {
                samples: vec![777],
                sample_rate: 24_000,
            }
        );
        server.join().unwrap();
    }

    #[test]
    fn worker_reports_fatal_handshake_and_synthesis_errors() {
        let (client_in_r, mut server_out_w) = memory_pipe();
        let (_server_in_r, client_out_w) = memory_pipe();
        writeln!(
            server_out_w,
            "{}",
            json!({"type": "fatal", "error": {"code": "model_file", "message": "bad gguf"}})
        )
        .unwrap();
        let err = SpeechCppWorker::from_streams(
            Box::new(client_in_r),
            Box::new(client_out_w),
            PathBuf::from("bad.gguf"),
        )
        .err()
        .expect("fatal handshake must error");
        assert!(err.message.contains("bad gguf"), "{}", err.message);

        let (client_in_r, mut server_out_w) = memory_pipe();
        let (mut server_in_r, client_out_w) = memory_pipe();
        let server = std::thread::spawn(move || {
            writeln!(server_out_w, "{}", json!({"type": "ready", "protocol": 4})).unwrap();
            let mut line = String::new();
            BufReader::new(&mut server_in_r)
                .read_line(&mut line)
                .unwrap();
            let req: Value = serde_json::from_str(&line).unwrap();
            writeln!(
                server_out_w,
                "{}",
                json!({
                    "type": "error",
                    "id": req["id"],
                    "error": {"code": "out_of_range", "option": "voice", "message": "unknown voice"}
                })
            )
            .unwrap();
        });
        let worker = SpeechCppWorker::from_streams(
            Box::new(client_in_r),
            Box::new(client_out_w),
            PathBuf::from("qwen3.gguf"),
        )
        .unwrap();
        let err = worker.synthesize_blocking("hi", "invalid").unwrap_err();
        assert!(err.message.contains("unknown voice"), "{}", err.message);
        server.join().unwrap();
    }

    #[test]
    fn malformed_chunk_or_missing_id_kills_worker_and_clears_channel() {
        for bad_frame in [
            json!({"type": "chunk", "seq": 0}),
            json!({"type": "chunk", "id": "REQ_ID", "seq": 0}),
            json!({"type": "chunk", "id": "REQ_ID", "seq": 0, "pcm": "%%%"}),
        ] {
            let (client_in_r, mut server_out_w) = memory_pipe();
            let (mut server_in_r, client_out_w) = memory_pipe();
            let server = std::thread::spawn(move || {
                writeln!(server_out_w, "{}", json!({"type": "ready", "protocol": 4})).unwrap();
                let mut line = String::new();
                BufReader::new(&mut server_in_r)
                    .read_line(&mut line)
                    .unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                let mut frame = bad_frame;
                if frame.get("id").and_then(Value::as_str) == Some("REQ_ID") {
                    frame["id"] = req["id"].clone();
                }
                writeln!(server_out_w, "{frame}").unwrap();
            });
            let worker = SpeechCppWorker::from_streams(
                Box::new(client_in_r),
                Box::new(client_out_w),
                PathBuf::from("qwen3.gguf"),
            )
            .unwrap();
            assert!(worker.is_alive());
            let err = worker.synthesize_blocking("hi", "vivian").unwrap_err();
            assert!(!err.message.is_empty());
            assert!(!worker.is_alive(), "损坏协议帧必须结束 worker 并清空通道");
            let second_err = worker.synthesize_blocking("hi", "vivian").unwrap_err();
            assert!(
                second_err.message.contains("已停止") || second_err.message.contains("已退出"),
                "{}",
                second_err.message
            );
            server.join().unwrap();
        }
    }
}
