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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archive {
    /// 单个文件，原样放到 `models/<file>`
    File,
    /// tar.bz2，解出顶层目录 `models/<dir>`
    TarBz2 { dir: &'static str },
}

/// 模型类别：判停、识别、合成各选一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Vad,
    Asr,
    Tts,
    Speaker,
}

/// 辅助程序加载模型的方式（与 `moss-voice-local-proto` 的引擎一一对应）。
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

/// 内置清单。SHA-256 取自 GitHub 发布页对各资产给出的摘要（2026-10-07 / 10-08 核对，并与实际下载的文件逐一核对一致）。
/// 前三项是 NV6 的默认模型，其余为可选模型。
pub const CATALOG: [ModelSpec; 9] = [
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
        Archive::TarBz2 { dir } => root.join(dir),
    }
}

/// 模型是否已完整安装。
pub fn installed(root: &Path, spec: &ModelSpec) -> bool {
    let path = install_path(root, spec);
    match spec.archive {
        Archive::File => path.metadata().map(|meta| meta.is_file() && meta.len() == spec.size).unwrap_or(false),
        Archive::TarBz2 { .. } => path.is_dir() && spec.required.iter().all(|item| path.join(item).exists()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Progress {
    Downloading { done: u64, total: u64 },
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
    tasks: Arc<Mutex<HashMap<&'static str, Task>>>,
    generation: std::sync::atomic::AtomicU64,
}

impl ModelManager {
    /// `root` 为运行目录下的 `models/`。
    pub fn new(root: PathBuf) -> Self {
        Self { root, tasks: Arc::default(), generation: std::sync::atomic::AtomicU64::new(1) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_if_installed(&self, id: &str) -> Option<PathBuf> {
        let spec = spec(id)?;
        installed(&self.root, spec).then(|| install_path(&self.root, spec))
    }

    pub fn status(&self) -> Vec<ModelStatus> {
        let tasks = self.tasks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        CATALOG
            .iter()
            .map(|spec| ModelStatus {
                id: spec.id,
                kind: spec.kind,
                engine: spec.engine,
                label: spec.label,
                note: spec.note,
                size: spec.download_size(),
                streaming: spec.engine == Engine::StreamingTransducer,
                voices: spec.voices,
                installed: installed(&self.root, spec),
                progress: tasks.get(spec.id).map(|task| task.progress.clone()),
            })
            .collect()
    }

    fn busy(progress: &Progress) -> bool {
        matches!(progress, Progress::Downloading { .. } | Progress::Verifying | Progress::Installing)
    }

    /// 开始下载（后台线程）。`base` 为发布地址或镜像。
    pub fn download(&self, id: &str, base: &str) -> Result<(), String> {
        let spec = *spec(id).ok_or_else(|| format!("未知模型：{id}"))?;
        self.download_spec(spec, base)
    }

    /// 与 [`download`](Self::download) 相同，但使用给定的清单项（测试用）。
    pub fn download_spec(&self, spec: ModelSpec, base: &str) -> Result<(), String> {
        let base = base.trim().trim_end_matches('/');
        if !(base.starts_with("https://") || base.starts_with("http://")) || base.contains(char::is_whitespace) {
            return Err("下载地址必须以 http:// 或 https:// 开头".into());
        }
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
        let root = self.root.clone();
        let tasks = self.tasks.clone();
        let base = base.to_owned();
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
                let result = run_download(&root, &spec, &base, &cancel, &report);
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
    pub fn delete(&self, id: &str) -> Result<(), String> {
        let spec = spec(id).ok_or_else(|| format!("未知模型：{id}"))?;
        self.cancel(id);
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.remove(spec.id);
        }
        let path = install_path(&self.root, spec);
        let removed = match spec.archive {
            Archive::File => fs::remove_file(&path),
            Archive::TarBz2 { .. } => fs::remove_dir_all(&path),
        };
        match removed {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("删除失败：{error}")),
        }
    }
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

fn run_download(root: &Path, spec: &ModelSpec, base: &str, cancel: &AtomicBool, report: &dyn Fn(Progress)) -> Result<(), String> {
    let staging = root.join(".download");
    fs::create_dir_all(&staging).map_err(|error| format!("创建下载目录失败：{error}"))?;
    let items: Vec<Item> = std::iter::once(Item { path: spec.path, sha256: spec.sha256, size: spec.size })
        .chain(spec.extras.iter().map(|extra| Item { path: extra.path, sha256: extra.sha256, size: extra.size }))
        .collect();
    let parts: Vec<PathBuf> = items.iter().map(|item| staging.join(format!("{}.part", base_name(item.path)))).collect();
    let total = spec.download_size();
    let result = (|| {
        let mut offset = 0;
        for (item, part) in items.iter().zip(&parts) {
            let progress = |done: u64| report(Progress::Downloading { done: offset + done, total });
            fetch(item, &format!("{base}/{}", item.path), part, cancel, &progress)?;
            offset += item.size;
        }
        report(Progress::Verifying);
        report(Progress::Installing);
        install(root, spec, &parts[0], &parts[1..], cancel)
    })();
    for part in &parts {
        let _ = fs::remove_file(part);
    }
    result
}

fn fetch(item: &Item, url: &str, part: &Path, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(None::<Duration>)
        // GitHub 发布资产会跳转到存储地址；完整性由 SHA-256 保证
        .redirect(reqwest::redirect::Policy::limited(5))
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
        if done > item.size {
            return Err("下载的文件比预期大，已停止".into());
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read]).map_err(|error| format!("写入失败（磁盘空间不足？）：{error}"))?;
        if done - last_report >= 1024 * 1024 || done == item.size {
            last_report = done;
            progress(done);
        }
    }
    file.flush().map_err(|error| format!("写入失败：{error}"))?;
    drop(file);
    if done != item.size {
        return Err(format!("下载不完整（{done} / {} 字节），请重试", item.size));
    }
    let digest = hex(&hasher.finalize());
    if digest != item.sha256 {
        return Err("SHA-256 校验不一致，文件已删除（可能被篡改或下载出错）".into());
    }
    Ok(())
}

/// 压缩包内的路径是否安全（相对路径、无 `..`）。
fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn install(root: &Path, spec: &ModelSpec, part: &Path, extras: &[PathBuf], cancel: &AtomicBool) -> Result<(), String> {
    let target = install_path(root, spec);
    match spec.archive {
        Archive::File => {
            let _ = fs::remove_file(&target);
            fs::rename(part, &target).map_err(|error| format!("安装失败：{error}"))
        }
        Archive::TarBz2 { dir } => {
            let extract = root.join(format!(".extract-{}", spec.id));
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
                let _ = fs::remove_dir_all(&target);
                fs::rename(&unpacked, &target).map_err(|error| format!("安装失败：{error}"))
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
            for extra in spec.extras {
                assert!(matches!(spec.archive, Archive::TarBz2 { .. }), "{}", spec.id);
                assert!(spec.required.contains(&base_name(extra.path)), "{}", spec.id);
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
}
