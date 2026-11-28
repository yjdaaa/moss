//! 对话装配：本地模型挂载、本地合成兜底与识别/大模型/合成提供方构建。

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RunningLocalSpec {
    vad: Option<(&'static str, std::path::PathBuf)>,
    /// 命令词检测模型目录（随本地判停运行，n101）
    kws: Option<(&'static str, std::path::PathBuf)>,
    denoise: Option<(&'static str, std::path::PathBuf)>,
    pub(super) asr: Option<(&'static str, std::path::PathBuf, bool, Vec<String>)>,
    pub(super) tts: Option<(
        &'static str,
        std::path::PathBuf,
        moss_voice_providers::local::TtsInfo,
    )>,
}

impl RunningLocalSpec {
    /// 辅助程序里真正加载成功的本地模型（类别 → 模型编号），供完整过程记录写服务启动情况（n110）。
    pub(super) fn loaded_ids(&self) -> Vec<(ModelKind, &'static str)> {
        [
            (ModelKind::Vad, self.vad.as_ref().map(|v| v.0)),
            (ModelKind::Kws, self.kws.as_ref().map(|v| v.0)),
            (ModelKind::Denoise, self.denoise.as_ref().map(|v| v.0)),
            (ModelKind::Asr, self.asr.as_ref().map(|v| v.0)),
            (ModelKind::Tts, self.tts.as_ref().map(|v| v.0)),
        ]
        .into_iter()
        .filter_map(|(kind, id)| id.map(|id| (kind, id)))
        .collect()
    }
}

/// 本次启动用到的本地模型。
#[derive(Default)]
pub(super) struct LocalParts {
    pub(super) asr: Option<LocalAsr>,
    /// 选用流式识别模型时：边说边识别（整段识别 `asr` 作为它失败时的退路）
    pub(super) streaming_asr: Option<Arc<dyn StreamingAsr>>,
    pub(super) tts: Option<Arc<dyn Tts>>,
    /// 选用 `speech.cpp`（Qwen3-TTS）时的长驻子进程句柄。
    pub(super) speech_worker: Option<SpeechCppWorker>,
    /// 选用 `audio.cpp`（Qwen3-TTS 声音克隆）时的长驻子进程句柄（n131）。
    pub(super) audiocpp_worker: Option<AudioCppWorker>,
    /// 选用尾音语义判停时的专用子进程句柄（与主 `helper` 隔离）。
    pub(super) turn_worker: Option<TurnHelper>,
    /// 主用云端 TTS 时，若本机已安装本地合成模型，挂作失败时的按需自动降级兜底。
    pub(super) fallback_tts: Option<Arc<dyn Tts>>,
    /// 用于在停止服务与删除模型时隔离并回收本地兜底子进程的句柄。
    pub(super) fallback_tts_handle: Option<Arc<LazyLocalFallbackTts>>,
    pub(super) vad: Option<Arc<dyn VadFactory>>,
    pub(super) turn_predictor: Option<Arc<dyn TurnEndPredictor>>,
    pub(super) denoise_active: Option<&'static str>,
    pub(super) turn_active: Option<&'static str>,
    pub(super) kws_active: Option<&'static str>,
}

#[derive(Default)]
pub(super) struct ChainTestLocalGuard {
    _helper: Option<LocalHelper>,
    turn_worker: Option<TurnHelper>,
    speech_worker: Option<SpeechCppWorker>,
    audiocpp_worker: Option<AudioCppWorker>,
    fallback_tts: Option<Arc<LazyLocalFallbackTts>>,
}

impl Drop for ChainTestLocalGuard {
    fn drop(&mut self) {
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
        if let Some(helper) = self._helper.take() {
            helper.terminate();
        }
    }
}

pub(super) fn resolve_model_entry_file(
    dir_or_file: &std::path::Path,
    spec: &moss_voice::local_models::ModelSpec,
) -> std::path::PathBuf {
    match spec.archive {
        moss_voice::local_models::Archive::File => dir_or_file.to_path_buf(),
        moss_voice::local_models::Archive::Directory { .. }
        | moss_voice::local_models::Archive::TarBz2 { .. }
        | moss_voice::local_models::Archive::TarGz { .. }
            if !spec.path.is_empty() =>
        {
            dir_or_file.join(moss_voice::local_models::installed_rel_path(spec.path))
        }
        _ => dir_or_file.to_path_buf(),
    }
}

pub(super) fn helper_asr_engine(engine: Engine) -> AsrEngine {
    match engine {
        Engine::StreamingTransducer => AsrEngine::StreamingTransducer,
        Engine::Qwen3Asr => AsrEngine::Qwen3Asr,
        Engine::FireRedAsr2Ctc => AsrEngine::FireRedAsr2Ctc,
        _ => AsrEngine::SenseVoice,
    }
}

pub(super) fn helper_tts_engine(engine: Engine) -> Option<TtsEngine> {
    match engine {
        Engine::Kokoro => Some(TtsEngine::Kokoro),
        Engine::Matcha => Some(TtsEngine::Matcha),
        Engine::ZipVoice => Some(TtsEngine::ZipVoice),
        Engine::Vits => Some(TtsEngine::Vits),
        _ => None,
    }
}

struct LocalTurnEndPredictor {
    helper: TurnHelper,
}

impl TurnEndPredictor for LocalTurnEndPredictor {
    fn predict_turn_end<'a>(
        &'a self,
        pcm: &'a [i16],
        sample_rate: u32,
    ) -> moss_voice_providers::BoxFuture<'a, Option<f32>> {
        let helper = self.helper.clone();
        let samples = pcm.to_vec();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                helper
                    .predict_blocking(sample_rate, &samples, std::time::Duration::from_millis(250))
                    .ok()
                    .map(|(_, prob)| prob)
            })
            .await
            .ok()
            .flatten()
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FallbackTtsBackend {
    Helper(TtsEngine),
    SpeechCpp { model_file: std::path::PathBuf },
    AudioCpp { model_file: std::path::PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalFallbackCandidate {
    id: &'static str,
    dir: std::path::PathBuf,
    backend: FallbackTtsBackend,
    preferred_speaker: i32,
    default_speaker: i32,
}

/// 按「用户指定模型优先 → 其余已安装本地 TTS 模型顺延」顺序生成候选列表。
/// 顺延到非用户所选的另一款模型（或音色编号落在 Kokoro 英文音色区 / 超出模型音色范围）时，回退为该候选模型自带的默认中文音色。
fn ordered_local_tts_candidates(
    preferred_id: &str,
    preferred_speaker: i32,
    mut path_if_installed: impl FnMut(&str) -> Option<std::path::PathBuf>,
) -> Vec<LocalFallbackCandidate> {
    let mut candidates = Vec::new();
    let to_candidate = |spec: &'static moss_voice::local_models::ModelSpec,
                        dir: std::path::PathBuf,
                        is_preferred_model: bool| {
        let backend = if spec.engine.uses_speech_runtime() {
            FallbackTtsBackend::SpeechCpp {
                model_file: resolve_model_entry_file(&dir, spec),
            }
        } else if spec.engine.uses_audiocpp_runtime() {
            FallbackTtsBackend::AudioCpp {
                model_file: resolve_model_entry_file(&dir, spec),
            }
        } else {
            FallbackTtsBackend::Helper(helper_tts_engine(spec.engine)?)
        };
        let default_speaker = spec.voices.map(|v| v.default).unwrap_or(0);
        let out_of_spec_range = spec
            .voices
            .is_some_and(|v| preferred_speaker < 0 || preferred_speaker > v.max);
        let candidate_speaker = if !is_preferred_model
            || out_of_spec_range
            || (spec.id == "kokoro" && !(45..=52).contains(&preferred_speaker))
            || (spec.id == "kokoro_v11" && !(3..=102).contains(&preferred_speaker))
        {
            default_speaker
        } else {
            preferred_speaker
        };
        Some(LocalFallbackCandidate {
            id: spec.id,
            dir,
            backend,
            preferred_speaker: candidate_speaker,
            default_speaker,
        })
    };
    if let Some(spec) = moss_voice::local_models::spec_of(preferred_id, ModelKind::Tts)
        && let Some(dir) = path_if_installed(spec.id)
        && let Some(cand) = to_candidate(spec, dir, true)
    {
        candidates.push(cand);
    }
    for spec in moss_voice::local_models::CATALOG
        .iter()
        .filter(|spec| spec.kind == ModelKind::Tts)
    {
        if candidates.iter().any(|c| c.id == spec.id) {
            continue;
        }
        if let Some(dir) = path_if_installed(spec.id)
            && let Some(cand) = to_candidate(spec, dir, false)
        {
            candidates.push(cand);
        }
    }
    candidates
}

enum LoadedFallbackTts {
    Helper {
        tts: LocalTts,
        owns_helper: bool,
    },
    SpeechCpp {
        worker: SpeechCppWorker,
        voice: String,
        /// Qwen3-TTS 1.7B 按语速选的说话指令（其他为空，n126）
        instructions: String,
    },
    AudioCpp {
        worker: AudioCppWorker,
        resolve_ref: moss_voice_providers::audio_cpp::VoiceRefResolver,
    },
}

impl Drop for LoadedFallbackTts {
    fn drop(&mut self) {
        match self {
            Self::Helper { tts, owns_helper } => {
                if *owns_helper {
                    tts.helper.terminate();
                }
            }
            Self::SpeechCpp { worker, .. } => {
                worker.terminate();
            }
            Self::AudioCpp { worker, .. } => {
                worker.terminate();
            }
        }
    }
}

impl LoadedFallbackTts {
    fn is_alive(&self) -> bool {
        match self {
            Self::Helper { tts, .. } => tts.helper.is_alive(),
            Self::SpeechCpp { worker, .. } => worker.is_alive(),
            Self::AudioCpp { worker, .. } => worker.is_alive(),
        }
    }

    fn into_reusable_helper(mut self) -> Option<(LocalHelper, bool)> {
        match &mut self {
            Self::Helper { tts, owns_helper } if tts.helper.is_alive() => {
                let owns = std::mem::replace(owns_helper, false);
                Some((tts.helper.clone(), owns))
            }
            _ => None,
        }
    }

    fn to_arc_tts(&self) -> Arc<dyn Tts> {
        match self {
            Self::Helper { tts, .. } => Arc::new(LocalTts {
                helper: tts.helper.clone(),
                speaker: tts.speaker,
                speed: tts.speed,
            }),
            Self::SpeechCpp {
                worker,
                voice,
                instructions,
            } => Arc::new(SpeechCppTts {
                worker: worker.clone(),
                voice: voice.clone(),
                instructions: instructions.clone(),
            }),
            Self::AudioCpp {
                worker,
                resolve_ref,
            } => Arc::new(AudioCppTts {
                worker: worker.clone(),
                resolve_ref: resolve_ref.clone(),
            }),
        }
    }
}

struct OwnedHelperCache(Option<(LocalHelper, bool)>);

impl Drop for OwnedHelperCache {
    fn drop(&mut self) {
        if let Some((helper, true)) = self.0.take() {
            helper.terminate();
        }
    }
}

#[derive(Default)]
struct FallbackRuntimeState {
    loaded: Option<(usize, LoadedFallbackTts)>,
    deleting: std::collections::HashSet<String>,
}

#[derive(Default)]
pub(super) struct FallbackTtsDeleteGuard {
    state: Option<Arc<Mutex<FallbackRuntimeState>>>,
    id: String,
}

impl Drop for FallbackTtsDeleteGuard {
    fn drop(&mut self) {
        if let Some(state) = self.state.take()
            && let Ok(mut guard) = state.lock()
        {
            guard.deleting.remove(&self.id);
        }
    }
}

/// 云端 TTS 失败且当句尚未产出音频时，按需加载本机已安装的本地 TTS（优先用户选定模型，坏了自动顺延本机其他已安装模型）。
pub(super) struct LazyLocalFallbackTts {
    helper_exe: Option<std::path::PathBuf>,
    speech_exe: Option<std::path::PathBuf>,
    audiocpp_exe: Option<std::path::PathBuf>,
    audiocpp_voice_dir: std::path::PathBuf,
    audiocpp_backend: &'static str,
    audiocpp_resolve_ref: moss_voice_providers::audio_cpp::VoiceRefResolver,
    candidates: Vec<LocalFallbackCandidate>,
    speed: f32,
    shared_helper: Option<LocalHelper>,
    state: Arc<Mutex<FallbackRuntimeState>>,
}

impl LazyLocalFallbackTts {
    pub(super) fn terminate_all(&self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.loaded = None;
        }
    }

    pub(super) fn invalidate_audiocpp(&self) {
        if let Ok(mut guard) = self.state.lock()
            && guard
                .loaded
                .as_ref()
                .is_some_and(|(_, entry)| matches!(entry, LoadedFallbackTts::AudioCpp { .. }))
        {
            guard.loaded = None;
        }
    }

    #[cfg(test)]
    pub(super) fn for_test_loaded(
        id: &'static str,
        dir: std::path::PathBuf,
        other_id: &'static str,
        other_dir: std::path::PathBuf,
        helper: LocalHelper,
    ) -> Arc<Self> {
        let state = FallbackRuntimeState {
            loaded: Some((
                0,
                LoadedFallbackTts::Helper {
                    tts: LocalTts {
                        helper,
                        speaker: 0,
                        speed: 1.0,
                    },
                    owns_helper: true,
                },
            )),
            deleting: Default::default(),
        };
        Arc::new(Self {
            helper_exe: None,
            speech_exe: None,
            audiocpp_exe: None,
            audiocpp_voice_dir: std::path::PathBuf::new(),
            audiocpp_backend: "cpu",
            audiocpp_resolve_ref: Arc::new(|| {
                Err(moss_voice_providers::ProviderError::new("test"))
            }),
            candidates: vec![
                LocalFallbackCandidate {
                    id,
                    dir,
                    backend: FallbackTtsBackend::Helper(TtsEngine::ZipVoice),
                    preferred_speaker: 0,
                    default_speaker: 0,
                },
                LocalFallbackCandidate {
                    id: other_id,
                    dir: other_dir,
                    backend: FallbackTtsBackend::Helper(TtsEngine::Kokoro),
                    preferred_speaker: 50,
                    default_speaker: 50,
                },
            ],
            speed: 1.0,
            shared_helper: None,
            state: Arc::new(Mutex::new(state)),
        })
    }

    #[cfg(test)]
    pub(super) fn is_deleting(&self, id: &str) -> bool {
        self.state
            .lock()
            .map(|g| g.deleting.contains(id))
            .unwrap_or(false)
    }

    async fn get_or_init_from(
        &self,
        start_index: usize,
    ) -> Result<(usize, Arc<dyn Tts>), moss_voice_providers::ProviderError> {
        if start_index >= self.candidates.len() {
            return Err(moss_voice_providers::ProviderError::new(
                "没有可用的本地降级语音合成模型",
            ));
        }
        if let Ok(mut guard) = self.state.lock()
            && let Some((idx, existing)) = guard.loaded.as_ref()
            && *idx >= start_index
            && existing.is_alive()
            && !guard.deleting.contains(self.candidates[*idx].id)
        {
            let audiocpp_unready = matches!(
                self.candidates[*idx].backend,
                FallbackTtsBackend::AudioCpp { .. }
            ) && (self.audiocpp_resolve_ref)().is_err();
            if audiocpp_unready {
                guard.loaded = None;
            } else {
                return Ok((*idx, existing.to_arc_tts()));
            }
        }
        let helper_exe = self.helper_exe.clone();
        let speech_exe = self.speech_exe.clone();
        let audiocpp_exe = self.audiocpp_exe.clone();
        let audiocpp_voice_dir = self.audiocpp_voice_dir.clone();
        let audiocpp_backend = self.audiocpp_backend;
        let audiocpp_resolve_ref = self.audiocpp_resolve_ref.clone();
        let candidates = self.candidates.clone();
        let speed = self.speed.clamp(0.5, 2.0);
        let shared_helper = self.shared_helper.clone();
        let state_slot = self.state.clone();

        tokio::task::spawn_blocking(move || {
            let mut guard = state_slot
                .lock()
                .map_err(|_| moss_voice_providers::ProviderError::new("本地降级合成锁失败"))?;
            if let Some((idx, existing)) = guard.loaded.as_ref()
                && *idx >= start_index
                && existing.is_alive()
                && !guard.deleting.contains(candidates[*idx].id)
            {
                let audiocpp_unready = matches!(
                    candidates[*idx].backend,
                    FallbackTtsBackend::AudioCpp { .. }
                ) && (audiocpp_resolve_ref)().is_err();
                if audiocpp_unready {
                    guard.loaded = None;
                } else {
                    return Ok((*idx, existing.to_arc_tts()));
                }
            }
            let reused = guard
                .loaded
                .take()
                .and_then(|(_, existing)| existing.into_reusable_helper())
                .or_else(|| shared_helper.filter(|h| h.is_alive()).map(|h| (h, false)));
            let mut helper_cache = OwnedHelperCache(reused);
            let mut last_err =
                moss_voice_providers::ProviderError::new("没有可用的本地降级语音合成模型");
            for (idx, cand) in candidates.iter().enumerate().skip(start_index) {
                if guard.deleting.contains(cand.id) {
                    continue;
                }
                match &cand.backend {
                    FallbackTtsBackend::SpeechCpp { model_file } => {
                        let Some(exe) = speech_exe.as_deref() else {
                            continue;
                        };
                        match SpeechCppWorker::spawn(exe, model_file) {
                            Ok(worker) => {
                                let voice = moss_voice::tts_voices::qwen3_tts_voice_slug(
                                    cand.preferred_speaker,
                                )
                                .to_owned();
                                let instructions =
                                    moss_voice::local_models::spec_of(cand.id, ModelKind::Tts)
                                        .map(|spec| {
                                            moss_voice::tts_voices::speech_tts_instructions(
                                                spec, speed,
                                            )
                                        })
                                        .unwrap_or_default()
                                        .to_owned();
                                let entry = LoadedFallbackTts::SpeechCpp {
                                    worker,
                                    voice,
                                    instructions,
                                };
                                let tts = entry.to_arc_tts();
                                guard.loaded = Some((idx, entry));
                                return Ok((idx, tts));
                            }
                            Err(err) => {
                                last_err = err;
                            }
                        }
                    }
                    FallbackTtsBackend::AudioCpp { model_file } => {
                        let Some(exe) = audiocpp_exe.as_deref() else {
                            continue;
                        };
                        if (audiocpp_resolve_ref)().is_err() {
                            continue;
                        }
                        match AudioCppWorker::spawn(
                            exe,
                            model_file,
                            &audiocpp_voice_dir,
                            audiocpp_backend,
                        ) {
                            Ok(worker) => {
                                let entry = LoadedFallbackTts::AudioCpp {
                                    worker,
                                    resolve_ref: audiocpp_resolve_ref.clone(),
                                };
                                let tts = entry.to_arc_tts();
                                guard.loaded = Some((idx, entry));
                                return Ok((idx, tts));
                            }
                            Err(err) => {
                                last_err = err;
                            }
                        }
                    }
                    FallbackTtsBackend::Helper(engine) => {
                        let (helper, owns_helper) = match helper_cache.0.take() {
                            Some((h, owns)) if h.is_alive() => (h, owns),
                            old => {
                                if let Some((h, true)) = old {
                                    h.terminate();
                                }
                                let Some(exe) = helper_exe.as_deref() else {
                                    continue;
                                };
                                match LocalHelper::spawn(exe) {
                                    Ok(h) => (h, true),
                                    Err(err) => {
                                        last_err = err;
                                        continue;
                                    }
                                }
                            }
                        };
                        let res = helper.load_tts(&cand.dir, *engine);
                        match res {
                            Ok(info) => {
                                let speaker = if info.speakers > 1 {
                                    if cand.preferred_speaker >= 0
                                        && (cand.preferred_speaker as u32) < info.speakers
                                    {
                                        cand.preferred_speaker
                                    } else {
                                        cand.default_speaker.clamp(0, (info.speakers as i32) - 1)
                                    }
                                } else {
                                    0
                                };
                                let entry = LoadedFallbackTts::Helper {
                                    tts: LocalTts {
                                        helper,
                                        speaker,
                                        speed,
                                    },
                                    owns_helper,
                                };
                                let tts = entry.to_arc_tts();
                                guard.loaded = Some((idx, entry));
                                return Ok((idx, tts));
                            }
                            Err(err) => {
                                if helper.is_alive() {
                                    helper_cache.0 = Some((helper, owns_helper));
                                } else if owns_helper {
                                    helper.terminate();
                                }
                                last_err = err;
                            }
                        }
                    }
                }
            }
            Err(last_err)
        })
        .await
        .map_err(|_| moss_voice_providers::ProviderError::new("本地降级合成线程异常退出"))?
    }

    fn invalidate_index(&self, failed_idx: usize) {
        if let Ok(mut guard) = self.state.lock()
            && guard
                .loaded
                .as_ref()
                .is_some_and(|(idx, _)| *idx == failed_idx)
        {
            guard.loaded = None;
        }
    }
}

impl Tts for LazyLocalFallbackTts {
    fn synthesize<'a>(
        &'a self,
        text: &'a str,
    ) -> moss_voice_providers::BoxFuture<
        'a,
        Result<moss_voice_providers::Pcm, moss_voice_providers::ProviderError>,
    > {
        Box::pin(async move {
            let mut start_idx = 0;
            let mut last_err =
                moss_voice_providers::ProviderError::new("没有可用的本地降级语音合成模型");
            while start_idx < self.candidates.len() {
                let (idx, tts) = self.get_or_init_from(start_idx).await?;
                match tts.synthesize(text).await {
                    Ok(pcm) => return Ok(pcm),
                    Err(err) => {
                        self.invalidate_index(idx);
                        last_err = err;
                        start_idx = idx + 1;
                    }
                }
            }
            Err(last_err)
        })
    }

    fn synthesize_stream<'a>(
        &'a self,
        text: &'a str,
        chunks: tokio::sync::mpsc::UnboundedSender<moss_voice_providers::Pcm>,
    ) -> moss_voice_providers::BoxFuture<'a, Result<(), moss_voice_providers::ProviderError>> {
        Box::pin(async move {
            let mut start_idx = 0;
            let mut last_err =
                moss_voice_providers::ProviderError::new("没有可用的本地降级语音合成模型");
            while start_idx < self.candidates.len() {
                let (idx, tts) = self.get_or_init_from(start_idx).await?;
                let (stage_tx, mut stage_rx) = tokio::sync::mpsc::unbounded_channel();
                let res = tts.synthesize_stream(text, stage_tx).await;
                let mut emitted = false;
                while let Ok(pcm) = stage_rx.try_recv() {
                    emitted = true;
                    let _ = chunks.send(pcm);
                }
                match res {
                    Ok(()) => return Ok(()),
                    Err(err) if !emitted && idx + 1 < self.candidates.len() => {
                        self.invalidate_index(idx);
                        last_err = err;
                        start_idx = idx + 1;
                    }
                    Err(err) => return Err(err),
                }
            }
            Err(last_err)
        })
    }
}

impl BuiltinVoice {
    pub(super) fn begin_fallback_tts_delete(
        &self,
        id: &str,
    ) -> Result<FallbackTtsDeleteGuard, CenterError> {
        let Some(fb) = self.fallback_tts.lock().ok().and_then(|g| g.clone()) else {
            return Ok(FallbackTtsDeleteGuard::default());
        };
        if fb.candidates.iter().all(|c| c.id != id) {
            return Ok(FallbackTtsDeleteGuard::default());
        }
        let mut state = fb.state.lock().unwrap_or_else(|p| p.into_inner());
        let matches_loaded = state
            .loaded
            .as_ref()
            .is_some_and(|(idx, _)| fb.candidates.get(*idx).is_some_and(|c| c.id == id));
        if matches_loaded {
            let still_alive = state
                .loaded
                .as_ref()
                .is_some_and(|(_, entry)| entry.is_alive());
            if still_alive && self.running() {
                return Err(CenterError::new(
                    "local_model_in_use",
                    409,
                    "内置服务正在使用本地兜底合成模型，请先停止内置服务再删除",
                ));
            }
            state.loaded = None;
        }
        state.deleting.insert(id.to_owned());
        Ok(FallbackTtsDeleteGuard {
            state: Some(fb.state.clone()),
            id: id.to_owned(),
        })
    }

    fn attach_fallback_tts(
        &self,
        parts: &mut LocalParts,
        local: &BuiltinLocal,
        shared_helper: Option<LocalHelper>,
        colab_tts_override: bool,
    ) {
        if parts.tts.is_none()
            && let Some(fb) = self.local_fallback_tts(local, shared_helper, colab_tts_override)
        {
            parts.fallback_tts_handle = Some(fb.clone());
            parts.fallback_tts = Some(fb);
        }
    }

    /// 查找本机已安装的本地 TTS 模型作为主用云端 TTS 失败时的自动降级兜底（优先使用用户选中的 `tts_model`，坏了自动顺延其他已安装模型）。
    fn local_fallback_tts(
        &self,
        local: &BuiltinLocal,
        shared_helper: Option<LocalHelper>,
        colab_tts_override: bool,
    ) -> Option<Arc<LazyLocalFallbackTts>> {
        if local.tts && !colab_tts_override {
            return None;
        }
        let helper_exe = helper_path().filter(|path| path.is_file());
        let speech_exe =
            moss_voice::speech_cpp::resolve_speech_binary(self.models.speech_runtime_root());
        let audiocpp_exe =
            moss_voice::audio_cpp::resolve_audiocpp_binary(self.models.audiocpp_runtime_root());
        let audiocpp_backend = audiocpp_exe
            .as_deref()
            .map(moss_voice::audio_cpp::recommended_audiocpp_backend)
            .unwrap_or("cpu");
        let clone_root = self.local_clone_root();
        let candidates: Vec<_> =
            ordered_local_tts_candidates(&local.tts_model, local.tts_speaker, |id| {
                self.models.path_if_installed(id)
            })
            .into_iter()
            .filter(|cand| match &cand.backend {
                FallbackTtsBackend::Helper(_) => helper_exe.is_some(),
                FallbackTtsBackend::SpeechCpp { .. } => speech_exe.is_some(),
                FallbackTtsBackend::AudioCpp { .. } => audiocpp_exe.is_some(),
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        Some(Arc::new(LazyLocalFallbackTts {
            helper_exe,
            speech_exe,
            audiocpp_exe,
            audiocpp_voice_dir: moss_voice::audio_cpp::clone_voices_dir(&clone_root),
            audiocpp_backend,
            audiocpp_resolve_ref: self.clone_voice_ref_resolver(),
            candidates,
            speed: local.tts_speed,
            shared_helper,
            state: Arc::new(Mutex::new(FallbackRuntimeState::default())),
        }))
    }

    fn attach_turn_predictor(parts: &mut LocalParts, spec_id: &'static str, worker: TurnHelper) {
        parts.turn_active = Some(spec_id);
        parts.turn_predictor = Some(Arc::new(LocalTurnEndPredictor { helper: worker }));
    }

    /// 按开关启动辅助程序或 `speech.cpp` 并加载选用且已安装的本地模型。没有打开任何本地开关（或模型未安装）时不启动子进程。
    pub(super) fn local_parts(
        &self,
        local: &BuiltinLocal,
        colab_tts_override: bool,
    ) -> Result<(LocalParts, Option<(LocalHelper, RunningLocalSpec)>), CenterError> {
        struct TerminateSpeechOnErr(Option<SpeechCppWorker>);
        impl Drop for TerminateSpeechOnErr {
            fn drop(&mut self) {
                if let Some(worker) = self.0.take() {
                    worker.terminate();
                }
            }
        }
        struct TerminateAudioCppOnErr(Option<AudioCppWorker>);
        impl Drop for TerminateAudioCppOnErr {
            fn drop(&mut self) {
                if let Some(worker) = self.0.take() {
                    worker.terminate();
                }
            }
        }
        struct TerminateTurnOnErr(Option<TurnHelper>);
        impl Drop for TerminateTurnOnErr {
            fn drop(&mut self) {
                if let Some(worker) = self.0.take() {
                    worker.terminate();
                }
            }
        }

        let pick = |kind| {
            local.selected(kind).and_then(|spec| {
                self.models
                    .path_if_installed(spec.id)
                    .map(|path| (spec, path))
            })
        };
        let vad = pick(ModelKind::Vad);
        let asr = pick(ModelKind::Asr);
        let tts = if colab_tts_override {
            None
        } else {
            pick(ModelKind::Tts)
        };
        let turn = pick(ModelKind::Turn);
        // GTCRN 前端降噪仅作用于本地判停与本地识别；未启用这两者时不加载也不单独拉起辅助进程
        let denoise = if vad.is_some() || asr.is_some() {
            pick(ModelKind::Denoise)
        } else {
            None
        };
        // 命令词检测跑在本地判停流上：没开本地判停时不加载
        let kws = if vad.is_some() {
            pick(ModelKind::Kws)
        } else {
            None
        };
        let (speech_tts, audiocpp_tts, helper_tts) = match tts {
            Some((spec, dir)) if spec.engine.uses_speech_runtime() => {
                (Some((spec, dir)), None, None)
            }
            Some((spec, dir)) if spec.engine.uses_audiocpp_runtime() => {
                (None, Some((spec, dir)), None)
            }
            other => (None, None, other),
        };
        let mut parts = LocalParts::default();
        let mut speech_guard = TerminateSpeechOnErr(None);
        let mut audiocpp_guard = TerminateAudioCppOnErr(None);
        if let Some((spec, dir)) = speech_tts {
            if let Some(voices) = spec.voices
                && (local.tts_speaker < 0 || local.tts_speaker > voices.max)
            {
                return Err(start_error(format!(
                    "{} 的音色编号须在 0 ~ {} 之间",
                    spec.label, voices.max
                )));
            }
            let speech_exe =
                moss_voice::speech_cpp::resolve_speech_binary(self.models.root_for(spec.id))
                    .ok_or_else(|| {
                        start_error(
                            "未找到 Qwen3-TTS 推理引擎 speech，请删除并重新下载 Qwen3-TTS 模型包",
                        )
                    })?;
            let model_file = resolve_model_entry_file(&dir, spec);
            let worker = SpeechCppWorker::spawn(&speech_exe, &model_file)
                .map_err(|error| start_error(format!("本地合成（{}）：{}", spec.label, error)))?;
            let voice = moss_voice::tts_voices::qwen3_tts_voice_slug(local.tts_speaker).to_owned();
            let instructions =
                moss_voice::tts_voices::speech_tts_instructions(spec, local.tts_speed).to_owned();
            parts.tts = Some(Arc::new(SpeechCppTts {
                worker: worker.clone(),
                voice,
                instructions,
            }));
            speech_guard.0 = Some(worker);
        } else if let Some((spec, dir)) = audiocpp_tts {
            let audiocpp_exe =
                moss_voice::audio_cpp::resolve_audiocpp_binary(self.models.root_for(spec.id))
                    .ok_or_else(|| {
                        start_error(
                            "未找到声音克隆推理引擎 audiocpp_server，请删除并重新下载 Qwen3-TTS 声音克隆模型",
                        )
                    })?;
            let backend = moss_voice::audio_cpp::recommended_audiocpp_backend(&audiocpp_exe);
            let voice_dir = moss_voice::audio_cpp::clone_voices_dir(&self.local_clone_root());
            let model_file = resolve_model_entry_file(&dir, spec);
            let worker = AudioCppWorker::spawn(&audiocpp_exe, &model_file, &voice_dir, backend)
                .map_err(|error| start_error(format!("本地合成（{}）：{}", spec.label, error)))?;
            parts.tts = Some(Arc::new(AudioCppTts {
                worker: worker.clone(),
                resolve_ref: self.clone_voice_ref_resolver(),
            }));
            audiocpp_guard.0 = Some(worker);
        }
        let mut turn_guard = TerminateTurnOnErr(None);
        if let Some((spec, path)) = turn {
            let file = resolve_model_entry_file(&path, spec);
            let turn_exe =
                helper_path().ok_or_else(|| start_error("无法确定 MOSS 程序所在目录"))?;
            let worker = TurnHelper::spawn(&turn_exe, Some(&file))
                .map_err(|error| start_error(format!("语义判停（{}）：{}", spec.label, error)))?;
            Self::attach_turn_predictor(&mut parts, spec.id, worker.clone());
            turn_guard.0 = Some(worker);
        }
        let needs_helper =
            vad.is_some() || asr.is_some() || helper_tts.is_some() || denoise.is_some();
        if !needs_helper {
            self.attach_fallback_tts(&mut parts, local, None, colab_tts_override);
            parts.speech_worker = speech_guard.0.take();
            parts.audiocpp_worker = audiocpp_guard.0.take();
            parts.turn_worker = turn_guard.0.take();
            return Ok((parts, None));
        }
        let exe = helper_path().ok_or_else(|| start_error("无法确定 MOSS 程序所在目录"))?;
        let helper = LocalHelper::spawn(&exe).map_err(|error| start_error(error.to_string()))?;
        let mut running_spec = RunningLocalSpec {
            vad: None,
            kws: None,
            denoise: None,
            asr: None,
            tts: None,
        };
        if let Some((spec, path)) = denoise {
            let file = resolve_model_entry_file(&path, spec);
            helper
                .load_denoiser(Some(&file))
                .map_err(|error| start_error(format!("本地降噪（{}）：{}", spec.label, error)))?;
            running_spec.denoise = Some((spec.id, file));
            parts.denoise_active = Some(spec.id);
        }
        if let Some((spec, model)) = vad {
            let engine = if spec.engine == Engine::TenVad {
                VadEngine::Ten
            } else {
                VadEngine::Silero
            };
            // 单文件（Silero v4 / TEN）即文件本身；目录包（Silero v6.2）取目录内的模型文件
            let model = resolve_model_entry_file(&model, spec);
            helper
                .load_vad(&model, VadParams::for_engine(engine), engine)
                .map_err(|error| start_error(format!("本地判停（{}）：{}", spec.label, error)))?;
            running_spec.vad = Some((spec.id, model));
            parts.vad = Some(Arc::new(LocalVadFactory::new(
                helper.clone(),
                format!("本地判停 {}（{}）", spec.label, spec.id),
            )));
            if let Some((spec, dir)) = kws {
                helper.load_kws(Some(&dir)).map_err(|error| {
                    start_error(format!("命令词抗噪（{}）：{}", spec.label, error))
                })?;
                running_spec.kws = Some((spec.id, dir));
                parts.kws_active = Some(spec.id);
            }
        }
        if let Some((spec, dir)) = asr {
            let engine = helper_asr_engine(spec.engine);
            let hotwords = self.hotwords(local);
            helper
                .load_asr_with_options(
                    &dir,
                    engine,
                    moss_voice_providers::local::AsrDecodeOptions {
                        beam_search: local.asr_beam_search,
                        hotwords: hotwords.clone(),
                    },
                )
                .map_err(|error| start_error(format!("本地识别（{}）：{}", spec.label, error)))?;
            running_spec.asr = Some((spec.id, dir, local.asr_beam_search, hotwords));
            parts.asr = Some(LocalAsr {
                helper: helper.clone(),
            });
            if engine.streaming() {
                parts.streaming_asr = Some(Arc::new(LocalStreamingAsr {
                    helper: helper.clone(),
                }));
            }
        }
        if let Some((spec, dir)) = helper_tts {
            let engine = helper_tts_engine(spec.engine)
                .ok_or_else(|| start_error(format!("不支持的本地合成引擎：{}", spec.label)))?;
            let info = helper
                .load_tts(&dir, engine)
                .map_err(|error| start_error(format!("本地合成（{}）：{}", spec.label, error)))?;
            running_spec.tts = Some((spec.id, dir, info));
            // 单音色模型忽略音色编号
            let speaker = if info.speakers > 1 {
                if local.tts_speaker < 0 || local.tts_speaker as u32 >= info.speakers {
                    return Err(start_error(format!(
                        "{} 的音色编号须在 0 ~ {} 之间",
                        spec.label,
                        info.speakers - 1
                    )));
                }
                local.tts_speaker
            } else {
                0
            };
            parts.tts = Some(Arc::new(LocalTts {
                helper: helper.clone(),
                speaker,
                speed: local.tts_speed,
            }));
        } else {
            self.attach_fallback_tts(&mut parts, local, Some(helper.clone()), colab_tts_override);
        }
        parts.speech_worker = speech_guard.0.take();
        parts.audiocpp_worker = audiocpp_guard.0.take();
        parts.turn_worker = turn_guard.0.take();
        Ok((parts, Some((helper, running_spec))))
    }

    /// 全链连贯试跑：当内置服务已运行且已加载同款本地模型时直接复用现有进程（零重载），否则按需启动临时子进程。
    pub(super) fn local_parts_for_chain_test(
        &self,
        local: &BuiltinLocal,
        colab_tts_override: bool,
    ) -> Result<(LocalParts, ChainTestLocalGuard), CenterError> {
        let pick = |kind| {
            local.selected(kind).and_then(|spec| {
                self.models
                    .path_if_installed(spec.id)
                    .map(|path| (spec, path))
            })
        };
        let vad = pick(ModelKind::Vad);
        let asr = pick(ModelKind::Asr);
        let tts = if colab_tts_override {
            None
        } else {
            pick(ModelKind::Tts)
        };
        let turn = pick(ModelKind::Turn);
        let denoise = if vad.is_some() || asr.is_some() {
            pick(ModelKind::Denoise)
        } else {
            None
        };
        let kws = if vad.is_some() {
            pick(ModelKind::Kws)
        } else {
            None
        };
        if vad.is_none() && asr.is_none() && tts.is_none() && turn.is_none() && denoise.is_none() {
            let mut parts = LocalParts::default();
            self.attach_fallback_tts(&mut parts, local, None, colab_tts_override);
            let guard = ChainTestLocalGuard {
                _helper: None,
                turn_worker: None,
                speech_worker: None,
                audiocpp_worker: None,
                fallback_tts: parts.fallback_tts_handle.take(),
            };
            return Ok((parts, guard));
        }
        let (speech_tts, audiocpp_tts, helper_tts) = match tts {
            Some((spec, dir)) if spec.engine.uses_speech_runtime() => {
                (Some((spec, dir)), None, None)
            }
            Some((spec, dir)) if spec.engine.uses_audiocpp_runtime() => {
                (None, Some((spec, dir)), None)
            }
            other => (None, None, other),
        };
        let needs_helper =
            vad.is_some() || asr.is_some() || helper_tts.is_some() || denoise.is_some();
        let hotwords = self.hotwords(local);
        let reusable_helper = if needs_helper {
            let helper_guard = self.helper.lock().ok();
            let spec_guard = self.running_local.lock().ok();
            if let (Some(Some(helper)), Some(Some(running_spec))) =
                (helper_guard.as_deref(), spec_guard.as_deref())
                && helper.is_alive()
            {
                let denoise_matches = match &denoise {
                    Some((spec, path)) => {
                        running_spec.denoise.as_ref()
                            == Some(&(spec.id, resolve_model_entry_file(path, spec)))
                    }
                    None => true,
                };
                let vad_matches = match &vad {
                    Some((spec, path)) => {
                        running_spec.vad.as_ref()
                            == Some(&(spec.id, resolve_model_entry_file(path, spec)))
                    }
                    None => true,
                };
                let kws_matches = running_spec.kws.as_ref()
                    == kws
                        .as_ref()
                        .map(|(spec, dir)| (spec.id, dir.clone()))
                        .as_ref();
                let asr_matches = match &asr {
                    Some((spec, dir)) => {
                        running_spec.asr.as_ref()
                            == Some(&(
                                spec.id,
                                dir.clone(),
                                local.asr_beam_search,
                                hotwords.clone(),
                            ))
                    }
                    None => true,
                };
                let tts_info = match &helper_tts {
                    Some((spec, dir)) => running_spec
                        .tts
                        .as_ref()
                        .filter(|(id, loaded_dir, _)| *id == spec.id && loaded_dir == dir)
                        .map(|(_, _, info)| Some(*info)),
                    None => Some(None),
                };
                if denoise_matches
                    && vad_matches
                    && kws_matches
                    && asr_matches
                    && let Some(tts_info) = tts_info
                {
                    Some((helper.clone(), tts_info))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if !needs_helper || reusable_helper.is_some() {
            let mut parts = LocalParts::default();
            let mut guard = ChainTestLocalGuard::default();
            if let Some((helper, tts_info)) = reusable_helper {
                if let Some((spec, _)) = denoise {
                    parts.denoise_active = Some(spec.id);
                }
                if let Some((spec, _)) = kws {
                    parts.kws_active = Some(spec.id);
                }
                if let Some((spec, _)) = vad {
                    parts.vad = Some(Arc::new(LocalVadFactory::new(
                        helper.clone(),
                        format!("本地判停 {}（{}）", spec.label, spec.id),
                    )));
                }
                if let Some((spec, _)) = asr {
                    parts.asr = Some(LocalAsr {
                        helper: helper.clone(),
                    });
                    if helper_asr_engine(spec.engine).streaming() {
                        parts.streaming_asr = Some(Arc::new(LocalStreamingAsr {
                            helper: helper.clone(),
                        }));
                    }
                }
                if let (Some((spec, _)), Some(info)) = (helper_tts, tts_info) {
                    let speaker = if info.speakers > 1 {
                        if local.tts_speaker < 0 || local.tts_speaker as u32 >= info.speakers {
                            return Err(start_error(format!(
                                "{} 的音色编号须在 0 ~ {} 之间",
                                spec.label,
                                info.speakers - 1
                            )));
                        }
                        local.tts_speaker
                    } else {
                        0
                    };
                    parts.tts = Some(Arc::new(LocalTts {
                        helper: helper.clone(),
                        speaker,
                        speed: local.tts_speed,
                    }));
                }
            }
            if let Some((spec, dir)) = speech_tts {
                if let Some(voices) = spec.voices
                    && (local.tts_speaker < 0 || local.tts_speaker > voices.max)
                {
                    return Err(start_error(format!(
                        "{} 的音色编号须在 0 ~ {} 之间",
                        spec.label, voices.max
                    )));
                }
                let model_file = resolve_model_entry_file(&dir, spec);
                let reusable_worker = self.speech_worker.lock().ok().and_then(|w| {
                    w.as_ref()
                        .filter(|worker| worker.is_alive() && worker.model_path() == model_file)
                        .cloned()
                });
                let worker = match reusable_worker {
                    Some(w) => w,
                    None => {
                        let speech_exe = moss_voice::speech_cpp::resolve_speech_binary(
                            self.models.root_for(spec.id),
                        )
                        .ok_or_else(|| {
                            start_error("未找到 Qwen3-TTS 推理引擎 speech，请删除并重新下载 Qwen3-TTS 模型包")
                        })?;
                        let spawned =
                            SpeechCppWorker::spawn(&speech_exe, &model_file).map_err(|error| {
                                start_error(format!("本地合成（{}）：{}", spec.label, error))
                            })?;
                        guard.speech_worker = Some(spawned.clone());
                        spawned
                    }
                };
                let voice =
                    moss_voice::tts_voices::qwen3_tts_voice_slug(local.tts_speaker).to_owned();
                let instructions =
                    moss_voice::tts_voices::speech_tts_instructions(spec, local.tts_speed)
                        .to_owned();
                parts.tts = Some(Arc::new(SpeechCppTts {
                    worker,
                    voice,
                    instructions,
                }));
            } else if let Some((spec, dir)) = audiocpp_tts {
                let model_file = resolve_model_entry_file(&dir, spec);
                let voice_dir = moss_voice::audio_cpp::clone_voices_dir(&self.local_clone_root());
                let reusable_worker = self.audiocpp_worker.lock().ok().and_then(|w| {
                    w.as_ref()
                        .filter(|worker| {
                            worker.is_alive()
                                && worker.model_path() == model_file
                                && worker.voice_dir() == voice_dir
                        })
                        .cloned()
                });
                let worker = match reusable_worker {
                    Some(w) => w,
                    None => {
                        let audiocpp_exe = moss_voice::audio_cpp::resolve_audiocpp_binary(
                            self.models.root_for(spec.id),
                        )
                        .ok_or_else(|| {
                            start_error("未找到声音克隆推理引擎 audiocpp_server，请删除并重新下载 Qwen3-TTS 声音克隆模型")
                        })?;
                        let backend =
                            moss_voice::audio_cpp::recommended_audiocpp_backend(&audiocpp_exe);
                        let spawned =
                            AudioCppWorker::spawn(&audiocpp_exe, &model_file, &voice_dir, backend)
                                .map_err(|error| {
                                    start_error(format!("本地合成（{}）：{}", spec.label, error))
                                })?;
                        guard.audiocpp_worker = Some(spawned.clone());
                        spawned
                    }
                };
                parts.tts = Some(Arc::new(AudioCppTts {
                    worker,
                    resolve_ref: self.clone_voice_ref_resolver(),
                }));
            } else if parts.tts.is_none() {
                // 兜底合成不向常驻运行中的 helper 发 LoadTts（避免覆盖线上兜底槽位或永久滞留 ONNX 会话）；
                // 主云端 TTS 可用时惰性不拉起任何进程，真触发兜底时单独拉起临时进程并在试跑结束后自动回收。
                self.attach_fallback_tts(&mut parts, local, None, colab_tts_override);
                guard.fallback_tts = parts.fallback_tts_handle.take();
            }
            if let Some((spec, path)) = turn {
                let file = resolve_model_entry_file(&path, spec);
                let reusable_turn = self.turn_worker.lock().ok().and_then(|w| {
                    w.as_ref()
                        .filter(|worker| {
                            worker.is_alive() && worker.model_path() == Some(file.as_path())
                        })
                        .cloned()
                });
                let worker = match reusable_turn {
                    Some(w) => w,
                    None => {
                        let exe = helper_path()
                            .ok_or_else(|| start_error("无法确定 MOSS 程序所在目录"))?;
                        let spawned = TurnHelper::spawn(&exe, Some(&file)).map_err(|error| {
                            start_error(format!("语义判停（{}）：{}", spec.label, error))
                        })?;
                        guard.turn_worker = Some(spawned.clone());
                        spawned
                    }
                };
                Self::attach_turn_predictor(&mut parts, spec.id, worker);
            }
            return Ok((parts, guard));
        }
        let (mut parts, spawned) = self.local_parts(local, colab_tts_override)?;
        let guard = ChainTestLocalGuard {
            _helper: spawned.map(|(h, _)| h),
            turn_worker: parts.turn_worker.take(),
            speech_worker: parts.speech_worker.take(),
            audiocpp_worker: parts.audiocpp_worker.take(),
            fallback_tts: parts.fallback_tts_handle.take(),
        };
        Ok((parts, guard))
    }

    /// 三个分区都可用时构造对话（本地识别 / 合成优先于云端；开启 Colab T4 联动时临时接管大模型与合成）；否则返回 `None`（回声模式）。
    /// 第二项为合成是否流式（豆包），用于缩短播放启动缓冲。
    pub(super) fn conversation(
        &self,
        models: &moss_voice::settings::BuiltinModels,
        local: &mut LocalParts,
        experience: &BuiltinLocal,
        colab: Option<&(moss_voice::colab::BuiltinColab, String)>,
        timers: Arc<moss_voice_server::TimerStore>,
    ) -> Result<Option<(Conversation, bool)>, CenterError> {
        let key = |section| self.secret(section).map(Option::unwrap_or_default);
        let invalid = |error: moss_voice_providers::ProviderError| {
            CenterError::new("builtin_models_invalid", 400, error.to_string())
        };
        let (llm_url, llm_key, llm_model, llm_label) = match colab {
            Some((c, c_key)) => (
                c.url.as_str(),
                c_key.clone(),
                c.llm_model.as_str(),
                format!("LLM:colab/{}", c.llm_model),
            ),
            None => {
                let Some(llm) = &models.llm else {
                    return Ok(None);
                };
                (
                    llm.url.as_str(),
                    key(Section::Llm)?,
                    llm.model.as_str(),
                    format!("LLM:{}", llm.model),
                )
            }
        };
        let hotwords = self.hotwords(experience);
        let mut streaming_asr: Option<Arc<dyn StreamingAsr>> = None;
        let asr: Arc<dyn Asr> = match (local.asr.take(), &models.asr) {
            (Some(local_asr), _) => {
                streaming_asr = local.streaming_asr.take();
                Arc::new(local_asr)
            }
            (None, Some(cloud)) if cloud.kind == ProviderKind::Doubao => {
                let doubao = Arc::new(
                    DoubaoAsr::new(&cloud.url, &key(Section::Asr)?, &cloud.model)
                        .map_err(invalid)?
                        .with_hotwords(&hotwords),
                );
                streaming_asr = Some(doubao.clone());
                doubao
            }
            (None, Some(cloud)) => Arc::new(
                OpenAiAsr::new(&cloud.url, &key(Section::Asr)?, &cloud.model, Some("zh"))
                    .map_err(invalid)?
                    .with_hotwords(&hotwords),
            ),
            (None, None) => return Ok(None),
        };
        let (tts, streaming_tts, tts_label): (Arc<dyn Tts>, bool, String) =
            match (colab, local.tts.take(), &models.tts) {
                (Some((c, c_key)), _, _) => {
                    let openai =
                        OpenAiTts::new(&c.url, c_key, &c.tts_model, &c.tts_voice, c.tts_speed);
                    (
                        Arc::new(openai.map_err(invalid)?),
                        false,
                        format!("TTS:colab/{}", c.tts_model),
                    )
                }
                (None, Some(local), _) => (
                    local,
                    true,
                    format!("TTS:{}", experience.tts_model.as_str()),
                ),
                (None, None, Some(cloud)) if cloud.kind == ProviderKind::Doubao => {
                    let doubao = DoubaoTts::new(
                        &cloud.url,
                        &key(Section::Tts)?,
                        &cloud.model,
                        &cloud.voice,
                        cloud.speed,
                    );
                    (
                        Arc::new(doubao.map_err(invalid)?),
                        true,
                        format!("TTS:{}", cloud.model),
                    )
                }
                (None, None, Some(cloud)) if cloud.kind == ProviderKind::Dashscope => {
                    let dashscope = DashscopeTts::new(
                        &cloud.url,
                        &key(Section::Tts)?,
                        &cloud.model,
                        &cloud.voice,
                        cloud.speed,
                    );
                    (
                        Arc::new(dashscope.map_err(invalid)?),
                        true,
                        format!("TTS:{}", cloud.model),
                    )
                }
                (None, None, Some(cloud)) => {
                    let openai = OpenAiTts::new(
                        &cloud.url,
                        &key(Section::Tts)?,
                        &cloud.model,
                        &cloud.voice,
                        cloud.speed,
                    );
                    (
                        Arc::new(openai.map_err(invalid)?),
                        false,
                        format!("TTS:{}", cloud.model),
                    )
                }
                (None, None, None) => return Ok(None),
            };
        let mut jev_options = JevFilterOptions {
            enabled: experience.jev_filter_enabled,
            unfinished_hold: experience.jev_unfinished_hold,
            fast_route: experience.read_only_shortcuts && experience.jev_fast_route,
            tool_prune: experience.jev_tool_prune,
            memory_filter: experience.jev_memory_filter,
        };
        let jev_model = moss_voice::settings::validate_jev_model(&experience.jev_model)
            .unwrap_or_else(|_| moss_voice::settings::default_jev_model());
        let local_jev_id = moss_voice::settings::canonical_local_jev_model(&jev_model);
        // 2026-11-07：本地 OneJev 按档位与是否有显卡加速决定等待预算；来不及时关闭“没说完”判断（界面另有说明）
        let local_budget = local_jev_id
            .and_then(|id| moss_voice::onejev::local_jev_budget(id, self.onejev_uses_gpu(id)));
        if local_budget.is_some_and(|budget| budget.hold_ms.is_none()) {
            jev_options.unfinished_hold = false;
        }
        let jev_ready = match local_jev_id {
            Some(id) => self.models.path_if_installed(id).is_some(),
            None => self.jev_effective_secret()?.is_some(),
        };
        let mut labels = vec![
            llm_label,
            format!(
                "ASR:{}",
                if experience.asr {
                    experience.asr_model.as_str()
                } else {
                    models
                        .asr
                        .as_ref()
                        .map(|v| v.model.as_str())
                        .unwrap_or("unknown")
                }
            ),
            tts_label,
        ];
        if jev_options.active() && jev_ready {
            labels.push(format!("Jev:{jev_model}"));
        }
        if let Some(kws_id) = local.kws_active {
            labels.push(format!("Kws:{kws_id}"));
        }
        if let Some(denoise_id) = local.denoise_active {
            labels.push(format!("Denoise:{denoise_id}"));
        }
        if let Some(turn_id) = local.turn_active {
            labels.push(format!("Turn:{turn_id}"));
        }
        let llm = OpenAiLlm::new(llm_url, &llm_key, llm_model)
            .map_err(invalid)?
            .with_fast_reply(experience.llm_fast_reply);

        self.persona.configure(persona_config(experience));
        let tools = self.base_tool_executor_with_timers(timers)?;
        let system_prompt = self.persona.system_prompt();
        let asr_corrections = moss_voice::settings::effective_asr_corrections(
            experience.asr_homophone_fix,
            &experience.custom_asr_corrections,
        );
        let mut conversation = Conversation::new(
            asr,
            Arc::new(llm),
            tts,
            tools,
            ConversationSettings {
                system_prompt,
                asr_corrections,
                read_only_shortcuts: experience.read_only_shortcuts,
                announce_job_done: experience.job_done_announce,
                ignore_bystanders: experience.ignore_bystanders,
                command_keywords: local.kws_active.is_some(),
                screen_preview_sync: experience.screen_preview_sync,
                model_labels: labels,
                ..Default::default()
            },
        )
        .with_memory(self.memory.clone())
        .with_speaker_enrollment(self.speaker.clone())
        .with_persona(self.persona.clone());
        // n105 完整过程记录（默认关）：与激光侧共用同一个记录器，开关在偏好设置里，改了不用重启
        conversation.process_trace = self.host.api_service.process_trace().cloned();
        if let Some(fallback_tts) = local.fallback_tts.take() {
            conversation = conversation.with_fallback_tts(fallback_tts);
        }
        if jev_options.active() {
            let filter = if let Some(local_id) = local_jev_id {
                let reader: moss_voice_server::jev::SecretReader =
                    Arc::new(|| Some("moss-local-jev".to_owned()));
                let upstream_model = moss_voice::settings::local_jev_upstream_model_name(local_id);
                let client = Arc::new(OpenRouterDecisionClient::with_endpoint(
                    moss_voice::models::DEFAULT_LOCAL_LAYA_DECISIONS_URL,
                    reader,
                    upstream_model,
                ));
                match local_budget {
                    Some(budget) => VoiceJevFilter::new(client, jev_options).with_timeouts(
                        std::time::Duration::from_millis(budget.turn_ms),
                        std::time::Duration::from_millis(budget.hold_ms.unwrap_or(0)),
                    ),
                    None => VoiceJevFilter::new(client, jev_options),
                }
            } else {
                let db = self.host.db.clone();
                let settings_file = self.settings_file();
                let reader = Arc::new(move || {
                    let guard = db.lock().ok()?;
                    resolve_openrouter_jev_secret_locked(&guard, &settings_file)
                        .ok()
                        .and_then(|(secret, _)| secret)
                });
                let client = Arc::new(OpenRouterDecisionClient::with_model(reader, jev_model));
                VoiceJevFilter::new(client, jev_options)
            };
            conversation = conversation.with_jev_filter(Arc::new(filter));
        }
        // NV19: both hooks stay inert until the user switches them on in the private window.
        let (retrieval, extraction) = self.context.hooks();
        conversation = conversation
            .with_context_retrieval(retrieval)
            .with_context_extraction(extraction);
        if self.identity.valid_calibration().is_some()
            && self.settings().builtin_person_memory_enabled
        {
            // 换设备后若尚未下载声纹模型，仍必须挂上个人上下文：未知身份绝不回退共享设备记忆，同时不阻断语音服务启动
            match self.prepare_speaker_helper() {
                Ok(()) => {
                    if let Ok(mut slot) = self.speaker_warning.lock() {
                        *slot = None;
                    }
                }
                Err(error) => {
                    let warning = format!(
                        "声纹按人记忆暂未生效（{}）：当前按未知身份处理、不会回退共享设备记忆，请下载声纹模型后重启内置服务",
                        error.message
                    );
                    eprintln!("[moss-desktop] {warning}");
                    self.diagnostics.push_from(
                        moss_voice_server::DiagSource::Moss,
                        EventRecord::warn(DiagCategory::Model, warning.clone())
                            .hint("到“隐私与家居 → 声纹”下载声纹模型，再重启内置服务"),
                    );
                    if let Ok(mut slot) = self.speaker_warning.lock() {
                        *slot = Some(warning);
                    }
                }
            }
            conversation = conversation.with_personal_context(self.personal.clone());
        } else if let Ok(mut slot) = self.speaker_warning.lock() {
            *slot = None;
        }
        if let Some(streaming) = streaming_asr {
            conversation = conversation.with_streaming_asr(streaming);
        }
        if let Some(predictor) = local.turn_predictor.take() {
            conversation = conversation.with_turn_predictor(predictor);
        }
        Ok(Some((conversation, streaming_tts)))
    }

    pub(super) fn base_tool_executor(&self) -> Result<Arc<dyn ToolExecutor>, CenterError> {
        self.base_tool_executor_with_timers(Arc::new(moss_voice_server::TimerStore::new()))
    }

    pub(super) fn base_tool_executor_with_timers(
        &self,
        timers: Arc<moss_voice_server::TimerStore>,
    ) -> Result<Arc<dyn ToolExecutor>, CenterError> {
        let original: Arc<dyn ToolExecutor> = Arc::new(HostTools::new(Arc::new(DesktopToolHost {
            host: self.host.clone(),
        })));
        let plugin_host = Arc::new(DesktopInfoHost {
            host: self.host.clone(),
        });
        let info =
            InfoTools::new(plugin_host).map_err(|_| internal("信息插件 HTTP 客户端初始化失败"))?;
        let combined: Arc<dyn ToolExecutor> = Arc::new(CombinedTools {
            original,
            info: Arc::new(info),
        });
        let tools: Arc<dyn ToolExecutor> = Arc::new(HomeTools {
            original: combined,
            home: Arc::new(DesktopHomeHost {
                host: self.host.clone(),
                control: self.home_control.clone(),
            }),
        });
        let persona_host = self.host.clone();
        let persona_tools: Arc<dyn ToolExecutor> = Arc::new(PersonaTools {
            original: tools,
            persona: self.persona.clone(),
            permitted: Some(Arc::new(move |ctx: &ToolContext| {
                persona_host
                    .api_service
                    .voice_session_allowed(&ctx.device_id, &ctx.session_id)
            })),
        });
        let timer_host = self.host.clone();
        let timer_disabled_host = self.host.clone();
        Ok(Arc::new(moss_voice_server::TimerTools {
            original: persona_tools,
            timers,
            permitted: Some(Arc::new(move |ctx: &ToolContext| {
                timer_host
                    .api_service
                    .voice_session_allowed(&ctx.device_id, &ctx.session_id)
            })),
            disabled: Some(Arc::new(move || {
                timer_disabled_host
                    .api_service
                    .tool_disabled(moss_voice_server::TIMER_TOOL)
            })),
        }))
    }

    /// 构造与真机一致的完整工具规格清单（仅用于 Jev 工具组评估，不执行任何调用）。
    pub(super) fn jev_tool_specs(
        &self,
        music: &moss_voice::settings::BuiltinMusic,
    ) -> Result<Vec<moss_voice_providers::ToolSpec>, CenterError> {
        let mut list = self.base_tool_executor()?.tools();
        if music::scan_music_for_start(music, true).0.is_some() {
            list.retain(|tool| tool.name != "play_music");
            list.push(moss_voice_server::music::play_music_tool_spec());
        }
        Ok(list)
    }
}

#[cfg(test)]
mod local_fallback_tests {
    use super::ordered_local_tts_candidates;
    use std::path::PathBuf;

    #[test]
    fn user_selected_fallback_model_is_prioritized_then_other_installed_models_follow() {
        let both_installed = |id: &str| match id {
            "kokoro" | "matcha_zh_en" => Some(PathBuf::from(format!("/models/{id}"))),
            _ => None,
        };
        let matcha_first = ordered_local_tts_candidates("matcha_zh_en", 0, both_installed);
        assert_eq!(
            matcha_first.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec!["matcha_zh_en", "kokoro"],
            "用户指定 Matcha 兜底时应先试 Matcha，若失败再顺延 Kokoro"
        );

        let kokoro_first = ordered_local_tts_candidates("kokoro", 50, both_installed);
        assert_eq!(
            kokoro_first.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec!["kokoro", "matcha_zh_en"],
            "用户指定 Kokoro 兜底时应先试 Kokoro，若失败再顺延 Matcha"
        );

        let only_kokoro = ordered_local_tts_candidates("matcha_zh_en", 0, |id| {
            (id == "kokoro").then(|| PathBuf::from("/models/kokoro"))
        });
        assert_eq!(
            only_kokoro.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec!["kokoro"],
            "用户所选未安装时自动顺延本机已安装的另一个模型"
        );
        assert_eq!(
            only_kokoro[0].preferred_speaker, 50,
            "跨模型顺延到 Kokoro v1.0 时须使用其默认中文音色 50，避免把 0/3 号英文音色带入导致洋腔读中文"
        );
        let v11_to_v10 = ordered_local_tts_candidates("kokoro_v11", 3, |id| {
            (id == "kokoro").then(|| PathBuf::from("/models/kokoro"))
        });
        assert_eq!(v11_to_v10[0].preferred_speaker, 50);
    }

    #[test]
    fn qwen3_and_zipvoice_fallback_candidates_respect_engine_and_speaker_bounds() {
        use super::FallbackTtsBackend;
        use moss_voice_providers::local::TtsEngine;

        let installed = |id: &str| match id {
            "qwen3_tts_0_6b" => Some(PathBuf::from("/models/qwen3-tts-0.6b-customvoice")),
            "zipvoice_distill" => Some(PathBuf::from("/models/zipvoice-zh-en-distill")),
            "kokoro" => Some(PathBuf::from("/models/kokoro")),
            _ => None,
        };
        let qwen_first = ordered_local_tts_candidates("qwen3_tts_0_6b", 5, installed);
        assert_eq!(
            qwen_first.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec!["qwen3_tts_0_6b", "kokoro", "zipvoice_distill"]
        );
        assert_eq!(qwen_first[0].preferred_speaker, 5);
        assert_eq!(
            qwen_first[0].backend,
            FallbackTtsBackend::SpeechCpp {
                model_file: PathBuf::from(
                    "/models/qwen3-tts-0.6b-customvoice/Qwen3-TTS-12Hz-0.6B-CustomVoice-Q8_0.gguf"
                ),
            }
        );
        assert_eq!(qwen_first[1].preferred_speaker, 50);
        assert_eq!(
            qwen_first[2].backend,
            FallbackTtsBackend::Helper(TtsEngine::ZipVoice)
        );
        assert_eq!(qwen_first[2].preferred_speaker, 0);

        let zip_invalid_spk = ordered_local_tts_candidates("zipvoice_distill", 99, installed);
        assert_eq!(zip_invalid_spk[0].id, "zipvoice_distill");
        assert_eq!(
            zip_invalid_spk[0].preferred_speaker, 0,
            "音色编号超出模型上限时须回退为该模型默认中文音色"
        );
    }
}
