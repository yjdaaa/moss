//! 激光真机空跑模式（LB2 / LC，2026-11-28 用户决定）：
//!
//! 在发送预检与最底层传输出口（[`DryRunTransport`]）强制执行两条硬约束，前端、脚本、MCP、语音、工作台均无法绕过：
//! 1. **不出光**：所有 `S` 字（含 `M3`/`M4` 后、`G1`/`G2`/`G3` 内、单独 `S` 字及紧凑写法）改写为 `S0`，
//!    不带 `S` 的 `M3`/`M4` 补 `S0`，任务开头补 `G21 G94 G1 M5 S0 F<折算上限>`；
//! 2. **慢速上限**：所有 `F` 值钳在折算上限内，`G0` 改写为 `G1 F<折算上限> S0`；裸 `G53` 与复位后首条无模态坐标行补 `G1 F<折算上限> S0`；
//!    禁止会绕过 `F` 限速的 `G28`/`G30`（回预存位置，按快速速度运动；`G28.1`/`G30.1` 仅存位置不运动，放行）、
//!    `G93`（反时间进给模式）、`G20`（英制单位，25.4× 放大速度）以及外部文件里的固件脚本/宏执行指令
//!    （`$LocalFS/Run`、`$Macro`、`$RM`、`$N0=`/`$N1=`、外部 `$SD/Run`）。
//!    `$H` 回零按固件自设回零速度执行，保持放行。
//!
//! 关于实时进给倍率（0x90–0x94，GRBL/FluidNC 最高 200%）：
//! 实时倍率字节照常放行（便于 L2 空跑验证实时倍率链路），因此下发到控制器的 `F` 上限按最大进给倍率
//! 200% 折半（`effective_max_feed = max_feed_mm_min / 2`），确保即使控制器此前残留或运行中调至
//! 200% 进给倍率，物理进给速度仍严格不超过用户设定的 `dry_run_max_feed_mm_min`。
//! 急停（`0x18`）、`!`、`~`、`?` 与裸急停句柄不受任何影响。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{DeviceTransport, RealtimeCommand};

/// 默认空跑速度上限（mm/min）。
pub const DRY_RUN_DEFAULT_MAX_FEED_MM_MIN: u32 = 2000;
/// 允许配置的最低空跑速度上限（mm/min）。
pub const DRY_RUN_MIN_FEED_MM_MIN: u32 = 300;
/// 允许配置的最高空跑速度上限（mm/min）。
pub const DRY_RUN_MAX_FEED_MM_MIN: u32 = 5000;
/// 空跑开关开启后的自动关闭时长（60 分钟）。
pub const DRY_RUN_TTL_SECS: u64 = 60 * 60;
/// GRBL / FluidNC 进给倍率最高 200%：下发 `F` 按 2 折算，保证 200% 倍率下实际速度仍不超上限。
pub const DRY_RUN_FEED_OVERRIDE_DIVISOR: u32 = 2;

/// 校验空跑速度上限是否在允许区间 `300..=5000` 内。
pub fn validate_dry_run_max_feed(value: u32) -> Result<u32, String> {
    if (DRY_RUN_MIN_FEED_MM_MIN..=DRY_RUN_MAX_FEED_MM_MIN).contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "空跑速度上限必须在 {DRY_RUN_MIN_FEED_MM_MIN} ~ {DRY_RUN_MAX_FEED_MM_MIN} mm/min 之间，收到 {value}"
        ))
    }
}

/// 空跑模式下被禁止的指令类别（会绕过 `F` 限速或主机侧改写）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRunBlockedKind {
    /// `G28` / `G30` 回预存位置（按固件最大快速速度移动，不受 `F` 约束；`G28.1`/`G30.1` 放行）
    RapidHome,
    /// `G93` 反时间进给模式或 `G20` 英制单位（会使 `F` 脱离 `mm/min` 限速上限）
    SpeedModeBypass,
    /// 外部 G-code 直接调用固件本地/SD 文件或宏执行（`$SD/Run`、`$LocalFS/Run`、`$Macro`、`$RM`、`$N0=`/`$N1=`）
    FirmwareExecBypass,
}

/// 空跑拦截命中明细（供发送预检报错与传输层兜底日志共用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunBlockedCommand {
    pub kind: DryRunBlockedKind,
    /// 规范化的指令名，如 `"G28"`、`"G30"`、`"G93"`、`"G20"`、`"$LocalFS/Run"`
    pub code: &'static str,
    /// 整文件预检时的 1-based 行号；传输层单行兜底时为 `None`
    pub line_number: Option<usize>,
    /// 触发拦截的原始行（已去首尾空白）
    pub original_line: String,
}

impl DryRunBlockedCommand {
    pub fn error_code(&self) -> &'static str {
        match self.kind {
            DryRunBlockedKind::RapidHome => "dry_run_rapid_home_blocked",
            DryRunBlockedKind::SpeedModeBypass => "dry_run_speed_mode_blocked",
            DryRunBlockedKind::FirmwareExecBypass => "dry_run_firmware_exec_blocked",
        }
    }

    pub fn user_message(&self) -> String {
        let base = match self.kind {
            DryRunBlockedKind::RapidHome => {
                "空跑模式下不支持 G28 / G30（会按机器最快速度移动），请去掉后重试".to_owned()
            }
            DryRunBlockedKind::SpeedModeBypass if self.code == "G93" => {
                "空跑模式下不支持 G93 反时间进给模式（会绕过 mm/min 限速），请改用 G94 后重试"
                    .to_owned()
            }
            DryRunBlockedKind::SpeedModeBypass => {
                "空跑模式下不支持 G20 英制单位（会按 25.4 倍放大进给速度），请改用 G21 毫米单位后重试"
                    .to_owned()
            }
            DryRunBlockedKind::FirmwareExecBypass => {
                format!(
                    "空跑模式下不支持在 G-code 中调用 {}（会绕过主机侧不出光与限速改写），请去掉后重试",
                    self.code
                )
            }
        };
        match self.line_number {
            Some(line_no) => format!("{base}（第 {line_no} 行：{}）", self.code),
            None => base,
        }
    }

    /// 传输层兜底替换后的无运动注释行（GRBL / FluidNC 会忽略注释并回复 `ok`）。
    pub fn blocked_comment(&self) -> String {
        format!("; dry-run blocked {}", self.code)
    }
}

/// 传输层兜底拦截到违规指令时的告警回调（由上层接入 `ProcessTrace` 并附带来源标签）。
pub type DryRunBlockHandler = Arc<dyn Fn(&DryRunBlockedCommand) + Send + Sync>;

/// 生效中的空跑改写策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DryRunPolicy {
    /// 用户设定的物理速度上限（mm/min，300..=5000）。
    pub max_feed_mm_min: u32,
}

impl DryRunPolicy {
    pub fn new(max_feed_mm_min: u32) -> Self {
        Self {
            max_feed_mm_min: max_feed_mm_min
                .clamp(DRY_RUN_MIN_FEED_MM_MIN, DRY_RUN_MAX_FEED_MM_MIN),
        }
    }

    /// 折算 200% 最大进给倍率后的 G-code `F` 钳位上限（mm/min）。
    pub fn effective_max_feed_mm_min(&self) -> u32 {
        (self.max_feed_mm_min / DRY_RUN_FEED_OVERRIDE_DIVISOR).max(1)
    }

    /// 任务开头注入的关光、毫米单位、每分钟进给、线性模态与限速初始化行（无轴字不运动，消除上电默认 G0/残留 G20/G93）。
    pub fn preamble_line(&self) -> String {
        format!("G21 G94 G1 M5 S0 F{}", self.effective_max_feed_mm_min())
    }

    /// 按空跑规则改写单行 G-code（幂等；遇到 `G28`/`G30`/`G93`/`G20` 等禁止指令时替换为无运动注释）。
    pub fn rewrite_line(&self, line: &str) -> String {
        rewrite_gcode_line_for_dry_run(line, self.effective_max_feed_mm_min())
    }

    /// 带模态状态感知的单行改写（供 `DryRunTransport` 使用）：
    /// `require_explicit_g1_on_axes` 为真时，若该行含轴坐标但没有显式 `G1`/`G2`/`G3`（例如控制器刚复位处于默认模态 `G0`），
    /// 自动补入 `G1 F<上限> S0`，防止裸坐标行以快速速度移动。
    fn rewrite_line_with_modal(
        &self,
        line: &str,
        require_explicit_g1_on_axes: bool,
    ) -> (String, bool) {
        let max_f = self.effective_max_feed_mm_min().max(1);
        let trimmed_start = line.trim_start();
        if trimmed_start.starts_with('$') {
            if trimmed_start.len() >= 3 && trimmed_start[..3].eq_ignore_ascii_case("$J=") {
                let lead_len = line.len() - trimmed_start.len();
                let prefix = &line[..lead_len + 3];
                let body = &trimmed_start[3..];
                let (rewritten, _) = rewrite_gcode_segment_inner(body, max_f, false, false);
                return (format!("{prefix}{rewritten}"), false);
            }
            return (line.to_owned(), false);
        }
        rewrite_gcode_segment_inner(line, max_f, true, require_explicit_g1_on_axes)
    }
}

/// 对外只读快照（供 `/api/system/info` 与 `/api/settings` 返回）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DryRunSnapshot {
    pub enabled: bool,
    pub max_feed_mm_min: u32,
    pub effective_max_feed_mm_min: u32,
    pub remaining_seconds: u64,
    pub expires_at: Option<u64>,
}

#[derive(Debug)]
struct DryRunInner {
    enabled: bool,
    max_feed_mm_min: u32,
    expires_at_unix: Option<u64>,
}

/// 进程内空跑模式运行时状态：默认关闭，开启后 60 分钟自动到期关闭，重启不继承开启状态。
#[derive(Debug)]
pub struct DryRunState {
    inner: Mutex<DryRunInner>,
}

impl Default for DryRunState {
    fn default() -> Self {
        Self::new(DRY_RUN_DEFAULT_MAX_FEED_MM_MIN)
    }
}

impl DryRunState {
    pub fn new(max_feed_mm_min: u32) -> Self {
        let clamped = max_feed_mm_min.clamp(DRY_RUN_MIN_FEED_MM_MIN, DRY_RUN_MAX_FEED_MM_MIN);
        Self {
            inner: Mutex::new(DryRunInner {
                enabled: false,
                max_feed_mm_min: clamped,
                expires_at_unix: None,
            }),
        }
    }

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn snapshot_locked(inner: &mut DryRunInner, now_unix: u64) -> DryRunSnapshot {
        if inner.enabled {
            let remaining = inner
                .expires_at_unix
                .map(|exp| exp.saturating_sub(now_unix))
                .unwrap_or(0);
            if remaining == 0 {
                inner.enabled = false;
                inner.expires_at_unix = None;
            }
        }
        let policy = DryRunPolicy::new(inner.max_feed_mm_min);
        let remaining_seconds = if inner.enabled {
            inner
                .expires_at_unix
                .map(|exp| exp.saturating_sub(now_unix))
                .unwrap_or(0)
        } else {
            0
        };
        DryRunSnapshot {
            enabled: inner.enabled,
            max_feed_mm_min: policy.max_feed_mm_min,
            effective_max_feed_mm_min: policy.effective_max_feed_mm_min(),
            remaining_seconds,
            expires_at: if inner.enabled {
                inner.expires_at_unix
            } else {
                None
            },
        }
    }

    /// 开启空跑模式并刷新 60 分钟倒计时（可同时更新速度上限）。
    pub fn enable_at(
        &self,
        now_unix: u64,
        max_feed_mm_min: Option<u32>,
    ) -> Result<DryRunSnapshot, String> {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(feed) = max_feed_mm_min {
            guard.max_feed_mm_min = validate_dry_run_max_feed(feed)?;
        }
        guard.enabled = true;
        guard.expires_at_unix = Some(now_unix.saturating_add(DRY_RUN_TTL_SECS));
        Ok(Self::snapshot_locked(&mut guard, now_unix))
    }

    pub fn enable(&self, max_feed_mm_min: Option<u32>) -> Result<DryRunSnapshot, String> {
        self.enable_at(Self::now_unix(), max_feed_mm_min)
    }

    /// 立即关闭空跑模式。
    pub fn disable(&self) -> DryRunSnapshot {
        let now = Self::now_unix();
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.enabled = false;
        guard.expires_at_unix = None;
        Self::snapshot_locked(&mut guard, now)
    }

    /// 仅更新速度上限（不改变开关与剩余时长）。
    pub fn set_max_feed(&self, max_feed_mm_min: u32) -> Result<DryRunSnapshot, String> {
        let validated = validate_dry_run_max_feed(max_feed_mm_min)?;
        let now = Self::now_unix();
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.max_feed_mm_min = validated;
        Ok(Self::snapshot_locked(&mut guard, now))
    }

    pub fn snapshot_at(&self, now_unix: u64) -> DryRunSnapshot {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::snapshot_locked(&mut guard, now_unix)
    }

    pub fn snapshot_now(&self) -> DryRunSnapshot {
        self.snapshot_at(Self::now_unix())
    }

    pub fn active_policy_at(&self, now_unix: u64) -> Option<DryRunPolicy> {
        let snap = self.snapshot_at(now_unix);
        snap.enabled
            .then(|| DryRunPolicy::new(snap.max_feed_mm_min))
    }

    pub fn active_policy_now(&self) -> Option<DryRunPolicy> {
        self.active_policy_at(Self::now_unix())
    }
}

/// 在传输层出口强制执行空跑改写与兜底拦截的包装器：
/// - 打开连接时若空跑已开启，则锁存该连接的空跑策略，防止任务中途到期或关开关导致激光突然开启；
/// - 打开连接后若空跑被开启，后续写入也会即时生效；
/// - 开关关闭后新打开的连接完全透传，不改写任何字节；
/// - 漏网的 `G28`/`G30`/`G93`/`G20` 或未改写固件文件/宏执行指令不下发运动，替换为 `; dry-run blocked <code>` 并触发告警回调；
/// - 实时字节（`0x18`/`!`/`~`/`?`/倍率）与裸急停句柄完全透传。
pub struct DryRunTransport<T> {
    inner: T,
    state: Arc<DryRunState>,
    latched: Option<DryRunPolicy>,
    on_blocked: Option<DryRunBlockHandler>,
    modal_motion_safe: AtomicBool,
}

impl<T> DryRunTransport<T> {
    pub fn wrap(inner: T, state: Arc<DryRunState>) -> Self {
        let latched = state.active_policy_now();
        Self {
            inner,
            state,
            latched,
            on_blocked: None,
            modal_motion_safe: AtomicBool::new(false),
        }
    }

    /// 供测试或固定策略场景直接构造常驻空跑传输层。
    pub fn with_policy(inner: T, policy: DryRunPolicy) -> Self {
        let state = Arc::new(DryRunState::new(policy.max_feed_mm_min));
        Self {
            inner,
            state,
            latched: Some(policy),
            on_blocked: None,
            modal_motion_safe: AtomicBool::new(false),
        }
    }

    /// 绑定传输层兜底拦截告警回调（如写入 `ProcessTrace` 并附带来源标签）。
    pub fn with_block_handler(mut self, handler: DryRunBlockHandler) -> Self {
        self.on_blocked = Some(handler);
        self
    }

    fn effective_policy(&self) -> Option<DryRunPolicy> {
        match (self.latched, self.state.active_policy_now()) {
            (Some(a), Some(b)) => Some(DryRunPolicy::new(a.max_feed_mm_min.min(b.max_feed_mm_min))),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }
}

impl<T: DeviceTransport> DeviceTransport for DryRunTransport<T> {
    fn write_line(&self, line: &str) -> Result<(), String> {
        if let Some(policy) = self.effective_policy() {
            if let Some(hit) = detect_dry_run_blocked_line_for_transport(line) {
                if let Some(handler) = &self.on_blocked {
                    handler(&hit);
                } else {
                    eprintln!(
                        "[laser-device][dry-run] 传输层兜底拦截 {}: {}",
                        hit.code, hit.original_line
                    );
                }
                return self.inner.write_line(&hit.blocked_comment());
            }
            let modal_safe = self.modal_motion_safe.load(Ordering::Acquire);
            let (rewritten, sets_safe_modal) = policy.rewrite_line_with_modal(line, !modal_safe);
            if sets_safe_modal {
                self.modal_motion_safe.store(true, Ordering::Release);
            }
            self.inner.write_line(&rewritten)
        } else {
            self.inner.write_line(line)
        }
    }

    fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
        if matches!(command, RealtimeCommand::EmergencyStop) {
            self.modal_motion_safe.store(false, Ordering::Release);
        }
        self.inner.write_realtime(command)
    }

    fn read_line(&self) -> Result<Option<String>, String> {
        self.inner.read_line()
    }

    fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> {
        self.inner.raw_abort_handle()
    }

    fn default_rx_buffer_size(&self) -> usize {
        self.inner.default_rx_buffer_size()
    }

    fn dry_run_policy(&self) -> Option<DryRunPolicy> {
        self.effective_policy()
    }
}

/// 检测单行待发送 G-code（含文件预检场景）是否包含空跑模式禁止的指令：
/// - `G28` / `G30`（回预存位置，按快速速度移动；`G28.1`/`G30.1` 仅存位置不运动，放行）；
/// - `G93`（反时间进给模式）与 `G20`（英制单位）；
/// - 外部 G-code 中的固件文件/宏执行指令（`$SD/Run`、`$LocalFS/Run`、`$Macro`、`$RM`、`$N0=`/`$N1=`）；
/// - `$J=` 点动正文内的上述禁止 G 指令。
///
/// `$H` / `$HX` / `$HY` / `$HZ` 回零按固件自设回零速度执行，放行。
pub fn detect_dry_run_blocked_line(line: &str) -> Option<DryRunBlockedCommand> {
    detect_dry_run_blocked_inner(line, false)
}

/// 传输层兜底检测：与 [`detect_dry_run_blocked_line`] 一致，仅放行 MOSS 自身经空跑改写后上传的
/// `$SD/Run=/sd/moss_...`（外部未改写路径仍拦截）。
pub fn detect_dry_run_blocked_line_for_transport(line: &str) -> Option<DryRunBlockedCommand> {
    detect_dry_run_blocked_inner(line, true)
}

/// 整份 G-code 行序列的空跑发送预检：发现首个禁止指令即返回带 1-based 行号的错误。
pub fn check_gcode_lines_for_dry_run(lines: &[String]) -> Result<(), DryRunBlockedCommand> {
    for (idx, line) in lines.iter().enumerate() {
        if let Some(mut hit) = detect_dry_run_blocked_line(line) {
            hit.line_number = Some(idx + 1);
            return Err(hit);
        }
    }
    Ok(())
}

fn detect_dry_run_blocked_inner(
    line: &str,
    allow_moss_rewritten_sd_run: bool,
) -> Option<DryRunBlockedCommand> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(';') {
        return None;
    }
    if trimmed.starts_with('$') {
        if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("$J=") {
            return detect_gcode_segment_blocked(&trimmed[3..], trimmed);
        }
        return detect_dollar_exec_blocked(trimmed, allow_moss_rewritten_sd_run);
    }
    detect_gcode_segment_blocked(trimmed, trimmed)
}

fn detect_dollar_exec_blocked(
    trimmed: &str,
    allow_moss_rewritten_sd_run: bool,
) -> Option<DryRunBlockedCommand> {
    let lower = trimmed.to_ascii_lowercase();
    let hit_code = if let Some(rest) = lower.strip_prefix("$sd/run")
        && (rest.is_empty() || rest.starts_with('=') || rest.starts_with(' '))
    {
        let target = rest.trim_start_matches(['=', ' ', '\t']);
        if allow_moss_rewritten_sd_run && target.starts_with("/sd/moss_") {
            return None;
        }
        Some("$SD/Run")
    } else if let Some(rest) = lower.strip_prefix("$localfs/run")
        && (rest.is_empty() || rest.starts_with('=') || rest.starts_with(' '))
    {
        Some("$LocalFS/Run")
    } else if lower.starts_with("$macros/run") || lower.starts_with("$macro") {
        Some("$Macro")
    } else if let Some(rest) = lower.strip_prefix("$rm")
        && (rest.is_empty() || rest.starts_with('=') || rest.starts_with(' '))
    {
        Some("$RM")
    } else if lower.starts_with("$n0=") || lower.starts_with("$n1=") {
        Some("$N")
    } else {
        None
    };

    hit_code.map(|code| DryRunBlockedCommand {
        kind: DryRunBlockedKind::FirmwareExecBypass,
        code,
        line_number: None,
        original_line: trimmed.to_owned(),
    })
}

fn split_semi_comment(segment: &str) -> (&str, Option<&str>) {
    let mut paren_depth: usize = 0;
    for (idx, ch) in segment.char_indices() {
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            ';' if paren_depth == 0 => {
                return (&segment[..idx], Some(&segment[idx..]));
            }
            _ => {}
        }
    }
    (segment, None)
}

fn detect_gcode_segment_blocked(segment: &str, original: &str) -> Option<DryRunBlockedCommand> {
    let (code_part, _) = split_semi_comment(segment);
    let bytes = code_part.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut paren: usize = 0;

    while i < len {
        let ch = code_part[i..].chars().next().unwrap();
        if ch == '(' {
            paren += 1;
            i += 1;
            continue;
        }
        if ch == ')' {
            paren = paren.saturating_sub(1);
            i += 1;
            continue;
        }
        if paren > 0 || !ch.is_ascii_alphabetic() {
            i += ch.len_utf8();
            continue;
        }

        let letter = ch.to_ascii_uppercase();
        i += 1;
        let num_ws_start = i;
        while i < len && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        let num_start = i;
        if i < len && matches!(bytes[i], b'+' | b'-') {
            i += 1;
        }
        let mut saw_digit = false;
        let mut saw_dot = false;
        while i < len {
            let b = bytes[i];
            if b.is_ascii_digit() {
                saw_digit = true;
                i += 1;
            } else if b == b'.' && !saw_dot {
                saw_dot = true;
                i += 1;
            } else {
                break;
            }
        }
        if !saw_digit {
            i = num_ws_start;
            continue;
        }
        if letter != 'G' {
            continue;
        }
        let Ok(val) = code_part[num_start..i].parse::<f64>() else {
            continue;
        };
        if !val.is_finite() {
            continue;
        }
        // G28 / G30（含 G028 / G28.0 / G+28 / g28）会快速移动；G28.1 / G30.1 仅保存坐标不运动，差值 0.1 > 1e-6 自动放行
        if (val - 28.0).abs() < 1e-6 {
            return Some(DryRunBlockedCommand {
                kind: DryRunBlockedKind::RapidHome,
                code: "G28",
                line_number: None,
                original_line: original.to_owned(),
            });
        }
        if (val - 30.0).abs() < 1e-6 {
            return Some(DryRunBlockedCommand {
                kind: DryRunBlockedKind::RapidHome,
                code: "G30",
                line_number: None,
                original_line: original.to_owned(),
            });
        }
        if (val - 93.0).abs() < 1e-6 {
            return Some(DryRunBlockedCommand {
                kind: DryRunBlockedKind::SpeedModeBypass,
                code: "G93",
                line_number: None,
                original_line: original.to_owned(),
            });
        }
        if (val - 20.0).abs() < 1e-6 {
            return Some(DryRunBlockedCommand {
                kind: DryRunBlockedKind::SpeedModeBypass,
                code: "G20",
                line_number: None,
                original_line: original.to_owned(),
            });
        }
    }
    None
}

/// 按空跑硬约束改写单行 G-code（纯函数，幂等）：
/// - `G28`/`G30`（`G28.1`/`G30.1` 放行）、`G93`、`G20` 与非 MOSS 改写的固件文件/宏执行替换为无运动注释 `; dry-run blocked <code>`；
/// - `$` 系统指令（`$$`/`$CD`/`$I`/`$X`/`$H`/`$RI=`/MOSS 改写后的 `$SD/Run=/sd/moss_*` 等）原样保留；`$J=` 点动命令钳位 `F` 与 `S0`；
/// - 注释（`;...`、`(...)`）原样保留；
/// - 所有 `S` 字统一改成 `S0`；`M3`/`M4` 无 `S` 时补 `S0`；
/// - 所有 `F` 字钳位到 `<= effective_max_feed`；
/// - `G0`/`G00` 改写为 `G1`，无 `F` 时补 `F<effective_max_feed>`，无 `S` 时补 `S0`；
/// - 裸 `G53`（未同行指定 `G1`/`G2`/`G3`）自动补 `G1 F<effective_max_feed> S0`，防止继承默认 `G0` 快速移动。
pub fn rewrite_gcode_line_for_dry_run(line: &str, effective_max_feed: u32) -> String {
    if let Some(hit) = detect_dry_run_blocked_line_for_transport(line) {
        return hit.blocked_comment();
    }
    let max_f = effective_max_feed.max(1);
    let trimmed_start = line.trim_start();
    if trimmed_start.starts_with('$') {
        if trimmed_start.len() >= 3 && trimmed_start[..3].eq_ignore_ascii_case("$J=") {
            let lead_len = line.len() - trimmed_start.len();
            let prefix = &line[..lead_len + 3];
            let body = &trimmed_start[3..];
            let (rewritten, _) = rewrite_gcode_segment_inner(body, max_f, false, false);
            return format!("{prefix}{rewritten}");
        }
        return line.to_owned();
    }
    rewrite_gcode_segment_inner(line, max_f, true, false).0
}

fn rewrite_gcode_segment_inner(
    segment: &str,
    max_f: u32,
    allow_g1_injection: bool,
    require_explicit_g1_on_axes: bool,
) -> (String, bool) {
    // 分离行尾 `;` 注释，避免改写注释文本，同时将补加的 G1/F/S0 放在注释之前
    let (code_part, trailing_comment) = split_semi_comment(segment);

    let bytes = code_part.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut out = String::with_capacity(code_part.len() + 24);
    let mut paren: usize = 0;
    let mut saw_s = false;
    let mut saw_f = false;
    let mut rewrote_g0 = false;
    let mut has_m3_or_m4 = false;
    let mut has_g53 = false;
    let mut has_explicit_safe_motion = false;
    let mut has_non_motion_coord_g = false;
    let mut has_axis_word = false;

    while i < len {
        let ch = code_part[i..].chars().next().unwrap();
        if ch == '(' {
            paren += 1;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == ')' {
            paren = paren.saturating_sub(1);
            out.push(ch);
            i += 1;
            continue;
        }
        if paren > 0 || !ch.is_ascii_alphabetic() {
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }

        let letter = ch.to_ascii_uppercase();
        let word_start = i;
        i += 1;
        // GRBL/FluidNC 允许字地址与数值间有空白和前导正负号
        let num_ws_start = i;
        while i < len && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        let num_start = i;
        if i < len && matches!(bytes[i], b'+' | b'-') {
            i += 1;
        }
        let mut saw_digit = false;
        let mut saw_dot = false;
        while i < len {
            let b = bytes[i];
            if b.is_ascii_digit() {
                saw_digit = true;
                i += 1;
            } else if b == b'.' && !saw_dot {
                saw_dot = true;
                i += 1;
            } else {
                break;
            }
        }
        if !saw_digit {
            // 非合法字地址（回退到紧跟字母处原样输出）
            out.push(ch);
            i = num_ws_start;
            continue;
        }
        let num_str = &code_part[num_start..i];
        let parsed = num_str.parse::<f64>();

        match letter {
            'S' => {
                saw_s = true;
                out.push('S');
                out.push('0');
            }
            'M' => {
                if let Ok(val) = parsed {
                    let code = val.round() as i64;
                    if (val - code as f64).abs() < 1e-6 && (code == 3 || code == 4) {
                        has_m3_or_m4 = true;
                    }
                }
                out.push_str(&code_part[word_start..i]);
            }
            'G' => {
                if let Ok(val) = parsed
                    && val.is_finite()
                {
                    if val.abs() < 1e-6 {
                        rewrote_g0 = true;
                        out.push_str("G1");
                        continue;
                    }
                    if (val - 53.0).abs() < 1e-6 {
                        has_g53 = true;
                    } else if (val - 1.0).abs() < 1e-6
                        || (val - 2.0).abs() < 1e-6
                        || (val - 3.0).abs() < 1e-6
                        || ((38.0..39.0).contains(&val))
                    {
                        has_explicit_safe_motion = true;
                    } else if (val - 4.0).abs() < 1e-6
                        || (val - 10.0).abs() < 1e-6
                        || (val - 28.1).abs() < 1e-6
                        || (val - 30.1).abs() < 1e-6
                        || (val - 92.0).abs() < 1e-6
                    {
                        has_non_motion_coord_g = true;
                    }
                }
                out.push_str(&code_part[word_start..i]);
            }
            'F' => {
                saw_f = true;
                match parsed {
                    Ok(val) if val.is_finite() && val > 0.0 && val <= max_f as f64 => {
                        out.push_str(&code_part[word_start..i]);
                    }
                    _ => {
                        out.push('F');
                        out.push_str(&max_f.to_string());
                    }
                }
            }
            'X' | 'Y' | 'Z' | 'A' | 'B' | 'C' => {
                has_axis_word = true;
                out.push_str(&code_part[word_start..i]);
            }
            _ => {
                out.push_str(&code_part[word_start..i]);
            }
        }
    }

    let trailing_ws_len = out.len() - out.trim_end_matches([' ', '\t']).len();
    let trailing_ws = out[out.len() - trailing_ws_len..].to_owned();
    out.truncate(out.len() - trailing_ws_len);

    let needs_injected_g1 = allow_g1_injection
        && !rewrote_g0
        && !has_explicit_safe_motion
        && !has_non_motion_coord_g
        && (has_g53 || (require_explicit_g1_on_axes && has_axis_word));

    if needs_injected_g1 {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("G1");
    }

    if rewrote_g0 || needs_injected_g1 {
        if !saw_f {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push('F');
            out.push_str(&max_f.to_string());
        }
        if !saw_s {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str("S0");
        }
    } else if has_m3_or_m4 && !saw_s {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str("S0");
    }

    if let Some(comment) = trailing_comment {
        if trailing_ws.is_empty() {
            out.push(' ');
        } else {
            out.push_str(&trailing_ws);
        }
        out.push_str(comment);
    }

    let sets_safe_motion_modal = rewrote_g0 || has_explicit_safe_motion || needs_injected_g1;
    (out, sets_safe_motion_modal)
}

/// 批量改写 G-code 行序列（用于 SD 脱机上传等整文件出口）：开头强制补 `G21 G94 G1 M5 S0 F<上限>`，逐行改写。
pub fn rewrite_gcode_lines_for_dry_run(lines: &[String], policy: DryRunPolicy) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len() + 1);
    out.push(policy.preamble_line());
    for line in lines {
        out.push(policy.rewrite_line(line));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceConnection, FakeGrbl, JobWorker};

    #[test]
    fn rewrites_all_spindle_s_values_to_zero() {
        let p = DryRunPolicy::new(2000); // effective F max = 1000
        assert_eq!(p.effective_max_feed_mm_min(), 1000);
        assert_eq!(p.rewrite_line("M3 S500"), "M3 S0");
        assert_eq!(p.rewrite_line("M4 S1000"), "M4 S0");
        assert_eq!(p.rewrite_line("M03"), "M03 S0");
        assert_eq!(p.rewrite_line("M4"), "M4 S0");
        assert_eq!(p.rewrite_line("G1 X10 Y20 S800 F900"), "G1 X10 Y20 S0 F900");
        assert_eq!(
            p.rewrite_line("G2 X5 Y5 I2 J0 S300 F800"),
            "G2 X5 Y5 I2 J0 S0 F800"
        );
        assert_eq!(
            p.rewrite_line("G3 X5 Y5 I2 J0 S+450 F500"),
            "G3 X5 Y5 I2 J0 S0 F500"
        );
        assert_eq!(p.rewrite_line("S650"), "S0");
        assert_eq!(p.rewrite_line("G1X10S500F3000"), "G1X10S0F1000");
        assert_eq!(
            p.rewrite_line("N12 G1 X10 S 400 F 2500"),
            "N12 G1 X10 S0 F1000"
        );
    }

    #[test]
    fn rewrites_g0_and_clamps_feed_to_override_safe_limit() {
        let p = DryRunPolicy::new(2000); // effective = 1000
        assert_eq!(p.rewrite_line("G0 X10 Y20"), "G1 X10 Y20 F1000 S0");
        assert_eq!(p.rewrite_line("G00 X10 Y20 F6000"), "G1 X10 Y20 F1000 S0");
        assert_eq!(p.rewrite_line("G0 X10 Y20 F500"), "G1 X10 Y20 F500 S0");
        assert_eq!(p.rewrite_line("G0"), "G1 F1000 S0");
        assert_eq!(p.rewrite_line("N5 G0 X10 Y20"), "N5 G1 X10 Y20 F1000 S0");
        assert_eq!(p.rewrite_line("G53 G0 X0 Y0"), "G53 G1 X0 Y0 F1000 S0");
        // 裸 G53 不带 G0/G1 时同样强制补 G1 F<上限> S0，防止继承默认模态 G0 快速移动
        assert_eq!(p.rewrite_line("G53 X0 Y0"), "G53 X0 Y0 G1 F1000 S0");
        assert_eq!(p.rewrite_line("G53 G1 X0 Y0 F600"), "G53 G1 X0 Y0 F600");
        assert_eq!(p.rewrite_line("G1 X10 F3000"), "G1 X10 F1000");
        assert_eq!(p.rewrite_line("G1 X10 F600"), "G1 X10 F600");
        // 系统指令原样保留（含 $H 回零），仅 $J= 点动限速关光
        assert_eq!(p.rewrite_line("$$"), "$$");
        assert_eq!(p.rewrite_line("$CD"), "$CD");
        assert_eq!(p.rewrite_line("$H"), "$H");
        assert_eq!(p.rewrite_line("$RI=200"), "$RI=200");
        assert_eq!(
            p.rewrite_line("$SD/Run=/sd/moss_wf1_deadbeef.gcode"),
            "$SD/Run=/sd/moss_wf1_deadbeef.gcode"
        );
        assert_eq!(p.rewrite_line("$J=G91 X5 F4000 S200"), "$J=G91 X5 F1000 S0");
        assert_eq!(p.rewrite_line("$J=G53 X5 F4000"), "$J=G53 X5 F1000");
        // 幂等性
        let once = p.rewrite_line("N7 G0 X20 Y15 F3000 S800");
        assert_eq!(once, "N7 G1 X20 Y15 F1000 S0");
        assert_eq!(p.rewrite_line(&once), once);
        // 注释不误改
        assert_eq!(
            p.rewrite_line("G1 X10 S500 F3000 ; keep S500 F3000"),
            "G1 X10 S0 F1000 ; keep S500 F3000"
        );
    }

    #[test]
    fn blocks_g28_g30_and_speed_mode_bypasses_while_allowing_g28_1_g30_1() {
        let p = DryRunPolicy::new(2000);
        // G28 / G30 各种写法：大写、带坐标、前导零、小数 .0、小写、行内注释、行尾注释、N 行号、$J= 内
        for (line, expected_code) in [
            ("G28", "G28"),
            ("G30", "G30"),
            ("G28 X0 Y0", "G28"),
            ("G30 X10 Y20 Z0", "G30"),
            ("G028", "G28"),
            ("G28.0 X5", "G28"),
            ("G30.0", "G30"),
            ("g28", "G28"),
            ("g30 x0 y0", "G30"),
            ("G 28 X0", "G28"),
            ("G+30", "G30"),
            ("(go home) G28 X0 Y0 ; trailing", "G28"),
            ("N15 G28", "G28"),
            ("G91 G28 Z0", "G28"),
            ("$J=G28 X0 F500", "G28"),
        ] {
            let hit = detect_dry_run_blocked_line(line)
                .unwrap_or_else(|| panic!("应拦截 `{line}` 但未拦截"));
            assert_eq!(hit.kind, DryRunBlockedKind::RapidHome, "line={line}");
            assert_eq!(hit.code, expected_code, "line={line}");
            assert_eq!(hit.error_code(), "dry_run_rapid_home_blocked");
            assert_eq!(
                p.rewrite_line(line),
                format!("; dry-run blocked {expected_code}"),
                "line={line}"
            );
        }

        // G28.1 / G30.1 仅存储预存坐标，不运动 -> 放行；注释里的 G28/G30 放行；$H 回零放行
        for allowed in [
            "G28.1",
            "g28.1",
            "G30.1",
            "G28.1 ; store home",
            "(G28) G1 X10 F500",
            "G1 X10 F500 ; G30",
            "; G28 X0 Y0",
            "$H",
        ] {
            assert_eq!(
                detect_dry_run_blocked_line(allowed),
                None,
                "应放行 `{allowed}`"
            );
        }

        // G93（反时间进给）、G20（英制单位）与外部固件文件/宏执行均拦下
        for (line, expected_kind, expected_code) in [
            ("G93 G1 X50 F60", DryRunBlockedKind::SpeedModeBypass, "G93"),
            ("g20", DryRunBlockedKind::SpeedModeBypass, "G20"),
            (
                "$LocalFS/Run=/macro.gcode",
                DryRunBlockedKind::FirmwareExecBypass,
                "$LocalFS/Run",
            ),
            (
                "$SD/Run=/sd/external.gcode",
                DryRunBlockedKind::FirmwareExecBypass,
                "$SD/Run",
            ),
            (
                "$Macros/Run=0",
                DryRunBlockedKind::FirmwareExecBypass,
                "$Macro",
            ),
            ("$N0=G0 X100", DryRunBlockedKind::FirmwareExecBypass, "$N"),
        ] {
            let hit = detect_dry_run_blocked_line(line)
                .unwrap_or_else(|| panic!("应拦截 `{line}` 但未拦截"));
            assert_eq!(hit.kind, expected_kind, "line={line}");
            assert_eq!(hit.code, expected_code, "line={line}");
            assert_eq!(
                p.rewrite_line(line),
                format!("; dry-run blocked {expected_code}"),
                "line={line}"
            );
        }

        // 整文件预检报告精确 1-based 行号与提示文案
        let err = check_gcode_lines_for_dry_run(&[
            "G21".into(),
            "G90".into(),
            "g28 x0 y0".into(),
            "G1 X10 F500".into(),
        ])
        .unwrap_err();
        assert_eq!(err.line_number, Some(3));
        assert_eq!(err.error_code(), "dry_run_rapid_home_blocked");
        assert!(err.user_message().contains("G28 / G30"));
        assert!(err.user_message().contains("第 3 行"));
    }

    #[test]
    fn dry_run_state_expires_after_ttl_and_validates_feed_bounds() {
        assert!(validate_dry_run_max_feed(299).is_err());
        assert!(validate_dry_run_max_feed(5001).is_err());
        assert_eq!(validate_dry_run_max_feed(300), Ok(300));
        assert_eq!(validate_dry_run_max_feed(5000), Ok(5000));

        let state = DryRunState::default();
        assert!(!state.snapshot_at(1000).enabled);
        let on = state.enable_at(1000, Some(1600)).unwrap();
        assert!(on.enabled);
        assert_eq!(on.max_feed_mm_min, 1600);
        assert_eq!(on.effective_max_feed_mm_min, 800);
        assert_eq!(on.remaining_seconds, DRY_RUN_TTL_SECS);
        assert_eq!(on.expires_at, Some(1000 + DRY_RUN_TTL_SECS));

        // 59 分钟后仍开启
        let mid = state.snapshot_at(1000 + DRY_RUN_TTL_SECS - 60);
        assert!(mid.enabled);
        assert_eq!(mid.remaining_seconds, 60);

        // 60 分钟整自动关闭
        let expired = state.snapshot_at(1000 + DRY_RUN_TTL_SECS);
        assert!(!expired.enabled);
        assert_eq!(expired.remaining_seconds, 0);
        assert_eq!(expired.expires_at, None);
    }

    #[test]
    fn transport_enforces_dry_run_preserves_estop_and_restores_when_disabled() {
        let state = Arc::new(DryRunState::default());
        state.enable(Some(2000)).unwrap();

        // 开启期间打开的连接：流式任务开头补 G21 G94 G1 M5 S0 F1000、所有 S 变 S0、G0 限速、急停直通实时信道
        let fake_on = FakeGrbl::new([
            "$32=1".to_string(),
            "ok".to_string(),
            "ok".to_string(), // G21 G94 G1 M5 S0 F1000 preamble
            "ok".to_string(), // M4 S0
            "ok".to_string(), // G1 X10 F1000 S0
            "ok".to_string(), // G1 X20 S0 F1000
            "ok".to_string(), // M5
            "ok".to_string(), // G4 P0
            "<Idle|MPos:0,0,0>".to_string(),
        ]);
        let wrapped = DryRunTransport::wrap(fake_on.clone(), state.clone());
        let mut dev = DeviceConnection::new(wrapped);
        dev.handshake().unwrap();
        let mut worker = JobWorker::new(dev, crate::CancellationToken::default());
        worker
            .send_lines(vec![
                "M4 S800".into(),
                "G0 X10".into(),
                "G1 X20 S900 F4000".into(),
                "M5".into(),
            ])
            .unwrap();
        let sent = fake_on.lines();
        assert_eq!(sent[0], "$$");
        assert_eq!(sent[1], "G21 G94 G1 M5 S0 F1000");
        assert_eq!(sent[2], "M4 S0");
        assert_eq!(sent[3], "G1 X10 F1000 S0");
        assert_eq!(sent[4], "G1 X20 S0 F1000");
        assert_eq!(sent[5], "M5");

        // 传输层兜底：直接写入 G28 / G30 不会下发运动，替换为无运动注释并触发告警回调；裸坐标行补 G1；G92 不误补 G1
        let blocked_events = Arc::new(Mutex::new(Vec::<DryRunBlockedCommand>::new()));
        let sink = blocked_events.clone();
        let fake_guard = FakeGrbl::new(vec!["ok".to_string(); 5]);
        let wrapped_guard = DryRunTransport::wrap(fake_guard.clone(), state.clone())
            .with_block_handler(Arc::new(move |hit| {
                sink.lock().unwrap().push(hit.clone());
            }));
        wrapped_guard.write_line("G28 X0 Y0").unwrap();
        wrapped_guard.write_line("g30").unwrap();
        wrapped_guard.write_line("G92 X0 Y0").unwrap();
        wrapped_guard.write_line("X10 Y20").unwrap();
        wrapped_guard.write_line("X30 Y40").unwrap();
        assert_eq!(
            fake_guard.lines(),
            vec![
                "; dry-run blocked G28",
                "; dry-run blocked G30",
                "G92 X0 Y0",
                "X10 Y20 G1 F1000 S0",
                "X30 Y40",
            ]
        );
        let hits = blocked_events.lock().unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].code, "G28");
        assert_eq!(hits[1].code, "G30");

        // 急停 0x18 仍直通实时通道
        let wrapped_estop = DryRunTransport::wrap(fake_on.clone(), state.clone());
        wrapped_estop
            .write_realtime(RealtimeCommand::EmergencyStop)
            .unwrap();
        assert!(fake_on.realtime().contains(&0x18));

        // 关闭开关后新连接恢复原样
        state.disable();
        let fake_off = FakeGrbl::new(["ok".to_string(), "ok".to_string()]);
        let wrapped_off = DryRunTransport::wrap(fake_off.clone(), state.clone());
        wrapped_off.write_line("M4 S800").unwrap();
        wrapped_off.write_line("G0 X10 F4000").unwrap();
        assert_eq!(fake_off.lines(), vec!["M4 S800", "G0 X10 F4000"]);
    }
}
