//! 声纹登记后端、简易标定与说话人身份识别，以及声纹管理命令。

use super::*;

/// `tests`：简易标定时每人另录的测试音向量，只在内存里，算完阈值或删除声纹即丢弃。
pub(super) struct DesktopSpeakerBackend {
    pub(super) db: Arc<Mutex<laser_runtime::RuntimeDb>>,
    pub(super) helper: Mutex<Option<moss_voice_providers::local::SpeakerHelper>>,
    pub(super) personal:
        Mutex<Option<std::sync::Weak<moss_voice_server::person_context::PersonContext>>>,
    pub(super) tests: Mutex<std::collections::HashMap<String, Vec<Vec<f32>>>>,
}
impl moss_voice_server::speaker_enrollment::EnrollmentBackend for DesktopSpeakerBackend {
    fn save_test(&self, speaker_id: &str, clips: &[Vec<f32>]) -> Result<(), String> {
        let known = self
            .db
            .lock()
            .map_err(|_| "数据库不可用")?
            .list_voice_speakers()
            .map_err(|_| "读取声纹失败")?
            .iter()
            .any(|r| r.id == speaker_id);
        if !known {
            return Err("这个人的声纹已删除".into());
        }
        self.tests
            .lock()
            .map_err(|_| "标定数据不可用")?
            .insert(speaker_id.into(), clips.to_vec());
        Ok(())
    }
    fn embedding(&self, pcm: &[i16], generation: u64) -> Result<Vec<f32>, String> {
        let helper = self
            .helper
            .lock()
            .map_err(|_| "声纹辅助程序不可用")?
            .clone()
            .ok_or("声纹辅助程序未准备")?;
        helper
            .embedding_blocking(pcm, 16000, generation)
            .map_err(|_| "声纹计算失败".into())
    }
    fn save(&self, name: &str, clips: &[Vec<f32>]) -> Result<(), String> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| "声纹编号生成失败")?;
        let id = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        self.db
            .lock()
            .map_err(|_| "数据库不可用")?
            .enroll_voice_speaker(&id, name, "campplus_sv_zh_cn_16k_common", clips, now())
            .map_err(|_| "声纹登记失败")?;
        if let Some(personal) = self
            .personal
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(std::sync::Weak::upgrade))
        {
            personal.directory_changed();
        }
        Ok(())
    }
}
pub(super) const SPEAKER_CALIBRATION_KEY: &str = "voice.speaker.calibration";
pub(super) const SPEAKER_MODEL: &str = "campplus_sv_zh_cn_16k_common";
/// 简易标定结果（2026-10-14 用户决定，取代“源码写死 None”）：没有标定就不识别身份。
/// 只对标定时的登记名单（编号 + 版本）有效；增删或重新登记任何人都会让它失效，需要重新标定。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Calibration {
    pub(super) report: CalibrationReport,
    pub(super) directory: String,
    pub(super) calibrated_at: f64,
}
/// 登记名单的比较键：CampPlus 192 维声纹的“编号:版本”，排序后拼接。
pub(super) fn speaker_directory<'a>(items: impl Iterator<Item = (&'a str, i64)>) -> String {
    let mut keys = items
        .map(|(id, revision)| format!("{id}:{revision}"))
        .collect::<Vec<_>>();
    keys.sort();
    keys.join(",")
}
pub(super) struct DesktopIdentity {
    pub(super) host: std::sync::Weak<laser_host::HostApplication>,
    pub(super) backend: Arc<DesktopSpeakerBackend>,
    pub(super) calibration: Mutex<Option<Calibration>>,
    pub(super) workers: Arc<tokio::sync::Semaphore>,
}
impl DesktopIdentity {
    /// 有标定且登记名单没变时返回标定结果。
    pub(super) fn valid_calibration(&self) -> Option<Calibration> {
        let calibration = self.calibration.lock().ok()?.clone()?;
        let rows = self.backend.db.lock().ok()?.list_voice_speakers().ok()?;
        let current = speaker_directory(
            rows.iter()
                .filter(|r| r.model_id == SPEAKER_MODEL && r.dimension == 192)
                .map(|r| (r.id.as_str(), r.revision)),
        );
        (current == calibration.directory).then_some(calibration)
    }
}
impl IdentityResolver for DesktopIdentity {
    fn identify<'a>(
        &'a self,
        _: &'a ToolContext,
        pcm: &'a [i16],
        rate: u32,
        request: IdentityRequest,
    ) -> moss_voice_providers::BoxFuture<'a, Option<PersonScope>> {
        Box::pin(async move {
            let calibration = self.calibration.lock().ok()?.clone()?;
            let (threshold, margin, directory) = (
                calibration.report.threshold,
                calibration.report.margin,
                calibration.directory,
            );
            // Keep the physical worker slot until a cancelled blocking
            // helper operation actually finishes; no unbounded queue.
            let worker = self.workers.clone().try_acquire_owned().ok()?;
            let backend = self.backend.clone();
            let pcm = pcm.to_vec();
            tokio::task::spawn_blocking(move || {
                let _worker = worker;
                let pcm = if rate == 16000 {
                    pcm
                } else {
                    moss_voice_providers::resample::Resampler::new(rate, 16000).process(&pcm)
                };
                let helper = backend.helper.lock().ok()?.clone()?;
                let embedding = helper
                    .embedding_blocking(&pcm, 16000, request.utterance_generation)
                    .ok()?;
                let templates = backend
                    .db
                    .lock()
                    .ok()?
                    .native_voice_speaker_templates()
                    .ok()?
                    .into_iter()
                    .map(|t| SpeakerTemplate {
                        id: t.id,
                        revision: t.revision,
                        embedding: t.embedding,
                    })
                    .collect::<Vec<_>>();
                // 登记名单在标定后变过：阈值不再可信，不识别
                if speaker_directory(templates.iter().map(|t| (t.id.as_str(), t.revision)))
                    != directory
                {
                    return None;
                }
                match_embedding(
                    &embedding,
                    &templates,
                    request.directory_generation,
                    threshold,
                    margin,
                )
            })
            .await
            .ok()
            .flatten()
        })
    }
    fn permitted(&self, ctx: &ToolContext) -> bool {
        self.host.upgrade().is_some_and(|h| {
            h.api_service
                .voice_session_allowed(&ctx.device_id, &ctx.session_id)
        })
    }
    fn device_allowed(&self, device: &str) -> bool {
        self.device_generation(device).is_some()
    }
    fn device_generation(&self, device: &str) -> Option<u64> {
        let host = self.host.upgrade()?;
        let db = host.db.lock().ok()?;
        let row = db.voice_device(device).ok()??;
        if !row.is_allowed() {
            return None;
        }
        Some(row.decided_at?.to_bits())
    }
    fn current(&self, scope: &PersonScope) -> bool {
        self.backend
            .db
            .lock()
            .ok()
            .and_then(|db| db.list_voice_speakers().ok())
            .is_some_and(|rows| {
                rows.iter().any(|r| {
                    r.id == scope.id()
                        && r.revision == scope.revision()
                        && r.model_id == "campplus_sv_zh_cn_16k_common"
                        && r.dimension == 192
                })
            })
    }
}

impl BuiltinVoice {
    pub(super) fn prepare_speaker_helper(&self) -> Result<(), CenterError> {
        let mut helper = self
            .speaker_backend
            .helper
            .lock()
            .map_err(|_| internal("声纹辅助程序锁失败"))?;
        if helper.as_ref().is_none_or(|h| !h.is_alive()) {
            let exe = helper_path().ok_or_else(|| {
                CenterError::new("speaker_helper_missing", 409, "请先准备声纹辅助程序")
            })?;
            *helper = Some(
                moss_voice_providers::local::SpeakerHelper::spawn(
                    &exe,
                    self.models.root_for("campplus_sv_zh_cn_16k_common"),
                )
                .map_err(|_| {
                    CenterError::new("speaker_model_missing", 409, "声纹模型未安装或校验失败")
                })?,
            );
        }
        Ok(())
    }

    pub fn speaker_manage(&self, input: &Value) -> Result<Value, CenterError> {
        let invalid = || CenterError::new("speaker_invalid", 400, "声纹操作格式无效");
        let object = input.as_object().ok_or_else(invalid)?;
        let action = input["action"].as_str().ok_or_else(invalid)?;
        let allowed: &[&str] = match action {
            "status" | "cancel" => &["action"],
            "begin" => &["action", "device_id", "session_id", "name", "confirmed"],
            "delete" => &["action", "id", "revision", "confirmed"],
            "save_memory" => &["action", "id", "revision", "summary"],
            "personal_mode" => &["action", "enabled", "confirmed"],
            "begin_test" => &["action", "device_id", "session_id", "id", "confirmed"],
            "calibrate" | "reset_calibration" => &["action", "confirmed"],
            _ => return Err(invalid()),
        };
        if object.len() != allowed.len() || object.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(invalid());
        }
        match action {
            "begin" => {
                if input["confirmed"].as_bool() != Some(true) {
                    return Err(invalid());
                }
                let device = input["device_id"].as_str().ok_or_else(invalid)?;
                let session = input["session_id"].as_str().ok_or_else(invalid)?;
                let name = input["name"].as_str().ok_or_else(invalid)?;
                let chat = self
                    .server
                    .lock()
                    .map_err(|_| internal("语音服务锁失败"))?
                    .as_ref()
                    .is_some_and(|s| {
                        s.status().mode == ServerMode::Chat
                            && s.active_device_sessions()
                                .iter()
                                .any(|r| r.device_id == device && r.session_id == session)
                    });
                if !chat {
                    return Err(CenterError::new(
                        "speaker_session_required",
                        409,
                        "请选择已连接的对话模式设备",
                    ));
                }
                let context = laser_contracts::CallerContext::Voice {
                    device_id: device.into(),
                    session_id: session.into(),
                };
                let permit = self
                    .host
                    .api_service
                    .external_voice_permit(&context)
                    .map_err(|_| CenterError::new("speaker_revoked", 403, "当前设备会话已撤销"))?;
                self.prepare_speaker_helper()?;
                self.speaker
                    .begin(device, session, name, permit, true)
                    .map_err(|code| {
                        CenterError::new(
                            code,
                            409,
                            "登记无法启动，请检查会话、名称或正在进行的登记",
                        )
                    })?;
            }
            "cancel" => {
                self.speaker
                    .cancel()
                    .map_err(|_| internal("取消声纹登记失败"))?;
            }
            // 简易标定（2026-10-14）：已登记的人各录三段测试音 → 计算阈值并自检 → 通过才开放识别
            "begin_test" => {
                if input["confirmed"].as_bool() != Some(true) {
                    return Err(invalid());
                }
                let device = input["device_id"].as_str().ok_or_else(invalid)?;
                let session = input["session_id"].as_str().ok_or_else(invalid)?;
                let id = input["id"].as_str().ok_or_else(invalid)?;
                let known = self
                    .host
                    .db
                    .lock()
                    .map_err(|_| internal("数据库锁失败"))?
                    .list_voice_speakers()
                    .map_err(|_| internal("读取声纹失败"))?
                    .iter()
                    .any(|r| r.id == id);
                if !known {
                    return Err(CenterError::new(
                        "speaker_conflict",
                        409,
                        "这个人的声纹已不存在，请刷新",
                    ));
                }
                let chat = self
                    .server
                    .lock()
                    .map_err(|_| internal("语音服务锁失败"))?
                    .as_ref()
                    .is_some_and(|s| {
                        s.status().mode == ServerMode::Chat
                            && s.active_device_sessions()
                                .iter()
                                .any(|r| r.device_id == device && r.session_id == session)
                    });
                if !chat {
                    return Err(CenterError::new(
                        "speaker_session_required",
                        409,
                        "请选择已连接的对话模式设备",
                    ));
                }
                let context = laser_contracts::CallerContext::Voice {
                    device_id: device.into(),
                    session_id: session.into(),
                };
                let permit = self
                    .host
                    .api_service
                    .external_voice_permit(&context)
                    .map_err(|_| CenterError::new("speaker_revoked", 403, "当前设备会话已撤销"))?;
                self.prepare_speaker_helper()?;
                self.speaker
                    .begin_test(device, session, id, permit, true)
                    .map_err(|code| {
                        CenterError::new(
                            code,
                            409,
                            "测试音采集无法启动，请检查会话或正在进行的登记",
                        )
                    })?;
            }
            "calibrate" => {
                if input["confirmed"].as_bool() != Some(true) {
                    return Err(invalid());
                }
                let templates = self
                    .host
                    .db
                    .lock()
                    .map_err(|_| internal("数据库锁失败"))?
                    .native_voice_speaker_templates()
                    .map_err(|_| internal("读取声纹失败"))?
                    .into_iter()
                    .map(|t| SpeakerTemplate {
                        id: t.id,
                        revision: t.revision,
                        embedding: t.embedding,
                    })
                    .collect::<Vec<_>>();
                let samples = self
                    .speaker_backend
                    .tests
                    .lock()
                    .map_err(|_| internal("标定数据不可用"))?
                    .iter()
                    .flat_map(|(id, clips)| {
                        clips.iter().map(|clip| CalibrationSample {
                            speaker_id: id.clone(),
                            embedding: clip.clone(),
                        })
                    })
                    .collect::<Vec<_>>();
                let report = moss_voice_server::person_context::calibrate(&templates, &samples)
                    .map_err(|reason| {
                        CenterError::new("speaker_calibration_failed", 409, reason)
                    })?;
                let calibration = Calibration {
                    report,
                    directory: speaker_directory(
                        templates.iter().map(|t| (t.id.as_str(), t.revision)),
                    ),
                    calibrated_at: now(),
                };
                let text = serde_json::to_string(&calibration)
                    .map_err(|_| internal("标定结果序列化失败"))?;
                self.host
                    .db
                    .lock()
                    .map_err(|_| internal("数据库锁失败"))?
                    .set_secret(SPEAKER_CALIBRATION_KEY, &text, now())
                    .map_err(|_| internal("保存标定结果失败"))?;
                *self
                    .identity
                    .calibration
                    .lock()
                    .map_err(|_| internal("标定状态锁失败"))? = Some(calibration);
                self.speaker_backend
                    .tests
                    .lock()
                    .map_err(|_| internal("标定数据不可用"))?
                    .clear();
                self.personal.directory_changed();
                self.context.speakers_invalidated();
                if self.settings().builtin_person_memory_enabled {
                    self.restart_if_running()?;
                }
            }
            "reset_calibration" => {
                if input["confirmed"].as_bool() != Some(true) {
                    return Err(invalid());
                }
                self.host
                    .db
                    .lock()
                    .map_err(|_| internal("数据库锁失败"))?
                    .delete_secret(SPEAKER_CALIBRATION_KEY)
                    .map_err(|_| internal("清除标定结果失败"))?;
                *self
                    .identity
                    .calibration
                    .lock()
                    .map_err(|_| internal("标定状态锁失败"))? = None;
                self.speaker_backend
                    .tests
                    .lock()
                    .map_err(|_| internal("标定数据不可用"))?
                    .clear();
                self.personal.directory_changed();
                self.context.speakers_invalidated();
                if self.settings().builtin_person_memory_enabled {
                    self.restart_if_running()?;
                }
            }
            "delete" => {
                if input["confirmed"].as_bool() != Some(true) {
                    return Err(invalid());
                }
                let id = input["id"].as_str().ok_or_else(invalid)?;
                let revision = input["revision"].as_i64().ok_or_else(invalid)?;
                if id.is_empty()
                    || id.len() > 64
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    || revision < 1
                {
                    return Err(invalid());
                }
                if !self
                    .personal
                    .person_deleted(id, || {
                        self.host
                            .db
                            .lock()
                            .map_err(|_| "数据库锁失败".to_owned())?
                            .delete_voice_speaker_if_revision(id, revision)
                            .map_err(db_error)
                    })
                    .map_err(internal)?
                {
                    return Err(CenterError::new(
                        "speaker_conflict",
                        409,
                        "声纹记录已变化，请刷新",
                    ));
                }
                self.context.person_changed(id);
                if let Ok(mut tests) = self.speaker_backend.tests.lock() {
                    tests.remove(id);
                }
            }
            "save_memory" => {
                let id = input["id"].as_str().ok_or_else(invalid)?;
                let revision = input["revision"].as_i64().ok_or_else(invalid)?;
                let text = input["summary"].as_str().ok_or_else(invalid)?;
                let row = self
                    .host
                    .db
                    .lock()
                    .map_err(|_| internal("数据库锁失败"))?
                    .list_voice_speakers()
                    .map_err(|_| internal("读取声纹失败"))?
                    .into_iter()
                    .find(|r| r.id == id)
                    .ok_or_else(invalid)?;
                let scope =
                    PersonScope::candidate(&row.id, row.revision, self.personal.generation())
                        .ok_or_else(invalid)?;
                if !self
                    .personal
                    .operator_edit(&scope, revision, text)
                    .map_err(|_| invalid())?
                {
                    return Err(CenterError::new(
                        "speaker_memory_conflict",
                        409,
                        "个人摘要已改变，请刷新后重试",
                    ));
                }
            }
            "personal_mode" => {
                let enabled = input["enabled"].as_bool().ok_or_else(invalid)?;
                input["confirmed"].as_bool().ok_or_else(invalid)?;
                if enabled
                    && (input["confirmed"].as_bool() != Some(true)
                        || self.identity.valid_calibration().is_none())
                {
                    return Err(CenterError::new(
                        "speaker_calibration_required",
                        409,
                        "请先完成声纹简易标定（登记名单变化后需重新标定）",
                    ));
                }
                if enabled {
                    self.prepare_speaker_helper()?;
                }
                self.personal.directory_changed();
                self.context.speakers_invalidated();
                moss_voice::settings::update_voice(&self.settings_file(), |settings| {
                    settings.builtin_person_memory_enabled = enabled;
                    Ok(())
                })
                .map_err(internal)?;
                self.restart_if_running()?;
            }
            _ => {}
        }
        let rows = self
            .host
            .db
            .lock()
            .map_err(|_| internal("数据库锁失败"))?
            .list_voice_speakers()
            .map_err(|_| internal("读取声纹失败"))?;
        let rows = {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            rows.into_iter()
                .map(|row| {
                    let (summary, revision) = db
                        .voice_person_summary(&row.id)
                        .map_err(|_| internal("读取个人记忆失败"))?
                        .unwrap_or((String::new(), 0));
                    let mut value =
                        serde_json::to_value(row).map_err(|_| internal("声纹元数据序列化失败"))?;
                    value["memory_summary"] = Value::String(summary);
                    value["memory_revision"] = Value::from(revision);
                    Ok(value)
                })
                .collect::<Result<Vec<_>, CenterError>>()?
        };
        let sessions = self
            .server
            .lock()
            .map_err(|_| internal("语音服务锁失败"))?
            .as_ref()
            .map(|s| s.active_device_sessions())
            .unwrap_or_default()
            .into_iter()
            .map(|r| serde_json::json!({"device_id":r.device_id,"session_id":r.session_id}))
            .collect::<Vec<_>>();
        let valid = self.identity.valid_calibration();
        let calibration = match (
            &valid,
            self.identity
                .calibration
                .lock()
                .ok()
                .and_then(|c| c.clone()),
        ) {
            (Some(c), _) => {
                serde_json::json!({"state":"valid","report":c.report,"calibrated_at":c.calibrated_at})
            }
            (None, Some(c)) => {
                serde_json::json!({"state":"stale","report":c.report,"calibrated_at":c.calibrated_at})
            }
            (None, None) => serde_json::json!({"state":"none"}),
        };
        let test_clips = self
            .speaker_backend
            .tests
            .lock()
            .map(|tests| {
                tests
                    .iter()
                    .map(|(id, clips)| (id.clone(), Value::from(clips.len())))
                    .collect::<serde_json::Map<_, _>>()
            })
            .unwrap_or_default();
        let warning = self
            .speaker_warning
            .lock()
            .ok()
            .and_then(|slot| slot.clone());
        Ok(
            serde_json::json!({"enrollment":self.speaker.status(),"speakers":rows,"sessions":sessions,"recognition_enabled":valid.is_some(),"personal_memory_enabled":valid.is_some()&&self.settings().builtin_person_memory_enabled,"calibration":calibration,"test_clips":test_clips,"warning":warning}),
        )
    }
}

#[cfg(test)]
mod speaker_directory_tests {
    /// 标定只对当时的登记名单有效：顺序无关，增删人或重新登记（版本变）都会得到不同的键。
    #[test]
    fn directory_key_changes_when_anyone_is_added_removed_or_re_enrolled() {
        let key = |items: &[(&str, i64)]| super::speaker_directory(items.iter().copied());
        assert_eq!(key(&[("a", 1), ("b", 1)]), key(&[("b", 1), ("a", 1)]));
        assert_ne!(key(&[("a", 1), ("b", 1)]), key(&[("a", 1)]));
        assert_ne!(
            key(&[("a", 1), ("b", 1)]),
            key(&[("a", 1), ("b", 1), ("c", 1)])
        );
        assert_ne!(key(&[("a", 1), ("b", 1)]), key(&[("a", 2), ("b", 1)]));
    }
}
