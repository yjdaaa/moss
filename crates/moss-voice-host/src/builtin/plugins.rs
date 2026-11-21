//! 说话角色存储、信息服务插件宿主与插件设置。

use super::*;

/// n59 当前说话角色的组成：内置 / 自建角色、共用的工坊补充背景。
pub(super) fn persona_config(local: &BuiltinLocal) -> PersonaConfig {
    PersonaConfig {
        active: local.persona_style.clone(),
        background: local.persona_prompt.clone(),
        custom: local
            .custom_personas
            .iter()
            .map(|c| CustomRole {
                id: c.id.clone(),
                name: c.name.clone(),
                prompt: c.prompt.clone(),
            })
            .collect(),
        risk_notes: local.persona_risk_notes,
    }
}

/// 语音切换角色时写入设置（只改 `persona_style`，不重启服务）。核对与更新运行中的角色都在设置写入锁内，
/// 与界面保存（`local_save`）排成同一顺序。
pub(super) struct DesktopPersonaStore {
    pub(super) settings_file: std::path::PathBuf,
}
impl PersonaStore for DesktopPersonaStore {
    fn save_active(&self, id: &str, apply: &mut dyn FnMut(PersonaConfig)) -> Result<(), String> {
        self.save_active_if(id, &|| true, apply)
    }

    fn save_active_if(
        &self,
        id: &str,
        allowed: &dyn Fn() -> bool,
        apply: &mut dyn FnMut(PersonaConfig),
    ) -> Result<(), String> {
        let mut touched = false;
        let result = moss_voice::settings::update_voice(&self.settings_file, |settings| {
            // 持设置写锁后再次复核设备授权，防止撤销后的滞后阻塞任务改写全局角色
            if !allowed() {
                return Err("语音设备授权已失效，未修改角色".into());
            }
            let local = &mut settings.builtin_local;
            touched = true;
            match moss_voice::settings::validate_active_persona(id, &local.custom_personas) {
                Ok(active) => {
                    local.persona_style = active;
                    apply(persona_config(local));
                    Ok(())
                }
                Err(message) => {
                    // The role is gone from the file (e.g. just deleted); re-sync the running role to it.
                    apply(persona_config(local));
                    Err(message)
                }
            }
        });
        if result.is_err() && touched {
            // The file write failed after the running role changed; follow the file again.
            apply(persona_config(
                &moss_voice::settings::load(&self.settings_file).builtin_local,
            ));
        }
        result
    }
}

pub(super) struct DesktopInfoHost {
    pub(super) host: Arc<laser_host::HostApplication>,
}
impl PluginHost for DesktopInfoHost {
    fn config(&self) -> Value {
        let path = self.host.runtime_root.path.join("settings.json");
        serde_json::to_value(moss_voice::settings::load(&path).builtin_plugins)
            .unwrap_or(Value::Null)
    }
    fn secret(&self, kind: &str) -> Option<String> {
        let name = match kind {
            "weather" => "voice.plugin.weather",
            "search" => "voice.plugin.search",
            "metaso" => "voice.plugin.metaso",
            "rag" => "voice.plugin.rag",
            _ => return None,
        };
        self.host
            .db
            .lock()
            .ok()
            .and_then(|db| db.get_secret(name).ok().flatten())
            .filter(|s| !s.is_empty())
    }
    fn permitted(&self, ctx: &ToolContext) -> bool {
        self.host
            .api_service
            .voice_session_allowed(&ctx.device_id, &ctx.session_id)
    }
}

impl BuiltinVoice {
    pub fn plugins_status(&self) -> Value {
        let settings = self.settings().builtin_plugins;
        let mut keys = serde_json::Map::new();
        for kind in ["weather", "search", "metaso", "rag"] {
            let name = format!("voice.plugin.{kind}");
            let found = self
                .host
                .db
                .lock()
                .ok()
                .and_then(|db| db.get_secret(&name).ok().flatten())
                .is_some_and(|value| !value.is_empty());
            keys.insert(kind.into(), Value::Bool(found));
        }
        serde_json::json!({"settings":settings,"keys":keys})
    }

    pub fn plugins_save(&self, input: &Value) -> Result<Value, CenterError> {
        use moss_voice::settings::BuiltinPlugins;
        use moss_voice_server::plugins::http;
        let invalid = |message: &str| CenterError::new("plugin_config_invalid", 400, message);
        let config: BuiltinPlugins =
            serde_json::from_value(input.get("settings").cloned().unwrap_or(Value::Null))
                .map_err(|_| invalid("信息插件配置格式不正确"))?;
        if !["api_key", "bearer"].contains(&config.weather_auth.as_str()) {
            return Err(invalid("天气认证方式必须为 API Key 或 Bearer"));
        }
        if config.weather_enabled {
            http::validate_endpoint(&config.weather_host, false)
                .map_err(|_| invalid("天气服务地址必须是合法 HTTPS 地址"))?;
            let lat = config
                .weather_latitude
                .ok_or_else(|| invalid("请设置天气地点纬度"))?;
            let lon = config
                .weather_longitude
                .ok_or_else(|| invalid("请设置天气地点经度"))?;
            if !lat.is_finite()
                || !(-90.0..=90.0).contains(&lat)
                || !lon.is_finite()
                || !(-180.0..=180.0).contains(&lon)
            {
                return Err(invalid("纬度经度不在允许范围"));
            }
        }
        if !(-720..=840).contains(&config.lunar_utc_offset_minutes) {
            return Err(invalid("农历时区偏移无效"));
        }
        if config.news_sources.len() > 10 {
            return Err(invalid("RSS 来源最多 10 个"));
        }
        let mut unique = std::collections::HashSet::new();
        for source in &config.news_sources {
            if source.id.is_empty()
                || source.id.len() > 40
                || !source
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                || !unique.insert(source.id.clone())
            {
                return Err(invalid(
                    "RSS 来源编号必须唯一且仅含字母、数字、短横线和下划线",
                ));
            }
            let is_newsnow = matches!(source.kind, moss_voice::settings::NewsSourceKind::Newsnow);
            let url = http::validate_endpoint(&source.url, is_newsnow).map_err(|_| {
                invalid("RSS 必须使用 HTTPS；NewsNow 可使用明确配置的本机/LAN 地址")
            })?;
            if url.query().is_some() {
                return Err(invalid(
                    "新闻源地址不得包含查询参数，避免将密钥写入普通配置",
                ));
            }
            if matches!(source.kind, moss_voice::settings::NewsSourceKind::Newsnow)
                && (source.source_id.is_empty()
                    || source.source_id.len() > 80
                    || !source
                        .source_id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            {
                return Err(invalid("NewsNow 来源 ID 必须为字母、数字、短横线或下划线"));
            }
        }
        if config.news_enabled && config.news_sources.is_empty() {
            return Err(invalid("开启新闻插件前必须添加 RSS 来源"));
        }
        if config.search_enabled {
            let host = match config.search_provider {
                moss_voice::settings::SearchProvider::Tavily => &config.search_host,
                moss_voice::settings::SearchProvider::Metaso => &config.metaso_host,
            };
            http::validate_endpoint(host, false)
                .map_err(|_| invalid("联网搜索必须使用 HTTPS 服务地址"))?;
        }
        if config.rag_enabled {
            http::validate_endpoint(&config.rag_host, true)
                .map_err(|_| invalid("RAGFlow 服务地址必须为 HTTPS 或明确的本机/LAN 地址"))?;
            if config.rag_dataset_ids.is_empty()
                || config.rag_dataset_ids.len() > 20
                || config
                    .rag_dataset_ids
                    .iter()
                    .any(|id| id.trim().is_empty() || id.len() > 128)
            {
                return Err(invalid("RAGFlow 数据集白名单格式不正确"));
            }
        }

        // Keep/Set/Clear: missing key input means keep; no returned secret.
        let keys = &input["keys"];
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        let mut operations = Vec::<(String, Option<String>)>::new();
        for (kind, enabled) in [
            ("weather", config.weather_enabled),
            (
                "search",
                config.search_enabled
                    && config.search_provider == moss_voice::settings::SearchProvider::Tavily,
            ),
            (
                "metaso",
                config.search_enabled
                    && config.search_provider == moss_voice::settings::SearchProvider::Metaso,
            ),
            ("rag", config.rag_enabled),
        ] {
            let name = format!("voice.plugin.{kind}");
            let command = keys[kind]["action"].as_str().unwrap_or("keep");
            let existing = db
                .get_secret(&name)
                .map_err(db_failure)?
                .is_some_and(|k| !k.is_empty());
            let available = match command {
                "keep" => existing,
                "set" => {
                    let value = keys[kind]["value"].as_str().unwrap_or("");
                    if value.trim().is_empty() || value.len() > 4096 {
                        return Err(invalid("API 密钥格式不正确（不能空或超过4096字节）"));
                    }
                    operations.push((name, Some(value.to_owned())));
                    true
                }
                "clear" => {
                    operations.push((name, None));
                    false
                }
                _ => return Err(invalid("API 密钥操作必须为 keep/set/clear")),
            };
            if enabled && !available {
                return Err(invalid("开启需要 API 密钥的插件前必须设置密钥"));
            }
        }

        // 持锁到提交完成：校验时看到的密钥状态不会被并发保存改掉
        commit_secrets_then_settings(&db, &operations, || {
            moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                settings.builtin_plugins = config.clone();
                Ok(())
            })
            .map_err(|_| CenterError::new("plugin_settings_failed", 500, "插件配置保存失败"))
        })?;
        drop(db);
        Ok(self.plugins_status())
    }
}
