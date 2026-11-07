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
    epoch: u64,
    seq: u64,
}
pub(crate) struct VadWorker {
    input: SyncSender<Work>,
    output: tokio::sync::mpsc::Receiver<VadFrame>,
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
                let mut vad = factory
                    .create(rate, &config)
                    .unwrap_or_else(|| Box::new(EnergyVad::new(rate, config)));
                let mut previous = 0;
                while let Ok(work) = jobs.recv() {
                    if current.load(Ordering::Acquire) != work.epoch
                        || work.at.elapsed() >= DEADLINE
                    {
                        continue;
                    }
                    if previous != work.epoch {
                        vad.reset();
                        previous = work.epoch;
                    }
                    let events = vad.accept(&work.samples);
                    let notice = vad.take_notice();
                    if current.load(Ordering::Acquire) != work.epoch {
                        continue;
                    }
                    if results
                        .blocking_send(VadFrame {
                            samples: work.samples,
                            events,
                            notice,
                            prime: work.prime,
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
        while self.output.try_recv().is_ok() {}
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
                continue;
            }
            self.waiting.pop_front();
            self.completed_seq = frame.seq;
            return Ok(frame);
        }
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
