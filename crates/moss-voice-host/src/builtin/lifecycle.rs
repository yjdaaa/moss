//! 内置语音的初始化、随 MOSS 自动启动与服务启停编排。

use super::*;
use serde_json::json;

/// Tauri 托管状态：后端未启动时为空。
pub struct BuiltinState(pub Option<Arc<BuiltinVoice>>);

/// 宿主持有的诊断记录本（内置语音初始化时登记），供更新模块、本地模型进程等写“MOSS 软件”事件。
static HOST_LOG: std::sync::OnceLock<Arc<moss_voice_server::DiagnosticsLog>> =
    std::sync::OnceLock::new();

/// 写一条“MOSS 软件”事件到语音中心“记录与诊断”（2026-11-09）；后端没起来时静默丢弃。
pub fn record_moss_event(record: EventRecord) {
    if let Some(log) = HOST_LOG.get() {
        log.push_from(moss_voice_server::DiagSource::Moss, record);
    }
}

/// 完整过程记录里“本地模型程序”这条流程的名字。
const HELPER_FLOW: &str = "语音·本地模型程序";
/// 完整过程记录里内置语音服务启停这条流程的名字。
const SERVICE_FLOW: &str = "语音·服务";

/// 把本地模型程序（判停 / 识别 / 合成辅助程序、语义判停程序、声纹程序）的启动、标准错误输出、意外退出写进
/// 完整过程记录（n110；记录关着时什么都不做）。输出先去掉疑似密钥再写。
pub(super) struct TraceHelperObserver {
    pub(super) trace: laser_runtime::ProcessTrace,
}

impl TraceHelperObserver {
    fn record(&self, kind: &str, step: String, data: Value) {
        if self.trace.enabled() {
            self.trace.record("voice", HELPER_FLOW, kind, &step, data);
        }
    }
}

fn scrub_line(text: &str) -> String {
    moss_voice_providers::scrub_text_with_options(text, false, 0, false).0
}

impl moss_voice_providers::local::HelperObserver for TraceHelperObserver {
    fn started(&self, role: &str, pid: Option<u32>) {
        self.record(
            "helper_start",
            format!("{role}已启动"),
            json!({"role": role, "pid": pid}),
        );
    }

    fn stderr_line(&self, role: &str, line: &str) {
        if !self.trace.enabled() {
            return;
        }
        let line = scrub_line(line);
        let shown: String = line.chars().take(200).collect();
        self.record(
            "helper_stderr",
            format!("{role}输出：{shown}"),
            json!({"role": role, "line": line}),
        );
    }

    fn exited(&self, role: &str, detail: &str) {
        self.record(
            "helper_exit",
            format!("{role}停了：{}", scrub_line(detail)),
            json!({"role": role, "detail": scrub_line(detail)}),
        );
    }
}

pub fn init(host: Option<Arc<laser_host::HostApplication>>) -> BuiltinState {
    BuiltinState(host.map(|host| {
            let diagnostics = moss_voice_server::DiagnosticsLog::new();
            if let Some(trace) = host.api_service.process_trace() {
                diagnostics.set_process_trace(trace.clone());
                moss_voice_providers::local::set_helper_observer(Some(Arc::new(
                    TraceHelperObserver {
                        trace: trace.clone(),
                    },
                )));
            }
            let _ = HOST_LOG.set(diagnostics.clone());
            // 2026-11-09：激光雕刻机相关事件也写进同一本记录，标为“激光雕刻”（语音中心“记录与诊断”）
            {
                let laser_log = diagnostics.clone();
                host.api_service
                    .set_event_sink(Arc::new(move |event: laser_api::LaserEvent| {
                        let level = match event.level {
                            laser_api::LaserEventLevel::Info => moss_voice_server::EventLevel::Info,
                            laser_api::LaserEventLevel::Warn => moss_voice_server::EventLevel::Warn,
                            laser_api::LaserEventLevel::Error => {
                                moss_voice_server::EventLevel::Error
                            }
                        };
                        let mut record = EventRecord::new(level, DiagCategory::Device, event.text);
                        if let Some(hint) = event.hint {
                            record = record.hint(hint);
                        }
                        if let Some(detail) = event.detail {
                            record = record.detail(detail);
                        }
                        laser_log.push_from(moss_voice_server::DiagSource::Laser, record);
                    }));
            }
            let setting_path = host.runtime_root.path.join("settings.json");
            if let Err(error) = moss_agent::AgentCoordinator::attach(
                &host.api_service,
                host.db.clone(),
                setting_path.clone(),
                vec![host.runtime_root.path.clone()],
            ) {
                eprintln!("[moss-desktop] AgentRun 只读工具不可用：{error}");
                diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    EventRecord::warn(
                        DiagCategory::Internal,
                        "本机 Agent 的只读工具没能挂上，语音里暂时用不了 Agent 查询",
                    )
                    .hint("重启 MOSS 再试；不用 Agent 功能可以忽略")
                    .detail(&error),
                );
            }
            let (default_models_dir, fallback_models_dirs) =
                resolve_model_roots(&host.runtime_root.path);
            let mut models = ModelManager::new(default_models_dir)
                .with_fallback_roots(fallback_models_dirs);
            if !cfg!(test)
                && let Some(env_root) = moss_laya_root_env()
            {
                let e_models = env_root.join("models");
                if std::fs::create_dir_all(&e_models).is_ok() {
                    for id in moss_voice::settings::LOCAL_JEV_MODEL_IDS {
                        let installed_in_env = local_models::spec(id)
                            .is_some_and(|spec| local_models::installed(&e_models, spec));
                        if installed_in_env || models.path_if_installed(id).is_none() {
                            models = models.with_root_override(id, e_models.clone());
                        }
                    }
                }
            }
            // 2026-11-07：清掉上次中途退出（强关、崩溃、断电）留下的半截下载与解压目录
            let cleaned = models.clean_leftovers();
            moss_usb_provision::sweep_usb_temp_dir(&host.runtime_root.path.join("usb-nvs-tmp"));
            if cleaned > 0 {
                eprintln!("[moss-desktop] 已清理 {cleaned} 个本地模型下载残留");
                diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    EventRecord::info(
                        DiagCategory::Model,
                        format!(
                            "已清理 {cleaned} 个本地模型下载残留（上次下载中途退出留下的半截文件）"
                        ),
                    ),
                );
            }
            // 2026-11-07：macOS / Linux 上 MOSS 崩溃或被强制结束后，OneJev 推理进程会被系统收养继续占着内存；
            // 启动时只结束 MOSS 模型目录里 OneJev 权重对应、父进程已不在的那些（Windows 由 Job Object 自动处理）
            if !cfg!(test) {
                let mut roots = models.all_roots();
                roots.sort();
                roots.dedup();
                let reaped = moss_voice::onejev::reap_orphaned_llama_servers(&roots);
                if reaped > 0 {
                    eprintln!(
                        "[moss-desktop] 已结束 {reaped} 个上次异常退出遗留的本地 OneJev 推理进程"
                    );
                    diagnostics.push_from(
                        moss_voice_server::DiagSource::Moss,
                        EventRecord::warn(
                            DiagCategory::Model,
                            format!("已结束 {reaped} 个上次异常退出遗留的本地 OneJev 推理进程"),
                        )
                        .hint(
                            "说明 MOSS 上次没有正常退出（崩溃、强制结束或断电）；偶尔一次不用处理",
                        ),
                    );
                }
                let clone_root = host.runtime_root.path.join("local-tts-clone");
                let mut audiocpp_roots = roots;
                audiocpp_roots.push(clone_root.clone());
                let reaped_audio =
                    moss_voice::audio_cpp::reap_orphaned_audiocpp_servers(&audiocpp_roots);
                let _ = moss_voice::audio_cpp::sweep_leftover_audiocpp_configs(&clone_root);
                if reaped_audio > 0 {
                    eprintln!(
                        "[moss-desktop] 已结束 {reaped_audio} 个上次异常退出遗留的本地声音克隆推理进程"
                    );
                    diagnostics.push_from(
                        moss_voice_server::DiagSource::Moss,
                        EventRecord::warn(
                            DiagCategory::Model,
                            format!(
                                "已结束 {reaped_audio} 个上次异常退出遗留的本地声音克隆推理进程"
                            ),
                        )
                        .hint(
                            "说明 MOSS 上次没有正常退出（崩溃、强制结束或断电）；偶尔一次不用处理",
                        ),
                    );
                }
            }
            let memory = MemoryService::new(Arc::new(DbMemoryStore {
                db: host.db.clone(),
            }));
            let enabled = moss_voice::settings::load(&host.runtime_root.path.join("settings.json"))
                .builtin_memory_enabled;
            let _ = memory.configure(enabled, || Ok(()));
            let home_control = moss_home::HomeControlCoordinator::attach(
                &host.api_service,
                &host.db,
                host.runtime_root.path.join("settings.json"),
            );
            let speaker_backend = Arc::new(DesktopSpeakerBackend {
                db: host.db.clone(),
                helper: Mutex::new(None),
                personal: Mutex::new(None),
                tests: Mutex::new(Default::default()),
            });
            let speaker = moss_voice_server::speaker_enrollment::SpeakerEnrollment::new(
                speaker_backend.clone(),
            );
            let saved_calibration = host
                .db
                .lock()
                .ok()
                .and_then(|db| db.get_secret(SPEAKER_CALIBRATION_KEY).ok().flatten())
                .and_then(|text| serde_json::from_str::<Calibration>(&text).ok());
            let identity = Arc::new(DesktopIdentity {
                host: Arc::downgrade(&host),
                backend: speaker_backend.clone(),
                calibration: Mutex::new(saved_calibration),
                workers: Arc::new(tokio::sync::Semaphore::new(2)),
            });
            let personal = PersonContext::new(
                identity.clone(),
                Arc::new(DbPersonStore {
                    db: host.db.clone(),
                }),
            );
            if let Ok(mut slot) = speaker_backend.personal.lock() {
                *slot = Some(Arc::downgrade(&personal));
            }
            if let Ok(db) = host.db.lock()
                && let Err(error) = db.recover_firmware_downloads(now())
            {
                eprintln!("[moss-desktop] 固件下载记录恢复失败，固件传输仍禁用");
                diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    EventRecord::error(
                        DiagCategory::Firmware,
                        "固件下载记录恢复失败，这次运行不能给设备推送固件",
                    )
                    .hint("重启 MOSS 再试；一直失败请导出诊断包反馈")
                    .detail(format!("{error:?}")),
                );
            }
            let identity_for_context = identity.clone();
            let db_for_context = host.db.clone();
            let calibrated_speakers: crate::context::CalibratedSpeakersFn =
                Arc::new(move || {
                    if identity_for_context.valid_calibration().is_none() {
                        return Vec::new();
                    }
                    db_for_context
                        .lock()
                        .ok()
                        .and_then(|db| db.list_voice_speakers().ok())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|r| {
                            r.model_id == SPEAKER_MODEL && r.dimension == 192 && r.revision > 0
                        })
                        .map(|r| (r.id, r.name, r.revision))
                        .collect()
                });
            let context = crate::context::ContextHost::with_speakers(
                host.db.clone(),
                setting_path.clone(),
                calibrated_speakers,
            );
            let initial_settings = moss_voice::settings::load(&setting_path);
            let windows_old_notice = if cfg!(test) {
                None
            } else {
                detect_windows_old_moss()
            };
            if let Some(notice) = &windows_old_notice
                && let Some(event) = notice.event_record(&models, &initial_settings)
            {
                if let Some(warn) = notice.warning_text(&models, &initial_settings) {
                    eprintln!("[moss-desktop] {warn}");
                }
                diagnostics.push_from(moss_voice_server::DiagSource::Moss, event);
            }
            let initial_local = initial_settings.builtin_local;
            let initial_chat_log = initial_local.chat_log_enabled;
            let persona = Arc::new(PersonaSwitch::new(
                persona_config(&initial_local),
                Some(Arc::new(DesktopPersonaStore {
                    settings_file: setting_path.clone(),
                })),
            ));
            let builtin = Arc::new_cyclic(|weak| BuiltinVoice {
                self_weak: weak.clone(),
                host,
                memory,
                speaker,
                speaker_backend,
                personal,
                identity,
                home_control,
                lifecycle: Mutex::new(()),
                server: Arc::new(Mutex::new(None)),
                debug_trace_enabled: Arc::new(std::sync::atomic::AtomicBool::new(initial_chat_log)),
                chat_log_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                last_error: Mutex::new(None),
                diagnostics,
                music_warning: Mutex::new(None),
                speaker_warning: Mutex::new(None),
                windows_old_notice,
                models,
                laya_setup: moss_voice::laya_env::LayaSetup::new(Arc::new(
                    moss_voice::laya_env::SystemHost,
                )),
                #[cfg(test)]
                laya_root_for_test: Mutex::new(None),
                helper: Mutex::new(None),
                turn_worker: Mutex::new(None),
                speech_worker: Mutex::new(None),
                audiocpp_worker: Mutex::new(None),
                local_clone_io: Mutex::new(()),
                fallback_tts: Mutex::new(None),
                running_local: Mutex::new(None),
                chain_test_lock: Mutex::new(()),
                jev_start_lock: Arc::new(Mutex::new(())),
                local_vision: Arc::new(LocalVision::default()),
                colab_cli: Arc::new(Mutex::new(ColabCliState::default())),
                colab_ref_io: Mutex::new(()),
                #[cfg(test)]
                colab_cli_installed_for_test: std::sync::atomic::AtomicBool::new(true),
                laya: Arc::new(Mutex::new(LocalLayaRunner {
                    active_model: None,
                    starting_model: None,
                    starting_cancel: None,
                    cancelling_model: None,
                    child: None,
                    job: None,
                    onejev: None,
                    generation: 0,
                    last_failure: None,
                })),
                context,
                persona,
                status_probe_cache: Mutex::new(None),
                tts_preview_warmer: (
                    Mutex::new(TtsPreviewWarmState::default()),
                    std::sync::Condvar::new(),
                ),
                wakeword_base_fp_cache: Mutex::new(None),
                usb_provision_lock: Mutex::new(()),
            });
            let weak_for_tts_cache = Arc::downgrade(&builtin);
            builtin.models.set_on_installed(move |installed_id| {
                if let Some(target) = weak_for_tts_cache.upgrade() {
                    if installed_id == local_models::XIAOZHI_NOTO_V1_FULL_ID {
                        target.sync_running_full_font_dir();
                        target.diagnostics.push_from(
                            moss_voice_server::DiagSource::Moss,
                            EventRecord::from(
                                "小智全量补字库（Noto v1 · 48,662 字）已就绪，生僻字与多语种动态补字已热生效",
                            ),
                        );
                    } else {
                        target.schedule_tts_preview_warmup(installed_id);
                    }
                }
            });
            builtin
        }))
}

/// 本地 Jev（OneJev / Laya）启动失败 → 诊断事件。
pub(super) fn local_jev_failure(what: &str, err: &str) -> EventRecord {
    EventRecord::warn(DiagCategory::Model, what)
            .hint("Jev 只做信息筛选，失败时语音照常可用；到“服务 → 本地模型”检查模型是否下载完整、运行环境是否就绪")
            .detail(err)
}

/// 内置语音服务启动失败 → 诊断事件（端口占用归网络类，其余按配置类）。
fn start_failure_event(what: &str, error: &CenterError) -> EventRecord {
    let category = if error.message.contains("端口") {
        DiagCategory::Network
    } else {
        DiagCategory::Config
    };
    EventRecord::error(category, format!("{what}：{}", error.message))
        .hint("按提示改好后，在“服务”标签点“启动”重试")
        .detail(format!("错误码 {}", error.code))
}

/// 语音链路为“内置服务”时，在后台启动内置语音服务；若启用了本地 Laya 加工反馈，即使当前非内置语音模式也在后台拉起本地 Laya。
pub fn autostart(state: &BuiltinState) {
    let Some(builtin) = state.0.clone() else {
        return;
    };
    // 2026-11-09 自动保持全屏画风：开关默认关，开着时每 20 秒看一眼在线小智（只持弱引用，随 MOSS 退出）
    let weak = Arc::downgrade(&builtin);
    let _ = std::thread::Builder::new()
        .name("voice-screen-pack-keeper".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(20));
                let Some(builtin) = weak.upgrade() else {
                    break;
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                builtin.screen_pack_auto_tick(now);
            }
        });
    // 已安装但尚未补齐试听缓存的本地 TTS 模型，启动 2 秒后在后台静默预热缓存（不抢首屏 CPU）
    let weak_for_tts_warm = Arc::downgrade(&builtin);
    let _ = std::thread::Builder::new()
        .name("voice-tts-preview-boot-check".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let Some(builtin) = weak_for_tts_warm.upgrade() else {
                return;
            };
            for spec in local_models::CATALOG
                .iter()
                .filter(|s| s.kind == ModelKind::Tts)
            {
                builtin.schedule_tts_preview_warmup(spec.id);
            }
        });
    let start_voice = builtin.settings().mode == VoiceMode::Builtin;
    let start_feedback_laya = builtin.should_run_local_laya_for_feedback();
    if !start_voice && !start_feedback_laya {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("voice-builtin-autostart".into())
        .spawn(move || {
            if start_feedback_laya && let Err(err) = builtin.ensure_laya_running(false) {
                eprintln!("[moss-desktop] 本地 Laya 决策服务自动启动失败：{err}");
                builtin.diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    local_jev_failure("MOSS 启动时自动拉起本地 Jev 决策服务失败", &err),
                );
            }
            if start_voice && let Err(error) = builtin.start() {
                eprintln!("[moss-desktop] 内置语音服务自动启动失败：{}", error.message);
                if let Ok(mut last) = builtin.last_error.lock() {
                    *last = Some(error.message);
                }
            }
        });
}

impl BuiltinVoice {
    fn start_locked(&self) -> Result<(), CenterError> {
        let result = self.start_locked_inner();
        if let Err(error) = &result {
            self.diagnostics.push_from(
                moss_voice_server::DiagSource::Moss,
                start_failure_event("内置语音服务启动失败", error),
            );
        } else {
            // n74：选了本地视觉模型时在后台预加载，设备第一次看图不用等
            self.sync_local_vision(true);
            // n83：自动在后台补齐小智服务端全量字库包（单测环境不发起真实外网请求）
            #[cfg(not(test))]
            {
                let _ = self.ensure_full_font_ready_with_base(None);
            }
        }
        result
    }

    fn start_locked_inner(&self) -> Result<(), CenterError> {
        if !self.running() {
            let settings = self.settings();
            let colab = self.active_colab(&settings)?;
            struct StartLocalCleanupGuard {
                armed: bool,
                helper: Option<LocalHelper>,
                turn_worker: Option<TurnHelper>,
                speech_worker: Option<SpeechCppWorker>,
                audiocpp_worker: Option<AudioCppWorker>,
                fallback_tts: Option<Arc<LazyLocalFallbackTts>>,
            }
            impl Drop for StartLocalCleanupGuard {
                fn drop(&mut self) {
                    if !self.armed {
                        return;
                    }
                    if let Some(fb) = self.fallback_tts.take() {
                        fb.terminate_all();
                    }
                    if let Some(worker) = self.turn_worker.take() {
                        worker.terminate();
                    }
                    if let Some(worker) = self.speech_worker.take() {
                        worker.terminate();
                    }
                    if let Some(worker) = self.audiocpp_worker.take() {
                        worker.terminate();
                    }
                    if let Some(helper) = self.helper.take() {
                        helper.terminate();
                    }
                }
            }

            let (mut local, helper_and_spec) =
                self.local_parts(&settings.builtin_local, colab.is_some())?;
            let (helper, running_spec) = match helper_and_spec {
                Some((h, spec)) => (Some(h), Some(spec)),
                None => (None, None),
            };
            let has_local_tts = local.tts.is_some();
            let speech_worker = local.speech_worker.take();
            let audiocpp_worker = local.audiocpp_worker.take();
            let turn_worker = local.turn_worker.take();
            let fallback_tts_handle = local.fallback_tts_handle.take();
            let mut local_cleanup = StartLocalCleanupGuard {
                armed: true,
                helper: helper.clone(),
                turn_worker: turn_worker.clone(),
                speech_worker: speech_worker.clone(),
                audiocpp_worker: audiocpp_worker.clone(),
                fallback_tts: fallback_tts_handle.clone(),
            };
            let mut conversation = self.conversation(
                &settings.builtin_models,
                &mut local,
                &settings.builtin_local,
                colab.as_ref(),
            )?;
            // n110：对话模式确定后再汇总每类本地模型的实际情况（回声模式下识别/合成/语义判停未挂载，不虚报“已加载”）
            let local_models = self.local_models_report(
                &settings.builtin_local,
                running_spec.as_ref(),
                &local,
                has_local_tts,
                colab.is_some(),
                conversation.is_some(),
            );
            // No music scanning while disabled or chat unavailable;换机或拔盘目录不可用时跳过音乐索引，不阻断内置语音主服务启动。
            let (music, next_music_warning) =
                scan_music_for_start(&settings.builtin_music, conversation.is_some());
            if let (Some(index), Some((chat, _))) = (&music, conversation.as_mut()) {
                chat.tools = Arc::new(moss_voice_server::music::MusicTools::new(
                    chat.tools.clone(),
                    index.clone(),
                ));
            }
            let model_labels = conversation
                .as_ref()
                .map(|(chat, _)| chat.settings.model_labels.clone());
            let defaults = ServerConfig::default();
            let playback_start_ms = match &conversation {
                Some((_, true)) => STREAMING_PLAYBACK_START_MS,
                _ => defaults.playback_start_ms,
            };
            let config = ServerConfig {
                ws_port: settings.builtin_ws_port,
                http_port: settings.builtin_http_port,
                mode: if conversation.is_some() {
                    ServerMode::Chat
                } else {
                    ServerMode::Echo
                },
                conversation: conversation.map(|(conversation, _)| Arc::new(conversation)),
                music,
                vision: Some(Arc::new(DesktopVisionFactory {
                    host: self.host.clone(),
                    local: self.local_vision.clone(),
                })),
                firmware: Some(Arc::new(DesktopFirmwareHost {
                    db: self.host.db.clone(),
                    dir: self.firmware_dir(),
                })),
                vad_factory: local.vad.take(),
                barge_in: settings.builtin_local.barge_in,
                barge_in_ms: settings.builtin_local.barge_in_ms,
                playback_start_ms,
                screen_card_style: moss_voice_server::ScreenCardStyle::from_setting(
                    &settings.builtin_local.screen_card_style,
                ),
                diagnostics: self.diagnostics.clone(),
                // n71 常连模式（默认关）
                mqtt: settings
                    .builtin_mqtt_enabled
                    .then_some(moss_voice_server::MqttConfig {
                        port: settings.builtin_mqtt_port,
                        udp_port: settings.builtin_udp_port,
                    }),
                full_font_dir: Some(self.full_font_dir()),
                debug_epoch: self.chat_log_epoch.clone(),
                eager_device_mcp: true,
                ..defaults
            };
            let registry = Arc::new(DbRegistry {
                db: self.host.db.clone(),
                host: self.host.clone(),
                settings_file: self.settings_file(),
                chat_log_enabled: self.debug_trace_enabled.clone(),
                chat_log_epoch: self.chat_log_epoch.clone(),
            });
            let server = VoiceServer::start(config, registry)
                .map_err(|message| CenterError::new("builtin_start_failed", 409, message))?;
            self.trace_service_start(&server, local_models, model_labels);
            server.prewarm_speech_cache();
            {
                let mut guard = self
                    .server
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // 同 server 锁重读最新设置，防止启动慢路径期间用户关闭对话记录却被开场旧快照覆盖
                let chat_log_on = moss_voice::settings::load(&self.settings_file())
                    .builtin_local
                    .chat_log_enabled;
                self.debug_trace_enabled
                    .store(chat_log_on, std::sync::atomic::Ordering::Release);
                server.set_debug_enabled(chat_log_on);
                *guard = Some(server);
            }
            if let Ok(mut slot) = self.helper.lock() {
                *slot = helper;
            }
            if let Ok(mut slot) = self.turn_worker.lock() {
                *slot = turn_worker;
            }
            if let Ok(mut slot) = self.speech_worker.lock() {
                *slot = speech_worker;
            }
            if let Ok(mut slot) = self.audiocpp_worker.lock() {
                *slot = audiocpp_worker;
            }
            if let Ok(mut slot) = self.fallback_tts.lock() {
                *slot = fallback_tts_handle;
            }
            if let Ok(mut slot) = self.running_local.lock() {
                *slot = running_spec;
            }
            local_cleanup.armed = false;
            if let Some(warning) = &next_music_warning {
                self.diagnostics.push_from(
                    moss_voice_server::DiagSource::Moss,
                    EventRecord::warn(DiagCategory::Config, warning.clone())
                        .hint("在“能力 → 本地音乐”里重新选择音乐文件夹"),
                );
            }
            if let Ok(mut slot) = self.music_warning.lock() {
                *slot = next_music_warning;
            }
            // 顶栏：服务已运行、还没有设备连接
            laser_host::set_voice_device_sessions(&self.host.api_service, Some(0));
            laser_host::set_voice_pulse(&self.host.api_service, None, 0);
            if let Ok(mut last) = self.last_error.lock() {
                *last = None;
            }
        }
        if self.should_run_local_laya()
            && let Err(err) = self.ensure_laya_running(false)
        {
            eprintln!("[moss-desktop] 本地 Laya 决策服务启动失败：{err}");
            self.diagnostics.push_from(
                moss_voice_server::DiagSource::Moss,
                local_jev_failure("开启内置语音时拉起本地 Jev 决策服务失败", &err),
            );
        }
        Ok(())
    }

    fn stop_locked(&self) {
        self.stop_locked_inner(false);
        // n74：手动停止服务与退出时结束本地视觉推理进程；设置变更触发的重启不经过这里，模型保持加载
        self.local_vision.configure(None);
    }

    pub(super) fn stop_locked_inner(&self, keep_local_jev: bool) {
        self.stop_locked_for(keep_local_jev, false);
    }

    /// 同 [`Self::stop_locked_inner`]；`restarting` 为设置变更触发的重启（停了马上再起，完整过程记录如实写明）。
    fn stop_locked_for(&self, keep_local_jev: bool, restarting: bool) {
        self.personal.directory_changed();
        let _ = self.speaker.cancel();
        let had_speaker_helper = self
            .speaker_backend
            .helper
            .lock()
            .ok()
            .and_then(|mut helper| helper.take())
            .is_some();
        if let Ok(mut slot) = self.music_warning.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = self.speaker_warning.lock() {
            *slot = None;
        }
        let server = self
            .server
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let stopped_server = server.is_some();
        if let Some(server) = server {
            server.stop();
            laser_host::set_voice_device_sessions(&self.host.api_service, None);
            laser_host::set_voice_pulse(&self.host.api_service, None, 0);
        }
        // 结束辅助程序、判停子进程、声纹、本地兜底句柄与 Qwen3-TTS 子进程（记下这次真正结束了哪些）
        let mut ended: Vec<&str> = Vec::new();
        if had_speaker_helper {
            ended.push("声纹程序");
        }
        if let Ok(mut slot) = self.running_local.lock() {
            *slot = None;
        }
        if let Ok(mut fb) = self.fallback_tts.lock()
            && let Some(fb) = fb.take()
        {
            fb.terminate_all();
            ended.push("本地兜底合成");
        }
        if let Ok(mut worker) = self.turn_worker.lock()
            && let Some(w) = worker.take()
        {
            w.terminate();
            ended.push("语义判停程序");
        }
        if let Ok(mut worker) = self.speech_worker.lock()
            && let Some(w) = worker.take()
        {
            w.terminate();
            ended.push("Qwen3-TTS 合成程序");
        }
        if let Ok(mut worker) = self.audiocpp_worker.lock()
            && let Some(w) = worker.take()
        {
            w.terminate();
            ended.push("Qwen3-TTS 克隆程序");
        }
        if let Ok(mut helper) = self.helper.lock()
            && let Some(h) = helper.take()
        {
            h.terminate();
            ended.push("本地模型程序");
        }
        if !keep_local_jev && !self.should_run_local_laya_for_feedback() {
            let had_laya = self.laya.lock().ok().is_some_and(|st| {
                st.child.is_some() || st.starting_model.is_some() || st.onejev.is_some()
            });
            self.stop_laya();
            if had_laya {
                ended.push("本地 Jev 服务");
            }
        }
        if stopped_server && let Some(trace) = self.trace_on() {
            let mut step = if restarting {
                "内置语音服务停止（设置变了，马上按新设置重启）".to_owned()
            } else {
                "内置语音服务停止".to_owned()
            };
            if !ended.is_empty() {
                step.push_str(&format!("，结束了{}", ended.join("、")));
            }
            trace.record(
                "voice",
                SERVICE_FLOW,
                "service_stop",
                &step,
                json!({"restarting": restarting, "ended": ended}),
            );
        }
    }

    pub fn start(&self) -> Result<Value, CenterError> {
        {
            let _lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.start_locked()?;
        }
        Ok(self.status())
    }

    pub fn stop(&self) -> Value {
        {
            let _lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.stop_locked();
        }
        self.status()
    }

    pub(super) fn restart_if_running(&self) -> Result<bool, CenterError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.running() {
            return Ok(false);
        }
        let keep_local_jev = self.should_run_local_laya();
        self.stop_locked_for(keep_local_jev, true);
        if let Err(err) = self.start_locked() {
            if !self.should_run_local_laya_for_feedback() {
                self.stop_laya();
            }
            return Err(err);
        }
        Ok(true)
    }

    /// 完整过程记录打开时返回记录器（关着时为 `None`，调用方什么都不做）。
    fn trace_on(&self) -> Option<laser_runtime::ProcessTrace> {
        self.host
            .api_service
            .process_trace()
            .filter(|trace| trace.enabled())
            .cloned()
    }

    /// 各类本地模型（判停、降噪、命令词、语义判停、识别、合成）这次启动的实际情况。
    fn local_models_report(
        &self,
        local: &BuiltinLocal,
        running: Option<&RunningLocalSpec>,
        parts: &LocalParts,
        has_local_tts: bool,
        colab_tts: bool,
        chat_active: bool,
    ) -> Vec<Value> {
        if self.trace_on().is_none() {
            return Vec::new();
        }
        let loaded = running
            .map(RunningLocalSpec::loaded_ids)
            .unwrap_or_default();
        let local_vad = loaded.iter().any(|(kind, _)| *kind == ModelKind::Vad);
        let local_asr = chat_active && loaded.iter().any(|(kind, _)| *kind == ModelKind::Asr);
        [
            (ModelKind::Vad, "判停"),
            (ModelKind::Denoise, "降噪"),
            (ModelKind::Kws, "命令词抗噪"),
            (ModelKind::Turn, "语义判停"),
            (ModelKind::Asr, "识别"),
            (ModelKind::Tts, "合成"),
        ]
        .into_iter()
        .map(|(kind, name)| {
            let selected = local.selected(kind);
            let installed = selected.map(|spec| self.models.path_if_installed(spec.id).is_some());
            let chat_only = matches!(kind, ModelKind::Turn | ModelKind::Asr | ModelKind::Tts);
            let loaded_id = if chat_only && !chat_active {
                None
            } else {
                match kind {
                    ModelKind::Denoise => parts.denoise_active.filter(|_| local_vad || local_asr),
                    ModelKind::Kws => parts.kws_active,
                    ModelKind::Turn => parts.turn_active,
                    // 本地合成可能走辅助程序，也可能走 speech.cpp（Qwen3-TTS）：挂进了对话就算加载成功
                    ModelKind::Tts => has_local_tts
                        .then_some(selected)
                        .flatten()
                        .map(|spec| spec.id),
                    _ => loaded
                        .iter()
                        .find(|(loaded_kind, _)| *loaded_kind == kind)
                        .map(|(_, id)| *id),
                }
            };
            let status = match (selected, installed, loaded_id) {
                (None, _, _) => match kind {
                    ModelKind::Vad => "未开启（用能量判停）".to_owned(),
                    ModelKind::Asr | ModelKind::Tts => "未开启（用云端）".to_owned(),
                    _ => "未开启".to_owned(),
                },
                (Some(_), Some(false), _) => "选了但没安装，这次没用上".to_owned(),
                (Some(_), _, Some(_)) => "已加载".to_owned(),
                (Some(_), _, None) => match kind {
                    _ if chat_only && !chat_active => {
                        "没用上：未配好对话模型，当前为回声测试模式".to_owned()
                    }
                    ModelKind::Denoise if !local_vad && !local_asr => {
                        "没用上：降噪只作用于本地判停与本地识别，两者都没开".to_owned()
                    }
                    ModelKind::Kws if !local_vad => {
                        "没用上：命令词检测跑在本地判停上，本地判停没开".to_owned()
                    }
                    ModelKind::Tts if colab_tts => "没用上：合成由 Colab 接管".to_owned(),
                    _ => "没用上".to_owned(),
                },
            };
            json!({
                "kind": name,
                "model_id": selected.map(|spec| spec.id),
                "model": selected.map(|spec| spec.label),
                "installed": installed,
                "loaded": loaded_id.is_some(),
                "status": status,
            })
        })
        .collect()
    }

    /// 服务启动成功：写一步“启动了什么”（端口、对话用的模型、每类本地模型的情况）。
    fn trace_service_start(
        &self,
        server: &VoiceServer,
        local_models: Vec<Value>,
        model_labels: Option<Vec<String>>,
    ) {
        let Some(trace) = self.trace_on() else {
            return;
        };
        let summary: Vec<String> = local_models
            .iter()
            .map(|item| {
                let kind = item["kind"].as_str().unwrap_or("");
                match item["model"].as_str() {
                    Some(model) => {
                        format!("{kind} {model} {}", item["status"].as_str().unwrap_or(""))
                    }
                    None => format!("{kind} {}", item["status"].as_str().unwrap_or("")),
                }
            })
            .collect();
        let step = format!(
            "内置语音服务启动：设备连接端口 {}，OTA 端口 {}；{}；本地模型：{}",
            server.ws_port(),
            server.http_port(),
            match &model_labels {
                Some(labels) => format!("对话模式（{}）", labels.join("、")),
                None => "回声测试模式（没配好识别 / 大模型 / 合成）".to_owned(),
            },
            summary.join("；")
        );
        trace.record(
            "voice",
            SERVICE_FLOW,
            "service_start",
            &step,
            json!({
                "ws_port": server.ws_port(),
                "http_port": server.http_port(),
                "mqtt": server.mqtt_ports().map(|ports| json!({"port": ports.port, "udp_port": ports.udp_port})),
                "mode": if model_labels.is_some() { "chat" } else { "echo" },
                "models": model_labels,
                "local_models": local_models,
            }),
        );
    }

    pub(super) fn running(&self) -> bool {
        self.server
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }
}
