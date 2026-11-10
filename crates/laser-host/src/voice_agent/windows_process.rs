//! NV13 Windows process-lifetime harness.
//!
//! This is NOT an OS read/network sandbox: Job Objects constrain process trees
//! and kill on handle close; they do not protect unrelated files or credentials.
//! Since 2026-10-15 the production AgentCoordinator runs CLI plans through this
//! harness (JobRunner in voice_agent.rs). The CLI's own sandbox on Windows still
//! cannot restrict reads outside the workspace; that residual risk is declared
//! in engine.rs.

#[cfg(test)]
mod tests;

use super::{MAX_OUTPUT_BYTES, MAX_OUTPUT_LINE_BYTES, MAX_RUN_SECONDS};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, c_void},
    fs::File,
    io::{self, Read, Write},
    os::windows::{ffi::OsStrExt, io::FromRawHandle},
    path::Path,
    ptr,
    sync::{Arc, Mutex, atomic::AtomicBool},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::SECURITY_ATTRIBUTES,
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
            JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
            TerminateJobObject,
        },
        Pipes::CreatePipe,
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            InitializeProcThreadAttributeList, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            PROCESS_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE, ResumeThread,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

/// Never bypassed even in the fixture path. Does not constrain filesystem/network.
const MAX_JOB_PROCESSES: u32 = 8;
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
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "audited by process-harness unit tests")
    )]
    pub truncated: bool,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "audited by process-harness unit tests")
    )]
    pub children_reaped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessFailure {
    InvalidInput,
    StartFailed,
    PipeFailed,
    JobFailed,
    ResumeFailed,
    WaitFailed,
    ReadFailed,
    ReapUnconfirmed,
}

struct Handle(HANDLE);
impl Handle {
    fn from_raw(value: HANDLE) -> Result<Self, ProcessFailure> {
        if value.is_null() {
            Err(ProcessFailure::StartFailed)
        } else {
            Ok(Self(value))
        }
    }
    fn file(mut self) -> File {
        let raw = self.0;
        self.0 = ptr::null_mut();
        // SAFETY: the handle was obtained from CreatePipe, and is uniquely owned.
        unsafe { File::from_raw_handle(raw) }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Handle owns one kernel handle and closes it exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
struct Job(Handle);
impl Job {
    fn new() -> Result<Self, ProcessFailure> {
        // SAFETY: unnamed job and valid zero-initialized limit information.
        unsafe {
            let job = Self(
                Handle::from_raw(CreateJobObjectW(ptr::null(), ptr::null()))
                    .map_err(|_| ProcessFailure::JobFailed)?,
            );
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            info.BasicLimitInformation.ActiveProcessLimit = MAX_JOB_PROCESSES;
            if SetInformationJobObject(
                job.0.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                size_of_val(&info) as u32,
            ) == 0
            {
                return Err(ProcessFailure::JobFailed);
            }
            Ok(job)
        }
    }
    fn terminate(&self) -> Result<(), ProcessFailure> {
        // SAFETY: job remains owned for duration of request; 1 is a synthetic exit code.
        if unsafe { TerminateJobObject(self.0.0, 1) } == 0 {
            return Err(ProcessFailure::ReapUnconfirmed);
        }
        Ok(())
    }
    fn no_active_processes(&self) -> Result<bool, ProcessFailure> {
        // SAFETY: query into a fixed-sized, correctly aligned allocation.
        unsafe {
            let mut usage: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
            if QueryInformationJobObject(
                self.0.0,
                JobObjectBasicAccountingInformation,
                &mut usage as *mut _ as *mut c_void,
                size_of_val(&usage) as u32,
                ptr::null_mut(),
            ) == 0
            {
                return Err(ProcessFailure::ReapUnconfirmed);
            }
            Ok(usage.ActiveProcesses == 0)
        }
    }
    fn reap(&self) -> Result<(), ProcessFailure> {
        self.terminate()?;
        let started = Instant::now();
        loop {
            if self.no_active_processes()? {
                return Ok(());
            }
            if started.elapsed() > REAP_DEADLINE {
                return Err(ProcessFailure::ReapUnconfirmed);
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Kill-on-close Job 守卫：把一个已经启动的探测进程（`--version` / `--help`）约束进 Job。
/// 守卫 drop 时关闭 Job 句柄，任何幸存的探测进程树一起结束，不残留孤儿进程。
/// 绑定失败（例如进程已属于别的 Job）时返回 None，探测仍照常运行，只是不受 Job 约束。
pub(super) struct ConfineGuard(
    #[expect(dead_code, reason = "RAII guard keeps Job handle alive until Drop")] Option<Job>,
);
pub(super) fn confine_running_process(pid: u32) -> ConfineGuard {
    ConfineGuard(try_confine_running_process(pid))
}
fn try_confine_running_process(pid: u32) -> Option<Job> {
    // SAFETY: PROCESS_SET_QUOTA|PROCESS_TERMINATE 仅用于 Job 绑定与兜底终止；
    // 句柄不可继承，由 Handle 包装器唯一持有并在 drop 时关闭一次。
    let process =
        Handle::from_raw(unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) })
            .ok()?;
    let job = Job::new().ok()?;
    // SAFETY: process 与 job 句柄均被独占持有且在调用期间有效；绑定对子进程原子生效。
    if unsafe { AssignProcessToJobObject(job.0.0, process.0) } == 0 {
        return None;
    }
    Some(job)
}

fn size_of_val<T>(value: &T) -> usize {
    std::mem::size_of_val(value)
}

/// Only child pipe ends are inheritable. A PROC_THREAD_ATTRIBUTE_HANDLE_LIST
/// restricts child handle inheritance to these three handles, even if other
/// components have inheritable device/credential handles open concurrently.
fn pipe(parent_reads: bool) -> Result<(Handle, Handle), ProcessFailure> {
    // SAFETY: CreatePipe initializes both handles and security struct is valid.
    unsafe {
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: ptr::null_mut(),
            bInheritHandle: 1,
        };
        let mut r = ptr::null_mut();
        let mut w = ptr::null_mut();
        if CreatePipe(&mut r, &mut w, &attrs, 0) == 0 {
            return Err(ProcessFailure::PipeFailed);
        }
        let read = Handle(r);
        let write = Handle(w);
        let parent_end = if parent_reads { read.0 } else { write.0 };
        if SetHandleInformation(parent_end, HANDLE_FLAG_INHERIT, 0) == 0 {
            return Err(ProcessFailure::PipeFailed);
        }
        Ok((read, write))
    }
}

/// Quote according to the Windows CRT command-line parser. Task text is never
/// passed as an argv item by the Agent plan: it is written via stdin.
fn quote_windows(argument: &str) -> String {
    if !argument.is_empty() && !argument.chars().any(|ch| ch.is_whitespace() || ch == '"') {
        return argument.into();
    }
    let mut output = String::from("\"");
    let mut slash_count = 0;
    for ch in argument.chars() {
        match ch {
            '\\' => slash_count += 1,
            '"' => {
                output.push_str(&"\\".repeat(slash_count * 2 + 1));
                output.push('"');
                slash_count = 0;
            }
            _ => {
                output.push_str(&"\\".repeat(slash_count));
                slash_count = 0;
                output.push(ch);
            }
        }
    }
    output.push_str(&"\\".repeat(slash_count * 2));
    output.push('"');
    output
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}
fn make_command_line(program: &Path, args: &[String]) -> Result<Vec<u16>, ProcessFailure> {
    let program = program.to_str().ok_or(ProcessFailure::InvalidInput)?;
    let mut line = quote_windows(program);
    for arg in args {
        if arg.contains('\0') {
            return Err(ProcessFailure::InvalidInput);
        }
        line.push(' ');
        line.push_str(&quote_windows(arg));
    }
    Ok(wide(OsStr::new(&line)))
}
pub(crate) fn environment_block(
    env: &BTreeMap<String, String>,
) -> Result<Vec<u16>, ProcessFailure> {
    let mut sorted = env.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|entry| entry.0.to_ascii_lowercase());
    let mut output = Vec::new();
    let mut previous = String::new();
    for (key, value) in sorted {
        // Fail closed: a future caller must never pass inherited MOSS device,
        // MCP, session, provider or cloud-account credentials via environment.
        // The allowed set is shared with the plan builder (voice_agent::CLI_ENV_KEYS)
        // so the two lists cannot drift. CODEX_HOME stays allowed as the authorized
        // login-home reference (the plan never sets it), and the fixture keys exist
        // only in the compiled unit-test binary, never in a release one.
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
        let item = format!("{key}={value}");
        output.extend(OsStr::new(&item).encode_wide());
        output.push(0);
    }
    output.push(0);
    if output.len() == 1 {
        output.push(0);
    }
    Ok(output)
}

/// Attribute list owns only native metadata; the separate Handle wrappers own
/// all pipe/process handles. This prevents use-after-free during CreateProcessW.
struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new(handles: &mut [HANDLE; 3]) -> Result<Self, ProcessFailure> {
        // SAFETY: Windows first provides the required byte length; usize Vec
        // provides >= pointer alignment. Handles outlive CreateProcessW.
        unsafe {
            let mut count = 0usize;
            InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut count);
            if count == 0 {
                return Err(ProcessFailure::StartFailed);
            }
            let items = count.div_ceil(std::mem::size_of::<usize>());
            let mut result = Self {
                storage: vec![0; items],
                initialized: false,
            };
            if InitializeProcThreadAttributeList(result.pointer(), 1, 0, &mut count) == 0 {
                return Err(ProcessFailure::StartFailed);
            }
            result.initialized = true;
            if UpdateProcThreadAttribute(
                result.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_mut_ptr() as *const c_void,
                std::mem::size_of_val(handles),
                ptr::null_mut(),
                ptr::null(),
            ) == 0
            {
                return Err(ProcessFailure::StartFailed);
            }
            Ok(result)
        }
    }
    fn pointer(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr() as *mut c_void
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: initialized on this allocation and never moved within Vec.
            unsafe {
                DeleteProcThreadAttributeList(self.pointer());
            }
        }
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
        // An intentionally conservative redact filter. Do not log raw command
        // output anywhere. This does not purport to detect *all* secrets.
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
fn drain(mut input: File, collector: Arc<Mutex<Output>>) -> io::Result<()> {
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

/// Suspended launch -> atomically inherited pipe-handle list -> Job assignment
/// -> resumed main thread. No child instruction runs before the Job is active.
/// Used by the production JobRunner (voice_agent.rs) and by tests.
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
        || !alive()
        || cancel.load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(ProcessFailure::InvalidInput);
    }
    let executable_wide = wide(executable.as_os_str());
    let cwd_wide = wide(cwd.as_os_str());
    let mut command_line = make_command_line(executable, args)?;
    let env_block = environment_block(env)?;
    let job = Job::new()?;
    let (stdin_read, stdin_write) = pipe(false)?;
    let (stdout_read, stdout_write) = pipe(true)?;
    let (stderr_read, stderr_write) = pipe(true)?;
    let mut inherited = [stdin_read.0, stdout_write.0, stderr_write.0];
    let mut attributes = Attributes::new(&mut inherited)?;
    // SAFETY: all fields are initialized and all pointers remain valid through
    // CreateProcessW. The child is SUSPENDED until Job assignment succeeds.
    let (process, main_thread) = unsafe {
        let mut startup: STARTUPINFOEXW = std::mem::zeroed();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdin_read.0;
        startup.StartupInfo.hStdOutput = stdout_write.0;
        startup.StartupInfo.hStdError = stderr_write.0;
        startup.lpAttributeList = attributes.pointer();
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            executable_wide.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            env_block.as_ptr() as *const c_void,
            cwd_wide.as_ptr(),
            &startup.StartupInfo,
            &mut pi,
        ) == 0
        {
            return Err(ProcessFailure::StartFailed);
        }
        (Handle(pi.hProcess), Handle(pi.hThread))
    };
    // The parent no longer needs inheritable ends. No leaked inheritable handles.
    drop(stdin_read);
    drop(stdout_write);
    drop(stderr_write);
    if unsafe { AssignProcessToJobObject(job.0.0, process.0) } == 0 {
        // Assignment failed: never resume the suspended process.
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(process.0, 1);
        }
        return Err(ProcessFailure::JobFailed);
    }
    // Re-check just before resumption. Cancellation never starts an unbound process.
    if !alive() || cancel.load(std::sync::atomic::Ordering::Acquire) {
        job.reap()?;
        return Ok(ProcessResult {
            finish: Finish::Cancelled,
            output: String::new(),
            truncated: false,
            children_reaped: true,
        });
    }
    if unsafe { ResumeThread(main_thread.0) } == u32::MAX {
        job.reap()?;
        return Err(ProcessFailure::ResumeFailed);
    }
    drop(main_thread);
    drop(attributes);
    let collector = Arc::new(Mutex::new(Output::default()));
    let out = Arc::clone(&collector);
    let err = Arc::clone(&collector);
    // Convert native HANDLEs into owned File objects BEFORE transferring them
    // to threads. Raw Windows HANDLE pointers themselves are not Send.
    let stdout_file = stdout_read.file();
    let stderr_file = stderr_read.file();
    let mut stdin_file = stdin_write.file();
    let stdout = thread::spawn(move || drain(stdout_file, out));
    let stderr = thread::spawn(move || drain(stderr_file, err));
    let input = stdin.to_vec();
    let writer = thread::spawn(move || stdin_file.write_all(&input));
    let started = Instant::now();
    let deadline = timeout.min(Duration::from_secs(MAX_RUN_SECONDS));
    let finish = loop {
        if cancel.load(std::sync::atomic::Ordering::Acquire) || !alive() {
            break Finish::Cancelled;
        }
        if started.elapsed() >= deadline {
            break Finish::TimedOut;
        }
        // SAFETY: process HANDLE is owned until after loop, 40ms bounded wait.
        match unsafe { WaitForSingleObject(process.0, POLL_INTERVAL.as_millis() as u32) } {
            WAIT_OBJECT_0 => {
                let mut code = 1;
                if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 {
                    job.reap()?;
                    return Err(ProcessFailure::WaitFailed);
                }
                break if code == 0 {
                    Finish::Completed
                } else {
                    Finish::ExitFailure(code)
                };
            }
            WAIT_TIMEOUT => {}
            _ => {
                job.reap()?;
                return Err(ProcessFailure::WaitFailed);
            }
        }
    };
    // Even when the parent exits normally, descendants must not survive.
    // This also ensures that inherited stdout/stderr pipes eventually close.
    job.reap()?;
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
