//! 语音中心（技术书 §9.4，实施清单 P4）。
//!
//! - [`settings`]：`settings.json` 中的 `voice` 设置与链路模式（P4-01）；
//! - [`redact`]：日志与地址脱敏（A-66）；
//! - [`models`]：模型地址规范化与测试连接（内置服务的模型配置共用）；
//! - [`builtin`]：内置语音服务的模型设置与密钥处理计划（内置语音技术书 NV5）；
//! - [`local_models`]：本地模型的下载、SHA-256 校验与安装（NV6）；
//! - [`job`]：本地子进程的 Job Object 句柄（Windows 上随 MOSS 退出结束进程树）；
//! - [`center`]：链路切换（官方小智 / MOSS 内置服务）与设置保存的编排（P4-05 / P4-07）。
//!
//! 2026-11-17 起删除自建服务端链路（外部 xiaozhi-server 进程管理、`data/.config.yaml` 写入、
//! 接入点健康检查与服务端日志），旧设置读取时迁移为内置服务，见 [`settings::migrate_legacy_self_hosted`]。
//!
//! 本 crate 不依赖 Tauri，便于单元测试；只由桌面壳在非 Android 平台使用。

pub mod audio_cpp;
pub mod builtin;
mod center;
pub mod colab;
pub mod job;
pub mod laya_env;
pub mod local_models;
pub mod local_vision;
pub mod models;
pub mod onejev;
pub mod redact;
pub mod settings;
pub mod speech_cpp;
pub mod tts_voices;

pub use center::{CenterError, HostHooks, MAX_EXTRA_ENDPOINTS, VoiceCenter};
pub use models::{ModelsInput, ModelsListResult, ProviderKind, Section, SectionInput, TestResult};
pub use redact::{redact_line, redact_url};
pub use settings::{VoiceMode, VoiceSettings};
