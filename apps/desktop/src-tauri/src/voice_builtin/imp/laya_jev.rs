//! 本地 Jev 决策服务（OneJev / Laya）的环境探测、启停、配置与测试。

use super::*;

pub(super) fn probe_local_jev_health_model(
    addr: std::net::SocketAddr,
    connect_timeout: std::time::Duration,
) -> Option<String> {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, connect_timeout) else {
        return None;
    };
    let io_timeout = Some(std::time::Duration::from_millis(300));
    let _ = stream.set_write_timeout(io_timeout);
    let _ = stream.set_read_timeout(io_timeout);
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1:18090\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return None;
    }
    let mut buf = [0u8; 1024];
    let mut read_total = 0;
    while read_total < buf.len() {
        match stream.read(&mut buf[read_total..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => read_total += n,
        }
    }
    let raw = &buf[..read_total];
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..sep]);
    let status_line = head.lines().next()?;
    let is_200 = status_line == "HTTP/1.1 200"
        || status_line.starts_with("HTTP/1.1 200 ")
        || status_line == "HTTP/1.0 200"
        || status_line.starts_with("HTTP/1.0 200 ");
    if !is_200 {
        return None;
    }
    let body = String::from_utf8_lossy(&raw[sep + 4..]);
    let trimmed = body.trim();
    if trimmed.eq_ignore_ascii_case("ok") {
        return Some(moss_voice::settings::LOCAL_LAYA_JEV_MODEL.to_owned());
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return None;
    };
    let healthy = json.get("ok").and_then(serde_json::Value::as_bool) == Some(true)
        || json
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| {
                matches!(
                    s.trim().to_ascii_lowercase().as_str(),
                    "ok" | "healthy" | "ready"
                )
            });
    if !healthy {
        return None;
    }
    let is_onejev_like = json.get("engine").and_then(serde_json::Value::as_str) == Some("onejev")
        || json.get("fork_mode").is_some()
        || json.get("prompt_version").is_some();
    if let Some(raw_model) = json
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(canonical) = moss_voice::settings::canonical_local_jev_model(raw_model) {
            return Some(canonical.to_owned());
        }
        if is_onejev_like {
            return Some(format!("external_onejev:{raw_model}"));
        }
    }
    if is_onejev_like {
        return Some("external_onejev".to_owned());
    }
    Some(moss_voice::settings::LOCAL_LAYA_JEV_MODEL.to_owned())
}

pub(super) struct LocalLayaRunner {
    pub(super) active_model: Option<String>,
    pub(super) starting_model: Option<String>,
    pub(super) starting_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub(super) cancelling_model: Option<String>,
    pub(super) child: Option<std::process::Child>,
    pub(super) job: Option<moss_voice::job::JobHandle>,
    pub(super) onejev: Option<moss_voice::onejev::OneJevGateway>,
    pub(super) generation: u64,
    /// 最近一次启动失败（模型编号、原因），供状态接口说明；开始新的启动或启动成功时清除（2026-11-07）。
    pub(super) last_failure: Option<(String, String)>,
}

impl LocalLayaRunner {
    fn is_alive_for(&mut self, canonical_model: &str) -> bool {
        if self.active_model.as_deref() != Some(canonical_model) {
            return false;
        }
        if canonical_model == moss_voice::settings::LOCAL_LAYA_JEV_MODEL {
            self.child
                .as_mut()
                .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
        } else {
            self.onejev
                .as_ref()
                .is_some_and(moss_voice::onejev::OneJevGateway::is_alive)
        }
    }

    fn is_starting_for(&self, canonical_model: &str) -> bool {
        self.starting_model.as_deref() == Some(canonical_model)
            && self
                .starting_cancel
                .as_ref()
                .is_some_and(|c| !c.load(std::sync::atomic::Ordering::SeqCst))
    }

    fn cancel_in_flight(&mut self) {
        if let Some(model) = self.starting_model.take() {
            self.cancelling_model = Some(model);
        }
        if let Some(cancel) = self.starting_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.cancel_in_flight();
        self.kill_reaped();
    }

    fn kill_reaped(&mut self) {
        self.active_model = None;
        if let Some(mut gw) = self.onejev.take() {
            gw.stop();
        }
        let job = self.job.take();
        if let Some(mut child) = self.child.take() {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let pid = child.id();
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .creation_flags(0x0800_0000)
                    .status();
            }
            if let Some(job) = job.as_ref() {
                job.terminate();
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for LocalLayaRunner {
    fn drop(&mut self) {
        self.stop();
    }
}

impl BuiltinVoice {
    /// MOSS 可以自动管理的 Laya 根目录；用户用 `MOSS_LAYA_PYTHON` 指定了外部 Python 时返回 `None`。
    pub(super) fn laya_env_root(&self) -> Option<std::path::PathBuf> {
        if moss_laya_python_env().is_some() {
            return None;
        }
        #[cfg(test)]
        {
            self.laya_root_for_test
                .lock()
                .ok()
                .and_then(|root| root.clone())
        }
        #[cfg(not(test))]
        {
            Some(Self::resolve_laya_python().0).filter(|root| !root.as_os_str().is_empty())
        }
    }

    /// 开始自动准备 Laya 运行环境；本机没有可用 Python 时返回 412 并说明原因。
    pub(super) fn start_laya_setup(&self) -> Result<(), CenterError> {
        if self.laya_setup.running() {
            return Ok(());
        }
        let root = self.laya_env_root().ok_or_else(|| {
                CenterError::new(
                    "laya_env_missing",
                    412,
                    "已通过 MOSS_LAYA_PYTHON 指定外部 Python，MOSS 不会改动该环境；请在其中自行执行 pip install \"laya[serve]\"",
                )
            })?;
        self.laya_setup
            .find_python()
            .map_err(|message| CenterError::new("laya_env_missing", 412, message))?;
        let mirror = self
            .settings()
            .builtin_local
            .mirror
            .unwrap_or_default()
            .to_ascii_lowercase();
        let prefer_mirror = [
            "mirror",
            "ghfast",
            "tuna",
            "aliyun",
            "ustc",
            "huaweicloud",
            "tencent",
        ]
        .iter()
        .any(|hint| mirror.contains(hint));
        self.laya_setup
            .start(moss_voice::laya_env::SetupRequest {
                root,
                prefer_mirror,
            })
            .map_err(|message| CenterError::new("laya_setup_failed", 500, message))
    }

    /// `voice_local_laya_setup`：`start` 开始准备、`cancel` 取消。
    pub fn laya_setup_action(&self, action: &str) -> Result<Value, CenterError> {
        match action {
            "start" => self.start_laya_setup()?,
            "cancel" => self.laya_setup.cancel(),
            _ => {
                return Err(CenterError::new(
                    "voice_settings_invalid",
                    400,
                    "action 只能是 start 或 cancel",
                ));
            }
        }
        Ok(self.status())
    }

    /// 读取 Jev 可用密钥：优先本机 `jev_api_key`，未设时若大模型指向 `openrouter.ai` 则沿用大模型密钥。
    pub(super) fn jev_effective_secret(&self) -> Result<Option<String>, CenterError> {
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        resolve_openrouter_jev_secret_locked(&db, &self.settings_file()).map(|(secret, _)| secret)
    }

    pub(super) fn stop_laya(&self) {
        if let Ok(mut runner) = self.laya.lock() {
            runner.stop();
        }
    }

    pub(super) fn stop_laya_and_wait(&self) {
        self.stop_laya();
        if let Ok(_guard) = self.jev_start_lock.lock() {
            self.stop_laya();
            if let Ok(mut r) = self.laya.lock() {
                r.cancelling_model = None;
            }
        }
    }

    fn is_local_laya_configured_and_installed(&self, settings: &moss_voice::VoiceSettings) -> bool {
        let jev_model = moss_voice::settings::validate_jev_model(&settings.builtin_local.jev_model)
            .unwrap_or_else(|_| moss_voice::settings::default_jev_model());
        moss_voice::settings::canonical_local_jev_model(&jev_model)
            .is_some_and(|id| self.models.path_if_installed(id).is_some())
    }

    pub(super) fn should_run_local_laya_for_feedback(&self) -> bool {
        let settings = self.settings();
        self.is_local_laya_configured_and_installed(&settings)
            && moss_voice::settings::jev_feedback_enabled(&self.settings_file())
    }

    pub(super) fn should_run_local_laya(&self) -> bool {
        let settings = self.settings();
        self.is_local_laya_configured_and_installed(&settings)
            && (settings.builtin_local.jev_filter_enabled
                || moss_voice::settings::jev_feedback_enabled(&self.settings_file()))
    }

    pub(super) fn select_laya_device(
        cuda_available: bool,
        mps_available: bool,
        env_override: Option<&str>,
    ) -> String {
        if let Some(raw) = env_override.map(str::trim).filter(|v| !v.is_empty()) {
            return raw.to_owned();
        }
        if cuda_available {
            "cuda:0".to_owned()
        } else if mps_available {
            "mps".to_owned()
        } else {
            "cpu".to_owned()
        }
    }

    fn probe_python_flag(py: &std::path::Path, snippet: &str) -> bool {
        if !py.is_file() {
            return false;
        }
        let mut cmd = std::process::Command::new(py);
        cmd.args(["-c", snippet])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let Ok(mut child) = cmd.spawn() else {
            return false;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return false;
                    }
                    let mut out = String::new();
                    if let Some(mut stdout) = child.stdout.take() {
                        use std::io::Read;
                        let _ = stdout.read_to_string(&mut out);
                    }
                    return out.trim() == "1";
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return false;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(40));
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }

    pub(super) fn probe_laya_cuda_available(py: &std::path::Path, cuda_lib_present: bool) -> bool {
        if !cuda_lib_present {
            return false;
        }
        Self::probe_python_flag(
            py,
            "import sys, torch; sys.stdout.write('1' if torch.cuda.is_available() else '0')",
        )
    }

    pub(super) fn probe_laya_mps_available(py: &std::path::Path, is_apple_silicon: bool) -> bool {
        if !is_apple_silicon {
            return false;
        }
        Self::probe_python_flag(
            py,
            "import sys, torch; b = getattr(torch.backends, 'mps', None); sys.stdout.write('1' if b is not None and b.is_available() else '0')",
        )
    }

    fn laya_venv_python(root: &std::path::Path) -> std::path::PathBuf {
        #[cfg(windows)]
        {
            root.join(".venv").join("Scripts").join("python.exe")
        }
        #[cfg(not(windows))]
        {
            let py = root.join(".venv").join("bin").join("python");
            if py.is_file() {
                py
            } else {
                let py3 = root.join(".venv").join("bin").join("python3");
                if py3.is_file() { py3 } else { py }
            }
        }
    }

    pub(super) fn resolve_laya_python() -> (std::path::PathBuf, std::path::PathBuf) {
        let install_dir = writable_install_dir();
        let env_py = moss_laya_python_env();
        let laya_root = moss_laya_root_env()
            .or_else(|| {
                install_dir.as_ref().and_then(|dir| {
                    [dir.join("laya"), dir.join("moss-laya")]
                        .into_iter()
                        .find(|cand| Self::laya_venv_python(cand).is_file())
                })
            })
            .or_else(|| {
                env_py
                    .as_ref()
                    .and_then(|p| p.parent())
                    .and_then(|bin| bin.parent())
                    .and_then(|venv| venv.parent().map(std::path::Path::to_path_buf))
            })
            .unwrap_or_else(|| {
                if let Some(dir) = install_dir {
                    dir.join("laya")
                } else {
                    let home = std::env::var_os("HOME")
                        .or_else(|| std::env::var_os("USERPROFILE"))
                        .map(std::path::PathBuf::from)
                        .filter(|p| p.is_absolute());
                    let local_appdata = std::env::var_os("LOCALAPPDATA")
                        .map(std::path::PathBuf::from)
                        .filter(|p| p.is_absolute());
                    let mut cands = Vec::new();
                    if let Some(h) = &home {
                        cands.push(h.join("moss-laya"));
                        cands.push(h.join(".moss").join("laya"));
                    }
                    if let Some(appdata) = &local_appdata {
                        cands.push(appdata.join("moss").join("laya"));
                    }
                    cands
                        .iter()
                        .find(|cand| Self::laya_venv_python(cand).is_file())
                        .cloned()
                        .or_else(|| home.map(|h| h.join("moss-laya")))
                        .or_else(|| local_appdata.map(|a| a.join("moss").join("laya")))
                        .unwrap_or_default()
                }
            });
        let py = env_py.unwrap_or_else(|| {
            if laya_root.as_os_str().is_empty() {
                std::path::PathBuf::new()
            } else {
                Self::laya_venv_python(&laya_root)
            }
        });
        (laya_root, py)
    }

    pub(super) fn detect_laya_env() -> Value {
        let skip_accel_probe = std::env::var("MOSS_LAYA_DEVICE")
            .or_else(|_| std::env::var("MOSS_JEV_DEVICE"))
            .ok()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("cpu"));
        Self::detect_laya_env_with_probe(!skip_accel_probe)
    }

    pub(super) fn detect_laya_env_with_probe(probe_accel: bool) -> Value {
        let (_laya_root, py) = Self::resolve_laya_python();
        let force_cpu = std::env::var("MOSS_LAYA_DEVICE")
            .or_else(|_| std::env::var("MOSS_JEV_DEVICE"))
            .ok()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("cpu"));
        let python_found = py.is_file();
        let site_packages = py.parent().and_then(|s| s.parent()).and_then(|venv| {
            let win_sp = venv.join("Lib").join("site-packages");
            if win_sp.is_dir() {
                return Some(win_sp);
            }
            let lib_dir = venv.join("lib");
            std::fs::read_dir(&lib_dir)
                .ok()?
                .flatten()
                .find_map(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with("python")
                        .then(|| entry.path().join("site-packages"))
                        .filter(|p| p.is_dir())
                })
        });
        let laya_serve_found = site_packages.as_ref().is_some_and(|sp| {
            sp.join("laya").join("serve.py").is_file() && sp.join("uvicorn").is_dir()
        });
        let torch_found = site_packages
            .as_ref()
            .is_some_and(|sp| sp.join("torch").is_dir());
        let cuda_lib_found = site_packages.as_ref().is_some_and(|sp| {
            let torch_lib = sp.join("torch").join("lib");
            torch_lib.join("torch_cuda.dll").is_file()
                || torch_lib.join("libtorch_cuda.so").is_file()
        });
        let ready = python_found && laya_serve_found;
        // 虚拟环境由 MOSS 自动创建（删除 Laya 时一并删除）
        let venv_dir = py
            .parent()
            .and_then(|bin| bin.parent())
            .map(std::path::Path::to_path_buf);
        let managed = venv_dir
            .as_deref()
            .is_some_and(moss_voice::laya_env::is_managed);
        let is_apple_silicon = cfg!(all(target_os = "macos", target_arch = "aarch64"));
        let cuda_found = if !ready || force_cpu {
            false
        } else if probe_accel {
            Self::probe_laya_cuda_available(&py, cuda_lib_found)
        } else {
            cuda_lib_found
        };
        let mps_found = if !ready || force_cpu || cuda_found {
            false
        } else if probe_accel {
            Self::probe_laya_mps_available(&py, is_apple_silicon && torch_found)
        } else {
            is_apple_silicon && torch_found
        };
        let accelerator = if cuda_found {
            "cuda"
        } else if mps_found {
            "mps"
        } else {
            "cpu"
        };
        let summary = if ready && cuda_found {
            "运行环境检测通过（Python + laya[serve] + NVIDIA CUDA 显卡加速已就绪），可直接下载并启用。".to_owned()
        } else if ready && mps_found {
            "运行环境检测通过（Python + laya[serve] + Apple Metal MPS 加速已就绪），可直接下载并启用。".to_owned()
        } else if ready {
            "运行环境检测通过（Python + laya[serve] CPU 模式已就绪），可直接下载并启用。".to_owned()
        } else if !python_found {
            "尚未准备 Laya 运行环境：点“下载”或“准备运行环境”后，MOSS 会用本机 Python 3.10+ 自动创建虚拟环境并安装 laya[serve]（有 NVIDIA 显卡时装 CUDA 版 PyTorch）；也可直接选用上方免 Python 的 OneJev 1.6。".to_owned()
        } else {
            "已找到 Python 虚拟环境，但缺少 laya[serve] 依赖包：点“准备运行环境”自动补齐。"
                .to_owned()
        };
        serde_json::json!({
            "ready": ready,
            "python_found": python_found,
            "laya_serve_found": laya_serve_found,
            "cuda_found": cuda_found,
            "mps_found": mps_found,
            "accelerator": accelerator,
            "managed": managed,
            "venv_dir": venv_dir.map(|dir| dir.to_string_lossy().into_owned()),
            "summary": summary,
        })
    }

    fn local_jev_missing_message(canonical_id: &str) -> String {
        let label = local_models::spec(canonical_id)
            .map(|s| s.label)
            .unwrap_or(canonical_id);
        format!(
            "本地决策模型“{label}”（{canonical_id}）尚未下载：请先在下方“本地模型（断网可用）”点击下载"
        )
    }

    pub(super) fn ensure_laya_running(&self, wait_ready: bool) -> Result<(), String> {
        let settings = self.settings();
        let validated = moss_voice::settings::validate_jev_model(&settings.builtin_local.jev_model)
            .unwrap_or_else(|_| moss_voice::settings::default_jev_model());
        let Some(canonical_id) = moss_voice::settings::canonical_local_jev_model(&validated) else {
            return Ok(());
        };
        let result = self.ensure_local_jev_running_for(canonical_id, wait_ready);
        // 启动失败的原因留给状态接口显示（打包后的程序看不到控制台输出）；主动取消不算失败
        if let Err(err) = &result
            && !err.contains("已取消")
            && let Ok(mut runner) = self.laya.lock()
        {
            runner.last_failure = Some((canonical_id.to_owned(), err.clone()));
        }
        result
    }

    /// 本地 OneJev 是否走显卡加速：推理进程已在运行时以实际探测为准，否则按推理引擎文件预估。
    pub(super) fn onejev_uses_gpu(&self, canonical_id: &str) -> bool {
        if let Ok(runner) = self.laya.lock()
            && runner.active_model.as_deref() == Some(canonical_id)
            && let Some(gw) = runner.onejev.as_ref()
        {
            return gw.accelerator().kind.uses_gpu();
        }
        moss_voice::onejev::expected_gpu(self.models.root_for(canonical_id))
    }

    /// 当前所选本地 Jev 的实际运行情况（2026-11-07）：未下载 / 未启用 / 加载中 / 已就绪 / 启动失败 / 已停止。
    /// 后台启动失败或推理进程意外退出时带上原因，界面据此提示，不再“看着开着、实际每轮都跳过”。
    pub(super) fn local_jev_runtime_status(
        &self,
        settings: &moss_voice::VoiceSettings,
        feedback_enabled: bool,
    ) -> Value {
        let validated = moss_voice::settings::validate_jev_model(&settings.builtin_local.jev_model)
            .unwrap_or_else(|_| moss_voice::settings::default_jev_model());
        let Some(id) = moss_voice::settings::canonical_local_jev_model(&validated) else {
            return serde_json::json!({ "state": "cloud" });
        };
        let is_onejev = moss_voice::settings::is_local_onejev_model(id);
        let installed = self.models.path_if_installed(id).is_some();
        let wanted = settings.builtin_local.jev_filter_enabled || feedback_enabled;
        let (alive, starting, crashed, accelerator, crash_log, failure) = match self.laya.lock() {
            Ok(mut runner) => {
                let alive = runner.is_alive_for(id);
                let starting = runner.is_starting_for(id);
                let crashed = !alive && runner.active_model.as_deref() == Some(id);
                let gw = runner
                    .onejev
                    .as_ref()
                    .filter(|_| runner.active_model.as_deref() == Some(id));
                let accelerator = gw.map(|gw| {
                    (
                        gw.accelerator().display_text(),
                        gw.accelerator().kind.uses_gpu(),
                    )
                });
                let crash_log = gw.map(|gw| gw.log_summary()).unwrap_or_default();
                let failure = runner
                    .last_failure
                    .as_ref()
                    .filter(|(model, _)| model == id)
                    .map(|(_, message)| message.clone());
                (alive, starting, crashed, accelerator, crash_log, failure)
            }
            Err(_) => (false, false, false, None, String::new(), None),
        };
        // Laya 子进程起来后还要加载权重，能答 /health 才算就绪
        let answering = alive
            && (is_onejev
                || probe_local_jev_health_model(
                    std::net::SocketAddr::from(([127, 0, 0, 1], 18090)),
                    std::time::Duration::from_millis(120),
                )
                .as_deref()
                    == Some(id));
        let (state, error) = if !installed {
            ("not_installed", None)
        } else if answering {
            ("ready", None)
        } else if starting || alive {
            ("starting", None)
        } else if crashed {
            let hint = if crash_log.is_empty() {
                String::new()
            } else {
                format!("（{crash_log}）")
            };
            (
                "failed",
                Some(format!(
                    "本地推理进程已意外退出{hint}；关闭再打开 Jev 开关或重启语音服务即可重新拉起"
                )),
            )
        } else if let Some(message) = failure {
            ("failed", Some(message))
        } else if !wanted {
            ("idle", None)
        } else {
            ("stopped", None)
        };
        let notice = if is_onejev {
            let gpu = accelerator
                .as_ref()
                .map(|(_, gpu)| *gpu)
                .unwrap_or_else(|| moss_voice::onejev::expected_gpu(self.models.root_for(id)));
            moss_voice::onejev::local_jev_budget(id, gpu).and_then(|budget| budget.notice)
        } else {
            None
        };
        serde_json::json!({
            "model": id,
            "state": state,
            "accelerator": accelerator.map(|(label, _)| label),
            "error": error,
            "notice": notice,
        })
    }

    pub(super) fn ensure_local_jev_running_for(
        &self,
        canonical_id: &str,
        wait_ready: bool,
    ) -> Result<(), String> {
        let Some(model_dir) = self.models.path_if_installed(canonical_id) else {
            return Err(Self::local_jev_missing_message(canonical_id));
        };
        if cfg!(test) {
            return Ok(());
        }
        {
            let mut runner = self
                .laya
                .lock()
                .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
            if !wait_ready
                && (runner.is_alive_for(canonical_id) || runner.is_starting_for(canonical_id))
            {
                return Ok(());
            }
            if runner
                .starting_model
                .as_deref()
                .is_some_and(|m| m != canonical_id)
            {
                runner.cancel_in_flight();
            }
        }
        let start_guard = self
            .jev_start_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 18090));
        let (already_alive, start_generation) = {
            let mut runner = self
                .laya
                .lock()
                .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
            let alive = runner.is_alive_for(canonical_id);
            if !alive {
                runner.cancel_in_flight();
                runner.kill_reaped();
            }
            (alive, runner.generation)
        };

        if let Some(serving_model) =
            probe_local_jev_health_model(addr, std::time::Duration::from_millis(120))
        {
            if serving_model == canonical_id {
                return Ok(());
            }
            let release_deadline =
                std::time::Instant::now() + std::time::Duration::from_millis(1500);
            let mut freed = false;
            while std::time::Instant::now() < release_deadline {
                if probe_local_jev_health_model(addr, std::time::Duration::from_millis(100))
                    .is_none()
                {
                    freed = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if !freed {
                return Err(format!(
                    "本地端口 127.0.0.1:18090 已被外部 {serving_model} 服务占用，无法切换到 {canonical_id}；请先关闭外部 18090 进程"
                ));
            }
        }

        if canonical_id == moss_voice::settings::LOCAL_LAYA_JEV_MODEL {
            if !already_alive {
                let (laya_root, py) = Self::resolve_laya_python();
                let env_device = std::env::var("MOSS_LAYA_DEVICE")
                    .or_else(|_| std::env::var("MOSS_JEV_DEVICE"))
                    .ok();
                let has_override = env_device.as_deref().is_some_and(|v| !v.trim().is_empty());
                let env_info = Self::detect_laya_env_with_probe(!has_override);
                if env_info.get("ready").and_then(Value::as_bool) != Some(true) {
                    return Err(env_info
                        .get("summary")
                        .and_then(Value::as_str)
                        .unwrap_or("未找到本地 Laya 运行环境")
                        .to_owned());
                }
                let cuda_available =
                    env_info.get("cuda_found").and_then(Value::as_bool) == Some(true);
                let mps_available =
                    env_info.get("mps_found").and_then(Value::as_bool) == Some(true);
                let laya_device =
                    Self::select_laya_device(cuda_available, mps_available, env_device.as_deref());
                let tmp_dir = laya_root.join("tmp");
                let tmp_existed = tmp_dir.exists();
                let _ = std::fs::create_dir_all(&tmp_dir);
                if !tmp_existed {
                    let _ = std::fs::write(tmp_dir.join(moss_voice::laya_env::MANAGED_MARKER), b"");
                }
                let extra_models = serde_json::json!({
                    "multilingual": model_dir.to_string_lossy().replace('\\', "/")
                })
                .to_string();
                let mut runner = self
                    .laya
                    .lock()
                    .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
                if runner.generation != start_generation {
                    return Err("本地 Laya 启动已取消（开关已关闭或服务已停止）".into());
                }
                if !runner.is_alive_for(canonical_id)
                    && probe_local_jev_health_model(addr, std::time::Duration::from_millis(120))
                        .as_deref()
                        != Some(canonical_id)
                {
                    let mut cmd = std::process::Command::new(&py);
                    cmd.args(["-m", "laya.serve"])
                        .env("USE_TF", "0")
                        .env("HF_HUB_OFFLINE", "1")
                        .env("TMP", &tmp_dir)
                        .env("TEMP", &tmp_dir)
                        .env("LAYA_HOST", "127.0.0.1")
                        .env("LAYA_PORT", "18090")
                        .env("LAYA_DEVICE", &laya_device)
                        .env("LAYA_PRELOAD", "1")
                        .env("LAYA_MODELS", "multilingual")
                        .env("LAYA_DEFAULT_MODEL", "multilingual")
                        .env("LAYA_MAX_LOADED", "1")
                        .env("LAYA_IDLE_UNLOAD_SECONDS", "0")
                        .env("LAYA_MAX_TOKEN_BUDGET", "4096")
                        .env("LAYA_EXTRA_MODELS", extra_models)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null());
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                        cmd.creation_flags(CREATE_NO_WINDOW);
                    }
                    let child = cmd
                        .spawn()
                        .map_err(|e| format!("启动本地 Laya 服务失败：{e}"))?;
                    runner.job = moss_voice::job::JobHandle::for_child(&child);
                    runner.child = Some(child);
                    runner.active_model = Some(canonical_id.to_owned());
                    runner.last_failure = None;
                }
            }
            if wait_ready {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(28);
                while std::time::Instant::now() < deadline {
                    if self
                        .laya
                        .lock()
                        .map(|r| r.generation != start_generation)
                        .unwrap_or(false)
                    {
                        return Err("本地 Laya 启动已取消（开关已关闭或服务已停止）".into());
                    }
                    if probe_local_jev_health_model(addr, std::time::Duration::from_millis(200))
                        .as_deref()
                        == Some(canonical_id)
                    {
                        return Ok(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                return Err("本地 Laya 决策服务启动超时（正在加载模型，请稍后再试）".into());
            }
            return Ok(());
        }

        let models_root = self.models.root_for(canonical_id).to_path_buf();
        if moss_voice::onejev::resolve_llama_server(&models_root).is_none()
            && !moss_voice::onejev::has_platform_runtime_asset()
        {
            return Err(
                    "当前系统架构暂无内置预编译包，请安装 llama.cpp 并设置 MOSS_LLAMA_SERVER 指向 llama-server。".into(),
                );
        }

        if !already_alive {
            let mirror_hint = self.settings().builtin_local.mirror;
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let mut runner = self
                    .laya
                    .lock()
                    .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
                if runner.generation != start_generation {
                    return Err("本地 OneJev 启动已取消（开关已关闭或服务已停止）".into());
                }
                runner.starting_model = Some(canonical_id.to_owned());
                runner.starting_cancel = Some(cancel.clone());
                runner.last_failure = None;
            }

            if !wait_ready {
                let start_lock = self.jev_start_lock.clone();
                let laya_slot = self.laya.clone();
                let diagnostics = self.diagnostics.clone();
                let id_owned = canonical_id.to_owned();
                drop(start_guard);
                let _ = std::thread::Builder::new()
                        .name(format!("onejev-start-{canonical_id}"))
                        .spawn(move || {
                            let _bg_guard = start_lock
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                                if let Ok(mut r) = laya_slot.lock() {
                                    if r.generation == start_generation {
                                        r.starting_model = None;
                                        r.starting_cancel = None;
                                    }
                                    if r.cancelling_model.as_deref() == Some(id_owned.as_str()) {
                                        r.cancelling_model = None;
                                    }
                                }
                                return;
                            }
                            match moss_voice::onejev::OneJevGateway::start_with_cancel(
                                &id_owned,
                                &model_dir,
                                &models_root,
                                addr,
                                mirror_hint.as_deref(),
                                &cancel,
                            ) {
                                Ok(mut gw) => {
                                    if let Ok(mut r) = laya_slot.lock() {
                                        if r.cancelling_model.as_deref() == Some(id_owned.as_str())
                                        {
                                            r.cancelling_model = None;
                                        }
                                        if r.generation != start_generation
                                            || cancel.load(std::sync::atomic::Ordering::SeqCst)
                                        {
                                            gw.stop();
                                        } else {
                                            r.starting_model = None;
                                            r.starting_cancel = None;
                                            r.onejev = Some(gw);
                                            r.active_model = Some(id_owned);
                                            r.last_failure = None;
                                        }
                                    } else {
                                        gw.stop();
                                    }
                                }
                                Err(err) => {
                                    if let Ok(mut r) = laya_slot.lock() {
                                        if r.generation == start_generation {
                                            r.starting_model = None;
                                            r.starting_cancel = None;
                                        }
                                        if r.cancelling_model.as_deref() == Some(id_owned.as_str())
                                        {
                                            r.cancelling_model = None;
                                        }
                                    }
                                    if !cancel.load(std::sync::atomic::Ordering::SeqCst) {
                                        eprintln!("[moss-desktop] 本地 OneJev ({id_owned}) 后台启动失败：{err}");
                                        diagnostics.push_from(moss_voice_server::DiagSource::Moss, local_jev_failure(&format!("本地 OneJev（{id_owned}）后台启动失败"), &err));
                                        if let Ok(mut r) = laya_slot.lock()
                                            && r.generation == start_generation
                                        {
                                            r.last_failure = Some((id_owned.clone(), err));
                                        }
                                    }
                                }
                            }
                        });
                return Ok(());
            }

            let start_res = moss_voice::onejev::OneJevGateway::start_with_cancel(
                canonical_id,
                &model_dir,
                &models_root,
                addr,
                mirror_hint.as_deref(),
                &cancel,
            );
            let mut runner = self
                .laya
                .lock()
                .map_err(|_| "本地 Jev 进程锁失败".to_owned())?;
            if runner.generation == start_generation {
                runner.starting_model = None;
                runner.starting_cancel = None;
            }
            if runner.cancelling_model.as_deref() == Some(canonical_id) {
                runner.cancelling_model = None;
            }
            let mut gateway = start_res?;
            if runner.generation != start_generation
                || cancel.load(std::sync::atomic::Ordering::SeqCst)
            {
                gateway.stop();
                return Err("本地 OneJev 启动已取消（开关已关闭或服务已停止）".into());
            }
            runner.onejev = Some(gateway);
            runner.active_model = Some(canonical_id.to_owned());
            runner.last_failure = None;
        }
        if wait_ready
            && probe_local_jev_health_model(addr, std::time::Duration::from_millis(300)).as_deref()
                != Some(canonical_id)
        {
            return Err("本地 OneJev 网关未就绪".into());
        }
        Ok(())
    }

    pub fn jev_test(&self, input: &Value) -> Result<Value, CenterError> {
        let current_model = self.settings().builtin_local.jev_model;
        let model = input
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or(current_model.as_str());
        let model = moss_voice::settings::validate_jev_model(model)
            .map_err(|msg| CenterError::new("model_input_invalid", 400, msg))?;
        if let Some(local_id) = moss_voice::settings::canonical_local_jev_model(&model) {
            self.ensure_local_jev_running_for(local_id, true)
                .map_err(|msg| CenterError::new("model_input_invalid", 400, msg))?;
            return Ok(moss_voice::models::test_jev_decisions(
                moss_voice::models::DEFAULT_LOCAL_LAYA_DECISIONS_URL,
                "moss-local-jev",
                local_id,
            ));
        }
        let typed_key = input
            .get("api_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let api_key = if !typed_key.is_empty() {
            typed_key.to_owned()
        } else {
            self.jev_effective_secret()?.ok_or_else(|| {
                CenterError::new("model_input_invalid", 400, "请填写 OpenRouter API Key")
            })?
        };
        Ok(moss_voice::models::test_jev_decisions(
            moss_voice::models::DEFAULT_JEV_DECISIONS_URL,
            &api_key,
            &model,
        ))
    }

    pub fn jev_models_list(&self, input: &Value) -> Result<Value, CenterError> {
        let typed_key = input
            .get("api_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_owned);
        let effective = match typed_key {
            Some(k) => Some(k),
            None => self.jev_effective_secret()?,
        };
        Ok(moss_voice::models::list_jev_models(
            moss_voice::models::DEFAULT_OPENROUTER_MODELS_URL,
            effective.as_deref(),
        ))
    }

    /// 组合保存 `models_save` + `jev_save` 前预校验 Jev 输入，防止普通模型落盘后因 Jev 参数格式错误造成半提交。
    pub fn precheck_jev_save_input(
        &self,
        input: Option<&Value>,
        clear: bool,
        pending_models: &ModelsInput,
        clearing_sections: &[Section],
    ) -> Result<(), CenterError> {
        if clear {
            return Ok(());
        }
        let invalid = |msg: &str| CenterError::new("model_input_invalid", 400, msg);
        let obj = input
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("Jev 配置格式不正确"))?;
        let current_settings = self.settings();
        let model_raw = obj
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or(current_settings.builtin_local.jev_model.as_str());
        let model =
            moss_voice::settings::validate_jev_model(model_raw).map_err(|msg| invalid(&msg))?;
        let next_voice = match obj.get("voice_enabled") {
            Some(v) => v
                .as_bool()
                .ok_or_else(|| invalid("语音筛选开关取值不正确"))?,
            None => current_settings.builtin_local.jev_filter_enabled,
        };
        let effective_feedback = match obj.get("feedback_enabled") {
            Some(v) => v
                .as_bool()
                .ok_or_else(|| invalid("加工反馈开关取值不正确"))?,
            None => moss_voice::settings::jev_feedback_enabled(&self.settings_file()),
        };
        if let Some(local_id) = moss_voice::settings::canonical_local_jev_model(&model) {
            if (next_voice || effective_feedback)
                && self.models.path_if_installed(local_id).is_none()
            {
                return Err(invalid(&Self::local_jev_missing_message(local_id)));
            }
            return Ok(());
        }
        let typed_key = obj
            .get("api_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !typed_key.is_empty() {
            return Ok(());
        }
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        if db
            .get_secret("jev_api_key")
            .map_err(db_failure)?
            .is_some_and(|v| !v.trim().is_empty())
        {
            return Ok(());
        }
        let plan = builtin::plan_save(
            &current_settings.builtin_models,
            pending_models,
            clearing_sections,
        )
        .map_err(|message| invalid(&message))?;
        let next_openrouter_llm = plan.models.llm.as_ref().is_some_and(|p| {
            p.kind == ProviderKind::Openai && moss_voice::models::is_openrouter_https_url(&p.url)
        });
        let llm_has_key = plan
            .keys
            .iter()
            .find(|(sec, _)| *sec == Section::Llm)
            .map(|(_, ch)| match ch {
                KeyChange::Set(v) => !v.trim().is_empty(),
                KeyChange::Clear => false,
                KeyChange::Keep => db
                    .get_secret(builtin_secret_name(Section::Llm))
                    .ok()
                    .flatten()
                    .is_some_and(|v| !v.trim().is_empty()),
            })
            .unwrap_or(false);
        if next_openrouter_llm && llm_has_key {
            Ok(())
        } else {
            Err(invalid("请填写 OpenRouter API Key"))
        }
    }

    pub fn jev_save(&self, input: Option<&Value>, clear: bool) -> Result<Value, CenterError> {
        let invalid = |msg: &str| CenterError::new("model_input_invalid", 400, msg);
        enum LayaAction {
            Start,
            Stop,
        }
        let laya_action = {
            let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
            let snapshot = self.settings();
            let mut changes: Vec<(String, Option<String>)> = Vec::new();
            let next_model: String;
            let next_voice: bool;
            let feedback_patch: Option<bool>;
            let action: LayaAction;
            if clear {
                changes.push(("jev_api_key".into(), None));
                next_voice = false;
                next_model = moss_voice::settings::default_jev_model();
                feedback_patch = Some(false);
                action = LayaAction::Stop;
            } else {
                let obj = input
                    .and_then(Value::as_object)
                    .ok_or_else(|| invalid("Jev 配置格式不正确"))?;
                let model_raw = obj
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .unwrap_or(snapshot.builtin_local.jev_model.as_str())
                    .to_owned();
                let model = moss_voice::settings::validate_jev_model(&model_raw)
                    .map_err(|msg| invalid(&msg))?;
                let local_id = moss_voice::settings::canonical_local_jev_model(&model);
                let typed_key = obj
                    .get("api_key")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                next_voice = match obj.get("voice_enabled") {
                    Some(v) => v
                        .as_bool()
                        .ok_or_else(|| invalid("语音筛选开关取值不正确"))?,
                    None => snapshot.builtin_local.jev_filter_enabled,
                };
                feedback_patch = match obj.get("feedback_enabled") {
                    Some(v) => Some(
                        v.as_bool()
                            .ok_or_else(|| invalid("加工反馈开关取值不正确"))?,
                    ),
                    None => None,
                };
                let effective_feedback = feedback_patch.unwrap_or_else(|| {
                    moss_voice::settings::jev_feedback_enabled(&self.settings_file())
                });
                if let Some(lid) = local_id {
                    if (next_voice || effective_feedback)
                        && self.models.path_if_installed(lid).is_none()
                    {
                        return Err(invalid(&Self::local_jev_missing_message(lid)));
                    }
                    if !typed_key.is_empty() {
                        changes.push(("jev_api_key".into(), Some(typed_key.to_owned())));
                    }
                } else if !typed_key.is_empty() {
                    changes.push(("jev_api_key".into(), Some(typed_key.to_owned())));
                } else {
                    let (existing, has_own) =
                        resolve_openrouter_jev_secret_locked(&db, &self.settings_file())?;
                    let Some(resolved_key) = existing else {
                        return Err(invalid("请填写 OpenRouter API Key"));
                    };
                    if !has_own {
                        changes.push(("jev_api_key".into(), Some(resolved_key)));
                    }
                }
                next_model = model;
                action = if local_id.is_some() && (next_voice || effective_feedback) {
                    LayaAction::Start
                } else {
                    LayaAction::Stop
                };
            }
            commit_secrets_then_settings(&db, &changes, || {
                moss_voice::settings::update_voice_with_jev_feedback(
                    &self.settings_file(),
                    |settings| {
                        settings.builtin_local.jev_model = next_model.clone();
                        settings.builtin_local.jev_filter_enabled = next_voice;
                        Ok(((), feedback_patch))
                    },
                )
                .map_err(|message| CenterError::new("voice_settings_write_failed", 500, message))
            })?;
            action
        };
        match laya_action {
            LayaAction::Start => {
                if let Err(err) = self.ensure_laya_running(false) {
                    eprintln!("[moss-desktop] 本地 Jev 决策服务启动失败：{err}");
                    self.diagnostics.push_from(
                        moss_voice_server::DiagSource::Moss,
                        local_jev_failure("切换 Jev 设置后本地 Jev 决策服务启动失败", &err),
                    );
                }
            }
            LayaAction::Stop => self.stop_laya(),
        }
        let restarted = self.restart_if_running()?;
        let mut status = self.status();
        status["restarted"] = Value::Bool(restarted);
        Ok(status)
    }
}
