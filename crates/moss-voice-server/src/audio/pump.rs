//! 播放节奏泵：把下行 PCM 切成 60 ms 帧编码，由会话按绝对时钟每 60 ms 取一帧发送。
//!
//! 思路取海门 `crates/haimen-xiaozhi/src/ws.rs` 与 `src/xiaozhi_asr_llm_tts.rs` 的连续性泵（`513d9e6`，MIT），按 MOSS 结构重写：
//! - 启动前先攒够 `start_ms` 的音频（或上游已结束），避免合成刚开始时供不上；
//! - 播放中上游暂时没有数据（例如下一句还在合成）时补静音帧，设备不会因断流而停播；
//! - 只用一个编码器，句与句之间不重建；
//! - 打断时 [`clear`](PlaybackPump::clear) 立即丢弃未播内容；
//! - 每句话的文字作为标记随音频推入，播到这句的第一帧时随帧返回，会话据此发 `sentence_start`，设备屏幕上的字与声音同步。
//!
//! 本模块不做网络与计时，便于用确定的输入测试。

use std::collections::VecDeque;

use super::codec::OpusEncoder;

/// 一次取帧的结果。
#[derive(Debug, PartialEq, Eq)]
pub enum PumpTick {
    /// 发送这一帧；`marks` 为从这一帧开始播放的句子文字（先发文字再发这一帧）
    Frame { data: Vec<u8>, marks: Vec<String> },
    /// 还在攒启动缓冲，这一拍不发
    Waiting,
    /// 上游已结束且全部发完
    Done,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PumpStats {
    /// 已发送的有内容的帧
    pub frames: u64,
    /// 播放中因上游供不上而补的静音帧
    pub underruns: u64,
}

pub struct PlaybackPump {
    encoder: OpusEncoder,
    start_samples: usize,
    pending: VecDeque<i16>,
    /// （从本段开始累计的样本位置, 句子文字）
    marks: VecDeque<(u64, String)>,
    pushed: u64,
    consumed: u64,
    started: bool,
    finished: bool,
    stats: PumpStats,
}

impl PlaybackPump {
    pub fn new(encoder: OpusEncoder, sample_rate: u32, start_ms: u32) -> Self {
        Self {
            encoder,
            start_samples: sample_rate as usize * start_ms as usize / 1000,
            pending: VecDeque::new(),
            marks: VecDeque::new(),
            pushed: 0,
            consumed: 0,
            started: false,
            finished: false,
            stats: PumpStats::default(),
        }
    }

    /// 开始新一段播放（上一段已结束或被打断）。
    pub fn begin(&mut self) {
        self.pending.clear();
        self.marks.clear();
        self.pushed = 0;
        self.consumed = 0;
        self.started = false;
        self.finished = false;
        self.stats = PumpStats::default();
    }

    pub fn push(&mut self, pcm: &[i16]) {
        if !self.finished {
            self.pending.extend(pcm.iter().copied());
            self.pushed += pcm.len() as u64;
        }
    }

    /// 推入一句话：文字在这句音频的第一帧随帧返回。
    pub fn push_marked(&mut self, pcm: &[i16], mark: String) {
        if !self.finished {
            self.marks.push_back((self.pushed, mark));
            self.push(pcm);
        }
    }

    /// 上游不会再有数据。
    pub fn finish(&mut self) {
        self.finished = true;
    }

    /// 打断：丢弃未播内容，重置编码器。
    pub fn clear(&mut self) {
        self.pending.clear();
        self.marks.clear();
        self.started = false;
        self.finished = true;
        self.encoder.reset();
    }

    pub fn stats(&self) -> PumpStats {
        self.stats
    }

    /// 还有多少样本没播。
    pub fn buffered_samples(&self) -> usize {
        self.pending.len()
    }

    pub fn next_frame(&mut self) -> Result<PumpTick, String> {
        let frame = self.encoder.frame();
        if !self.started {
            if self.pending.is_empty() && self.finished {
                return Ok(PumpTick::Done);
            }
            if self.pending.len() < self.start_samples.max(frame) && !self.finished {
                return Ok(PumpTick::Waiting);
            }
            self.started = true;
        }
        if self.pending.len() >= frame || (self.finished && !self.pending.is_empty()) {
            let take = frame.min(self.pending.len());
            let block: Vec<i16> = self.pending.drain(..take).collect();
            let end = self.consumed + take as u64;
            let mut marks = Vec::new();
            // 上游已结束且这是最后一帧时，没有音频的句子标记也一并送出
            let last = self.finished && self.pending.is_empty();
            while self.marks.front().is_some_and(|(at, _)| *at < end || last) {
                marks.extend(self.marks.pop_front().map(|(_, mark)| mark));
            }
            self.consumed = end;
            self.stats.frames += 1;
            return self.encoder.encode(&block).map(|data| PumpTick::Frame { data, marks });
        }
        if self.finished {
            return Ok(PumpTick::Done);
        }
        // 播放中供不上：补一帧静音保持设备连续播放（不足一帧的尾巴留到下一拍凑满）
        self.stats.underruns += 1;
        self.encoder.encode(&[]).map(|data| PumpTick::Frame { data, marks: Vec::new() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::codec::OpusDecoder;
    use crate::audio::test_signal::{rms, tone};

    const RATE: u32 = 24_000;
    const FRAME: usize = 1440;

    fn pump(start_ms: u32) -> PlaybackPump {
        PlaybackPump::new(OpusEncoder::new(RATE, 60).unwrap(), RATE, start_ms)
    }

    fn drain(pump: &mut PlaybackPump) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        loop {
            match pump.next_frame().unwrap() {
                PumpTick::Frame { data, .. } => frames.push(data),
                PumpTick::Done => return frames,
                PumpTick::Waiting => panic!("已结束的上游不应等待"),
            }
        }
    }

    #[test]
    fn finished_stream_is_played_fully_with_padding() {
        let mut pump = pump(600);
        pump.begin();
        let input = tone(RATE, 440.0, 1000, 8000.0);
        pump.push(&input);
        pump.finish();
        let frames = drain(&mut pump);
        // 1000 ms = 16 帧 + 1 帧尾巴补零
        assert_eq!(frames.len(), 17);
        assert_eq!(pump.stats(), PumpStats { frames: 17, underruns: 0 });
        let mut decoder = OpusDecoder::new(RATE).unwrap();
        let output: Vec<i16> = frames.iter().flat_map(|frame| decoder.decode(frame).unwrap()).collect();
        assert_eq!(output.len(), 17 * FRAME);
        let ratio = rms(&output[FRAME..input.len()]) / rms(&input[FRAME..]);
        assert!((0.7..1.3).contains(&ratio), "{ratio}");
        assert_eq!(pump.next_frame().unwrap(), PumpTick::Done);
    }

    #[test]
    fn waits_for_start_buffer_then_fills_underruns_with_silence() {
        let mut pump = pump(300);
        pump.begin();
        pump.push(&tone(RATE, 440.0, 120, 8000.0));
        assert_eq!(pump.next_frame().unwrap(), PumpTick::Waiting, "未满 300 ms 启动缓冲");
        pump.push(&tone(RATE, 440.0, 200, 8000.0));
        // 320 ms：5 帧有内容，剩 20 ms 尾巴
        for _ in 0..5 {
            assert!(matches!(pump.next_frame().unwrap(), PumpTick::Frame { .. }));
        }
        assert!(matches!(pump.next_frame().unwrap(), PumpTick::Frame { .. }), "供不上时补静音而不是停");
        assert_eq!(pump.stats().underruns, 1);
        assert_eq!(pump.buffered_samples(), 480, "尾巴留到下一拍");
        pump.push(&tone(RATE, 440.0, 60, 8000.0));
        pump.finish();
        assert_eq!(drain(&mut pump).len(), 2);
        assert_eq!(pump.stats(), PumpStats { frames: 7, underruns: 1 });
    }

    #[test]
    fn sentence_marks_arrive_with_their_first_frame() {
        let mut pump = pump(0);
        pump.begin();
        pump.push_marked(&tone(RATE, 440.0, 100, 8000.0), "第一句".into());
        pump.push_marked(&tone(RATE, 440.0, 60, 8000.0), "第二句".into());
        pump.push_marked(&[], "空句".into());
        pump.finish();
        let mut seen = Vec::new();
        loop {
            match pump.next_frame().unwrap() {
                PumpTick::Frame { marks, .. } => seen.push(marks),
                PumpTick::Done => break,
                PumpTick::Waiting => unreachable!(),
            }
        }
        // 100 ms = 2400 样本：第 1 帧 0..1440 含第一句；第 2 帧 1440..2880 含第二句（起点 2400）；空句起点 3840 落在第 3 帧
        assert_eq!(seen, vec![vec!["第一句".to_owned()], vec!["第二句".to_owned()], vec!["空句".to_owned()]]);
        pump.begin();
        pump.push_marked(&tone(RATE, 440.0, 60, 8000.0), "打断前".into());
        pump.clear();
        pump.begin();
        pump.push(&tone(RATE, 440.0, 60, 8000.0));
        pump.finish();
        match pump.next_frame().unwrap() {
            PumpTick::Frame { marks, .. } => assert!(marks.is_empty(), "打断后旧句子的标记被清掉"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clear_drops_pending_audio_immediately() {
        let mut pump = pump(0);
        pump.begin();
        pump.push(&tone(RATE, 440.0, 2000, 8000.0));
        assert!(matches!(pump.next_frame().unwrap(), PumpTick::Frame { .. }));
        pump.clear();
        assert_eq!(pump.next_frame().unwrap(), PumpTick::Done);
        pump.push(&tone(RATE, 440.0, 200, 8000.0));
        assert_eq!(pump.buffered_samples(), 0, "打断后的迟到数据被丢弃");
        pump.begin();
        pump.push(&tone(RATE, 440.0, 120, 8000.0));
        pump.finish();
        assert_eq!(drain(&mut pump).len(), 2, "下一段播放正常");
    }
}
