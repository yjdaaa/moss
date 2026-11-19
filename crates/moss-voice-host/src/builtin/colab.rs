//! Colab CLI 状态、登录检测与界面操作分派。

use super::*;

#[derive(Default)]
pub(super) struct ColabCliState {
    account_hint: String,
    expires_in: String,
    whoami_probed: bool,
    whoami_probing: bool,
    auth_url: Option<String>,
    pub(super) runner_state: String,
    pub(super) runner_step: String,
    pub(super) runner_error: Option<String>,
    pub(super) generation: u64,
    pub(super) active_pid: Option<u32>,
    pub(super) ref_sync_gen: u64,
    pub(super) synced_ref_gen: u64,
    pub(super) ref_sync_pid: Option<u32>,
}

impl BuiltinVoice {
    pub(super) fn colab_secret(&self) -> Result<Option<String>, CenterError> {
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        Ok(db
            .get_secret(moss_voice::colab::COLAB_SECRET_NAME)
            .map_err(db_failure)?
            .filter(|s| !s.trim().is_empty()))
    }

    /// n80：Colab T4 专属联动已开启、地址合法且密钥已保存时返回规范化后的配置与密钥；未开启时零开销返回 `None`。
    pub(super) fn active_colab(
        &self,
        settings: &moss_voice::VoiceSettings,
    ) -> Result<Option<(moss_voice::colab::BuiltinColab, String)>, CenterError> {
        if !settings.builtin_colab.enabled {
            return Ok(None);
        }
        let Some(key) = self.colab_secret()? else {
            return Ok(None);
        };
        if !settings.builtin_colab.active(true) {
            return Ok(None);
        }
        let Ok(url) = moss_voice::colab::normalize_colab_url(&settings.builtin_colab.url) else {
            return Ok(None);
        };
        let mut normalized = settings.builtin_colab.clone();
        normalized.url = url;
        Ok(Some((normalized, key)))
    }

    pub(super) fn find_colab_cli_binary(&self) -> Option<std::path::PathBuf> {
        #[cfg(test)]
        {
            if self
                .colab_cli_installed_for_test
                .load(std::sync::atomic::Ordering::Acquire)
            {
                Some(std::path::PathBuf::from("colab"))
            } else {
                None
            }
        }
        #[cfg(not(test))]
        {
            let bin_name = if cfg!(windows) { "colab.exe" } else { "colab" };
            if let Some(path_os) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path_os) {
                    let cand = dir.join(bin_name);
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
            let home_opt = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(std::path::PathBuf::from);
            if let Some(ref home) = home_opt {
                let local_bin = home.join(".local").join("bin").join(bin_name);
                if local_bin.is_file() {
                    return Some(local_bin);
                }
                #[cfg(target_os = "macos")]
                if let Ok(entries) = std::fs::read_dir(home.join("Library").join("Python")) {
                    for entry in entries.flatten() {
                        let cand = entry.path().join("bin").join(bin_name);
                        if cand.is_file() {
                            return Some(cand);
                        }
                    }
                }
            }
            if let Some(data) = std::env::var_os("APPDATA").map(std::path::PathBuf::from) {
                let uv_bin = if cfg!(windows) {
                    data.join("uv")
                        .join("tools")
                        .join("google-colab-cli")
                        .join("Scripts")
                        .join("colab.exe")
                } else {
                    data.join("uv")
                        .join("tools")
                        .join("google-colab-cli")
                        .join("bin")
                        .join("colab")
                };
                if uv_bin.is_file() {
                    return Some(uv_bin);
                }
                #[cfg(windows)]
                if let Ok(entries) = std::fs::read_dir(data.join("Python")) {
                    for entry in entries.flatten() {
                        let cand = entry.path().join("Scripts").join("colab.exe");
                        if cand.is_file() {
                            return Some(cand);
                        }
                    }
                }
            }
            None
        }
    }

    pub(super) fn colab_ref_audio_path(&self) -> std::path::PathBuf {
        self.host
            .runtime_root
            .path
            .join("colab-cli")
            .join("moss_ref.wav")
    }

    fn colab_ref_audio_meta_path(&self) -> std::path::PathBuf {
        self.host
            .runtime_root
            .path
            .join("colab-cli")
            .join("moss_ref.meta")
    }

    fn colab_cli_token_path(&self) -> Option<std::path::PathBuf> {
        #[cfg(test)]
        {
            Some(
                self.host
                    .runtime_root
                    .path
                    .join("colab-cli")
                    .join("token.json"),
            )
        }
        #[cfg(not(test))]
        {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(std::path::PathBuf::from)
                .map(|h| h.join(".config").join("colab-cli").join("token.json"))
        }
    }

    fn colab_token_file_valid(path: &std::path::Path) -> bool {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(val) = serde_json::from_str::<Value>(&raw) else {
            return false;
        };
        val.get("refresh_token")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
            || val
                .get("token")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
    }

    pub(super) fn colab_cli_logged_in(&self) -> bool {
        self.find_colab_cli_binary().is_some()
            && self
                .colab_cli_token_path()
                .as_deref()
                .is_some_and(Self::colab_token_file_valid)
    }

    #[cfg(not(test))]
    pub(super) fn colab_cli_command(bin: &std::path::Path) -> std::process::Command {
        let mut cmd = std::process::Command::new(bin);
        cmd.env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("COLAB_CLI_REQUEST_TIMEOUT", "25")
            .env("COLAB_CLI_RETRY_BACKOFF", "2")
            .env("COLAB_CLI_ASSIGN_RETRIES", "5")
            .arg("--auth=oauth2");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        cmd
    }

    pub(super) fn kill_colab_pid(pid: u32) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(0x0800_0000)
                .status();
        }
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    #[cfg(not(test))]
    fn open_google_oauth_url(url: &str) {
        if !url.starts_with("https://accounts.google.com/") {
            return;
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", url])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(0x0800_0000)
                .spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open")
                .arg(url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let _ = std::process::Command::new("xdg-open")
                .arg(url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }

    pub(super) fn colab_cli_status(&self) -> Value {
        let installed = self.find_colab_cli_binary().is_some();
        let logged_in = self.colab_cli_logged_in();
        let mut guard = self
            .colab_cli
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !logged_in {
            guard.account_hint.clear();
            guard.expires_in.clear();
            guard.whoami_probed = false;
            guard.whoami_probing = false;
        } else if !guard.whoami_probed && !guard.whoami_probing {
            #[cfg(test)]
            {
                guard.account_hint = "t***r@gmail.com".into();
                guard.expires_in = "59m".into();
                guard.whoami_probed = true;
            }
            #[cfg(not(test))]
            if let Some(bin) = self.find_colab_cli_binary() {
                guard.whoami_probing = true;
                let slot = self.colab_cli.clone();
                std::thread::spawn(move || {
                    let out = Self::colab_cli_command(&bin)
                        .arg("whoami")
                        .stdin(std::process::Stdio::null())
                        .output();
                    if let Ok(mut st) = slot.lock() {
                        st.whoami_probing = false;
                        st.whoami_probed = true;
                        if let Ok(out) = out
                            && out.status.success()
                        {
                            let text = String::from_utf8_lossy(&out.stdout);
                            if let Some((masked, exp)) =
                                moss_voice::colab::parse_whoami_output(&text)
                            {
                                st.account_hint = masked;
                                st.expires_in = exp;
                            }
                        }
                    }
                });
            }
        }
        let runner_state = if guard.runner_state.is_empty() {
            "idle"
        } else {
            guard.runner_state.as_str()
        };
        let account_hint = if logged_in {
            if guard.account_hint.is_empty() {
                "已授权账号".to_owned()
            } else {
                guard.account_hint.clone()
            }
        } else {
            String::new()
        };
        let ref_audio_path = self.colab_ref_audio_path();
        let ref_audio_configured = ref_audio_path.is_file();
        let ref_audio_name = if ref_audio_configured {
            std::fs::read_to_string(self.colab_ref_audio_meta_path())
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .or_else(|| Some("moss_ref.wav".to_owned()))
        } else {
            None
        };
        serde_json::json!({
            "installed": installed,
            "logged_in": logged_in,
            "account_hint": account_hint,
            "expires_in": guard.expires_in,
            "auth_url": guard.auth_url,
            "runner_state": runner_state,
            "runner_step": guard.runner_step,
            "runner_error": guard.runner_error,
            "ref_audio_configured": ref_audio_configured,
            "ref_audio_name": ref_audio_name,
        })
    }

    pub(super) fn handle_colab_cli_action(&self, input: &Value) -> Result<(), CenterError> {
        let invalid = |message: &str| CenterError::new("voice_settings_invalid", 400, message);
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        match action {
            "refresh" => {
                #[cfg(not(test))]
                if let Some(bin) = self.find_colab_cli_binary()
                    && self.colab_cli_logged_in()
                    && let Ok(out) = Self::colab_cli_command(&bin)
                        .arg("whoami")
                        .stdin(std::process::Stdio::null())
                        .output()
                    && let Ok(mut st) = self.colab_cli.lock()
                {
                    st.whoami_probed = true;
                    st.whoami_probing = false;
                    if out.status.success() {
                        let text = String::from_utf8_lossy(&out.stdout);
                        if let Some((masked, exp)) = moss_voice::colab::parse_whoami_output(&text) {
                            st.account_hint = masked;
                            st.expires_in = exp;
                        }
                    }
                }
                Ok(())
            }
            "install" => {
                if self.find_colab_cli_binary().is_some() {
                    return Ok(());
                }
                #[cfg(test)]
                {
                    self.colab_cli_installed_for_test
                        .store(true, std::sync::atomic::Ordering::Release);
                    if let Ok(mut st) = self.colab_cli.lock() {
                        st.runner_error = None;
                        st.runner_step =
                            "已安装 Colab CLI，请点击「登录 Colab CLI」完成 Google 授权".into();
                    }
                    Ok(())
                }
                #[cfg(not(test))]
                {
                    let mut installed_ok = false;
                    let uv_name = if cfg!(windows) { "uv.exe" } else { "uv" };
                    let mut uv_bin: Option<std::path::PathBuf> = None;
                    if let Some(path_os) = std::env::var_os("PATH") {
                        for dir in std::env::split_paths(&path_os) {
                            let cand = dir.join(uv_name);
                            if cand.is_file() {
                                uv_bin = Some(cand);
                                break;
                            }
                        }
                    }
                    if uv_bin.is_none()
                        && let Some(home) = std::env::var_os("USERPROFILE")
                            .or_else(|| std::env::var_os("HOME"))
                            .map(std::path::PathBuf::from)
                    {
                        let cand = home.join(".local").join("bin").join(uv_name);
                        if cand.is_file() {
                            uv_bin = Some(cand);
                        }
                    }
                    if let Some(uv) = uv_bin {
                        let mut cmd = std::process::Command::new(uv);
                        cmd.args(["tool", "install", "google-colab-cli"])
                            .stdin(std::process::Stdio::null());
                        #[cfg(windows)]
                        {
                            use std::os::windows::process::CommandExt;
                            cmd.creation_flags(0x0800_0000);
                        }
                        if let Ok(out) = cmd.output()
                            && out.status.success()
                        {
                            installed_ok = self.find_colab_cli_binary().is_some();
                        }
                    }
                    if !installed_ok {
                        let py = self.laya_setup.find_python().map_err(|msg| {
                                CenterError::new(
                                    "colab_cli_install_failed",
                                    412,
                                    format!("自动安装 Colab CLI 需要本机有 uv 或 Python 3.10+：{msg}（你也可以点右上角导出 .ipynb 笔记本去浏览器手动运行）"),
                                )
                            })?;
                        let mut cmd = std::process::Command::new(&py.executable);
                        cmd.args(["-m", "pip", "install", "--user", "-U", "google-colab-cli"])
                            .stdin(std::process::Stdio::null());
                        #[cfg(windows)]
                        {
                            use std::os::windows::process::CommandExt;
                            cmd.creation_flags(0x0800_0000);
                        }
                        let out = cmd.output().map_err(|e| {
                            internal(format!("调用 pip 安装 google-colab-cli 失败：{e}"))
                        })?;
                        if !out.status.success() || self.find_colab_cli_binary().is_none() {
                            let err_tail = String::from_utf8_lossy(&out.stderr);
                            return Err(CenterError::new(
                                "colab_cli_install_failed",
                                500,
                                format!(
                                    "自动安装 google-colab-cli 未成功：{}（也可在终端手动运行 uv tool install google-colab-cli）",
                                    err_tail.lines().last().unwrap_or("安装失败")
                                ),
                            ));
                        }
                    }
                    if let Ok(mut st) = self.colab_cli.lock() {
                        st.runner_error = None;
                        st.runner_step =
                            "已安装 Colab CLI，请点击「登录 Colab CLI」完成 Google 授权".into();
                    }
                    Ok(())
                }
            }
            "set_ref_audio" => {
                use base64::Engine as _;
                let raw_b64 = input
                    .get("wav_base64")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                if raw_b64.is_empty() {
                    return Err(invalid("请选择有效的 WAV 参考音频文件"));
                }
                let clean_b64 = raw_b64
                    .split_once(";base64,")
                    .map(|(_, b)| b)
                    .unwrap_or(raw_b64)
                    .trim();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(clean_b64)
                    .map_err(|_| invalid("参考音频 Base64 解码失败"))?;
                moss_voice::colab::validate_ref_wav(&bytes).map_err(|msg| invalid(&msg))?;
                let raw_name = input
                    .get("file_name")
                    .and_then(Value::as_str)
                    .unwrap_or("moss_ref.wav");
                let safe_name: String = std::path::Path::new(raw_name)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("moss_ref.wav")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(80)
                    .collect();
                let wav_path = self.colab_ref_audio_path();
                let (runner_gen, ref_sync_gen, can_sync_now) = {
                    let _io_guard = self
                        .colab_ref_io
                        .lock()
                        .map_err(|_| internal("Colab 参考音频锁不可用"))?;
                    if let Some(parent) = wav_path.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| internal(e.to_string()))?;
                    }
                    let tmp_path = wav_path.with_extension("wav.tmp");
                    std::fs::write(&tmp_path, &bytes)
                        .map_err(|e| internal(format!("保存参考音频失败：{e}")))?;
                    std::fs::rename(&tmp_path, &wav_path).map_err(|e| {
                        let _ = std::fs::remove_file(&tmp_path);
                        internal(format!("替换参考音频失败：{e}"))
                    })?;
                    let _ = std::fs::write(
                        self.colab_ref_audio_meta_path(),
                        if safe_name.is_empty() {
                            "moss_ref.wav"
                        } else {
                            &safe_name
                        },
                    );
                    self.bump_colab_ref_sync()
                };
                #[cfg(not(test))]
                if can_sync_now
                    && let Some(bin) = self.find_colab_cli_binary()
                    && self.colab_cli_logged_in()
                {
                    let slot = self.colab_cli.clone();
                    let diag = self.diagnostics.clone();
                    std::thread::spawn(move || {
                        let mut upload_cmd = Self::colab_cli_command(&bin);
                        upload_cmd.args([
                            "upload",
                            "-s",
                            moss_voice::colab::COLAB_CLI_SESSION_NAME,
                            &wav_path.to_string_lossy(),
                            "/content/moss_ref.wav",
                        ]);
                        match Self::run_tracked_colab_ref_sync(
                            &slot,
                            runner_gen,
                            ref_sync_gen,
                            &mut upload_cmd,
                        ) {
                            Ok(Some(out)) if out.status.success() => {
                                if let Ok(mut st) = slot.lock()
                                    && st.generation == runner_gen
                                    && st.ref_sync_gen == ref_sync_gen
                                {
                                    st.synced_ref_gen = ref_sync_gen;
                                    st.runner_error = None;
                                    st.runner_step =
                                            "已将 550W 参考音频同步到云端 Colab T4（下一句合成即刻生效）"
                                                .into();
                                }
                            }
                            Ok(Some(out)) => {
                                let detail = format!(
                                    "{} {}",
                                    String::from_utf8_lossy(&out.stderr).trim(),
                                    String::from_utf8_lossy(&out.stdout).trim()
                                );
                                let trimmed = detail.trim();
                                if !trimmed.contains("not found")
                                    && !trimmed.contains("No active sessions")
                                {
                                    if let Ok(mut st) = slot.lock()
                                        && st.generation == runner_gen
                                        && st.ref_sync_gen == ref_sync_gen
                                    {
                                        st.runner_error = Some(format!(
                                            "参考音频已保存在本机，但同步到当前云端会话失败：{trimmed}"
                                        ));
                                    }
                                    diag.push_from(
                                        moss_voice_server::DiagSource::Moss,
                                        EventRecord::warn(
                                            DiagCategory::Model,
                                            format!("同步 550W 参考音频到 Colab 失败：{trimmed}"),
                                        ),
                                    );
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                if let Ok(mut st) = slot.lock()
                                    && st.generation == runner_gen
                                    && st.ref_sync_gen == ref_sync_gen
                                {
                                    st.runner_error = Some(format!(
                                        "参考音频已保存在本机，但调用 colab upload 失败：{e}"
                                    ));
                                }
                                diag.push_from(
                                    moss_voice_server::DiagSource::Moss,
                                    EventRecord::warn(
                                        DiagCategory::Model,
                                        format!("调用 colab upload 同步 550W 参考音频失败：{e}"),
                                    ),
                                );
                            }
                        }
                    });
                }
                #[cfg(test)]
                let _ = (runner_gen, ref_sync_gen, can_sync_now);
                Ok(())
            }
            "clear_ref_audio" => {
                let (runner_gen, ref_sync_gen, can_sync_now) = {
                    let _io_guard = self
                        .colab_ref_io
                        .lock()
                        .map_err(|_| internal("Colab 参考音频锁不可用"))?;
                    for target in [
                        self.colab_ref_audio_path(),
                        self.colab_ref_audio_meta_path(),
                    ] {
                        if let Err(err) = std::fs::remove_file(&target)
                            && err.kind() != std::io::ErrorKind::NotFound
                        {
                            return Err(CenterError::new(
                                "colab_ref_clear_failed",
                                500,
                                format!("清除本地参考音频失败：{err}"),
                            ));
                        }
                    }
                    self.bump_colab_ref_sync()
                };
                #[cfg(not(test))]
                if can_sync_now
                    && let Some(bin) = self.find_colab_cli_binary()
                    && self.colab_cli_logged_in()
                {
                    let slot = self.colab_cli.clone();
                    let diag = self.diagnostics.clone();
                    std::thread::spawn(move || {
                        let mut clear_cmd = Self::colab_cli_command(&bin);
                        clear_cmd.args([
                                "exec",
                                "-s",
                                moss_voice::colab::COLAB_CLI_SESSION_NAME,
                                "--timeout",
                                "25",
                                "-c",
                                "import pathlib; pathlib.Path('/content/moss_ref.wav').unlink(missing_ok=True)",
                            ]);
                        match Self::run_tracked_colab_ref_sync(
                            &slot,
                            runner_gen,
                            ref_sync_gen,
                            &mut clear_cmd,
                        ) {
                            Ok(Some(out)) if out.status.success() => {
                                if let Ok(mut st) = slot.lock()
                                    && st.generation == runner_gen
                                    && st.ref_sync_gen == ref_sync_gen
                                {
                                    st.synced_ref_gen = ref_sync_gen;
                                    st.runner_error = None;
                                    st.runner_step =
                                            "已清除云端自定义参考音频，恢复内置低沉男声基准（下一句合成生效）"
                                                .into();
                                }
                            }
                            Ok(Some(out)) => {
                                let detail = format!(
                                    "{} {}",
                                    String::from_utf8_lossy(&out.stderr).trim(),
                                    String::from_utf8_lossy(&out.stdout).trim()
                                );
                                let trimmed = detail.trim();
                                if !trimmed.contains("not found")
                                    && !trimmed.contains("No active sessions")
                                {
                                    if let Ok(mut st) = slot.lock()
                                        && st.generation == runner_gen
                                        && st.ref_sync_gen == ref_sync_gen
                                    {
                                        st.runner_error = Some(format!(
                                            "本地参考音频已清除，但清除当前云端会话副本失败：{trimmed}"
                                        ));
                                    }
                                    diag.push_from(
                                        moss_voice_server::DiagSource::Moss,
                                        EventRecord::warn(
                                            DiagCategory::Model,
                                            format!("清除 Colab 550W 参考音频失败：{trimmed}"),
                                        ),
                                    );
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                if let Ok(mut st) = slot.lock()
                                    && st.generation == runner_gen
                                    && st.ref_sync_gen == ref_sync_gen
                                {
                                    st.runner_error = Some(format!(
                                        "本地参考音频已清除，但调用 colab exec 失败：{e}"
                                    ));
                                }
                                diag.push_from(
                                    moss_voice_server::DiagSource::Moss,
                                    EventRecord::warn(
                                        DiagCategory::Model,
                                        format!("调用 colab exec 清除 550W 参考音频失败：{e}"),
                                    ),
                                );
                            }
                        }
                    });
                }
                #[cfg(test)]
                let _ = (runner_gen, ref_sync_gen, can_sync_now);
                Ok(())
            }
            "login_start" => {
                let Some(_bin) = self.find_colab_cli_binary() else {
                    return Err(CenterError::new(
                        "colab_cli_missing",
                        412,
                        "本机尚未安装 Colab CLI，请先在终端执行：uv tool install google-colab-cli",
                    ));
                };
                if let Some(token_path) = self.colab_cli_token_path()
                    && !Self::colab_token_file_valid(&token_path)
                {
                    let _ = std::fs::remove_file(&token_path);
                }
                #[cfg(test)]
                let url =
                    "https://accounts.google.com/o/oauth2/auth?response_type=code&client_id=mock"
                        .to_owned();
                #[cfg(not(test))]
                let url = {
                    if let Some(token_path) = self.colab_cli_token_path() {
                        let _ = std::fs::remove_file(&token_path);
                    }
                    let out = Self::colab_cli_command(&_bin)
                        .arg("whoami")
                        .stdin(std::process::Stdio::null())
                        .output()
                        .map_err(|e| internal(format!("启动 Colab CLI 授权失败：{e}")))?;
                    let combined = format!(
                        "{}\n{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                    let extracted = moss_voice::colab::extract_oauth_url(&combined).ok_or_else(|| {
                            internal("未能从 Colab CLI 获取 Google 授权链接，请确认 google-colab-cli 版本最新")
                        })?;
                    Self::open_google_oauth_url(&extracted);
                    extracted
                };
                if let Ok(mut st) = self.colab_cli.lock() {
                    st.auth_url = Some(url);
                    st.runner_error = None;
                    st.runner_step =
                        "已在浏览器打开 Google 授权页，请复制授权码粘贴到下方完成登录".into();
                }
                Ok(())
            }
            "login_complete" => {
                let Some(_bin) = self.find_colab_cli_binary() else {
                    return Err(CenterError::new(
                        "colab_cli_missing",
                        412,
                        "本机尚未安装 Colab CLI，请先在终端执行：uv tool install google-colab-cli",
                    ));
                };
                let code = input
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                if code.is_empty()
                    || code.len() > 512
                    || code.chars().any(|c| c.is_whitespace() || c.is_control())
                {
                    return Err(invalid("请粘贴有效的 Google 授权码（不能含空格）"));
                }
                #[cfg(test)]
                {
                    if let Some(path) = self.colab_cli_token_path() {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        std::fs::write(
                            &path,
                            r#"{"token":"mock-access","refresh_token":"mock-refresh"}"#,
                        )
                        .map_err(|e| internal(e.to_string()))?;
                    }
                    if let Ok(mut st) = self.colab_cli.lock() {
                        st.auth_url = None;
                        st.account_hint = "t***r@gmail.com".into();
                        st.expires_in = "59m".into();
                        st.whoami_probed = true;
                        st.runner_error = None;
                        st.runner_step = "Colab CLI 登录成功".into();
                    }
                }
                #[cfg(not(test))]
                {
                    use std::io::Write;
                    let mut child = Self::colab_cli_command(&_bin)
                        .arg("whoami")
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                        .map_err(|e| internal(format!("执行 Colab CLI 登录失败：{e}")))?;
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(format!("{code}\n").as_bytes());
                    }
                    let out = child
                        .wait_with_output()
                        .map_err(|e| internal(format!("等待 Colab CLI 登录结果失败：{e}")))?;
                    if !self.colab_cli_logged_in() {
                        let err_txt = String::from_utf8_lossy(&out.stderr);
                        return Err(invalid(&format!(
                            "Google 授权码验证未通过，请重新获取授权码再试（{}）",
                            err_txt.lines().last().unwrap_or("验证失败")
                        )));
                    }
                    let stdout_txt = String::from_utf8_lossy(&out.stdout);
                    if let Ok(mut st) = self.colab_cli.lock() {
                        st.auth_url = None;
                        st.whoami_probed = true;
                        st.runner_error = None;
                        st.runner_step = "Colab CLI 登录成功".into();
                        if let Some((masked, exp)) =
                            moss_voice::colab::parse_whoami_output(&stdout_txt)
                        {
                            st.account_hint = masked;
                            st.expires_in = exp;
                        }
                    }
                }
                if self.settings().builtin_colab.enabled {
                    self.spawn_colab_cli_runner();
                }
                Ok(())
            }
            "logout" => {
                self.cancel_colab_cli_runner(Some("正在退出 Colab CLI 登录..."));
                if let Some(path) = self.colab_cli_token_path()
                    && let Err(e) = std::fs::remove_file(&path)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(internal(format!("删除 Colab CLI 登录凭据失败：{e}")));
                }
                if let Ok(mut st) = self.colab_cli.lock() {
                    st.account_hint.clear();
                    st.expires_in.clear();
                    st.auth_url = None;
                    st.whoami_probed = false;
                    st.whoami_probing = false;
                    st.runner_error = None;
                    st.runner_step = "已退出 Colab CLI 登录".into();
                }
                Ok(())
            }
            "start_session" => {
                if !self.colab_cli_logged_in() {
                    return Err(invalid(
                        "尚未登录 Colab CLI：请先点击「登录 Colab CLI」完成授权",
                    ));
                }
                moss_voice::settings::update_voice(&self.settings_file(), |s| {
                    s.builtin_colab.enabled = true;
                    Ok(())
                })
                .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
                self.spawn_colab_cli_runner();
                Ok(())
            }
            "stop_session" => {
                self.cancel_colab_cli_runner(Some("正在释放云端 Colab T4 会话..."));
                #[cfg(not(test))]
                {
                    let Some(bin) = self.find_colab_cli_binary() else {
                        return Err(CenterError::new(
                            "colab_cli_missing",
                            412,
                            "本机尚未安装 Colab CLI，无法释放云端会话",
                        ));
                    };
                    let stop_gen = self
                        .colab_cli
                        .lock()
                        .map(|st| st.generation)
                        .map_err(|_| internal("Colab CLI 状态锁不可用"))?;
                    let mut cmd = Self::colab_cli_command(&bin);
                    cmd.args(["stop", "-s", moss_voice::colab::COLAB_CLI_SESSION_NAME]);
                    match Self::run_tracked_colab_output(&self.colab_cli, stop_gen, &mut cmd) {
                        Ok(Some(out)) => {
                            let combined = format!(
                                "{} {}",
                                String::from_utf8_lossy(&out.stderr).trim(),
                                String::from_utf8_lossy(&out.stdout).trim()
                            );
                            let trimmed = combined.trim();
                            if !out.status.success()
                                && !trimmed.contains("not found")
                                && !trimmed.contains("No active sessions")
                            {
                                let msg = format!(
                                    "释放云端 Colab T4 会话失败：{}",
                                    if trimmed.is_empty() {
                                        "colab stop 返回非零状态"
                                    } else {
                                        trimmed
                                    }
                                );
                                if let Ok(mut st) = self.colab_cli.lock()
                                    && st.generation == stop_gen
                                {
                                    st.runner_state = "error".into();
                                    st.runner_error = Some(msg.clone());
                                }
                                return Err(internal(msg));
                            }
                            if let Ok(mut st) = self.colab_cli.lock()
                                && st.generation == stop_gen
                            {
                                st.synced_ref_gen = st.ref_sync_gen;
                                st.runner_state = "idle".into();
                                st.runner_step = "云端 Colab T4 会话已释放".into();
                                st.runner_error = None;
                            }
                        }
                        Ok(None) => {}
                        Err(msg) => {
                            let err_text = format!("释放云端 Colab T4 会话失败：{msg}");
                            if let Ok(mut st) = self.colab_cli.lock()
                                && st.generation == stop_gen
                            {
                                st.runner_state = "error".into();
                                st.runner_error = Some(err_text.clone());
                            }
                            return Err(internal(err_text));
                        }
                    }
                }
                #[cfg(test)]
                if let Ok(mut st) = self.colab_cli.lock() {
                    st.runner_step = "云端 Colab T4 会话已释放".into();
                }
                Ok(())
            }
            _ => Err(invalid("未知的 colab_cli 操作")),
        }
    }
}
