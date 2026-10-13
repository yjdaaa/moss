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
}

/// 为每个会话创建判停器（NV6）。返回 `None` 时会话使用能量判停。
pub trait VadFactory: Send + Sync {
    fn create(&self, sample_rate: u32, config: &VadConfig) -> Option<Box<dyn Vad>>;
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
}

static NEXT_STREAM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl LocalVad {
    /// 打开一路判停流；失败返回错误（调用方改用能量判停）。
    pub fn open(helper: moss_voice_providers::local::LocalHelper, config: &VadConfig) -> Result<Self, String> {
        let stream = NEXT_STREAM.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        helper.vad_open(stream).map_err(|error| error.0)?;
        Ok(Self {
            helper,
            stream: Some(stream),
            fallback: EnergyVad::new(16_000, config.clone()),
            failed: false,
            in_speech: false,
            notice: None,
        })
    }

    fn fail(&mut self, reason: String) {
        self.failed = true;
        if let Some(stream) = self.stream.take() {
            self.helper.vad_close(stream);
        }
        self.fallback.reset();
        self.notice = Some(format!("本地判停出错，本会话改用能量判停：{reason}"));
    }
}

impl Vad for LocalVad {
    fn accept(&mut self, pcm: &[i16]) -> Vec<VadEvent> {
        if self.failed {
            return self.fallback.accept(pcm);
        }
        let Some(stream) = self.stream else { return Vec::new() };
        match self.helper.vad_feed(stream, pcm) {
            Ok(events) => events
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
                .collect(),
            Err(error) => {
                // 本帧起交给能量判停；说话中途切换时能量判停会再报一次“开始”，会话对重复的开始不做处理
                self.fail(error.0);
                self.in_speech = false;
                self.fallback.accept(pcm)
            }
        }
    }

    fn in_speech(&self) -> bool {
        if self.failed { self.fallback.in_speech() } else { self.in_speech }
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
        match self.helper.vad_open(stream) {
            Ok(()) => self.stream = Some(stream),
            Err(error) => self.fail(error.0),
        }
    }

    fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
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
}

impl VadFactory for LocalVadFactory {
    fn create(&self, sample_rate: u32, config: &VadConfig) -> Option<Box<dyn Vad>> {
        if sample_rate != 16_000 {
            return None;
        }
        LocalVad::open(self.helper.clone(), config).ok().map(|vad| Box::new(vad) as Box<dyn Vad>)
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
            min_silence_ms: 200,
            min_speech_ms: 60,
            start_rms: 500.0,
            keep_rms: 250.0,
            start_over_noise: 4.0,
            keep_over_noise: 2.0,
        }
    }
}

/// 检测窗口 20 ms：比设备 60 ms 帧更细，200 ms 静音判停的误差不超过一个窗口。
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
        Self { config, window, pending: Vec::new(), noise: INITIAL_NOISE, in_speech: false, loud_ms: 0, quiet_ms: 0 }
    }

    fn thresholds(&self) -> (f64, f64) {
        (
            self.config.start_rms.max(self.noise * self.config.start_over_noise),
            self.config.keep_rms.max(self.noise * self.config.keep_over_noise),
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
    (samples.iter().map(|&value| (value as f64) * (value as f64)).sum::<f64>() / samples.len() as f64).sqrt()
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
    fn speech_then_200ms_silence_ends_utterance() {
        let mut vad = EnergyVad::new(RATE, VadConfig::default());
        let mut pcm = silence(RATE, 300);
        pcm.extend(tone(RATE, 220.0, 900, 6000.0));
        pcm.extend(silence(RATE, 600));
        let events = feed(&mut vad, &pcm);
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].0, VadEvent::SpeechStart);
        assert!((360..=480).contains(&events[0].1), "开始时刻 {events:?}");
        assert_eq!(events[1].0, VadEvent::SpeechEnd);
        // 说话在 1200 ms 结束，200 ms 静音后判停；以 60 ms 帧送入，最迟在下一帧边界报告
        assert!((1380..=1500).contains(&events[1].1), "结束时刻 {events:?}");
        assert!(!vad.in_speech());
    }

    #[test]
    fn short_pauses_inside_speech_do_not_split() {
        let mut vad = EnergyVad::new(RATE, VadConfig::default());
        let mut pcm = tone(RATE, 220.0, 500, 6000.0);
        pcm.extend(silence(RATE, 120));
        pcm.extend(tone(RATE, 220.0, 500, 6000.0));
        pcm.extend(silence(RATE, 400));
        let events: Vec<_> = feed(&mut vad, &pcm).into_iter().map(|(event, _)| event).collect();
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
        let events: Vec<_> = feed(&mut vad, &loud).into_iter().map(|(event, _)| event).collect();
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

        use moss_voice_local_proto::{Op, Reply, Request, Response, VadEventKind, read_frame, write_frame};
        use moss_voice_providers::local::LocalHelper;

        use super::*;

        /// 假辅助程序：第 N 次送入（从 1 起）时按脚本回应；`die_at` 那次之后断开连接。记录收到的请求。
        fn fake(script: Vec<(usize, Vec<VadEventKind>)>, die_at: Option<usize>) -> (LocalHelper, Arc<Mutex<Vec<Op>>>) {
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
                            let events = script.iter().find(|(at, _)| *at == feeds).map(|(_, e)| e.clone()).unwrap_or_default();
                            Reply::VadEvents { events }
                        }
                        _ => Reply::Done,
                    };
                    if write_frame(&mut writer, &Response { id: request.id, reply }, &[]).is_err() {
                        return;
                    }
                }
            });
            (LocalHelper::from_streams(Box::new(client.try_clone().unwrap()), Box::new(client)), seen)
        }

        #[test]
        fn silero_events_are_mapped_and_reset_opens_a_new_stream() {
            let (helper, seen) = fake(vec![(2, vec![VadEventKind::SpeechStart]), (4, vec![VadEventKind::SpeechEnd])], None);
            let factory = LocalVadFactory { helper };
            assert!(factory.create(24_000, &VadConfig::default()).is_none(), "只支持 16 kHz");
            let mut vad = factory.create(RATE, &VadConfig::default()).unwrap();
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
            let opened: Vec<u64> = ops.iter().filter_map(|op| if let Op::VadOpen { stream } = op { Some(*stream) } else { None }).collect();
            assert_eq!(opened.len(), 2, "初始一路 + 重置后新开一路：{ops:?}");
            assert_ne!(opened[0], opened[1]);
            let closed = ops.iter().filter(|op| matches!(op, Op::VadClose { .. })).count();
            assert_eq!(closed, 2, "重置关旧流，丢弃时关新流");
        }

        #[test]
        fn helper_failure_falls_back_to_energy_vad_with_a_notice() {
            let (helper, _) = fake(vec![], Some(2));
            let mut vad = LocalVadFactory { helper }.create(RATE, &VadConfig::default()).unwrap();
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
            assert!(vad.take_notice().is_none(), "提示只给一次");
            vad.reset();
            assert!(vad.take_notice().is_none(), "已改用能量判停，重置不再尝试辅助程序");
        }
    }
}
