//! 语音活动检测（VAD）：自动拾音模式下判断用户什么时候说完。
//!
//! [`Vad`] 是统一接口：进程内的 [`EnergyVad`]（双阈值能量检测，不需要模型），以及 NV6 的 [`LocalVad`]
//! （Silero 模型，在本地模型辅助程序中运行，技术书 §11.2）。[`LocalVad`] 任何一次出错都改用能量检测，不中断会话。
//!
//! 参数取 xiaozhi-esp32-server `core/providers/vad/silero.py`（`7a260ee`）的语义：
//! 进入语音用高阈值、保持语音用低阈值（双阈值），连续静音 200 ms 判定说完。

/// 一次检测产生的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    SpeechStart,
    SpeechEnd,
    /// 命令词检测模型在这段音频里听到了停止 / 退出命令词（协议 7，n101；与判停状态无关）
    Keyword(moss_voice_providers::local::CommandKeyword),
}

pub trait Vad: Send {
    /// 送入一段 PCM（采样率为创建时指定的值），返回期间发生的事件。
    fn accept(&mut self, pcm: &[i16]) -> Vec<VadEvent>;
    fn in_speech(&self) -> bool;
    /// 新一轮拾音开始时调用。
    fn reset(&mut self);
    /// 需要记入事件的提示（例如本地判停失败、已改用能量判停）；取走后清空。
    fn take_notice(&mut self) -> Option<String> {
        None
    }
    /// 此刻实际在用的判停方式（完整过程记录写“谁判定的开口 / 说完”，n110）。
    fn engine(&self) -> String {
        ENERGY_VAD_LABEL.to_owned()
    }
    /// 本地模型辅助程序报告的这一路判停流降噪情况（新开一路判停流后有一次；取走后清空）。
    fn take_denoise(&mut self) -> Option<moss_voice_providers::local::DenoiseReport> {
        None
    }
}

/// 能量判停在完整过程记录里的称呼。
pub const ENERGY_VAD_LABEL: &str = "能量判停（按音量）";

/// 为每个会话创建判停器（NV6）。返回 `None` 时会话使用能量判停。
pub trait VadFactory: Send + Sync {
    fn create(&self, sample_rate: u32, config: &VadConfig) -> Option<Box<dyn Vad>>;
    /// 同 [`Self::create`]，建不起来时说明原因（会话改用能量判停，并把原因记进完整过程记录，n110）。
    fn create_explained(
        &self,
        sample_rate: u32,
        config: &VadConfig,
    ) -> Result<Box<dyn Vad>, String> {
        self.create(sample_rate, config)
            .ok_or_else(|| format!("{}没建起来", self.label()))
    }
    /// 这个判停模型的称呼（如“本地判停 Silero v6.2”）。
    fn label(&self) -> String {
        "本地判停模型".to_owned()
    }
}

impl std::fmt::Debug for dyn VadFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VadFactory")
    }
}

/// Silero 判停（经本地模型辅助程序）。只支持 16 kHz；出错后本会话改用能量判停。
pub struct LocalVad {
    helper: moss_voice_providers::local::LocalHelper,
    stream: Option<u64>,
    fallback: EnergyVad,
    failed: bool,
    in_speech: bool,
    notice: Option<String>,
    label: String,
    denoise: Option<moss_voice_providers::local::DenoiseReport>,
}

static NEXT_STREAM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl LocalVad {
    /// 打开一路判停流；失败返回错误（调用方改用能量判停）。
    pub fn open(
        helper: moss_voice_providers::local::LocalHelper,
        config: &VadConfig,
    ) -> Result<Self, String> {
        Self::open_labelled(helper, config, "本地判停模型".to_owned())
    }

    fn open_labelled(
        helper: moss_voice_providers::local::LocalHelper,
        config: &VadConfig,
        label: String,
    ) -> Result<Self, String> {
        let stream = NEXT_STREAM.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let denoise = helper
            .vad_open_report(stream)
            .map_err(|error| error.message)?;
        Ok(Self {
            helper,
            stream: Some(stream),
            fallback: EnergyVad::new(16_000, config.clone()),
            failed: false,
            in_speech: false,
            notice: None,
            label,
            denoise,
        })
    }

    fn fail(&mut self, reason: String) {
        self.failed = true;
        if let Some(stream) = self.stream.take() {
            self.helper.vad_close(stream);
        }
        self.fallback.reset();
        // 改用能量判停后这一路不再经降噪：之前那份“已降噪”的报告作废，不能再交出去
        self.denoise = None;
        self.notice = Some(format!("本地判停出错，本会话改用能量判停：{reason}"));
    }
}

impl Vad for LocalVad {
    fn accept(&mut self, pcm: &[i16]) -> Vec<VadEvent> {
        if self.failed {
            return self.fallback.accept(pcm);
        }
        let Some(stream) = self.stream else {
            return Vec::new();
        };
        match self.helper.vad_feed_with_keywords(stream, pcm) {
            Ok((events, keywords)) => events
                .into_iter()
                .map(|event| match event {
                    moss_voice_providers::local::VadEventKind::SpeechStart => {
                        self.in_speech = true;
                        VadEvent::SpeechStart
                    }
                    moss_voice_providers::local::VadEventKind::SpeechEnd => {
                        self.in_speech = false;
                        VadEvent::SpeechEnd
                    }
                })
                .chain(
                    keywords
                        .iter()
                        .filter_map(|label| {
                            moss_voice_providers::local::command_keyword_kind(label)
                        })
                        .map(VadEvent::Keyword),
                )
                .collect(),
            Err(error) => {
                // 本帧起交给能量判停；说话中途切换时能量判停会再报一次“开始”，会话对重复的开始不做处理
                self.fail(error.message);
                self.in_speech = false;
                self.fallback.accept(pcm)
            }
        }
    }

    fn in_speech(&self) -> bool {
        if self.failed {
            self.fallback.in_speech()
        } else {
            self.in_speech
        }
    }

    fn reset(&mut self) {
        self.in_speech = false;
        if self.failed {
            self.fallback.reset();
            return;
        }
        // 新一轮拾音：换一路新的判停流（丢弃 Silero 的内部状态）
        if let Some(stream) = self.stream.take() {
            self.helper.vad_close(stream);
        }
        let stream = NEXT_STREAM.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.helper.vad_open_report(stream) {
            Ok(denoise) => {
                self.stream = Some(stream);
                self.denoise = denoise;
            }
            Err(error) => self.fail(error.message),
        }
    }

    fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    fn engine(&self) -> String {
        if self.failed {
            format!("{ENERGY_VAD_LABEL}，{}出错后改用", self.label)
        } else {
            self.label.clone()
        }
    }

    fn take_denoise(&mut self) -> Option<moss_voice_providers::local::DenoiseReport> {
        self.denoise.take()
    }
}

impl Drop for LocalVad {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            self.helper.vad_close(stream);
        }
    }
}

/// 用本地模型辅助程序创建 Silero 判停；非 16 kHz 或打开失败时返回 `None`（使用能量判停）。
pub struct LocalVadFactory {
    pub helper: moss_voice_providers::local::LocalHelper,
    /// 完整过程记录里的称呼（如“本地判停 Silero v6.2”）
    pub label: String,
}

impl LocalVadFactory {
    pub fn new(helper: moss_voice_providers::local::LocalHelper, label: impl Into<String>) -> Self {
        Self {
            helper,
            label: label.into(),
        }
    }
}

impl VadFactory for LocalVadFactory {
    fn create(&self, sample_rate: u32, config: &VadConfig) -> Option<Box<dyn Vad>> {
        self.create_explained(sample_rate, config).ok()
    }

    fn create_explained(
        &self,
        sample_rate: u32,
        config: &VadConfig,
    ) -> Result<Box<dyn Vad>, String> {
        if sample_rate != 16_000 {
            return Err(format!(
                "设备上行音频是 {sample_rate} Hz，{}只支持 16 kHz",
                self.label
            ));
        }
        LocalVad::open_labelled(self.helper.clone(), config, self.label.clone())
            .map(|vad| Box::new(vad) as Box<dyn Vad>)
            .map_err(|error| format!("{}打不开：{error}", self.label))
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VadConfig {
    /// 连续静音多久判定说完（esp32-server 默认 200 ms）
    pub min_silence_ms: u32,
    /// 连续多久的高能量才算开始说话（滤掉按键声、咳嗽）
    pub min_speech_ms: u32,
    /// 开始说话的绝对下限（RMS，满幅 32767）
    pub start_rms: f64,
    /// 保持说话的绝对下限
    pub keep_rms: f64,
    /// 开始阈值至少是背景噪声的多少倍
    pub start_over_noise: f64,
    /// 保持阈值至少是背景噪声的多少倍
    pub keep_over_noise: f64,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            // 2026-11-13 n86：静音 250 ms 先交给识别与大模型抢跑；真正开口前还要等多久由会话按
            // 这句话像不像说完来定（`fast::endpoint_wait_ms`），期间用户接着说则合并重来
            min_silence_ms: 250,
            min_speech_ms: 60,
            start_rms: 500.0,
            keep_rms: 250.0,
            start_over_noise: 4.0,
            keep_over_noise: 2.0,
        }
    }
}

/// 检测窗口 20 ms：比设备 60 ms 帧更细，静音判停的误差不超过一个窗口。
const WINDOW_MS: u32 = 20;
const INITIAL_NOISE: f64 = 100.0;

/// 双阈值能量 VAD，背景噪声自适应（只在非语音时更新）。
pub struct EnergyVad {
    config: VadConfig,
    window: usize,
    pending: Vec<i16>,
    noise: f64,
    in_speech: bool,
    /// 开始前已累计的高能量时长 / 语音中已累计的静音时长
    loud_ms: u32,
    quiet_ms: u32,
}

impl EnergyVad {
    pub fn new(sample_rate: u32, config: VadConfig) -> Self {
        let window = (sample_rate as usize * WINDOW_MS as usize / 1000).max(1);
        Self {
            config,
            window,
            pending: Vec::new(),
            noise: INITIAL_NOISE,
            in_speech: false,
            loud_ms: 0,
            quiet_ms: 0,
        }
    }

    fn thresholds(&self) -> (f64, f64) {
        (
            self.config
                .start_rms
                .max(self.noise * self.config.start_over_noise),
            self.config
                .keep_rms
                .max(self.noise * self.config.keep_over_noise),
        )
    }

    fn window_event(&mut self, rms: f64) -> Option<VadEvent> {
        let (start, keep) = self.thresholds();
        if self.in_speech {
            if rms >= keep {
                self.quiet_ms = 0;
                return None;
            }
            self.quiet_ms += WINDOW_MS;
            if self.quiet_ms >= self.config.min_silence_ms {
                self.in_speech = false;
                self.quiet_ms = 0;
                self.loud_ms = 0;
                return Some(VadEvent::SpeechEnd);
            }
            return None;
        }
        if rms >= start {
            self.loud_ms += WINDOW_MS;
            if self.loud_ms >= self.config.min_speech_ms {
                self.in_speech = true;
                self.quiet_ms = 0;
                return Some(VadEvent::SpeechStart);
            }
        } else {
            self.loud_ms = 0;
            // 慢速跟踪背景噪声；高于开始阈值的窗口不计入，避免把人声当噪声
            self.noise = self.noise * 0.95 + rms.max(1.0) * 0.05;
        }
        None
    }
}

fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples
        .iter()
        .map(|&value| (value as f64) * (value as f64))
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt()
}

impl Vad for EnergyVad {
    fn accept(&mut self, pcm: &[i16]) -> Vec<VadEvent> {
        self.pending.extend_from_slice(pcm);
        let mut events = Vec::new();
        let mut offset = 0;
        while self.pending.len() - offset >= self.window {
            let level = rms(&self.pending[offset..offset + self.window]);
            offset += self.window;
            if let Some(event) = self.window_event(level) {
                events.push(event);
            }
        }
        self.pending.drain(..offset);
        events
    }

    fn in_speech(&self) -> bool {
        self.in_speech
    }

    fn reset(&mut self) {
        // 保留背景噪声估计：同一台设备的环境在两轮之间不会突变
        self.pending.clear();
        self.in_speech = false;
        self.loud_ms = 0;
        self.quiet_ms = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::test_signal::{noise, silence, tone};

    const RATE: u32 = 16_000;

    /// 以设备的 60 ms 帧送入，返回（事件，事件发生时已送入的毫秒数）。
    fn feed(vad: &mut EnergyVad, pcm: &[i16]) -> Vec<(VadEvent, u32)> {
        let frame = (RATE * 60 / 1000) as usize;
        let mut out = Vec::new();
        for (index, chunk) in pcm.chunks(frame).enumerate() {
            for event in vad.accept(chunk) {
                out.push((event, (index as u32 + 1) * 60));
            }
        }
        out
    }

    #[test]
    fn speech_then_250ms_silence_ends_utterance() {
        let mut vad = EnergyVad::new(RATE, VadConfig::default());
        let mut pcm = silence(RATE, 300);
        pcm.extend(tone(RATE, 220.0, 900, 6000.0));
        pcm.extend(silence(RATE, 600));
        let events = feed(&mut vad, &pcm);
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].0, VadEvent::SpeechStart);
        assert!((360..=480).contains(&events[0].1), "开始时刻 {events:?}");
        assert_eq!(events[1].0, VadEvent::SpeechEnd);
        // 说话在 1200 ms 结束，250 ms 静音后判停；以 60 ms 帧送入，最迟在下一帧边界报告
        assert!((1440..=1560).contains(&events[1].1), "结束时刻 {events:?}");
        assert!(!vad.in_speech());
    }

    #[test]
    fn short_pauses_inside_speech_do_not_split() {
        let mut vad = EnergyVad::new(RATE, VadConfig::default());
        let mut pcm = tone(RATE, 220.0, 500, 6000.0);
        pcm.extend(silence(RATE, 120));
        pcm.extend(tone(RATE, 220.0, 500, 6000.0));
        pcm.extend(silence(RATE, 400));
        let events: Vec<_> = feed(&mut vad, &pcm)
            .into_iter()
            .map(|(event, _)| event)
            .collect();
        assert_eq!(events, vec![VadEvent::SpeechStart, VadEvent::SpeechEnd]);
    }

    #[test]
    fn clicks_and_background_noise_are_ignored() {
        let mut vad = EnergyVad::new(RATE, VadConfig::default());
        let mut pcm = noise(RATE, 2000, 300.0, 7);
        // 20 ms 的按键声短于 min_speech_ms
        pcm.extend(tone(RATE, 1000.0, 20, 9000.0));
        pcm.extend(noise(RATE, 1000, 300.0, 9));
        assert!(feed(&mut vad, &pcm).is_empty());
        // 背景噪声较大时，正常说话仍能检测到
        let mut loud = noise(RATE, 1000, 900.0, 11);
        loud.extend(tone(RATE, 220.0, 600, 9000.0));
        loud.extend(noise(RATE, 600, 900.0, 13));
        let events: Vec<_> = feed(&mut vad, &loud)
            .into_iter()
            .map(|(event, _)| event)
            .collect();
        assert_eq!(events, vec![VadEvent::SpeechStart, VadEvent::SpeechEnd]);
    }

    #[test]
    fn reset_clears_speech_state() {
        let mut vad = EnergyVad::new(RATE, VadConfig::default());
        feed(&mut vad, &tone(RATE, 220.0, 300, 6000.0));
        assert!(vad.in_speech());
        vad.reset();
        assert!(!vad.in_speech());
        assert!(feed(&mut vad, &silence(RATE, 500)).is_empty());
    }

    mod local {
        use std::net::{TcpListener, TcpStream};
        use std::sync::{Arc, Mutex};

        use moss_voice_local_proto::{
            Op, Reply, Request, Response, VadEventKind, read_frame, write_frame,
        };
        use moss_voice_providers::local::LocalHelper;

        use super::*;

        /// 假辅助程序：第 N 次送入（从 1 起）时按脚本回应；`die_at` 那次之后断开连接。记录收到的请求。
        fn fake(
            script: Vec<(usize, Vec<VadEventKind>)>,
            die_at: Option<usize>,
        ) -> (LocalHelper, Arc<Mutex<Vec<Op>>>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (server, _) = listener.accept().unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            std::thread::spawn(move || {
                let (mut reader, mut writer) = (server.try_clone().unwrap(), server);
                let mut feeds = 0;
                while let Ok(Some((request, _))) = read_frame::<_, Request>(&mut reader) {
                    log.lock().unwrap().push(request.op.clone());
                    let reply = match request.op {
                        Op::VadFeed { .. } => {
                            feeds += 1;
                            if Some(feeds) == die_at {
                                return;
                            }
                            let events = script
                                .iter()
                                .find(|(at, _)| *at == feeds)
                                .map(|(_, e)| e.clone())
                                .unwrap_or_default();
                            Reply::VadEvents {
                                events,
                                keywords: Vec::new(),
                            }
                        }
                        // 协议 8：打开判停流时报告这一路已降噪
                        Op::VadOpen { .. } => Reply::Opened {
                            denoise: moss_voice_local_proto::DenoiseReport {
                                applied: true,
                                model: "gtcrn_simple.onnx".into(),
                                engine: "gtcrn".into(),
                                error: None,
                            },
                        },
                        _ => Reply::Done,
                    };
                    if write_frame(
                        &mut writer,
                        &Response {
                            id: request.id,
                            reply,
                        },
                        &[],
                    )
                    .is_err()
                    {
                        return;
                    }
                }
            });
            (
                LocalHelper::from_streams(Box::new(client.try_clone().unwrap()), Box::new(client)),
                seen,
            )
        }

        #[test]
        fn silero_events_are_mapped_and_reset_opens_a_new_stream() {
            let (helper, seen) = fake(
                vec![
                    (2, vec![VadEventKind::SpeechStart]),
                    (4, vec![VadEventKind::SpeechEnd]),
                ],
                None,
            );
            let factory = LocalVadFactory::new(helper, "本地判停");
            assert!(
                factory.create(24_000, &VadConfig::default()).is_none(),
                "只支持 16 kHz"
            );
            let mut vad = factory.create(RATE, &VadConfig::default()).unwrap();
            assert!(vad.take_denoise().is_some_and(|report| report.applied));
            assert!(vad.take_denoise().is_none(), "每一路只交一次");
            assert_eq!(vad.engine(), "本地判停");
            let frame = silence(RATE, 60);
            assert!(vad.accept(&frame).is_empty());
            assert_eq!(vad.accept(&frame), vec![VadEvent::SpeechStart]);
            assert!(vad.in_speech());
            assert!(vad.accept(&frame).is_empty());
            assert_eq!(vad.accept(&frame), vec![VadEvent::SpeechEnd]);
            assert!(!vad.in_speech());
            vad.reset();
            assert!(vad.take_notice().is_none());
            drop(vad);
            std::thread::sleep(std::time::Duration::from_millis(200));
            let ops = seen.lock().unwrap();
            let opened: Vec<u64> = ops
                .iter()
                .filter_map(|op| {
                    if let Op::VadOpen { stream } = op {
                        Some(*stream)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(opened.len(), 2, "初始一路 + 重置后新开一路：{ops:?}");
            assert_ne!(opened[0], opened[1]);
            let closed = ops
                .iter()
                .filter(|op| matches!(op, Op::VadClose { .. }))
                .count();
            assert_eq!(closed, 2, "重置关旧流，丢弃时关新流");
        }

        #[test]
        fn helper_failure_falls_back_to_energy_vad_with_a_notice() {
            let (helper, _) = fake(vec![], Some(2));
            let mut vad = LocalVadFactory::new(helper, "本地判停")
                .create(RATE, &VadConfig::default())
                .unwrap();
            assert!(vad.accept(&silence(RATE, 60)).is_empty());
            // 辅助程序在第二帧时退出：本帧起改用能量判停，会话照常判停
            let mut pcm = tone(RATE, 220.0, 600, 6000.0);
            pcm.extend(silence(RATE, 600));
            let mut events = Vec::new();
            for chunk in pcm.chunks(960) {
                events.extend(vad.accept(chunk));
            }
            assert_eq!(events, vec![VadEvent::SpeechStart, VadEvent::SpeechEnd]);
            let notice = vad.take_notice().unwrap();
            assert!(notice.contains("改用能量判停"), "{notice}");
            // n110 复审 5①：改用能量判停后，打开时那份“已降噪”的报告作废，不再交出
            assert!(vad.take_denoise().is_none());
            assert!(
                vad.engine().starts_with(ENERGY_VAD_LABEL),
                "{}",
                vad.engine()
            );
            assert!(vad.take_notice().is_none(), "提示只给一次");
            vad.reset();
            assert!(
                vad.take_notice().is_none(),
                "已改用能量判停，重置不再尝试辅助程序"
            );
        }
    }
}

/// 内置能量判停在真实语音上的表现（与本地 Silero / TEN 对照用，不在常规测试中运行）：
/// `MOSS_VAD_CLIPS=<目录，含 meta.json 与 16 kHz s16 片段> cargo test -p moss-voice-server energy_vad_report -- --ignored --nocapture`
/// 片段为“3 秒纯背景 + 语音（前补 1 秒、后补 1.5 秒）”，已经过设备同款 libopus 上行编解码。
#[cfg(test)]
#[test]
#[ignore]
fn energy_vad_report() {
    let dir = std::path::PathBuf::from(std::env::var("MOSS_VAD_CLIPS").expect("MOSS_VAD_CLIPS"));
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap();
    for level in [0, 150, 500, 999] {
        let (mut starts, mut ends, mut splits, mut misses, mut false_noise, mut clips) =
            (Vec::new(), Vec::new(), 0, 0, 0, 0);
        for item in meta
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["level"] == level)
        {
            clips += 1;
            let bytes = std::fs::read(dir.join(item["file"].as_str().unwrap())).unwrap();
            let pcm: Vec<i16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b))
                .collect();
            let lead = item["lead_ms"].as_i64().unwrap();
            let (on, off) = (
                item["on_ms"].as_i64().unwrap(),
                item["off_ms"].as_i64().unwrap(),
            );
            let mut vad = EnergyVad::new(16_000, VadConfig::default());
            let mut events = Vec::new();
            for (index, frame) in pcm.chunks(960).enumerate() {
                let t = (index as i64 + 1) * 60 - lead;
                events.extend(vad.accept(frame).into_iter().map(|e| (e, t)));
            }
            false_noise += events
                .iter()
                .filter(|(e, t)| *e == VadEvent::SpeechStart && *t < 0)
                .count();
            let real: Vec<_> = events.iter().filter(|(_, t)| *t >= 0).collect();
            let first = real
                .iter()
                .find(|(e, _)| *e == VadEvent::SpeechStart)
                .map(|(_, t)| *t);
            let end_times: Vec<i64> = real
                .iter()
                .filter(|(e, _)| *e == VadEvent::SpeechEnd)
                .map(|(_, t)| *t)
                .collect();
            let (Some(first), false) = (first, end_times.is_empty()) else {
                misses += 1;
                continue;
            };
            starts.push(first - on);
            let last = end_times
                .iter()
                .find(|t| **t >= off)
                .copied()
                .unwrap_or(*end_times.last().unwrap());
            ends.push(last - off);
            splits += end_times.iter().filter(|t| **t > on && **t < off).count();
        }
        let avg = |v: &[i64]| {
            if v.is_empty() {
                f64::NAN
            } else {
                v.iter().sum::<i64>() as f64 / v.len() as f64
            }
        };
        println!(
            "能量判停 噪声 {level:>3} | 开口判出 平均晚 {:5.0} ms | 说完判停 平均晚 {:5.0} ms（最晚 {} ms）| 句中误切 {splits} 次 | 漏判 {misses}/{clips} | 纯噪声误触发 {false_noise} 次",
            avg(&starts),
            avg(&ends),
            ends.iter().max().copied().unwrap_or(0)
        );
    }
}
