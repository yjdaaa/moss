//! 本地模型的下载、校验与安装（内置语音技术书 §7，NV6）。
//!
//! - 模型不打进安装包，按需从 sherpa-onnx 官方发布地址下载到运行目录 `models/`；
//! - 下载时边写边算 SHA-256，与内置清单比对，不一致即删除，不安装；镜像地址可改，校验不变，所以换镜像也安全；
//! - 压缩包（tar.bz2）先解到临时目录，只接受普通文件与目录，拒绝绝对路径、`..`、链接，核对必需文件后再整体改名就位；
//! - 一个模型同一时间只有一个下载；取消立即生效（状态马上变为已取消，卡住的连接在线程结束时清理）；
//! - 判停、识别、合成各有几个可选模型（[`ModelKind`]），用户在语音中心选用；个别模型另需附加文件
//!   （Matcha 的声码器），与压缩包一起下载、各自校验后放进模型目录。

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};

/// sherpa-onnx 官方发布地址（可在语音中心换成镜像；校验值不变）
pub const DEFAULT_BASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";
/// Laya 多语言决策模型官方发布地址與国内镜像回退地址（SHA-256 校验值恒定不变）
pub const DEFAULT_LAYA_BASE: &str = "https://huggingface.co/convaiinnovations/laya-multilingual/resolve/main";
pub const DEFAULT_LAYA_MIRROR_BASE: &str = "https://hf-mirror.com/convaiinnovations/laya-multilingual/resolve/main";
/// OneJev 1.6 多模态决策模型 GGUF 官方发布地址（SHA-256 校验值恒定不变，支持 hf-mirror 自动回退）
pub const DEFAULT_ONEJEV_0_8B_BASE: &str = "https://huggingface.co/mradermacher/OneJev1.6-0.8B-GGUF/resolve/main";
pub const DEFAULT_ONEJEV_4B_BASE: &str = "https://huggingface.co/mradermacher/OneJev1.6-4B-GGUF/resolve/main";
pub const DEFAULT_ONEJEV_9B_BASE: &str = "https://huggingface.co/mradermacher/OneJev1.6-9B-GGUF/resolve/main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archive {
    /// 单个文件，原样放到 `models/<file>`
    File,
    /// tar.bz2，解出顶层目录 `models/<dir>`
    TarBz2 { dir: &'static str },
    /// 多文件模型目录，逐个文件校验 SHA-256 后组装为 `models/<dir>`
    Directory { dir: &'static str, base_url: &'static str },
}

/// 模型类别：判停、识别、合成、Jev 决策各选一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Vad,
    Asr,
    Tts,
    Jev,
    Speaker,
}

/// 辅助程序加载模型的方式（与 `moss-voice-local-proto` 的引擎及本地 Laya 决策服务对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    SileroVad,
    TenVad,
    /// 整段识别
    SenseVoice,
    /// 流式识别：边说边识别
    StreamingTransducer,
    Kokoro,
    Matcha,
    Laya,
    OneJev,
    CampPlus,
}

/// 与压缩包一起下载、放进模型目录的附加文件（同样校验 SHA-256）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtraFile {
    /// 相对发布地址的路径；放进模型目录时用其文件名
    pub path: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// 多音色合成模型的音色范围（单音色模型为空）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Voices {
    /// 默认音色编号（切换到该模型时界面填入）
    pub default: i32,
    /// 最大音色编号
    pub max: i32,
    /// 音色说明（哪些编号是中文女声 / 男声）
    pub hint: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: &'static str,
    pub kind: ModelKind,
    pub engine: Engine,
    pub label: &'static str,
    /// 优缺点（一两句，显示在模型名下方；速度为本机实测，见技术书 §13.7）
    pub note: &'static str,
    /// 相对发布地址的路径
    pub path: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub archive: Archive,
    /// 安装后必须存在的文件或目录（相对 `models/<dir>`；单文件模型为空）
    pub required: &'static [&'static str],
    /// 附加文件（只用于压缩包模型）
    pub extras: &'static [ExtraFile],
    pub voices: Option<Voices>,
}

impl ModelSpec {
    /// 需要下载的总字节数（含附加文件）。
    pub fn download_size(&self) -> u64 {
        self.size + self.extras.iter().map(|extra| extra.size).sum::<u64>()
    }
}

const KOKORO_FILES: [&str; 6] = ["voices.bin", "tokens.txt", "lexicon-zh.txt", "lexicon-us-en.txt", "espeak-ng-data", "dict"];
const LAYA_MULTILINGUAL_FILES: [&str; 6] = [
    "model.safetensors",
    "config.json",
    "rl_agent_config.json",
    "encoder/config.json",
    "tokenizer/tokenizer_config.json",
    "tokenizer/tokenizer.json",
];

const LAYA_MULTILINGUAL_EXTRAS: [ExtraFile; 5] = [
    ExtraFile {
        path: "config.json",
        sha256: "7add7c041b53f7091e65908553af79ab3f077946e5f2312d510ae13b17c7c134",
        size: 160,
    },
    ExtraFile {
        path: "rl_agent_config.json",
        sha256: "25061739243b617ad88d1219ba6f8a9c86c5881ca28df024fa2d9b3b2fcc30c6",
        size: 472,
    },
    ExtraFile {
        path: "encoder/config.json",
        sha256: "83f6916d13ef0f556ac461f28308dc2bffa7ebeadee8ec9e2db5812020ea5bb4",
        size: 1_938,
    },
    ExtraFile {
        path: "tokenizer/tokenizer_config.json",
        sha256: "424b69444bf7b5809dc2cd2e36d0bd71b8055124dd24274d6db3c655d38205e7",
        size: 502,
    },
    ExtraFile {
        path: "tokenizer/tokenizer.json",
        sha256: "609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f",
        size: 34_363_188,
    },
];

const ONEJEV_0_8B_FILES: [&str; 2] = [
    "OneJev1.6-0.8B.Q8_0.gguf",
    "OneJev1.6-0.8B.mmproj-Q8_0.gguf",
];
const ONEJEV_0_8B_EXTRAS: [ExtraFile; 1] = [ExtraFile {
    path: "OneJev1.6-0.8B.mmproj-Q8_0.gguf",
    sha256: "231e2ba51cd7f4c8092009428e249fd5cb2c2562a4c306cadba682f354fffdf4",
    size: 115_923_712,
}];

const ONEJEV_4B_FILES: [&str; 2] = [
    "OneJev1.6-4B.Q4_K_M.gguf",
    "OneJev1.6-4B.mmproj-Q8_0.gguf",
];
const ONEJEV_4B_EXTRAS: [ExtraFile; 1] = [ExtraFile {
    path: "OneJev1.6-4B.mmproj-Q8_0.gguf",
    sha256: "bb24088d1d1a051677a16811facb39a6cfd1f3fbb0e6139695fc5ab2f48dbe77",
    size: 366_894_656,
}];

const ONEJEV_9B_FILES: [&str; 2] = [
    "OneJev1.6-9B.Q4_K_M.gguf",
    "OneJev1.6-9B.mmproj-Q8_0.gguf",
];
const ONEJEV_9B_EXTRAS: [ExtraFile; 1] = [ExtraFile {
    path: "OneJev1.6-9B.mmproj-Q8_0.gguf",
    sha256: "40d295bdd324b1a1c8b716c6b4e6a88152db13338bba4097cac60ea536ed6cbd",
    size: 624_229_728,
}];

/// 内置清单。SHA-256 取自官方发布页对各资产给出的摘要（并与实际下载的文件逐一核对一致）。
/// 前三项是 NV6 的默认模型，其余为可选模型。
pub const CATALOG: [ModelSpec; 13] = [
    ModelSpec {
        id: "campplus_sv_zh_cn_16k_common",
        kind: ModelKind::Speaker, engine: Engine::CampPlus,
        label: "CampPlus 声纹（中文 16kHz）",
        note: "Apache-2.0；192维向量。仅准备模型，真人阈值未标定，身份识别默认关闭。",
        path: "speaker-recongition-models/3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx",
        sha256: "f682b514c05d947ee3fa91cd6ec6c5c7543479a128373fa29b1faedccd21fd11",
        size: 28_281_138, archive: Archive::File,
        required: &[], extras: &[], voices: None,
    },
    ModelSpec {
        id: "silero_vad",
        kind: ModelKind::Vad,
        engine: Engine::SileroVad,
        label: "Silero 判停",
        note: "老牌判停模型，稳定可靠，不容易被噪声误触发（推荐）。",
        path: "asr-models/silero_vad.onnx",
        sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
        size: 643_854,
        archive: Archive::File,
        required: &[],
        extras: &[],
        voices: None,
    },
    ModelSpec {
        id: "ten_vad",
        kind: ModelKind::Vad,
        engine: Engine::TenVad,
        label: "TEN-VAD 判停（声网）",
        note: "体积最小，开口后确认“开始说话”比 Silero 快；但突然的噪声偶尔会被当成一小段语音。许可证带附加条款：不得用于与声网竞争的产品。",
        path: "asr-models/ten-vad.onnx",
        sha256: "718cb7eef47e3cf5ddbe7e967a7503f46b8b469c0706872f494dfa921b486206",
        size: 332_211,
        archive: Archive::File,
        required: &[],
        extras: &[],
        voices: None,
    },
    ModelSpec {
        id: "sense_voice",
        kind: ModelKind::Asr,
        engine: Engine::SenseVoice,
        label: "SenseVoice 识别 2024 版（中英粤日韩）",
        note: "说完后整段识别，一句约 0.1 秒，带标点；支持中、英、粤、日、韩。",
        path: "asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2",
        sha256: "7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e",
        size: 163_002_883,
        archive: Archive::TarBz2 { dir: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17" },
        required: &["model.int8.onnx", "tokens.txt"],
        extras: &[],
        voices: None,
    },
    ModelSpec {
        id: "sense_voice_2025",
        kind: ModelKind::Asr,
        engine: Engine::SenseVoice,
        label: "SenseVoice 识别 2025 版（中英粤日韩）",
        note: "2024 版的官方更新版，用法相同，一句约 0.15 秒；本机实测输出不带标点（不影响对话）。",
        path: "asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2",
        sha256: "7305f7905bfcf77fa0b39388a313f3da35c68d971661a65475b56fb2162c8e63",
        size: 165_783_878,
        archive: Archive::TarBz2 { dir: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09" },
        required: &["model.int8.onnx", "tokens.txt"],
        extras: &[],
        voices: None,
    },
    ModelSpec {
        id: "x_asr_streaming",
        kind: ModelKind::Asr,
        engine: Engine::StreamingTransducer,
        label: "X-ASR 流式识别（中英，带标点）",
        note: "边说边识别，说完约 0.05–0.1 秒就出字，回答最快；只认中文和英文，启动时加载约 3.5 秒。",
        path: "asr-models/sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05.tar.bz2",
        sha256: "fa5f63d618e5a01526e275a358bb7772e403f84808a4769fba52cffd8160bf74",
        size: 133_895_136,
        archive: Archive::TarBz2 { dir: "sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05" },
        required: &["encoder.int8.onnx", "decoder.onnx", "joiner.int8.onnx", "tokens.txt"],
        extras: &[],
        voices: None,
    },
    ModelSpec {
        id: "kokoro",
        kind: ModelKind::Tts,
        engine: Engine::Kokoro,
        label: "Kokoro 合成 v1.0（中英，多音色）",
        note: "音质自然，8 个中文音色；体积大，本机合成 1 秒语音约需 0.5 秒。",
        path: "tts-models/kokoro-multi-lang-v1_0.tar.bz2",
        sha256: "c5f7e2d2caf082bc1d20fb70334a61d99d20b484500aad32e7cf84c128ea3298",
        size: 349_906_910,
        archive: Archive::TarBz2 { dir: "kokoro-multi-lang-v1_0" },
        required: &["model.onnx", KOKORO_FILES[0], KOKORO_FILES[1], KOKORO_FILES[2], KOKORO_FILES[3], KOKORO_FILES[4], KOKORO_FILES[5]],
        extras: &[],
        voices: Some(Voices { default: 50, max: 53, hint: "45–48 中文女声，49–52 中文男声" }),
    },
    ModelSpec {
        id: "kokoro_v11",
        kind: ModelKind::Tts,
        engine: Engine::Kokoro,
        label: "Kokoro 合成 v1.1 int8（中英，100 个中文音色）",
        note: "中文音色最多，体积不到 v1.0 的一半；但本机合成偏慢（1 秒语音约需 1 秒），慢电脑上等待明显。",
        path: "tts-models/kokoro-int8-multi-lang-v1_1.tar.bz2",
        sha256: "a1e94694776049035c4f2c6529f003aaece993c76aae9a78995831c3c4dcafc6",
        size: 147_031_220,
        archive: Archive::TarBz2 { dir: "kokoro-int8-multi-lang-v1_1" },
        required: &["model.int8.onnx", KOKORO_FILES[0], KOKORO_FILES[1], KOKORO_FILES[2], KOKORO_FILES[3], KOKORO_FILES[4], KOKORO_FILES[5]],
        extras: &[],
        voices: Some(Voices { default: 3, max: 102, hint: "3–57 中文女声，58–102 中文男声" }),
    },
    ModelSpec {
        id: "matcha_zh_en",
        kind: ModelKind::Tts,
        engine: Engine::Matcha,
        label: "Matcha 合成（中英）",
        note: "最快，合成 1 秒语音约需 0.04 秒，适合配置低的电脑；只有一个女声，音质一般（16 kHz）。",
        path: "tts-models/matcha-icefall-zh-en.tar.bz2",
        sha256: "271b804af570400d3bcdcb53bf6e53cc9f75180ee763b9f13eb5eaf2b0d086ef",
        size: 79_033_838,
        archive: Archive::TarBz2 { dir: "matcha-icefall-zh-en" },
        required: &["model-steps-3.onnx", "tokens.txt", "lexicon.txt", "espeak-ng-data", "vocos-16khz-univ.onnx"],
        extras: &[ExtraFile {
            path: "vocoder-models/vocos-16khz-univ.onnx",
            sha256: "b599142a1fb8ff03de3e84ac35ff537c619e56f4267a6fe894851a42844acf9e",
            size: 53_882_848,
        }],
        voices: None,
    },
    ModelSpec {
        id: "onejev_1_6_0_8b",
        kind: ModelKind::Jev,
        engine: Engine::OneJev,
        label: "OneJev 1.6 · 0.8B 多模态极速决策（推荐）",
        note: "Apache-2.0；<2.5B 判别榜第一（Q8_0+视觉投影）。Mac 走 Apple Metal、Win 走 Vulkan/CUDA 硬件加速，免装 Python，稳压 360ms 实时语音红线且支持看图质检。",
        path: "OneJev1.6-0.8B.Q8_0.gguf",
        sha256: "1225c4f8d907636c2d4725ae5ca33a2ba32a64e84efc6a09052839790970b3ae",
        size: 811_844_096,
        archive: Archive::Directory {
            dir: "onejev_1_6_0_8b",
            base_url: DEFAULT_ONEJEV_0_8B_BASE,
        },
        required: &ONEJEV_0_8B_FILES,
        extras: &ONEJEV_0_8B_EXTRAS,
        voices: None,
    },
    ModelSpec {
        id: "onejev_1_6_4b",
        kind: ModelKind::Jev,
        engine: Engine::OneJev,
        label: "OneJev 1.6 · 4B 多模态均衡决策",
        note: "Apache-2.0；3B–6B 判别榜第一（Q4_K_M+视觉投影）。适合 8GB+ 独显（Win）或 16GB+ 统一内存（Mac Metal）的高精度加工反馈与复杂语义分诊。",
        path: "OneJev1.6-4B.Q4_K_M.gguf",
        sha256: "8a12b976e7d2ea60fb603720ce07e2ef5e85b9b8a2a5519cb0bf2cc72aacac8d",
        size: 2_708_805_024,
        archive: Archive::Directory {
            dir: "onejev_1_6_4b",
            base_url: DEFAULT_ONEJEV_4B_BASE,
        },
        required: &ONEJEV_4B_FILES,
        extras: &ONEJEV_4B_EXTRAS,
        voices: None,
    },
    ModelSpec {
        id: "onejev_1_6_9b",
        kind: ModelKind::Jev,
        engine: Engine::OneJev,
        label: "OneJev 1.6 · 9B 多模态旗舰决策",
        note: "Apache-2.0；6B–12B 判别榜第一（Q4_K_M+视觉投影）。适合高配显卡或大内存 Mac 的离线看图质检与加工调参；实时语音若超 750ms 会自动无感回退主模型。",
        path: "OneJev1.6-9B.Q4_K_M.gguf",
        sha256: "2b3b2007465427fafb5060f0d5172d9c37c2349021f22532ae741fd823f0d738",
        size: 5_629_109_696,
        archive: Archive::Directory {
            dir: "onejev_1_6_9b",
            base_url: DEFAULT_ONEJEV_9B_BASE,
        },
        required: &ONEJEV_9B_FILES,
        extras: &ONEJEV_9B_EXTRAS,
        voices: None,
    },
    ModelSpec {
        id: "laya_multilingual",
        kind: ModelKind::Jev,
        engine: Engine::Laya,
        label: "Laya 多语言决策（322M，中英 100+ 语言）",
        note: "Apache-2.0；纯文本非自回归 System 1 决策模型，需外部 Python laya[serve] 环境（Win 走 CUDA、Mac 走 Apple MPS），兼容 /v1/systemone。",
        path: "model.safetensors",
        sha256: "9d628fd971b700382ac6f65920a86f149777b2e748e0c955fb3b19695aa8f204",
        size: 643_835_514,
        archive: Archive::Directory {
            dir: "laya_multilingual",
            base_url: DEFAULT_LAYA_BASE,
        },
        required: &LAYA_MULTILINGUAL_FILES,
        extras: &LAYA_MULTILINGUAL_EXTRAS,
        voices: None,
    },
];

pub fn spec(id: &str) -> Option<&'static ModelSpec> {
    CATALOG.iter().find(|spec| spec.id == id)
}

/// 指定类别中的模型（id 不存在或类别不符时为空）。
pub fn spec_of(id: &str, kind: ModelKind) -> Option<&'static ModelSpec> {
    spec(id).filter(|spec| spec.kind == kind)
}

fn base_name(path: &'static str) -> &'static str {
    path.rsplit('/').next().unwrap_or(path)
}

fn file_name(spec: &ModelSpec) -> &'static str {
    base_name(spec.path)
}

/// 安装后的位置（单文件为文件路径，压缩包为目录）。
pub fn install_path(root: &Path, spec: &ModelSpec) -> PathBuf {
    match spec.archive {
        Archive::File => root.join(file_name(spec)),
        Archive::TarBz2 { dir } | Archive::Directory { dir, .. } => root.join(dir),
    }
}

/// 模型是否已完整安装。
pub fn installed(root: &Path, spec: &ModelSpec) -> bool {
    let path = install_path(root, spec);
    match spec.archive {
        Archive::File => path.metadata().map(|meta| meta.is_file() && meta.len() == spec.size).unwrap_or(false),
        Archive::TarBz2 { .. } => path.is_dir() && spec.required.iter().all(|item| path.join(item).exists()),
        Archive::Directory { .. } => {
            path.is_dir()
                && path.join(spec.path).metadata().map(|m| m.is_file() && m.len() == spec.size).unwrap_or(false)
                && spec.extras.iter().all(|extra| {
                    path.join(extra.path).metadata().map(|m| m.is_file() && m.len() == extra.size).unwrap_or(false)
                })
                && spec.required.iter().all(|item| path.join(item).exists())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Progress {
    Downloading { done: u64, total: u64 },
    /// OneJev：正在下载三档共用的推理引擎 llama-server（2026-11-07）
    PreparingRuntime { done: u64, total: u64 },
    /// OneJev：另一档正在安装推理引擎，排队等待（可取消）
    WaitingRuntime,
    Verifying,
    Installing,
    Failed { message: String },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelStatus {
    pub id: &'static str,
    pub kind: ModelKind,
    pub engine: Engine,
    pub label: &'static str,
    pub note: &'static str,
    /// 下载大小（含附加文件）
    pub size: u64,
    /// 是否边说边识别
    pub streaming: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voices: Option<Voices>,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
}

struct Task {
    progress: Progress,
    cancel: Arc<AtomicBool>,
    /// 递增的任务号：旧线程结束时发现已不是当前任务就不改状态
    generation: u64,
}

pub struct ModelManager {
    root: PathBuf,
    overrides: HashMap<&'static str, PathBuf>,
    tasks: Arc<Mutex<HashMap<&'static str, Task>>>,
    generation: std::sync::atomic::AtomicU64,
    /// 安装最后一步（改名就位）与删除互斥：删除先拿到锁则安装看到取消标志放弃，安装先完成则删除随后删掉（2026-11-07）
    install_lock: Arc<Mutex<()>>,
}

impl ModelManager {
    /// `root` 为运行目录下的 `models/`。
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            overrides: HashMap::new(),
            tasks: Arc::default(),
            generation: std::sync::atomic::AtomicU64::new(1),
            install_lock: Arc::default(),
        }
    }

    /// 为指定模型 id 覆盖存放父目录（例如通过 `MOSS_LAYA_ROOT` 指向 `<path>/models`）。
    pub fn with_root_override(mut self, id: &'static str, custom_root: PathBuf) -> Self {
        self.overrides.insert(id, custom_root);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn root_for(&self, id: &str) -> &Path {
        self.overrides.get(id).map(PathBuf::as_path).unwrap_or(&self.root)
    }

    pub fn path_if_installed(&self, id: &str) -> Option<PathBuf> {
        let spec = spec(id)?;
        let root = self.root_for(id);
        installed(root, spec).then(|| install_path(root, spec))
    }

    pub fn status(&self) -> Vec<ModelStatus> {
        let tasks = self.tasks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        CATALOG
            .iter()
            .map(|spec| {
                let root = self.root_for(spec.id);
                ModelStatus {
                    id: spec.id,
                    kind: spec.kind,
                    engine: spec.engine,
                    label: spec.label,
                    note: spec.note,
                    size: spec.download_size(),
                    streaming: spec.engine == Engine::StreamingTransducer,
                    voices: spec.voices,
                    installed: installed(root, spec),
                    progress: tasks.get(spec.id).map(|task| task.progress.clone()),
                }
            })
            .collect()
    }

    fn busy(progress: &Progress) -> bool {
        matches!(
            progress,
            Progress::Downloading { .. }
                | Progress::PreparingRuntime { .. }
                | Progress::WaitingRuntime
                | Progress::Verifying
                | Progress::Installing
        )
    }

    /// 本进程管理的所有模型目录（默认目录 + 外置覆盖目录，去重）。
    fn roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.root.clone()];
        for root in self.overrides.values() {
            if !roots.contains(root) {
                roots.push(root.clone());
            }
        }
        roots
    }

    /// 清理上次运行中途退出（强关、崩溃、断电）留下的半截文件：`.download/`、`.extract-*`、`.trash-*`。
    /// 只在没有下载任务时执行（桌面启动时调用）；返回清掉的条目数（2026-11-07）。
    pub fn clean_leftovers(&self) -> usize {
        let idle = self
            .tasks
            .lock()
            .map(|tasks| !tasks.values().any(|task| Self::busy(&task.progress)))
            .unwrap_or(false);
        if !idle {
            return 0;
        }
        let mut removed = 0;
        for root in self.roots() {
            let Ok(entries) = fs::read_dir(&root) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !(name == ".download" || name.starts_with(".extract-") || name.starts_with(".trash-")) {
                    continue;
                }
                let path = entry.path();
                let is_real_dir = fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir());
                let result = if is_real_dir { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
                if result.is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// 同一目录下是否还有已安装或正在下载的 OneJev（决定能否删掉共用的推理引擎）。
    fn onejev_still_needed(&self, root: &Path) -> bool {
        let tasks = self.tasks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        CATALOG.iter().filter(|spec| spec.engine == Engine::OneJev && self.root_for(spec.id) == root).any(|spec| {
            installed(root, spec) || tasks.get(spec.id).is_some_and(|task| Self::busy(&task.progress))
        })
    }

    /// 开始下载（后台线程）。`base` 为发布地址或镜像。
    pub fn download(&self, id: &str, base: &str) -> Result<(), String> {
        let spec = *spec(id).ok_or_else(|| format!("未知模型：{id}"))?;
        self.download_spec(spec, base)
    }

    /// 与 [`download`](Self::download) 相同，但使用给定的清单项（测试用）。
    pub fn download_spec(&self, spec: ModelSpec, base: &str) -> Result<(), String> {
        let trimmed_base = base.trim().trim_end_matches('/');
        if !(trimmed_base.starts_with("https://") || trimmed_base.starts_with("http://"))
            || trimmed_base.contains(char::is_whitespace)
        {
            return Err("下载地址必须以 http:// 或 https:// 开头".into());
        }
        let candidate_bases = resolve_download_bases(&spec, trimmed_base);
        let cancel = Arc::new(AtomicBool::new(false));
        let generation = self.generation.fetch_add(1, Ordering::SeqCst);
        {
            let mut tasks = self.tasks.lock().map_err(|_| "内部错误".to_owned())?;
            if tasks.get(spec.id).is_some_and(|task| Self::busy(&task.progress)) {
                return Err("该模型正在下载".into());
            }
            tasks.insert(
                spec.id,
                Task {
                    progress: Progress::Downloading { done: 0, total: spec.download_size() },
                    cancel: cancel.clone(),
                    generation,
                },
            );
        }
        let root = self.root_for(spec.id).to_path_buf();
        let tasks = self.tasks.clone();
        let install_lock = self.install_lock.clone();
        std::thread::Builder::new()
            .name(format!("model-download-{}", spec.id))
            .spawn(move || {
                let report = |progress: Progress| {
                    if let Ok(mut tasks) = tasks.lock()
                        && let Some(task) = tasks.get_mut(spec.id)
                        && task.generation == generation
                        && !matches!(task.progress, Progress::Cancelled)
                    {
                        task.progress = progress;
                    }
                };
                let result = run_download(&root, &spec, &candidate_bases, generation, &cancel, &install_lock, &report);
                if let Ok(mut tasks) = tasks.lock()
                    && let Some(task) = tasks.get(spec.id)
                    && task.generation == generation
                {
                    match result {
                        // 安装完成：去掉任务，状态由“已安装”体现
                        Ok(()) => {
                            tasks.remove(spec.id);
                        }
                        Err(message) if !cancel.load(Ordering::SeqCst) => {
                            if let Some(task) = tasks.get_mut(spec.id) {
                                task.progress = Progress::Failed { message };
                            }
                        }
                        Err(_) => {}
                    }
                }
            })
            .map_err(|error| format!("创建下载线程失败：{error}"))?;
        Ok(())
    }

    pub fn cancel(&self, id: &str) {
        if let Ok(mut tasks) = self.tasks.lock()
            && let Some(task) = tasks.get_mut(id)
            && Self::busy(&task.progress)
        {
            task.cancel.store(true, Ordering::SeqCst);
            task.progress = Progress::Cancelled;
        }
    }

    /// 删除已安装的模型（下载中时先取消）。
    ///
    /// 先把模型整体改名到 `.trash-*` 再删：文件被占用时改名失败、模型原样保留，不会留下删了一半的目录；
    /// 改名后删不掉的残留在下次启动时由 [`clean_leftovers`](Self::clean_leftovers) 清掉。
    /// 删掉同目录最后一档 OneJev 时，一并删除 MOSS 自动下载的推理引擎（2026-11-07）。
    pub fn delete(&self, id: &str) -> Result<(), String> {
        let spec = spec(id).ok_or_else(|| format!("未知模型：{id}"))?;
        self.cancel(id);
        let root = self.root_for(spec.id).to_path_buf();
        {
            let _guard = self.install_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Ok(mut tasks) = self.tasks.lock() {
                tasks.remove(spec.id);
            }
            remove_installed(&root, spec)?;
        }
        if spec.engine == Engine::OneJev && !self.onejev_still_needed(&root) {
            crate::onejev::remove_managed_runtime(&root)?;
        }
        Ok(())
    }
}

/// 把已安装的模型改名到回收目录后删除；不存在时视为成功。
fn remove_installed(root: &Path, spec: &ModelSpec) -> Result<(), String> {
    let path = install_path(root, spec);
    if fs::symlink_metadata(&path).is_err() {
        return Ok(());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let trash = root.join(format!(".trash-{}-{stamp}", spec.id));
    fs::rename(&path, &trash).map_err(|error| format!("删除失败（模型文件可能正被其他程序占用，关闭后重试）：{error}"))?;
    let is_real_dir = fs::symlink_metadata(&trash).is_ok_and(|m| m.file_type().is_dir());
    let _ = if is_real_dir { fs::remove_dir_all(&trash) } else { fs::remove_file(&trash) };
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 一个要下载的文件（模型本体或附加文件）。
struct Item {
    path: &'static str,
    sha256: &'static str,
    size: u64,
}

fn hf_mirror_for(base_url: &str) -> String {
    match base_url.strip_prefix("https://huggingface.co") {
        Some(suffix) => format!("https://hf-mirror.com{suffix}"),
        None => DEFAULT_LAYA_MIRROR_BASE.to_owned(),
    }
}

/// 为给定模型解析下载候选地址列表（主地址 + 镜像兜底）。
/// `Archive::Directory`（如 HuggingFace 上的 `OneJev` / `laya_multilingual`）不与 sherpa-onnx GitHub Release 混用路径。
fn resolve_download_bases(spec: &ModelSpec, trimmed_base: &str) -> Vec<String> {
    match spec.archive {
        Archive::Directory { base_url, .. } => {
            let mirror_url = hf_mirror_for(base_url);
            if trimmed_base == DEFAULT_BASE {
                vec![base_url.to_owned(), mirror_url]
            } else if trimmed_base.contains("k2-fsa/sherpa-onnx") {
                // 用户填写的是 sherpa-onnx GitHub 发布镜像：目录模型优先走对应仓库的 HF 国内镜像，再兜底官方 HF
                vec![mirror_url, base_url.to_owned()]
            } else if let Some(suffix) = base_url.strip_prefix("https://huggingface.co")
                && !trimmed_base.contains("/resolve/")
                && (trimmed_base.contains("hf-mirror") || trimmed_base.contains("huggingface"))
            {
                let rewritten = format!("{trimmed_base}{suffix}");
                if rewritten == base_url {
                    vec![rewritten, mirror_url]
                } else {
                    vec![rewritten, base_url.to_owned()]
                }
            } else if trimmed_base == base_url {
                vec![base_url.to_owned(), mirror_url]
            } else if trimmed_base == mirror_url {
                vec![mirror_url, base_url.to_owned()]
            } else {
                vec![trimmed_base.to_owned()]
            }
        }
        _ => vec![trimmed_base.to_owned()],
    }
}

fn is_loopback_test_base(base: &str) -> bool {
    base.starts_with("http://127.0.0.1") || base.starts_with("http://localhost") || base.starts_with("http://[::1]")
}

fn run_download(
    root: &Path,
    spec: &ModelSpec,
    bases: &[String],
    generation: u64,
    cancel: &AtomicBool,
    install_lock: &Mutex<()>,
    report: &dyn Fn(Progress),
) -> Result<(), String> {
    let staging = root.join(".download");
    fs::create_dir_all(&staging).map_err(|error| format!("创建下载目录失败：{error}"))?;
    let items: Vec<Item> = std::iter::once(Item { path: spec.path, sha256: spec.sha256, size: spec.size })
        .chain(spec.extras.iter().map(|extra| Item { path: extra.path, sha256: extra.sha256, size: extra.size }))
        .collect();
    let parts: Vec<PathBuf> = items
        .iter()
        .enumerate()
        // 文件名带任务号：取消后立刻重下时，旧线程收尾不会删掉新任务的临时文件
        .map(|(idx, item)| staging.join(format!("{}-{generation}-{idx}-{}.part", spec.id, base_name(item.path))))
        .collect();
    let total = spec.download_size();
    let result = (|| {
        if spec.engine == Engine::OneJev && !bases.iter().any(|b| is_loopback_test_base(b)) {
            crate::onejev::ensure_llama_runtime(root, bases.first().map(String::as_str), cancel, &|progress| match progress {
                crate::onejev::RuntimeProgress::Waiting => report(Progress::WaitingRuntime),
                crate::onejev::RuntimeProgress::Downloading { done, total } => report(Progress::PreparingRuntime { done, total }),
            })?;
            report(Progress::Downloading { done: 0, total });
        }
        let mut offset = 0;
        for (item, part) in items.iter().zip(&parts) {
            let progress = |done: u64| report(Progress::Downloading { done: offset + done, total });
            let mut last_err = None;
            for (idx, base) in bases.iter().enumerate() {
                if idx > 0 && cancel.load(Ordering::SeqCst) {
                    break;
                }
                let url = format!("{base}/{}", item.path);
                match fetch(item, &url, part, cancel, &progress) {
                    Ok(()) => {
                        last_err = None;
                        break;
                    }
                    Err(err) => {
                        last_err = Some(err);
                    }
                }
            }
            if let Some(err) = last_err {
                return Err(err);
            }
            offset += item.size;
        }
        report(Progress::Verifying);
        report(Progress::Installing);
        install(root, spec, &parts[0], &parts[1..], generation, cancel, install_lock)
    })();
    for part in &parts {
        let _ = fs::remove_file(part);
    }
    result
}

fn is_private_or_loopback_host(host: &str) -> bool {
    let clean = host.trim_matches(['[', ']']);
    if clean.eq_ignore_ascii_case("localhost") || clean.ends_with(".localhost") || clean.ends_with(".local") {
        return true;
    }
    if let Ok(ip) = clean.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        };
    }
    false
}

fn safe_model_redirect_policy(initial_loopback: bool, initial_https: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("重定向次数过多");
        }
        let next = attempt.url();
        if initial_https && next.scheme() != "https" {
            return attempt.error("禁止从 HTTPS 降级重定向到非加密地址");
        }
        if !matches!(next.scheme(), "https" | "http") {
            return attempt.error("不支持的重定向协议");
        }
        let Some(host) = next.host_str() else {
            return attempt.error("重定向地址缺少主机名");
        };
        if !initial_loopback && is_private_or_loopback_host(host) {
            return attempt.error("禁止重定向到本机或局域网地址");
        }
        attempt.follow()
    })
}

pub(crate) fn fetch_verified_file(
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    part: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
) -> Result<(), String> {
    let initial_loopback = is_loopback_test_base(url);
    let initial_https = url.starts_with("https://");
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(None::<Duration>)
        // GitHub / HuggingFace 发布资产会跳转到 CDN 存储地址；禁止降级协议与内网 SSRF，完整性由 SHA-256 保证
        .redirect(safe_model_redirect_policy(initial_loopback, initial_https))
        .build()
        .map_err(|_| "无法创建网络客户端".to_owned())?;
    let response = client.get(url).send().map_err(|error| {
        if error.is_timeout() {
            "连接超时，可换用镜像地址后重试".to_owned()
        } else {
            "无法连接下载地址，可检查网络或换用镜像地址".to_owned()
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("下载地址返回 HTTP {}", status.as_u16()));
    }
    let mut response = response;
    let mut file = fs::File::create(part).map_err(|error| format!("写入失败：{error}"))?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut last_report = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        let read = response.read(&mut buffer).map_err(|_| "下载中断，请重试".to_owned())?;
        if read == 0 {
            break;
        }
        done += read as u64;
        if done > expected_size {
            return Err("下载的文件比预期大，已停止".into());
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read]).map_err(|error| format!("写入失败（磁盘空间不足？）：{error}"))?;
        if done - last_report >= 1024 * 1024 || done == expected_size {
            last_report = done;
            progress(done);
        }
    }
    file.flush().map_err(|error| format!("写入失败：{error}"))?;
    drop(file);
    if done != expected_size {
        return Err(format!("下载不完整（{done} / {expected_size} 字节），请重试"));
    }
    let digest = hex(&hasher.finalize());
    if digest != expected_sha256 {
        return Err("SHA-256 校验不一致，文件已删除（可能被篡改或下载出错）".into());
    }
    Ok(())
}

fn fetch(item: &Item, url: &str, part: &Path, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Result<(), String> {
    fetch_verified_file(url, item.sha256, item.size, part, cancel, progress)
}

/// 压缩包内的路径是否安全（相对路径、无 `..`）。
pub(crate) fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// 安装最后一步：持锁复核取消标志后把成品改名就位（与 [`ModelManager::delete`] 互斥）。
fn place(install_lock: &Mutex<()>, cancel: &AtomicBool, from: &Path, target: &Path, is_dir: bool) -> Result<(), String> {
    let _guard = install_lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if cancel.load(Ordering::SeqCst) {
        return Err("已取消".to_owned());
    }
    let _ = if is_dir { fs::remove_dir_all(target) } else { fs::remove_file(target) };
    fs::rename(from, target).map_err(|error| format!("安装失败：{error}"))
}

fn install(
    root: &Path,
    spec: &ModelSpec,
    part: &Path,
    extras: &[PathBuf],
    generation: u64,
    cancel: &AtomicBool,
    install_lock: &Mutex<()>,
) -> Result<(), String> {
    let target = install_path(root, spec);
    match spec.archive {
        Archive::File => place(install_lock, cancel, part, &target, false),
        Archive::TarBz2 { dir } => {
            let extract = root.join(format!(".extract-{}-{generation}", spec.id));
            let _ = fs::remove_dir_all(&extract);
            fs::create_dir_all(&extract).map_err(|error| format!("创建解压目录失败：{error}"))?;
            let result = (|| {
                let file = fs::File::open(part).map_err(|error| format!("读取压缩包失败：{error}"))?;
                let mut archive = tar::Archive::new(bzip2::read::BzDecoder::new(std::io::BufReader::new(file)));
                let entries = archive.entries().map_err(|_| "压缩包无法读取".to_owned())?;
                for entry in entries {
                    if cancel.load(Ordering::SeqCst) {
                        return Err("已取消".to_owned());
                    }
                    let mut entry = entry.map_err(|_| "压缩包已损坏".to_owned())?;
                    let path = entry.path().map_err(|_| "压缩包路径无效".to_owned())?.into_owned();
                    if !safe_relative(&path) {
                        return Err(format!("压缩包含不安全的路径：{}", path.display()));
                    }
                    let kind = entry.header().entry_type();
                    if !(kind.is_file() || kind.is_dir()) {
                        // 链接等特殊条目一律跳过
                        continue;
                    }
                    if !entry.unpack_in(&extract).map_err(|error| format!("解压失败：{error}"))? {
                        return Err(format!("压缩包含不安全的路径：{}", path.display()));
                    }
                }
                let unpacked = extract.join(dir);
                // 附加文件放进模型目录（已校验）
                for (extra, file) in spec.extras.iter().zip(extras) {
                    fs::create_dir_all(&unpacked).map_err(|error| format!("安装失败：{error}"))?;
                    fs::rename(file, unpacked.join(base_name(extra.path))).map_err(|error| format!("安装失败：{error}"))?;
                }
                if let Some(missing) = spec.required.iter().find(|item| !unpacked.join(item).exists()) {
                    return Err(format!("压缩包缺少 {missing}"));
                }
                place(install_lock, cancel, &unpacked, &target, true)
            })();
            let _ = fs::remove_dir_all(&extract);
            result
        }
        Archive::Directory { dir, .. } => {
            let extract = root.join(format!(".extract-{}-{generation}", spec.id));
            let _ = fs::remove_dir_all(&extract);
            fs::create_dir_all(&extract).map_err(|error| format!("创建安装目录失败：{error}"))?;
            let result = (|| {
                let unpacked = extract.join(dir);
                let all_files = std::iter::once((spec.path, part)).chain(spec.extras.iter().map(|e| e.path).zip(extras.iter().map(PathBuf::as_path)));
                for (rel_path, src_part) in all_files {
                    if cancel.load(Ordering::SeqCst) {
                        return Err("已取消".to_owned());
                    }
                    let rel = Path::new(rel_path);
                    if !safe_relative(rel) {
                        return Err(format!("模型包含不安全的路径：{}", rel.display()));
                    }
                    let dest = unpacked.join(rel);
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).map_err(|error| format!("安装失败：{error}"))?;
                    }
                    fs::rename(src_part, &dest).map_err(|error| format!("安装失败：{error}"))?;
                }
                if let Some(missing) = spec.required.iter().find(|item| !unpacked.join(item).exists()) {
                    return Err(format!("模型目录缺少 {missing}"));
                }
                place(install_lock, cancel, &unpacked, &target, true)
            })();
            let _ = fs::remove_dir_all(&extract);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::fake_http::serve_once;

    fn tar_bz2(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            // 直接写入名字字段，允许构造含 `..` 的恶意条目
            let name = path.as_bytes();
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn sha(bytes: &[u8]) -> &'static str {
        Box::leak(hex(&Sha256::digest(bytes)).into_boxed_str())
    }

    fn spec_for(bytes: &[u8], archive: Archive, path: &'static str, required: &'static [&'static str]) -> ModelSpec {
        ModelSpec {
            id: "test",
            kind: ModelKind::Tts,
            engine: Engine::Matcha,
            label: "测试",
            note: "",
            path,
            sha256: sha(bytes),
            size: bytes.len() as u64,
            archive,
            required,
            extras: &[],
            voices: None,
        }
    }

    fn wait(manager: &ModelManager) -> Option<Progress> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let progress = manager.tasks.lock().unwrap().get("test").map(|task| task.progress.clone());
            match &progress {
                Some(progress) if ModelManager::busy(progress) => {}
                _ => return progress,
            }
            assert!(std::time::Instant::now() < deadline, "下载未结束");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn archive_is_verified_extracted_and_installed() {
        let dir = tempfile::tempdir().unwrap();
        let manager = ModelManager::new(dir.path().join("models"));
        let bytes = tar_bz2(&[("pkg/model.onnx", b"model"), ("pkg/tokens.txt", b"a 1"), ("pkg/sub/x.txt", b"x")]);
        let spec = spec_for(&bytes, Archive::TarBz2 { dir: "pkg" }, "tts-models/pkg.tar.bz2", &["model.onnx", "tokens.txt", "sub"]);
        let (url, requests) = serve_once("HTTP/1.1 200 OK", "Content-Type: application/octet-stream\r\n", bytes);
        manager.download_spec(spec, &format!("{url}/")).unwrap();
        assert_eq!(wait(&manager), None, "成功后任务移除");
        assert!(requests.recv().unwrap().head.starts_with("GET /tts-models/pkg.tar.bz2 "));
        let root = dir.path().join("models");
        assert!(installed(&root, &spec));
        assert_eq!(fs::read(root.join("pkg/sub/x.txt")).unwrap(), b"x");
        assert!(!root.join(".extract-test").exists());
        assert!(fs::read_dir(root.join(".download")).unwrap().next().is_none(), "临时文件已清理");
    }

    /// 按清单造出“已安装”的模型（大文件用 set_len 生成稀疏文件，不真的写入数据）。
    fn fake_install(root: &Path, spec: &ModelSpec) {
        let path = install_path(root, spec);
        let sized = |file: PathBuf, size: u64| {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::File::create(&file).unwrap().set_len(size).unwrap();
        };
        match spec.archive {
            Archive::File => sized(path, spec.size),
            _ => {
                if matches!(spec.archive, Archive::Directory { .. }) {
                    sized(path.join(spec.path), spec.size);
                    for extra in spec.extras {
                        sized(path.join(extra.path), extra.size);
                    }
                }
                for item in spec.required {
                    if !path.join(item).exists() {
                        sized(path.join(item), 1);
                    }
                }
            }
        }
        assert!(installed(root, spec), "{} 未造成已安装", spec.id);
    }

    /// 2026-11-07：中途退出留下的半截文件启动时清掉；删除先改名到回收目录再删，不留残留。
    #[test]
    fn leftovers_are_cleaned_and_delete_goes_through_trash() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("models");
        let laya_root = dir.path().join("external").join("models");
        let manager = ModelManager::new(root.clone()).with_root_override("laya_multilingual", laya_root.clone());
        let kokoro = spec("kokoro").unwrap();
        fake_install(&root, kokoro);
        fs::create_dir_all(root.join(".download")).unwrap();
        fs::write(root.join(".download").join("onejev_1_6_9b-3-0-x.gguf.part"), b"half").unwrap();
        fs::create_dir_all(root.join(".extract-kokoro-7").join("pkg")).unwrap();
        fs::create_dir_all(root.join(".extract-llama-macos-arm64")).unwrap();
        fs::create_dir_all(root.join(".trash-kokoro-1")).unwrap();
        fs::create_dir_all(laya_root.join(".download")).unwrap();
        assert_eq!(manager.clean_leftovers(), 5);
        let left: Vec<_> = fs::read_dir(&root).unwrap().flatten().map(|entry| entry.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from(base_name_dir(kokoro))], "只剩已安装的模型");
        assert!(!laya_root.join(".download").exists(), "外置目录也清理");

        manager.delete("kokoro").unwrap();
        assert!(!install_path(&root, kokoro).exists());
        assert!(fs::read_dir(&root).unwrap().next().is_none(), "删除后不留 .trash 残留");
        // 安装最后一步看到取消标志就放弃，不会在删除之后又装回去
        let cancelled = AtomicBool::new(true);
        let staged = dir.path().join("staged");
        fs::create_dir_all(&staged).unwrap();
        assert!(place(&Mutex::new(()), &cancelled, &staged, &install_path(&root, kokoro), true).is_err());
        assert!(!install_path(&root, kokoro).exists());
    }

    fn base_name_dir(spec: &ModelSpec) -> &'static str {
        match spec.archive {
            Archive::TarBz2 { dir } | Archive::Directory { dir, .. } => dir,
            Archive::File => base_name(spec.path),
        }
    }

    /// 2026-11-07：删掉最后一档 OneJev 时一并删除 MOSS 下载的推理引擎；用户自己放的 llama/ 不动。
    #[test]
    fn deleting_the_last_onejev_removes_the_managed_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("models");
        let manager = ModelManager::new(root.clone());
        let small = spec("onejev_1_6_0_8b").unwrap();
        let mid = spec("onejev_1_6_4b").unwrap();
        fake_install(&root, small);
        fake_install(&root, mid);
        let managed = root.join(".runtime").join("llama-b12167").join("platform");
        fs::create_dir_all(&managed).unwrap();
        fs::write(managed.join("llama-server"), b"bin").unwrap();
        fs::create_dir_all(root.join("llama")).unwrap();
        fs::write(root.join("llama").join("llama-server"), b"user").unwrap();

        manager.delete(mid.id).unwrap();
        assert!(managed.join("llama-server").is_file(), "还有 0.8B 在用，推理引擎保留");
        manager.delete(small.id).unwrap();
        assert!(!root.join(".runtime").exists(), "最后一档删掉后推理引擎一起删");
        assert!(root.join("llama").join("llama-server").is_file(), "用户自备的 llama/ 不动");
        // 删除非 OneJev 模型不影响推理引擎
        fs::create_dir_all(&managed).unwrap();
        manager.delete("kokoro").unwrap();
        assert!(managed.is_dir());
    }

    /// 按请求路径回应的本机 HTTP 服务（每个连接一个请求）。
    fn serve_paths(files: Vec<(&'static str, Vec<u8>)>) -> String {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(files.len()) {
                let mut stream = stream.unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let body = files.iter().find(|(name, _)| path == format!("/{name}")).map(|(_, body)| body.clone());
                let head = match &body {
                    Some(body) => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                };
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&body.unwrap_or_default()).unwrap();
            }
        });
        url
    }

    #[test]
    fn extra_files_are_downloaded_verified_and_placed_in_model_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("models");
        let manager = ModelManager::new(root.clone());
        let archive = tar_bz2(&[("pkg/model.onnx", b"am")]);
        let vocoder = b"vocoder-bytes".to_vec();
        let extras: &'static [ExtraFile] =
            Box::leak(vec![ExtraFile { path: "voc/vocos.onnx", sha256: sha(&vocoder), size: vocoder.len() as u64 }].into_boxed_slice());
        let spec = ModelSpec {
            extras,
            ..spec_for(&archive, Archive::TarBz2 { dir: "pkg" }, "tts/pkg.tar.bz2", &["model.onnx", "vocos.onnx"])
        };
        let url = serve_paths(vec![("tts/pkg.tar.bz2", archive.clone()), ("voc/vocos.onnx", vocoder.clone())]);
        manager.download_spec(spec, &url).unwrap();
        assert_eq!(wait(&manager), None, "{:?}", manager.tasks.lock().unwrap().get("test").map(|task| task.progress.clone()));
        assert!(installed(&root, &spec));
        assert_eq!(fs::read(root.join("pkg/vocos.onnx")).unwrap(), vocoder);
        // 附加文件校验不一致：整体不安装
        fs::remove_dir_all(root.join("pkg")).unwrap();
        let url = serve_paths(vec![("tts/pkg.tar.bz2", archive), ("voc/vocos.onnx", b"vocoder-bytez".to_vec())]);
        manager.download_spec(spec, &url).unwrap();
        assert!(matches!(wait(&manager), Some(Progress::Failed { message }) if message.contains("SHA-256")));
        assert!(!root.join("pkg").exists());
        assert!(fs::read_dir(root.join(".download")).unwrap().next().is_none(), "临时文件已清理");
    }

    #[test]
    fn checksum_mismatch_and_truncation_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("models");
        let manager = ModelManager::new(root.clone());
        let good = b"0123456789".to_vec();
        let mut spec = spec_for(&good, Archive::File, "asr-models/vad.onnx", &[]);
        let (url, _) = serve_once("HTTP/1.1 200 OK", "", b"0123456780".to_vec());
        manager.download_spec(spec, &url).unwrap();
        match wait(&manager) {
            Some(Progress::Failed { message }) => assert!(message.contains("SHA-256"), "{message}"),
            other => panic!("{other:?}"),
        }
        assert!(!root.join("vad.onnx").exists(), "校验失败不安装");
        let (url, _) = serve_once("HTTP/1.1 200 OK", "", b"01234".to_vec());
        manager.download_spec(spec, &url).unwrap();
        assert!(matches!(wait(&manager), Some(Progress::Failed { message }) if message.contains("不完整")));
        let (url, _) = serve_once("HTTP/1.1 404 Not Found", "", Vec::new());
        manager.download_spec(spec, &url).unwrap();
        assert!(matches!(wait(&manager), Some(Progress::Failed { message }) if message.contains("404")));
        // 正确的单文件模型
        let (url, _) = serve_once("HTTP/1.1 200 OK", "", good.clone());
        manager.download_spec(spec, &url).unwrap();
        assert_eq!(wait(&manager), None);
        assert_eq!(fs::read(root.join("vad.onnx")).unwrap(), good);
        spec.size += 1;
        assert!(!installed(&root, &spec), "大小不符视为未安装");
        assert!(manager.download_spec(spec, "ftp://x").unwrap_err().contains("http"));
    }

    #[test]
    fn path_traversal_entries_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("models");
        let manager = ModelManager::new(root.clone());
        let bytes = tar_bz2(&[("pkg/model.onnx", b"m"), ("pkg/../../evil.txt", b"pwned")]);
        let spec = spec_for(&bytes, Archive::TarBz2 { dir: "pkg" }, "x/pkg.tar.bz2", &["model.onnx"]);
        let (url, _) = serve_once("HTTP/1.1 200 OK", "", bytes);
        manager.download_spec(spec, &url).unwrap();
        match wait(&manager) {
            Some(Progress::Failed { message }) => assert!(message.contains("不安全"), "{message}"),
            other => panic!("{other:?}"),
        }
        assert!(!dir.path().join("evil.txt").exists() && !root.join("evil.txt").exists());
        assert!(!installed(&root, &spec));
        assert!(!root.join(".extract-test").exists());
    }

    #[test]
    fn missing_required_files_fail_and_catalog_is_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let manager = ModelManager::new(dir.path().join("models"));
        let bytes = tar_bz2(&[("pkg/model.onnx", b"m")]);
        let spec = spec_for(&bytes, Archive::TarBz2 { dir: "pkg" }, "x/pkg.tar.bz2", &["model.onnx", "tokens.txt"]);
        let (url, _) = serve_once("HTTP/1.1 200 OK", "", bytes);
        manager.download_spec(spec, &url).unwrap();
        assert!(matches!(wait(&manager), Some(Progress::Failed { message }) if message.contains("tokens.txt")));
        let digests = CATALOG.iter().map(|spec| spec.sha256).chain(CATALOG.iter().flat_map(|spec| spec.extras.iter().map(|extra| extra.sha256)));
        for digest in digests {
            assert_eq!(digest.len(), 64, "{digest}");
            assert!(digest.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        }
        let mut ids: Vec<_> = CATALOG.iter().map(|spec| spec.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len(), "模型 id 不重复");
        for spec in CATALOG {
            assert!(spec.size > 0 && !spec.note.is_empty(), "{}", spec.id);
            // 附加文件只用于压缩包模型，且安装检查包含它
            // 附加文件只用于压缩包或多文件目录模型，且安装检查包含它
            for extra in spec.extras {
                match spec.archive {
                    Archive::TarBz2 { .. } => assert!(spec.required.contains(&base_name(extra.path)), "{}", spec.id),
                    Archive::Directory { .. } => assert!(spec.required.contains(&extra.path), "{}", spec.id),
                    Archive::File => panic!("unexpected extra file for {}", spec.id),
                }
            }
            // 多音色只用于合成模型，默认音色在范围内
            if let Some(voices) = spec.voices {
                assert_eq!(spec.kind, ModelKind::Tts);
                assert!((0..=voices.max).contains(&voices.default), "{}", spec.id);
            }
            let kind = match spec.engine {
                Engine::SileroVad | Engine::TenVad => ModelKind::Vad,
                Engine::SenseVoice | Engine::StreamingTransducer => ModelKind::Asr,
                Engine::Kokoro | Engine::Matcha => ModelKind::Tts,
                Engine::Laya | Engine::OneJev => ModelKind::Jev,
                Engine::CampPlus => ModelKind::Speaker,
            };
            assert_eq!(spec.kind, kind, "{} 的引擎与类别一致", spec.id);
        }
        assert!(spec_of("ten_vad", ModelKind::Vad).is_some());
        assert!(spec_of("ten_vad", ModelKind::Asr).is_none(), "类别不符");
        let matcha = super::spec("matcha_zh_en").unwrap();
        assert_eq!(matcha.download_size(), 79_033_838 + 53_882_848);
        let status = manager.status();
        assert_eq!(status.len(), CATALOG.len());
        assert_eq!(status.iter().find(|model| model.id == "matcha_zh_en").unwrap().size, matcha.download_size());
        assert!(status.iter().find(|model| model.id == "x_asr_streaming").unwrap().streaming);
        assert!(status.iter().all(|model| !model.installed && model.progress.is_none()));
        assert!(manager.download("nope", DEFAULT_BASE).is_err());
        assert!(manager.delete("kokoro").is_ok(), "未安装时删除也成功");
    }

    /// 访问真实发布地址（不在常规测试中运行）：`cargo test -p moss-voice real_download -- --ignored`
    #[test]
    #[ignore]
    fn real_download_silero() {
        let dir = tempfile::tempdir().unwrap();
        let manager = ModelManager::new(dir.path().join("models"));
        manager.download("silero_vad", DEFAULT_BASE).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while manager.status()[0].progress.as_ref().is_some_and(ModelManager::busy) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(200));
        }
        let status = &manager.status()[0];
        assert!(status.installed, "{status:?}");
        assert!(manager.path_if_installed("silero_vad").is_some());
    }

    #[test]
    fn traversal_check_rules() {
        assert!(safe_relative(Path::new("a/b.txt")));
        assert!(safe_relative(Path::new("./a")));
        assert!(!safe_relative(Path::new("../a")));
        assert!(!safe_relative(Path::new("a/../../b")));
        assert!(!safe_relative(Path::new("/etc/passwd")));
        assert!(!safe_relative(Path::new("")));
    }

    #[test]
    fn directory_model_ignores_sherpa_github_mirror_and_supports_hf_mirrors() {
        let laya = super::spec("laya_multilingual").unwrap();
        assert_eq!(
            resolve_download_bases(laya, DEFAULT_BASE),
            vec![DEFAULT_LAYA_BASE.to_owned(), DEFAULT_LAYA_MIRROR_BASE.to_owned()]
        );
        assert_eq!(
            resolve_download_bases(
                laya,
                "https://ghfast.top/https://github.com/k2-fsa/sherpa-onnx/releases/download"
            ),
            vec![DEFAULT_LAYA_MIRROR_BASE.to_owned(), DEFAULT_LAYA_BASE.to_owned()]
        );
        assert_eq!(
            resolve_download_bases(laya, "https://hf-mirror.com"),
            vec![DEFAULT_LAYA_MIRROR_BASE.to_owned(), DEFAULT_LAYA_BASE.to_owned()]
        );
        assert_eq!(
            resolve_download_bases(laya, "http://127.0.0.1:19999/laya"),
            vec!["http://127.0.0.1:19999/laya".to_owned()]
        );

        let onejev = super::spec("onejev_1_6_0_8b").unwrap();
        let expected_mirror = "https://hf-mirror.com/mradermacher/OneJev1.6-0.8B-GGUF/resolve/main";
        assert_eq!(
            resolve_download_bases(onejev, DEFAULT_BASE),
            vec![DEFAULT_ONEJEV_0_8B_BASE.to_owned(), expected_mirror.to_owned()]
        );
        assert_eq!(
            resolve_download_bases(onejev, "https://hf-mirror.com"),
            vec![expected_mirror.to_owned(), DEFAULT_ONEJEV_0_8B_BASE.to_owned()]
        );
    }
}
