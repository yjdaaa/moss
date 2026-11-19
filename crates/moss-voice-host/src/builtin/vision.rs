//! 视觉凭据解析、本地视觉模型运行管理与视觉设置命令。

use super::*;

/// NV12: fresh signed-in device permission and fresh user configuration are
/// consulted for every image; snapshots are never logged or stored as conversation history.
/// 2026-10-16：`use_dialog_llm` 打开时改用当前对话大模型的地址 / 模型名与 `voice.llm.main` 密钥，
/// 仍然只发这一次请求、不进对话历史；对话模型必须 OpenAI 兼容且支持看图。
fn vision_credentials(
    setting: &moss_voice::settings::BuiltinVision,
    llm: Option<&moss_voice::settings::BuiltinProvider>,
    vision_key: Option<&str>,
    llm_key: Option<&str>,
) -> Option<(String, String, String)> {
    if !setting.enabled {
        return None;
    }
    let usable = |url: &str, model: &str, key: &str| {
        !model.trim().is_empty()
            && !key.trim().is_empty()
            && moss_voice_providers::vision::endpoint(url).is_ok()
    };
    if setting.use_dialog_llm {
        let llm = llm.filter(|llm| llm.kind.is_openai())?;
        let key = llm_key?;
        return usable(&llm.url, &llm.model, key)
            .then(|| (llm.url.clone(), llm.model.clone(), key.to_owned()));
    }
    let key = vision_key?;
    usable(&setting.url, &setting.model, key)
        .then(|| (setting.url.clone(), setting.model.clone(), key.to_owned()))
}
/// 本地视觉模型要怎么启动（由 `BuiltinVoice::sync_local_vision` 按设置与安装情况给出）。
#[derive(Clone, PartialEq, Eq)]
pub(super) struct LocalVisionLaunch {
    pub(super) model: String,
    pub(super) dir: std::path::PathBuf,
    pub(super) root: std::path::PathBuf,
    pub(super) mirror: Option<String>,
}

#[derive(Default)]
struct LocalVisionState {
    launch: Option<LocalVisionLaunch>,
    server: Option<moss_voice::local_vision::LocalVisionServer>,
    starting: Option<Arc<std::sync::atomic::AtomicBool>>,
    failure: Option<String>,
    generation: u64,
}

/// n74 本地视觉模型（Qwen3-VL）运行管理：后台预加载，就绪后视觉理解直接调用本机地址；
/// 停用、换模型或服务停止时结束推理进程释放内存。与本地 Jev 分开运行，互不排队。
#[derive(Default)]
pub(crate) struct LocalVision {
    state: Mutex<LocalVisionState>,
    /// 串行化启动，避免预加载与设备请求同时各起一个进程
    start_lock: Mutex<()>,
}

impl LocalVision {
    fn lock(&self) -> std::sync::MutexGuard<'_, LocalVisionState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 设置要用的模型；为 `None` 时停掉推理进程。模型或路径变了也会先停掉旧的。
    pub(super) fn configure(&self, launch: Option<LocalVisionLaunch>) {
        let mut state = self.lock();
        if state.launch == launch {
            return;
        }
        state.generation += 1;
        if let Some(cancel) = state.starting.take() {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        state.server = None;
        state.failure = None;
        state.launch = launch;
    }

    pub(super) fn launch(&self) -> Option<LocalVisionLaunch> {
        self.lock().launch.clone()
    }

    fn ready_url(&self) -> Option<String> {
        let mut state = self.lock();
        let alive = state.server.as_mut()?.check_alive();
        match alive {
            Ok(()) => state.server.as_ref().map(|server| server.base_url()),
            Err(error) => {
                state.server = None;
                state.failure = Some(error.clone());
                record_moss_event(
                    EventRecord::error(DiagCategory::Model, "本地视觉模型的推理进程意外退出")
                        .hint("下次看图时会自动重新加载；反复出现时到“本地模型”重新下载视觉模型")
                        .detail(error),
                );
                None
            }
        }
    }

    /// 确保推理进程在跑，返回本机地址；没在跑就启动并等它就绪（会阻塞，只在后台线程调用）。
    pub(super) fn ensure(&self) -> Result<String, String> {
        if let Some(url) = self.ready_url() {
            return Ok(url);
        }
        let _start = self
            .start_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(url) = self.ready_url() {
            return Ok(url);
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (launch, generation) = {
            let mut state = self.lock();
            let launch = state
                .launch
                .clone()
                .ok_or_else(|| "没有选用已下载的本地视觉模型".to_owned())?;
            state.starting = Some(cancel.clone());
            state.failure = None;
            (launch, state.generation)
        };
        let result = moss_voice::local_vision::LocalVisionServer::start(
            &launch.model,
            &launch.dir,
            &launch.root,
            launch.mirror.as_deref(),
            &cancel,
        );
        let mut state = self.lock();
        if state.generation != generation {
            return Err("本地视觉模型设置已变化，请重试".into());
        }
        state.starting = None;
        match result {
            Ok(server) => {
                let url = server.base_url();
                state.server = Some(server);
                Ok(url)
            }
            Err(error) => {
                state.failure = Some(error.clone());
                record_moss_event(
                    EventRecord::error(
                        DiagCategory::Model,
                        format!("本地视觉模型（{}）启动失败", launch.model),
                    )
                    .hint("确认模型已完整下载、磁盘与内存够用；看技术细节里推理进程的输出")
                    .detail(&error),
                );
                Err(error)
            }
        }
    }

    /// 后台预加载（服务启动、保存设置后调用），不阻塞调用方。
    fn preload(self: &Arc<Self>) {
        if self.launch().is_none() || self.ready_url().is_some() || self.lock().starting.is_some() {
            return;
        }
        let this = self.clone();
        let _ = std::thread::Builder::new()
            .name("moss-local-vision".into())
            .spawn(move || {
                let _ = this.ensure();
            });
    }

    /// 给语音中心卡片显示：`idle` / `starting` / `ready` / `failed`。
    pub(super) fn status(&self) -> Value {
        let ready = self.ready_url().is_some();
        let state = self.lock();
        let phase = if ready {
            "ready"
        } else if state.starting.is_some() {
            "starting"
        } else if state.failure.is_some() {
            "failed"
        } else {
            "idle"
        };
        serde_json::json!({
            "state": phase,
            "model": state.launch.as_ref().map(|launch| launch.model.clone()),
            "accelerator": state.server.as_ref().map(|server| server.accelerator_text()),
            "error": state.failure,
        })
    }
}

pub(super) struct DesktopVisionFactory {
    pub(super) host: Arc<laser_host::HostApplication>,
    pub(super) local: Arc<LocalVision>,
}
impl DesktopVisionFactory {
    fn vision_setting(&self) -> moss_voice::settings::BuiltinVision {
        moss_voice::settings::load(&self.host.runtime_root.path.join("settings.json"))
            .builtin_vision
    }
    /// 返回（地址、模型、密钥、配置修订号）。
    fn profile(&self) -> Option<(String, String, String, u64)> {
        use std::hash::{Hash, Hasher};
        let settings =
            moss_voice::settings::load(&self.host.runtime_root.path.join("settings.json"));
        // n80：Colab T4 联动开启且密钥就绪时，临时接管摄像头看图（关闭开关即刻恢复下方原视觉配置）
        if settings.builtin_colab.enabled
            && let Some(key) = self
                .host
                .db
                .lock()
                .ok()
                .and_then(|db| {
                    db.get_secret(moss_voice::colab::COLAB_SECRET_NAME)
                        .ok()
                        .flatten()
                })
                .filter(|s| !s.trim().is_empty())
            && settings.builtin_colab.active(true)
            && let Ok(url) = moss_voice::colab::normalize_colab_url(&settings.builtin_colab.url)
            && moss_voice_providers::vision::endpoint(&url).is_ok()
        {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            "colab".hash(&mut hasher);
            url.hash(&mut hasher);
            settings.builtin_colab.llm_model.hash(&mut hasher);
            key.hash(&mut hasher);
            return Some((url, settings.builtin_colab.llm_model, key, hasher.finish()));
        }
        let setting = settings.builtin_vision;
        if setting.enabled && !setting.use_dialog_llm && !setting.local_model.is_empty() {
            // 本地视觉：地址在打开会话时才确定（推理进程按需启动），这里只看是否已选好并下载
            let launch = self
                .local
                .launch()
                .filter(|launch| launch.model == setting.local_model)?;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            "local".hash(&mut hasher);
            launch.model.hash(&mut hasher);
            launch.dir.hash(&mut hasher);
            return Some((String::new(), launch.model, String::new(), hasher.finish()));
        }
        let (vision_key, llm_key) = {
            let db = self.host.db.lock().ok()?;
            (
                db.get_secret("voice.vision.main").ok().flatten(),
                db.get_secret("voice.llm.main").ok().flatten(),
            )
        };
        let (url, model, key) = vision_credentials(
            &setting,
            settings.builtin_models.llm.as_ref(),
            vision_key.as_deref(),
            llm_key.as_deref(),
        )?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        setting.enabled.hash(&mut hasher);
        setting.use_dialog_llm.hash(&mut hasher);
        url.hash(&mut hasher);
        model.hash(&mut hasher);
        key.hash(&mut hasher);
        Some((url, model, key, hasher.finish()))
    }
}
impl moss_voice_server::vision::VisionFactory for DesktopVisionFactory {
    fn ready(&self) -> bool {
        self.profile().is_some()
    }
    fn open(&self) -> Option<moss_voice_server::vision::VisionSession> {
        let (url, model, key, generation) = self.profile()?;
        let (url, model) = if url.is_empty() {
            // 本地视觉：在后台线程里调用，推理进程没在跑就等它加载好（服务启动时已预加载，通常立即可用）
            (
                self.local.ensure().ok()?,
                moss_voice::local_vision::LOCAL_VISION_MODEL_NAME.to_owned(),
            )
        } else {
            (url, model)
        };
        let provider = moss_voice_providers::vision::OpenAiVision::new(&url, &model, &key).ok()?;
        Some(moss_voice_server::vision::VisionSession {
            provider: Arc::new(provider),
            generation,
        })
    }
    fn still_current(&self, generation: u64) -> bool {
        self.profile()
            .is_some_and(|(_, _, _, revision)| revision == generation)
    }
    /// 2026-10-16：视觉卡片打开“把照片留在对话上下文里”时，服务端才会把这张照片交给会话。
    fn keep_in_history(&self) -> bool {
        let setting = self.vision_setting();
        setting.enabled && setting.keep_in_history
    }
}

impl BuiltinVoice {
    /// NV12: private desktop-only settings and API-key presence. Never return the key.
    pub fn vision_status(&self) -> Value {
        use moss_voice_server::vision::VisionFactory;
        let settings = self.settings();
        let vision = &settings.builtin_vision;
        let key_configured = self
            .host
            .db
            .lock()
            .ok()
            .and_then(|db| db.get_secret("voice.vision.main").ok().flatten())
            .is_some_and(|s| !s.trim().is_empty());
        // 2026-10-16：只回对话模型的地址与模型名，用于卡片显示“照片会发给谁”，不回密钥。
        let dialog_llm = match settings.builtin_models.llm.as_ref() {
            Some(llm) => {
                let key = self
                    .host
                    .db
                    .lock()
                    .ok()
                    .and_then(|db| db.get_secret("voice.llm.main").ok().flatten())
                    .is_some_and(|s| !s.trim().is_empty());
                serde_json::json!({
                    "configured":llm.kind.is_openai()&&!llm.model.trim().is_empty()
                        &&moss_voice_providers::vision::endpoint(&llm.url).is_ok()&&key,
                    "url":llm.url,"model":llm.model,
                })
            }
            None => serde_json::json!({"configured":false,"url":"","model":""}),
        };
        let ready = DesktopVisionFactory {
            host: self.host.clone(),
            local: self.local_vision.clone(),
        }
        .ready();
        let colab_active = self.active_colab(&settings).ok().flatten().is_some();
        let colab_override = serde_json::json!({
            "active": colab_active,
            "url": if colab_active { settings.builtin_colab.url.clone() } else { String::new() },
            "model": if colab_active { settings.builtin_colab.llm_model.clone() } else { String::new() },
        });
        // n74：可选的本地视觉模型及是否已下载（下载在“本地模型”里做）
        let local_models: Vec<Value> = moss_voice::local_models::CATALOG
            .iter()
            .filter(|spec| spec.kind == moss_voice::local_models::ModelKind::Vision)
            .map(|spec| {
                serde_json::json!({
                    "id": spec.id,
                    "label": spec.label,
                    "size": spec.download_size(),
                    "installed": self.models.path_if_installed(spec.id).is_some(),
                })
            })
            .collect();
        serde_json::json!({"settings":vision,"key_configured":key_configured,"ready":ready,"dialog_llm":dialog_llm,"colab_override":colab_override,"local_models":local_models,"local_runtime":self.local_vision.status()})
    }

    pub fn vision_save(&self, input: &Value) -> Result<Value, CenterError> {
        use moss_voice::settings::BuiltinVision;
        let invalid = |reason: &str| CenterError::new("vision_invalid", 400, reason);
        let next: BuiltinVision =
            serde_json::from_value(input.get("settings").cloned().unwrap_or(Value::Null))
                .map_err(|_| invalid("视觉配置格式错误"))?;
        if next.url.len() > 1024 || next.model.len() > 200 {
            return Err(invalid("模型名称或地址太长"));
        }
        let local = next.enabled && !next.use_dialog_llm && !next.local_model.is_empty();
        if !next.local_model.is_empty()
            && moss_voice::local_models::spec_of(
                &next.local_model,
                moss_voice::local_models::ModelKind::Vision,
            )
            .is_none()
        {
            return Err(invalid("未知的本地视觉模型"));
        }
        // n74：本地视觉只要求模型已下载，不需要地址与密钥
        if local {
            if self.models.path_if_installed(&next.local_model).is_none() {
                return Err(invalid("请先在“本地模型”里下载这个视觉模型"));
            }
        } else if next.enabled && next.use_dialog_llm {
            let llm_ok = self
                .settings()
                .builtin_models
                .llm
                .as_ref()
                .is_some_and(|llm| {
                    llm.kind.is_openai()
                        && !llm.model.trim().is_empty()
                        && moss_voice_providers::vision::endpoint(&llm.url).is_ok()
                });
            if !llm_ok {
                return Err(invalid(
                    "请先在模型设置里填写 OpenAI 兼容的对话模型地址与模型名，再用它看照片",
                ));
            }
            let llm_key = self
                .host
                .db
                .lock()
                .ok()
                .and_then(|db| db.get_secret("voice.llm.main").ok().flatten())
                .is_some_and(|s| !s.trim().is_empty());
            if !llm_key {
                return Err(invalid("请先保存对话大模型的密钥"));
            }
        } else if next.enabled
            && (next.model.trim().is_empty()
                || moss_voice_providers::vision::endpoint(&next.url).is_err())
        {
            return Err(invalid(
                "请填写有效的模型名称和 HTTPS 地址（或本地明确的 HTTP 地址）",
            ));
        }
        let action = input["key"]["action"].as_str().unwrap_or("keep");
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        let existing = db
            .get_secret("voice.vision.main")
            .map_err(db_failure)?
            .is_some_and(|s| !s.is_empty());
        let new_key = match action {
            "keep" => None,
            "set" => {
                let key = input["key"]["value"].as_str().unwrap_or("");
                if key.trim().is_empty() || key.len() > 8192 {
                    return Err(invalid("视觉模型密钥不能空或超过 8192 字节"));
                }
                Some(Some(key.to_owned()))
            }
            "clear" => Some(None),
            _ => return Err(invalid("密钥操作只能保留、更新或清除")),
        };
        let has_key = match &new_key {
            None => existing,
            Some(Some(_)) => true,
            Some(None) => false,
        };
        if next.enabled && !next.use_dialog_llm && !local && !has_key {
            return Err(invalid("启用视觉前必须保存对应模型密钥"));
        }
        let changes: Vec<(String, Option<String>)> = new_key
            .into_iter()
            .map(|key| ("voice.vision.main".to_owned(), key))
            .collect();
        commit_secrets_then_settings(&db, &changes, || {
            moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                settings.builtin_vision = next.clone();
                Ok(())
            })
            .map_err(|_| CenterError::new("vision_save_failed", 500, "视觉配置保存失败"))
        })?;
        drop(db);
        self.sync_local_vision(true);
        Ok(self.vision_status())
    }

    /// 按当前设置配置本地视觉模型：选了已下载的本地模型就记下启动方式（`preload` 时后台加载），
    /// 否则停掉推理进程。服务启动 / 停止、保存视觉设置、删除模型后调用。
    pub(super) fn sync_local_vision(&self, preload: bool) {
        let settings = self.settings();
        let colab_active = self.active_colab(&settings).ok().flatten().is_some();
        let vision = settings.builtin_vision;
        let launch = (!colab_active
            && vision.enabled
            && !vision.use_dialog_llm
            && !vision.local_model.is_empty())
        .then(|| self.models.path_if_installed(&vision.local_model))
        .flatten()
        .map(|dir| LocalVisionLaunch {
            root: self.models.root_for(&vision.local_model).to_path_buf(),
            model: vision.local_model.clone(),
            dir,
            mirror: settings.builtin_local.mirror,
        });
        self.local_vision.configure(launch);
        if preload && self.running() {
            self.local_vision.preload();
        }
    }

    /// A button press uses only an in-memory synthetic 8x8 image.
    /// It does not access a device or a computer camera.
    pub fn vision_test(&self) -> Result<Value, CenterError> {
        use moss_voice_server::vision::VisionFactory;
        self.sync_local_vision(false);
        let factory = DesktopVisionFactory {
            host: self.host.clone(),
            local: self.local_vision.clone(),
        };
        let settings = self.settings();
        let colab_active = self.active_colab(&settings).ok().flatten().is_some();
        let vision_setting = settings.builtin_vision;
        if !colab_active
            && vision_setting.enabled
            && !vision_setting.use_dialog_llm
            && !vision_setting.local_model.is_empty()
        {
            // 点测试时等本地模型加载完再测；加载失败直接说原因
            self.local_vision
                .ensure()
                .map_err(|error| CenterError::new("vision_local_failed", 502, error))?;
        }
        let missing = if self.settings().builtin_vision.use_dialog_llm {
            "请先启用视觉，并在模型设置里配好支持看图的对话模型与密钥"
        } else {
            "请先启用并保存视觉模型与密钥"
        };
        let session = factory
            .open()
            .ok_or_else(|| CenterError::new("vision_disabled", 409, missing))?;
        let mut output = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 8)
            .write_to(&mut output, image::ImageFormat::Png)
            .map_err(|_| internal("生成内存测试图片失败"))?;
        let png = output.into_inner();
        let started = std::time::Instant::now();
        let result = crate::block_on(session.provider.explain(
            "请仅用一句话描述此测试图像。",
            "image/png",
            &png,
        ));
        match result {
            Ok(text) if factory.still_current(session.generation) => Ok(serde_json::json!({
                "ok":true,"elapsed_ms":started.elapsed().as_millis(),"preview":text.chars().take(160).collect::<String>(),
            })),
            Ok(_) => Err(CenterError::new(
                "vision_settings_changed",
                409,
                "测试中配置发生变化，请重试",
            )),
            Err(error) => Err(CenterError::new(
                error.code(),
                502,
                "视觉模型连接或返回内容异常，请核对模型和密钥",
            )),
        }
    }
}

#[cfg(test)]
mod vision_credentials_tests {
    use super::vision_credentials;
    use moss_voice::models::ProviderKind;
    use moss_voice::settings::{BuiltinProvider, BuiltinVision};

    fn provider(kind: ProviderKind) -> BuiltinProvider {
        BuiltinProvider {
            kind,
            url: "https://llm.example.test/v1".into(),
            model: "main-model".into(),
            voice: String::new(),
            speed: 1.0,
        }
    }

    #[test]
    fn independent_vision_needs_its_own_key() {
        let setting = BuiltinVision {
            enabled: true,
            url: "https://vision.example.test/v1".into(),
            model: "vl-1".into(),
            use_dialog_llm: false,
            keep_in_history: false,
            local_model: String::new(),
        };
        assert!(
            vision_credentials(&setting, None, None, Some("llm-key")).is_none(),
            "独立的视觉模型没有密钥就不能用"
        );
        assert_eq!(
            vision_credentials(&setting, None, Some("vision-key"), Some("llm-key")),
            Some((
                "https://vision.example.test/v1".into(),
                "vl-1".into(),
                "vision-key".into()
            ))
        );
    }

    #[test]
    fn dialog_llm_mode_uses_the_conversation_model_and_key() {
        let setting = BuiltinVision {
            enabled: true,
            url: String::new(),
            model: String::new(),
            use_dialog_llm: true,
            keep_in_history: false,
            local_model: String::new(),
        };
        let openai = provider(ProviderKind::Openai);
        assert_eq!(
            vision_credentials(&setting, Some(&openai), Some("vision-key"), Some("llm-key")),
            Some((
                "https://llm.example.test/v1".into(),
                "main-model".into(),
                "llm-key".into()
            )),
            "多模态模式用对话模型的地址 / 模型名 / 密钥，视觉专用密钥与地址都不参与"
        );
        assert!(
            vision_credentials(&setting, Some(&openai), Some("vision-key"), None).is_none(),
            "对话模型没有密钥就不能用"
        );
        assert!(
            vision_credentials(&setting, None, Some("vision-key"), Some("llm-key")).is_none(),
            "没有配置对话模型就不能用"
        );
    }

    #[test]
    fn dialog_llm_mode_refuses_non_openai_and_disabled() {
        let mut setting = BuiltinVision {
            enabled: true,
            url: String::new(),
            model: String::new(),
            use_dialog_llm: true,
            keep_in_history: false,
            local_model: String::new(),
        };
        let doubao = provider(ProviderKind::Doubao);
        assert!(
            vision_credentials(&setting, Some(&doubao), None, Some("llm-key")).is_none(),
            "非 OpenAI 兼容的对话模型不能看图"
        );
        setting.enabled = false;
        let openai = provider(ProviderKind::Openai);
        assert!(
            vision_credentials(&setting, Some(&openai), Some("k"), Some("k")).is_none(),
            "总开关关着就不提供"
        );
    }
}
