//! NV13 Unix / macOS process-lifetime harness.
//!
//! Mirrors `windows_process`: spawns the CLI inside a dedicated process group
//! (`process_group(0)` / `setpgid(0, 0)` before `exec`), re-checks revocation
//! before delivering task `stdin`, enforces the exact `CLI_ENV_KEYS` environment
//! whitelist, drains and redacts bounded `stdout`/`stderr`, and kills + reaps
//! the entire process group (`kill(-pgid, SIGKILL)` until `ESRCH`) on exit,
//! cancellation, revocation, or timeout.
//! Like Windows Job Objects, process groups bound process-tree lifetime only and
//! do not restrict filesystem reads or network access outside the CLI's own sandbox.
#![allow(dead_code)]

#[cfg(test)]
mod tests;

use super::{MAX_OUTPUT_BYTES, MAX_OUTPUT_LINE_BYTES, MAX_RUN_SECONDS};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(40);
const REAP_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Finish {
    Completed,
    ExitFailure(u32),
    Cancelled,
    TimedOut,
}

#[derive(Debug)]
pub(super) struct ProcessResult {
    pub finish: Finish,
    pub output: String,
    pub truncated: bool,
    pub children_reaped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessFailure {
    InvalidInput,
    StartFailed,
    PipeFailed,
    WaitFailed,
    ReadFailed,
    ReapUnconfirmed,
}

pub(super) fn validate_environment(env: &BTreeMap<String, String>) -> Result<(), ProcessFailure> {
    let mut sorted = env.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
    let mut previous = String::new();
    for (key, value) in sorted {
        let allowed = super::CLI_ENV_KEYS;
        let login_home = ["CODEX_HOME"];
        #[cfg(test)]
        let fixture = ["NV13_PROCESS_FIXTURE", "NV13_FIXTURE_MARKER"];
        #[cfg(not(test))]
        let fixture: [&str; 0] = [];
        if !allowed
            .iter()
            .chain(login_home.iter())
            .chain(fixture.iter())
            .any(|item| item.eq_ignore_ascii_case(key))
            || key.is_empty()
            || key.starts_with('=')
            || key.contains(['=', '\0'])
            || value.contains('\0')
            || value.len() > 4096
            || key.eq_ignore_ascii_case(&previous)
        {
            return Err(ProcessFailure::InvalidInput);
        }
        previous = key.clone();
    }
    Ok(())
}

/// Guard for a dedicated Unix process group (`pgid > 1`). Dropping an unreaped
/// guard sends `SIGKILL` to `-pgid` so descendants do not survive panics/timeouts.
pub(super) struct ProcessGroupGuard {
    pgid: libc::pid_t,
    reaped: bool,
}
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if !self.reaped && self.pgid > 1 {
            // SAFETY: negative pgid targets the dedicated process group created via process_group(0).
            unsafe {
                let _ = libc::kill(-self.pgid, libc::SIGKILL);
            }
        }
    }
}

pub(super) fn confine_running_group(pid: u32) -> ProcessGroupGuard {
    let pgid = i32::try_from(pid).unwrap_or(0);
    ProcessGroupGuard { pgid, reaped: pgid <= 1 }
}

fn reap_group(guard: &mut ProcessGroupGuard, child: &mut Child) -> Result<(), ProcessFailure> {
    if guard.pgid <= 1 {
        return Err(ProcessFailure::ReapUnconfirmed);
    }
    // SAFETY: kill(-pgid, SIGKILL) terminates every member of the dedicated process group.
    unsafe {
        let _ = libc::kill(-guard.pgid, libc::SIGKILL);
    }
    let _ = child.wait();
    let started = Instant::now();
    loop {
        // SAFETY: signal 0 probes whether any member remains in process group `pgid`.
        let probe = unsafe { libc::kill(-guard.pgid, 0) };
        if probe == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            guard.reaped = true;
            return Ok(());
        }
        if started.elapsed() > REAP_DEADLINE {
            return Err(ProcessFailure::ReapUnconfirmed);
        }
        unsafe {
            let _ = libc::kill(-guard.pgid, libc::SIGKILL);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[derive(Default)]
struct Output {
    buffer: String,
    truncated: bool,
}
impl Output {
    fn add_line(&mut self, bytes: &[u8], too_long: bool) {
        if too_long {
            self.truncated = true;
        }
        let message = String::from_utf8_lossy(bytes);
        let lowered = message.to_ascii_lowercase();
        let denied = [
            "authorization",
            "bearer ",
            "token",
            "password",
            "secret",
            "api_key",
            "apikey",
            "sk-",
            "cookie",
            ".env",
        ];
        let line = if denied.iter().any(|word| lowered.contains(word)) {
            "[redacted]".to_owned()
        } else {
            message
                .chars()
                .filter(|c| *c == '\t' || !c.is_control())
                .collect::<String>()
        };
        let space = MAX_OUTPUT_BYTES.saturating_sub(self.buffer.len());
        if space == 0 {
            self.truncated = true;
            return;
        }
        let mut append = line;
        append.push('\n');
        if append.len() > space {
            self.truncated = true;
            let end = append
                .char_indices()
                .map(|(offset, _)| offset)
                .take_while(|offset| *offset <= space)
                .last()
                .unwrap_or(0);
            self.buffer.push_str(&append[..end]);
        } else {
            self.buffer.push_str(&append);
        }
    }
}

fn drain<R: Read>(mut input: R, collector: Arc<Mutex<Output>>) -> io::Result<()> {
    let mut buffer = [0u8; 8192];
    let mut line = Vec::with_capacity(4096);
    let mut too_long = false;
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        for &byte in &buffer[..n] {
            if byte == b'\n' {
                let mut out = collector.lock().unwrap_or_else(|e| e.into_inner());
                out.add_line(&line, too_long);
                line.clear();
                too_long = false;
            } else if line.len() < MAX_OUTPUT_LINE_BYTES {
                line.push(byte);
            } else {
                too_long = true;
            }
        }
    }
    if !line.is_empty() || too_long {
        collector
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .add_line(&line, too_long);
    }
    Ok(())
}

pub(super) fn run_fixture_process(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    stdin: &[u8],
    alive: &dyn Fn() -> bool,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<ProcessResult, ProcessFailure> {
    if !executable.is_absolute()
        || !executable.is_file()
        || !cwd.is_absolute()
        || !cwd.is_dir()
        || timeout.is_zero()
        || stdin.len() > 128 * 1024
        || args.len() > 64
        || args.iter().any(|arg| arg.contains('\0'))
        || !alive()
        || cancel.load(Ordering::Acquire)
    {
        return Err(ProcessFailure::InvalidInput);
    }
    validate_environment(env)?;

    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let mut child = command.spawn().map_err(|_| ProcessFailure::StartFailed)?;

    let pgid = i32::try_from(child.id()).map_err(|_| ProcessFailure::StartFailed)?;
    if pgid <= 1 {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ProcessFailure::StartFailed);
    }
    let mut group = ProcessGroupGuard { pgid, reaped: false };

    // Re-check immediately after spawn before writing task stdin; kill entire group if revoked.
    if !alive() || cancel.load(Ordering::Acquire) {
        drop(child.stdin.take());
        reap_group(&mut group, &mut child)?;
        return Ok(ProcessResult {
            finish: Finish::Cancelled,
            output: String::new(),
            truncated: false,
            children_reaped: true,
        });
    }

    let mut child_stdin = child.stdin.take().ok_or(ProcessFailure::PipeFailed)?;
    let child_stdout = child.stdout.take().ok_or(ProcessFailure::PipeFailed)?;
    let child_stderr = child.stderr.take().ok_or(ProcessFailure::PipeFailed)?;

    let collector = Arc::new(Mutex::new(Output::default()));
    let out = Arc::clone(&collector);
    let err = Arc::clone(&collector);
    let stdout = thread::spawn(move || drain(child_stdout, out));
    let stderr = thread::spawn(move || drain(child_stderr, err));
    let input = stdin.to_vec();
    let writer = thread::spawn(move || child_stdin.write_all(&input));

    let started = Instant::now();
    let deadline = timeout.min(Duration::from_secs(MAX_RUN_SECONDS));
    let finish = loop {
        if cancel.load(Ordering::Acquire) || !alive() {
            break Finish::Cancelled;
        }
        if started.elapsed() >= deadline {
            break Finish::TimedOut;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break match status.code() {
                    Some(0) => Finish::Completed,
                    Some(code) => Finish::ExitFailure(code.cast_unsigned()),
                    None => Finish::ExitFailure(128),
                };
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(_) => {
                reap_group(&mut group, &mut child)?;
                return Err(ProcessFailure::WaitFailed);
            }
        }
    };

    reap_group(&mut group, &mut child)?;
    let wrote = writer.join().map_err(|_| ProcessFailure::PipeFailed)?;
    let read_out = stdout.join().map_err(|_| ProcessFailure::ReadFailed)?;
    let read_err = stderr.join().map_err(|_| ProcessFailure::ReadFailed)?;
    if read_out.is_err() || read_err.is_err() {
        return Err(ProcessFailure::ReadFailed);
    }
    if wrote.is_err() && finish == Finish::Completed {
        return Err(ProcessFailure::PipeFailed);
    }
    let collected = collector.lock().unwrap_or_else(|e| e.into_inner());
    Ok(ProcessResult {
        finish,
        output: collected.buffer.clone(),
        truncated: collected.truncated,
        children_reaped: true,
    })
}
