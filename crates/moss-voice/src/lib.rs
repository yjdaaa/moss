//! 语音中心（技术书 §9.4，实施清单 P4）。
//!
//! - [`settings`]：`settings.json` 中的 `voice` 设置与链路模式（P4-01）；
//! - [`redact`]：日志脱敏（A-66）；
//! - [`logfile`]：服务端日志写入与 5 MB × 3 轮转（P4-04）；
//! - [`supervisor`]：xiaozhi-server 子进程启动、停止、崩溃记录（P4-02 / P4-04）；
//! - [`models`] / [`config_yaml`]：模型配置向导的测试连接与 `data/.config.yaml` 写入（P4-06）；
//! - [`health`]：接入点健康检查（设备在线状态）；
//! - [`builtin`]：内置语音服务的模型设置与密钥处理计划（内置语音技术书 NV5）；
//! - [`local_models`]：本地模型的下载、SHA-256 校验与安装（NV6）；
//! - [`center`]：链路切换、启停、向导等页面操作的编排（P4-05 / P4-07）。
//!
//! 本 crate 不依赖 Tauri，便于单元测试；只由桌面壳在非 Android 平台使用。

pub mod builtin;
pub mod center;
pub mod config_yaml;
pub mod health;
pub mod home_readonly;
pub mod home_backup_audit;
pub mod local_models;
pub mod logfile;
pub mod models;
pub mod onejev;
pub mod redact;
pub mod settings;
pub mod supervisor;

pub use center::{CenterError, HostHooks, MAX_EXTRA_ENDPOINTS, VoiceCenter};
pub use logfile::RotatingLog;
pub use models::{ModelsInput, ModelsListResult, ProviderKind, Section, SectionInput, TestResult};
pub use redact::redact_line;
pub use settings::{VoiceMode, VoiceSettings};
pub use supervisor::{ServerSpec, ServerState, ServerStatus, Supervisor, VoiceError};
