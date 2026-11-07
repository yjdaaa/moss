//! Laya 决策模型的 Python 运行环境自动准备（2026-11-07 用户决定：点下载 Laya 时在软件目录自动创建
//! Python 虚拟环境并安装 `laya[serve]`，有 NVIDIA 显卡时装 CUDA 版 PyTorch；本机没有 Python 或 CUDA
//! 条件时说明原因）。
//!
//! - 不下载 Python 解释器本身：用本机已安装的 Python 3.10+ 执行 `python -m venv <laya>/.venv`；
//! - 显卡：Windows / Linux 运行 `nvidia-smi` 读驱动支持的 CUDA 版本，选对应的 PyTorch CUDA 轮子
//!   （CUDA 运行库随轮子安装，用户不必另装 CUDA Toolkit，只需 NVIDIA 驱动）；没有驱动或驱动过旧按 CPU 安装并说明；
//!   macOS 直接装 PyPI 版 PyTorch（Apple Silicon 自带 Metal / MPS 加速）；
//! - `pip --no-cache-dir`：不在用户目录留下数 GB 的 pip 缓存，删除 Laya 时连同虚拟环境一起删干净；
//! - MOSS 创建的虚拟环境写入 [`MANAGED_MARKER`]，只有带这个标记的环境才会被 MOSS 删除；
//! - 全程可取消（结束 pip 子进程），测试通过 [`SetupHost`] 注入假命令，不碰真实网络与子进程。

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;

/// 最低 Python 版本。
pub const MIN_PYTHON: (u32, u32) = (3, 10);
/// 优先选用的 Python 版本（PyTorch 与 laya 轮子最齐全）。
const PREFERRED_PYTHON: [(u32, u32); 4] = [(3, 12), (3, 11), (3, 13), (3, 10)];
/// MOSS 创建的虚拟环境标记文件（位于 `.venv/` 内）。
pub const MANAGED_MARKER: &str = ".moss-managed";

const PYPI_OFFICIAL: &str = "https://pypi.org/simple";
const PYPI_TUNA: &str = "https://pypi.tuna.tsinghua.edu.cn/simple";
const TORCH_INDEX: &str = "https://download.pytorch.org/whl";

/// 检查 Python 版本、`venv` 与 `ensurepip` 是否可用（Debian 未装 python3-venv 时缺 ensurepip，建不了虚拟环境）。
const PYTHON_PROBE: &str =
    "import sys, venv, ensurepip; print('%d.%d' % sys.version_info[:2]); print(sys.executable)";
/// 安装后验证：PyTorch 可导入、laya.serve 与 uvicorn 存在，输出可用加速。
const VERIFY_PROBE: &str = "import sys, importlib.util as u, torch\n\
if u.find_spec('laya.serve') is None or u.find_spec('uvicorn') is None: sys.exit('laya[serve] 未安装完整')\n\
m = getattr(torch.backends, 'mps', None)\n\
sys.stdout.write('cuda' if torch.cuda.is_available() else ('mps' if m is not None and m.is_available() else 'cpu'))";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// 虚拟环境中的 Python 路径（与桌面端 `laya_venv_python` 一致）。
pub fn venv_python(venv: &Path, platform: Platform) -> PathBuf {
    match platform {
        Platform::Windows => venv.join("Scripts").join("python.exe"),
        _ => venv.join("bin").join("python"),
    }
}

/// `venv` 目录是否由 MOSS 创建。
pub fn is_managed(venv: &Path) -> bool {
    venv.join(MANAGED_MARKER).is_file()
}

/// 解析 [`PYTHON_PROBE`] 的输出：第一行版本，第二行解释器绝对路径。
pub fn parse_python_probe(output: &str) -> Option<((u32, u32), PathBuf)> {
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let version = lines.next()?;
    let (major, minor) = version.split_once('.')?;
    let version = (major.parse().ok()?, minor.parse().ok()?);
    let exe = PathBuf::from(lines.next()?);
    exe.is_absolute().then_some((version, exe))
}

/// 从 `nvidia-smi` 输出读驱动支持的最高 CUDA 版本（“CUDA Version: 12.4”）。
pub fn parse_driver_cuda(output: &str) -> Option<(u32, u32)> {
    let rest = &output[output.find("CUDA Version")?..];
    let rest = rest
        .trim_start_matches("CUDA Version")
        .trim_start_matches([':', ' ']);
    let token: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    let (major, minor) = token.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// PyTorch 安装方案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorchPlan {
    /// 从 PyTorch 官方 CUDA 索引安装（`tag` 如 `cu124`）
    Cuda {
        tag: &'static str,
        version: (u32, u32),
    },
    /// 从 PyTorch 官方 CPU 索引安装（Linux 无显卡时避免下载数 GB 的 CUDA 版）
    CpuIndex,
    /// 交给 `laya[serve]` 从 PyPI 拉取（macOS 带 MPS；Windows PyPI 版即 CPU 版）
    Pypi,
}

/// 按平台与驱动 CUDA 版本选择 PyTorch，返回方案与给用户看的说明。
pub fn plan_torch(platform: Platform, driver_cuda: Option<(u32, u32)>) -> (TorchPlan, String) {
    if platform == Platform::MacOs {
        return (
            TorchPlan::Pypi,
            "macOS 使用 Apple Metal（MPS）或 CPU 运行，不需要 CUDA".into(),
        );
    }
    let cpu = if platform == Platform::Linux {
        TorchPlan::CpuIndex
    } else {
        TorchPlan::Pypi
    };
    let Some(version) = driver_cuda else {
        return (
            cpu,
            "未检测到 NVIDIA 显卡驱动（nvidia-smi），已按 CPU 模式安装；装好 NVIDIA 驱动后点“重新准备环境”即可换成 CUDA 显卡加速".into(),
        );
    };
    let tag = match version {
        v if v >= (12, 8) => "cu128",
        v if v >= (12, 6) => "cu126",
        v if v >= (12, 4) => "cu124",
        v if v >= (12, 1) => "cu121",
        v if v >= (11, 8) => "cu118",
        _ => {
            return (
                cpu,
                format!(
                    "NVIDIA 驱动过旧（只支持 CUDA {}.{}，PyTorch 需要 11.8 以上），已按 CPU 模式安装；升级显卡驱动后点“重新准备环境”即可换成 CUDA 加速",
                    version.0, version.1
                ),
            );
        }
    };
    (
        TorchPlan::Cuda { tag, version },
        format!(
            "检测到 NVIDIA 显卡（驱动支持 CUDA {}.{}），安装 {tag} 版 PyTorch（约 2.5 GB，CUDA 运行库随包安装，无需另装 CUDA Toolkit）",
            version.0, version.1
        ),
    )
}

/// 一次 pip 命令可依次尝试的参数组（主索引失败后换备用索引）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipStep {
    pub label: String,
    pub attempts: Vec<Vec<String>>,
    /// 失败是否中止整个准备（升级 pip 失败不中止）
    pub fatal: bool,
}

fn pip_args(extra: &[&str]) -> Vec<String> {
    [
        "-m",
        "pip",
        "install",
        "--no-cache-dir",
        "--disable-pip-version-check",
        "--progress-bar",
        "off",
        "--retries",
        "5",
        "--timeout",
        "60",
    ]
    .iter()
    .chain(extra)
    .map(|arg| (*arg).to_owned())
    .collect()
}

/// 依次执行的 pip 步骤。`prefer_mirror` 为真（用户在模型下载地址填了国内镜像）时先用清华 PyPI 镜像。
pub fn pip_steps(plan: &TorchPlan, prefer_mirror: bool) -> Vec<PipStep> {
    let indexes = if prefer_mirror {
        [PYPI_TUNA, PYPI_OFFICIAL]
    } else {
        [PYPI_OFFICIAL, PYPI_TUNA]
    };
    let mut steps = vec![PipStep {
        label: "升级 pip".into(),
        attempts: indexes
            .iter()
            .map(|index| pip_args(&["--upgrade", "pip", "-i", index]))
            .collect(),
        fatal: false,
    }];
    let torch = match plan {
        TorchPlan::Cuda { tag, version } => Some((
            format!(
                "安装 PyTorch（CUDA {}.{} · {tag}，约 2.5 GB）",
                version.0, version.1
            ),
            format!("{TORCH_INDEX}/{tag}"),
        )),
        TorchPlan::CpuIndex => Some((
            "安装 PyTorch（CPU 版）".to_owned(),
            format!("{TORCH_INDEX}/cpu"),
        )),
        TorchPlan::Pypi => None,
    };
    if let Some((label, torch_index)) = torch {
        steps.push(PipStep {
            label,
            // PyTorch 只从官方索引装（国内镜像不同步 CUDA 轮子）；依赖包走 PyPI
            attempts: indexes
                .iter()
                .map(|index| {
                    pip_args(&[
                        "torch",
                        "--index-url",
                        &torch_index,
                        "--extra-index-url",
                        index,
                    ])
                })
                .collect(),
            fatal: true,
        });
    }
    steps.push(PipStep {
        label: "安装 laya[serve]".into(),
        attempts: indexes
            .iter()
            .map(|index| pip_args(&["laya[serve]", "-i", index]))
            .collect(),
        fatal: true,
    });
    steps
}

/// 找到的 Python。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonFound {
    pub version: (u32, u32),
    pub executable: PathBuf,
}

/// 一个待探测的 Python 启动方式（如 `py -3.12`、`/opt/homebrew/bin/python3`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonCandidate {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// 没有可用 Python 时给用户的说明。
pub fn python_missing_message(platform: Platform) -> String {
    let how = match platform {
        Platform::Windows => {
            "请从 python.org 下载安装 Python 3.12（安装时勾选“Add python.exe to PATH”；微软商店版不支持）"
        }
        Platform::MacOs => {
            "请从 python.org 下载安装 Python 3.12，或在终端执行 brew install python@3.12"
        }
        Platform::Linux => "请用系统包管理器安装 python3（3.10 以上）与 python3-venv",
    };
    format!(
        "未检测到可用的 Python 3.10 或更高版本，MOSS 无法自动创建 Laya 运行环境。{how}，装好后回到这里点“准备运行环境”。也可以直接选用免 Python 的 OneJev 1.6。"
    )
}

/// 命令输出逐行回调。
pub type LineSink<'a> = &'a mut dyn FnMut(&str);

/// 准备过程依赖的外部环境（真实实现 [`SystemHost`]；测试注入假实现）。
pub trait SetupHost: Send + Sync {
    fn platform(&self) -> Platform;
    fn python_candidates(&self) -> Vec<PythonCandidate>;
    fn nvidia_smi(&self) -> Option<PathBuf>;
    /// 运行命令直到退出，返回 stdout；非零退出、超时或取消返回 `Err`（取消时为 `"已取消"`）。
    fn run(
        &self,
        program: &Path,
        args: &[String],
        cancel: &AtomicBool,
        timeout: Option<Duration>,
        line: LineSink<'_>,
    ) -> Result<String, String>;
}

/// 准备状态（`voice_builtin_status().local.laya_setup`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SetupStatus {
    Idle,
    Running {
        stage: String,
        step: u32,
        steps: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        elapsed_secs: u64,
    },
    Done {
        summary: String,
        accelerator: String,
    },
    Failed {
        message: String,
        log: Vec<String>,
    },
    Cancelled,
}

/// 一次准备的参数。
#[derive(Debug, Clone)]
pub struct SetupRequest {
    /// Laya 根目录；虚拟环境建在 `<root>/.venv`
    pub root: PathBuf,
    pub prefer_mirror: bool,
}

struct Inner {
    status: SetupStatus,
    started: Option<Instant>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

/// 后台准备任务（同一时间只有一个）。
pub struct LayaSetup {
    host: Arc<dyn SetupHost>,
    inner: Arc<Mutex<Inner>>,
}

impl LayaSetup {
    pub fn new(host: Arc<dyn SetupHost>) -> Self {
        Self {
            host,
            inner: Arc::new(Mutex::new(Inner {
                status: SetupStatus::Idle,
                started: None,
                cancel: Arc::default(),
                worker: None,
            })),
        }
    }

    pub fn status(&self) -> SetupStatus {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &inner.status {
            SetupStatus::Running {
                stage,
                step,
                steps,
                detail,
                ..
            } => SetupStatus::Running {
                stage: stage.clone(),
                step: *step,
                steps: *steps,
                detail: detail.clone(),
                elapsed_secs: inner
                    .started
                    .map(|at| at.elapsed().as_secs())
                    .unwrap_or_default(),
            },
            other => other.clone(),
        }
    }

    pub fn running(&self) -> bool {
        matches!(self.status(), SetupStatus::Running { .. })
    }

    /// 同步查找本机 Python（点下载前先确认，没有就直接说明原因）。
    pub fn find_python(&self) -> Result<PythonFound, String> {
        find_python(self.host.as_ref(), &AtomicBool::new(false))
    }

    /// 开始准备；已在进行中时直接返回成功。
    pub fn start(&self, request: SetupRequest) -> Result<(), String> {
        let mut inner = self.inner.lock().map_err(|_| "内部错误".to_owned())?;
        if matches!(inner.status, SetupStatus::Running { .. }) {
            return Ok(());
        }
        if let Some(old) = inner.worker.take() {
            // 上一次已结束（状态不是 Running），回收线程
            let _ = old.join();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        inner.cancel = cancel.clone();
        inner.started = Some(Instant::now());
        inner.status = SetupStatus::Running {
            stage: "准备开始".into(),
            step: 0,
            steps: STEPS,
            detail: None,
            elapsed_secs: 0,
        };
        let host = self.host.clone();
        let shared = self.inner.clone();
        let worker = std::thread::Builder::new()
            .name("laya-env-setup".into())
            .spawn(move || {
                let progress = |stage: &str, step: u32, detail: Option<String>| {
                    if let Ok(mut inner) = shared.lock()
                        && matches!(inner.status, SetupStatus::Running { .. })
                    {
                        inner.status = SetupStatus::Running {
                            stage: stage.to_owned(),
                            step,
                            steps: STEPS,
                            detail,
                            elapsed_secs: 0,
                        };
                    }
                };
                let mut log = VecDeque::new();
                let result = run_setup(host.as_ref(), &request, &cancel, &progress, &mut log);
                if let Ok(mut inner) = shared.lock() {
                    inner.status = match result {
                        _ if cancel.load(Ordering::SeqCst) => SetupStatus::Cancelled,
                        Ok((summary, accelerator)) => SetupStatus::Done {
                            summary,
                            accelerator,
                        },
                        Err(message) => SetupStatus::Failed {
                            message,
                            log: log.into_iter().collect(),
                        },
                    };
                }
            })
            .map_err(|error| format!("创建准备线程失败：{error}"))?;
        inner.worker = Some(worker);
        Ok(())
    }

    /// 取消（结束 pip 子进程）；状态立即变为已取消。
    pub fn cancel(&self) {
        if let Ok(mut inner) = self.inner.lock()
            && matches!(inner.status, SetupStatus::Running { .. })
        {
            inner.cancel.store(true, Ordering::SeqCst);
            inner.status = SetupStatus::Cancelled;
        }
    }

    /// 取消并等后台线程结束（删除虚拟环境、退出程序前调用）。
    pub fn cancel_and_wait(&self) {
        self.cancel();
        let worker = self
            .inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.worker.take());
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

const STEPS: u32 = 6;
const LOG_LINES: usize = 12;

fn remember(log: &mut VecDeque<String>, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if log.len() == LOG_LINES {
        log.pop_front();
    }
    log.push_back(line.chars().take(240).collect());
}

fn find_python(host: &dyn SetupHost, cancel: &AtomicBool) -> Result<PythonFound, String> {
    let probe = ["-c".to_owned(), PYTHON_PROBE.to_owned()];
    let mut found: Vec<PythonFound> = Vec::new();
    for candidate in host.python_candidates() {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        let args: Vec<String> = candidate
            .args
            .iter()
            .cloned()
            .chain(probe.iter().cloned())
            .collect();
        let Ok(output) = host.run(
            &candidate.program,
            &args,
            cancel,
            Some(Duration::from_secs(15)),
            &mut |_| {},
        ) else {
            continue;
        };
        if let Some((version, executable)) = parse_python_probe(&output)
            && version >= MIN_PYTHON
            && !found.iter().any(|item| item.executable == executable)
        {
            found.push(PythonFound {
                version,
                executable,
            });
        }
    }
    let rank = |item: &PythonFound| {
        PREFERRED_PYTHON
            .iter()
            .position(|v| *v == item.version)
            .unwrap_or(PREFERRED_PYTHON.len())
    };
    found
        .into_iter()
        .min_by_key(rank)
        .ok_or_else(|| python_missing_message(host.platform()))
}

fn run_setup(
    host: &dyn SetupHost,
    request: &SetupRequest,
    cancel: &AtomicBool,
    progress: &dyn Fn(&str, u32, Option<String>),
    log: &mut VecDeque<String>,
) -> Result<(String, String), String> {
    let platform = host.platform();
    progress("查找本机 Python 3.10+", 1, None);
    let python = find_python(host, cancel)?;

    progress("检测 NVIDIA 显卡驱动", 2, None);
    let driver_cuda = if platform == Platform::MacOs {
        None
    } else {
        host.nvidia_smi().and_then(|smi| {
            host.run(
                &smi,
                &[],
                cancel,
                Some(Duration::from_secs(15)),
                &mut |_| {},
            )
            .ok()
            .and_then(|out| parse_driver_cuda(&out))
        })
    };
    let (plan, plan_note) = plan_torch(platform, driver_cuda);
    if cancel.load(Ordering::SeqCst) {
        return Err("已取消".into());
    }

    progress(
        "创建 Python 虚拟环境",
        3,
        Some(request.root.join(".venv").display().to_string()),
    );
    let venv = request.root.join(".venv");
    let venv_py = venv_python(&venv, platform);
    let probe_ok = |log: &mut VecDeque<String>| {
        venv_py.is_file()
            && host
                .run(
                    &venv_py,
                    &["-c".into(), "import sys".into()],
                    cancel,
                    Some(Duration::from_secs(15)),
                    &mut |line| remember(log, line),
                )
                .is_ok()
    };
    if !probe_ok(log) {
        if venv.exists() {
            if !is_managed(&venv) {
                return Err(format!(
                    "{} 已存在但无法使用，且不是 MOSS 创建的，为避免误删请手动处理后重试",
                    venv.display()
                ));
            }
            fs::remove_dir_all(&venv)
                .map_err(|error| format!("清理损坏的虚拟环境失败：{error}"))?;
        }
        fs::create_dir_all(&request.root).map_err(|error| format!("创建目录失败：{error}"))?;
        let args = vec![
            "-m".to_owned(),
            "venv".to_owned(),
            venv.display().to_string(),
        ];
        host.run(
            &python.executable,
            &args,
            cancel,
            Some(Duration::from_secs(300)),
            &mut |line| remember(log, line),
        )
        .map_err(|error| format!("创建虚拟环境失败：{error}"))?;
        fs::write(
            venv.join(MANAGED_MARKER),
            b"created by MOSS for Laya; removed together with the Laya model\n",
        )
        .map_err(|error| format!("写入虚拟环境标记失败：{error}"))?;
        if !venv_py.is_file() {
            return Err(format!("虚拟环境已创建，但找不到 {}", venv_py.display()));
        }
    }

    let steps = pip_steps(&plan, request.prefer_mirror);
    let last = steps.len() - 1;
    for (offset, step) in steps.into_iter().enumerate() {
        // 第 4 步：升级 pip / 安装 PyTorch；第 5 步：安装 laya[serve]
        let number = if offset == last { 5 } else { 4 };
        let mut last_err = None;
        for args in &step.attempts {
            if cancel.load(Ordering::SeqCst) {
                return Err("已取消".into());
            }
            progress(&step.label, number, None);
            let mut sink = |line: &str| {
                remember(log, line);
                let line = line.trim();
                if !line.is_empty() {
                    progress(&step.label, number, Some(line.chars().take(160).collect()));
                }
            };
            match host.run(&venv_py, args, cancel, None, &mut sink) {
                Ok(_) => {
                    last_err = None;
                    break;
                }
                Err(error) => last_err = Some(error),
            }
        }
        if let Some(error) = last_err {
            if cancel.load(Ordering::SeqCst) {
                return Err("已取消".into());
            }
            if step.fatal {
                return Err(format!(
                    "{}失败：{error}（可检查网络，或在“模型下载地址”填国内镜像后重试）",
                    step.label
                ));
            }
        }
    }

    progress("验证运行环境", STEPS, None);
    let accelerator = host
        .run(
            &venv_py,
            &["-c".into(), VERIFY_PROBE.into()],
            cancel,
            Some(Duration::from_secs(180)),
            &mut |line| remember(log, line),
        )
        .map_err(|error| format!("安装完成但验证失败：{error}"))?
        .trim()
        .to_owned();
    let accel_label = match accelerator.as_str() {
        "cuda" => "NVIDIA CUDA 显卡加速",
        "mps" => "Apple Metal MPS 加速",
        _ => "CPU 模式",
    };
    let mut summary = format!(
        "已在 {} 准备好 Laya 运行环境（Python {}.{} · {accel_label}）。{plan_note}。",
        venv.display(),
        python.version.0,
        python.version.1
    );
    if matches!(plan, TorchPlan::Cuda { .. }) && accelerator != "cuda" {
        summary.push_str(
            "已安装 CUDA 版 PyTorch，但当前检测不到可用显卡（驱动异常或被占用），暂按 CPU 运行。",
        );
    }
    Ok((summary, accelerator))
}

/// 真实环境：查找本机 Python 与 nvidia-smi，并以无窗口子进程运行命令。
pub struct SystemHost;

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// Windows 下按版本号倒序列出 `<dir>/Python3XX/python.exe`。
fn windows_python_dirs(base: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(base) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("python3")
        })
        .map(|entry| entry.path().join("python.exe"))
        .filter(|exe| exe.is_file())
        .collect();
    found.sort();
    found.reverse();
    found
}

impl SetupHost for SystemHost {
    fn platform(&self) -> Platform {
        Platform::current()
    }

    fn python_candidates(&self) -> Vec<PythonCandidate> {
        let mut out: Vec<PythonCandidate> = Vec::new();
        let mut push = |program: PathBuf, args: &[&str]| {
            let candidate = PythonCandidate {
                program,
                args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            };
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        };
        match Platform::current() {
            Platform::Windows => {
                let mut launchers: Vec<PathBuf> = path_dirs()
                    .into_iter()
                    .map(|dir| dir.join("py.exe"))
                    .collect();
                if let Some(root) = env_dir("SystemRoot") {
                    launchers.push(root.join("py.exe"));
                }
                if let Some(local) = env_dir("LOCALAPPDATA") {
                    launchers.push(
                        local
                            .join("Programs")
                            .join("Python")
                            .join("Launcher")
                            .join("py.exe"),
                    );
                }
                if let Some(launcher) = launchers.into_iter().find(|path| path.is_file()) {
                    for version in ["-3.12", "-3.11", "-3.13", "-3.10", "-3"] {
                        push(launcher.clone(), &[version]);
                    }
                }
                let mut dirs = Vec::new();
                if let Some(local) = env_dir("LOCALAPPDATA") {
                    dirs.extend(windows_python_dirs(&local.join("Programs").join("Python")));
                }
                for var in ["ProgramFiles", "ProgramFiles(x86)"] {
                    if let Some(base) = env_dir(var) {
                        dirs.extend(windows_python_dirs(&base));
                    }
                }
                for exe in dirs {
                    push(exe, &[]);
                }
                // WindowsApps 下的 python.exe 是商店占位符，运行会弹出微软商店，跳过
                for dir in path_dirs().into_iter().filter(|dir| {
                    !dir.to_string_lossy()
                        .to_ascii_lowercase()
                        .contains("windowsapps")
                }) {
                    for name in ["python.exe", "python3.exe"] {
                        let exe = dir.join(name);
                        if exe.is_file() {
                            push(exe, &[]);
                        }
                    }
                }
            }
            platform => {
                let mut dirs = path_dirs();
                // 从访达启动的程序 PATH 很短，补上常见安装位置
                let extra: &[&str] = if platform == Platform::MacOs {
                    &[
                        "/opt/homebrew/bin",
                        "/usr/local/bin",
                        "/Library/Frameworks/Python.framework/Versions/Current/bin",
                    ]
                } else {
                    &["/usr/local/bin", "/usr/bin"]
                };
                dirs.extend(extra.iter().map(PathBuf::from));
                for name in [
                    "python3.12",
                    "python3.11",
                    "python3.13",
                    "python3.10",
                    "python3",
                ] {
                    for dir in &dirs {
                        let exe = dir.join(name);
                        // macOS 的 /usr/bin/python3 在没装命令行工具时会弹安装对话框，只在命令行工具已装时使用
                        if platform == Platform::MacOs
                            && exe == Path::new("/usr/bin/python3")
                            && !Path::new("/Library/Developer/CommandLineTools/usr/bin/python3")
                                .is_file()
                        {
                            continue;
                        }
                        if exe.is_file() {
                            push(exe, &[]);
                        }
                    }
                }
            }
        }
        out
    }

    fn nvidia_smi(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) {
            "nvidia-smi.exe"
        } else {
            "nvidia-smi"
        };
        let mut dirs = path_dirs();
        if let Some(root) = env_dir("SystemRoot") {
            dirs.push(root.join("System32"));
        }
        if let Some(base) = env_dir("ProgramFiles") {
            dirs.push(base.join("NVIDIA Corporation").join("NVSMI"));
        }
        dirs.push(PathBuf::from("/usr/bin"));
        dirs.into_iter()
            .map(|dir| dir.join(name))
            .find(|path| path.is_file())
    }

    fn run(
        &self,
        program: &Path,
        args: &[String],
        cancel: &AtomicBool,
        timeout: Option<Duration>,
        line: LineSink<'_>,
    ) -> Result<String, String> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
            .env("PIP_NO_INPUT", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .env_remove("PYTHONHOME")
            .env_remove("PYTHONPATH")
            .env_remove("PIP_REQUIRE_VIRTUALENV");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("无法运行 {}：{error}", program.display()))?;
        let (tx, rx) = mpsc::channel::<(bool, String)>();
        let mut readers = Vec::new();
        for (is_stdout, stream) in [
            (
                true,
                child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
            ),
            (
                false,
                child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
            ),
        ] {
            let Some(stream) = stream else { continue };
            let tx = tx.clone();
            readers.push(std::thread::spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut buf = Vec::new();
                while reader
                    .read_until(b'\n', &mut buf)
                    .map(|n| n > 0)
                    .unwrap_or(false)
                {
                    let _ = tx.send((
                        is_stdout,
                        String::from_utf8_lossy(&buf).trim_end().to_owned(),
                    ));
                    buf.clear();
                }
            }));
        }
        drop(tx);
        let deadline = timeout.map(|limit| Instant::now() + limit);
        let mut stdout = String::new();
        let mut tail: VecDeque<String> = VecDeque::new();
        let mut handle = |(is_stdout, text): (bool, String),
                          stdout: &mut String,
                          tail: &mut VecDeque<String>| {
            if is_stdout {
                stdout.push_str(&text);
                stdout.push('\n');
            }
            remember(tail, &text);
            line(&text);
        };
        let status = loop {
            while let Ok(item) = rx.try_recv() {
                handle(item, &mut stdout, &mut tail);
            }
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("已取消".into());
            }
            if deadline.is_some_and(|at| Instant::now() >= at) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("命令超时".into());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if let Ok(item) = rx.recv_timeout(Duration::from_millis(200)) {
                        handle(item, &mut stdout, &mut tail);
                    }
                }
                Err(error) => return Err(format!("等待进程失败：{error}")),
            }
        };
        for reader in readers {
            let _ = reader.join();
        }
        while let Ok(item) = rx.try_recv() {
            handle(item, &mut stdout, &mut tail);
        }
        if status.success() {
            Ok(stdout)
        } else {
            let last = tail
                .back()
                .cloned()
                .unwrap_or_else(|| format!("退出码 {:?}", status.code()));
            Err(last)
        }
    }
}

/// 删除 MOSS 创建的 Laya 虚拟环境与受管临时目录（不带标记的环境与用户自有目录一律不动）；`root` 为空目录时一并删除。
/// 返回是否删除了虚拟环境。
pub fn remove_managed_env(root: &Path) -> Result<bool, String> {
    let venv = root.join(".venv");
    let mut removed = false;
    if is_managed(&venv) {
        fs::remove_dir_all(&venv).map_err(|error| {
            format!("删除 Laya 运行环境失败（文件可能正被占用，关闭后重试）：{error}")
        })?;
        removed = true;
    }
    let tmp = root.join("tmp");
    let tmp_managed = is_managed(&tmp);
    if removed || tmp_managed {
        let _ = fs::remove_dir_all(&tmp);
        let _ = fs::remove_dir(root);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// 用本机真实 Python 探测（不在常规测试中运行，不安装任何东西）：
    /// `cargo test -p moss-voice real_python_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_python_probe() {
        let host = SystemHost;
        println!("candidates: {:?}", host.python_candidates());
        println!("found: {:?}", find_python(&host, &AtomicBool::new(false)));
        println!("nvidia-smi: {:?}", host.nvidia_smi());
    }

    #[test]
    fn parses_python_probe_and_driver_cuda() {
        let exe = if cfg!(windows) {
            r"C:\Python312\python.exe"
        } else {
            "/usr/local/bin/python3.12"
        };
        assert_eq!(
            parse_python_probe(&format!("3.12\n{exe}\n")),
            Some(((3, 12), PathBuf::from(exe)))
        );
        assert_eq!(parse_python_probe("3.12\nrelative/python\n"), None);
        assert_eq!(parse_python_probe("garbage"), None);
        let smi = "| NVIDIA-SMI 560.94    Driver Version: 560.94    CUDA Version: 12.6     |";
        assert_eq!(parse_driver_cuda(smi), Some((12, 6)));
        assert_eq!(parse_driver_cuda("CUDA Version: 11.4"), Some((11, 4)));
        assert_eq!(parse_driver_cuda("no gpu"), None);
    }

    #[test]
    fn torch_plan_follows_platform_and_driver() {
        assert_eq!(
            plan_torch(Platform::MacOs, Some((12, 4))).0,
            TorchPlan::Pypi
        );
        assert_eq!(
            plan_torch(Platform::Windows, Some((13, 0))).0,
            TorchPlan::Cuda {
                tag: "cu128",
                version: (13, 0)
            }
        );
        assert_eq!(
            plan_torch(Platform::Windows, Some((12, 6))).0,
            TorchPlan::Cuda {
                tag: "cu126",
                version: (12, 6)
            }
        );
        assert_eq!(
            plan_torch(Platform::Windows, Some((12, 5))).0,
            TorchPlan::Cuda {
                tag: "cu124",
                version: (12, 5)
            }
        );
        assert_eq!(
            plan_torch(Platform::Linux, Some((12, 2))).0,
            TorchPlan::Cuda {
                tag: "cu121",
                version: (12, 2)
            }
        );
        assert_eq!(
            plan_torch(Platform::Windows, Some((11, 8))).0,
            TorchPlan::Cuda {
                tag: "cu118",
                version: (11, 8)
            }
        );
        let (old, note) = plan_torch(Platform::Windows, Some((11, 4)));
        assert_eq!(old, TorchPlan::Pypi);
        assert!(note.contains("驱动过旧"), "{note}");
        let (none, note) = plan_torch(Platform::Windows, None);
        assert_eq!(none, TorchPlan::Pypi);
        assert!(note.contains("未检测到 NVIDIA"), "{note}");
        assert_eq!(plan_torch(Platform::Linux, None).0, TorchPlan::CpuIndex);
    }

    #[test]
    fn pip_steps_use_cuda_index_no_cache_and_mirror_order() {
        let steps = pip_steps(
            &TorchPlan::Cuda {
                tag: "cu124",
                version: (12, 4),
            },
            true,
        );
        assert_eq!(steps.len(), 3);
        assert!(!steps[0].fatal && steps[1].fatal && steps[2].fatal);
        let torch = steps[1].attempts[0].join(" ");
        assert!(
            torch.contains("torch --index-url https://download.pytorch.org/whl/cu124"),
            "{torch}"
        );
        assert!(
            torch.contains("--extra-index-url https://pypi.tuna.tsinghua.edu.cn/simple"),
            "{torch}"
        );
        let laya = steps[2]
            .attempts
            .iter()
            .map(|args| args.join(" "))
            .collect::<Vec<_>>();
        assert!(laya[0].contains("laya[serve] -i https://pypi.tuna.tsinghua.edu.cn/simple"));
        assert!(laya[1].contains("-i https://pypi.org/simple"));
        assert!(
            steps
                .iter()
                .flat_map(|step| &step.attempts)
                .all(|args| args.contains(&"--no-cache-dir".to_owned()))
        );
        // macOS / Windows CPU：PyTorch 交给 laya[serve] 从 PyPI 拉
        assert_eq!(pip_steps(&TorchPlan::Pypi, false).len(), 2);
        assert!(
            pip_steps(&TorchPlan::CpuIndex, false)[1].attempts[0]
                .join(" ")
                .contains("whl/cpu")
        );
    }

    /// 假环境：按命令内容返回脚本化结果；`-m venv` 时在临时目录里造出虚拟环境的 Python。
    struct FakeHost {
        platform: Platform,
        pythons: Vec<(&'static str, &'static str)>,
        smi: Option<&'static str>,
        fail_torch_once: bool,
        torch_calls: AtomicUsize,
        calls: Mutex<Vec<String>>,
        block_on_laya: Arc<AtomicBool>,
    }

    impl FakeHost {
        fn new(platform: Platform) -> Self {
            Self {
                platform,
                pythons: Vec::new(),
                smi: None,
                fail_torch_once: false,
                torch_calls: AtomicUsize::new(0),
                calls: Mutex::default(),
                block_on_laya: Arc::default(),
            }
        }
    }

    impl SetupHost for FakeHost {
        fn platform(&self) -> Platform {
            self.platform
        }
        fn python_candidates(&self) -> Vec<PythonCandidate> {
            self.pythons
                .iter()
                .map(|(program, _)| PythonCandidate {
                    program: PathBuf::from(program),
                    args: Vec::new(),
                })
                .collect()
        }
        fn nvidia_smi(&self) -> Option<PathBuf> {
            self.smi.map(|_| PathBuf::from("nvidia-smi"))
        }
        fn run(
            &self,
            program: &Path,
            args: &[String],
            cancel: &AtomicBool,
            _: Option<Duration>,
            line: LineSink<'_>,
        ) -> Result<String, String> {
            let joined = format!("{} {}", program.display(), args.join(" "));
            self.calls.lock().unwrap().push(joined.clone());
            if args.iter().any(|arg| arg == PYTHON_PROBE) {
                return self
                    .pythons
                    .iter()
                    .find(|(name, _)| Path::new(name) == program)
                    .map(|(_, out)| (*out).to_owned())
                    .filter(|out| !out.is_empty())
                    .ok_or_else(|| "not found".to_owned());
            }
            if program == Path::new("nvidia-smi") {
                return Ok(self.smi.unwrap_or_default().to_owned());
            }
            if args.len() == 3 && args[0] == "-m" && args[1] == "venv" {
                let py = venv_python(Path::new(&args[2]), self.platform);
                fs::create_dir_all(py.parent().unwrap()).unwrap();
                fs::write(py, b"").unwrap();
                return Ok(String::new());
            }
            if joined.contains(" torch --index-url") {
                line("Downloading torch-2.5.1+cu124-cp312-cp312-win_amd64.whl (2.5 GB)");
                if self.fail_torch_once && self.torch_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err("Read timed out".into());
                }
            }
            if joined.contains("laya[serve]") && self.block_on_laya.load(Ordering::SeqCst) {
                while !cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                return Err("已取消".into());
            }
            if args.iter().any(|arg| arg == VERIFY_PROBE) {
                return Ok(if self.smi.is_some() { "cuda" } else { "cpu" }.into());
            }
            Ok(String::new())
        }
    }

    fn wait_done(setup: &LayaSetup) -> SetupStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        while setup.running() {
            assert!(Instant::now() < deadline, "准备超时");
            std::thread::sleep(Duration::from_millis(10));
        }
        setup.status()
    }

    fn abs(name: &str) -> &'static str {
        Box::leak(
            if cfg!(windows) {
                format!(r"C:\py\{name}.exe")
            } else {
                format!("/py/{name}")
            }
            .into_boxed_str(),
        )
    }

    #[test]
    fn picks_preferred_python_and_explains_when_missing() {
        let mut host = FakeHost::new(Platform::Windows);
        let p39 = abs("p39");
        let p313 = abs("p313");
        let p312 = abs("p312");
        host.pythons = vec![
            (p39, Box::leak(format!("3.9\n{p39}").into_boxed_str())),
            ("missing", ""),
            (p313, Box::leak(format!("3.13\n{p313}").into_boxed_str())),
            (p312, Box::leak(format!("3.12\n{p312}").into_boxed_str())),
        ];
        let setup = LayaSetup::new(Arc::new(host));
        assert_eq!(
            setup.find_python().unwrap(),
            PythonFound {
                version: (3, 12),
                executable: PathBuf::from(p312)
            }
        );

        let mut none = FakeHost::new(Platform::Windows);
        none.pythons = vec![(p39, Box::leak(format!("3.9\n{p39}").into_boxed_str()))];
        let error = LayaSetup::new(Arc::new(none)).find_python().unwrap_err();
        assert!(
            error.contains("python.org") && error.contains("OneJev"),
            "{error}"
        );
    }

    #[test]
    fn setup_creates_marked_venv_installs_cuda_torch_and_retries_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("laya");
        let p312 = abs("p312");
        let mut host = FakeHost::new(Platform::Windows);
        host.pythons = vec![(p312, Box::leak(format!("3.12\n{p312}").into_boxed_str()))];
        host.smi = Some("Driver Version: 560.94   CUDA Version: 12.4");
        host.fail_torch_once = true;
        let host = Arc::new(host);
        let setup = LayaSetup::new(host.clone());
        setup
            .start(SetupRequest {
                root: root.clone(),
                prefer_mirror: false,
            })
            .unwrap();
        let status = wait_done(&setup);
        let SetupStatus::Done {
            summary,
            accelerator,
        } = status
        else {
            panic!("{status:?}")
        };
        assert_eq!(accelerator, "cuda");
        assert!(
            summary.contains("CUDA 12.4") && summary.contains("NVIDIA CUDA 显卡加速"),
            "{summary}"
        );
        assert!(is_managed(&root.join(".venv")));
        let calls = host.calls.lock().unwrap().clone();
        let torch: Vec<_> = calls
            .iter()
            .filter(|call| call.contains(" torch --index-url"))
            .collect();
        assert_eq!(torch.len(), 2, "第一次超时后换备用 PyPI 重试");
        assert!(torch[0].contains("whl/cu124") && torch[0].contains("pypi.org"));
        assert!(torch[1].contains("pypi.tuna"));
        assert!(calls.iter().any(|call| call.contains("-m venv")));

        // 删除：带标记的环境连同 tmp 一起删掉，空的根目录也删掉
        fs::create_dir_all(root.join("tmp")).unwrap();
        assert!(remove_managed_env(&root).unwrap());
        assert!(!root.exists());

        // 用户自定义目录（如 MOSS_LAYA_ROOT）没有受管 .venv 时，绝不误删其中的 tmp/
        let user_root = dir.path().join("user-laya-root");
        let user_file = user_root.join("tmp").join("keep.txt");
        fs::create_dir_all(user_file.parent().unwrap()).unwrap();
        fs::write(&user_file, b"user-owned").unwrap();
        assert!(!remove_managed_env(&user_root).unwrap());
        assert!(user_file.is_file());
    }

    #[test]
    fn setup_without_python_fails_with_explanation_and_cpu_note_without_gpu() {
        let dir = tempfile::tempdir().unwrap();
        let setup = LayaSetup::new(Arc::new(FakeHost::new(Platform::Windows)));
        setup
            .start(SetupRequest {
                root: dir.path().join("laya"),
                prefer_mirror: false,
            })
            .unwrap();
        let SetupStatus::Failed { message, .. } = wait_done(&setup) else {
            panic!()
        };
        assert!(message.contains("未检测到可用的 Python"), "{message}");
        assert!(
            !dir.path().join("laya").exists(),
            "没有 Python 时不创建任何目录"
        );

        let p311 = abs("p311");
        let mut host = FakeHost::new(Platform::Windows);
        host.pythons = vec![(p311, Box::leak(format!("3.11\n{p311}").into_boxed_str()))];
        let setup = LayaSetup::new(Arc::new(host));
        setup
            .start(SetupRequest {
                root: dir.path().join("laya"),
                prefer_mirror: false,
            })
            .unwrap();
        let SetupStatus::Done {
            summary,
            accelerator,
        } = wait_done(&setup)
        else {
            panic!()
        };
        assert_eq!(accelerator, "cpu");
        assert!(summary.contains("未检测到 NVIDIA 显卡驱动"), "{summary}");
    }

    #[test]
    fn cancel_stops_pip_and_unmanaged_venv_is_never_removed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("laya");
        let p312 = abs("p312");
        let mut host = FakeHost::new(Platform::Linux);
        host.pythons = vec![(p312, Box::leak(format!("3.12\n{p312}").into_boxed_str()))];
        host.block_on_laya.store(true, Ordering::SeqCst);
        let setup = LayaSetup::new(Arc::new(host));
        setup
            .start(SetupRequest {
                root: root.clone(),
                prefer_mirror: false,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(setup.status(), SetupStatus::Running { ref stage, .. } if stage.contains("laya"))
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        setup.cancel_and_wait();
        assert_eq!(setup.status(), SetupStatus::Cancelled);

        // 用户自己建的环境（无标记）不删
        let user = dir.path().join("user");
        fs::create_dir_all(user.join(".venv").join("bin")).unwrap();
        assert!(!remove_managed_env(&user).unwrap());
        assert!(user.join(".venv").exists());
    }
}
