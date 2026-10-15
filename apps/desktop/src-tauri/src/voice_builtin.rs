//! 内置语音服务的 Tauri 命令（内置语音技术书 NV1 起）：启动 / 停止、状态、设备允许与撤销、模型配置（NV5）。
//!
//! 与 `voice.rs` 一样以 Tauri 命令提供，只有 MOSS 桌面窗口能调用；Android 不编译服务本身，命令直接返回错误。
//! 设备登记存 SQLite `voice_devices`（schema v6），设备 token 的签名密钥与模型密钥存 `app_secrets`
//! （`voice.device_token_secret`、`voice.llm.main` 等，任何命令都不返回原文）。
//!
//! 识别、大模型、合成三项都可用时以对话模式启动，否则以回声模式启动；对话中调用 MOSS 工具时身份为
//! `CallerContext::Voice`（运动与出光跟随严格模式开关）。语音链路为“内置服务”时随 MOSS 启动。
//!
//! 本地模型（NV6）：识别（SenseVoice）、合成（Kokoro）、判停（Silero）可改用本机模型，由与 MOSS 同目录的
//! 辅助程序 `moss-voice-local`（GPL-3.0）运行；模型按需下载到运行目录 `models/` 并校验 SHA-256。
//!
//! 豆包流式（NV7）：识别与合成可选豆包。识别在用户开始说话时就建立连接、边说边传；合成每句的第一段音频
//! 一到就播放（播放启动缓冲相应从 600 ms 降到 300 ms）。

use serde_json::Value;

type CmdResult = Result<Value, Value>;

/// Defense in depth beyond Tauri ACL: private commands require the unique
/// packaged voice-private webview and its original local asset URL.
#[cfg(desktop)]
pub(crate) fn is_voice_private_asset(
    label: &str, scheme: &str, host: Option<&str>, port: Option<u16>,
    path: &str, query: Option<&str>, fragment: Option<&str>,
) -> bool {
    label == "voice-private"
        && port.is_none() && query.is_none() && fragment.is_none()
        && path == "/voice-private.html"
        && ((matches!(scheme, "http" | "https") && host == Some("tauri.localhost"))
            || (scheme == "tauri" && host == Some("localhost")))
}

#[cfg(desktop)]
pub(crate) fn require_voice_private_asset(window: &tauri::WebviewWindow) -> Result<(), Value> {
    let fail = || serde_json::json!({
        "code": "voice_acl_denied",
        "message": "私人记忆和插件配置只能在 MOSS 本地安全设置窗口访问"
    });
    let url = window.url().map_err(|_| fail())?;
    if !is_voice_private_asset(
        window.label(), url.scheme(), url.host_str(), url.port(),
        url.path(), url.query(), url.fragment(),
    ) {
        return Err(fail());
    }
    Ok(())
}

/// 主窗口专用命令：只接受后端地址上的 `main` 窗口（见 [`crate::is_trusted_main`]）。
#[cfg(desktop)]
fn require_main_window(window: &tauri::WebviewWindow) -> Result<(), Value> {
    if crate::window_is_trusted_main(window) {
        return Ok(());
    }
    Err(serde_json::json!({
        "code": "voice_acl_denied",
        "message": "只能在 MOSS 主窗口修改内置语音设置"
    }))
}

#[cfg(all(test, desktop))]
#[test]
fn sensitive_voice_commands_require_packaged_private_window_origin() {
    let allowed = |label, scheme, host, port, path, query, fragment| {
        is_voice_private_asset(label, scheme, host, port, path, query, fragment)
    };
    assert!(allowed("voice-private", "http", Some("tauri.localhost"), None,
        "/voice-private.html", None, None));
    assert!(allowed("voice-private", "tauri", Some("localhost"), None,
        "/voice-private.html", None, None));
    for (label, scheme, host, port, path, query, fragment) in [
        ("main", "http", Some("tauri.localhost"), None, "/voice-private.html", None, None),
        ("main", "http", Some("127.0.0.1"), Some(9666), "/voice-private.html", None, None),
        ("voice-private", "http", Some("127.0.0.1"), Some(9666), "/voice-private.html", None, None),
        ("voice-private", "https", Some("example.com"), None, "/voice-private.html", None, None),
        ("voice-private", "http", Some("tauri.localhost"), Some(8080), "/voice-private.html", None, None),
        ("voice-private", "http", Some("tauri.localhost"), None, "/index.html", None, None),
        ("voice-private", "http", Some("tauri.localhost"), None, "/voice-private.html", Some("x=1"), None),
        ("voice-private", "http", Some("tauri.localhost"), None, "/voice-private.html", None, Some("leak")),
    ] {
        assert!(!allowed(label, scheme, host, port, path, query, fragment),
            "unexpectedly allowed: {label}/{scheme}/{host:?}/{path}");
    }
}

/// NV19-08: entry points for `voice_context.rs` commands (the backend type stays private).
#[cfg(desktop)]
pub(crate) async fn context_manage(app: tauri::AppHandle, input: Value) -> CmdResult {
    imp::run(app, move |voice| voice.context_manage(&input)).await
}

#[cfg(desktop)]
pub(crate) async fn context_add_folder(app: tauri::AppHandle, path: std::path::PathBuf) -> CmdResult {
    imp::run(app, move |voice| voice.context_add_folder(&path)).await
}

#[cfg(mobile)]
fn desktop_only<T>(_: T) -> CmdResult {
    Err(serde_json::json!({"code": "voice_desktop_only", "message": "内置语音服务仅在 MOSS 桌面端可用"}))
}

#[cfg(desktop)]
mod imp {
    use std::net::UdpSocket;
    use std::sync::{Arc, Mutex};

    use moss_voice::builtin::{self, KeyChange};
    use moss_voice::local_models::{self, Engine, ModelKind, ModelManager};
    use moss_voice::settings::{BuiltinLocal, builtin_secret_name};
    use moss_voice::{CenterError, ModelsInput, ProviderKind, Section, SectionInput, VoiceMode};
    use moss_voice_providers::doubao::{DoubaoAsr, DoubaoTts};
    use moss_voice_providers::local::{
        AsrEngine, HELPER_FILE, LocalAsr, LocalHelper, LocalStreamingAsr, LocalTts, TtsEngine, VadEngine, VadParams,
    };
    use moss_voice_providers::openai::{OpenAiAsr, OpenAiLlm, OpenAiTts};
    use moss_voice_providers::{Asr, StreamingAsr, Tts};
    use moss_voice_server::{
        Conversation, ConversationSettings, DeviceRegistry, DeviceStatus, DeviceVisit, HostTools, LocalVadFactory, ServerConfig,
        ServerMode, ToolContext, ToolHost, ToolHostResult, VadFactory, VoiceServer,
    };
    use serde_json::Value;
    use tauri::Manager;

    use super::CmdResult;

    const TOKEN_SECRET_NAME: &str = "voice.device_token_secret";
    /// 流式合成时播放前攒的音频（批量合成沿用默认 600 ms）
    const STREAMING_PLAYBACK_START_MS: u32 = 300;

    fn now() -> f64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
    }

    fn db_error(error: impl std::fmt::Debug) -> String {
        format!("数据库操作失败：{error:?}")
    }

    fn internal(message: impl Into<String>) -> CenterError {
        CenterError::new("voice_internal", 500, message)
    }

    /// 密钥存 `app_secrets`、其余配置存 `settings.json`，两处无法共用一个事务：先在事务内写密钥，
    /// 再写设置；设置写入失败时按原值还原密钥，不留下“已开启但缺密钥”或“密钥已换、设置未变”的
    /// 半写状态（2026-10-13 审查，NV11-4）。调用方须持有数据库锁，避免校验与写入之间被并发保存改动。
    pub(super) fn commit_secrets_then_settings(
        db: &laser_runtime::db::RuntimeDb,
        changes: &[(String, Option<String>)],
        save_settings: impl FnOnce() -> Result<(), CenterError>,
    ) -> Result<(), CenterError> {
        let previous = db.apply_secret_changes(changes, now()).map_err(|error| internal(db_error(error)))?;
        let Err(error) = save_settings() else { return Ok(()) };
        if let Err(restore) = db.restore_secrets(&previous, now()) {
            return Err(internal(format!("{}；密钥还原也失败，请重新保存：{}", error.message, db_error(restore))));
        }
        Err(error)
    }

    #[cfg(test)]
    mod commit_tests {
        use super::{CenterError, commit_secrets_then_settings};
        use laser_runtime::db::RuntimeDb;

        #[test]
        fn settings_failure_restores_secrets() {
            let db = RuntimeDb::open_memory().unwrap();
            db.set_secret("voice.plugin.weather", "old", 1.).unwrap();
            let changes = vec![
                ("voice.plugin.weather".to_owned(), None),
                ("voice.plugin.rag".to_owned(), Some("new-rag".to_owned())),
            ];
            let error = commit_secrets_then_settings(&db, &changes, || {
                Err(CenterError::new("plugin_settings_failed", 500, "插件配置保存失败"))
            })
            .unwrap_err();
            assert_eq!(error.code, "plugin_settings_failed");
            assert_eq!(db.get_secret("voice.plugin.weather").unwrap().as_deref(), Some("old"));
            assert_eq!(db.get_secret("voice.plugin.rag").unwrap(), None);
        }

        #[test]
        fn settings_success_keeps_new_secrets() {
            let db = RuntimeDb::open_memory().unwrap();
            let changes = vec![("voice.vision.main".to_owned(), Some("key".to_owned()))];
            let mut saved = false;
            commit_secrets_then_settings(&db, &changes, || {
                saved = true;
                Ok(())
            })
            .unwrap();
            assert!(saved);
            assert_eq!(db.get_secret("voice.vision.main").unwrap().as_deref(), Some("key"));
        }
    }

    use moss_voice_server::memory::{MemoryRecord, MemoryService, VoiceMemoryStore};

    struct DbMemoryStore { db: Arc<Mutex<laser_runtime::RuntimeDb>> }
    /// `tests`：简易标定时每人另录的测试音向量，只在内存里，算完阈值或删除声纹即丢弃。
    struct DesktopSpeakerBackend {db:Arc<Mutex<laser_runtime::RuntimeDb>>,helper:Mutex<Option<moss_voice_providers::local::SpeakerHelper>>,personal:Mutex<Option<std::sync::Weak<moss_voice_server::person_context::PersonContext>>>,tests:Mutex<std::collections::HashMap<String,Vec<Vec<f32>>>>}
    impl moss_voice_server::speaker_enrollment::EnrollmentBackend for DesktopSpeakerBackend {
        fn save_test(&self,speaker_id:&str,clips:&[Vec<f32>])->Result<(),String>{
            let known=self.db.lock().map_err(|_|"数据库不可用")?.list_voice_speakers().map_err(|_|"读取声纹失败")?.iter().any(|r|r.id==speaker_id);
            if !known{return Err("这个人的声纹已删除".into());}
            self.tests.lock().map_err(|_|"标定数据不可用")?.insert(speaker_id.into(),clips.to_vec());
            Ok(())
        }
        fn embedding(&self,pcm:&[i16],generation:u64)->Result<Vec<f32>,String>{
            let helper=self.helper.lock().map_err(|_|"声纹辅助程序不可用")?.clone().ok_or("声纹辅助程序未准备")?;
            helper.embedding_blocking(pcm,16000,generation).map_err(|_|"声纹计算失败".into())
        }
        fn save(&self,name:&str,clips:&[Vec<f32>])->Result<(),String>{
            let mut bytes=[0u8;16];getrandom::fill(&mut bytes).map_err(|_|"声纹编号生成失败")?;
            let id=bytes.iter().map(|b|format!("{b:02x}")).collect::<String>();
            self.db.lock().map_err(|_|"数据库不可用")?.enroll_voice_speaker(&id,name,"campplus_sv_zh_cn_16k_common",clips,now()).map_err(|_|"声纹登记失败")?;
            if let Some(personal)=self.personal.lock().ok().and_then(|slot|slot.as_ref().and_then(std::sync::Weak::upgrade)){personal.directory_changed();}
            Ok(())
        }
    }
    use moss_voice_server::person_context::{IdentityResolver,IdentityRequest,PersonScope,PersonContext,PersonMemoryStore,PersonSummary,SpeakerTemplate,match_embedding,CalibrationReport,CalibrationSample};
    const SPEAKER_CALIBRATION_KEY:&str="voice.speaker.calibration";
    const SPEAKER_MODEL:&str="campplus_sv_zh_cn_16k_common";
    /// 简易标定结果（2026-10-14 用户决定，取代“源码写死 None”）：没有标定就不识别身份。
    /// 只对标定时的登记名单（编号 + 版本）有效；增删或重新登记任何人都会让它失效，需要重新标定。
    #[derive(Clone,serde::Serialize,serde::Deserialize)]
    struct Calibration {report:CalibrationReport,directory:String,calibrated_at:f64}
    /// 登记名单的比较键：CampPlus 192 维声纹的“编号:版本”，排序后拼接。
    fn speaker_directory<'a>(items:impl Iterator<Item=(&'a str,i64)>)->String{
        let mut keys=items.map(|(id,revision)|format!("{id}:{revision}")).collect::<Vec<_>>();keys.sort();keys.join(",")
    }
    #[cfg(test)]
    mod speaker_directory_tests {
        /// 标定只对当时的登记名单有效：顺序无关，增删人或重新登记（版本变）都会得到不同的键。
        #[test]
        fn directory_key_changes_when_anyone_is_added_removed_or_re_enrolled(){
            let key=|items:&[(&str,i64)]|super::speaker_directory(items.iter().copied());
            assert_eq!(key(&[("a",1),("b",1)]),key(&[("b",1),("a",1)]));
            assert_ne!(key(&[("a",1),("b",1)]),key(&[("a",1)]));
            assert_ne!(key(&[("a",1),("b",1)]),key(&[("a",1),("b",1),("c",1)]));
            assert_ne!(key(&[("a",1),("b",1)]),key(&[("a",2),("b",1)]));
        }
    }
    struct DesktopIdentity {host:std::sync::Weak<laser_host::HostApplication>,backend:Arc<DesktopSpeakerBackend>,calibration:Mutex<Option<Calibration>>,workers:Arc<tokio::sync::Semaphore>}
    impl DesktopIdentity {
        /// 有标定且登记名单没变时返回标定结果。
        fn valid_calibration(&self)->Option<Calibration>{
            let calibration=self.calibration.lock().ok()?.clone()?;
            let rows=self.backend.db.lock().ok()?.list_voice_speakers().ok()?;
            let current=speaker_directory(rows.iter().filter(|r|r.model_id==SPEAKER_MODEL&&r.dimension==192).map(|r|(r.id.as_str(),r.revision)));
            (current==calibration.directory).then_some(calibration)
        }
    }
    impl IdentityResolver for DesktopIdentity {
        fn identify<'a>(&'a self,_:&'a ToolContext,pcm:&'a [i16],rate:u32,request:IdentityRequest)->moss_voice_providers::BoxFuture<'a,Option<PersonScope>>{
            Box::pin(async move {
                let calibration=self.calibration.lock().ok()?.clone()?;
                let (threshold,margin,directory)=(calibration.report.threshold,calibration.report.margin,calibration.directory);
                // Keep the physical worker slot until a cancelled blocking
                // helper operation actually finishes; no unbounded queue.
                let worker=self.workers.clone().try_acquire_owned().ok()?;
                let backend=self.backend.clone();let pcm=pcm.to_vec();
                tauri::async_runtime::spawn_blocking(move||{
                    let _worker=worker;
                    let pcm=if rate==16000{pcm}else{moss_voice_providers::resample::Resampler::new(rate,16000).process(&pcm)};
                    let helper=backend.helper.lock().ok()?.clone()?;
                    let embedding=helper.embedding_blocking(&pcm,16000,request.utterance_generation).ok()?;
                    let templates=backend.db.lock().ok()?.native_voice_speaker_templates().ok()?.into_iter().map(|t|SpeakerTemplate{id:t.id,revision:t.revision,embedding:t.embedding}).collect::<Vec<_>>();
                    // 登记名单在标定后变过：阈值不再可信，不识别
                    if speaker_directory(templates.iter().map(|t|(t.id.as_str(),t.revision)))!=directory{return None;}
                    match_embedding(&embedding,&templates,request.directory_generation,threshold,margin)
                }).await.ok().flatten()
            })
        }
        fn permitted(&self,ctx:&ToolContext)->bool{self.host.upgrade().is_some_and(|h|h.api_service.voice_session_allowed(&ctx.device_id,&ctx.session_id))}
        fn device_allowed(&self,device:&str)->bool{self.device_generation(device).is_some()}
        fn device_generation(&self,device:&str)->Option<u64>{
            let host=self.host.upgrade()?;let db=host.db.lock().ok()?;
            let row=db.voice_device(device).ok()??;if !row.is_allowed(){return None;}Some(row.decided_at?.to_bits())
        }
        fn current(&self,scope:&PersonScope)->bool{
            self.backend.db.lock().ok().and_then(|db|db.list_voice_speakers().ok()).is_some_and(|rows|rows.iter().any(|r|r.id==scope.id()&&r.revision==scope.revision()&&r.model_id=="campplus_sv_zh_cn_16k_common"&&r.dimension==192))
        }
    }
    struct DbPersonStore {db:Arc<Mutex<laser_runtime::RuntimeDb>>}
    impl PersonMemoryStore for DbPersonStore {
        fn load(&self,scope:&PersonScope)->Result<Option<PersonSummary>,String>{
            let db=self.db.lock().map_err(|_|"数据库不可用")?;
            if !db.list_voice_speakers().map_err(db_error)?.iter().any(|r|r.id==scope.id()&&r.revision==scope.revision()){return Ok(None);}
            db.voice_person_summary(scope.id()).map(|r|r.map(|(text,revision)|PersonSummary{text,revision})).map_err(db_error)
        }
        fn update(&self,scope:&PersonScope,revision:i64,text:&str)->Result<bool,String>{
            let db=self.db.lock().map_err(|_|"数据库不可用")?;
            if !db.list_voice_speakers().map_err(db_error)?.iter().any(|r|r.id==scope.id()&&r.revision==scope.revision()){return Ok(false);}
            db.update_voice_person_summary_if_revision(scope.id(),revision,text,now()).map_err(db_error)
        }
    }
    impl VoiceMemoryStore for DbMemoryStore {
        fn allowed(&self, id: &str) -> Result<bool, String> {
            self.db.lock().map_err(|_| "数据库锁失败".to_owned())?.voice_device(id).map(|item| item.is_some_and(|d| d.is_allowed())).map_err(db_error)
        }
        fn load(&self, id: &str) -> Result<Option<MemoryRecord>, String> {
            self.db.lock().map_err(|_| "数据库锁失败".to_owned())?.voice_memory(id).map(|r| r.map(convert_memory)).map_err(db_error)
        }
        fn ensure(&self, id: &str) -> Result<MemoryRecord, String> {
            self.db.lock().map_err(|_| "数据库锁失败".to_owned())?.ensure_voice_memory(id, now()).map(convert_memory).map_err(db_error)
        }
        fn update(&self, id: &str, revision: i64, summary: &str) -> Result<Option<MemoryRecord>, String> {
            self.db.lock().map_err(|_| "数据库锁失败".to_owned())?.update_voice_memory_if_revision(id, revision, summary, now()).map(|r| r.map(convert_memory)).map_err(db_error)
        }
        fn list(&self) -> Result<Vec<MemoryRecord>, String> {
            self.db.lock().map_err(|_| "数据库锁失败".to_owned())?.voice_memories().map(|rows| rows.into_iter().map(convert_memory).collect()).map_err(db_error)
        }
    }
    fn convert_memory(value: laser_runtime::VoiceMemoryRecord) -> MemoryRecord {
        MemoryRecord { device_id: value.device_id, summary: value.summary, revision: value.revision, updated_at: value.updated_at }
    }

    use moss_voice_server::plugins::runtime::{CombinedTools, InfoTools, PluginHost};
    use moss_voice_server::ToolExecutor;
    use moss_voice_server::home_tools::{HomeAvailability, HomeHost, HomeTools};

    struct DesktopInfoHost { host: Arc<laser_host::HostApplication> }
    impl PluginHost for DesktopInfoHost {
        fn config(&self) -> Value {
            let path = self.host.runtime_root.path.join("settings.json");
            serde_json::to_value(moss_voice::settings::load(&path).builtin_plugins).unwrap_or(Value::Null)
        }
        fn secret(&self, kind: &str) -> Option<String> {
            let name = match kind { "weather" => "voice.plugin.weather", "search" => "voice.plugin.search", "metaso" => "voice.plugin.metaso", "rag" => "voice.plugin.rag", _ => return None };
            self.host.db.lock().ok().and_then(|db|db.get_secret(name).ok().flatten()).filter(|s|!s.is_empty())
        }
        fn permitted(&self, ctx: &ToolContext) -> bool {
            self.host.api_service.voice_session_allowed(&ctx.device_id,&ctx.session_id)
        }
    }

    // NV15 opt-in *read-only* model tool. Never expose the token or POST.
    // A saved grant binds the exact allowlist revision and token revision,
    // so editing either disables the tool until the desktop user re-consents.
    const HOME_VOICE_GRANT:&str="voice.homeassistant.voice_read_consent";
    struct DesktopHomeHost { host:Arc<laser_host::HostApplication>, control:Arc<laser_host::home_control::HomeControlCoordinator> }
    impl DesktopHomeHost {
        fn current(&self)->Option<(moss_voice::settings::BuiltinHomeAssistant,u64,u64,String)>{
            let (settings,policy)=moss_voice::settings::read_home_offline(
                &self.host.runtime_root.path.join("settings.json")).ok()?;
            settings.validate_offline().ok()?;
            if policy==0 || settings.base_url.is_empty() ||
                !settings.entities.iter().any(|e|e.enabled) {return None;}
            let db=self.host.db.lock().ok()?;
            let (has_token,revision)=db.home_token_status().ok()?;
            if !has_token || revision==0 {return None;}
            let grant=db.get_secret(HOME_VOICE_GRANT).ok()??;
            if grant!=format!("{policy}:{revision}") {return None;}
            Some((settings,policy,revision,grant))
        }
    }
    impl HomeHost for DesktopHomeHost {
        fn availability(&self)->HomeAvailability{
            HomeAvailability{read:self.current().is_some(),control:self.control.enabled()}
        }
        fn permitted(&self,context:&ToolContext)->bool{
            self.host.api_service.voice_session_allowed(&context.device_id,&context.session_id)
        }
        fn invoke(&self,context:&ToolContext,name:&str,arguments:&Value)->Value{
            let deny=|code:&str|serde_json::json!({"ok":false,"error":{"code":code,"retryable":false}});
            if !self.permitted(context) {return deny("home_disabled");}
            if name!="hass_get_state" {
                return self.control.prepare(&context.device_id,&context.session_id,name,arguments).unwrap_or_else(|code|deny(code));
            }
            let Some((config,policy,revision,_))=self.current()else{return deny("home_disabled")};
            let target=arguments["entity"].as_str().unwrap_or("");
            if !config.entities.iter().any(|e|e.enabled&&e.entity_id==target) {
                return deny("home_unknown_entity");
            }
            let token={
                let Ok(db)=self.host.db.lock()else{return deny("home_unavailable")};
                match db.home_token_for_connection(){Ok(Some(token))=>token,_=>return deny("home_unavailable")}
            };
            let discovered=moss_voice::home_readonly::discover_guarded(&config,&token,policy,revision,||{
                self.permitted(context) && self.current()
                    .is_some_and(|(latest,p,r,_)|latest==config&&p==policy&&r==revision)
            });
            if !self.permitted(context) || self.current()
                .is_none_or(|(new,p,r,_)|new!=config||p!=policy||r!=revision){
                return deny("home_stale_permission");
            }
            match discovered {
                Ok(output)=>match output.entities.iter().find(|e|e.entity_id==target) {
                    Some(entity)=>serde_json::json!({"ok":true,"entity_id":entity.entity_id,
                        "state":entity.state,"last_updated":entity.last_updated,
                        "attributes":entity.attributes,"actions_available":entity.actions_available}),
                    None=>deny("home_unknown_entity"),
                },
                Err(moss_voice::home_readonly::DiscoveryError::Unauthorized)=>deny("home_unauthorized"),
                Err(_)=>deny("home_unavailable"),
            }
        }
    }

    /// NV12: fresh signed-in device permission and fresh user configuration are
    /// consulted for every image; snapshots are never logged or stored as conversation history.
    struct DesktopVisionFactory { host: Arc<laser_host::HostApplication> }
    impl DesktopVisionFactory {
        fn profile(&self) -> Option<(moss_voice::settings::BuiltinVision, String, u64)> {
            use std::hash::{Hash,Hasher};
            let setting=moss_voice::settings::load(&self.host.runtime_root.path.join("settings.json")).builtin_vision;
            if !setting.enabled || setting.model.trim().is_empty()
                || moss_voice_providers::vision::endpoint(&setting.url).is_err() {return None;}
            let key=self.host.db.lock().ok()?.get_secret("voice.vision.main").ok()??;
            if key.trim().is_empty(){return None;}
            let mut hasher=std::collections::hash_map::DefaultHasher::new();
            setting.url.hash(&mut hasher);
            setting.model.hash(&mut hasher);
            setting.enabled.hash(&mut hasher);
            key.hash(&mut hasher);
            let revision=hasher.finish();
            Some((setting,key,revision))
        }
    }
    impl moss_voice_server::vision::VisionFactory for DesktopVisionFactory {
        fn ready(&self)->bool {self.profile().is_some()}
        fn open(&self)->Option<moss_voice_server::vision::VisionSession>{
            let (setting,key,generation)=self.profile()?;
            let provider=moss_voice_providers::vision::OpenAiVision::new(&setting.url,&setting.model,&key).ok()?;
            Some(moss_voice_server::vision::VisionSession{provider:Arc::new(provider),generation})
        }
        fn still_current(&self,generation:u64)->bool{
            self.profile().is_some_and(|(_,_,revision)|revision==generation)
        }
    }

    /// 设备登记与密钥：存 MOSS 的 SQLite。
    struct DbRegistry {
        db: Arc<Mutex<laser_runtime::RuntimeDb>>,
        host: Arc<laser_host::HostApplication>,
    }

    impl DeviceRegistry for DbRegistry {
        fn seen(&self, visit: &DeviceVisit) -> Result<DeviceStatus, String> {
            let db = self.db.lock().map_err(|_| "数据库锁失败".to_owned())?;
            let record = db
                .voice_device_seen(&visit.device_id, &visit.client_id, &visit.model, &visit.firmware_version, &visit.ip, now())
                .map_err(db_error)?;
            drop(db);
            if !record.is_allowed() { self.host.api_service.revoke_voice_sessions(&visit.device_id); }
            Ok(match record.status.as_str() {
                laser_runtime::VOICE_DEVICE_ALLOWED => DeviceStatus::Allowed,
                laser_runtime::VOICE_DEVICE_REVOKED => DeviceStatus::Revoked,
                _ => DeviceStatus::Pending,
            })
        }

        fn authorize_http(&self, device: &str, client: &str) -> Result<Option<u64>, String> {
            let db=self.db.lock().map_err(|_| "数据库锁失败".to_string())?;
            db.voice_device(device).map(|row|row.and_then(|row|{
                (row.is_allowed() && row.client_id==client).then_some(row.decided_at?.to_bits())
            })).map_err(db_error)
        }
        fn open_session(&self, device: &str, client: &str, session: &str) -> Result<Arc<std::sync::atomic::AtomicBool>, String> {
            self.host.api_service.register_voice_session(device, client, session)
        }
        fn close_session(&self, session: &str) { self.host.api_service.unregister_voice_session(session); }

        fn token_secret(&self) -> Result<Vec<u8>, String> {
            let db = self.db.lock().map_err(|_| "数据库锁失败".to_owned())?;
            if let Some(secret) = db.get_secret(TOKEN_SECRET_NAME).map_err(db_error)? {
                if secret.len() >= 32 {
                    return Ok(secret.into_bytes());
                }
            }
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|error| format!("生成设备密钥失败：{error}"))?;
            let secret: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            db.set_secret(TOKEN_SECRET_NAME, &secret, now()).map_err(db_error)?;
            Ok(secret.into_bytes())
        }

        fn sessions_changed(&self, count: usize) {
            laser_host::set_voice_device_sessions(Some(u32::try_from(count).unwrap_or(u32::MAX)));
        }
    }

    /// 对话中调用 MOSS 工具（NV4）：身份 `CallerContext::Voice`；运动与出光跟随严格模式开关，桌面控制类工具始终拒绝。
    struct DesktopToolHost {
        host: Arc<laser_host::HostApplication>,
    }

    impl ToolHost for DesktopToolHost {
        fn list(&self) -> Value {
            let mut base=laser_host::voice_tools_list(&self.host.api_service);
            let extra=self.host.api_service.agent_tools_list();
            if let (Some(base_tools),Some(agent_tools))=(base["tools"].as_array_mut(),extra["tools"].as_array()) {
                base_tools.extend(agent_tools.iter().cloned());
            }
            base
        }

        fn call(&self, context: &ToolContext, name: &str, arguments: &Value) -> ToolHostResult {
            let result=if matches!(name,"agent_codex"|"agent_claude_code") {
                let caller=laser_contracts::CallerContext::Voice {device_id:context.device_id.clone(),session_id:context.session_id.clone()};
                self.host.api_service.call_agent_tool(&caller,name,arguments)
            } else {
                laser_host::voice_tool_call(&self.host.api_service, &context.device_id, &context.session_id, name, arguments)
            };
            ToolHostResult { success: result.success, result: result.result, detail: result.detail }
        }
    }

    pub struct BuiltinVoice {
        host: Arc<laser_host::HostApplication>,
        server: Mutex<Option<VoiceServer>>,
        memory: Arc<MemoryService>,
        speaker:Arc<moss_voice_server::speaker_enrollment::SpeakerEnrollment>,
        speaker_backend:Arc<DesktopSpeakerBackend>,
        personal:Arc<PersonContext>,
        identity:Arc<DesktopIdentity>,
        home_control:Arc<laser_host::home_control::HomeControlCoordinator>,
        /// 最近一次自动启动失败的原因（语音链路为内置服务时随 MOSS 启动）
        last_error: Mutex<Option<String>>,
        /// 本地模型下载与安装（运行目录 `models/`）
        models: ModelManager,
        /// 运行中的本地模型辅助程序（随内置服务启停）
        helper: Mutex<Option<LocalHelper>>,
        /// NV19 长期记忆、资料与恢复摘要；检索与提取默认关闭
        context: Arc<crate::voice_context::host::ContextHost>,
    }

    /// 本次启动用到的本地模型。
    #[derive(Default)]
    struct LocalParts {
        asr: Option<LocalAsr>,
        /// 选用流式识别模型时：边说边识别（整段识别 `asr` 作为它失败时的退路）
        streaming_asr: Option<Arc<dyn StreamingAsr>>,
        tts: Option<LocalTts>,
        vad: Option<Arc<dyn VadFactory>>,
    }

    /// 辅助程序与 MOSS 放在同一目录。
    fn helper_path() -> Option<std::path::PathBuf> {
        std::env::current_exe().ok()?.parent().map(|dir| dir.join(HELPER_FILE))
    }

    fn start_error(message: impl Into<String>) -> CenterError {
        CenterError::new("builtin_start_failed", 409, message)
    }

    /// Tauri 托管状态：后端未启动时为空。
    pub struct BuiltinState(pub Option<Arc<BuiltinVoice>>);

    pub fn init(host: Option<Arc<laser_host::HostApplication>>) -> BuiltinState {
        BuiltinState(host.map(|host| {
            let setting_path=host.runtime_root.path.join("settings.json");
            if let Err(error)=laser_host::voice_agent::AgentCoordinator::attach(
                &host.api_service,host.db.clone(),setting_path.clone(),vec![host.runtime_root.path.clone()]
            ) {
                eprintln!("[moss-desktop] AgentRun 只读工具不可用：{error}");
            }
            let models = ModelManager::new(host.runtime_root.path.join("models"));
            let memory = MemoryService::new(Arc::new(DbMemoryStore { db: host.db.clone() }));
            let enabled = moss_voice::settings::load(&host.runtime_root.path.join("settings.json")).builtin_memory_enabled;
            let _ = memory.configure(enabled, || Ok(()));
            let home_control=laser_host::home_control::HomeControlCoordinator::attach(&host);
            let speaker_backend=Arc::new(DesktopSpeakerBackend{db:host.db.clone(),helper:Mutex::new(None),personal:Mutex::new(None),tests:Mutex::new(Default::default())});
            let speaker=moss_voice_server::speaker_enrollment::SpeakerEnrollment::new(speaker_backend.clone());
            let saved_calibration=host.db.lock().ok().and_then(|db|db.get_secret(SPEAKER_CALIBRATION_KEY).ok().flatten()).and_then(|text|serde_json::from_str::<Calibration>(&text).ok());
            let identity=Arc::new(DesktopIdentity{host:Arc::downgrade(&host),backend:speaker_backend.clone(),calibration:Mutex::new(saved_calibration),workers:Arc::new(tokio::sync::Semaphore::new(2))});
            let personal=PersonContext::new(identity.clone(),Arc::new(DbPersonStore{db:host.db.clone()}));
            if let Ok(mut slot)=speaker_backend.personal.lock(){*slot=Some(Arc::downgrade(&personal));}
            if let Ok(db)=host.db.lock(){if db.recover_firmware_downloads(now()).is_err(){eprintln!("[moss-desktop] 固件下载记录恢复失败，固件传输仍禁用");}}
            let context = crate::voice_context::host::ContextHost::new(host.db.clone(), setting_path.clone());
            Arc::new(BuiltinVoice { host, memory, speaker,speaker_backend,personal,identity,home_control, server: Mutex::new(None), last_error: Mutex::new(None), models, helper: Mutex::new(None), context })
        }))
    }

    /// 语音链路为“内置服务”时，在后台启动内置语音服务；失败原因显示在内置服务卡片上。
    pub fn autostart(state: &BuiltinState) {
        let Some(builtin) = state.0.clone() else { return };
        if builtin.settings().mode != VoiceMode::Builtin {
            return;
        }
        let _ = std::thread::Builder::new().name("voice-builtin-autostart".into()).spawn(move || {
            if let Err(error) = builtin.start() {
                eprintln!("[moss-desktop] 内置语音服务自动启动失败：{}", error.message);
                if let Ok(mut last) = builtin.last_error.lock() {
                    *last = Some(error.message);
                }
            }
        });
    }

    /// 本机局域网地址（只用于选路，不发送数据），用于告诉用户设备 OTA 要填的地址。
    fn local_ip() -> Option<String> {
        let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("192.0.2.1:80").ok()?;
        let ip = socket.local_addr().ok()?.ip();
        (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
    }

    impl BuiltinVoice {
        fn prepare_speaker_helper(&self)->Result<(),CenterError>{
            let mut helper=self.speaker_backend.helper.lock().map_err(|_|internal("声纹辅助程序锁失败"))?;
            if helper.as_ref().is_none_or(|h|!h.is_alive()){
                let exe=helper_path().ok_or_else(||CenterError::new("speaker_helper_missing",409,"请先准备声纹辅助程序"))?;
                *helper=Some(moss_voice_providers::local::SpeakerHelper::spawn(&exe,self.models.root()).map_err(|_|CenterError::new("speaker_model_missing",409,"声纹模型未安装或校验失败"))?);
            }
            Ok(())
        }
        fn hotwords(&self, local: &BuiltinLocal) -> Vec<String> {
            let materials: Vec<String> = self.host.db.lock().ok().and_then(|db| db.export_material_library().ok())
                .map(|library| library.materials.into_keys().collect()).unwrap_or_default();
            moss_voice::settings::effective_hotwords(&local.custom_hotwords, &materials)
        }
        fn settings_file(&self) -> std::path::PathBuf {
            self.host.runtime_root.path.join("settings.json")
        }

        fn settings(&self) -> moss_voice::VoiceSettings {
            moss_voice::settings::load(&self.settings_file())
        }

        fn secret(&self, section: Section) -> Result<Option<String>, CenterError> {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            db.get_secret(builtin_secret_name(section)).map_err(|error| internal(db_error(error)))
        }

        fn devices(&self) -> Value {
            self.host
                .db
                .lock()
                .ok()
                .and_then(|db| db.voice_devices().ok())
                .map(|devices| serde_json::to_value(devices).unwrap_or_default())
                .unwrap_or_else(|| Value::Array(Vec::new()))
        }

        /// 按开关启动辅助程序并加载选用且已安装的本地模型。没有打开任何本地开关（或模型未安装）时不启动辅助程序。
        fn local_parts(&self, local: &BuiltinLocal) -> Result<(LocalParts, Option<LocalHelper>), CenterError> {
            let pick = |kind| {
                local.selected(kind).and_then(|spec| self.models.path_if_installed(spec.id).map(|path| (spec, path)))
            };
            let (vad, asr, tts) = (pick(ModelKind::Vad), pick(ModelKind::Asr), pick(ModelKind::Tts));
            if vad.is_none() && asr.is_none() && tts.is_none() {
                return Ok((LocalParts::default(), None));
            }
            let exe = helper_path().ok_or_else(|| start_error("无法确定 MOSS 程序所在目录"))?;
            let helper = LocalHelper::spawn(&exe).map_err(|error| start_error(error.0))?;
            let mut parts = LocalParts::default();
            if let Some((spec, model)) = vad {
                let engine = if spec.engine == Engine::TenVad { VadEngine::Ten } else { VadEngine::Silero };
                helper
                    .load_vad(&model, VadParams::for_engine(engine), engine)
                    .map_err(|error| start_error(format!("本地判停（{}）：{}", spec.label, error.0)))?;
                parts.vad = Some(Arc::new(LocalVadFactory { helper: helper.clone() }));
            }
            if let Some((spec, dir)) = asr {
                let engine =
                    if spec.engine == Engine::StreamingTransducer { AsrEngine::StreamingTransducer } else { AsrEngine::SenseVoice };
                helper.load_asr_with_options(&dir, engine, moss_voice_providers::local::AsrDecodeOptions { beam_search: local.asr_beam_search, hotwords: self.hotwords(local) })
                    .map_err(|error| start_error(format!("本地识别（{}）：{}", spec.label, error.0)))?;
                parts.asr = Some(LocalAsr { helper: helper.clone() });
                if engine.streaming() {
                    parts.streaming_asr = Some(Arc::new(LocalStreamingAsr { helper: helper.clone() }));
                }
            }
            if let Some((spec, dir)) = tts {
                let engine = if spec.engine == Engine::Matcha { TtsEngine::Matcha } else { TtsEngine::Kokoro };
                let info =
                    helper.load_tts(&dir, engine).map_err(|error| start_error(format!("本地合成（{}）：{}", spec.label, error.0)))?;
                // 单音色模型忽略音色编号
                let speaker = if info.speakers > 1 {
                    if local.tts_speaker < 0 || local.tts_speaker as u32 >= info.speakers {
                        return Err(start_error(format!("{} 的音色编号须在 0 ~ {} 之间", spec.label, info.speakers - 1)));
                    }
                    local.tts_speaker
                } else {
                    0
                };
                parts.tts = Some(LocalTts { helper: helper.clone(), speaker, speed: local.tts_speed });
            }
            Ok((parts, Some(helper)))
        }

        /// 三个分区都可用时构造对话（本地识别 / 合成优先于云端）；否则返回 `None`（回声模式）。
        /// 第二项为合成是否流式（豆包），用于缩短播放启动缓冲。
        fn conversation(
            &self,
            models: &moss_voice::settings::BuiltinModels,
            local: &mut LocalParts,
            experience: &BuiltinLocal,
        ) -> Result<Option<(Conversation, bool)>, CenterError> {
            let Some(llm) = &models.llm else { return Ok(None) };
            let key = |section| self.secret(section).map(Option::unwrap_or_default);
            let invalid = |error: moss_voice_providers::ProviderError| CenterError::new("builtin_models_invalid", 400, error.0);
            let hotwords = self.hotwords(experience);
            let mut streaming_asr: Option<Arc<dyn StreamingAsr>> = None;
            let asr: Arc<dyn Asr> = match (local.asr.take(), &models.asr) {
                (Some(local_asr), _) => {
                    streaming_asr = local.streaming_asr.take();
                    Arc::new(local_asr)
                }
                (None, Some(cloud)) if cloud.kind == ProviderKind::Doubao => {
                    let doubao = Arc::new(DoubaoAsr::new(&cloud.url, &key(Section::Asr)?, &cloud.model).map_err(invalid)?.with_hotwords(&hotwords));
                    streaming_asr = Some(doubao.clone());
                    doubao
                }
                (None, Some(cloud)) => Arc::new(OpenAiAsr::new(&cloud.url, &key(Section::Asr)?, &cloud.model, None).map_err(invalid)?.with_hotwords(&hotwords)),
                (None, None) => return Ok(None),
            };
            let (tts, streaming_tts): (Arc<dyn Tts>, bool) = match (local.tts.take(), &models.tts) {
                (Some(local), _) => (Arc::new(local), true),
                (None, Some(cloud)) if cloud.kind == ProviderKind::Doubao => {
                    let doubao = DoubaoTts::new(&cloud.url, &key(Section::Tts)?, &cloud.model, &cloud.voice, cloud.speed);
                    (Arc::new(doubao.map_err(invalid)?), true)
                }
                (None, Some(cloud)) => {
                    let openai = OpenAiTts::new(&cloud.url, &key(Section::Tts)?, &cloud.model, &cloud.voice, cloud.speed);
                    (Arc::new(openai.map_err(invalid)?), false)
                }
                (None, None) => return Ok(None),
            };
            let labels = vec![format!("LLM:{}", llm.model),
                format!("ASR:{}", if experience.asr { experience.asr_model.as_str() } else { models.asr.as_ref().map(|v| v.model.as_str()).unwrap_or("unknown") }),
                format!("TTS:{}", if experience.tts { experience.tts_model.as_str() } else { models.tts.as_ref().map(|v| v.model.as_str()).unwrap_or("unknown") })];
            let llm = OpenAiLlm::new(&llm.url, &key(Section::Llm)?, &llm.model).map_err(invalid)?;
            let original: Arc<dyn ToolExecutor> = Arc::new(HostTools::new(Arc::new(DesktopToolHost { host: self.host.clone() })));
            let plugin_host = Arc::new(DesktopInfoHost { host: self.host.clone() });
            let info = InfoTools::new(plugin_host).map_err(|_| internal("信息插件 HTTP 客户端初始化失败"))?;
            let combined: Arc<dyn ToolExecutor> = Arc::new(CombinedTools { original, info: Arc::new(info) });
            let tools: Arc<dyn ToolExecutor> = Arc::new(HomeTools {
                original:combined, home:Arc::new(DesktopHomeHost{host:self.host.clone(),control:self.home_control.clone()})
            });
            let mut conversation = Conversation::new(asr, Arc::new(llm), tts, tools, ConversationSettings { read_only_shortcuts: experience.read_only_shortcuts, model_labels: labels, ..Default::default() }).with_memory(self.memory.clone()).with_speaker_enrollment(self.speaker.clone());
            // NV19: both hooks stay inert until the user switches them on in the private window.
            let (retrieval, extraction) = self.context.hooks();
            conversation = conversation.with_context_retrieval(retrieval).with_context_extraction(extraction);
            if self.identity.valid_calibration().is_some() && self.settings().builtin_person_memory_enabled {
                self.prepare_speaker_helper()?;
                conversation=conversation.with_personal_context(self.personal.clone());
            }
            if let Some(streaming) = streaming_asr {
                conversation = conversation.with_streaming_asr(streaming);
            }
            Ok(Some((conversation, streaming_tts)))
        }

        pub fn status(&self) -> Value {
            let settings = self.settings();
            let guard = self.server.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let view = guard.as_ref().map(VoiceServer::status);
            drop(guard);
            let (ws_port, http_port) = view
                .as_ref()
                .map(|view| (view.ws_port, view.http_port))
                .unwrap_or((settings.builtin_ws_port, settings.builtin_http_port));
            // 运行中显示实际模式；未运行时显示下次启动的模式
            let mode = match &view {
                Some(view) => serde_json::to_value(view.mode).unwrap_or_default(),
                None => Value::String(if settings.builtin_chat_ready(self.models.root()) { "chat" } else { "echo" }.into()),
            };
            let db = self.host.db.lock().ok();
            let key_set = |name: &str| {
                db.as_ref().and_then(|db| db.get_secret(name).ok().flatten()).is_some_and(|value| !value.is_empty())
            };
            let models = builtin::status_view(&settings.builtin_models, key_set);
            drop(db);
            serde_json::json!({
                "running": view.is_some(),
                "mode": mode,
                "link_mode": settings.mode.as_str(),
                "ws_port": ws_port,
                "http_port": http_port,
                "ota_url": local_ip().map(|ip| format!("http://{ip}:{http_port}/xiaozhi/ota/")),
                "models": models,
                "last_error": self.last_error.lock().ok().and_then(|last| last.clone()),
                "local": {
                    "settings": settings.builtin_local,
                    "default_mirror": local_models::DEFAULT_BASE,
                    "helper_found": helper_path().is_some_and(|path| path.is_file()),
                    "helper_running": self.helper.lock().ok().and_then(|helper| helper.as_ref().map(LocalHelper::is_alive)).unwrap_or(false),
                    "models": self.models.status(),
                    "ready": {
                        "asr": settings.builtin_section_ready(Section::Asr, self.models.root()),
                        "tts": settings.builtin_section_ready(Section::Tts, self.models.root()),
                    },
                },
                "sessions": view.as_ref().map(|view| serde_json::to_value(&view.sessions).unwrap_or_default()).unwrap_or_else(|| Value::Array(Vec::new())),
                "events": view.as_ref().map(|view| serde_json::to_value(&view.events).unwrap_or_default()).unwrap_or_else(|| Value::Array(Vec::new())),
                "timings": view.as_ref().map(|view| serde_json::to_value(&view.timings).unwrap_or_default()).unwrap_or_else(|| Value::Array(Vec::new())),
                "memory_enabled": self.memory.enabled(),
                "devices": self.devices(),
            })
        }

        pub fn start(&self) -> Result<Value, CenterError> {
            {
                let mut guard = self.server.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if guard.is_none() {
                    let settings = self.settings();
                    let (mut local, helper) = self.local_parts(&settings.builtin_local)?;
                    let mut conversation = self.conversation(&settings.builtin_models, &mut local, &settings.builtin_local)?;
                    // No music scanning while disabled or chat unavailable.
                    let music = if settings.builtin_music.enabled && conversation.is_some() {
                        let index = Arc::new(moss_voice_server::music::MusicIndex::scan(
                            std::path::Path::new(&settings.builtin_music.directory)
                        ).map_err(|_| CenterError::new("music_invalid",400,"音乐目录不安全、不可读取或数量超出限制"))?);
                        if let Some((chat,_)) = conversation.as_mut() {
                            chat.tools = Arc::new(moss_voice_server::music::MusicTools::new(chat.tools.clone(),index.clone()));
                        }
                        Some(index)
                    } else {None};
                    let defaults = ServerConfig::default();
                    let playback_start_ms = match &conversation {
                        Some((_, true)) => STREAMING_PLAYBACK_START_MS,
                        _ => defaults.playback_start_ms,
                    };
                    let config = ServerConfig {
                        ws_port: settings.builtin_ws_port,
                        http_port: settings.builtin_http_port,
                        mode: if conversation.is_some() { ServerMode::Chat } else { ServerMode::Echo },
                        conversation: conversation.map(|(conversation, _)| Arc::new(conversation)),
                        music,
                        vision: Some(Arc::new(DesktopVisionFactory { host: self.host.clone() })),
                        vad_factory: local.vad.take(),
                        barge_in: settings.builtin_local.barge_in,
                        barge_in_ms: settings.builtin_local.barge_in_ms,
                        playback_start_ms,
                        ..defaults
                    };
                    let registry = Arc::new(DbRegistry { db: self.host.db.clone(), host: self.host.clone() });
                    let server = VoiceServer::start(config, registry)
                        .map_err(|message| CenterError::new("builtin_start_failed", 409, message))?;
                    *guard = Some(server);
                    if let Ok(mut slot) = self.helper.lock() {
                        *slot = helper;
                    }
                    // 顶栏：服务已运行、还没有设备连接
                    laser_host::set_voice_device_sessions(Some(0));
                    if let Ok(mut last) = self.last_error.lock() {
                        *last = None;
                    }
                }
            }
            Ok(self.status())
        }

        pub fn stop(&self) -> Value {
            self.personal.directory_changed();
            let _=self.speaker.cancel();
            if let Ok(mut helper)=self.speaker_backend.helper.lock(){*helper=None;}
            let server = self.server.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
            if let Some(server) = server {
                server.stop();
                laser_host::set_voice_device_sessions(None);
            }
            // 结束辅助程序（最后一个句柄丢弃时结束进程）
            if let Ok(mut helper) = self.helper.lock() {
                *helper = None;
            }
            self.status()
        }

        fn restart_if_running(&self) -> Result<bool, CenterError> {
            if !self.running() {
                return Ok(false);
            }
            self.stop();
            self.start()?;
            Ok(true)
        }

        /// 保存本地模型开关、音色、语速与镜像；运行中则重启服务以生效。
        pub fn local_save(&self, patch: &Value) -> Result<Value, CenterError> {
            let invalid = |message: &str| CenterError::new("voice_settings_invalid", 400, message);
            let mut settings = self.settings();
            let local = &mut settings.builtin_local;
            for (key, slot) in [("asr", &mut local.asr), ("tts", &mut local.tts), ("vad", &mut local.vad), ("read_only_shortcuts", &mut local.read_only_shortcuts), ("asr_beam_search", &mut local.asr_beam_search), ("barge_in", &mut local.barge_in)] {
                if let Some(value) = patch.get(key) {
                    *slot = value.as_bool().ok_or_else(|| invalid("开关取值不正确"))?;
                }
            }
            if let Some(value) = patch.get("barge_in_ms") {
                let ms = value.as_u64().ok_or_else(|| invalid("插话时长须为 300 ~ 600 毫秒的整数"))?;
                local.barge_in_ms = moss_voice::settings::validate_barge_in_ms(ms).map_err(|message| invalid(&message))?;
            }
            if let Some(value) = patch.get("custom_hotwords") {
                let words: Vec<String> = serde_json::from_value(value.clone()).map_err(|_| invalid("热词须为文字数组"))?;
                local.custom_hotwords = moss_voice::settings::normalize_hotwords(&words).map_err(|message| invalid(&message))?;
            }
            for (key, kind, slot) in [
                ("vad_model", ModelKind::Vad, &mut local.vad_model),
                ("asr_model", ModelKind::Asr, &mut local.asr_model),
                ("tts_model", ModelKind::Tts, &mut local.tts_model),
            ] {
                if let Some(value) = patch.get(key) {
                    let id = value.as_str().filter(|id| local_models::spec_of(id, kind).is_some());
                    *slot = id.ok_or_else(|| invalid("选择的本地模型不存在"))?.to_owned();
                }
            }
            if let Some(value) = patch.get("tts_speaker") {
                local.tts_speaker =
                    value.as_i64().filter(|value| (0..=999).contains(value)).ok_or_else(|| invalid("音色编号须在 0 ~ 999 之间"))? as i32;
            }
            if let Some(value) = patch.get("tts_speed") {
                let speed = value.as_f64().filter(|value| (0.5..=2.0).contains(value)).ok_or_else(|| invalid("本地合成语速须在 0.5 ~ 2.0 之间"))?;
                local.tts_speed = speed as f32;
            }
            if let Some(value) = patch.get("mirror") {
                let mirror = value.as_str().map(str::trim).unwrap_or_default();
                local.mirror = if mirror.is_empty() {
                    None
                } else if (mirror.starts_with("https://") || mirror.starts_with("http://")) && !mirror.contains(char::is_whitespace) {
                    Some(mirror.trim_end_matches('/').to_owned())
                } else {
                    return Err(invalid("下载地址必须以 http:// 或 https:// 开头"));
                };
            }
            moss_voice::settings::save(&self.settings_file(), &settings)
                .map_err(|message| CenterError::new("voice_settings_write_failed", 500, message))?;
            let restarted = self.restart_if_running()?;
            let mut status = self.status();
            status["restarted"] = Value::Bool(restarted);
            Ok(status)
        }

        pub fn model_download(&self, id: &str) -> Result<Value, CenterError> {
            let mirror = self.settings().builtin_local.mirror;
            self.models
                .download(id, mirror.as_deref().unwrap_or(local_models::DEFAULT_BASE))
                .map_err(|message| CenterError::new("local_model_download_failed", 409, message))?;
            Ok(self.status())
        }

        pub fn model_cancel(&self, id: &str) -> Value {
            self.models.cancel(id);
            self.status()
        }

        pub fn model_delete(&self, id: &str) -> Result<Value, CenterError> {
            if id=="campplus_sv_zh_cn_16k_common" && self.speaker_backend.helper.lock().map(|h|h.is_some()).unwrap_or(true) {
                return Err(CenterError::new("local_model_in_use",409,"声纹模型已加载，请先停止内置服务"));
            }
            if self.running() && self.helper.lock().map(|helper| helper.is_some()).unwrap_or(false) {
                return Err(CenterError::new("local_model_in_use", 409, "内置服务正在使用本地模型，请先停止内置服务再删除"));
            }
            self.models.delete(id).map_err(|message| CenterError::new("local_model_delete_failed", 500, message))?;
            Ok(self.status())
        }

        fn running(&self) -> bool {
            self.server.lock().map(|guard| guard.is_some()).unwrap_or(false)
        }

        /// 测试一个分区的连接：密钥留空时用已保存的密钥。结果不含密钥。
        pub fn models_test(&self, section: Section, input: &SectionInput) -> Result<Value, CenterError> {
            let stored = self.secret(section)?;
            let resolved = builtin::resolve_for_test(section, input, stored)
                .map_err(|message| CenterError::new("model_input_invalid", 400, message))?;
            let result = match resolved.kind {
                ProviderKind::Openai => moss_voice::models::test_section(&resolved),
                ProviderKind::Doubao => test_doubao(&resolved),
            };
            serde_json::to_value(result).map_err(|error| internal(error.to_string()))
        }

        /// 保存模型设置（地址等写 settings.json，密钥写 app_secrets）；运行中则重启服务以生效。
        pub fn models_save(&self, input: &ModelsInput, clear: &[Section]) -> Result<Value, CenterError> {
            let mut settings = self.settings();
            let plan = builtin::plan_save(&settings.builtin_models, input, clear)
                .map_err(|message| CenterError::new("model_input_invalid", 400, message))?;
            let changes: Vec<(String, Option<String>)> = plan
                .keys
                .iter()
                .filter_map(|(section, change)| {
                    let name = builtin_secret_name(*section).to_owned();
                    match change {
                        KeyChange::Keep => None,
                        KeyChange::Set(value) => Some((name, Some(value.clone()))),
                        KeyChange::Clear => Some((name, None)),
                    }
                })
                .collect();
            settings.builtin_models = plan.models;
            {
                let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
                commit_secrets_then_settings(&db, &changes, || {
                    moss_voice::settings::save(&self.settings_file(), &settings)
                        .map_err(|message| CenterError::new("voice_settings_write_failed", 500, message))
                })?;
            }
            let restarted = self.running();
            if restarted {
                self.stop();
                self.start()?;
            }
            let mut status = self.status();
            status["restarted"] = Value::Bool(restarted);
            Ok(status)
        }

        pub fn set_device_status(&self, device_id: &str, status: &str) -> Result<Value, CenterError> {
            let status = match status.trim() {
                "allowed" => laser_runtime::VOICE_DEVICE_ALLOWED,
                "revoked" => laser_runtime::VOICE_DEVICE_REVOKED,
                _ => return Err(CenterError::new("voice_device_invalid", 400, "设备状态只能是 allowed 或 revoked")),
            };
            let found = self.personal.device_changed(device_id,||self.memory.device_changed(device_id, || self.host.api_service.set_voice_device_status(device_id, status, now()))).map_err(internal)?;
            self.context.device_changed(device_id);
            if !found {
                return Err(CenterError::new("voice_device_not_found", 404, "找不到该设备"));
            }
            Ok(self.status())
        }

        /// NV19 private-window management (`voice_context_manage`).
        pub fn context_manage(&self, input: &Value) -> Result<Value, CenterError> {
            self.context.manage(input, || self.chat_llm())
        }

        pub fn context_add_folder(&self, path: &std::path::Path) -> Result<Value, CenterError> {
            self.context.add_folder(path)
        }

        /// The configured cloud LLM for NV19 deep search; `None` when not configured.
        fn chat_llm(&self) -> Option<Arc<dyn moss_voice_providers::Llm>> {
            let settings = self.settings();
            let llm = settings.builtin_models.llm.as_ref()?;
            let key = self.secret(Section::Llm).ok()?.unwrap_or_default();
            OpenAiLlm::new(&llm.url, &key, &llm.model).ok().map(|llm| Arc::new(llm) as Arc<dyn moss_voice_providers::Llm>)
        }

        /// NV13 desktop-only inert configuration. Live execution remains locked
        /// until separately authorized real CLI sandbox-negative tests succeed.
        pub fn agent_status(&self)->Value{
            let mut status=self.host.api_service.agent_status();
            status["settings"]=serde_json::to_value(self.settings().builtin_agents).unwrap_or(Value::Null);
            status
        }
        pub fn agent_save(&self,input:&Value)->Result<Value,CenterError>{
            use moss_voice::settings::BuiltinAgents;
            let invalid=|message:&str|CenterError::new("agent_config_invalid",400,message);
            let config:BuiltinAgents=serde_json::from_value(input.clone())
                .map_err(|_|invalid("Agent 配置格式无效"))?;
            if config.enabled {
                return Err(CenterError::new("agent_sandbox_unavailable",409,"CLI 只读沙箱尚未通过权限负向测试，暂不允许启动 Agent"));
            }
            for text in [&config.workspace,&config.codex_executable,&config.claude_executable] {
                if text.len()>1024{return Err(invalid("Agent 路径最长 1024 字节"));}
            }
            let sensitive=[self.host.runtime_root.path.clone()];
            if !config.workspace.is_empty() {
                laser_host::voice_agent::workspace(std::path::Path::new(&config.workspace),&sensitive)
                    .map_err(|_|invalid("工作目录必须为现有、独立、非敏感的本地目录"))?;
            }
            if !config.codex_executable.is_empty() {
                laser_host::voice_agent::executable(std::path::Path::new(&config.codex_executable),laser_host::voice_agent::Provider::Codex)
                    .map_err(|_|invalid("Codex 必须是明确选定的 codex.exe，不接受脚本或快捷方式"))?;
            }
            if !config.claude_executable.is_empty() {
                laser_host::voice_agent::executable(std::path::Path::new(&config.claude_executable),laser_host::voice_agent::Provider::ClaudeCode)
                    .map_err(|_|invalid("Claude 必须是明确选定的 claude.exe，不接受脚本或快捷方式"))?;
            }
            let mut settings=self.settings();settings.builtin_agents=config;
            moss_voice::settings::save(&self.settings_file(),&settings)
                .map_err(|_|CenterError::new("agent_save_failed",500,"Agent 配置保存失败"))?;
            Ok(self.agent_status())
        }


        /// NV14: main-window-only local music settings. Reading status does not
        /// scan the user's filesystem; saving with enabled=true explicitly
        /// validates only the requested local folder.
        pub fn music_status(&self) -> Value {
            let settings = self.settings();
            serde_json::json!({
                "settings":settings.builtin_music,
                "running":self.running(),
                "requires_chat":true
            })
        }
        pub fn music_save(&self, input:&Value) -> Result<Value, CenterError> {
            use moss_voice::settings::BuiltinMusic;
            let invalid=|message:&str|CenterError::new("music_invalid",400,message);
            let config:BuiltinMusic = serde_json::from_value(
                input.get("settings").cloned().unwrap_or(Value::Null)
            ).map_err(|_|invalid("本地音乐配置格式无效"))?;
            if config.directory.len()>1024 || config.directory.chars().any(char::is_control) {
                return Err(invalid("目录太长或含非法字符"));
            }
            let track_count = if config.enabled {
                if config.directory.trim().is_empty() {
                    return Err(invalid("先明确选择本地音乐目录"));
                }
                moss_voice_server::music::MusicIndex::scan(
                    std::path::Path::new(&config.directory)
                ).map_err(|_|invalid("音乐目录无法读取、路径不安全或超过索引限制"))?.len()
            } else {0};
            let mut settings=self.settings();
            settings.builtin_music=config;
            moss_voice::settings::save(&self.settings_file(),&settings)
                .map_err(|_|CenterError::new("music_save_failed",500,"音乐配置写入失败"))?;
            let restarted=self.running();
            if restarted { self.stop(); self.start()?; }
            let mut result=self.music_status();
            result["restarted"]=Value::Bool(restarted);
            result["track_count"]=serde_json::json!(track_count);
            Ok(result)
        }

        /// NV15 inert Home Assistant configuration with optimistic revision.
        pub fn home_status(&self) -> Value {
            let credentials = self.host.db.lock().ok()
                .and_then(|db| db.home_token_status().ok());
            let (key_configured,credential_revision)=match credentials {
                Some(status)=>status,
                None=>{
                    // A copied SQLite file may hold an un-decryptable DPAPI
                    // blob from another Windows account. Permit ONLY private
                    // window import/recovery with revision CAS, not use of the
                    // foreign bearer nor any read/control authority.
                    let recover=self.host.db.lock().ok()
                        .and_then(|db|db.home_token_recovery_revision().ok());
                    let profile=moss_voice::settings::read_home_offline(&self.settings_file()).ok();
                    if let (Some(credential_revision),Some((settings,revision)))=(recover,profile){
                        return serde_json::json!({
                            "settings":settings,"revision":revision,
                            "credential_revision":credential_revision,
                            "key_configured":false,"ready":false,"can_control":false,
                            "voice_read_enabled":false,"voice_control_enabled":false,
                            "recovery_only":true,
                            "reason":"旧凭据可能属于其他 Windows 账户或已损坏；只能导入安全配置并撤销旧密文，随后重新设置令牌。旧控制和只读授权不可用"
                        });
                    }
                    return serde_json::json!({
                        "settings":null,"revision":null,"credential_revision":null,
                        "key_configured":false,"ready":false,"can_control":false,
                        "reason":"Home Assistant 凭据或版本损坏，安全恢复不可用"
                    });
                },
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
                })
            }
        }
        pub fn home_save(&self, input: &Value) -> Result<Value, CenterError> {
            use moss_voice::settings::{BuiltinHomeAssistant, HomeOfflineError};
            let invalid = |message:&str| CenterError::new("home_invalid",400,message);
            let object=input.as_object().ok_or_else(||invalid("Home Assistant 配置必须是对象"))?;
            if object.len()!=2 || !object.contains_key("settings") || !object.contains_key("expected_revision") {
                return Err(invalid("只接受 settings 和 expected_revision，拒绝令牌、密钥及控制字段"));
            }
            let revision=object["expected_revision"].as_u64()
                .filter(|value| *value <= 9_007_199_254_740_991)
                .ok_or_else(||invalid("缺少有效的 Home Assistant 配置版本"))?;
            let next:BuiltinHomeAssistant=serde_json::from_value(object["settings"].clone())
                .map_err(|_|invalid("实体白名单格式无效或有禁止字段"))?;
            self.home_control.fence_sensitive_change(||
                moss_voice::settings::save_home_offline(&self.settings_file(),&next,revision)
            ).map_err(|_|internal("无法安全撤销在途家居授权"))?
                .map_err(|error|match error {
                    HomeOfflineError::Invalid(message)=>invalid(message),
                    HomeOfflineError::Conflict=>CenterError::new("home_conflict",409,"白名单已在其他窗口修改，请重新加载后再保存"),
                    HomeOfflineError::Storage(_)=>CenterError::new("home_save_failed",500,"Home Assistant 配置保存失败；原设置未被覆盖"),
                })?;
            Ok(self.home_status())
        }

        pub fn home_voice_consent(&self,input:&Value)->Result<Value,CenterError>{
            let invalid=||CenterError::new("home_invalid",400,"语音只读授权参数无效");
            let obj=input.as_object().ok_or_else(invalid)?;
            if obj.len()!=3||!obj.contains_key("enabled")||!obj.contains_key("policy_revision")
                ||!obj.contains_key("credential_revision"){return Err(invalid());}
            let enabled=obj["enabled"].as_bool().ok_or_else(invalid)?;
            let expected_policy=obj["policy_revision"].as_u64().ok_or_else(invalid)?;
            let expected_credential=obj["credential_revision"].as_u64().ok_or_else(invalid)?;
            // The configuration read is protected by revision. A later change
            // automatically invalidates this exact grant.
            let (config,policy)=moss_voice::settings::read_home_offline(&self.settings_file())
                .map_err(|_|invalid())?;
            config.validate_offline().map_err(|_|invalid())?;
            if policy!=expected_policy {return Err(CenterError::new("home_conflict",409,"家居白名单已改变，拒绝授权"));}
            let db=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?;
            let (present,credential)=db.home_token_status().map_err(|_|invalid())?;
            if credential!=expected_credential ||(enabled&&(!present||config.base_url.is_empty()
                ||!config.entities.iter().any(|e|e.enabled))){
                return Err(CenterError::new("home_conflict",409,"令牌或实体已更改，拒绝授权"));
            }
            if enabled {
                db.set_secret(HOME_VOICE_GRANT,&format!("{policy}:{credential}"),now())
                    .map_err(|_|internal("语音只读授权未保存"))?;
            }else{
                db.delete_secret(HOME_VOICE_GRANT).map_err(|_|internal("撤销语音只读授权失败"))?;
            }
            drop(db);
            Ok(self.home_status())
        }
        pub fn home_control_consent(&self,input:&Value)->Result<Value,CenterError>{
            let invalid=||CenterError::new("home_invalid",400,"家居控制授权参数无效");
            let map=input.as_object().ok_or_else(invalid)?;
            if map.len()!=4 || map.keys().any(|key|!matches!(key.as_str(),"enabled"|"confirmed"|"policy_revision"|"credential_revision")){return Err(invalid());}
            self.home_control.consent(map["enabled"].as_bool().ok_or_else(invalid)?,map["policy_revision"].as_u64().ok_or_else(invalid)?,map["credential_revision"].as_u64().ok_or_else(invalid)?,map["confirmed"].as_bool().ok_or_else(invalid)?)
                .map_err(|code|CenterError::new(code,409,"家居控制授权未生效，请核对白名单、凭据与确认"))?;
            Ok(self.home_status())
        }
        pub fn home_approval_approve(&self,input:&Value)->Result<Value,CenterError>{
            let invalid=||CenterError::new("home_approval_invalid",400,"家居批准参数无效");
            let map=input.as_object().ok_or_else(invalid)?;
            if map.len()!=3 || map.keys().any(|key|!matches!(key.as_str(),"approval_id"|"digest"|"confirmed")){return Err(invalid());}
            let id=map["approval_id"].as_str().filter(|s|s.len()<=128).ok_or_else(invalid)?;
            let digest=map["digest"].as_str().filter(|s|s.len()==64 && s.bytes().all(|b|b.is_ascii_hexdigit())).ok_or_else(invalid)?;
            self.home_control.approve(id,digest,map["confirmed"].as_bool().ok_or_else(invalid)?)
                .map_err(|code|CenterError::new(code,409,"家居批准或执行未确认；请查看持久结果，不自动重发"))
        }

        /// Main-window private-only redacted profile export. A portable
        /// profile NEVER contains the DPAPI ciphertext, bearer key, grants,
        /// approvals or the rest of settings.json.
        pub fn home_backup_export(&self)->Result<Value,CenterError>{
            let profile=moss_voice::settings::export_home_portable(&self.settings_file())
                .map_err(|_|CenterError::new("home_backup_invalid",409,"白名单损坏或过大，无法生成安全备份"))?;
            Ok(serde_json::json!({
                "profile":profile,
                "credentials_included":false,
                "reenter_token_required":true,
            }))
        }

        /// Import is explicitly destructive to old *authority*, but not to
        /// user files: revoke token/grants FIRST, then CAS-save an inert
        /// imported profile. If settings write fails, privileges remain revoked.
        pub fn home_backup_restore(&self,input:&Value)->Result<Value,CenterError>{
            use moss_voice::settings::{self,HomeOfflineError};
            let invalid=||CenterError::new("home_backup_invalid",400,"备份格式、版本或确认无效");
            let map=input.as_object().ok_or_else(invalid)?;
            if map.len()!=4 || !map.keys().all(|key|matches!(key.as_str(),
                "profile"|"expected_revision"|"expected_credential_revision"|"confirmed")) {
                return Err(invalid());
            }
            if map["confirmed"].as_bool()!=Some(true) {return Err(invalid());}
            let payload=map["profile"].as_str().filter(|s|!s.is_empty()&&s.len()<=65536)
                .ok_or_else(invalid)?;
            let imported=settings::validate_home_portable(payload.as_bytes())
                .map_err(|_|invalid())?;
            let revision=map["expected_revision"].as_u64()
                .filter(|v|*v<=9_007_199_254_740_991).ok_or_else(invalid)?;
            let credential=map["expected_credential_revision"].as_u64()
                .filter(|v|*v<=9_007_199_254_740_991).ok_or_else(invalid)?;
            let file=self.settings_file();
            self.home_control.fence_sensitive_change(||->Result<(),CenterError>{
                let (_,current)=settings::read_home_offline(&file)
                    .map_err(|_|CenterError::new("home_conflict",409,"白名单状态已改变或损坏，未恢复"))?;
                if current!=revision {return Err(CenterError::new("home_conflict",409,"其他窗口已修改白名单，请重新加载"));}
                // Import must also recover a database copied from another
                // Windows user: it can revoke a foreign DPAPI blob without
                // decrypting it. SQLite compares the expected revision under
                // BEGIN IMMEDIATE; it never imports old credentials.
                let db=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?;
                let changed=db.update_home_token_if_revision(credential,None,now())
                    .map_err(|_|CenterError::new("home_unavailable",503,"撤销旧令牌失败，备份未导入"))?;
                if changed.is_none() {return Err(CenterError::new("home_conflict",409,"令牌版本并发冲突，备份未导入"));}
                // DB mutation and config file write are not one transaction.
                // If writing the inactive template fails, the old token and
                // grants remain REVOKED (fail closed), never rolled back.
                settings::save_home_offline(&file,&imported,revision)
                    .map_err(|error|match error {
                        HomeOfflineError::Invalid(_)=>invalid(),
                        HomeOfflineError::Conflict=>CenterError::new("home_conflict",409,"令牌已撤销，但白名单版本变更；请重新加载"),
                        HomeOfflineError::Storage(_)=>CenterError::new("home_save_failed",500,"令牌已撤销，但白名单保存失败；请重新加载"),
                    })?;
                Ok(())
            }).map_err(|_|internal("无法安全撤销在途家居授权"))??;
            Ok(self.home_status())
        }

        /// NV15 desktop-only token mutation: stored only in app_secrets,
        /// guarded by monotonic credential revision. No network call is made.
        pub fn home_token_save(&self,input:&Value)->Result<Value,CenterError>{
            let invalid=|message:&str|CenterError::new("home_token_invalid",400,message);
            let map=input.as_object().ok_or_else(||invalid("凭据变更必须是对象"))?;
            let action=map.get("action").and_then(Value::as_str).ok_or_else(||invalid("必须选择更新或清除"))?;
            let expected=map.get("expected_revision").and_then(Value::as_u64)
                .filter(|rev| *rev <= 9_007_199_254_740_991)
                .ok_or_else(||invalid("缺少有效的凭据版本"))?;
            if map.keys().any(|key| !matches!(key.as_str(),"action"|"expected_revision"|"value")) {
                return Err(invalid("凭据操作包含禁止字段"));
            }
            let replacement=match action {
                "set"=>Some(map.get("value").and_then(Value::as_str).ok_or_else(||invalid("请输入长期访问令牌"))?),
                "clear"=>{if map.contains_key("value") {return Err(invalid("清除操作不能携带令牌"));} None},
                _=>return Err(invalid("仅支持更新或清除令牌")),
            };
            let changed=self.home_control.fence_sensitive_change(||{
                let db=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?;
                db.update_home_token_if_revision(expected,replacement,now())
                    .map_err(|_|CenterError::new("home_token_invalid",400,"访问令牌未保存；请检查令牌格式和数据库"))
            }).map_err(|_|internal("无法安全撤销在途家居授权"))??;
            if changed.is_none() {return Err(CenterError::new("home_conflict",409,"凭据已更新，请重新加载最新状态"));}
            Ok(self.home_status())
        }

        /// Explicit trusted main-window read-only discovery; never sends a
        /// Home Assistant service POST and never exposes the bearer token.
        pub fn home_discover(&self)->Result<Value,CenterError>{
            use moss_voice::home_readonly::{self,DiscoveryError};
            let (settings,policy_revision)=moss_voice::settings::read_home_offline(&self.settings_file())
                .map_err(|_|CenterError::new("home_unavailable",409,"无法安全读取家居白名单"))?;
            let (token,credential_revision)={
                let db=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?;
                let (has_token,revision)=db.home_token_status()
                    .map_err(|_|CenterError::new("home_unavailable",409,"访问令牌配置损坏"))?;
                if !has_token {return Err(CenterError::new("home_unauthorized",401,"请先设置访问令牌"));}
                let token=db.home_token_for_connection()
                    .map_err(|_|CenterError::new("home_unavailable",409,"无法读取访问令牌"))?
                    .ok_or_else(||CenterError::new("home_unauthorized",401,"访问令牌不存在"))?;
                (token,revision)
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
            let (_,latest_policy)=moss_voice::settings::read_home_offline(&self.settings_file())
                .map_err(|_|CenterError::new("home_conflict",409,"家居配置读取失败，请重新加载"))?;
            let latest_token=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?
                .home_token_status().map_err(|_|CenterError::new("home_conflict",409,"令牌状态读取失败"))?;
            if latest_policy!=policy_revision || latest_token.1!=credential_revision || !latest_token.0 {
                return Err(CenterError::new("home_conflict",409,"查询期间配置已更改，结果已丢弃"));
            }
            serde_json::to_value(output).map_err(|_|internal("Home Assistant 查询结果序列化失败"))
        }

        /// NV15 private HomeControl inbox. Only sanitized previews; never return
        /// saved connection fingerprint, the request body, secrets or raw URLs.
        pub fn home_approvals(&self)->Result<Value,CenterError>{
            use laser_contracts::{ApprovalKind,ApprovalStatus};
            let approvals=laser_api::approval::ApprovalService::new(self.host.db.clone());
            let rows=approvals.list(None,250)
                .map_err(|_|CenterError::new("home_approval_unavailable",503,"无法读取家居审批记录"))?;
            let (policy,credential)={
                let (_,p)=moss_voice::settings::read_home_offline(&self.settings_file())
                    .map_err(|_|CenterError::new("home_unavailable",409,"Home Assistant 白名单不可读取"))?;
                let (_,c)=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?
                    .home_token_status().map_err(|_|CenterError::new("home_unavailable",409,"HA 凭据状态不可读取"))?;
                (p,c)
            };
            let mut items=Vec::new();
            for row in rows.iter().filter(|r|r.kind==ApprovalKind::HomeControl).take(60) {
                let requested=row.request.as_ref();
                let path=requested.and_then(|req|req.get("path")).and_then(Value::as_str).unwrap_or("");
                let service=path.strip_prefix("/api/services/")
                    .and_then(|tail|tail.split_once('/'))
                    .filter(|(domain,action)|{
                        matches!(*domain,"light"|"switch"|"climate"|"lock"|"alarm_control_panel"|"cover"|"media_player")
                        && matches!(*action,"turn_on"|"turn_off"|"set_temperature"|"lock"|"unlock"
                            |"alarm_arm_home"|"alarm_arm_away"|"alarm_disarm"
                            |"open_cover"|"close_cover"|"stop_cover"|"play_media")
                    }).map(|(domain,action)|format!("{domain}.{action}"))
                    .unwrap_or_else(||"unsupported".into());
                let mut safe_parameters=serde_json::Map::new();
                if let Some(body)=requested.and_then(|req|req.get("body")).and_then(Value::as_object) {
                    if let Some(brightness)=body.get("brightness").and_then(Value::as_u64).filter(|n|*n<=255) {
                        safe_parameters.insert("brightness".into(),brightness.into());
                    }
                    if let Some(temperature)=body.get("temperature").and_then(Value::as_f64).filter(|n|n.is_finite()&&(-40.0..=100.0).contains(n)) {
                        safe_parameters.insert("temperature".into(),serde_json::json!(temperature));
                    }
                }
                let (saved_policy,saved_credential)=requested.map(|req|{
                    (req.get("policy").or_else(||req.get("policy_revision")).and_then(Value::as_u64),
                     req.get("credential").or_else(||req.get("credential_revision")).and_then(Value::as_u64))
                }).unwrap_or_default();
                let stale=saved_policy!=Some(policy)||saved_credential!=Some(credential);
                let phase=row.outcome.as_ref().and_then(|o|o.phase)
                    .and_then(|phase|serde_json::to_value(phase).ok());
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
        pub fn home_approval_cancel(&self,input:&Value)->Result<Value,CenterError>{
            let invalid=||CenterError::new("home_approval_invalid",400,"家居撤销请求格式无效");
            let object=input.as_object().ok_or_else(invalid)?;
            if object.len()!=3 || object.get("confirmed")!=Some(&Value::Bool(true)) {
                return Err(invalid());
            }
            let id=object.get("approval_id").and_then(Value::as_str).filter(|id|{
                (12..=128).contains(&id.len()) &&
                id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_')
            }).ok_or_else(invalid)?;
            let digest=object.get("digest").and_then(Value::as_str)
                .filter(|d|d.len()==64&&d.bytes().all(|b|b.is_ascii_hexdigit()))
                .ok_or_else(invalid)?;
            // Cancellation cannot race between the final permit check and
            // the first byte of a HomeControl POST.
            let result=self.home_control.fence_request_change(id,||{
                let service=laser_api::approval::ApprovalService::new(self.host.db.clone());
                service.cancel_home_control(id,digest)
                    .map_err(|err|CenterError::new(err.code(),err.http_status(),err.message()))
            }).map_err(|_|internal("家居待执行请求撤销失败"))??;
            Ok(serde_json::json!({
                "approval_id":result.approval_id,
                "status":result.status.as_str(),
                "can_control":false,
            }))
        }

        /// NV12: private desktop-only settings and API-key presence. Never return the key.
        pub fn vision_status(&self)->Value{
            use moss_voice_server::vision::VisionFactory;
            let settings=self.settings().builtin_vision;
            let key_configured=self.host.db.lock().ok().and_then(|db|db.get_secret("voice.vision.main").ok().flatten()).is_some_and(|s|!s.is_empty());
            let ready=DesktopVisionFactory{host:self.host.clone()}.ready();
            serde_json::json!({"settings":settings,"key_configured":key_configured,"ready":ready})
        }

        pub fn vision_save(&self,input:&Value)->Result<Value,CenterError>{
            use moss_voice::settings::BuiltinVision;
            let invalid=|reason:&str|CenterError::new("vision_invalid",400,reason);
            let next:BuiltinVision=serde_json::from_value(input.get("settings").cloned().unwrap_or(Value::Null))
                .map_err(|_|invalid("视觉配置格式错误"))?;
            if next.url.len()>1024||next.model.len()>200 {return Err(invalid("模型名称或地址太长"));}
            if next.enabled && (next.model.trim().is_empty() || moss_voice_providers::vision::endpoint(&next.url).is_err()){
                return Err(invalid("请填写有效的模型名称和 HTTPS 地址（或本地明确的 HTTP 地址）"));
            }
            let action=input["key"]["action"].as_str().unwrap_or("keep");
            let db=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?;
            let existing=db.get_secret("voice.vision.main").map_err(|e|internal(db_error(e)))?.is_some_and(|s|!s.is_empty());
            let new_key=match action{
                "keep"=>None,
                "set"=>{
                    let key=input["key"]["value"].as_str().unwrap_or("");
                    if key.trim().is_empty()||key.len()>8192 {return Err(invalid("视觉模型密钥不能空或超过 8192 字节"));}
                    Some(Some(key.to_owned()))
                },
                "clear"=>Some(None),
                _=>return Err(invalid("密钥操作只能保留、更新或清除")),
            };
            let has_key=match &new_key{None=>existing,Some(Some(_))=>true,Some(None)=>false};
            if next.enabled&&!has_key{return Err(invalid("启用视觉前必须保存对应模型密钥"));}
            let changes:Vec<(String,Option<String>)>=new_key.into_iter().map(|key|("voice.vision.main".to_owned(),key)).collect();
            let mut settings=self.settings();
            settings.builtin_vision=next;
            commit_secrets_then_settings(&db,&changes,||{
                moss_voice::settings::save(&self.settings_file(),&settings)
                    .map_err(|_|CenterError::new("vision_save_failed",500,"视觉配置保存失败"))
            })?;
            drop(db);
            Ok(self.vision_status())
        }

        /// A button press uses only an in-memory synthetic 8x8 image.
        /// It does not access a device or a computer camera.
        pub fn vision_test(&self)->Result<Value,CenterError>{
            use moss_voice_server::vision::VisionFactory;
            let factory=DesktopVisionFactory{host:self.host.clone()};
            let session=factory.open().ok_or_else(||CenterError::new("vision_disabled",409,"请先启用并保存视觉模型与密钥"))?;
            let mut output=std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(8,8).write_to(&mut output,image::ImageFormat::Png)
                .map_err(|_|internal("生成内存测试图片失败"))?;
            let png=output.into_inner();
            let started=std::time::Instant::now();
            let result=tauri::async_runtime::block_on(session.provider.explain("请仅用一句话描述此测试图像。","image/png",&png));
            match result{
                Ok(text) if factory.still_current(session.generation)=>Ok(serde_json::json!({
                    "ok":true,"elapsed_ms":started.elapsed().as_millis(),"preview":text.chars().take(160).collect::<String>(),
                })),
                Ok(_)=>Err(CenterError::new("vision_settings_changed",409,"测试中配置发生变化，请重试")),
                Err(error)=>Err(CenterError::new(error.code(),502,"视觉模型连接或返回内容异常，请核对模型和密钥")),
            }
        }

        pub fn plugins_status(&self) -> Value {
            let settings = self.settings().builtin_plugins;
            let mut keys = serde_json::Map::new();
            for kind in ["weather","search","metaso","rag"] {
                let name = format!("voice.plugin.{kind}");
                let found = self.host.db.lock().ok()
                    .and_then(|db| db.get_secret(&name).ok().flatten())
                    .is_some_and(|value| !value.is_empty());
                keys.insert(kind.into(), Value::Bool(found));
            }
            serde_json::json!({"settings":settings,"keys":keys})
        }

        pub fn plugins_save(&self, input: &Value) -> Result<Value, CenterError> {
            use moss_voice::settings::BuiltinPlugins;
            use moss_voice_server::plugins::http;
            let invalid = |message: &str| CenterError::new("plugin_config_invalid",400,message);
            let config: BuiltinPlugins = serde_json::from_value(input.get("settings").cloned().unwrap_or(Value::Null))
                .map_err(|_| invalid("信息插件配置格式不正确"))?;
            if !["api_key","bearer"].contains(&config.weather_auth.as_str()) {
                return Err(invalid("天气认证方式必须为 API Key 或 Bearer"));
            }
            if config.weather_enabled {
                http::validate_endpoint(&config.weather_host,false).map_err(|_| invalid("天气服务地址必须是合法 HTTPS 地址"))?;
                let lat=config.weather_latitude.ok_or_else(||invalid("请设置天气地点纬度"))?;
                let lon=config.weather_longitude.ok_or_else(||invalid("请设置天气地点经度"))?;
                if !lat.is_finite() || !(-90.0..=90.0).contains(&lat)
                    || !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
                    return Err(invalid("纬度经度不在允许范围"));
                }
            }
            if !(-720..=840).contains(&config.lunar_utc_offset_minutes) {
                return Err(invalid("农历时区偏移无效"));
            }
            if config.news_sources.len()>10 {return Err(invalid("RSS 来源最多 10 个"));}
            let mut unique = std::collections::HashSet::new();
            for source in &config.news_sources {
                if source.id.is_empty() || source.id.len()>40
                    || !source.id.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'||c=='_')
                    || !unique.insert(source.id.clone()) {
                    return Err(invalid("RSS 来源编号必须唯一且仅含字母、数字、短横线和下划线"));
                }
                let is_newsnow=matches!(source.kind, moss_voice::settings::NewsSourceKind::Newsnow);
                let url=http::validate_endpoint(&source.url,is_newsnow).map_err(|_|invalid("RSS 必须使用 HTTPS；NewsNow 可使用明确配置的本机/LAN 地址"))?;
                if url.query().is_some() { return Err(invalid("新闻源地址不得包含查询参数，避免将密钥写入普通配置")); }
                if matches!(source.kind, moss_voice::settings::NewsSourceKind::Newsnow)
                    && (source.source_id.is_empty() || source.source_id.len()>80
                        || !source.source_id.chars().all(|c| c.is_ascii_alphanumeric() || c=='-' || c=='_')) {
                    return Err(invalid("NewsNow 来源 ID 必须为字母、数字、短横线或下划线"));
                }
            }
            if config.news_enabled && config.news_sources.is_empty() {
                return Err(invalid("开启新闻插件前必须添加 RSS 来源"));
            }
            if config.search_enabled {
                let host=match config.search_provider {
                    moss_voice::settings::SearchProvider::Tavily=>&config.search_host,
                    moss_voice::settings::SearchProvider::Metaso=>&config.metaso_host,
                };
                http::validate_endpoint(host,false).map_err(|_|invalid("联网搜索必须使用 HTTPS 服务地址"))?;
            }
            if config.rag_enabled {
                http::validate_endpoint(&config.rag_host,true).map_err(|_|invalid("RAGFlow 服务地址必须为 HTTPS 或明确的本机/LAN 地址"))?;
                if config.rag_dataset_ids.is_empty() || config.rag_dataset_ids.len()>20
                   || config.rag_dataset_ids.iter().any(|id|id.trim().is_empty()||id.len()>128) {
                    return Err(invalid("RAGFlow 数据集白名单格式不正确"));
                }
            }

            // Keep/Set/Clear: missing key input means keep; no returned secret.
            let keys = &input["keys"];
            let db=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?;
            let mut operations=Vec::<(String,Option<String>)>::new();
            for (kind, enabled) in [
                ("weather",config.weather_enabled),
                ("search",config.search_enabled && config.search_provider==moss_voice::settings::SearchProvider::Tavily),
                ("metaso",config.search_enabled && config.search_provider==moss_voice::settings::SearchProvider::Metaso),
                ("rag",config.rag_enabled)
            ] {
                let name=format!("voice.plugin.{kind}");
                let command=keys[kind]["action"].as_str().unwrap_or("keep");
                let existing=db.get_secret(&name).map_err(|e|internal(db_error(e)))?.is_some_and(|k|!k.is_empty());
                let available=match command {
                    "keep" => existing,
                    "set" => {
                        let value=keys[kind]["value"].as_str().unwrap_or("");
                        if value.trim().is_empty() || value.len()>4096 {
                            return Err(invalid("API 密钥格式不正确（不能空或超过4096字节）"));
                        }
                        operations.push((name,Some(value.to_owned())));
                        true
                    }
                    "clear" => { operations.push((name,None)); false }
                    _ => return Err(invalid("API 密钥操作必须为 keep/set/clear")),
                };
                if enabled && !available {
                    return Err(invalid("开启需要 API 密钥的插件前必须设置密钥"));
                }
            }

            // 持锁到提交完成：校验时看到的密钥状态不会被并发保存改掉
            let mut settings=self.settings();
            settings.builtin_plugins=config;
            commit_secrets_then_settings(&db,&operations,||{
                moss_voice::settings::save(&self.settings_file(),&settings)
                    .map_err(|_|CenterError::new("plugin_settings_failed",500,"插件配置保存失败"))
            })?;
            drop(db);
            Ok(self.plugins_status())
        }

        pub fn memory_list(&self) -> Result<Value, CenterError> {
            serde_json::to_value(self.memory.list().map_err(internal)?).map_err(|_| internal("记忆列表序列化失败"))
        }
        pub fn memory_edit(&self, device_id: &str, revision: i64, summary: &str) -> Result<Value, CenterError> {
            let updated = self.memory.edit(device_id, revision, summary).map_err(|e| CenterError::new("memory_invalid", 400, e))?;
            serde_json::to_value(updated.ok_or_else(|| CenterError::new("memory_conflict", 409, "记忆已更新，请刷新后重试"))?).map_err(|_| internal("保存结果序列化失败"))
        }
        pub fn memory_enable(&self, enabled: bool) -> Result<Value, CenterError> {
            self.memory.configure(enabled, || {
                let mut settings = self.settings();
                settings.builtin_memory_enabled = enabled;
                moss_voice::settings::save(&self.settings_file(), &settings)
            }).map_err(|e| CenterError::new("memory_settings_failed", 500, e))?;
            Ok(serde_json::json!({"enabled": enabled}))
        }

        pub fn firmware_status(&self)->Result<Value,CenterError>{
            let records=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?.list_firmware_deliveries().map_err(|_|internal("读取固件记录失败"))?;
            Ok(serde_json::json!({"enabled":false,"trusted_download_protocol":false,"records":records,"reason":"当前固件下载未提供可信设备鉴权及哈希验证协议；OTA offer保持空"}))
        }

        pub fn speaker_manage(&self,input:&Value)->Result<Value,CenterError>{
            let invalid=||CenterError::new("speaker_invalid",400,"声纹操作格式无效");
            let object=input.as_object().ok_or_else(invalid)?;
            let action=input["action"].as_str().ok_or_else(invalid)?;
            let allowed:&[&str]=match action {"status"|"cancel"=>&["action"],"begin"=>&["action","device_id","session_id","name","confirmed"],"delete"=>&["action","id","revision","confirmed"],"save_memory"=>&["action","id","revision","summary"],"personal_mode"=>&["action","enabled","confirmed"],"begin_test"=>&["action","device_id","session_id","id","confirmed"],"calibrate"|"reset_calibration"=>&["action","confirmed"],_=>return Err(invalid())};
            if object.len()!=allowed.len() || object.keys().any(|k|!allowed.contains(&k.as_str())){return Err(invalid());}
            match action {
                "begin"=>{
                    if input["confirmed"].as_bool()!=Some(true){return Err(invalid());}
                    let device=input["device_id"].as_str().ok_or_else(invalid)?;
                    let session=input["session_id"].as_str().ok_or_else(invalid)?;
                    let name=input["name"].as_str().ok_or_else(invalid)?;
                    let chat=self.server.lock().map_err(|_|internal("语音服务锁失败"))?.as_ref().is_some_and(|s|{
                        let status=s.status();status.mode==ServerMode::Chat && status.sessions.iter().any(|r|r.device_id==device&&r.session_id==session)
                    });
                    if !chat{return Err(CenterError::new("speaker_session_required",409,"请选择已连接的对话模式设备"));}
                    let context=laser_contracts::CallerContext::Voice{device_id:device.into(),session_id:session.into()};
                    let permit=self.host.api_service.external_voice_permit(&context).map_err(|_|CenterError::new("speaker_revoked",403,"当前设备会话已撤销"))?;
                    self.prepare_speaker_helper()?;
                    self.speaker.begin(device,session,name,permit,true).map_err(|code|CenterError::new(code,409,"登记无法启动，请检查会话、名称或正在进行的登记"))?;
                },
                "cancel"=>{self.speaker.cancel().map_err(|_|internal("取消声纹登记失败"))?;},
                // 简易标定（2026-10-14）：已登记的人各录三段测试音 → 计算阈值并自检 → 通过才开放识别
                "begin_test"=>{
                    if input["confirmed"].as_bool()!=Some(true){return Err(invalid());}
                    let device=input["device_id"].as_str().ok_or_else(invalid)?;
                    let session=input["session_id"].as_str().ok_or_else(invalid)?;
                    let id=input["id"].as_str().ok_or_else(invalid)?;
                    let known=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?.list_voice_speakers().map_err(|_|internal("读取声纹失败"))?.iter().any(|r|r.id==id);
                    if !known{return Err(CenterError::new("speaker_conflict",409,"这个人的声纹已不存在，请刷新"));}
                    let chat=self.server.lock().map_err(|_|internal("语音服务锁失败"))?.as_ref().is_some_and(|s|{
                        let status=s.status();status.mode==ServerMode::Chat && status.sessions.iter().any(|r|r.device_id==device&&r.session_id==session)
                    });
                    if !chat{return Err(CenterError::new("speaker_session_required",409,"请选择已连接的对话模式设备"));}
                    let context=laser_contracts::CallerContext::Voice{device_id:device.into(),session_id:session.into()};
                    let permit=self.host.api_service.external_voice_permit(&context).map_err(|_|CenterError::new("speaker_revoked",403,"当前设备会话已撤销"))?;
                    self.prepare_speaker_helper()?;
                    self.speaker.begin_test(device,session,id,permit,true).map_err(|code|CenterError::new(code,409,"测试音采集无法启动，请检查会话或正在进行的登记"))?;
                },
                "calibrate"=>{
                    if input["confirmed"].as_bool()!=Some(true){return Err(invalid());}
                    let templates=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?.native_voice_speaker_templates().map_err(|_|internal("读取声纹失败"))?
                        .into_iter().map(|t|SpeakerTemplate{id:t.id,revision:t.revision,embedding:t.embedding}).collect::<Vec<_>>();
                    let samples=self.speaker_backend.tests.lock().map_err(|_|internal("标定数据不可用"))?.iter()
                        .flat_map(|(id,clips)|clips.iter().map(|clip|CalibrationSample{speaker_id:id.clone(),embedding:clip.clone()}))
                        .collect::<Vec<_>>();
                    let report=moss_voice_server::person_context::calibrate(&templates,&samples).map_err(|reason|CenterError::new("speaker_calibration_failed",409,reason))?;
                    let calibration=Calibration{report,directory:speaker_directory(templates.iter().map(|t|(t.id.as_str(),t.revision))),calibrated_at:now()};
                    let text=serde_json::to_string(&calibration).map_err(|_|internal("标定结果序列化失败"))?;
                    self.host.db.lock().map_err(|_|internal("数据库锁失败"))?.set_secret(SPEAKER_CALIBRATION_KEY,&text,now()).map_err(|_|internal("保存标定结果失败"))?;
                    *self.identity.calibration.lock().map_err(|_|internal("标定状态锁失败"))?=Some(calibration);
                    self.speaker_backend.tests.lock().map_err(|_|internal("标定数据不可用"))?.clear();
                    self.personal.directory_changed();
                    if self.settings().builtin_person_memory_enabled{self.restart_if_running()?;}
                },
                "reset_calibration"=>{
                    if input["confirmed"].as_bool()!=Some(true){return Err(invalid());}
                    self.host.db.lock().map_err(|_|internal("数据库锁失败"))?.delete_secret(SPEAKER_CALIBRATION_KEY).map_err(|_|internal("清除标定结果失败"))?;
                    *self.identity.calibration.lock().map_err(|_|internal("标定状态锁失败"))?=None;
                    self.speaker_backend.tests.lock().map_err(|_|internal("标定数据不可用"))?.clear();
                    self.personal.directory_changed();
                    if self.settings().builtin_person_memory_enabled{self.restart_if_running()?;}
                },
                "delete"=>{
                    if input["confirmed"].as_bool()!=Some(true){return Err(invalid());}
                    let id=input["id"].as_str().ok_or_else(invalid)?;let revision=input["revision"].as_i64().ok_or_else(invalid)?;
                    if id.is_empty() || id.len()>64 || !id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_') || revision<1{return Err(invalid());}
                    if !self.personal.person_deleted(id,||self.host.db.lock().map_err(|_|"数据库锁失败".to_owned())?.delete_voice_speaker_if_revision(id,revision).map_err(db_error)).map_err(internal)? {
                        return Err(CenterError::new("speaker_conflict",409,"声纹记录已变化，请刷新"));
                    }
                    if let Ok(mut tests)=self.speaker_backend.tests.lock(){tests.remove(id);}
                },
                "save_memory"=>{
                    let id=input["id"].as_str().ok_or_else(invalid)?;let revision=input["revision"].as_i64().ok_or_else(invalid)?;
                    let text=input["summary"].as_str().ok_or_else(invalid)?;
                    let row=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?.list_voice_speakers().map_err(|_|internal("读取声纹失败"))?.into_iter().find(|r|r.id==id).ok_or_else(invalid)?;
                    let scope=PersonScope::candidate(&row.id,row.revision,self.personal.generation()).ok_or_else(invalid)?;
                    if !self.personal.operator_edit(&scope,revision,text).map_err(|_|invalid())? {return Err(CenterError::new("speaker_memory_conflict",409,"个人摘要已改变，请刷新后重试"));}
                },
                "personal_mode"=>{
                    let enabled=input["enabled"].as_bool().ok_or_else(invalid)?;
                    input["confirmed"].as_bool().ok_or_else(invalid)?;
                    if enabled && (input["confirmed"].as_bool()!=Some(true)||self.identity.valid_calibration().is_none()) {
                        return Err(CenterError::new("speaker_calibration_required",409,"请先完成声纹简易标定（登记名单变化后需重新标定）"));
                    }
                    if enabled{self.prepare_speaker_helper()?;}
                    self.personal.directory_changed();
                    let mut settings=self.settings();settings.builtin_person_memory_enabled=enabled;
                    moss_voice::settings::save(&self.settings_file(),&settings).map_err(internal)?;
                    self.restart_if_running()?;
                },_=>{}
            }
            let rows=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?.list_voice_speakers().map_err(|_|internal("读取声纹失败"))?;
            let rows={let db=self.host.db.lock().map_err(|_|internal("数据库锁失败"))?;
                rows.into_iter().map(|row|{let (summary,revision)=db.voice_person_summary(&row.id).map_err(|_|internal("读取个人记忆失败"))?.unwrap_or((String::new(),0));
                    let mut value=serde_json::to_value(row).map_err(|_|internal("声纹元数据序列化失败"))?;value["memory_summary"]=Value::String(summary);value["memory_revision"]=Value::from(revision);Ok(value)}).collect::<Result<Vec<_>,CenterError>>()?};
            let sessions=self.server.lock().map_err(|_|internal("语音服务锁失败"))?.as_ref().map(|s|s.status().sessions).unwrap_or_default().into_iter().map(|r|serde_json::json!({"device_id":r.device_id,"session_id":r.session_id})).collect::<Vec<_>>();
            let valid=self.identity.valid_calibration();
            let calibration=match (&valid,self.identity.calibration.lock().ok().and_then(|c|c.clone())) {
                (Some(c),_)=>serde_json::json!({"state":"valid","report":c.report,"calibrated_at":c.calibrated_at}),
                (None,Some(c))=>serde_json::json!({"state":"stale","report":c.report,"calibrated_at":c.calibrated_at}),
                (None,None)=>serde_json::json!({"state":"none"}),
            };
            let test_clips=self.speaker_backend.tests.lock().map(|tests|tests.iter().map(|(id,clips)|(id.clone(),Value::from(clips.len()))).collect::<serde_json::Map<_,_>>()).unwrap_or_default();
            Ok(serde_json::json!({"enrollment":self.speaker.status(),"speakers":rows,"sessions":sessions,"recognition_enabled":valid.is_some(),"personal_memory_enabled":valid.is_some()&&self.settings().builtin_person_memory_enabled,"calibration":calibration,"test_clips":test_clips}))
        }

        pub fn rename_device(&self, device_id: &str, name: &str) -> Result<Value, CenterError> {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            let found = db.rename_voice_device(device_id, name).map_err(|error| internal(db_error(error)))?;
            drop(db);
            if !found {
                return Err(CenterError::new("voice_device_not_found", 404, "找不到该设备"));
            }
            Ok(self.status())
        }
    }

    /// 测试豆包识别或合成（NV7）：识别送 1 秒低音量正弦音，合成一句短话。结果与错误都不含密钥。
    fn test_doubao(resolved: &moss_voice::models::ResolvedSection) -> moss_voice::TestResult {
        let started = std::time::Instant::now();
        let outcome = tauri::async_runtime::block_on(async {
            match resolved.section {
                Section::Asr => {
                    let asr = DoubaoAsr::new(&resolved.url, &resolved.api_key, &resolved.model)?;
                    let tone: Vec<i16> =
                        (0..16_000).map(|index| ((index as f64 / 16_000.0 * 440.0 * std::f64::consts::TAU).sin() * 1200.0) as i16).collect();
                    asr.transcribe(&tone, 16_000).await.map(|_| "连接成功，识别接口已返回结果（流式）".to_owned())
                }
                _ => {
                    let tts = DoubaoTts::new(&resolved.url, &resolved.api_key, &resolved.model, &resolved.voice, resolved.speed as f32)?;
                    let pcm = tts.synthesize("你好，我是 MOSS。").await?;
                    let seconds = pcm.samples.len() as f64 / pcm.sample_rate as f64;
                    Ok(format!("连接成功，合成了 {seconds:.1} 秒音频（流式）"))
                }
            }
        });
        let (ok, detail) = match outcome {
            Ok(detail) => (true, detail),
            Err(error) => (false, error.0),
        };
        moss_voice::TestResult {
            section: resolved.section,
            ok,
            elapsed_ms: started.elapsed().as_millis() as u64,
            http_status: None,
            final_url: resolved.url.clone(),
            detail,
        }
    }

    fn builtin(app: &tauri::AppHandle) -> Result<Arc<BuiltinVoice>, Value> {
        app.try_state::<BuiltinState>()
            .and_then(|state| state.0.clone())
            .ok_or_else(|| serde_json::json!({"code": "voice_unavailable", "message": "内置语音服务不可用：本机后端未启动"}))
    }

    /// 在阻塞线程池中执行（启动、停止会创建或关闭运行时，不能放在异步线程里）。
    pub async fn run<F>(app: tauri::AppHandle, task: F) -> CmdResult
    where
        F: FnOnce(&BuiltinVoice) -> Result<Value, CenterError> + Send + 'static,
    {
        let builtin = builtin(&app)?;
        tauri::async_runtime::spawn_blocking(move || {
            task(&builtin).map_err(|error| serde_json::to_value(error).unwrap_or_default())
        })
        .await
        .map_err(|_| serde_json::json!({"code": "voice_internal", "message": "内置语音服务任务异常结束"}))?
    }

    pub fn parse_sections(values: &[String]) -> Result<Vec<Section>, CenterError> {
        values
            .iter()
            .map(|value| Section::parse(value).ok_or_else(|| CenterError::new("model_input_invalid", 400, "分区只能是 llm、asr 或 tts")))
            .collect()
    }

    /// MOSS 退出时停止内置语音服务（在急停之后调用）。
    pub fn shutdown(app: &tauri::AppHandle) {
        if let Ok(builtin) = builtin(app) {
            builtin.stop();
        }
    }
}

#[cfg(desktop)]
pub use imp::{BuiltinVoice, autostart, init, shutdown};

#[tauri::command]
pub async fn voice_builtin_status(app: tauri::AppHandle) -> CmdResult {
    #[cfg(desktop)]
    let result = imp::run(app, |builtin| Ok(builtin.status())).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_builtin_start(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |builtin| builtin.start()).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_builtin_stop(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |builtin| Ok(builtin.stop())).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

/// 允许（`allowed`）或撤销（`revoked`）设备。允许后设备需重启以重新获取 token。
#[tauri::command]
pub async fn voice_device_set_status(app: tauri::AppHandle, window: tauri::WebviewWindow, device_id: String, status: String) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.set_device_status(&device_id, &status)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, device_id, status));
    result
}

#[tauri::command]
pub async fn voice_device_rename(app: tauri::AppHandle, window: tauri::WebviewWindow, device_id: String, name: String) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.rename_device(&device_id, &name)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, device_id, name));
    result
}

/// 测试内置服务的一个模型分区（`llm` / `asr` / `tts`）；密钥留空时用已保存的密钥。
#[tauri::command]
pub async fn voice_builtin_models_test(app: tauri::AppHandle, window: tauri::WebviewWindow, section: String, input: Value) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        let section = imp::parse_sections(std::slice::from_ref(&section))?[0];
        let input: moss_voice::SectionInput = serde_json::from_value(input)
            .map_err(|_| moss_voice::CenterError::new("model_input_invalid", 400, "模型配置格式不正确"))?;
        builtin.models_test(section, &input)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, section, input));
    result
}

/// 保存内置服务的模型设置。`clear` 中的分区删除设置与密钥。
#[tauri::command]
pub async fn voice_builtin_models_save(app: tauri::AppHandle, window: tauri::WebviewWindow, input: Value, clear: Option<Vec<String>>) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| {
        let input: moss_voice::ModelsInput = serde_json::from_value(input)
            .map_err(|_| moss_voice::CenterError::new("model_input_invalid", 400, "模型配置格式不正确"))?;
        let clear = imp::parse_sections(&clear.unwrap_or_default())?;
        builtin.models_save(&input, &clear)
    })
    .await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, input, clear));
    result
}

/// 保存本地模型设置（`asr` / `tts` / `vad` 开关、`tts_speaker`、`tts_speed`、`mirror`）。
#[tauri::command]
pub async fn voice_local_save(app: tauri::AppHandle, window: tauri::WebviewWindow, settings: Value) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.local_save(&settings)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, settings));
    result
}

/// 开始下载本地模型（id 见 `local_models::CATALOG`）；进度在状态的 `local.models` 中。
#[tauri::command]
pub async fn voice_local_model_download(app: tauri::AppHandle, window: tauri::WebviewWindow, id: String) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.model_download(&id)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, id));
    result
}

#[tauri::command]
pub async fn voice_local_model_cancel(app: tauri::AppHandle, window: tauri::WebviewWindow, id: String) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| Ok(builtin.model_cancel(&id))).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, id));
    result
}

#[tauri::command]
pub async fn voice_local_model_delete(app: tauri::AppHandle, window: tauri::WebviewWindow, id: String) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |builtin| builtin.model_delete(&id)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, window, id));
    result
}

/// 语音中心校验用的豆包默认值与模型提供方的默认值一致（两个 crate 各自定义，避免互相依赖）。
#[cfg(all(test, desktop))]
#[test]
fn doubao_defaults_match_the_provider() {
    use moss_voice::models::{DOUBAO_ASR_RESOURCE, DOUBAO_ASR_URL, DOUBAO_TTS_RESOURCE, DOUBAO_TTS_URL};
    use moss_voice_providers::doubao;
    assert_eq!(DOUBAO_ASR_URL, doubao::DEFAULT_ASR_URL);
    assert_eq!(DOUBAO_ASR_RESOURCE, doubao::DEFAULT_ASR_RESOURCE);
    assert_eq!(DOUBAO_TTS_URL, doubao::DEFAULT_TTS_URL);
    assert_eq!(DOUBAO_TTS_RESOURCE, doubao::DEFAULT_TTS_RESOURCE);
}

#[tauri::command]
pub async fn voice_agent_status(app:tauri::AppHandle, window: tauri::WebviewWindow)->CmdResult{
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,|voice|Ok(voice.agent_status())).await;
    #[cfg(mobile)]
    let result=desktop_only((app, window));
    result
}
#[tauri::command]
pub async fn voice_agent_save(app:tauri::AppHandle, window: tauri::WebviewWindow,input:Value)->CmdResult{
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,move|voice|voice.agent_save(&input)).await;
    #[cfg(mobile)]
    let result=desktop_only((app, window,input));
    result
}



#[tauri::command]
pub async fn voice_music_pick_directory(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        // Opens only after an explicit click in the desktop main window.
        // The user-selected path is returned to the form, not saved or scanned.
        let Some(picked) = app.dialog().file().set_title("选择本地音乐文件夹").blocking_pick_folder() else {
            return Ok(Value::Null);
        };
        let path = picked.into_path().map_err(|_|serde_json::json!({
            "code":"music_invalid","message":"无法识别选择的音乐目录"
        }))?;
        let name = path.to_str().ok_or_else(||serde_json::json!({
            "code":"music_invalid","message":"音乐目录包含不支持的字符"
        }))?;
        if !path.is_absolute() || name.len() > 1024 || name.chars().any(char::is_control) {
            return Err(serde_json::json!({
                "code":"music_invalid","message":"所选路径无效或过长"
            }));
        }
        Ok(serde_json::json!({"directory":name}))
    }).await.map_err(|_|serde_json::json!({
        "code":"voice_internal","message":"选择音乐目录失败"
    }))?;
    #[cfg(mobile)]
    let result = desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_music_status(app:tauri::AppHandle, window: tauri::WebviewWindow)->CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,|voice|Ok(voice.music_status())).await;
    #[cfg(mobile)]
    let result=desktop_only((app, window));
    result
}
#[tauri::command]
pub async fn voice_music_save(app:tauri::AppHandle, window: tauri::WebviewWindow,input:Value)->CmdResult {
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,move|voice|voice.music_save(&input)).await;
    #[cfg(mobile)]
    let result=desktop_only((app, window,input));
    result
}

#[tauri::command]
pub async fn voice_home_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.home_status())).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_save(app: tauri::AppHandle, window: tauri::WebviewWindow, input: Value) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app,input));
    result
}

#[tauri::command]
pub async fn voice_home_backup_export(app:tauri::AppHandle,window:tauri::WebviewWindow)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,|voice|voice.home_backup_export()).await;
    #[cfg(mobile)]
    let result=desktop_only(app);
    result
}
/// Explicit local-file picker; no path or bytes are supplied by web content.
#[tauri::command]
pub async fn voice_home_backup_audit(app:tauri::AppHandle,window:tauri::WebviewWindow)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let Some(picked)=app.dialog().file().set_title("选择历史备份文件（仅审计，不恢复）").blocking_pick_file() else {
            return Ok(Value::Null);
        };
        let path=picked.into_path().map_err(|_|serde_json::json!({"code":"home_audit_invalid","message":"无法识别所选文件"}))?;
        let audit=moss_voice::home_backup_audit::audit_selected_file(&path)
            .map_err(|_|serde_json::json!({"code":"home_audit_invalid","message":"文件不符合只读安全审计条件（路径、类型或大小）"}))?;
        let category=match audit.risk {
            moss_voice::home_backup_audit::BackupRisk::SuspectedLegacyCredential=>"suspected_legacy_credential",
            moss_voice::home_backup_audit::BackupRisk::EncryptedCredentialMaterial=>"encrypted_credential_material",
            moss_voice::home_backup_audit::BackupRisk::UnknownTreatAsSensitive=>"unknown_treat_as_sensitive",
        };
        Ok(serde_json::json!({"risk":category,"inspected_bytes":audit.inspected_bytes,"safe_to_share":false}))
    }).await.map_err(|_|serde_json::json!({"code":"home_audit_failed","message":"历史文件审计未完成"}))?;
    #[cfg(mobile)]
    let result=desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_home_backup_restore(app:tauri::AppHandle,window:tauri::WebviewWindow,input:Value)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,move|voice|voice.home_backup_restore(&input)).await;
    #[cfg(mobile)]
    let result=desktop_only((app,input));
    result
}
#[tauri::command]
pub async fn voice_home_token_save(app: tauri::AppHandle, window: tauri::WebviewWindow, input: Value) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.home_token_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app,input));
    result
}
#[tauri::command]
pub async fn voice_home_voice_consent(app:tauri::AppHandle,window:tauri::WebviewWindow,input:Value)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,move|voice|voice.home_voice_consent(&input)).await;
    #[cfg(mobile)]
    let result=desktop_only((app,input));
    result
}
#[tauri::command]
pub async fn voice_home_discover(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.home_discover()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_approvals(app:tauri::AppHandle,window:tauri::WebviewWindow)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,|voice|voice.home_approvals()).await;
    #[cfg(mobile)]
    let result=desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_approval_cancel(app:tauri::AppHandle,window:tauri::WebviewWindow,input:Value)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,move|voice|voice.home_approval_cancel(&input)).await;
    #[cfg(mobile)]
    let result=desktop_only((app,input));
    result
}
#[tauri::command]
pub async fn voice_home_control_consent(app:tauri::AppHandle,window:tauri::WebviewWindow,input:Value)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,move|voice|voice.home_control_consent(&input)).await;
    #[cfg(mobile)]
    let result=desktop_only((app,input));
    result
}
#[tauri::command]
pub async fn voice_speaker_manage(app:tauri::AppHandle,window:tauri::WebviewWindow,input:Value)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,move|voice|voice.speaker_manage(&input)).await;
    #[cfg(mobile)]
    let result=desktop_only((app,input));
    result
}
#[tauri::command]
pub async fn voice_firmware_status(app:tauri::AppHandle,window:tauri::WebviewWindow)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,|voice|voice.firmware_status()).await;
    #[cfg(mobile)]
    let result=desktop_only(app);
    result
}
#[tauri::command]
pub async fn voice_home_approval_approve(app:tauri::AppHandle,window:tauri::WebviewWindow,input:Value)->CmdResult{
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,move|voice|voice.home_approval_approve(&input)).await;
    #[cfg(mobile)]
    let result=desktop_only((app,input));
    result
}
#[tauri::command]
pub async fn voice_vision_status(app:tauri::AppHandle, window: tauri::WebviewWindow)->CmdResult{
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,|voice|Ok(voice.vision_status())).await;
    #[cfg(mobile)]
    let result=desktop_only((app, window));
    result
}
#[tauri::command]
pub async fn voice_vision_save(app:tauri::AppHandle, window: tauri::WebviewWindow,input:Value)->CmdResult{
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,move|voice|voice.vision_save(&input)).await;
    #[cfg(mobile)]
    let result=desktop_only((app, window,input));
    result
}
#[tauri::command]
pub async fn voice_vision_test(app:tauri::AppHandle, window: tauri::WebviewWindow)->CmdResult{
    #[cfg(desktop)]
    require_main_window(&window)?;
    #[cfg(desktop)]
    let result=imp::run(app,|voice|voice.vision_test()).await;
    #[cfg(mobile)]
    let result=desktop_only((app, window));
    result
}

#[tauri::command]
pub async fn voice_plugins_status(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| Ok(voice.plugins_status())).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_plugins_save(app: tauri::AppHandle, window: tauri::WebviewWindow, input: Value) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.plugins_save(&input)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, input));
    result
}

#[tauri::command]
pub async fn voice_memory_list(app: tauri::AppHandle, window: tauri::WebviewWindow) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, |voice| voice.memory_list()).await;
    #[cfg(mobile)]
    let result = desktop_only(app);
    result
}

#[tauri::command]
pub async fn voice_memory_set_enabled(app: tauri::AppHandle, window: tauri::WebviewWindow, enabled: bool) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.memory_enable(enabled)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, enabled));
    result
}

#[tauri::command]
pub async fn voice_memory_save(app: tauri::AppHandle, window: tauri::WebviewWindow, device_id: String, expected_revision: i64, summary: String) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.memory_edit(&device_id, expected_revision, &summary)).await;
    #[cfg(mobile)]
    let result = desktop_only((app, device_id, expected_revision, summary));
    result
}

#[tauri::command]
pub async fn voice_memory_clear(app: tauri::AppHandle, window: tauri::WebviewWindow, device_id: String, expected_revision: i64) -> CmdResult {
    #[cfg(desktop)]
    require_voice_private_asset(&window)?;
    #[cfg(desktop)]
    let result = imp::run(app, move |voice| voice.memory_edit(&device_id, expected_revision, "")).await;
    #[cfg(mobile)]
    let result = desktop_only((app, device_id, expected_revision));
    result
}

/// 前端调用的内置语音命令（build.rs 登记与 capability 授权的测试会核对这张表）。
#[cfg(test)]
pub const COMMANDS: [&str; 39] = [
    "voice_firmware_status",
    "voice_speaker_manage",
    "voice_music_pick_directory",
    "voice_music_status",
    "voice_music_save",
    "voice_home_status",
    "voice_home_save",
    "voice_home_backup_export",
    "voice_home_backup_audit",
    "voice_home_backup_restore",
    "voice_home_token_save",
    "voice_home_voice_consent",
    "voice_home_approvals",
    "voice_home_approval_cancel",
    "voice_home_control_consent",
    "voice_home_approval_approve",
    "voice_home_discover",
    "voice_agent_status",
    "voice_agent_save",
    "voice_vision_status",
    "voice_vision_save",
    "voice_vision_test",
    "voice_plugins_status",
    "voice_plugins_save",
    "voice_memory_list",
    "voice_memory_set_enabled",
    "voice_memory_save",
    "voice_memory_clear",
    "voice_builtin_status",
    "voice_builtin_start",
    "voice_builtin_stop",
    "voice_device_set_status",
    "voice_device_rename",
    "voice_builtin_models_test",
    "voice_builtin_models_save",
    "voice_local_save",
    "voice_local_model_download",
    "voice_local_model_cancel",
    "voice_local_model_delete",
];
