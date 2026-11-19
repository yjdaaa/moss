//! 家居宿主与家居设置、令牌、授权同意、审批与备份命令。

use super::*;

// NV15 opt-in *read-only* model tool. Never expose the token or POST.
// A saved grant binds the exact allowlist revision and token revision,
// so editing either disables the tool until the desktop user re-consents.
const HOME_VOICE_GRANT: &str = "voice.homeassistant.voice_read_consent";
pub(super) struct DesktopHomeHost {
    pub(super) host: Arc<laser_host::HostApplication>,
    pub(super) control: Arc<moss_home::HomeControlCoordinator>,
}
impl DesktopHomeHost {
    fn current(&self) -> Option<(moss_voice::settings::BuiltinHomeAssistant, u64, u64, String)> {
        let (settings, policy) = moss_voice::settings::read_home_offline(
            &self.host.runtime_root.path.join("settings.json"),
        )
        .ok()?;
        settings.validate_offline().ok()?;
        if policy == 0
            || settings.base_url.is_empty()
            || !settings.entities.iter().any(|e| e.enabled)
        {
            return None;
        }
        let db = self.host.db.lock().ok()?;
        let (has_token, revision) = db.home_token_status().ok()?;
        if !has_token || revision == 0 {
            return None;
        }
        let grant = db.get_secret(HOME_VOICE_GRANT).ok()??;
        if grant != format!("{policy}:{revision}") {
            return None;
        }
        Some((settings, policy, revision, grant))
    }
}
impl HomeHost for DesktopHomeHost {
    fn availability(&self) -> HomeAvailability {
        HomeAvailability {
            read: self.current().is_some(),
            control: self.control.enabled(),
        }
    }
    fn permitted(&self, context: &ToolContext) -> bool {
        self.host
            .api_service
            .voice_session_allowed(&context.device_id, &context.session_id)
    }
    fn invoke(&self, context: &ToolContext, name: &str, arguments: &Value) -> Value {
        let deny =
            |code: &str| serde_json::json!({"ok":false,"error":{"code":code,"retryable":false}});
        if !self.permitted(context) {
            return deny("home_disabled");
        }
        if name != "hass_get_state" {
            return self
                .control
                .prepare(&context.device_id, &context.session_id, name, arguments)
                .unwrap_or_else(&deny);
        }
        let Some((config, policy, revision, _)) = self.current() else {
            return deny("home_disabled");
        };
        let target = arguments["entity"].as_str().unwrap_or("");
        if !config
            .entities
            .iter()
            .any(|e| e.enabled && e.entity_id == target)
        {
            return deny("home_unknown_entity");
        }
        let token = {
            let Ok(db) = self.host.db.lock() else {
                return deny("home_unavailable");
            };
            match db.home_token_for_connection() {
                Ok(Some(token)) => token,
                _ => return deny("home_unavailable"),
            }
        };
        let discovered =
            moss_home::home_readonly::discover_guarded(&config, &token, policy, revision, || {
                self.permitted(context)
                    && self.current().is_some_and(|(latest, p, r, _)| {
                        latest == config && p == policy && r == revision
                    })
            });
        if !self.permitted(context)
            || self
                .current()
                .is_none_or(|(new, p, r, _)| new != config || p != policy || r != revision)
        {
            return deny("home_stale_permission");
        }
        match discovered {
            Ok(output) => match output.entities.iter().find(|e| e.entity_id == target) {
                Some(entity) => serde_json::json!({"ok":true,"entity_id":entity.entity_id,
                        "state":entity.state,"last_updated":entity.last_updated,
                        "attributes":entity.attributes,"actions_available":entity.actions_available}),
                None => deny("home_unknown_entity"),
            },
            Err(moss_home::home_readonly::DiscoveryError::Unauthorized) => {
                deny("home_unauthorized")
            }
            Err(_) => deny("home_unavailable"),
        }
    }
}

impl BuiltinVoice {
    /// NV15 inert Home Assistant configuration with optimistic revision.
    pub fn home_status(&self) -> Value {
        let credentials = self
            .host
            .db
            .lock()
            .ok()
            .and_then(|db| db.home_token_status().ok());
        let (key_configured, credential_revision) = match credentials {
            Some(status) => status,
            None => {
                // A copied SQLite file may hold an un-decryptable DPAPI or Keychain
                // envelope from another device or OS account. Permit ONLY private
                // window import/recovery with revision CAS, not use of the
                // foreign bearer nor any read/control authority.
                let recover = self
                    .host
                    .db
                    .lock()
                    .ok()
                    .and_then(|db| db.home_token_recovery_revision().ok());
                let profile = moss_voice::settings::read_home_offline(&self.settings_file()).ok();
                if let (Some(credential_revision), Some((settings, revision))) = (recover, profile)
                {
                    return serde_json::json!({
                        "settings":settings,"revision":revision,
                        "credential_revision":credential_revision,
                        "key_configured":false,"ready":false,"can_control":false,
                        "voice_read_enabled":false,"voice_control_enabled":false,
                        "recovery_only":true,
                        "reason":"旧凭据可能来自其他设备、其他系统账户或已损坏；只能导入安全配置并撤销旧密文，随后重新设置令牌。旧控制和只读授权不可用"
                    });
                }
                return serde_json::json!({
                    "settings":null,"revision":null,"credential_revision":null,
                    "key_configured":false,"ready":false,"can_control":false,
                    "reason":"Home Assistant 凭据或版本损坏，安全恢复不可用"
                });
            }
        };
        match moss_voice::settings::read_home_offline(&self.settings_file()) {
            Ok((settings, revision)) => serde_json::json!({
                "settings":settings, "revision":revision,"credential_revision":credential_revision,
                "voice_read_enabled":DesktopHomeHost{host:self.host.clone(),control:self.home_control.clone()}.current().is_some(),
                "voice_control_enabled":self.home_control.enabled(),
                "ready":self.home_control.enabled(), "can_control":self.home_control.enabled(), "key_configured":key_configured,
                "reason":"控制默认关闭；开启后语音只能创建家居审批，每次均须在电脑独立批准"
            }),
            Err(_) => serde_json::json!({
                "settings":null, "revision":null,"credential_revision":null,
                "ready":false, "can_control":false, "key_configured":false,
                "reason":"Home Assistant 配置损坏或无法读取，禁止覆盖，请先修复设置文件"
            }),
        }
    }
    pub fn home_save(&self, input: &Value) -> Result<Value, CenterError> {
        use moss_voice::settings::{BuiltinHomeAssistant, HomeOfflineError};
        let invalid = |message: &str| CenterError::new("home_invalid", 400, message);
        let object = input
            .as_object()
            .ok_or_else(|| invalid("Home Assistant 配置必须是对象"))?;
        if object.len() != 2
            || !object.contains_key("settings")
            || !object.contains_key("expected_revision")
        {
            return Err(invalid(
                "只接受 settings 和 expected_revision，拒绝令牌、密钥及控制字段",
            ));
        }
        let revision = object["expected_revision"]
            .as_u64()
            .filter(|value| *value <= 9_007_199_254_740_991)
            .ok_or_else(|| invalid("缺少有效的 Home Assistant 配置版本"))?;
        let next: BuiltinHomeAssistant = serde_json::from_value(object["settings"].clone())
            .map_err(|_| invalid("实体白名单格式无效或有禁止字段"))?;
        self.home_control
            .fence_sensitive_change(|| {
                moss_voice::settings::save_home_offline(&self.settings_file(), &next, revision)
            })
            .map_err(|_| internal("无法安全撤销在途家居授权"))?
            .map_err(|error| match error {
                HomeOfflineError::Invalid(message) => invalid(message),
                HomeOfflineError::Conflict => CenterError::new(
                    "home_conflict",
                    409,
                    "白名单已在其他窗口修改，请重新加载后再保存",
                ),
                HomeOfflineError::Storage(_) => CenterError::new(
                    "home_save_failed",
                    500,
                    "Home Assistant 配置保存失败；原设置未被覆盖",
                ),
            })?;
        Ok(self.home_status())
    }

    pub fn home_voice_consent(&self, input: &Value) -> Result<Value, CenterError> {
        let invalid = || CenterError::new("home_invalid", 400, "语音只读授权参数无效");
        let obj = input.as_object().ok_or_else(invalid)?;
        if obj.len() != 3
            || !obj.contains_key("enabled")
            || !obj.contains_key("policy_revision")
            || !obj.contains_key("credential_revision")
        {
            return Err(invalid());
        }
        let enabled = obj["enabled"].as_bool().ok_or_else(invalid)?;
        let expected_policy = obj["policy_revision"].as_u64().ok_or_else(invalid)?;
        let expected_credential = obj["credential_revision"].as_u64().ok_or_else(invalid)?;
        // The configuration read is protected by revision. A later change
        // automatically invalidates this exact grant.
        let (config, policy) = moss_voice::settings::read_home_offline(&self.settings_file())
            .map_err(|_| invalid())?;
        config.validate_offline().map_err(|_| invalid())?;
        if policy != expected_policy {
            return Err(CenterError::new(
                "home_conflict",
                409,
                "家居白名单已改变，拒绝授权",
            ));
        }
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        let (present, credential) = db.home_token_status().map_err(|_| invalid())?;
        if credential != expected_credential
            || (enabled
                && (!present
                    || config.base_url.is_empty()
                    || !config.entities.iter().any(|e| e.enabled)))
        {
            return Err(CenterError::new(
                "home_conflict",
                409,
                "令牌或实体已更改，拒绝授权",
            ));
        }
        if enabled {
            db.set_secret(HOME_VOICE_GRANT, &format!("{policy}:{credential}"), now())
                .map_err(|_| internal("语音只读授权未保存"))?;
        } else {
            db.delete_secret(HOME_VOICE_GRANT)
                .map_err(|_| internal("撤销语音只读授权失败"))?;
        }
        drop(db);
        Ok(self.home_status())
    }
    pub fn home_control_consent(&self, input: &Value) -> Result<Value, CenterError> {
        let invalid = || CenterError::new("home_invalid", 400, "家居控制授权参数无效");
        let map = input.as_object().ok_or_else(invalid)?;
        if map.len() != 4
            || map.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "enabled" | "confirmed" | "policy_revision" | "credential_revision"
                )
            })
        {
            return Err(invalid());
        }
        self.home_control
            .consent(
                map["enabled"].as_bool().ok_or_else(invalid)?,
                map["policy_revision"].as_u64().ok_or_else(invalid)?,
                map["credential_revision"].as_u64().ok_or_else(invalid)?,
                map["confirmed"].as_bool().ok_or_else(invalid)?,
            )
            .map_err(|code| {
                CenterError::new(code, 409, "家居控制授权未生效，请核对白名单、凭据与确认")
            })?;
        Ok(self.home_status())
    }
    pub fn home_approval_approve(&self, input: &Value) -> Result<Value, CenterError> {
        let invalid = || CenterError::new("home_approval_invalid", 400, "家居批准参数无效");
        let map = input.as_object().ok_or_else(invalid)?;
        if map.len() != 3
            || map
                .keys()
                .any(|key| !matches!(key.as_str(), "approval_id" | "digest" | "confirmed"))
        {
            return Err(invalid());
        }
        let id = map["approval_id"]
            .as_str()
            .filter(|s| s.len() <= 128)
            .ok_or_else(invalid)?;
        let digest = map["digest"]
            .as_str()
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(invalid)?;
        self.home_control
            .approve(id, digest, map["confirmed"].as_bool().ok_or_else(invalid)?)
            .map_err(|code| {
                CenterError::new(
                    code,
                    409,
                    "家居批准或执行未确认；请查看持久结果，不自动重发",
                )
            })
    }

    /// Main-window private-only redacted profile export. A portable
    /// profile NEVER contains the DPAPI ciphertext, bearer key, grants,
    /// approvals or the rest of settings.json.
    pub fn home_backup_export(&self) -> Result<Value, CenterError> {
        let profile =
            moss_voice::settings::export_home_portable(&self.settings_file()).map_err(|_| {
                CenterError::new(
                    "home_backup_invalid",
                    409,
                    "白名单损坏或过大，无法生成安全备份",
                )
            })?;
        Ok(serde_json::json!({
            "profile":profile,
            "credentials_included":false,
            "reenter_token_required":true,
        }))
    }

    /// Import is explicitly destructive to old *authority*, but not to
    /// user files: revoke token/grants FIRST, then CAS-save an inert
    /// imported profile. If settings write fails, privileges remain revoked.
    pub fn home_backup_restore(&self, input: &Value) -> Result<Value, CenterError> {
        use moss_voice::settings::{self, HomeOfflineError};
        let invalid = || CenterError::new("home_backup_invalid", 400, "备份格式、版本或确认无效");
        let map = input.as_object().ok_or_else(invalid)?;
        if map.len() != 4
            || !map.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "profile" | "expected_revision" | "expected_credential_revision" | "confirmed"
                )
            })
        {
            return Err(invalid());
        }
        if map["confirmed"].as_bool() != Some(true) {
            return Err(invalid());
        }
        let payload = map["profile"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 65536)
            .ok_or_else(invalid)?;
        let imported =
            settings::validate_home_portable(payload.as_bytes()).map_err(|_| invalid())?;
        let revision = map["expected_revision"]
            .as_u64()
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or_else(invalid)?;
        let credential = map["expected_credential_revision"]
            .as_u64()
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or_else(invalid)?;
        let file = self.settings_file();
        self.home_control
            .fence_sensitive_change(|| -> Result<(), CenterError> {
                let (_, current) = settings::read_home_offline(&file).map_err(|_| {
                    CenterError::new("home_conflict", 409, "白名单状态已改变或损坏，未恢复")
                })?;
                if current != revision {
                    return Err(CenterError::new(
                        "home_conflict",
                        409,
                        "其他窗口已修改白名单，请重新加载",
                    ));
                }
                // Import must also recover a database copied from another
                // Windows user: it can revoke a foreign DPAPI blob without
                // decrypting it. SQLite compares the expected revision under
                // BEGIN IMMEDIATE; it never imports old credentials.
                let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
                let changed = db
                    .update_home_token_if_revision(credential, None, now())
                    .map_err(|_| {
                        CenterError::new("home_unavailable", 503, "撤销旧令牌失败，备份未导入")
                    })?;
                if changed.is_none() {
                    return Err(CenterError::new(
                        "home_conflict",
                        409,
                        "令牌版本并发冲突，备份未导入",
                    ));
                }
                // DB mutation and config file write are not one transaction.
                // If writing the inactive template fails, the old token and
                // grants remain REVOKED (fail closed), never rolled back.
                settings::save_home_offline(&file, &imported, revision).map_err(
                    |error| match error {
                        HomeOfflineError::Invalid(_) => invalid(),
                        HomeOfflineError::Conflict => CenterError::new(
                            "home_conflict",
                            409,
                            "令牌已撤销，但白名单版本变更；请重新加载",
                        ),
                        HomeOfflineError::Storage(_) => CenterError::new(
                            "home_save_failed",
                            500,
                            "令牌已撤销，但白名单保存失败；请重新加载",
                        ),
                    },
                )?;
                Ok(())
            })
            .map_err(|_| internal("无法安全撤销在途家居授权"))??;
        Ok(self.home_status())
    }

    /// NV15 desktop-only token mutation: stored only in app_secrets,
    /// guarded by monotonic credential revision. No network call is made.
    pub fn home_token_save(&self, input: &Value) -> Result<Value, CenterError> {
        let invalid = |message: &str| CenterError::new("home_token_invalid", 400, message);
        let map = input
            .as_object()
            .ok_or_else(|| invalid("凭据变更必须是对象"))?;
        let action = map
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("必须选择更新或清除"))?;
        let expected = map
            .get("expected_revision")
            .and_then(Value::as_u64)
            .filter(|rev| *rev <= 9_007_199_254_740_991)
            .ok_or_else(|| invalid("缺少有效的凭据版本"))?;
        if map
            .keys()
            .any(|key| !matches!(key.as_str(), "action" | "expected_revision" | "value"))
        {
            return Err(invalid("凭据操作包含禁止字段"));
        }
        let replacement = match action {
            "set" => Some(
                map.get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("请输入长期访问令牌"))?,
            ),
            "clear" => {
                if map.contains_key("value") {
                    return Err(invalid("清除操作不能携带令牌"));
                }
                None
            }
            _ => return Err(invalid("仅支持更新或清除令牌")),
        };
        let changed = self
            .home_control
            .fence_sensitive_change(|| {
                let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
                db.update_home_token_if_revision(expected, replacement, now())
                    .map_err(|_| {
                        CenterError::new(
                            "home_token_invalid",
                            400,
                            "访问令牌未保存；请检查令牌格式和数据库",
                        )
                    })
            })
            .map_err(|_| internal("无法安全撤销在途家居授权"))??;
        if changed.is_none() {
            return Err(CenterError::new(
                "home_conflict",
                409,
                "凭据已更新，请重新加载最新状态",
            ));
        }
        Ok(self.home_status())
    }

    /// Explicit trusted main-window read-only discovery; never sends a
    /// Home Assistant service POST and never exposes the bearer token.
    pub fn home_discover(&self) -> Result<Value, CenterError> {
        use moss_home::home_readonly::{self, DiscoveryError};
        let (settings, policy_revision) =
            moss_voice::settings::read_home_offline(&self.settings_file())
                .map_err(|_| CenterError::new("home_unavailable", 409, "无法安全读取家居白名单"))?;
        let (token, credential_revision) = {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            let (has_token, revision) = db
                .home_token_status()
                .map_err(|_| CenterError::new("home_unavailable", 409, "访问令牌配置损坏"))?;
            if !has_token {
                return Err(CenterError::new(
                    "home_unauthorized",
                    401,
                    "请先设置访问令牌",
                ));
            }
            let token = db
                .home_token_for_connection()
                .map_err(|_| CenterError::new("home_unavailable", 409, "无法读取访问令牌"))?
                .ok_or_else(|| CenterError::new("home_unauthorized", 401, "访问令牌不存在"))?;
            (token, revision)
        };
        let output=home_readonly::discover_guarded(&settings,&token,policy_revision,credential_revision,||{
                let current_policy=moss_voice::settings::read_home_offline(&self.settings_file()).ok();
                let current_credential=self.host.db.lock().ok()
                    .and_then(|db|db.home_token_status().ok());
                matches!(current_policy,Some((ref latest,rev)) if *latest==settings && rev==policy_revision)
                    && matches!(current_credential,Some((true,rev)) if rev==credential_revision)
            })
                .map_err(|error|match error {
                    DiscoveryError::Unauthorized=>CenterError::new("home_unauthorized",401,"Home Assistant 拒绝访问，请检查令牌"),
                    DiscoveryError::Unavailable=>CenterError::new("home_unavailable",503,"Home Assistant 无法连接或连接超时"),
                    DiscoveryError::MissingCredential=>CenterError::new("home_unauthorized",401,"Home Assistant 令牌格式无效"),
                    DiscoveryError::InvalidSettings=>CenterError::new("home_invalid",400,"地址、白名单或配置版本无效"),
                    DiscoveryError::InvalidResponse=>CenterError::new("home_bad_response",502,"Home Assistant 返回无效或过大的响应"),
                })?;
        let (_, latest_policy) = moss_voice::settings::read_home_offline(&self.settings_file())
            .map_err(|_| CenterError::new("home_conflict", 409, "家居配置读取失败，请重新加载"))?;
        let latest_token = self
            .host
            .db
            .lock()
            .map_err(|_| internal("数据库锁失败"))?
            .home_token_status()
            .map_err(|_| CenterError::new("home_conflict", 409, "令牌状态读取失败"))?;
        if latest_policy != policy_revision
            || latest_token.1 != credential_revision
            || !latest_token.0
        {
            return Err(CenterError::new(
                "home_conflict",
                409,
                "查询期间配置已更改，结果已丢弃",
            ));
        }
        serde_json::to_value(output).map_err(|_| internal("Home Assistant 查询结果序列化失败"))
    }

    /// NV15 private HomeControl inbox. Only sanitized previews; never return
    /// saved connection fingerprint, the request body, secrets or raw URLs.
    pub fn home_approvals(&self) -> Result<Value, CenterError> {
        use laser_contracts::{ApprovalKind, ApprovalStatus};
        let approvals = laser_api::approval::ApprovalService::new(self.host.db.clone());
        let rows = approvals.list(None, 250).map_err(|_| {
            CenterError::new("home_approval_unavailable", 503, "无法读取家居审批记录")
        })?;
        let (policy, credential) = {
            let (_, p) =
                moss_voice::settings::read_home_offline(&self.settings_file()).map_err(|_| {
                    CenterError::new("home_unavailable", 409, "Home Assistant 白名单不可读取")
                })?;
            let (_, c) = self
                .host
                .db
                .lock()
                .map_err(|_| internal("数据库锁失败"))?
                .home_token_status()
                .map_err(|_| CenterError::new("home_unavailable", 409, "HA 凭据状态不可读取"))?;
            (p, c)
        };
        let mut items = Vec::new();
        for row in rows
            .iter()
            .filter(|r| r.kind == ApprovalKind::HomeControl)
            .take(60)
        {
            let requested = row.request.as_ref();
            let path = requested
                .and_then(|req| req.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let service = path
                .strip_prefix("/api/services/")
                .and_then(|tail| tail.split_once('/'))
                .filter(|(domain, action)| {
                    matches!(
                        *domain,
                        "light"
                            | "switch"
                            | "climate"
                            | "lock"
                            | "alarm_control_panel"
                            | "cover"
                            | "media_player"
                    ) && matches!(
                        *action,
                        "turn_on"
                            | "turn_off"
                            | "set_temperature"
                            | "lock"
                            | "unlock"
                            | "alarm_arm_home"
                            | "alarm_arm_away"
                            | "alarm_disarm"
                            | "open_cover"
                            | "close_cover"
                            | "stop_cover"
                            | "play_media"
                    )
                })
                .map(|(domain, action)| format!("{domain}.{action}"))
                .unwrap_or_else(|| "unsupported".into());
            let mut safe_parameters = serde_json::Map::new();
            if let Some(body) = requested
                .and_then(|req| req.get("body"))
                .and_then(Value::as_object)
            {
                if let Some(brightness) = body
                    .get("brightness")
                    .and_then(Value::as_u64)
                    .filter(|n| *n <= 255)
                {
                    safe_parameters.insert("brightness".into(), brightness.into());
                }
                if let Some(temperature) = body
                    .get("temperature")
                    .and_then(Value::as_f64)
                    .filter(|n| n.is_finite() && (-40.0..=100.0).contains(n))
                {
                    safe_parameters.insert("temperature".into(), serde_json::json!(temperature));
                }
            }
            let (saved_policy, saved_credential) = requested
                .map(|req| {
                    (
                        req.get("policy")
                            .or_else(|| req.get("policy_revision"))
                            .and_then(Value::as_u64),
                        req.get("credential")
                            .or_else(|| req.get("credential_revision"))
                            .and_then(Value::as_u64),
                    )
                })
                .unwrap_or_default();
            let stale = saved_policy != Some(policy) || saved_credential != Some(credential);
            let phase = row
                .outcome
                .as_ref()
                .and_then(|o| o.phase)
                .and_then(|phase| serde_json::to_value(phase).ok());
            items.push(serde_json::json!({
                    "approval_id":row.approval_id,
                    "digest":row.digest,
                    "status":row.status.as_str(),
                    "entity_id":row.target_record,
                    "service":service,
                    "parameters":safe_parameters,
                    "summary":row.summary.chars().take(200).collect::<String>(),
                    "source":"voice",
                    "device_suffix":row.session_id.as_deref().unwrap_or("").chars().rev().take(4).collect::<String>()
                        .chars().rev().collect::<String>(),
                    "created_at":row.created_at,
                    "expires_at":row.expires_at,
                    "stale":stale,
                    "can_revoke":matches!(row.status,ApprovalStatus::Pending|ApprovalStatus::Approved),
                    "can_approve":!stale && self.home_control.can_approve(&row.approval_id),
                    "outcome":row.outcome.as_ref().map(|outcome|serde_json::json!({
                        "phase":phase,
                        "error_code":outcome.error_code,
                        "message":outcome.message.chars().take(200).collect::<String>(),
                    })),
                }));
        }
        Ok(serde_json::json!({
            "approvals":items,
            "server_time":now(),
            "can_control":self.home_control.enabled(),
            "reason":"控制授权与逐次批准相互独立；语音断开、变更白名单或凭据将拒绝旧请求",
        }))
    }

    /// Independent private-window revoke. Unlike the general approval
    /// routes, this operation accepts HomeControl ONLY and cannot execute.
    pub fn home_approval_cancel(&self, input: &Value) -> Result<Value, CenterError> {
        let invalid = || CenterError::new("home_approval_invalid", 400, "家居撤销请求格式无效");
        let object = input.as_object().ok_or_else(invalid)?;
        if object.len() != 3 || object.get("confirmed") != Some(&Value::Bool(true)) {
            return Err(invalid());
        }
        let id = object
            .get("approval_id")
            .and_then(Value::as_str)
            .filter(|id| {
                (12..=128).contains(&id.len())
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .ok_or_else(invalid)?;
        let digest = object
            .get("digest")
            .and_then(Value::as_str)
            .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(invalid)?;
        // Cancellation cannot race between the final permit check and
        // the first byte of a HomeControl POST.
        let result = self
            .home_control
            .fence_request_change(id, || {
                let service = laser_api::approval::ApprovalService::new(self.host.db.clone());
                service
                    .cancel_home_control(id, digest)
                    .map_err(|err| CenterError::new(err.code(), err.http_status(), err.message()))
            })
            .map_err(|_| internal("家居待执行请求撤销失败"))??;
        Ok(serde_json::json!({
            "approval_id":result.approval_id,
            "status":result.status.as_str(),
            "can_control":false,
        }))
    }
}
