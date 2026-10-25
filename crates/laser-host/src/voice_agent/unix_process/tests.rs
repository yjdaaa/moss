//! Unix / macOS child-process tests using the current Rust test binary as a disposable fixture.
//! No real Claude/Codex model, network, shell, laser or private user files.
use super::*;
use std::{
    io::{Read, Write},
    process::Command,
    sync::atomic::Ordering,
};

fn fixture_environment(workspace: &Path, marker: &Path, mode: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PATH".into(), "/usr/bin:/bin".into());
    env.insert("TMPDIR".into(), workspace.display().to_string());
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
    command_args("voice_agent::unix_process::tests::fixture_worker")
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
        "stdin_gate" => {
            let mut data = String::new();
            std::io::stdin().read_to_string(&mut data).unwrap();
            if !data.is_empty() {
                std::fs::write(&marker, data.as_bytes()).unwrap();
            }
        }
        "spawn" => {
            let child = Command::new(self_executable())
                .args(command_args(
                    "voice_agent::unix_process::tests::fixture_grandchild",
                ))
                .env("NV13_PROCESS_FIXTURE", "grandchild")
                .env("NV13_FIXTURE_MARKER", &marker)
                .spawn()
                .unwrap();
            std::fs::write(&marker, child.id().to_string()).unwrap();
            println!("NV13_GRANDCHILD_PID={}", child.id());
            std::thread::sleep(Duration::from_secs(18));
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
fn unix_environment_validation_rejects_non_whitelisted_keys() {
    let mut env = BTreeMap::new();
    env.insert("PATH".into(), "/usr/bin:/bin".into());
    env.insert("Path".into(), "/bin".into());
    assert_eq!(validate_environment(&env), Err(ProcessFailure::InvalidInput));
    env.remove("Path");
    assert!(validate_environment(&env).is_ok());
    for name in [
        "MOSS_SESSION_TOKEN",
        "MCP_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "RUST_LOG",
    ] {
        env.insert(name.to_owned(), "fixture-secret".into());
        assert_eq!(
            validate_environment(&env),
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
    let data = b"hello \"quoted\" $(rm -rf fake)\n";
    let result = run_mode(&cwd, "stdin", &marker, data, &cancel, Duration::from_secs(8)).unwrap();
    assert_eq!(result.finish, Finish::Completed, "{result:?}");
    assert!(result.output.contains("hello \"quoted\""));
    assert!(result.output.contains("NV13_STDERR_OK"));
    assert!(!result.output.contains("fixture-secret-dont-display"));
    assert!(result.output.contains("[redacted]"));
    assert!(result.children_reaped);

    let large = run_mode(&cwd, "flood", &marker, b"", &cancel, Duration::from_secs(10)).unwrap();
    assert_eq!(large.finish, Finish::Completed, "{large:?}");
    assert!(large.truncated);
    assert!(large.output.len() <= MAX_OUTPUT_BYTES);
    assert!(large.output.lines().all(|line| line.len() <= MAX_OUTPUT_LINE_BYTES));
    assert!(large.children_reaped);
}

#[test]
fn timeout_terminates_process_group_without_survivors() {
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
            if let Ok(pid) = std::fs::read_to_string(&marker_worker) {
                if pid.trim().parse::<u32>().is_ok() {
                    cancel_worker.store(true, Ordering::Release);
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    });
    let result = run_mode(&cwd, "spawn", &marker, b"", &cancel, Duration::from_secs(10)).unwrap();
    signal.join().unwrap();
    assert_eq!(result.finish, Finish::Cancelled, "{result:?}");
    assert!(result.children_reaped);
    let grandchild: i32 = std::fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
    let probe = unsafe { libc::kill(grandchild, 0) };
    assert_eq!(probe, -1, "grandchild PID {grandchild} survived process-group cancellation");
}

#[test]
fn permission_change_before_stdin_delivery_cancels_without_sending_task() {
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
        &fixture_environment(&cwd, &marker, "stdin_gate"),
        b"task payload",
        &alive,
        &AtomicBool::new(false),
        Duration::from_secs(6),
    )
    .unwrap();
    assert_eq!(result.finish, Finish::Cancelled);
    assert!(result.children_reaped);
    assert!(!marker.exists(), "revoked run unexpectedly delivered task stdin");
}
