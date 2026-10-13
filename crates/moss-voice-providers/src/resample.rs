//! 线性插值重采样（有状态，分块输入时块与块之间连续）。
//!
//! 只用于语音：内置语音服务的回声模式把上行 16 kHz 转为下行 24 kHz、把合成音频转为下行采样率；
//! 豆包流式识别（NV7）把非 16 kHz 的上行音频转为 16 kHz。线性插值对语音足够，且没有额外延迟。

pub struct Resampler {
    from: u32,
    to: u32,
    /// 下一个输出样本在当前块中的位置（输入样本为单位）；-1..0 之间表示落在上一块末样本与本块首样本之间
    position: f64,
    last: Option<i16>,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self { from, to, position: 0.0, last: None }
    }

    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        if self.from == self.to || input.is_empty() {
            return input.to_vec();
        }
        let step = self.from as f64 / self.to as f64;
        let last = self.last.unwrap_or(input[0]);
        let sample = |index: isize| if index < 0 { last } else { input[index as usize] };
        let mut output = Vec::with_capacity((input.len() as f64 / step) as usize + 2);
        loop {
            let base = self.position.floor();
            let index = base as isize;
            if index + 1 >= input.len() as isize {
                break;
            }
            let fraction = self.position - base;
            let value = sample(index) as f64 * (1.0 - fraction) + sample(index + 1) as f64 * fraction;
            output.push(value.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16);
            self.position += step;
        }
        self.position -= input.len() as f64;
        self.last = input.last().copied();
        output
    }
}

