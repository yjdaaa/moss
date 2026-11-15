//! Colab 云端会话后台托管、参考音频同步与 Colab 连通测试。

use super::*;

impl BuiltinVoice {
    pub(super) fn run_tracked_colab_output(
        slot: &Mutex<ColabCliState>,
        generation: u64,
        cmd: &mut std::process::Command,
    ) -> Result<Option<std::process::Output>, String> {
        let mut child = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        if let Ok(mut st) = slot.lock() {
            if st.generation != generation {
                drop(st);
                Self::kill_colab_pid(pid);
                let _ = child.wait();
                return Ok(None);
            }
            st.active_pid = Some(pid);
        }
        let out = child.wait_with_output();
        let still_current = if let Ok(mut st) = slot.lock() {
            if st.active_pid == Some(pid) {
                st.active_pid = None;
            }
            st.generation == generation
        } else {
            false
        };
        if !still_current {
            return Ok(None);
        }
        out.map(Some).map_err(|e| e.to_string())
    }

    /// 推进参考音频云端同步世代并终止旧的在途同步子进程，返回 `(runner_gen, ref_sync_gen, can_sync_now)`。
    pub(super) fn bump_colab_ref_sync(&self) -> (u64, u64, bool) {
        let (runner_gen, ref_sync_gen, can_sync_now, pid_to_kill) =
            if let Ok(mut st) = self.colab_cli.lock() {
                st.ref_sync_gen = st.ref_sync_gen.wrapping_add(1);
                (
                    st.generation,
                    st.ref_sync_gen,
                    st.runner_state != "starting",
                    st.ref_sync_pid.take(),
                )
            } else {
                (0, 0, false, None)
            };
        if let Some(pid) = pid_to_kill {
            Self::kill_colab_pid(pid);
        }
        (runner_gen, ref_sync_gen, can_sync_now)
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(super) fn run_tracked_colab_ref_sync(
        slot: &Mutex<ColabCliState>,
        runner_gen: u64,
        ref_sync_gen: u64,
        cmd: &mut std::process::Command,
    ) -> Result<Option<std::process::Output>, String> {
        let mut child = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        if let Ok(mut st) = slot.lock() {
            if st.generation != runner_gen || st.ref_sync_gen != ref_sync_gen {
                drop(st);
                Self::kill_colab_pid(pid);
                let _ = child.wait();
                return Ok(None);
            }
            st.ref_sync_pid = Some(pid);
        }
        let out = child.wait_with_output();
        let still_current = if let Ok(mut st) = slot.lock() {
            if st.ref_sync_pid == Some(pid) {
                st.ref_sync_pid = None;
            }
            st.generation == runner_gen && st.ref_sync_gen == ref_sync_gen
        } else {
            false
        };
        if !still_current {
            return Ok(None);
        }
        out.map(Some).map_err(|e| e.to_string())
    }

    pub(super) fn cancel_colab_cli_runner(&self, step_note: Option<&str>) {
        let (pid_to_kill, ref_pid_to_kill) = if let Ok(mut st) = self.colab_cli.lock() {
            st.generation = st.generation.wrapping_add(1);
            if st.ref_sync_pid.is_some() {
                st.ref_sync_gen = st.ref_sync_gen.wrapping_add(1);
            }
            if st.runner_state == "starting" || step_note.is_some() {
                st.runner_state = "idle".into();
            }
            if let Some(note) = step_note {
                st.runner_step = note.into();
                st.runner_error = None;
            } else if st.runner_step.contains("正在") {
                st.runner_step = "已取消自动拉起云端 Colab T4".into();
            }
            (st.active_pid.take(), st.ref_sync_pid.take())
        } else {
            (None, None)
        };
        if let Some(pid) = pid_to_kill {
            Self::kill_colab_pid(pid);
        }
        if let Some(pid) = ref_pid_to_kill {
            Self::kill_colab_pid(pid);
        }
    }

    pub(super) fn abort_colab_starting(
        slot: &Mutex<ColabCliState>,
        generation: u64,
        reason: String,
    ) {
        if let Ok(mut st) = slot.lock()
            && st.generation == generation
            && st.runner_state == "starting"
        {
            st.runner_state = "error".into();
            st.runner_step = reason.clone();
            st.runner_error = Some(reason);
        }
    }

    pub(super) fn spawn_colab_cli_runner(&self) {
        if !self.colab_cli_logged_in() {
            return;
        }
        #[cfg_attr(test, allow(unused_variables))]
        let (generation, active_pid_to_kill, ref_pid_to_kill) = {
            let Ok(mut st) = self.colab_cli.lock() else {
                return;
            };
            if st.runner_state == "starting" {
                return;
            }
            st.generation = st.generation.wrapping_add(1);
            st.runner_state = "starting".into();
            st.runner_step = "正在通过 Colab CLI 自动拉起云端 T4...".into();
            st.runner_error = None;
            (st.generation, st.active_pid.take(), st.ref_sync_pid.take())
        };
        if let Some(pid) = active_pid_to_kill {
            Self::kill_colab_pid(pid);
        }
        if let Some(pid) = ref_pid_to_kill {
            Self::kill_colab_pid(pid);
        }
        #[cfg(not(test))]
        {
            let Some(bin) = self.find_colab_cli_binary() else {
                Self::abort_colab_starting(
                    &self.colab_cli,
                    generation,
                    "未找到 Colab CLI 可执行文件，请先安装 google-colab-cli".into(),
                );
                return;
            };
            let Some(voice) = self.self_weak.upgrade() else {
                Self::abort_colab_starting(
                    &self.colab_cli,
                    generation,
                    "内置语音服务尚未完成初始化".into(),
                );
                return;
            };
            if let Err(err) = std::thread::Builder::new()
                    .name("moss-colab-cli-runner".into())
                    .spawn(move || {
                        let is_current = |slot: &Mutex<ColabCliState>| -> bool {
                            slot.lock().is_ok_and(|st| st.generation == generation)
                        };
                        let set_step = |slot: &Mutex<ColabCliState>, step: &str| {
                            if let Ok(mut st) = slot.lock()
                                && st.generation == generation
                            {
                                st.runner_step = step.to_owned();
                            }
                        };
                        let fail = |slot: &Mutex<ColabCliState>, msg: String| {
                            if let Ok(mut st) = slot.lock()
                                && st.generation == generation
                            {
                                st.runner_state = "error".into();
                                st.runner_step = "云端 Colab T4 自动拉起失败".into();
                                st.runner_error = Some(msg.clone());
                            }
                            voice.diagnostics.push_from(
                                moss_voice_server::DiagSource::Moss,
                                EventRecord::warn(
                                    DiagCategory::Model,
                                    format!("Colab CLI 自动拉起云端 T4 失败：{msg}"),
                                )
                                .hint("可在“本地模型 → Colab T4 专属联动”查看进度或点“一键拉起云端 T4”重试"),
                            );
                        };

                        let session_name = moss_voice::colab::COLAB_CLI_SESSION_NAME;
                        let ref_audio = voice.colab_ref_audio_path();
                        let reconcile_ref_audio = |was_uploaded: &mut bool,
                                                   unlink_only: bool,
                                                   final_running_step: Option<&str>|
                         -> Option<String> {
                            let mut attempted_gen: Option<u64> = None;
                            let mut last_sync_warn: Option<String> = None;
                            loop {
                                let (pending_gen, disk_has_ref) = {
                                    let Ok(_io_guard) = voice.colab_ref_io.lock() else {
                                        return last_sync_warn;
                                    };
                                    let has_ref = ref_audio.is_file();
                                    let Ok(mut st) = voice.colab_cli.lock() else {
                                        return last_sync_warn;
                                    };
                                    if st.generation != generation {
                                        return last_sync_warn;
                                    }
                                    if st.ref_sync_gen == st.synced_ref_gen
                                        || attempted_gen == Some(st.ref_sync_gen)
                                    {
                                        if let Some(step) = final_running_step {
                                            st.runner_state = "running".into();
                                            st.runner_step = step.into();
                                            st.runner_error = last_sync_warn.clone();
                                        }
                                        return last_sync_warn;
                                    }
                                    (st.ref_sync_gen, has_ref)
                                };
                                attempted_gen = Some(pending_gen);
                                if !unlink_only && disk_has_ref {
                                    let mut cmd = Self::colab_cli_command(&bin);
                                    cmd.args([
                                        "upload",
                                        "-s",
                                        session_name,
                                        &ref_audio.to_string_lossy(),
                                        "/content/moss_ref.wav",
                                    ]);
                                    match Self::run_tracked_colab_output(
                                        &voice.colab_cli,
                                        generation,
                                        &mut cmd,
                                    ) {
                                        Ok(Some(out)) if out.status.success() => {
                                            *was_uploaded = true;
                                            last_sync_warn = None;
                                            if let Ok(mut st) = voice.colab_cli.lock()
                                                && st.generation == generation
                                            {
                                                st.synced_ref_gen = pending_gen;
                                            }
                                        }
                                        Ok(Some(out)) => {
                                            let detail = format!(
                                                "{} {}",
                                                String::from_utf8_lossy(&out.stderr).trim(),
                                                String::from_utf8_lossy(&out.stdout).trim()
                                            );
                                            let trimmed = detail.trim();
                                            let warn_msg = format!(
                                                "参考音频已保存在本机，但同步到当前云端会话失败：{trimmed}"
                                            );
                                            last_sync_warn = Some(warn_msg);
                                            voice.diagnostics.push_from(
                                                moss_voice_server::DiagSource::Moss,
                                                EventRecord::warn(
                                                    DiagCategory::Model,
                                                    format!(
                                                        "同步 550W 参考音频到 Colab 失败：{trimmed}"
                                                    ),
                                                ),
                                            );
                                        }
                                        Ok(None) => return last_sync_warn,
                                        Err(e) => {
                                            let warn_msg = format!(
                                                "参考音频已保存在本机，但调用 colab upload 失败：{e}"
                                            );
                                            last_sync_warn = Some(warn_msg);
                                            voice.diagnostics.push_from(
                                                moss_voice_server::DiagSource::Moss,
                                                EventRecord::warn(
                                                    DiagCategory::Model,
                                                    format!(
                                                        "调用 colab upload 同步 550W 参考音频失败：{e}"
                                                    ),
                                                ),
                                            );
                                        }
                                    }
                                } else if !disk_has_ref && (*was_uploaded || !unlink_only) {
                                    let mut cmd = Self::colab_cli_command(&bin);
                                    cmd.args([
                                        "exec",
                                        "-s",
                                        session_name,
                                        "--timeout",
                                        "25",
                                        "-c",
                                        "import pathlib; pathlib.Path('/content/moss_ref.wav').unlink(missing_ok=True)",
                                    ]);
                                    match Self::run_tracked_colab_output(
                                        &voice.colab_cli,
                                        generation,
                                        &mut cmd,
                                    ) {
                                        Ok(Some(out)) if out.status.success() => {
                                            *was_uploaded = false;
                                            last_sync_warn = None;
                                            if let Ok(mut st) = voice.colab_cli.lock()
                                                && st.generation == generation
                                            {
                                                st.synced_ref_gen = pending_gen;
                                            }
                                        }
                                        Ok(Some(out)) => {
                                            let detail = format!(
                                                "{} {}",
                                                String::from_utf8_lossy(&out.stderr).trim(),
                                                String::from_utf8_lossy(&out.stdout).trim()
                                            );
                                            let trimmed = detail.trim();
                                            if trimmed.contains("not found")
                                                || trimmed.contains("No active sessions")
                                            {
                                                *was_uploaded = false;
                                            } else {
                                                let warn_msg = format!(
                                                    "本地参考音频已清除，但清除当前云端会话副本失败：{trimmed}"
                                                );
                                                last_sync_warn = Some(warn_msg);
                                                voice.diagnostics.push_from(
                                                    moss_voice_server::DiagSource::Moss,
                                                    EventRecord::warn(
                                                        DiagCategory::Model,
                                                        format!(
                                                            "清除 Colab 550W 参考音频失败：{trimmed}"
                                                        ),
                                                    ),
                                                );
                                            }
                                        }
                                        Ok(None) => return last_sync_warn,
                                        Err(e) => {
                                            let warn_msg = format!(
                                                "本地参考音频已清除，但调用 colab exec 失败：{e}"
                                            );
                                            last_sync_warn = Some(warn_msg);
                                            voice.diagnostics.push_from(
                                                moss_voice_server::DiagSource::Moss,
                                                EventRecord::warn(
                                                    DiagCategory::Model,
                                                    format!(
                                                        "调用 colab exec 清除 550W 参考音频失败：{e}"
                                                    ),
                                                ),
                                            );
                                        }
                                    }
                                }
                            }
                        };

                        // 1. 若已有保存的隧道地址且 /healthz 存活，直接复用
                        let current_settings = voice.settings();
                        if let Ok(Some((active_cfg, _))) = voice.active_colab(&current_settings) {
                            let health_url = format!(
                                "{}/healthz",
                                active_cfg.url.trim_end_matches('/').trim_end_matches("/v1")
                            );
                            let alive = reqwest::blocking::Client::builder()
                                .timeout(std::time::Duration::from_secs(3))
                                .build()
                                .ok()
                                .and_then(|c| c.get(&health_url).send().ok())
                                .is_some_and(|r| r.status().is_success());
                            if alive {
                                let mut reused_uploaded = false;
                                let _ = reconcile_ref_audio(
                                    &mut reused_uploaded,
                                    false,
                                    Some("云端 Colab T4 隧道在线并已接管（Qwen3-VL-8B + 550W 克隆）"),
                                );
                                return;
                            }
                        }

                        if !is_current(&voice.colab_cli) {
                            return;
                        }
                        set_step(
                            &voice.colab_cli,
                            &format!("正在检查 Colab 云端会话 ({session_name})..."),
                        );
                        let mut status_cmd = Self::colab_cli_command(&bin);
                        status_cmd.args(["status", "-s", session_name]);
                        let session_exists = match Self::run_tracked_colab_output(
                            &voice.colab_cli,
                            generation,
                            &mut status_cmd,
                        ) {
                            Ok(Some(out)) => {
                                let combined = format!(
                                    "{}\n{}",
                                    String::from_utf8_lossy(&out.stdout),
                                    String::from_utf8_lossy(&out.stderr)
                                );
                                out.status.success()
                                    && !combined.contains("not found")
                                    && !combined.contains("No active sessions")
                            }
                            Ok(None) => return,
                            Err(_) => false,
                        };

                        if !session_exists {
                            if !is_current(&voice.colab_cli) {
                                return;
                            }
                            set_step(
                                &voice.colab_cli,
                                "正在向 Google 申请免费 T4 显卡 (colab new -s moss-t4 --gpu T4)...",
                            );
                            let mut new_cmd = Self::colab_cli_command(&bin);
                            new_cmd.args(["new", "-s", session_name, "--gpu", "T4"]);
                            match Self::run_tracked_colab_output(
                                &voice.colab_cli,
                                generation,
                                &mut new_cmd,
                            ) {
                                Ok(Some(out)) if out.status.success() => {}
                                Ok(Some(out)) => {
                                    let detail = format!(
                                        "{} {}",
                                        String::from_utf8_lossy(&out.stderr).trim(),
                                        String::from_utf8_lossy(&out.stdout).trim()
                                    );
                                    fail(
                                        &voice.colab_cli,
                                        format!("分配 T4 显卡未通过：{}", detail.trim()),
                                    );
                                    return;
                                }
                                Ok(None) => return,
                                Err(e) => {
                                    fail(&voice.colab_cli, format!("调用 colab new 失败：{e}"));
                                    return;
                                }
                            }
                        }

                        if !is_current(&voice.colab_cli) {
                            return;
                        }
                        let (upload_ref_gen, initial_disk_has_ref) = {
                            let _io_guard = voice.colab_ref_io.lock().ok();
                            let has_ref = ref_audio.is_file();
                            let cur_gen = voice
                                .colab_cli
                                .lock()
                                .ok()
                                .map(|st| st.ref_sync_gen)
                                .unwrap_or(0);
                            (cur_gen, has_ref)
                        };
                        let mut uploaded_ref = false;
                        if initial_disk_has_ref {
                            set_step(
                                &voice.colab_cli,
                                "正在将本机保存的 550W 参考音频同步到云端 T4...",
                            );
                            let mut upload_cmd = Self::colab_cli_command(&bin);
                            upload_cmd.args([
                                "upload",
                                "-s",
                                session_name,
                                &ref_audio.to_string_lossy(),
                                "/content/moss_ref.wav",
                            ]);
                            match Self::run_tracked_colab_output(
                                &voice.colab_cli,
                                generation,
                                &mut upload_cmd,
                            ) {
                                Ok(Some(out)) if out.status.success() => {
                                    uploaded_ref = true;
                                    if let Ok(mut st) = voice.colab_cli.lock()
                                        && st.generation == generation
                                    {
                                        st.synced_ref_gen = upload_ref_gen;
                                    }
                                }
                                Ok(Some(out)) => {
                                    let detail = format!(
                                        "{} {}",
                                        String::from_utf8_lossy(&out.stderr).trim(),
                                        String::from_utf8_lossy(&out.stdout).trim()
                                    );
                                    fail(
                                        &voice.colab_cli,
                                        format!("上传 550W 参考音频到云端失败：{}", detail.trim()),
                                    );
                                    return;
                                }
                                Ok(None) => return,
                                Err(e) => {
                                    fail(
                                        &voice.colab_cli,
                                        format!("调用 colab upload 同步参考音频失败：{e}"),
                                    );
                                    return;
                                }
                            }
                        }
                        let _ = reconcile_ref_audio(&mut uploaded_ref, false, None);

                        let fail_after_upload = |was_uploaded: &mut bool, msg: String| {
                            let full_msg = if *was_uploaded {
                                if let Some(warn) = reconcile_ref_audio(was_uploaded, true, None) {
                                    format!("{msg}（另：{warn}）")
                                } else {
                                    msg
                                }
                            } else {
                                msg
                            };
                            fail(&voice.colab_cli, full_msg);
                        };

                        if !is_current(&voice.colab_cli) {
                            return;
                        }
                        let bootstrap_path = voice
                            .host
                            .runtime_root
                            .path
                            .join("moss_colab_bootstrap.py");
                        if let Err(e) = std::fs::write(
                            &bootstrap_path,
                            moss_voice::colab::cli_bootstrap_script(),
                        ) {
                            fail_after_upload(
                                &mut uploaded_ref,
                                format!("写入启动脚本失败：{e}"),
                            );
                            return;
                        }

                        set_step(
                            &voice.colab_cli,
                            "正在云端 T4 极速并发部署 Qwen3-VL-8B 与 550W 克隆服务（约 1.5~2 分钟）...",
                        );

                        let mut creds_found: Option<(String, String)> = None;
                        let mut last_combined = String::new();
                        let mut last_exit_note = String::new();

                        for attempt in 0..4 {
                            if !is_current(&voice.colab_cli) {
                                return;
                            }
                            if attempt > 0 {
                                std::thread::sleep(std::time::Duration::from_secs(2));
                            }
                            let mut child = match Self::colab_cli_command(&bin)
                                .args([
                                    "exec",
                                    "-s",
                                    session_name,
                                    "--timeout",
                                    "1200",
                                    "-f",
                                    &bootstrap_path.to_string_lossy(),
                                ])
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped())
                                .spawn()
                            {
                                Ok(c) => c,
                                Err(e) => {
                                    fail_after_upload(
                                        &mut uploaded_ref,
                                        format!("调用 colab exec 失败：{e}"),
                                    );
                                    return;
                                }
                            };
                            let child_pid = child.id();
                            if let Ok(mut st) = voice.colab_cli.lock() {
                                if st.generation != generation {
                                    drop(st);
                                    Self::kill_colab_pid(child_pid);
                                    let _ = child.wait();
                                    return;
                                }
                                st.active_pid = Some(child_pid);
                            }

                            let stderr_handle = child.stderr.take().map(|err_pipe| {
                                std::thread::spawn(move || {
                                    use std::io::BufRead;
                                    let mut buf = String::new();
                                    let r = std::io::BufReader::new(err_pipe);
                                    for line in r.lines().map_while(Result::ok) {
                                        buf.push_str(&line);
                                        buf.push('\n');
                                        if line.contains("RuntimeError: Connection was lost.")
                                            || line.contains("TimeoutError:")
                                        {
                                            Self::kill_colab_pid(child_pid);
                                            break;
                                        }
                                    }
                                    buf
                                })
                            });

                            let mut collected_stdout = String::new();
                            if let Some(out_pipe) = child.stdout.take() {
                                use std::io::BufRead;
                                let reader = std::io::BufReader::new(out_pipe);
                                for line in reader.lines().map_while(Result::ok) {
                                    if !is_current(&voice.colab_cli) {
                                        Self::kill_colab_pid(child_pid);
                                        let _ = child.wait();
                                        return;
                                    }
                                    if let Some(rest) = line.trim().strip_prefix("[MOSS-COLAB]") {
                                        let clean = rest.trim();
                                        if !clean.is_empty() {
                                            set_step(&voice.colab_cli, clean);
                                        }
                                    }
                                    collected_stdout.push_str(&line);
                                    collected_stdout.push('\n');
                                    if moss_voice::colab::extract_tunnel_credentials(&collected_stdout)
                                        .is_some()
                                    {
                                        Self::kill_colab_pid(child_pid);
                                        break;
                                    }
                                }
                            }

                            let status = child.wait();
                            Self::kill_colab_pid(child_pid);
                            if let Ok(mut st) = voice.colab_cli.lock()
                                && st.active_pid == Some(child_pid)
                            {
                                st.active_pid = None;
                            }
                            let stderr_txt = stderr_handle
                                .and_then(|h| h.join().ok())
                                .unwrap_or_default();
                            last_combined = format!("{collected_stdout}\n{stderr_txt}");
                            last_exit_note = match status {
                                Ok(s) => format!("exit {}", s.code().unwrap_or(-1)),
                                Err(e) => e.to_string(),
                            };

                            if let Some(pair) =
                                moss_voice::colab::extract_tunnel_credentials(&last_combined)
                            {
                                creds_found = Some(pair);
                                break;
                            }
                            if !last_combined.contains("Connection was lost")
                                && !last_combined.contains("TimeoutError")
                            {
                                break;
                            }
                        }

                        if !is_current(&voice.colab_cli) {
                            return;
                        }

                        if let Some((url, token)) = creds_found {
                            let Ok(db) = voice.host.db.lock() else {
                                fail_after_upload(
                                    &mut uploaded_ref,
                                    "数据库繁忙，无法保存 Colab 隧道凭据".into(),
                                );
                                return;
                            };
                            if !is_current(&voice.colab_cli) {
                                return;
                            }
                            let ops = vec![(
                                moss_voice::colab::COLAB_SECRET_NAME.to_owned(),
                                Some(token),
                            )];
                            if let Err(err) = commit_secrets_then_settings(&db, &ops, || {
                                moss_voice::settings::update_voice(
                                    &voice.settings_file(),
                                    |s| {
                                        s.builtin_colab.url = url.clone();
                                        Ok(())
                                    },
                                )
                                .map_err(|msg| {
                                    CenterError::new("voice_settings_write_failed", 500, msg)
                                })
                            }) {
                                drop(db);
                                fail_after_upload(
                                    &mut uploaded_ref,
                                    format!("保存 Colab 隧道凭据失败：{}", err.message),
                                );
                                return;
                            }
                            drop(db);
                            if !is_current(&voice.colab_cli) {
                                return;
                            }
                            voice.sync_local_vision(true);
                            if let Err(err) = voice.restart_if_running() {
                                fail_after_upload(
                                    &mut uploaded_ref,
                                    format!("已写入 Colab 隧道但重启内置语音服务失败：{}", err.message),
                                );
                                return;
                            }
                            let _ = reconcile_ref_audio(
                                &mut uploaded_ref,
                                false,
                                Some("云端 Colab T4 已就绪并自动接管（Qwen3-VL-8B + 550W 克隆）"),
                            );
                        } else {
                            let last_line = last_combined
                                .lines()
                                .rev()
                                .find(|l| !l.trim().is_empty())
                                .unwrap_or("未返回隧道地址");
                            fail_after_upload(
                                &mut uploaded_ref,
                                format!("云端脚本未返回有效隧道（{last_exit_note}）：{last_line}"),
                            );
                        }
                    })
                {
                    Self::abort_colab_starting(
                        &self.colab_cli,
                        generation,
                        format!("无法启动 Colab 后台线程：{err}"),
                    );
                }
        }
    }

    /// n80：测试 Colab T4 专属联动（`target = "llm"` / `"tts"` 供顶部“测试全部环节”按环节探测；
    /// 默认 `"all"` 依次实测大模型工具调用、摄像头看图与 550W 克隆语音合成，并返回试听音频）。
    pub fn colab_test(&self, input: &Value) -> Result<Value, CenterError> {
        use moss_voice_providers::vision::VisionProvider as _;
        let invalid = |message: &str| CenterError::new("model_input_invalid", 400, message);
        let saved = self.settings().builtin_colab;
        let raw_url = input
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(saved.url.as_str());
        let url =
            moss_voice::colab::normalize_colab_url(raw_url).map_err(|message| invalid(&message))?;
        let raw_key = input
            .get("api_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let api_key = match raw_key {
            Some(k) => {
                if k.len() > 8192 || k.chars().any(char::is_control) {
                    return Err(invalid("Colab API 密钥格式不正确"));
                }
                k
            }
            None => self
                .colab_secret()?
                .ok_or_else(|| invalid("请先填写 Colab API 密钥"))?,
        };
        let llm_model = moss_voice::colab::validate_colab_token(
            "Colab 大模型名",
            input
                .get("llm_model")
                .and_then(Value::as_str)
                .unwrap_or(&saved.llm_model),
            moss_voice::colab::DEFAULT_COLAB_LLM_MODEL,
        )
        .map_err(|message| invalid(&message))?;
        let tts_model = moss_voice::colab::validate_colab_token(
            "Colab 合成模型名",
            input
                .get("tts_model")
                .and_then(Value::as_str)
                .unwrap_or(&saved.tts_model),
            moss_voice::colab::DEFAULT_COLAB_TTS_MODEL,
        )
        .map_err(|message| invalid(&message))?;
        let tts_voice = moss_voice::colab::validate_colab_token(
            "Colab 合成音色",
            input
                .get("tts_voice")
                .and_then(Value::as_str)
                .unwrap_or(&saved.tts_voice),
            moss_voice::colab::DEFAULT_COLAB_TTS_VOICE,
        )
        .map_err(|message| invalid(&message))?;
        let tts_speed = match input.get("tts_speed").and_then(Value::as_f64) {
            Some(v) => {
                moss_voice::colab::validate_colab_speed(v).map_err(|message| invalid(&message))?
            }
            None => saved.tts_speed,
        };
        let target = input
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or("all")
            .trim();
        let llm_resolved = moss_voice::models::ResolvedSection {
            section: Section::Llm,
            kind: ProviderKind::Openai,
            url: url.clone(),
            api_key: api_key.clone(),
            model: llm_model.clone(),
            voice: String::new(),
            speed: 1.0,
        };
        let tts_resolved = moss_voice::models::ResolvedSection {
            section: Section::Tts,
            kind: ProviderKind::Openai,
            url: moss_voice::models::tts_full_url(&url),
            api_key: api_key.clone(),
            model: tts_model,
            voice: tts_voice,
            speed: tts_speed as f64,
        };
        if target == "llm" {
            let res = moss_voice::models::test_section_with(&llm_resolved, None);
            return serde_json::to_value(res).map_err(|e| internal(e.to_string()));
        }
        if target == "tts" {
            let res = moss_voice::models::test_section_with(&tts_resolved, None);
            return serde_json::to_value(res).map_err(|e| internal(e.to_string()));
        }
        let llm_res = moss_voice::models::test_section_with(&llm_resolved, None);
        if !llm_res.ok {
            return Ok(serde_json::json!({
                "section": "colab",
                "ok": false,
                "elapsed_ms": llm_res.elapsed_ms,
                "detail": format!("大模型未通过：{}", llm_res.detail),
            }));
        }
        let mut png_buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 8)
            .write_to(&mut png_buf, image::ImageFormat::Png)
            .map_err(|_| internal("生成内存测试图片失败"))?;
        let png = png_buf.into_inner();
        let vision_started = std::time::Instant::now();
        let vision_ok = moss_voice_providers::vision::OpenAiVision::new(&url, &llm_model, &api_key)
            .ok()
            .and_then(|provider| {
                tauri::async_runtime::block_on(provider.explain(
                    "请仅用一句话描述此测试图像。",
                    "image/png",
                    &png,
                ))
                .ok()
            })
            .is_some_and(|text| !text.trim().is_empty());
        let vision_ms = vision_started.elapsed().as_millis() as u64;
        if !vision_ok {
            return Ok(serde_json::json!({
                "section": "colab",
                "ok": false,
                "elapsed_ms": llm_res.elapsed_ms + vision_ms,
                "detail": "大模型通过，但视觉看图未通过：请确认 Colab 上启动了带 --mmproj 的 Qwen3-VL 多模态模型",
            }));
        }
        let tts_res = moss_voice::models::test_section_with(&tts_resolved, None);
        let total_ms = llm_res.elapsed_ms + vision_ms + tts_res.elapsed_ms;
        if !tts_res.ok {
            return Ok(serde_json::json!({
                "section": "colab",
                "ok": false,
                "elapsed_ms": total_ms,
                "detail": format!("大模型与看图已通过，但 550W 克隆语音合成未通过：{}", tts_res.detail),
            }));
        }
        Ok(serde_json::json!({
            "section": "colab",
            "ok": true,
            "elapsed_ms": total_ms,
            "detail": format!(
                "三项全部通过（大模型工具调用 {} ms · 摄像头看图 {} ms · 550W 克隆合成 {} ms）",
                llm_res.elapsed_ms, vision_ms, tts_res.elapsed_ms
            ),
            "audio_data_url": tts_res.audio_data_url,
        }))
    }
}
