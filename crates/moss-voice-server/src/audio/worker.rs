//! Blocking model IPC stays off the socket/playback executor. Frames retain their audio and epoch.
use super::{EnergyVad, VadConfig, VadEvent, VadFactory};
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
    mpsc::{SyncSender, sync_channel},
};
use std::time::{Duration, Instant};

const CAPACITY: usize = 64;
const DEADLINE: Duration = Duration::from_millis(1500);
static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct WorkerSlot;
impl Drop for WorkerSlot {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Work {
    epoch: u64,
    seq: u64,
    samples: Vec<i16>,
    prime: bool,
    at: Instant,
}
pub(crate) struct VadFrame {
    pub samples: Vec<i16>,
    pub events: Vec<VadEvent>,
    pub notice: Option<String>,
    pub prime: bool,
    /// 判停线程这期间要写进完整过程记录的事实（`kind`、细节）：实际用的判停方式、模型建不起来改用能量判停、
    /// 因超时跳过的帧、这一路判停流的降噪情况（n110）。随下一帧交给会话，不因帧作废而丢。
    pub trace: Vec<(&'static str, serde_json::Value)>,
    epoch: u64,
    seq: u64,
}
pub(crate) struct VadWorker {
    input: SyncSender<Work>,
    output: tokio::sync::mpsc::Receiver<VadFrame>,
    /// 作废帧（重置或过期轮次）带着的记录与提示：不随帧丢掉，并到下一帧交给会话（n110）
    carried: (Vec<(&'static str, serde_json::Value)>, Option<String>),
    epoch: Arc<AtomicU64>,
    next_seq: u64,
    completed_seq: u64,
    waiting: VecDeque<Instant>,
    max_samples: usize,
}
impl VadWorker {
    pub fn spawn(
        factory: Arc<dyn VadFactory>,
        rate: u32,
        config: VadConfig,
    ) -> Result<Self, String> {
        WORKERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 16).then_some(n + 1)
            })
            .map_err(|_| "本地语音工作线程已达上限")?;
        let slot = WorkerSlot;
        let (input, jobs) = sync_channel::<Work>(CAPACITY);
        let (results, output) = tokio::sync::mpsc::channel(CAPACITY);
        let epoch = Arc::new(AtomicU64::new(1));
        let current = epoch.clone();
        std::thread::Builder::new()
            .name("moss-vad-worker".into())
            .spawn(move || {
                let _slot = slot;
                let mut notes: Vec<(&'static str, serde_json::Value)> = Vec::new();
                let mut vad = match factory.create_explained(rate, &config) {
                    Ok(mut vad) => {
                        notes.push(("vad_engine", serde_json::json!({"engine": vad.engine()})));
                        if let Some(report) = vad.take_denoise() {
                            notes.push(("denoise", super::denoise_trace("判停", &report)));
                        }
                        vad
                    }
                    Err(reason) => {
                        let vad: Box<dyn super::Vad> = Box::new(EnergyVad::new(rate, config));
                        notes.push((
                            "vad_engine",
                            serde_json::json!({"engine": vad.engine(), "fallback_reason": reason}),
                        ));
                        vad
                    }
                };
                let mut engine = vad.engine();
                let mut previous = 1;
                let mut accepted_any = false;
                // 排队太久（超过 DEADLINE）没来得及判停就丢掉的帧：帧数与音频时长
                let mut skipped = (0u64, 0u64);
                // 判停后才发现这一帧已作废时没送出的出错提示：并到下一帧
                let mut pending_notice: Option<String> = None;
                while let Ok(work) = jobs.recv() {
                    if current.load(Ordering::Acquire) != work.epoch {
                        continue;
                    }
                    if work.at.elapsed() >= DEADLINE {
                        skipped.0 += 1;
                        skipped.1 += work.samples.len() as u64 * 1000 / u64::from(rate.max(1));
                        continue;
                    }
                    if skipped.0 > 0 {
                        notes.push((
                            "vad_skipped",
                            serde_json::json!({
                                "frames": skipped.0,
                                "audio_ms": skipped.1,
                                "deadline_ms": DEADLINE.as_millis() as u64,
                            }),
                        ));
                        skipped = (0, 0);
                    }
                    if previous != work.epoch {
                        if accepted_any {
                            vad.reset();
                        }
                        previous = work.epoch;
                    }
                    let events = vad.accept(&work.samples);
                    accepted_any = true;
                    let notice = merge_notice(pending_notice.take(), vad.take_notice());
                    if let Some(report) = vad.take_denoise() {
                        notes.push(("denoise", super::denoise_trace("判停", &report)));
                    }
                    let now = vad.engine();
                    if now != engine {
                        notes.push((
                            "vad_engine",
                            serde_json::json!({"engine": now, "previous": engine, "fallback_reason": notice}),
                        ));
                        engine = now;
                    }
                    if current.load(Ordering::Acquire) != work.epoch {
                        pending_notice = notice;
                        continue;
                    }
                    if results
                        .blocking_send(VadFrame {
                            samples: work.samples,
                            events,
                            notice,
                            prime: work.prime,
                            trace: std::mem::take(&mut notes),
                            epoch: work.epoch,
                            seq: work.seq,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|_| "无法创建本地语音处理线程")?;
        Ok(Self {
            input,
            output,
            carried: (Vec::new(), None),
            epoch,
            next_seq: 0,
            completed_seq: 0,
            waiting: VecDeque::new(),
            max_samples: rate as usize * 2,
        })
    }
    pub fn reset(&mut self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.completed_seq = self.next_seq;
        self.waiting.clear();
        while let Ok(frame) = self.output.try_recv() {
            self.carry(frame);
        }
    }

    /// 作废帧的判停记录与出错提示留给下一帧（帧本身的音频与事件照旧丢弃）。
    fn carry(&mut self, frame: VadFrame) {
        self.carried.0.extend(frame.trace);
        self.carried.1 = merge_notice(self.carried.1.take(), frame.notice);
    }
    pub fn submit(&mut self, samples: Vec<i16>, prime: bool) -> Result<(), String> {
        if self.waiting.len() >= CAPACITY || samples.len() > self.max_samples {
            return Err("语音处理积压，已停止本轮识别，请重新连接；紧急情况请使用实体急停".into());
        }
        let at = Instant::now();
        let seq = self.next_seq.wrapping_add(1);
        self.input
            .try_send(Work {
                epoch: self.epoch.load(Ordering::Acquire),
                seq,
                samples,
                prime,
                at,
            })
            .map_err(|_| "语音处理队列不可用")?;
        self.next_seq = seq;
        self.waiting.push_back(at);
        Ok(())
    }
    pub fn pending(&self) -> bool {
        !self.waiting.is_empty()
    }
    /// 若当前存在已入队但尚未由会话循环消费的帧，返回当前最后入队序号作为排空屏障。
    pub fn pending_target(&self) -> Option<u64> {
        (self.completed_seq < self.next_seq).then_some(self.next_seq)
    }
    pub fn completed_seq(&self) -> u64 {
        self.completed_seq
    }
    pub async fn next(&mut self) -> Result<VadFrame, String> {
        let Some(first) = self.waiting.front().copied() else {
            return std::future::pending().await;
        };
        let deadline = tokio::time::Instant::from_std(first + DEADLINE);
        loop {
            let frame = tokio::time::timeout_at(deadline, self.output.recv())
                .await
                .map_err(|_| "本地语音处理超时，已停止本轮识别；紧急情况请使用实体急停")?
                .ok_or("本地语音处理线程已退出")?;
            if frame.epoch != self.epoch.load(Ordering::Acquire) {
                self.carry(frame);
                continue;
            }
            self.waiting.pop_front();
            self.completed_seq = frame.seq;
            let mut frame = frame;
            if !self.carried.0.is_empty() {
                let mut trace = std::mem::take(&mut self.carried.0);
                trace.append(&mut frame.trace);
                frame.trace = trace;
            }
            frame.notice = merge_notice(self.carried.1.take(), frame.notice);
            return Ok(frame);
        }
    }
}
fn merge_notice(earlier: Option<String>, later: Option<String>) -> Option<String> {
    match (earlier, later) {
        (Some(earlier), Some(later)) => Some(format!("{earlier}；{later}")),
        (earlier, later) => earlier.or(later),
    }
}

impl Drop for VadWorker {
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Vad;
    struct SlowFactory;
    struct Slow;
    impl VadFactory for SlowFactory {
        fn create(&self, _: u32, _: &VadConfig) -> Option<Box<dyn Vad>> {
            Some(Box::new(Slow))
        }
    }
    impl Vad for Slow {
        fn accept(&mut self, _: &[i16]) -> Vec<VadEvent> {
            std::thread::sleep(Duration::from_millis(300));
            vec![VadEvent::SpeechStart]
        }
        fn reset(&mut self) {}
        fn in_speech(&self) -> bool {
            true
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn slow_vad_does_not_block_executor_and_old_epochs_are_discarded() {
        let mut worker =
            VadWorker::spawn(Arc::new(SlowFactory), 16000, VadConfig::default()).unwrap();
        worker.submit(vec![1; 960], false).unwrap();
        let began = Instant::now();
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert!(
            began.elapsed() < Duration::from_millis(200),
            "blocking model ran on async executor"
        );
        worker.reset();
        worker.submit(vec![2; 960], false).unwrap();
        let frame = worker.next().await.unwrap();
        assert_eq!(frame.samples, vec![2; 960]);
        assert!(!worker.pending());
    }
    /// 判停模型建不起来的原因随第一帧送出；这一帧被重置作废时，原因不丢、并到下一帧（n110 复审 4）。
    #[tokio::test(flavor = "current_thread")]
    async fn trace_notes_of_discarded_frames_reach_the_next_frame() {
        struct Broken;
        impl VadFactory for Broken {
            fn create(&self, _: u32, _: &VadConfig) -> Option<Box<dyn Vad>> {
                None
            }
            fn label(&self) -> String {
                "本地判停 测试".into()
            }
        }
        let mut worker = VadWorker::spawn(Arc::new(Broken), 16000, VadConfig::default()).unwrap();
        worker.submit(vec![0; 960], false).unwrap();
        // 等判停线程把第一帧（带着回退原因）放进输出，再重置把它作废
        for _ in 0..100 {
            if !worker.output.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!worker.output.is_empty());
        worker.reset();
        worker.submit(vec![0; 960], false).unwrap();
        let frame = worker.next().await.unwrap();
        let engine = frame
            .trace
            .iter()
            .find(|(kind, _)| *kind == "vad_engine")
            .expect("回退原因随下一帧送出");
        assert_eq!(engine.1["fallback_reason"], "本地判停 测试没建起来");
        assert_eq!(engine.1["engine"], super::super::ENERGY_VAD_LABEL);
        assert_eq!(
            merge_notice(Some("甲".into()), Some("乙".into())).as_deref(),
            Some("甲；乙")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queue_is_bounded_even_when_model_is_stalled() {
        let mut worker =
            VadWorker::spawn(Arc::new(SlowFactory), 16000, VadConfig::default()).unwrap();
        for _ in 0..CAPACITY {
            worker.submit(vec![0; 960], false).unwrap();
        }
        assert!(worker.submit(vec![0; 960], false).is_err());
    }
}
