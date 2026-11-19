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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, SyncSender, TrySendError, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use moss_voice_local_proto::{
    AsrDecodeOptions, AsrEngine, COMMAND_KEYWORDS, CommandKeyword, TtsEngine, VadEngine,
    VadEventKind, VadParams, command_keyword_kind, command_keywords_text,
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
    job: Mutex<Option<moss_process::JobHandle>>,
    pacing: Mutex<Pacing>,
}

/// 本地合成的节奏估计：这台机器合成有多快、已经交出去的语音大概还能播多久。
/// 据此决定下一句的第一段可以多长——能整句合成就不切碎（切碎的每一小段都会重新起调，听着像机器）。
#[derive(Debug, Clone, Copy)]
struct Pacing {
    /// 实时率的滑动估计（合成 1 秒语音要算多少秒）
    rtf: f64,
    /// 已交出去的语音按实时播放会播到什么时候
    play_until: Option<std::time::Instant>,
    /// 正在合成或排队的流式请求数（排队等待的时间不能算进实时率）
    in_flight: usize,
    /// 已经量到过实时率（之前用的是偏慢的默认猜测）
    measured: bool,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            // 没量过之前按偏慢的机器算，宁可先切碎
            rtf: 0.7,
            play_until: None,
            in_flight: 0,
            measured: false,
        }
    }
}

impl Pacing {
    /// 新的一句第一段最多多少字（0 = 用最保守的阶梯）。
    /// 允许的合成耗时 = 前面的存货还能播多久 + 可接受的开口等待；按每秒约 4.5 个字换算，留两成余量。
    fn first_max_chars(&self, now: std::time::Instant) -> u32 {
        const ACCEPTABLE_WAIT: f64 = 0.5;
        let lead = self.play_until.map_or(0.0, |until| {
            until.saturating_duration_since(now).as_secs_f64()
        });
        let chars = (lead + ACCEPTABLE_WAIT) / self.rtf.max(0.02) * 4.5 * 0.8;
        if chars < 14.0 {
            0
        } else {
            chars.min(200.0) as u32
        }
    }

    /// 这一句开头能不能把很短的开场白单独先合成（协议 7 `short_opener`）：只在前面没有存货（一轮刚开口）、
    /// 且这台机器量过的实时率足够低时打开——实测 Kokoro v1.0（实时率约 0.21–0.25，短段因固定开销偏高）
    /// 合成“好的，”只要 0.16 秒，整句要 0.55 秒；第二段收短到 10 字以内（约 2.2 秒语音），
    /// 实时率 0.32 时约 0.7 秒算完，与开场白约 0.64 秒的播放时长相当，中间不会明显断开。
    fn short_opener(&self, now: std::time::Instant) -> bool {
        const FAST_ENOUGH: f64 = 0.32;
        let idle = self.play_until.is_none_or(|until| until <= now);
        idle && self.rtf <= FAST_ENOUGH
    }

    /// 一段语音合成完：`compute` 为这一段的纯合成耗时（没有排队时才有），`audio` 为它的时长。
    fn delivered(&mut self, now: std::time::Instant, compute: Option<Duration>, audio: f64) {
        if let Some(compute) = compute
            && audio >= 0.3
        {
            let sample = (compute.as_secs_f64() / audio).clamp(0.01, 3.0);
            // 第一次量到就直接用（默认值只是猜的，慢慢混合要五六轮才收敛，期间一直按慢机器处理），之后再平滑
            self.rtf = if self.measured {
                self.rtf * 0.6 + sample * 0.4
            } else {
                sample
            };
            self.measured = true;
        }
        let begin = self.play_until.filter(|until| *until > now).unwrap_or(now);
        self.play_until = Some(begin + Duration::from_secs_f64(audio));
    }
}

impl Inner {
    fn invalidate(&self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
        if let Ok(mut job) = self.job.lock()
            && let Some(job) = job.take()
        {
            job.terminate();
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
        if let Ok(mut job) = self.job.lock()
            && let Some(job) = job.take()
        {
            job.terminate();
        }
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

const TURN_CONSECUTIVE_TIMEOUT_CAP: usize = 4;

/// 尾音语义判停专用进程，不暴露共享的 ASR/TTS/VAD 句柄。
/// 单次超时只放弃当次推理（退回静音计时）而不杀死进程；连续达到上限才自行结束，避免判停偶发变慢连累主 helper。
#[derive(Clone)]
pub struct TurnHelper {
    helper: LocalHelper,
    model: Option<PathBuf>,
    consecutive_timeouts: Arc<AtomicUsize>,
}

impl TurnHelper {
    pub fn spawn(exe: &Path, model_file: Option<&Path>) -> Result<Self, ProviderError> {
        let helper = LocalHelper::spawn(exe)?;
        helper.load_smart_turn(model_file)?;
        Ok(Self::from_loaded_helper(helper, model_file))
    }

    pub fn from_loaded_helper(helper: LocalHelper, model_file: Option<&Path>) -> Self {
        Self {
            helper,
            model: model_file.map(Path::to_path_buf),
            consecutive_timeouts: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn model_path(&self) -> Option<&Path> {
        self.model.as_deref()
    }

    pub fn is_alive(&self) -> bool {
        self.helper.is_alive()
    }

    pub fn terminate(&self) {
        self.helper.terminate();
    }

    pub fn predict_blocking(
        &self,
        sample_rate: u32,
        pcm: &[i16],
        timeout: Duration,
    ) -> Result<(bool, f32), ProviderError> {
        match self
            .helper
            .predict_smart_turn_soft(sample_rate, pcm, timeout)
        {
            Ok(result) => {
                self.consecutive_timeouts.store(0, Ordering::SeqCst);
                Ok(result)
            }
            Err(failure) if failure.timed_out => {
                let streak = self.consecutive_timeouts.fetch_add(1, Ordering::SeqCst) + 1;
                if streak >= TURN_CONSECUTIVE_TIMEOUT_CAP {
                    self.helper.terminate();
                }
                Err(failure.error)
            }
            Err(failure) => {
                self.consecutive_timeouts.store(0, Ordering::SeqCst);
                Err(failure.error)
            }
        }
    }
}

/// 辅助程序加载的合成模型信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TtsInfo {
    pub sample_rate: u32,
    pub speakers: u32,
}

struct CallFailure {
    error: ProviderError,
    timed_out: bool,
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
        let job = moss_process::JobHandle::for_child(&child);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::new("本地模型程序启动失败：没有标准输入"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::new("本地模型程序启动失败：没有标准输出"))?;
        let helper = Self::from_streams(Box::new(stdout), Box::new(stdin));
        if let Ok(mut slot) = helper.inner.job.lock() {
            *slot = job;
        }
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
            job: Mutex::new(None),
            pacing: Mutex::new(Pacing::default()),
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

    /// 立即结束辅助程序子进程并等待句柄释放（用于卸载本地模型前释放文件锁）。
    pub fn terminate(&self) {
        self.inner.alive.store(false, Ordering::SeqCst);
        if let Ok(mut pending) = self.inner.pending.lock() {
            pending.clear();
        }
        if let Ok(mut job) = self.inner.job.lock()
            && let Some(job) = job.take()
        {
            job.terminate();
        }
        if let Ok(mut slot) = self.inner.child.lock()
            && let Some(mut child) = slot.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
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
        self.call_with_policy(op, payload, timeout, true)
            .map_err(|failure| failure.error)
    }

    fn call_with_policy(
        &self,
        op: Op,
        payload: &[u8],
        timeout: Duration,
        invalidate_on_timeout: bool,
    ) -> Result<(Reply, Vec<u8>), CallFailure> {
        if !self.is_alive() {
            return Err(CallFailure {
                error: dead(),
                timed_out: false,
            });
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = channel();
        self.inner
            .pending
            .lock()
            .map_err(|_| CallFailure {
                error: dead(),
                timed_out: false,
            })?
            .insert(id, sender);
        let deadline = std::time::Instant::now() + timeout;
        if let Err(error) = self.enqueue(id, op, payload) {
            self.forget(id);
            return Err(CallFailure {
                error,
                timed_out: false,
            });
        }
        let result =
            receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
        match result {
            Ok((Reply::Error { message }, _)) => Err(CallFailure {
                error: ProviderError::new(message),
                timed_out: false,
            }),
            Ok(reply) => Ok(reply),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.forget(id);
                if invalidate_on_timeout {
                    self.inner.invalidate();
                }
                Err(CallFailure {
                    error: ProviderError::new("本地模型处理或管道写入超时"),
                    timed_out: true,
                })
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(CallFailure {
                error: dead(),
                timed_out: false,
            }),
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

    pub fn load_denoiser(&self, model_file: Option<&Path>) -> Result<(), ProviderError> {
        let op = Op::LoadDenoiser {
            model: model_file
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        Self::expect_loaded(self.call(op, &[], LOAD_TIMEOUT)?.0).map(|_| ())
    }

    pub fn load_smart_turn(&self, model_file: Option<&Path>) -> Result<(), ProviderError> {
        let op = Op::LoadSmartTurn {
            model: model_file
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        Self::expect_loaded(self.call(op, &[], LOAD_TIMEOUT)?.0).map(|_| ())
    }

    pub fn predict_smart_turn(
        &self,
        sample_rate: u32,
        pcm: &[i16],
        timeout: Duration,
    ) -> Result<(bool, f32), ProviderError> {
        self.predict_smart_turn_soft(sample_rate, pcm, timeout)
            .map_err(|failure| failure.error)
    }

    fn predict_smart_turn_soft(
        &self,
        sample_rate: u32,
        pcm: &[i16],
        timeout: Duration,
    ) -> Result<(bool, f32), CallFailure> {
        let op = Op::SmartTurnPredict { sample_rate };
        match self
            .call_with_policy(op, &pcm_to_bytes(pcm), timeout, false)?
            .0
        {
            Reply::SmartTurn {
                probability,
                complete,
            } => Ok((complete, probability)),
            _ => Err(CallFailure {
                error: ProviderError::new("尾音语义判停返回了意外的结果"),
                timed_out: false,
            }),
        }
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
        let requested = std::time::Instant::now();
        // 排在别的请求后面时，等待时间不是这台机器的合成速度
        let (first_max_chars, short_opener, queued) = match self.inner.pacing.lock() {
            Ok(mut pacing) => {
                let queued = pacing.in_flight > 0;
                pacing.in_flight += 1;
                (
                    pacing.first_max_chars(requested),
                    !queued && pacing.short_opener(requested),
                    queued,
                )
            }
            Err(_) => (0, false, true),
        };
        let op = Op::SynthesizeStream {
            text,
            speaker,
            speed,
            first_max_chars,
            short_opener,
        };
        if let Err(error) = self.enqueue(id, op, &[]) {
            self.forget(id);
            self.finish_stream(false);
            return Err(error);
        }
        let mut last_chunk = (!queued).then_some(requested);
        let deadline = std::time::Instant::now() + SYNTHESIZE_TIMEOUT;
        // 接收方没了（对话被打断或合并重来）：让辅助程序放弃这一句，别占着合成线程挡住新回答
        let mut abandoned = false;
        let mut abandon = |helper: &Self| {
            abandoned = true;
            let cancel_id = helper.inner.next_id.fetch_add(1, Ordering::SeqCst);
            let _ = helper.enqueue(cancel_id, Op::Cancel { target: id }, &[]);
        };
        let result = loop {
            if chunks.is_closed() {
                abandon(self);
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
                    let now = std::time::Instant::now();
                    if let Ok(mut pacing) = self.inner.pacing.lock() {
                        pacing.delivered(
                            now,
                            last_chunk.map(|since| now.saturating_duration_since(since)),
                            payload.len() as f64 / 2.0 / f64::from(sample_rate),
                        );
                    }
                    last_chunk = Some(now);
                    if chunks
                        .send(Pcm {
                            sample_rate,
                            samples: bytes_to_pcm(&payload),
                        })
                        .is_err()
                    {
                        abandon(self);
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
        self.finish_stream(abandoned);
        result
    }

    /// 一次流式合成结束。被放弃时（对话被打断）交出去的语音已作废：下一句从“没有存货”算起。
    fn finish_stream(&self, abandoned: bool) {
        if let Ok(mut pacing) = self.inner.pacing.lock() {
            pacing.in_flight = pacing.in_flight.saturating_sub(1);
            if abandoned {
                pacing.play_until = None;
            }
        }
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
        self.vad_feed_with_keywords(stream, pcm)
            .map(|(events, _)| events)
    }

    /// 同 [`vad_feed`](Self::vad_feed)，另返回这段音频里检测到的命令词（协议 7，未加载命令词模型时为空）。
    pub fn vad_feed_with_keywords(
        &self,
        stream: u64,
        pcm: &[i16],
    ) -> Result<(Vec<VadEventKind>, Vec<String>), ProviderError> {
        match self
            .call(Op::VadFeed { stream }, &pcm_to_bytes(pcm), VAD_TIMEOUT)?
            .0
        {
            Reply::VadEvents { events, keywords } => Ok((events, keywords)),
            _ => Err(ProviderError::new("本地判停返回了意外的结果")),
        }
    }

    /// 加载命令词检测模型（协议 7，`None` 卸载）：之后每一路判停流顺带检测停止 / 退出命令词。
    pub fn load_kws(&self, model_dir: Option<&Path>) -> Result<(), ProviderError> {
        self.load_kws_with(model_dir, &command_keywords_text())
    }

    /// 同 [`load_kws`](Self::load_kws)，用给定的命令词表（调参实测用）。
    pub fn load_kws_with(
        &self,
        model_dir: Option<&Path>,
        keywords: &str,
    ) -> Result<(), ProviderError> {
        let op = Op::LoadKws {
            dir: model_dir
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            keywords: keywords.to_owned(),
        };
        Self::expect_loaded(self.call(op, &[], LOAD_TIMEOUT)?.0).map(|_| ())
    }

    /// 关闭判停流（尽力而为，不等待结果）。
    pub fn vad_close(&self, stream: u64) {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let _ = self.enqueue(id, Op::VadClose { stream }, &[]);
    }
}

/// 本地整段识别（SenseVoice / 流式模型均可整段识别，用作流式会话失败或单测直调时的退路）。
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

/// 本地流式识别（流式 Zipformer 逐块解码；SenseVoice 在说话期间按约 0.5 秒周期增量重认已录片段）：
/// 用户开始说话即打开一路，边说边送入并吐出中间结果，说完立即取最终文字。
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
