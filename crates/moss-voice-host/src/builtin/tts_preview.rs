//! 本地合成音色试听与后台预缓存队列。

use super::*;

#[derive(Clone)]
pub(super) enum PreviewRunner {
    Helper(LocalHelper),
    SpeechCpp(SpeechCppWorker),
    AudioCpp(AudioCppWorker),
}

impl PreviewRunner {
    fn terminate(&self) {
        match self {
            Self::Helper(h) => h.terminate_with_reason("MOSS 不再使用它，已结束"),
            Self::SpeechCpp(w) => w.terminate_with_reason("MOSS 不再使用它，已结束"),
            Self::AudioCpp(w) => w.terminate_with_reason("MOSS 不再使用它，已结束"),
        }
    }
}

#[derive(Default)]
pub(super) struct TtsPreviewWarmState {
    pub(super) queue: std::collections::VecDeque<&'static str>,
    pub(super) active: Option<&'static str>,
    pub(super) cancelled: std::collections::BTreeSet<&'static str>,
    pub(super) deleting: std::collections::BTreeSet<&'static str>,
    shutting_down: bool,
    pub(super) active_helper: Option<PreviewRunner>,
    pub(super) in_flight_previews: Vec<(u64, &'static str, Option<PreviewRunner>, bool)>,
    next_preview_seq: u64,
    worker_running: bool,
}

impl TtsPreviewWarmState {
    fn is_blocked(&self, model_id: &str) -> bool {
        self.shutting_down || self.cancelled.contains(model_id) || self.deleting.contains(model_id)
    }
}

pub(super) struct TtsModelDeleteGuard<'a> {
    builtin: &'a BuiltinVoice,
    model_id: Option<&'static str>,
}

impl Drop for TtsModelDeleteGuard<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.model_id {
            let (lock, cv) = &self.builtin.tts_preview_warmer;
            if let Ok(mut st) = lock.lock() {
                st.deleting.remove(id);
                if st.active != Some(id) {
                    st.cancelled.remove(id);
                }
                cv.notify_all();
            }
        }
    }
}

struct TtsInFlightPreviewGuard<'a> {
    builtin: &'a BuiltinVoice,
    seq: u64,
    model_id: &'static str,
}

impl TtsInFlightPreviewGuard<'_> {
    fn is_cancelled(&self) -> bool {
        let Ok(st) = self.builtin.tts_preview_warmer.0.lock() else {
            return true;
        };
        st.is_blocked(self.model_id)
            || st
                .in_flight_previews
                .iter()
                .find(|(seq, _, _, _)| *seq == self.seq)
                .is_none_or(|(_, _, _, cancelled)| *cancelled)
    }

    fn attach_helper(&self, helper: PreviewRunner) -> Result<(), CenterError> {
        let blocked = {
            let Ok(mut st) = self.builtin.tts_preview_warmer.0.lock() else {
                helper.terminate();
                return Err(CenterError::new(
                    "local_tts_not_installed",
                    409,
                    "该语音合成模型正在卸载或宿主正在关闭",
                ));
            };
            let entry_cancelled = st
                .in_flight_previews
                .iter()
                .find(|(seq, _, _, _)| *seq == self.seq)
                .is_none_or(|(_, _, _, cancelled)| *cancelled);
            if st.is_blocked(self.model_id) || entry_cancelled {
                true
            } else {
                if let Some((_, _, slot, _)) = st
                    .in_flight_previews
                    .iter_mut()
                    .find(|(seq, _, _, _)| *seq == self.seq)
                {
                    *slot = Some(helper.clone());
                }
                false
            }
        };
        if blocked {
            helper.terminate();
            return Err(CenterError::new(
                "local_tts_not_installed",
                409,
                "该语音合成模型正在卸载或宿主正在关闭",
            ));
        }
        Ok(())
    }
}

impl Drop for TtsInFlightPreviewGuard<'_> {
    fn drop(&mut self) {
        let (lock, cv) = &self.builtin.tts_preview_warmer;
        let helper = lock.lock().ok().and_then(|mut st| {
            st.in_flight_previews
                .iter_mut()
                .find(|(seq, _, _, _)| *seq == self.seq)
                .and_then(|(_, _, slot, _)| slot.take())
        });
        if let Some(h) = helper {
            h.terminate();
        }
        if let Ok(mut st) = lock.lock() {
            st.in_flight_previews
                .retain(|(seq, _, _, _)| *seq != self.seq);
            cv.notify_all();
        }
    }
}

impl BuiltinVoice {
    fn is_tts_preview_warming(&self, model_id: &str) -> bool {
        self.tts_preview_warmer
            .0
            .lock()
            .map(|st| {
                !st.is_blocked(model_id)
                    && (st.active == Some(model_id) || st.queue.iter().any(|id| *id == model_id))
            })
            .unwrap_or(false)
    }

    fn is_tts_warmup_blocked(&self, model_id: &str) -> bool {
        self.tts_preview_warmer
            .0
            .lock()
            .map(|st| st.is_blocked(model_id))
            .unwrap_or(true)
    }

    fn is_tts_warmup_cancelled(weak: &std::sync::Weak<Self>, model_id: &str) -> bool {
        let Some(builtin) = weak.upgrade() else {
            return true;
        };
        builtin.is_tts_warmup_blocked(model_id)
    }

    /// 卸载本地合成模型期间持有的门禁守卫：构造时置 `deleting`、终止活跃预热与单条试听 `LocalHelper` 并等待全部句柄释放，析构时恢复。
    pub(super) fn begin_tts_model_delete(&self, model_id: &str) -> TtsModelDeleteGuard<'_> {
        let Some(spec) = local_models::spec_of(model_id, ModelKind::Tts) else {
            return TtsModelDeleteGuard {
                builtin: self,
                model_id: None,
            };
        };
        let (lock, cv) = &self.tts_preview_warmer;
        let helpers = {
            let Ok(mut st) = lock.lock() else {
                return TtsModelDeleteGuard {
                    builtin: self,
                    model_id: None,
                };
            };
            st.deleting.insert(spec.id);
            st.queue.retain(|&queued| queued != spec.id);
            let mut to_terminate = Vec::new();
            if st.active == Some(spec.id) {
                st.cancelled.insert(spec.id);
                if let Some(helper) = st.active_helper.take() {
                    to_terminate.push(helper);
                }
            }
            for (_, in_flight_id, helper_slot, entry_cancelled) in &mut st.in_flight_previews {
                if *in_flight_id == spec.id {
                    *entry_cancelled = true;
                    if let Some(helper) = helper_slot.take() {
                        to_terminate.push(helper);
                    }
                }
            }
            cv.notify_all();
            to_terminate
        };
        for helper in helpers {
            helper.terminate();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(11);
        if let Ok(mut st) = lock.lock() {
            while st.active == Some(spec.id)
                || st
                    .in_flight_previews
                    .iter()
                    .any(|(_, id, _, _)| *id == spec.id)
            {
                let now = std::time::Instant::now();
                if now >= deadline {
                    break;
                }
                let Ok((next, _)) = cv.wait_timeout(st, deadline - now) else {
                    break;
                };
                st = next;
            }
        }
        TtsModelDeleteGuard {
            builtin: self,
            model_id: Some(spec.id),
        }
    }

    pub(super) fn stop_all_tts_preview_warmups(&self) {
        let (lock, cv) = &self.tts_preview_warmer;
        let helpers = {
            let Ok(mut st) = lock.lock() else {
                return;
            };
            st.shutting_down = true;
            st.queue.clear();
            let mut to_terminate = Vec::new();
            if let Some(active) = st.active {
                st.cancelled.insert(active);
            }
            if let Some(helper) = st.active_helper.take() {
                to_terminate.push(helper);
            }
            for (_, _, helper_slot, entry_cancelled) in &mut st.in_flight_previews {
                *entry_cancelled = true;
                if let Some(helper) = helper_slot.take() {
                    to_terminate.push(helper);
                }
            }
            cv.notify_all();
            to_terminate
        };
        for helper in helpers {
            helper.terminate();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(11);
        if let Ok(mut st) = lock.lock() {
            while st.active.is_some() || !st.in_flight_previews.is_empty() || st.worker_running {
                let now = std::time::Instant::now();
                if now >= deadline {
                    break;
                }
                let Ok((next, _)) = cv.wait_timeout(st, deadline - now) else {
                    break;
                };
                st = next;
            }
        }
    }

    /// 返回 `(是否入队, 是否需新拉起后台预热线程)`；正在卸载、取消收尾或宿主关闭期间一律拒绝重入队且绝不清除 `cancelled`。
    pub(super) fn enqueue_tts_preview_warmup_state(&self, model_id: &'static str) -> (bool, bool) {
        let Ok(mut st) = self.tts_preview_warmer.0.lock() else {
            return (false, false);
        };
        if st.is_blocked(model_id) {
            return (false, false);
        }
        if st.active != Some(model_id) && !st.queue.contains(&model_id) {
            st.queue.push_back(model_id);
        }
        let spawn_worker = if st.worker_running {
            false
        } else {
            st.worker_running = true;
            true
        };
        (true, spawn_worker)
    }

    /// 将已安装且仍有未缓存音色的本地 TTS 模型加入后台试听缓存预热队列。
    pub fn schedule_tts_preview_warmup(&self, model_id: &str) {
        if cfg!(test) {
            return;
        }
        let Some(spec) = local_models::spec_of(model_id, ModelKind::Tts) else {
            return;
        };
        if spec.engine.uses_audiocpp_runtime() {
            return;
        }
        if self.is_tts_warmup_blocked(spec.id) {
            return;
        }
        if self.models.path_if_installed(spec.id).is_none() {
            return;
        }
        let root = self.models.root_for(spec.id);
        let runtime_ready = if spec.engine.uses_speech_runtime() {
            moss_voice::speech_cpp::resolve_speech_binary(root).is_some()
        } else {
            helper_path().is_some_and(|p| p.is_file())
        };
        if !runtime_ready {
            return;
        }
        let missing_any = moss_voice::tts_voices::tts_warmup_plan(spec.id)
            .iter()
            .any(|s| !moss_voice::tts_voices::has_cached_preview(root, spec.id, s.id));
        if !missing_any {
            return;
        }
        let (_, spawn_worker) = self.enqueue_tts_preview_warmup_state(spec.id);
        if spawn_worker {
            let weak = self.self_weak.clone();
            let _ = std::thread::Builder::new()
                .name("voice-tts-preview-warmer".into())
                .spawn(move || Self::run_tts_preview_warmer_loop(weak));
        }
    }

    fn run_tts_preview_warmer_loop(weak: std::sync::Weak<Self>) {
        loop {
            let Some(builtin) = weak.upgrade() else {
                return;
            };
            let next_id = {
                let (lock, cv) = &builtin.tts_preview_warmer;
                let Ok(mut st) = lock.lock() else {
                    return;
                };
                if st.shutting_down {
                    st.queue.clear();
                    st.active = None;
                    st.active_helper = None;
                    st.worker_running = false;
                    cv.notify_all();
                    return;
                }
                let mut chosen = None;
                while let Some(id) = st.queue.pop_front() {
                    if st.deleting.contains(id) {
                        continue;
                    }
                    st.cancelled.remove(id);
                    st.active = Some(id);
                    st.active_helper = None;
                    chosen = Some(id);
                    break;
                }
                match chosen {
                    Some(id) => id,
                    None => {
                        st.active = None;
                        st.active_helper = None;
                        st.worker_running = false;
                        cv.notify_all();
                        return;
                    }
                }
            };
            drop(builtin);
            Self::warm_single_tts_model(&weak, next_id);
            if let Some(builtin) = weak.upgrade() {
                let (lock, cv) = &builtin.tts_preview_warmer;
                let leftover = lock.lock().ok().and_then(|mut st| st.active_helper.take());
                if let Some(helper) = leftover {
                    helper.terminate();
                }
                if let Ok(mut st) = lock.lock() {
                    if st.active == Some(next_id) {
                        st.active = None;
                    }
                    st.cancelled.remove(next_id);
                    cv.notify_all();
                }
            }
        }
    }

    /// 等待当前活跃语音对话（说话拾音 `level > 0`、思考推理或播报）空闲后再占用后台 CPU；
    /// 若宿主退出、收到卸载取消通知或目标模型被卸载则立即返回 `false` 以便释放 `LocalHelper` 文件句柄。
    fn wait_until_voice_idle_or_uninstalled(weak: &std::sync::Weak<Self>, model_id: &str) -> bool {
        loop {
            let Some(builtin) = weak.upgrade() else {
                return false;
            };
            if builtin.models.path_if_installed(model_id).is_none() {
                return false;
            }
            if builtin.is_tts_warmup_blocked(model_id) {
                return false;
            }
            let busy = builtin
                .server
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|s| s.has_busy_voice_session()))
                .unwrap_or(false);
            if !busy {
                return true;
            }
            let (lock, cv) = &builtin.tts_preview_warmer;
            if let Ok(st) = lock.lock() {
                if st.is_blocked(model_id) {
                    return false;
                }
                let _ = cv.wait_timeout(st, std::time::Duration::from_millis(250));
            } else {
                return false;
            }
        }
    }

    fn warm_single_tts_model(weak: &std::sync::Weak<Self>, model_id: &'static str) {
        struct TerminateOnDrop(PreviewRunner);
        impl Drop for TerminateOnDrop {
            fn drop(&mut self) {
                self.0.terminate();
            }
        }

        let Some(spec) = local_models::spec_of(model_id, ModelKind::Tts) else {
            return;
        };
        if spec.engine.uses_audiocpp_runtime() {
            return;
        }
        let (dir, root, speech_exe, helper_exe) = {
            let Some(builtin) = weak.upgrade() else {
                return;
            };
            let Some(dir) = builtin.models.path_if_installed(spec.id) else {
                return;
            };
            let root = builtin.models.root_for(spec.id).to_path_buf();
            let speech_exe = spec
                .engine
                .uses_speech_runtime()
                .then(|| moss_voice::speech_cpp::resolve_speech_binary(&root))
                .flatten();
            let helper_exe = (!spec.engine.uses_speech_runtime())
                .then(helper_path)
                .flatten()
                .filter(|p| p.is_file());
            if (spec.engine.uses_speech_runtime() && speech_exe.is_none())
                || (!spec.engine.uses_speech_runtime() && helper_exe.is_none())
            {
                return;
            }
            (dir, root, speech_exe, helper_exe)
        };
        let plan: Vec<_> = moss_voice::tts_voices::tts_warmup_plan(spec.id)
            .into_iter()
            .filter(|s| !moss_voice::tts_voices::has_cached_preview(&root, spec.id, s.id))
            .collect();
        if plan.is_empty() {
            return;
        }
        // 拉起子进程并加载数百 MB 权重前同样避开活跃对话，模型若被卸载立即清理退出
        if !Self::wait_until_voice_idle_or_uninstalled(weak, spec.id) {
            if weak
                .upgrade()
                .is_some_and(|b| b.models.path_if_installed(spec.id).is_none())
            {
                moss_voice::tts_voices::remove_model_previews(&root, spec.id);
            }
            return;
        }
        let register_runner = |runner: PreviewRunner| {
            let cancelled = if let Some(builtin) = weak.upgrade() {
                if let Ok(mut st) = builtin.tts_preview_warmer.0.lock() {
                    if st.is_blocked(spec.id) {
                        true
                    } else {
                        st.active_helper = Some(runner.clone());
                        false
                    }
                } else {
                    true
                }
            } else {
                true
            };
            if cancelled {
                runner.terminate();
                None
            } else {
                Some(TerminateOnDrop(runner))
            }
        };
        let runner_guard = if let Some(speech_exe) = speech_exe {
            let model_file = super::conversation::resolve_model_entry_file(&dir, spec);
            let Ok(worker) =
                SpeechCppWorker::spawn_as(&speech_exe, &model_file, "音色试听缓存程序")
            else {
                return;
            };
            let Some(guard) = register_runner(PreviewRunner::SpeechCpp(worker)) else {
                return;
            };
            guard
        } else {
            let Some(exe) = helper_exe else {
                return;
            };
            let Some(engine) = super::conversation::helper_tts_engine(spec.engine) else {
                return;
            };
            let Ok(helper) = LocalHelper::spawn_as(&exe, "音色试听缓存程序") else {
                return;
            };
            let Some(guard) = register_runner(PreviewRunner::Helper(helper.clone())) else {
                return;
            };
            if helper.load_tts(&dir, engine).is_err() {
                return;
            }
            guard
        };
        for item in plan {
            if !Self::wait_until_voice_idle_or_uninstalled(weak, spec.id) {
                if weak
                    .upgrade()
                    .is_some_and(|b| b.models.path_if_installed(spec.id).is_none())
                {
                    moss_voice::tts_voices::remove_model_previews(&root, spec.id);
                }
                return;
            }
            if moss_voice::tts_voices::has_cached_preview(&root, spec.id, item.id) {
                continue;
            }
            let synth = match &runner_guard.0 {
                PreviewRunner::Helper(helper) => {
                    helper.synthesize_blocking(item.sample_text, item.id, 1.0)
                }
                PreviewRunner::SpeechCpp(worker) => worker.synthesize_blocking(
                    item.sample_text,
                    moss_voice::tts_voices::qwen3_tts_voice_slug(item.id),
                ),
                PreviewRunner::AudioCpp(_) => continue,
            };
            let Ok(pcm) = synth else {
                if Self::is_tts_warmup_cancelled(weak, spec.id) {
                    return;
                }
                continue;
            };
            if pcm.samples.is_empty() || pcm.sample_rate == 0 {
                continue;
            }
            if Self::is_tts_warmup_cancelled(weak, spec.id) {
                return;
            }
            let wav = moss_voice_providers::wav::encode_wav(&pcm.samples, pcm.sample_rate);
            let still_installed_after = weak
                .upgrade()
                .is_some_and(|b| b.models.path_if_installed(spec.id).is_some());
            if !still_installed_after || Self::is_tts_warmup_cancelled(weak, spec.id) {
                if !still_installed_after {
                    moss_voice::tts_voices::remove_model_previews(&root, spec.id);
                }
                return;
            }
            let _ =
                moss_voice::tts_voices::write_cached_preview_atomic(&root, spec.id, item.id, &wav);
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// 查询本地合成模型支持的语言包、全部音色及本机试听缓存状态。
    pub fn local_tts_voices_list(&self, input: &Value) -> Result<Value, CenterError> {
        let model_id = input
            .get("model_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CenterError::new("model_input_invalid", 400, "缺少本地合成模型编号"))?;
        let catalog = moss_voice::tts_voices::tts_voice_catalog(model_id)
            .ok_or_else(|| CenterError::new("model_input_invalid", 400, "不支持的本地合成模型"))?;
        let installed = self.models.path_if_installed(catalog.model_id).is_some();
        let root = self.models.root_for(catalog.model_id);
        let mut cached_count = 0usize;
        let speakers: Vec<Value> = catalog
            .speakers
            .iter()
            .map(|s| {
                let cached = installed
                    && moss_voice::tts_voices::has_cached_preview(root, catalog.model_id, s.id);
                if cached {
                    cached_count += 1;
                }
                serde_json::json!({
                    "id": s.id,
                    "code": s.code,
                    "name": s.name,
                    "gender": s.gender,
                    "gender_label": s.gender_label,
                    "lang": s.lang,
                    "lang_label": s.lang_label,
                    "sample_text": s.sample_text,
                    "recommended_for_zh": s.recommended_for_zh,
                    "cached": cached,
                })
            })
            .collect();
        let total_count = speakers.len();
        if installed && cached_count < total_count {
            self.schedule_tts_preview_warmup(catalog.model_id);
        }
        let caching = installed
            && cached_count < total_count
            && self.is_tts_preview_warming(catalog.model_id);
        Ok(serde_json::json!({
            "model_id": catalog.model_id,
            "label": catalog.label,
            "note": catalog.note,
            "installed": installed,
            "caching": caching,
            "cached_count": cached_count,
            "total_count": total_count,
            "default_speaker": catalog.default_speaker,
            "languages": catalog.languages,
            "speakers": speakers,
        }))
    }

    fn enter_in_flight_tts_preview(
        &self,
        model_id: &'static str,
    ) -> Result<TtsInFlightPreviewGuard<'_>, CenterError> {
        let mut st = self
            .tts_preview_warmer
            .0
            .lock()
            .map_err(|_| internal("试听门禁锁异常"))?;
        if st.is_blocked(model_id) {
            return Err(CenterError::new(
                "local_tts_not_installed",
                409,
                "该语音合成模型正在卸载或宿主正在关闭",
            ));
        }
        let seq = st.next_preview_seq;
        st.next_preview_seq = st.next_preview_seq.wrapping_add(1);
        st.in_flight_previews.push((seq, model_id, None, false));
        Ok(TtsInFlightPreviewGuard {
            builtin: self,
            seq,
            model_id,
        })
    }

    /// 试听指定本地合成模型的单个音色：优先直接秒读本机 WAV 缓存；尚未缓存时当场合成并落盘缓存。
    ///
    /// 可选 `speed`（0.5 ~ 2.0，2026-11-25）：设置页“按当前音色和语速试一句”用未保存的语速试听。
    /// 非 1.0 的语速当场合成、不读也不写缓存（缓存只存 1.0x 的试听）；不支持调语速的模型（Qwen3-TTS 0.6B）
    /// 固定按 1.0x，Qwen3-TTS 1.7B 把语速换成说话指令（n126）。
    pub fn local_tts_preview(&self, input: &Value) -> Result<Value, CenterError> {
        let started = std::time::Instant::now();
        let model_id = input
            .get("model_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CenterError::new("model_input_invalid", 400, "缺少本地合成模型编号"))?;
        let speaker = input
            .get("speaker")
            .and_then(Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| CenterError::new("model_input_invalid", 400, "缺少有效的音色编号"))?;
        let speed = match input.get("speed") {
            None | Some(Value::Null) => 1.0f32,
            Some(value) => value
                .as_f64()
                .filter(|v| (0.5..=2.0).contains(v))
                .map(|v| v as f32)
                .ok_or_else(|| {
                    CenterError::new(
                        "model_input_invalid",
                        400,
                        "本地合成语速须在 0.5 ~ 2.0 之间",
                    )
                })?,
        };
        let spec = local_models::spec_of(model_id, ModelKind::Tts)
            .ok_or_else(|| CenterError::new("model_input_invalid", 400, "不支持的本地合成模型"))?;
        let speed = if spec.tts_speed_adjustable() == Some(true) {
            speed
        } else {
            1.0
        };
        let instructions = moss_voice::tts_voices::speech_tts_instructions(spec, speed);
        let custom_speed = (speed - 1.0).abs() > 1e-3;
        let meta = moss_voice::tts_voices::tts_speaker_meta(spec.id, speaker).ok_or_else(|| {
            CenterError::new("model_input_invalid", 400, "音色编号超出该模型支持范围")
        })?;
        if self.is_tts_warmup_blocked(spec.id) {
            return Err(CenterError::new(
                "local_tts_not_installed",
                409,
                "该语音合成模型正在卸载或宿主正在关闭",
            ));
        }
        let root = self.models.root_for(spec.id);
        let is_audiocpp = spec.engine.uses_audiocpp_runtime();
        let draft_transcript = input.get("transcript").and_then(Value::as_str);
        if !is_audiocpp
            && let Some(wav) = (!custom_speed)
                .then(|| moss_voice::tts_voices::read_cached_preview(root, spec.id, speaker))
                .flatten()
        {
            let data_url = moss_voice::models::audio_data_url(&wav, "audio/wav");
            return Ok(serde_json::json!({
                "section": "local_tts",
                "ok": true,
                "from_cache": true,
                "elapsed_ms": started.elapsed().as_millis() as u64,
                "model_id": spec.id,
                "speaker": speaker,
                "speed": speed,
                "sample_text": meta.sample_text,
                "audio_data_url": data_url,
            }));
        }
        let preview_guard = self.enter_in_flight_tts_preview(spec.id)?;
        let Some(dir) = self.models.path_if_installed(spec.id) else {
            return Err(CenterError::new(
                "local_tts_not_installed",
                409,
                "请先下载该语音合成模型，下载完成后会自动缓存本机试听语音",
            ));
        };
        // 若当前常驻进程恰好已加载同款合成模型，直接借用免重载；否则按引擎拉起短生命周期推理进程
        let pcm = if is_audiocpp {
            struct TempPreviewWavGuard(Option<std::path::PathBuf>);
            impl Drop for TempPreviewWavGuard {
                fn drop(&mut self) {
                    if let Some(path) = self.0.take() {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
            let (binding, temp_wav) = moss_voice::audio_cpp::prepare_clone_preview_binding(
                &self.local_clone_root(),
                draft_transcript,
            )
            .map_err(|msg| CenterError::new("model_input_invalid", 400, msg))?;
            let _temp_wav_guard = TempPreviewWavGuard(temp_wav);
            let model_file = super::conversation::resolve_model_entry_file(&dir, spec);
            let reusable_worker = self.audiocpp_worker.lock().ok().and_then(|guard| {
                guard
                    .as_ref()
                    .filter(|w| {
                        w.is_alive()
                            && w.model_path() == model_file
                            && w.voice_dir() == binding.voice_dir
                    })
                    .cloned()
            });
            let worker = match reusable_worker {
                Some(w) => w,
                None => {
                    let audiocpp_exe = moss_voice::audio_cpp::resolve_audiocpp_binary(root)
                        .ok_or_else(|| {
                            CenterError::new(
                                "local_helper_missing",
                                409,
                                "未找到声音克隆推理引擎 audiocpp_server，请删除并重新下载 Qwen3-TTS 声音克隆模型",
                            )
                        })?;
                    let backend =
                        moss_voice::audio_cpp::recommended_audiocpp_backend(&audiocpp_exe);
                    let spawned = AudioCppWorker::spawn_as_cancellable(
                        &audiocpp_exe,
                        &model_file,
                        &binding.voice_dir,
                        backend,
                        "克隆音色试听程序",
                        &|| preview_guard.is_cancelled(),
                    )
                    .map_err(|e| {
                        CenterError::new(
                            "local_tts_load_failed",
                            500,
                            format!("加载声音克隆模型失败：{e}"),
                        )
                    })?;
                    preview_guard.attach_helper(PreviewRunner::AudioCpp(spawned.clone()))?;
                    spawned
                }
            };
            let voice_ref = AudioCppVoiceRef {
                voice_name: binding.voice_name,
                transcript: binding.transcript,
            };
            crate::block_on(worker.synthesize_with_ref(meta.sample_text, &voice_ref))
                .map_err(|e| CenterError::new("local_tts_synth_failed", 500, e.to_string()))?
        } else if spec.engine.uses_speech_runtime() {
            let model_file = super::conversation::resolve_model_entry_file(&dir, spec);
            let voice = moss_voice::tts_voices::qwen3_tts_voice_slug(speaker);
            let reusable_worker = self.speech_worker.lock().ok().and_then(|guard| {
                guard
                    .as_ref()
                    .filter(|w| w.is_alive() && w.model_path() == model_file)
                    .cloned()
            });
            if let Some(worker) = reusable_worker {
                worker
                    .synthesize_blocking_with(meta.sample_text, voice, instructions)
                    .map_err(|e| CenterError::new("local_tts_synth_failed", 500, e.to_string()))?
            } else {
                let speech_exe =
                    moss_voice::speech_cpp::resolve_speech_binary(root).ok_or_else(|| {
                        CenterError::new(
                            "local_helper_missing",
                            409,
                            "未找到 Qwen3-TTS 推理引擎 speech，请删除并重新下载 Qwen3-TTS 模型包",
                        )
                    })?;
                let worker = SpeechCppWorker::spawn_as_cancellable(
                    &speech_exe,
                    &model_file,
                    "音色试听缓存程序",
                    &|| preview_guard.is_cancelled(),
                )
                .map_err(|e| {
                    CenterError::new(
                        "local_tts_load_failed",
                        500,
                        format!("加载合成模型失败：{e}"),
                    )
                })?;
                preview_guard.attach_helper(PreviewRunner::SpeechCpp(worker.clone()))?;
                worker
                    .synthesize_blocking_with(meta.sample_text, voice, instructions)
                    .map_err(|e| CenterError::new("local_tts_synth_failed", 500, e.to_string()))?
            }
        } else {
            let reusable_helper = {
                let h_guard = self.helper.lock().ok();
                let r_guard = self.running_local.lock().ok();
                if let (Some(Some(h)), Some(Some(r))) = (h_guard.as_deref(), r_guard.as_deref())
                    && h.is_alive()
                    && r.tts.as_ref().is_some_and(|(loaded_id, loaded_dir, _)| {
                        *loaded_id == spec.id && loaded_dir == &dir
                    })
                {
                    Some(h.clone())
                } else {
                    None
                }
            };
            if let Some(helper) = reusable_helper {
                helper
                    .synthesize_blocking(meta.sample_text, speaker, speed)
                    .map_err(|e| CenterError::new("local_tts_synth_failed", 500, e.to_string()))?
            } else {
                let exe = helper_path().filter(|p| p.is_file()).ok_or_else(|| {
                    CenterError::new(
                        "local_helper_missing",
                        409,
                        "未找到本地模型辅助程序 moss-voice-local",
                    )
                })?;
                let engine =
                    super::conversation::helper_tts_engine(spec.engine).ok_or_else(|| {
                        CenterError::new(
                            "model_input_invalid",
                            400,
                            format!("不支持的本地合成引擎：{}", spec.label),
                        )
                    })?;
                let helper = LocalHelper::spawn_as(&exe, "音色试听缓存程序").map_err(|e| {
                    CenterError::new("local_helper_failed", 500, format!("启动辅助程序失败：{e}"))
                })?;
                preview_guard.attach_helper(PreviewRunner::Helper(helper.clone()))?;
                helper.load_tts(&dir, engine).map_err(|e| {
                    CenterError::new(
                        "local_tts_load_failed",
                        500,
                        format!("加载合成模型失败：{e}"),
                    )
                })?;
                helper
                    .synthesize_blocking(meta.sample_text, speaker, speed)
                    .map_err(|e| CenterError::new("local_tts_synth_failed", 500, e.to_string()))?
            }
        };
        let wav = moss_voice_providers::wav::encode_wav(&pcm.samples, pcm.sample_rate);
        if !is_audiocpp
            && !custom_speed
            && !preview_guard.is_cancelled()
            && self.models.path_if_installed(spec.id).is_some()
        {
            let _ =
                moss_voice::tts_voices::write_cached_preview_atomic(root, spec.id, speaker, &wav);
        }
        drop(preview_guard);
        self.schedule_tts_preview_warmup(spec.id);
        let data_url = moss_voice::models::audio_data_url(&wav, "audio/wav");
        Ok(serde_json::json!({
            "section": "local_tts",
            "ok": true,
            "from_cache": false,
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "model_id": spec.id,
            "speaker": speaker,
            "speed": speed,
            "sample_text": meta.sample_text,
            "audio_data_url": data_url,
        }))
    }
}
