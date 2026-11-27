//! VA2 #4–#5 本机 AI 调试注入令牌管理与过程记录控制（位于语音宿主 `moss-voice-host`）。
//!
//! 职责：
//! - 开关 `debug_text_inject_enabled` 开启时在 `<runtime>/debug/voice-debug.token`（权限 0600）生成 32 字节随机令牌，
//!   并在 `<runtime>/debug/voice-debug.json` 写入无密钥元数据（`host: "127.0.0.1"`、`port`、`issued_at`、`expires_at`）；
//! - 令牌在关闭开关或开启满 24 小时后立即作废并删除文件；
//! - 在内置语音服务运行中热启停 `VoiceServer` 的 `127.0.0.1` 调试入口，绝不断开已连接小智。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use moss_voice::CenterError;
use moss_voice_server::debug_api::{
    DEBUG_TOKEN_TTL_SECS, DEFAULT_DEBUG_PORT, ProcessTraceControl, ProcessTraceToggleResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::*;

pub const DEBUG_DIR_REL: &str = "debug";
pub const DEBUG_TOKEN_REL: &str = "debug/voice-debug.token";
pub const DEBUG_META_REL: &str = "debug/voice-debug.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DebugMetaFile {
    host: String,
    port: Option<u16>,
    issued_at: u64,
    expires_at: u64,
    token_file: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct DebugInjectRuntime {
    pub(super) token: Option<String>,
    pub(super) issued_at: u64,
    pub(super) expires_at: u64,
    pub(super) port: Option<u16>,
}

/// 供 `moss-voice-server::debug_api` 开关过程记录归档的宿主实现。
pub(super) struct DesktopProcessTraceControl {
    pub(super) settings_file: PathBuf,
    pub(super) trace: Option<laser_runtime::ProcessTrace>,
}

impl ProcessTraceControl for DesktopProcessTraceControl {
    fn set_enabled(&self, enabled: bool) -> Result<ProcessTraceToggleResult, String> {
        let previous = laser_runtime::settings_store::update(&self.settings_file, |root| {
            let prev = root
                .get(laser_runtime::process_trace::SETTING_KEY)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            root.insert(
                laser_runtime::process_trace::SETTING_KEY.into(),
                Value::Bool(enabled),
            );
            Ok(prev)
        })?;
        let using_custom_dir = if let Some(trace) = self.trace.as_ref() {
            trace.invalidate();
            trace.directory_status().custom_dir.is_some()
        } else {
            false
        };
        Ok(ProcessTraceToggleResult {
            enabled,
            previous_enabled: previous,
            trace_dir_rel: laser_runtime::process_trace::TRACE_DIR.to_owned(),
            using_custom_dir,
        })
    }

    fn flush(&self) {
        if let Some(trace) = self.trace.as_ref() {
            trace.flush();
        }
    }
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn generate_random_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("生成调试令牌失败：{e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && is_link_or_reparse(&meta)
    {
        return Err("拒绝访问符号链接路径".into());
    }
    Ok(())
}

fn ensure_private_debug_dir(dir: &Path) -> Result<(), String> {
    reject_symlink(dir)?;
    if let Some(parent) = dir.parent() {
        reject_symlink(parent)?;
    }
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if is_link_or_reparse(&meta) {
                return Err("调试目录不能是符号链接".into());
            }
            if !meta.is_dir() {
                return Err("调试目录路径不是文件夹".into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(dir).map_err(|err| format!("创建调试目录失败：{err}"))?;
        }
        Err(e) => return Err(format!("检查调试目录失败：{e}")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("设置调试目录权限 0700 失败：{e}"))?;
    }
    Ok(())
}

fn unique_debug_tmp(path: &Path) -> Result<PathBuf, String> {
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(|e| format!("生成临时文件名失败：{e}"))?;
    let suffix = u64::from_le_bytes(rnd);
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("debug");
    Ok(path.with_file_name(format!(
        ".{file_name}.tmp.{}.{suffix:016x}",
        std::process::id()
    )))
}

fn write_token_file_0600(dir: &Path, token_path: &Path, token: &str) -> Result<(), String> {
    use std::io::Write;
    ensure_private_debug_dir(dir)?;
    reject_symlink(token_path)?;
    let tmp = unique_debug_tmp(token_path)?;
    let write_res: Result<(), String> = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&tmp)
            .map_err(|e| format!("创建调试令牌临时文件失败：{e}"))?;
        file.write_all(format!("{token}\n").as_bytes())
            .map_err(|e| format!("写入调试令牌失败：{e}"))?;
        file.sync_all()
            .map_err(|e| format!("落盘调试令牌失败：{e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("设置调试令牌文件权限失败：{e}"))?;
        }
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp, token_path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("替换调试令牌文件失败：{err}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(token_path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("确认调试令牌权限 0600 失败：{e}"))?;
    }
    Ok(())
}

fn write_meta_file(dir: &Path, meta_path: &Path, meta: &DebugMetaFile) -> Result<(), String> {
    use std::io::Write;
    ensure_private_debug_dir(dir)?;
    reject_symlink(meta_path)?;
    let body =
        serde_json::to_string_pretty(meta).map_err(|e| format!("序列化调试入口信息失败：{e}"))?;
    let tmp = unique_debug_tmp(meta_path)?;
    let write_res: Result<(), String> = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts
            .open(&tmp)
            .map_err(|e| format!("写入调试入口信息失败：{e}"))?;
        file.write_all(body.as_bytes())
            .map_err(|e| format!("写入调试入口信息失败：{e}"))?;
        file.sync_all()
            .map_err(|e| format!("落盘调试入口信息失败：{e}"))?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&tmp, meta_path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("替换调试入口信息失败：{err}"));
    }
    Ok(())
}

fn remove_debug_files(runtime_dir: &Path) {
    let _ = fs::remove_file(runtime_dir.join(DEBUG_TOKEN_REL));
    let _ = fs::remove_file(runtime_dir.join(DEBUG_META_REL));
}

impl BuiltinVoice {
    fn debug_dir(&self) -> PathBuf {
        self.host.runtime_root.path.join(DEBUG_DIR_REL)
    }

    pub(super) fn debug_token_path(&self) -> PathBuf {
        self.host.runtime_root.path.join(DEBUG_TOKEN_REL)
    }

    pub(super) fn debug_meta_path(&self) -> PathBuf {
        self.host.runtime_root.path.join(DEBUG_META_REL)
    }

    /// 供 `VoiceServer::enable_debug_api` 在令牌满 24 小时过期时回调清理文件与开关（全程持锁且按令牌匹配，防并发重开 TOCTOU）。
    fn make_on_expired_callback(&self, expected_token: String) -> Arc<dyn Fn() + Send + Sync> {
        let runtime_dir = self.host.runtime_root.path.clone();
        let settings_file = self.settings_file();
        let state = self.debug_inject.clone();
        Arc::new(move || {
            let Ok(mut guard) = state.lock() else {
                return;
            };
            if guard.token.as_deref() != Some(expected_token.as_str()) {
                return;
            }
            *guard = DebugInjectRuntime::default();
            remove_debug_files(&runtime_dir);
            let _ = moss_voice::settings::update_voice(&settings_file, |s| {
                s.builtin_local.debug_text_inject_enabled = false;
                Ok(())
            });
        })
    }

    /// 核对当前内存或磁盘上的调试令牌是否仍在 24 小时有效期内；过期则立即作废并清理。
    pub(super) fn sync_debug_inject_expiry(&self) -> Option<DebugInjectRuntime> {
        let now = unix_now_secs();
        let expired_token = {
            let mut guard = self
                .debug_inject
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(token) = guard.token.as_ref() {
                if guard.expires_at > now && !token.is_empty() {
                    return Some(guard.clone());
                }
                let expired = Some(token.clone());
                *guard = DebugInjectRuntime::default();
                expired
            } else {
                // 内存中为空时，仅在 settings 开关仍开时尝试从磁盘恢复尚未过期的令牌（重启 MOSS 场景）
                if !self.settings().builtin_local.debug_text_inject_enabled {
                    return None;
                }
                let token = fs::read_to_string(self.debug_token_path())
                    .ok()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| s.len() >= 32);
                let meta = fs::read_to_string(self.debug_meta_path())
                    .ok()
                    .and_then(|s| serde_json::from_str::<DebugMetaFile>(&s).ok());
                if let (Some(token), Some(meta)) = (token, meta)
                    && meta.expires_at > now
                {
                    guard.token = Some(token);
                    guard.issued_at = meta.issued_at;
                    guard.expires_at = meta.expires_at;
                    guard.port = meta.port;
                    return Some(guard.clone());
                }
                None
            }
        };
        self.revoke_debug_inject_after_expiry(expired_token.as_deref());
        None
    }

    /// 固定锁序 `server -> debug_inject`：仅在锁内确认尚未被并发重开写入新令牌时才关入口、删文件并回写设置。
    fn revoke_debug_inject_after_expiry(&self, expected_expired_token: Option<&str>) {
        let server_guard = self.server.lock().ok();
        let mut slot = self
            .debug_inject
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let still_stale = match (slot.token.as_deref(), expected_expired_token) {
            (None, _) => true,
            (Some(cur), Some(exp)) => cur == exp,
            (Some(_), None) => false,
        };
        if !still_stale {
            return;
        }
        *slot = DebugInjectRuntime::default();
        if let Some(server) = server_guard.as_ref().and_then(|g| g.as_ref()) {
            server.disable_debug_api();
        }
        remove_debug_files(&self.host.runtime_root.path);
        let _ = moss_voice::settings::update_voice(&self.settings_file(), |s| {
            s.builtin_local.debug_text_inject_enabled = false;
            Ok(())
        });
    }

    /// 热切换 `debug_text_inject_enabled`：开启生成 0600 随机令牌并启动 127.0.0.1 调试入口；
    /// 关闭立即作废令牌、删文件并停掉调试入口。整个过程不断开已连接小智，并按 `server -> debug_inject` 全程串行化。
    pub(super) fn set_debug_text_inject_enabled(&self, enabled: bool) -> Result<(), CenterError> {
        let server_guard = self
            .server
            .lock()
            .map_err(|_| internal("内置语音服务状态锁失败"))?;
        let mut slot = self
            .debug_inject
            .lock()
            .map_err(|_| internal("本机调试状态锁失败"))?;
        if !enabled {
            if let Some(server) = server_guard.as_ref() {
                server.disable_debug_api();
            }
            *slot = DebugInjectRuntime::default();
            remove_debug_files(&self.host.runtime_root.path);
            moss_voice::settings::update_voice(&self.settings_file(), |s| {
                s.builtin_local.debug_text_inject_enabled = false;
                Ok(())
            })
            .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
            return Ok(());
        }

        let issued_at = unix_now_secs();
        let expires_at = issued_at.saturating_add(DEBUG_TOKEN_TTL_SECS);
        let token = generate_random_token().map_err(internal)?;
        write_token_file_0600(&self.debug_dir(), &self.debug_token_path(), &token)
            .map_err(internal)?;

        let rollback = |server_opt: Option<&VoiceServer>, slot: &mut DebugInjectRuntime| {
            if let Some(server) = server_opt {
                server.disable_debug_api();
            }
            *slot = DebugInjectRuntime::default();
            remove_debug_files(&self.host.runtime_root.path);
        };

        let preferred_port = if cfg!(test) { 0 } else { DEFAULT_DEBUG_PORT };
        let bound_port = if let Some(server) = server_guard.as_ref() {
            let port = match server.enable_debug_api(
                preferred_port,
                token.clone(),
                issued_at,
                expires_at,
                Some(self.make_on_expired_callback(token.clone())),
            ) {
                Ok(port) => port,
                Err(msg) => {
                    rollback(None, &mut slot);
                    return Err(CenterError::new("debug_api_start_failed", 500, msg));
                }
            };
            Some(port)
        } else {
            None
        };

        let meta = DebugMetaFile {
            host: "127.0.0.1".into(),
            port: bound_port,
            issued_at,
            expires_at,
            token_file: DEBUG_TOKEN_REL.into(),
        };
        if let Err(err) = write_meta_file(&self.debug_dir(), &self.debug_meta_path(), &meta) {
            rollback(server_guard.as_ref(), &mut slot);
            return Err(internal(err));
        }
        slot.token = Some(token);
        slot.issued_at = issued_at;
        slot.expires_at = expires_at;
        slot.port = bound_port;
        if let Err(msg) = moss_voice::settings::update_voice(&self.settings_file(), |s| {
            s.builtin_local.debug_text_inject_enabled = true;
            Ok(())
        }) {
            rollback(server_guard.as_ref(), &mut slot);
            return Err(CenterError::new("voice_settings_write_failed", 500, msg));
        }
        Ok(())
    }

    /// `VoiceServer` 启动后若调试开关仍处于有效期内，自动拉起 `127.0.0.1` 调试入口并刷新端口元数据。
    pub(super) fn attach_debug_api_if_enabled(&self, server: &VoiceServer) {
        let Some(active) = self.sync_debug_inject_expiry() else {
            return;
        };
        let Some(token) = active.token else {
            return;
        };
        let preferred_port = if cfg!(test) { 0 } else { DEFAULT_DEBUG_PORT };
        match server.enable_debug_api(
            preferred_port,
            token.clone(),
            active.issued_at,
            active.expires_at,
            Some(self.make_on_expired_callback(token)),
        ) {
            Ok(port) => {
                if let Ok(mut slot) = self.debug_inject.lock() {
                    slot.port = Some(port);
                }
                let meta = DebugMetaFile {
                    host: "127.0.0.1".into(),
                    port: Some(port),
                    issued_at: active.issued_at,
                    expires_at: active.expires_at,
                    token_file: DEBUG_TOKEN_REL.into(),
                };
                let _ = write_meta_file(&self.debug_dir(), &self.debug_meta_path(), &meta);
            }
            Err(error) => {
                self.diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    EventRecord::warn(
                        DiagCategory::Network,
                        format!("本机 AI 调试注入入口未能随服务启动：{error}"),
                    ),
                );
            }
        }
    }

    /// 桌面端状态视图（绝不包含令牌原文，仅提供相对路径与端口/过期时间）。
    pub(super) fn debug_inject_status(&self) -> Value {
        let active = self.sync_debug_inject_expiry();
        let (port, bench_windows) = self
            .server
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| (s.debug_api_port(), s.bench_windows())))
            .unwrap_or((None, Vec::new()));
        let enabled = active.is_some();
        json!({
            "enabled": enabled,
            "active": enabled && port.is_some(),
            "host": "127.0.0.1",
            "port": port,
            "issued_at": active.as_ref().map(|a| a.issued_at),
            "expires_at": active.as_ref().map(|a| a.expires_at),
            "ttl_hours": DEBUG_TOKEN_TTL_SECS / 3600,
            "token_file_rel": DEBUG_TOKEN_REL,
            "meta_file_rel": DEBUG_META_REL,
            "bench_windows": bench_windows,
        })
    }

    /// 供桌面端 UI（VA5）走与本机脚本完全相同的模拟注入逻辑，要求调试开关开启且未过期。
    pub(super) fn inject_text_for_device(
        &self,
        device_id: &str,
        text: &str,
    ) -> Result<Value, CenterError> {
        if self.sync_debug_inject_expiry().is_none() {
            return Err(CenterError::new(
                "debug_inject_disabled",
                403,
                "请先在语音中心开启“允许本机 AI 调试注入”开关（开启后 24 小时有效）",
            ));
        }
        let server_guard = self
            .server
            .lock()
            .map_err(|_| internal("内置语音服务状态锁失败"))?;
        let server = server_guard
            .as_ref()
            .ok_or_else(|| CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行"))?;
        let ack = server
            .inject_text(device_id, text, true)
            .map_err(|err| CenterError::new(&err.code, err.http_status, err.message))?;
        serde_json::to_value(ack).map_err(|e| internal(format!("序列化注入结果失败：{e}")))
    }

    /// 测试辅助：模拟 24 小时到期，验证自动作废逻辑。
    #[cfg(test)]
    pub(crate) fn expire_debug_inject_for_test(&self) {
        if let Ok(mut slot) = self.debug_inject.lock() {
            slot.expires_at = 1;
        }
        if let Ok(meta) = serde_json::to_string_pretty(&DebugMetaFile {
            host: "127.0.0.1".into(),
            port: None,
            issued_at: 1,
            expires_at: 1,
            token_file: DEBUG_TOKEN_REL.into(),
        }) {
            let _ = fs::write(self.debug_meta_path(), meta);
        }
    }
}
