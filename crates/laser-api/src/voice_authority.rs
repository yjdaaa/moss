//! Live voice-session permissions. Revocation cannot be undone by re-allowing an old connection.
use laser_contracts::CallerContext;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct VoiceSession {
    device: String,
    client: String,
    allowed: Arc<AtomicBool>,
}
pub(super) type VoiceSessions = Mutex<HashMap<String, VoiceSession>>;

impl crate::ApiService {
    /// Called only by the host after verifying the device token; database and session admission share lock ordering.
    pub fn register_voice_session(
        &self,
        device: &str,
        client: &str,
        session: &str,
    ) -> Result<Arc<AtomicBool>, String> {
        if [device, client, session]
            .iter()
            .any(|s| s.trim().is_empty() || s.chars().count() > 128)
        {
            return Err("语音设备身份无效".into());
        }
        let db = self
            .runtime_db
            .as_ref()
            .ok_or("语音设备数据库不可用")?
            .lock()
            .map_err(|_| "语音设备数据库不可用")?;
        let record = db
            .voice_device(device)
            .map_err(|_| "无法检查设备授权")?
            .ok_or("设备未登记")?;
        if !record.is_allowed() || record.client_id != client {
            return Err("设备未允许或已撤销".into());
        }
        let mut sessions = self.voice_sessions.lock().map_err(|_| "语音会话不可用")?;
        if sessions.contains_key(session) {
            return Err("语音会话编号重复".into());
        }
        let allowed = Arc::new(AtomicBool::new(true));
        sessions.insert(
            session.into(),
            VoiceSession {
                device: device.into(),
                client: client.into(),
                allowed: allowed.clone(),
            },
        );
        Ok(allowed)
    }

    pub fn unregister_voice_session(&self, session: &str) {
        if let Ok(mut sessions) = self.voice_sessions.lock() {
            if let Some(lease) = sessions.remove(session) {
                lease.allowed.store(false, Ordering::Release);
            }
        }
    }

    pub fn revoke_voice_sessions(&self, device: &str) {
        if let Ok(mut sessions) = self.voice_sessions.lock() {
            sessions.retain(|_, lease| {
                if lease.device == device {
                    lease.allowed.store(false, Ordering::Release);
                    false
                } else {
                    true
                }
            });
        }
    }

    /// Checked again inside the blocking tool worker, not only when its future is queued.
    pub fn voice_session_allowed(&self, device: &str, session: &str) -> bool {
        let Some(db) = self.runtime_db.as_ref() else {
            return false;
        };
        let Ok(db) = db.lock() else { return false };
        let Ok(sessions) = self.voice_sessions.lock() else {
            return false;
        };
        let Some(lease) = sessions.get(session) else {
            return false;
        };
        if lease.device != device || !lease.allowed.load(Ordering::Acquire) {
            return false;
        }
        let valid = db
            .voice_device(device)
            .ok()
            .flatten()
            .is_some_and(|r| r.is_allowed() && r.client_id == lease.client);
        if !valid {
            lease.allowed.store(false, Ordering::Release);
        }
        valid
    }

    pub(crate) fn voice_permit(
        &self,
        ctx: &CallerContext,
    ) -> Result<Option<Arc<AtomicBool>>, (u16, serde_json::Value)> {
        let CallerContext::Voice {
            device_id,
            session_id,
        } = ctx
        else {
            return Ok(None);
        };
        let denied = || {
            (
                403,
                crate::failure(
                    "语音设备授权已失效，请重新连接",
                    Some(serde_json::json!({"error_code":"voice_session_revoked"})),
                ),
            )
        };
        let sessions = self.voice_sessions.lock().map_err(|_| denied())?;
        let lease = sessions
            .get(session_id)
            .filter(|lease| lease.device == *device_id && lease.allowed.load(Ordering::Acquire))
            .ok_or_else(denied)?;
        Ok(Some(lease.allowed.clone()))
    }

    pub(crate) fn voice_caller_allowed(&self, ctx: &CallerContext) -> bool {
        match ctx {
            CallerContext::Voice {
                device_id,
                session_id,
            } => self.voice_session_allowed(device_id, session_id),
            _ => true,
        }
    }

    /// Revoke live sessions and stored, unconsumed approvals. Already executing hardware work is not stopped here.
    pub fn set_voice_device_status(
        &self,
        device: &str,
        status: &str,
        now: f64,
    ) -> Result<bool, String> {
        if ![
            laser_runtime::VOICE_DEVICE_ALLOWED,
            laser_runtime::VOICE_DEVICE_REVOKED,
        ]
        .contains(&status)
        {
            return Err("设备状态无效".into());
        }
        let db = self
            .runtime_db
            .as_ref()
            .ok_or("语音设备数据库不可用")?
            .lock()
            .map_err(|_| "语音设备数据库不可用")?;
        if status == laser_runtime::VOICE_DEVICE_REVOKED {
            // Fail closed for live connections even if persistence subsequently fails.
            let mut sessions = self.voice_sessions.lock().map_err(|_| "语音会话不可用")?;
            sessions.retain(|_, lease| {
                if lease.device == device {
                    lease.allowed.store(false, Ordering::Release);
                    false
                } else {
                    true
                }
            });
        }
        db.set_voice_device_status(device, status, now)
            .map_err(|_| "保存设备授权失败".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revoked_sessions_cannot_return_after_allow_and_other_devices_survive() {
        let db = Arc::new(Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap()));
        let service = crate::ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_runtime_db(db.clone());
        for d in ["device-a", "device-b"] {
            db.lock()
                .unwrap()
                .voice_device_seen(d, "client", "", "", "", 1.0)
                .unwrap();
            service
                .set_voice_device_status(d, laser_runtime::VOICE_DEVICE_ALLOWED, 2.0)
                .unwrap();
        }
        let first = service
            .register_voice_session("device-a", "client", "a1")
            .unwrap();
        let second = service
            .register_voice_session("device-a", "client", "a2")
            .unwrap();
        service
            .register_voice_session("device-b", "client", "b1")
            .unwrap();
        assert!(service.voice_session_allowed("device-a", "a1"));
        assert!(!service.voice_session_allowed("device-b", "a1"));
        service
            .set_voice_device_status("device-a", laser_runtime::VOICE_DEVICE_REVOKED, 3.0)
            .unwrap();
        assert!(!first.load(Ordering::Acquire) && !second.load(Ordering::Acquire));
        assert!(!service.voice_session_allowed("device-a", "a1"));
        assert!(service.voice_session_allowed("device-b", "b1"));
        assert!(
            service
                .register_voice_session("device-a", "client", "a3")
                .is_err()
        );
        service
            .set_voice_device_status("device-a", laser_runtime::VOICE_DEVICE_ALLOWED, 4.0)
            .unwrap();
        assert!(!service.voice_session_allowed("device-a", "a1"));
        service
            .register_voice_session("device-a", "client", "a3")
            .unwrap();
        assert!(service.voice_session_allowed("device-a", "a3"));
        service.unregister_voice_session("a3");
        assert!(!service.voice_session_allowed("device-a", "a3"));
    }
}
