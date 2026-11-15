//! 本地 OneJev 1.6（0.8B / 4B / 9B）多模态 System 1 决策运行时与跨平台硬件加速网关。
//!
//! - **免 Python / 免 PyTorch**：由 MOSS 原生 Rust 直接实现 `OneJev`（`qev-labels-v2` / `onejev-slot-v2`）
//!   的提示渲染、共享前缀 KV 预填、槽位对数概率读取与置信度校准，对外提供标准
//!   `POST /v1/systemone` 与 `GET /health`；
//! - **跨平台异构硬件加速**：
//!   - macOS Apple Silicon（M1–M6）：启用 `Apple Metal` 统一内存 GPU 加速（`-ngl all`）；
//!   - macOS Intel：启用 CPU 推理（`-ngl 0`）；
//!   - Windows：优先识别用户放置的 `NVIDIA CUDA` 构建（`ggml-cuda.dll`），否则在具备现代显卡驱动
//!     （`vulkan-1.dll`）的机器上自动使用 `Vulkan` 显卡加速（通吃 NVIDIA / AMD / Intel 独显与核显），
//!     无显卡环境自动回退 AVX2 CPU（`-ngl 0`）；
//!   - Linux：识别 `CUDA` / `Vulkan` 或回退 CPU；
//! - **按需自动就绪**：若本机未安装 `llama-server`，下载 OneJev 模型或首次启动时自动拉取对应平台官方预编译包
//!   并逐字节核对 SHA-256 摘要，解压到 `<models>/.runtime/llama-b12167/`。

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::local_models::{fetch_verified_file, safe_relative};
use crate::supervisor::JobHandle;

mod archive;
mod gateway;
mod prompt;
mod runtime;
mod upstream;

pub(crate) use archive::*;
pub use gateway::*;
use prompt::*;
pub use runtime::*;
use upstream::*;

pub const LLAMA_CPP_RELEASE_TAG: &str = "b12167";
const LLAMA_GITHUB_RELEASE_BASE: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b12167";
const LLAMA_GHFAST_MIRROR_BASE: &str =
    "https://ghfast.top/https://github.com/ggml-org/llama.cpp/releases/download/b12167";

const PROMPT_VERSION: &str = "qev-labels-v2";
const SYSTEM_PROMPT: &str = "Apply the question to the state. Choose exactly one of the listed options. Respond with only its uppercase letter, with no explanation or reasoning.";
const NOUL_DEFAULT_INSTRUCTIONS: &str =
    "Is the statement true, or is the answer to the question yes?";
const NOUL_DEFAULT_TRUE: &str = "the statement is true / the answer is yes";
const NOUL_DEFAULT_FALSE: &str = "the statement is false / the answer is no";
const NONE_OPTION: &str = "none of the options listed here";
const NONE_DESCRIPTION: &str =
    "The correct option is not among the options listed in this question.";
const LETTERS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const NOT_ONE_TOKEN: [&str; 25] = [
    "BQ", "BZ", "CJ", "CQ", "CZ", "DQ", "DZ", "EJ", "EY", "FJ", "FQ", "FV", "FZ", "GJ", "GK", "GQ",
    "GZ", "HJ", "IY", "JF", "JG", "JH", "JL", "JN", "JQ",
];
const MAX_HTTP_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MAX_HTTP_HEADER_LINES: usize = 64;
const MAX_ACTIVE_GATEWAY_CONNS: usize = 16;
const DEFAULT_CTX_TOKENS: usize = 8192;
const MAX_REQUEST_TOKENS: usize = 32768;

static RUNTIME_INSTALL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeArchiveKind {
    TarGz { top_dir: &'static str },
    Zip,
}

#[derive(Debug, Clone, Copy)]
struct LlamaReleaseAsset {
    platform_dir: &'static str,
    file_name: &'static str,
    sha256: &'static str,
    size: u64,
    archive: RuntimeArchiveKind,
    expected_accel: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorKind {
    Metal,
    Cuda,
    Vulkan,
    Cpu,
    CpuOverride,
}

impl AcceleratorKind {
    pub fn uses_gpu(&self) -> bool {
        matches!(self, Self::Metal | Self::Cuda | Self::Vulkan)
    }

    pub fn short_label(&self) -> &'static str {
        match self {
            Self::Metal => "Apple Metal GPU",
            Self::Cuda => "NVIDIA CUDA GPU",
            Self::Vulkan => "Vulkan GPU",
            Self::Cpu => "CPU",
            Self::CpuOverride => "CPU（环境变量强制）",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DetectedAccelerator {
    pub kind: AcceleratorKind,
    pub device_detail: Option<String>,
}

impl DetectedAccelerator {
    pub fn display_text(&self) -> String {
        match &self.device_detail {
            Some(detail) if !detail.is_empty() => {
                format!("{}（{detail}）", self.kind.short_label())
            }
            _ => self.kind.short_label().to_owned(),
        }
    }
}

/// 本地 OneJev 在语音链路里的等待预算（2026-11-07 用户决定）。
///
/// - `hold_ms`：静音判停后“这句话是不是没说完”的判断最多等多久；为 `None` 时不做这项判断。
///   必须短于会话续听窗口（1200ms），否则结果回来时窗口已过，只会白白多等。9B 来不及，固定关闭。
/// - 没有显卡加速时 4B / 9B 基本赶不上语音节奏：不再加长等待（否则每句话都更慢），
///   改为关闭“没说完”判断并在界面提示改用 0.8B。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalJevBudget {
    pub turn_ms: u64,
    pub hold_ms: Option<u64>,
    pub notice: Option<&'static str>,
}

pub const NOTICE_9B_NO_HOLD: &str =
    "9B 推理较慢，来不及在续听窗口内判断“没说完”，已自动关闭这一项（其余筛选照常）";
pub const NOTICE_SLOW_ON_CPU: &str =
    "当前没有显卡加速，4B / 9B 推理较慢，语音筛选经常来不及而被自动跳过；建议改用 0.8B";

/// 按模型档位与是否有显卡加速给出等待预算；Laya 等非 OneJev 模型返回 `None`（沿用默认值）。
pub fn local_jev_budget(model_id: &str, gpu: bool) -> Option<LocalJevBudget> {
    let budget = match (model_id, gpu) {
        ("onejev_1_6_0_8b", true) => LocalJevBudget {
            turn_ms: 1500,
            hold_ms: Some(600),
            notice: None,
        },
        ("onejev_1_6_0_8b", false) => LocalJevBudget {
            turn_ms: 2000,
            hold_ms: Some(800),
            notice: None,
        },
        ("onejev_1_6_4b", true) => LocalJevBudget {
            turn_ms: 2500,
            hold_ms: Some(800),
            notice: None,
        },
        ("onejev_1_6_4b", false) => LocalJevBudget {
            turn_ms: 2500,
            hold_ms: None,
            notice: Some(NOTICE_SLOW_ON_CPU),
        },
        ("onejev_1_6_9b", true) => LocalJevBudget {
            turn_ms: 3500,
            hold_ms: None,
            notice: Some(NOTICE_9B_NO_HOLD),
        },
        ("onejev_1_6_9b", false) => LocalJevBudget {
            turn_ms: 3500,
            hold_ms: None,
            notice: Some(NOTICE_SLOW_ON_CPU),
        },
        _ => return None,
    };
    Some(budget)
}

/// 未启动推理进程前，按推理引擎文件与平台预估是否会走显卡加速（与 `detect_onejev_env` 同一口径）。
pub fn expected_gpu(models_root: &Path) -> bool {
    detect_onejev_env(models_root, false)
        .get("accelerator")
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "metal" | "cuda" | "vulkan"))
}

#[cfg(test)]
mod tests;
