//! Child-process tests use the current Rust test binary as a disposable fixture.
//! No real Claude/Codex model, network, shell, laser or private user files.
use super::*;
use std::{
    io::{Read, Write},
    process::Command,
    sync::atomic::Ordering,
};

fn fixture_environment(workspace: &Path, marker: &Path, mode: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in ["SystemRoot", "WINDIR"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.into(), value);
        }
    }
    env.insert(
        "PATH".into(),
        format!("{}\\System32", std::env::var("SystemRoot").unwrap()),
    );
    env.insert("TEMP".into(), workspace.display().to_string());
    env.insert("TMP".into(), workspace.display().to_string());
    env.insert("NV13_PROCESS_FIXTURE".into(), mode.into());
    env.insert("NV13_FIXTURE_MARKER".into(), marker.display().to_string());
    env
}
fn command_args(test_name: &str) -> Vec<String> {
    vec!["--exact".into(), test_name.into(), "--nocapture".into()]
}
fn self_executable() -> std::path::PathBuf {
    std::env::current_exe().unwrap()
}
fn worker_args() -> Vec<String> {
    command_args("voice_agent::windows_process::tests::fixture_worker")
}
fn run_mode(
    cwd: &Path,
    mode: &str,
    marker: &Path,
    stdin: &[u8],
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<ProcessResult, ProcessFailure> {
    run_fixture_process(
        &self_executable(),
        &worker_args(),
        cwd,
        &fixture_environment(cwd, marker, mode),
        stdin,
        &|| true,
        cancel,
        timeout,
    )
}

/// Runs only in child test process when a fixture-specific environment value
/// was passed explicitly. Called as an ordinary empty test in the parent.
#[test]
fn fixture_worker() {
    let Ok(mode) = std::env::var("NV13_PROCESS_FIXTURE") else {
        return;
    };
    let marker = std::env::var("NV13_FIXTURE_MARKER").unwrap_or_default();
    match mode.as_str() {
        "stdin" => {
            let mut data = String::new();
            std::io::stdin().read_to_string(&mut data).unwrap();
            println!("NV13_STDIN={data}");
            eprintln!("NV13_STDERR_OK");
            eprintln!("Authorization: Bearer fixture-secret-dont-display");
        }
        "flood" => {
            let long = "Z".repeat(MAX_OUTPUT_LINE_BYTES + 200);
            std::io::stdout().write_all(long.as_bytes()).unwrap();
            std::io::stdout().write_all(b"\n").unwrap();
            for _ in 0..85 {
                std::io::stdout().write_all(&vec![b'a'; 4096]).unwrap();
                std::io::stdout().write_all(b"\n").unwrap();
                std::io::stderr().write_all(&vec![b'b'; 4096]).unwrap();
                std::io::stderr().write_all(b"\n").unwrap();
            }
            std::io::stdout().flush().unwrap();
            std::io::stderr().flush().unwrap();
        }
        "wait" => {
            std::fs::write(&marker, b"RUNNING").unwrap();
            println!("NV13_WAIT");
            std::thread::sleep(Duration::from_secs(18));
        }
        "spawn" => {
            #[expect(
                clippy::zombie_processes,
                reason = "孙进程由被测的作业对象清理负责回收"
            )]
            let child = Command::new(self_executable())
                .args(command_args(
                    "voice_agent::windows_process::tests::fixture_grandchild",
                ))
                .env("NV13_PROCESS_FIXTURE", "grandchild")
                .env("NV13_FIXTURE_MARKER", &marker)
                .spawn()
                .unwrap();
            std::fs::write(&marker, child.id().to_string()).unwrap();
            println!("NV13_GRANDCHILD_PID={}", child.id());
            std::thread::sleep(Duration::from_secs(18));
        }
        "limit" => {
            let mut spawned = 0;
            let mut denied = 0;
            for _ in 0..12 {
                match Command::new(self_executable())
                    .args(command_args(
                        "voice_agent::windows_process::tests::fixture_grandchild",
                    ))
                    .env("NV13_PROCESS_FIXTURE", "grandchild")
                    .env("NV13_FIXTURE_MARKER", &marker)
                    .spawn()
                {
                    Ok(_child) => spawned += 1,
                    Err(_) => denied += 1,
                }
            }
            println!("NV13_PROCESS_LIMIT spawned={spawned} denied={denied}");
        }
        _ => panic!("invalid internal fixture mode"),
    }
}
#[test]
fn fixture_grandchild() {
    if std::env::var("NV13_PROCESS_FIXTURE").ok().as_deref() != Some("grandchild") {
        return;
    }
    std::thread::sleep(Duration::from_secs(18));
}

#[test]
fn windows_argument_quoting_and_environment_validation() {
    assert_eq!(quote_windows("hello world"), "\"hello world\"");
    assert_eq!(quote_windows(""), "\"\"");
    assert_eq!(quote_windows("x\\y"), "x\\y");
    assert_eq!(quote_windows("space \\"), "\"space \\\\\"");
    assert_eq!(quote_windows("a\"b"), "\"a\\\"b\"");
    let mut env = BTreeMap::new();
    env.insert("PATH".into(), "x".into());
    env.insert("Path".into(), "y".into());
    assert_eq!(environment_block(&env), Err(ProcessFailure::InvalidInput));
    env.remove("Path");
    assert!(environment_block(&env).unwrap().ends_with(&[0, 0]));
    for name in [
        "MOSS_SESSION_TOKEN",
        "MCP_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "RUST_LOG",
    ] {
        env.insert(name.to_owned(), "fixture-secret".into());
        assert_eq!(
            environment_block(&env),
            Err(ProcessFailure::InvalidInput),
            "unexpected allowed key {name}"
        );
        env.remove(name);
    }
}

#[test]
fn bounded_stdout_stderr_and_stdin_are_isolated_from_argv() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("agent-workspace");
    std::fs::create_dir(&cwd).unwrap();
    let marker = root.path().join("ignored-marker");
    let cancel = AtomicBool::new(false);
    let data = b"hello \"quoted\" $(Remove-Item fake)\n";
    let result = run_mode(
        &cwd,
        "stdin",
        &marker,
        data,
        &cancel,
        Duration::from_secs(8),
    )
    .unwrap();
    assert_eq!(result.finish, Finish::Completed, "{result:?}");
    assert!(result.output.contains("hello \"quoted\""));
    assert!(result.output.contains("NV13_STDERR_OK"));
    assert!(!result.output.contains("fixture-secret-dont-display"));
    assert!(result.output.contains("[redacted]"));
    assert!(result.children_reaped);

    let large = run_mode(
        &cwd,
        "flood",
        &marker,
        b"",
        &cancel,
        Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(large.finish, Finish::Completed, "{large:?}");
    assert!(large.truncated);
    assert!(large.output.len() <= MAX_OUTPUT_BYTES);
    assert!(
        large
            .output
            .lines()
            .all(|line| line.len() <= MAX_OUTPUT_LINE_BYTES)
    );
    assert!(large.children_reaped);
}

#[test]
fn timeout_terminates_job_without_survivors() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("agent-workspace");
    std::fs::create_dir(&cwd).unwrap();
    let marker = root.path().join("marker");
    let started = Instant::now();
    let result = run_mode(
        &cwd,
        "wait",
        &marker,
        b"",
        &AtomicBool::new(false),
        Duration::from_millis(350),
    )
    .unwrap();
    assert_eq!(result.finish, Finish::TimedOut, "{result:?}");
    assert!(result.children_reaped);
    assert!(started.elapsed() < Duration::from_secs(9));
}

#[test]
fn cancellation_kills_descendant_not_just_parent() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("agent-workspace");
    std::fs::create_dir(&cwd).unwrap();
    let marker = root.path().join("grandchild-pid");
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancel.clone();
    let marker_worker = marker.clone();
    let signal = std::thread::spawn(move || {
        for _ in 0..220 {
            if let Ok(pid) = std::fs::read_to_string(&marker_worker)
                && pid.trim().parse::<u32>().is_ok()
            {
                cancel_worker.store(true, Ordering::Release);
                return;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    });
    let result = run_mode(
        &cwd,
        "spawn",
        &marker,
        b"",
        &cancel,
        Duration::from_secs(10),
    )
    .unwrap();
    signal.join().unwrap();
    assert_eq!(result.finish, Finish::Cancelled, "{result:?}");
    assert!(result.children_reaped);
    let grandchild: u32 = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
    // All members have been reaped, including this grandchild. OpenProcess
    // may return null after the handle has fully disappeared, also success.
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    const SYNCHRONIZE: u32 = 0x0010_0000;
    // SAFETY: read-only process query using fixture's observed PID; no process is modified.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
            0,
            grandchild,
        )
    };
    if !handle.is_null() {
        // Kernel Job accounting can reach zero slightly before all process
        // handles become signaled. Require actual child exit within 3 seconds.
        let wait = unsafe { WaitForSingleObject(handle, 3000) };
        unsafe {
            CloseHandle(handle);
        }
        assert_eq!(
            wait, WAIT_OBJECT_0,
            "grandchild PID {grandchild} survived the Job cancellation"
        );
    }
}

#[test]
fn revoked_voice_permit_interrupts_waiting_fixture() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("agent-workspace");
    std::fs::create_dir(&cwd).unwrap();
    let marker = root.path().join("permit-marker");
    let permit = Arc::new(AtomicBool::new(true));
    let observed = permit.clone();
    let check = permit.clone();
    let notify = marker.clone();
    let watcher = std::thread::spawn(move || {
        for _ in 0..200 {
            if notify.exists() {
                observed.store(false, Ordering::Release);
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let result = run_fixture_process(
        &self_executable(),
        &worker_args(),
        &cwd,
        &fixture_environment(&cwd, &marker, "wait"),
        b"",
        &|| check.load(Ordering::Acquire),
        &AtomicBool::new(false),
        Duration::from_secs(10),
    )
    .unwrap();
    watcher.join().unwrap();
    assert_eq!(result.finish, Finish::Cancelled, "{result:?}");
    assert!(result.children_reaped);
}

#[test]
fn permission_change_between_create_and_resume_never_runs_code() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("agent-workspace");
    std::fs::create_dir(&cwd).unwrap();
    let marker = root.path().join("should-not-exist");
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let alive = || checks.fetch_add(1, Ordering::SeqCst) == 0;
    let result = run_fixture_process(
        &self_executable(),
        &worker_args(),
        &cwd,
        &fixture_environment(&cwd, &marker, "wait"),
        b"",
        &alive,
        &AtomicBool::new(false),
        Duration::from_secs(6),
    )
    .unwrap();
    assert_eq!(result.finish, Finish::Cancelled);
    assert!(result.children_reaped);
    assert!(
        !marker.exists(),
        "suspended child unexpectedly executed fixture code"
    );
}

#[test]
fn dropping_job_handle_terminates_test_child() {
    // Fixture-only demonstration of KILL_ON_JOB_CLOSE. The actual harness
    // uses CREATE_SUSPENDED before assignment, avoiding this test-only race.
    let job = Job::new().unwrap();
    let mut child = Command::new(self_executable())
        .args(command_args(
            "voice_agent::windows_process::tests::fixture_grandchild",
        ))
        .env("NV13_PROCESS_FIXTURE", "grandchild")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    use std::os::windows::io::AsRawHandle;
    let assigned = unsafe { AssignProcessToJobObject(job.0.0, child.as_raw_handle()) };
    if assigned == 0 {
        let _ = child.kill();
        let _ = child.wait();
        panic!("Windows cannot assign the fixture to the Job Object");
    }
    assert!(child.try_wait().unwrap().is_none());
    drop(job);
    let wait = unsafe { WaitForSingleObject(child.as_raw_handle(), 3000) };
    if wait != WAIT_OBJECT_0 {
        let _ = child.kill();
    }
    let _ = child.wait();
    assert_eq!(
        wait, WAIT_OBJECT_0,
        "closing Job handle failed to terminate fixture"
    );
}

#[test]
fn active_process_limit_rejects_ninth_member() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("agent-workspace");
    std::fs::create_dir(&cwd).unwrap();
    let result = run_mode(
        &cwd,
        "limit",
        &root.path().join("unused"),
        b"",
        &AtomicBool::new(false),
        Duration::from_secs(12),
    )
    .unwrap();
    assert_eq!(result.finish, Finish::Completed, "{result:?}");
    assert!(result.children_reaped);
    let line = result
        .output
        .lines()
        .find(|s| s.contains("NV13_PROCESS_LIMIT"))
        .unwrap();
    let spawned = line
        .split("spawned=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let denied = line
        .split("denied=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert!(spawned <= 7 && denied >= 5, "{line}");
    assert_eq!(spawned + denied, 12);
}
