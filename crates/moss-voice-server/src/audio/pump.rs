//! 播放节奏泵：把下行 PCM 切成 60 ms 帧编码，由会话按绝对时钟每 60 ms 取一帧发送。
//!
//! 思路取海门 `crates/haimen-xiaozhi/src/ws.rs` 与 `src/xiaozhi_asr_llm_tts.rs` 的连续性泵（`513d9e6`，MIT），按 MOSS 结构重写并消除句尾残帧冻结跳变：
//! - 启动前先攒够 `start_ms` 的音频（或上游已结束），避免合成刚开始时供不上；
//! - 句尾结束（`mark_sentence_end`）而下一句尚未合成就绪时，先把队列里的句尾残帧平滑补零播完再发静音保活，不把残帧冻在队列里吞尾字；句中流式分块或音乐解码短暂欠载时则保留未满一帧的半个音节，避免字中插零切碎音节；
//! - 编码前施加软限幅余量，避免满幅音频经 Opus 有损重构后在设备 100% 音量下削波破音；
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
    sample_rate: u32,
    start_samples: usize,
    pending: VecDeque<i16>,
    /// （从本段开始累计的样本位置, 句子文字）
    marks: VecDeque<(u64, String)>,
    pushed: u64,
    consumed: u64,
    started: bool,
    finished: bool,
    sentence_ended: bool,
    fade_in_on_resume: bool,
    stats: PumpStats,
    last_level: u8,
}

impl PlaybackPump {
    pub fn new(encoder: OpusEncoder, sample_rate: u32, start_ms: u32) -> Self {
        Self {
            encoder,
            sample_rate,
            start_samples: sample_rate as usize * start_ms as usize / 1000,
            pending: VecDeque::new(),
            marks: VecDeque::new(),
            pushed: 0,
            consumed: 0,
            started: false,
            finished: false,
            sentence_ended: false,
            fade_in_on_resume: false,
            stats: PumpStats::default(),
            last_level: 0,
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
        self.sentence_ended = false;
        self.fade_in_on_resume = false;
        self.stats = PumpStats::default();
        self.last_level = 0;
    }

    pub fn push(&mut self, pcm: &[i16]) {
        if !self.finished {
            if !pcm.is_empty() {
                self.sentence_ended = false;
            }
            self.pending.extend(pcm.iter().copied());
            self.pushed += pcm.len() as u64;
        }
    }

    /// 推入一句话：文字在这句音频的第一帧随帧返回。
    pub fn push_marked(&mut self, pcm: &[i16], mark: String) {
        if !self.finished {
            self.sentence_ended = false;
            self.marks.push_back((self.pushed, mark));
            self.push(pcm);
        }
    }

    /// 标记当前这句话的音频已全部推入（句尾残帧若遇句间欠载可立即冲刷播完，而句中流式分块半帧不冲刷）。
    pub fn mark_sentence_end(&mut self) {
        if !self.finished {
            self.sentence_ended = true;
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
        self.sentence_ended = false;
        self.fade_in_on_resume = false;
        self.encoder.reset();
        self.last_level = 0;
    }

    /// 尚未正式起播时，当前缓冲是否已满足启动阈值（供会话层在分块或首句收尾到达瞬间立即唤醒首帧，零等待）。
    pub fn ready_to_start(&self) -> bool {
        !self.started
            && (!self.pending.is_empty() || !self.marks.is_empty())
            && (self.pending.len() >= self.start_samples.max(self.encoder.frame())
                || self.finished
                || (!self.pending.is_empty() && self.sentence_ended))
    }

    /// 起播突发（burst）阶段是否还有完整数据帧或已收尾的句尾残帧可立即连发（避免配置极小缓冲时在 t=0 硬造欠载静音）。
    pub fn has_frame_for_burst(&self) -> bool {
        self.pending.len() >= self.encoder.frame()
            || (!self.pending.is_empty() && (self.finished || self.sentence_ended))
            || (self.finished && !self.marks.is_empty())
    }

    /// 最近一次送出的 60 ms 音频帧归一化音量（0..=100）；静音或结束时为 0。
    pub fn last_level(&self) -> u8 {
        self.last_level
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
            if self.pending.is_empty() && self.marks.is_empty() && self.finished {
                self.last_level = 0;
                return Ok(PumpTick::Done);
            }
            if self.pending.len() < self.start_samples.max(frame)
                && !self.finished
                && !(self.sentence_ended && !self.pending.is_empty())
            {
                return Ok(PumpTick::Waiting);
            }
            self.started = true;
        }
        if self.pending.len() >= frame
            || (!self.pending.is_empty() && (self.finished || self.sentence_ended))
        {
            let take = frame.min(self.pending.len());
            let mut block: Vec<i16> = self.pending.drain(..take).collect();
            if self.pending.is_empty() {
                self.sentence_ended = false;
            }
            if self.fade_in_on_resume {
                super::fade_head_i16(&mut block, self.sample_rate, super::UNDERRUN_FADE_MS);
                self.fade_in_on_resume = false;
            }
            if take < frame && !self.finished {
                // 当前句已收尾但下一句尚未就绪：把句尾残帧平滑微淡出后补零送出，不冻在队列里造成吞尾字或粘到下一句开头
                super::fade_tail_i16(&mut block, self.sample_rate, super::UNDERRUN_FADE_MS);
                self.fade_in_on_resume = true;
            }
            super::soft_limit_i16(&mut block);
            let end = self.consumed + take as u64;
            let mut marks = Vec::new();
            // 上游已结束且这是最后一帧时，没有音频的句子标记也一并送出
            let last = self.finished && self.pending.is_empty();
            while self.marks.front().is_some_and(|(at, _)| *at < end || last) {
                marks.extend(self.marks.pop_front().map(|(_, mark)| mark));
            }
            self.consumed = end;
            self.stats.frames += 1;
            self.last_level = super::pcm_rms_level(&block);
            return self
                .encoder
                .encode(&block)
                .map(|data| PumpTick::Frame { data, marks });
        }
        if self.finished {
            if !self.marks.is_empty() {
                let marks: Vec<String> = self.marks.drain(..).map(|(_, mark)| mark).collect();
                self.stats.frames += 1;
                self.last_level = 0;
                return self
                    .encoder
                    .encode(&[])
                    .map(|data| PumpTick::Frame { data, marks });
            }
            self.last_level = 0;
            return Ok(PumpTick::Done);
        }
        // 播放中上游暂时供不上（句间已排空，或句中流式分块尚差半帧）：发纯静音帧保活，恢复供流时做微淡入平滑衔接
        self.stats.underruns += 1;
        self.last_level = 0;
        self.fade_in_on_resume = true;
        self.encoder.encode(&[]).map(|data| PumpTick::Frame {
            data,
            marks: Vec::new(),
        })
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
        assert_eq!(
            pump.stats(),
            PumpStats {
                frames: 17,
                underruns: 0
            }
        );
        let mut decoder = OpusDecoder::new(RATE).unwrap();
        let output: Vec<i16> = frames
            .iter()
            .flat_map(|frame| decoder.decode(frame).unwrap())
            .collect();
        assert_eq!(output.len(), 17 * FRAME);
        let ratio = rms(&output[FRAME..input.len()]) / rms(&input[FRAME..]);
        assert!((0.7..1.3).contains(&ratio), "{ratio}");
        assert_eq!(pump.next_frame().unwrap(), PumpTick::Done);
    }

    #[test]
    fn waits_for_start_buffer_then_flushes_tail_and_fills_underruns_with_silence() {
        let mut pump = pump(300);
        pump.begin();
        pump.push(&tone(RATE, 440.0, 120, 8000.0));
        assert!(!pump.ready_to_start());
        assert_eq!(
            pump.next_frame().unwrap(),
            PumpTick::Waiting,
            "未满 300 ms 启动缓冲"
        );
        pump.push(&tone(RATE, 440.0, 200, 8000.0));
        assert!(pump.ready_to_start(), "凑满 300 ms 后立即可唤醒起播");
        // 320 ms = 5 整帧 + 20 ms（480 样本）残帧：未标句末时保留半帧音节等后续分块，不往词中插零切碎
        for _ in 0..5 {
            assert!(matches!(pump.next_frame().unwrap(), PumpTick::Frame { .. }));
        }
        assert_eq!(pump.buffered_samples(), 480);
        assert!(matches!(pump.next_frame().unwrap(), PumpTick::Frame { .. }));
        assert_eq!(pump.stats().underruns, 1, "句中欠载计入补静音并保留半帧");
        assert_eq!(pump.buffered_samples(), 480);
        // 标记当前句结束后，20 ms 句尾残帧立即平滑微淡出补零播完，不冻在队列里吞尾字
        pump.mark_sentence_end();
        assert!(matches!(pump.next_frame().unwrap(), PumpTick::Frame { .. }));
        assert_eq!(
            pump.buffered_samples(),
            0,
            "句尾残帧已平滑排空，不粘连到下一句开头"
        );
        assert_eq!(pump.stats().underruns, 1);
        // 队列已空但下一句仍未到：补纯静音保活帧
        assert!(
            matches!(pump.next_frame().unwrap(), PumpTick::Frame { .. }),
            "供不上时补静音而不是停"
        );
        assert_eq!(pump.stats().underruns, 2);
        pump.push(&tone(RATE, 440.0, 60, 8000.0));
        pump.finish();
        assert_eq!(drain(&mut pump).len(), 1);
        assert_eq!(
            pump.stats(),
            PumpStats {
                frames: 7,
                underruns: 2
            }
        );
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
        assert_eq!(
            seen,
            vec![
                vec!["第一句".to_owned()],
                vec!["第二句".to_owned()],
                vec!["空句".to_owned()]
            ]
        );
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
