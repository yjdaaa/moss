//! 对话记忆与人物记忆的数据库存储，NV19 长期记忆入口与记忆管理命令。

use super::*;

pub(super) struct DbMemoryStore {
    pub(super) db: Arc<Mutex<laser_runtime::RuntimeDb>>,
}
pub(super) struct DbPersonStore {
    pub(super) db: Arc<Mutex<laser_runtime::RuntimeDb>>,
}
impl PersonMemoryStore for DbPersonStore {
    fn load(&self, scope: &PersonScope) -> Result<Option<PersonSummary>, String> {
        let db = self.db.lock().map_err(|_| "数据库不可用")?;
        if !db
            .list_voice_speakers()
            .map_err(db_error)?
            .iter()
            .any(|r| r.id == scope.id() && r.revision == scope.revision())
        {
            return Ok(None);
        }
        db.voice_person_summary(scope.id())
            .map(|r| r.map(|(text, revision)| PersonSummary { text, revision }))
            .map_err(db_error)
    }
    fn update(&self, scope: &PersonScope, revision: i64, text: &str) -> Result<bool, String> {
        let db = self.db.lock().map_err(|_| "数据库不可用")?;
        if !db
            .list_voice_speakers()
            .map_err(db_error)?
            .iter()
            .any(|r| r.id == scope.id() && r.revision == scope.revision())
        {
            return Ok(false);
        }
        db.update_voice_person_summary_if_revision(scope.id(), revision, text, now())
            .map_err(db_error)
    }
}
impl VoiceMemoryStore for DbMemoryStore {
    fn allowed(&self, id: &str) -> Result<bool, String> {
        self.db
            .lock()
            .map_err(|_| "数据库锁失败".to_owned())?
            .voice_device(id)
            .map(|item| item.is_some_and(|d| d.is_allowed()))
            .map_err(db_error)
    }
    fn load(&self, id: &str) -> Result<Option<MemoryRecord>, String> {
        self.db
            .lock()
            .map_err(|_| "数据库锁失败".to_owned())?
            .voice_memory(id)
            .map(|r| r.map(convert_memory))
            .map_err(db_error)
    }
    fn ensure(&self, id: &str) -> Result<MemoryRecord, String> {
        self.db
            .lock()
            .map_err(|_| "数据库锁失败".to_owned())?
            .ensure_voice_memory(id, now())
            .map(convert_memory)
            .map_err(db_error)
    }
    fn update(
        &self,
        id: &str,
        revision: i64,
        summary: &str,
    ) -> Result<Option<MemoryRecord>, String> {
        self.db
            .lock()
            .map_err(|_| "数据库锁失败".to_owned())?
            .update_voice_memory_if_revision(id, revision, summary, now())
            .map(|r| r.map(convert_memory))
            .map_err(db_error)
    }
    fn list(&self) -> Result<Vec<MemoryRecord>, String> {
        self.db
            .lock()
            .map_err(|_| "数据库锁失败".to_owned())?
            .voice_memories()
            .map(|rows| rows.into_iter().map(convert_memory).collect())
            .map_err(db_error)
    }
}
fn convert_memory(value: laser_runtime::VoiceMemoryRecord) -> MemoryRecord {
    MemoryRecord {
        device_id: value.device_id,
        summary: value.summary,
        revision: value.revision,
        updated_at: value.updated_at,
    }
}

impl BuiltinVoice {
    /// NV19 private-window management (`voice_context_manage`).
    pub fn context_manage(&self, input: &Value) -> Result<Value, CenterError> {
        let out = self.context.manage(input, || self.chat_llm())?;
        if input.get("action").and_then(Value::as_str) == Some("set_options")
            && input.get("sync_legacy_memory").and_then(Value::as_bool) == Some(true)
            && let Some(enabled) = out.get("retrieval").and_then(Value::as_bool)
        {
            let _ = self.memory_enable(enabled);
            if !enabled
                && let Ok(db) = self.host.db.lock()
                && let Ok(devices) = db.voice_devices()
            {
                for d in devices.into_iter().filter(|d| d.is_allowed()) {
                    let _ = self
                        .memory
                        .device_changed(&d.device_id, || Ok::<_, String>(()));
                }
            }
        }
        if input.get("action").and_then(Value::as_str) == Some("clear_all")
            && let Ok(list) = self.memory.list()
        {
            for item in list {
                if !item.summary.is_empty() {
                    let _ = self.memory.edit(&item.device_id, item.revision, "");
                }
            }
        }
        Ok(out)
    }

    pub fn context_add_folder(&self, path: &std::path::Path) -> Result<Value, CenterError> {
        self.context.add_folder(path)
    }

    /// The configured cloud LLM for NV19 deep search; `None` when not configured.
    fn chat_llm(&self) -> Option<Arc<dyn moss_voice_providers::Llm>> {
        let settings = self.settings();
        let (url, key, model) = match self.active_colab(&settings).ok().flatten() {
            Some((colab, key)) => (colab.url, key, colab.llm_model),
            None => {
                let llm = settings.builtin_models.llm.as_ref()?;
                let key = self.secret(Section::Llm).ok()?.unwrap_or_default();
                (llm.url.clone(), key, llm.model.clone())
            }
        };
        OpenAiLlm::new(&url, &key, &model)
            .ok()
            .map(|llm| llm.with_fast_reply(settings.builtin_local.llm_fast_reply))
            .map(|llm| Arc::new(llm) as Arc<dyn moss_voice_providers::Llm>)
    }

    pub fn memory_enable(&self, enabled: bool) -> Result<Value, CenterError> {
        self.memory
            .configure(enabled, || {
                moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                    settings.builtin_memory_enabled = enabled;
                    Ok(())
                })
            })
            .map_err(|e| CenterError::new("memory_settings_failed", 500, e))?;
        Ok(serde_json::json!({"enabled": enabled}))
    }
}
