//! 本地模型辅助程序客户端（内置语音技术书 §11.2，NV6）。
//!
//! MOSS（MIT）不链接 sherpa-onnx：本地判停、识别、合成由独立程序 `moss-voice-local`（GPL-3.0）完成，
//! 这里只负责启动它、按 `moss-voice-local-proto` 收发帧。请求带 id，由读线程按 id 把响应交回等待方，
//! 所以识别、合成、判停可以同时进行。辅助程序退出或崩溃时，所有等待中的请求立即失败，MOSS 主进程与急停不受影响。
//!
//! [`LocalAsr`] / [`LocalTts`] 实现 [`Asr`] / [`Tts`]，可直接替换云端提供方；[`LocalStreamingAsr`] 实现
//! [`StreamingAsr`]（流式识别模型：边说边识别）；判停的同步接口供语音服务的 `Vad` 适配器调用。

use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, SyncSender, TrySendError, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use moss_voice_local_proto::{
    AsrDecodeOptions, AsrEngine, TtsEngine, VadEngine, VadEventKind, VadParams,
};
use moss_voice_local_proto::{
    Op, PROTOCOL_VERSION, Reply, Request, Response, bytes_to_pcm, pcm_to_bytes, read_frame,
    write_frame,
};

use crate::{Asr, AsrStream, BoxFuture, Pcm, ProviderError, StreamingAsr, Tts};

/// 辅助程序可执行文件名（与 MOSS 放在同一目录）
#[cfg(windows)]
pub const HELPER_FILE: &str = "moss-voice-local.exe";
#[cfg(not(windows))]
pub const HELPER_FILE: &str = "moss-voice-local";

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// 加载模型（SenseVoice 约 1 秒，Kokoro 约 1.5 秒；慢电脑留足余量）
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(30);
const SYNTHESIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// 判停每帧的往返上限：超过即视为辅助程序异常，调用方退回能量判停
const VAD_TIMEOUT: Duration = Duration::from_millis(500);
/// 流式识别打开 / 送入一段音频（含解码）的往返上限
const STREAM_TIMEOUT: Duration = Duration::from_secs(5);

type Pending = Sender<(Reply, Vec<u8>)>;

struct Inner {
    writer_tx: SyncSender<(Request, Vec<u8>)>,
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
    /// 流式识别的流编号（辅助程序内识别与判停各有自己的编号空间）
    next_stream: AtomicU64,
    alive: AtomicBool,
    child: Mutex<Option<Child>>,
}

impl Inner {
    fn invalidate(&self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.kill();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 运行中的辅助程序。克隆共享同一个进程；最后一个句柄丢弃时结束进程。
#[derive(Clone)]
pub struct LocalHelper {
    inner: Arc<Inner>,
}

/// 声纹专用进程，不暴露共享的 ASR/TTS/VAD 句柄。超时只结束本进程。
#[derive(Clone)]
pub struct SpeakerHelper {
    helper: LocalHelper,
}
impl SpeakerHelper {
    pub fn spawn(exe: &Path, models_root: &Path) -> Result<Self, ProviderError> {
        let helper = LocalHelper::spawn(exe)?;
        helper.load_speaker(models_root)?;
        Ok(Self { helper })
    }
    pub fn embedding_blocking(
        &self,
        pcm: &[i16],
        sample_rate: u32,
        generation: u64,
    ) -> Result<Vec<f32>, ProviderError> {
        self.helper
            .speaker_embedding_blocking(pcm, sample_rate, generation)
    }
    pub fn is_alive(&self) -> bool {
        self.helper.is_alive()
    }
}

/// 辅助程序加载的合成模型信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TtsInfo {
    pub sample_rate: u32,
    pub speakers: u32,
}

fn dead() -> ProviderError {
    ProviderError::new("本地模型程序已退出")
}

impl LocalHelper {
    /// 启动辅助程序并握手（核对协议版本）。
    pub fn spawn(exe: &Path) -> Result<Self, ProviderError> {
        if !exe.is_file() {
            return Err(ProviderError::new(format!(
                "未找到本地模型程序 {}（应与 MOSS 放在同一目录）",
                exe.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            )));
        }
        let mut command = Command::new(exe);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW：不弹出控制台窗口
            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .spawn()
            .map_err(|error| ProviderError::new(format!("本地模型程序启动失败：{error}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::new("本地模型程序启动失败：没有标准输入"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::new("本地模型程序启动失败：没有标准输出"))?;
        let helper = Self::from_streams(Box::new(stdout), Box::new(stdin));
        if let Ok(mut slot) = helper.inner.child.lock() {
            *slot = Some(child);
        }
        helper.hello()?;
        Ok(helper)
    }

    /// 用给定的读写流建立连接（测试用假辅助程序时也走这里）。不握手。
    pub fn from_streams(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self {
        let (writer_tx, writer_rx) = sync_channel::<(Request, Vec<u8>)>(16);
        let inner = Arc::new(Inner {
            writer_tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            next_stream: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            child: Mutex::new(None),
        });
        let weak_writer = Arc::downgrade(&inner);
        let _ = std::thread::Builder::new()
            .name("moss-voice-local-writer".into())
            .spawn(move || {
                let mut writer = BufWriter::new(writer);
                while let Ok((request, payload)) = writer_rx.recv() {
                    if write_frame(&mut writer, &request, &payload).is_err() {
                        if let Some(inner) = weak_writer.upgrade() {
                            inner.invalidate();
                        }
                        return;
                    }
                }
            });
        let weak = Arc::downgrade(&inner);
        let _ = std::thread::Builder::new()
            .name("moss-voice-local-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(reader);
                loop {
                    let frame: Option<(Response, Vec<u8>)> = read_frame(&mut reader).ok().flatten();
                    let Some(inner) = weak.upgrade() else { return };
                    match frame {
                        Some((Response { id, reply }, payload)) => {
                            let waiter = inner.pending.lock().ok().and_then(|mut pending| {
                                if matches!(reply, Reply::AudioChunk { .. }) {
                                    pending.get(&id).cloned()
                                } else {
                                    pending.remove(&id)
                                }
                            });
                            if let Some(waiter) = waiter {
                                let _ = waiter.send((reply, payload));
                            }
                        }
                        None => {
                            // 辅助程序退出或输出损坏：丢弃全部等待者，它们立即收到错误
                            inner.alive.store(false, Ordering::SeqCst);
                            if let Ok(mut pending) = inner.pending.lock() {
                                pending.clear();
                            }
                            return;
                        }
                    }
                }
            });
        Self { inner }
    }

    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    fn enqueue(&self, id: u64, op: Op, payload: &[u8]) -> Result<(), ProviderError> {
        if !self.is_alive() {
            return Err(dead());
        }
        match self
            .inner
            .writer_tx
            .try_send((Request { id, op }, payload.to_vec()))
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.inner.invalidate();
                Err(ProviderError::new("本地模型写入队列已满，辅助程序不可用"))
            }
            Err(TrySendError::Disconnected(_)) => {
                self.inner.invalidate();
                Err(dead())
            }
        }
    }

    fn call(
        &self,
        op: Op,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(Reply, Vec<u8>), ProviderError> {
        if !self.is_alive() {
            return Err(dead());
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = channel();
        self.inner
            .pending
            .lock()
            .map_err(|_| dead())?
            .insert(id, sender);
        let deadline = std::time::Instant::now() + timeout;
        if let Err(error) = self.enqueue(id, op, payload) {
            self.forget(id);
            return Err(error);
        }
        let result =
            receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
        match result {
            Ok((Reply::Error { message }, _)) => Err(ProviderError::new(message)),
            Ok(reply) => Ok(reply),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.forget(id);
                self.inner.invalidate();
                Err(ProviderError::new("本地模型处理或管道写入超时"))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(dead()),
        }
    }
    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.inner.pending.lock() {
            pending.remove(&id);
        }
    }

    fn hello(&self) -> Result<(), ProviderError> {
        match self.call(Op::Hello, &[], HELLO_TIMEOUT)?.0 {
            Reply::Hello { protocol, .. } if protocol == PROTOCOL_VERSION => Ok(()),
            Reply::Hello { protocol, .. } => Err(ProviderError::new(format!(
                "本地模型程序版本不匹配（协议 {protocol}，MOSS 需要 {PROTOCOL_VERSION}），请重新安装"
            ))),
            _ => Err(ProviderError::new("本地模型程序握手失败")),
        }
    }

    fn expect_loaded(reply: Reply) -> Result<(Option<u32>, Option<u32>), ProviderError> {
        match reply {
            Reply::Loaded {
                sample_rate,
                speakers,
            } => Ok((sample_rate, speakers)),
            _ => Err(ProviderError::new("本地模型程序返回了意外的结果")),
        }
    }

    pub fn load_vad(
        &self,
        model: &Path,
        params: VadParams,
        engine: VadEngine,
    ) -> Result<(), ProviderError> {
        let op = Op::LoadVad {
            model: model.to_string_lossy().into_owned(),
            params,
            engine,
        };
        Self::expect_loaded(self.call(op, &[], LOAD_TIMEOUT)?.0).map(|_| ())
    }

    pub fn load_asr(&self, dir: &Path, engine: AsrEngine) -> Result<(), ProviderError> {
        self.load_asr_with_options(
            dir,
            engine,
            moss_voice_local_proto::AsrDecodeOptions::default(),
        )
    }
    pub fn load_asr_with_options(
        &self,
        dir: &Path,
        engine: AsrEngine,
        options: moss_voice_local_proto::AsrDecodeOptions,
    ) -> Result<(), ProviderError> {
        let op = Op::LoadAsr {
            dir: dir.to_string_lossy().into_owned(),
            engine,
            options,
        };
        Self::expect_loaded(self.call(op, &[], LOAD_TIMEOUT)?.0).map(|_| ())
    }

    pub fn load_tts(&self, dir: &Path, engine: TtsEngine) -> Result<TtsInfo, ProviderError> {
        let op = Op::LoadTts {
            dir: dir.to_string_lossy().into_owned(),
            engine,
        };
        let (sample_rate, speakers) = Self::expect_loaded(self.call(op, &[], LOAD_TIMEOUT)?.0)?;
        Ok(TtsInfo {
            sample_rate: sample_rate.unwrap_or(24_000),
            speakers: speakers.unwrap_or(0),
        })
    }

    fn load_speaker(&self, models_root: &Path) -> Result<(), ProviderError> {
        use moss_voice_local_proto::{SPEAKER_DIMENSION, SPEAKER_MODEL_ID, SPEAKER_SAMPLE_RATE};
        let op = Op::LoadSpeaker {
            model_id: SPEAKER_MODEL_ID.into(),
            models_root: models_root.to_string_lossy().into_owned(),
        };
        let (reply, payload) = self.call(op, &[], LOAD_TIMEOUT)?;
        match reply {
            Reply::SpeakerLoaded {
                model_id,
                sample_rate,
                dimension,
            } if model_id == SPEAKER_MODEL_ID
                && sample_rate == SPEAKER_SAMPLE_RATE
                && dimension == SPEAKER_DIMENSION
                && payload.is_empty() =>
            {
                Ok(())
            }
            _ => Err(ProviderError::new("声纹模型协议或维度不匹配")),
        }
    }
    /// Use a dedicated helper for speaker work: its 1.5s timeout invalidates that
    /// process and must never terminate the ASR/TTS/VAD helper or delay stop.
    fn speaker_embedding_blocking(
        &self,
        pcm: &[i16],
        sample_rate: u32,
        generation: u64,
    ) -> Result<Vec<f32>, ProviderError> {
        use moss_voice_local_proto::{
            SPEAKER_DIMENSION, SPEAKER_MODEL_ID, SPEAKER_SAMPLE_RATE, bytes_to_embedding,
            valid_speaker_pcm,
        };
        if !valid_speaker_pcm(sample_rate, pcm.len().saturating_mul(2)) {
            return Err(ProviderError::new("声纹采样必须为2～10秒16kHz单声道PCM"));
        }
        let op = Op::SpeakerEmbed {
            model_id: SPEAKER_MODEL_ID.into(),
            sample_rate,
            utterance_generation: generation,
        };
        let (reply, bytes) = self.call(op, &pcm_to_bytes(pcm), Duration::from_millis(1500))?;
        match reply {
            Reply::Embedding {
                model_id,
                sample_rate,
                dimension,
                utterance_generation,
                format,
            } if model_id == SPEAKER_MODEL_ID
                && sample_rate == SPEAKER_SAMPLE_RATE
                && dimension == SPEAKER_DIMENSION
                && utterance_generation == generation
                && format == "f32le" =>
            {
                bytes_to_embedding(&bytes).map_err(|_| ProviderError::new("声纹向量长度或数值无效"))
            }
            _ => Err(ProviderError::new("声纹响应模型、采样率或代次不匹配")),
        }
    }

    /// 同步识别（阻塞调用方线程）。
    pub fn transcribe_blocking(
        &self,
        pcm: &[i16],
        sample_rate: u32,
    ) -> Result<String, ProviderError> {
        match self
            .call(
                Op::Transcribe { sample_rate },
                &pcm_to_bytes(pcm),
                TRANSCRIBE_TIMEOUT,
            )?
            .0
        {
            Reply::Text { text } => Ok(text),
            _ => Err(ProviderError::new("本地识别返回了意外的结果")),
        }
    }

    /// 同步合成（阻塞调用方线程）。
    pub fn synthesize_blocking(
        &self,
        text: &str,
        speaker: i32,
        speed: f32,
    ) -> Result<Pcm, ProviderError> {
        let op = Op::Synthesize {
            text: text.to_owned(),
            speaker,
            speed,
        };
        match self.call(op, &[], SYNTHESIZE_TIMEOUT)? {
            (Reply::Audio { sample_rate }, payload) if sample_rate > 0 => Ok(Pcm {
                samples: bytes_to_pcm(&payload),
                sample_rate,
            }),
            _ => Err(ProviderError::new("本地合成返回了意外的结果")),
        }
    }

    fn synthesize_stream_blocking(
        &self,
        text: String,
        speaker: i32,
        speed: f32,
        chunks: tokio::sync::mpsc::UnboundedSender<Pcm>,
    ) -> Result<(), ProviderError> {
        if !self.is_alive() {
            return Err(dead());
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = channel();
        self.inner
            .pending
            .lock()
            .map_err(|_| dead())?
            .insert(id, sender);
        let op = Op::SynthesizeStream {
            text,
            speaker,
            speed,
        };
        if let Err(error) = self.enqueue(id, op, &[]) {
            self.forget(id);
            return Err(error);
        }
        let deadline = std::time::Instant::now() + SYNTHESIZE_TIMEOUT;
        let result = loop {
            if chunks.is_closed() {
                break Ok(());
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                self.inner.invalidate();
                break Err(ProviderError::new("本地合成处理超时"));
            }
            match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok((Reply::AudioChunk { sample_rate }, payload))
                    if sample_rate > 0 && payload.len() % 2 == 0 =>
                {
                    if chunks
                        .send(Pcm {
                            sample_rate,
                            samples: bytes_to_pcm(&payload),
                        })
                        .is_err()
                    {
                        break Ok(());
                    }
                }
                Ok((Reply::Done, _)) => break Ok(()),
                Ok((Reply::Error { message }, _)) => break Err(ProviderError::new(message)),
                Ok(_) => break Err(ProviderError::new("本地流式合成返回了意外的结果")),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Err(dead()),
            }
        };
        self.forget(id);
        result
    }

    /// 新的流式识别流编号。
    pub fn next_stream(&self) -> u64 {
        self.inner.next_stream.fetch_add(1, Ordering::SeqCst)
    }

    pub fn asr_open(&self, stream: u64) -> Result<(), ProviderError> {
        self.call(Op::AsrOpen { stream }, &[], STREAM_TIMEOUT)
            .map(|_| ())
    }

    pub fn asr_feed(&self, stream: u64, pcm: &[i16]) -> Result<(), ProviderError> {
        self.asr_feed_text(stream, pcm).map(|_| ())
    }

    pub fn asr_feed_text(&self, stream: u64, pcm: &[i16]) -> Result<Option<String>, ProviderError> {
        match self
            .call(Op::AsrFeed { stream }, &pcm_to_bytes(pcm), STREAM_TIMEOUT)?
            .0
        {
            Reply::Text { text } => Ok(Some(text)),
            Reply::Done => Ok(None),
            _ => Err(ProviderError::new("本地流式识别返回了意外的结果")),
        }
    }

    /// 说完：取最终文字（辅助程序随即关闭该路）。
    pub fn asr_finish(&self, stream: u64) -> Result<String, ProviderError> {
        match self
            .call(Op::AsrFinish { stream }, &[], TRANSCRIBE_TIMEOUT)?
            .0
        {
            Reply::Text { text } => Ok(text),
            _ => Err(ProviderError::new("本地流式识别返回了意外的结果")),
        }
    }

    /// 放弃一路识别（尽力而为，不等待结果）。
    pub fn asr_close(&self, stream: u64) {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let _ = self.enqueue(id, Op::AsrClose { stream }, &[]);
    }

    pub fn vad_open(&self, stream: u64) -> Result<(), ProviderError> {
        self.call(Op::VadOpen { stream }, &[], VAD_TIMEOUT)
            .map(|_| ())
    }

    pub fn vad_feed(&self, stream: u64, pcm: &[i16]) -> Result<Vec<VadEventKind>, ProviderError> {
        match self
            .call(Op::VadFeed { stream }, &pcm_to_bytes(pcm), VAD_TIMEOUT)?
            .0
        {
            Reply::VadEvents { events } => Ok(events),
            _ => Err(ProviderError::new("本地判停返回了意外的结果")),
        }
    }

    /// 关闭判停流（尽力而为，不等待结果）。
    pub fn vad_close(&self, stream: u64) {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let _ = self.enqueue(id, Op::VadClose { stream }, &[]);
    }
}

/// 本地识别（整段：SenseVoice；流式模型也可整段识别，用作流式失败时的退路）。
pub struct LocalAsr {
    pub helper: LocalHelper,
}

impl Asr for LocalAsr {
    fn transcribe<'a>(
        &'a self,
        pcm: &'a [i16],
        sample_rate: u32,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        let helper = self.helper.clone();
        let pcm = pcm.to_vec();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || helper.transcribe_blocking(&pcm, sample_rate))
                .await
                .map_err(|_| ProviderError::new("本地识别任务异常结束"))?
        })
    }
}

/// 本地流式识别（流式 Zipformer）：用户开始说话即打开一路，边说边送入并解码，说完只剩最后一小段。
pub struct LocalStreamingAsr {
    pub helper: LocalHelper,
}

/// 后台任务被中止（打断、重新拾音）或出错时关闭辅助程序里的这一路。
struct StreamGuard {
    helper: LocalHelper,
    stream: u64,
    finished: bool,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.helper.asr_close(self.stream);
        }
    }
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, ProviderError> + Send + 'static,
) -> Result<T, ProviderError> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|_| ProviderError::new("本地识别任务异常结束"))?
}

impl StreamingAsr for LocalStreamingAsr {
    fn start(&self) -> AsrStream {
        let (audio, mut chunks) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let (interim_tx, interim) = tokio::sync::watch::channel(String::new());
        let (result_tx, result) = tokio::sync::oneshot::channel();
        let helper = self.helper.clone();
        let task = tokio::spawn(async move {
            let stream = helper.next_stream();
            let mut guard = StreamGuard {
                helper: helper.clone(),
                stream,
                finished: false,
            };
            let outcome = async {
                let opener = helper.clone();
                blocking(move || opener.asr_open(stream)).await?;
                while let Some(mut pcm) = chunks.recv().await {
                    // 上一段还在解码时到达的音频合并成一次送入
                    while let Ok(more) = chunks.try_recv() {
                        pcm.extend_from_slice(&more);
                    }
                    let feeder = helper.clone();
                    if let Some(text) = blocking(move || feeder.asr_feed_text(stream, &pcm)).await?
                    {
                        interim_tx.send_if_modified(|current| {
                            if *current == text {
                                false
                            } else {
                                *current = text;
                                true
                            }
                        });
                    }
                }
                let finisher = helper.clone();
                let text = blocking(move || finisher.asr_finish(stream)).await;
                // 辅助程序收到结束请求即已关闭该路（无论成败）
                guard.finished = true;
                text
            }
            .await;
            drop(guard);
            let _ = result_tx.send(outcome);
        });
        AsrStream::new(audio, result, task).with_interim(interim)
    }
}

/// 本地合成（Kokoro / Matcha）。
pub struct LocalTts {
    pub helper: LocalHelper,
    pub speaker: i32,
    pub speed: f32,
}

impl Tts for LocalTts {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        let helper = self.helper.clone();
        let (text, speaker, speed) = (text.to_owned(), self.speaker, self.speed);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || helper.synthesize_blocking(&text, speaker, speed))
                .await
                .map_err(|_| ProviderError::new("本地合成任务异常结束"))?
        })
    }

    fn synthesize_stream<'a>(
        &'a self,
        text: &'a str,
        chunks: tokio::sync::mpsc::UnboundedSender<Pcm>,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        let helper = self.helper.clone();
        let (text, speaker, speed) = (text.to_owned(), self.speaker, self.speed);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                helper.synthesize_stream_blocking(text, speaker, speed, chunks)
            })
            .await
            .map_err(|_| ProviderError::new("本地合成任务异常结束"))?
        })
    }
}

#[cfg(test)]
mod tests;
