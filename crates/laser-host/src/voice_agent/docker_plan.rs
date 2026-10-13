//! NV13 Docker launcher PLAN for an OFFLINE model-free Codex probe.
//!
//! No Docker execution is exported to AgentRunner. The plan cannot accept a
//! user workspace, task, shell fragment, model credential or network name.
//! It only mounts one temporary synthetic canary in a read-only directory.
#![allow(dead_code)]
use getrandom::fill;
use std::{ffi::OsString, fs, path::Path};

const IMAGE_ID: &str = "sha256:baa32539eac9ecca7831bac5848c0ecab8a0a84986c5822a4d371fffa7d435ac";
const CANARY_FILE: &str = "nv13-offline-canary.txt";
const CANARY_BYTES: &[u8] = b"NV13_SYNTHETIC_CANARY";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Version,
    MainHelp,
    ExecHelp,
}
impl Probe {
    fn argv(self) -> &'static [&'static str] {
        match self {
            Self::Version => &["--version"],
            Self::MainHelp => &["--help"],
            Self::ExecHelp => &["exec", "--help"],
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PlanDenied {
    WrongImage,
    UnsafeWorkspace,
    UnsafePath,
    RandomUnavailable,
}

struct DockerProbePlan {
    name: String,
    argv: Vec<OsString>,
}
impl DockerProbePlan {
    fn build(synthetic: &Path, image: &str, probe: Probe) -> Result<Self, PlanDenied> {
        if image != IMAGE_ID {
            return Err(PlanDenied::WrongImage);
        }
        let workspace = synthetic
            .canonicalize()
            .map_err(|_| PlanDenied::UnsafeWorkspace)?;
        if !workspace.is_dir() {
            return Err(PlanDenied::UnsafeWorkspace);
        }
        let root = workspace.parent().ok_or(PlanDenied::UnsafeWorkspace)?;
        let root_name = root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if !root_name.starts_with("nv13-codex-fixture-")
            || root.parent()
                != Some(
                    std::env::temp_dir()
                        .canonicalize()
                        .map_err(|_| PlanDenied::UnsafeWorkspace)?
                        .as_path(),
                )
            || workspace.file_name().and_then(|value| value.to_str()) != Some("workspace")
        {
            return Err(PlanDenied::UnsafeWorkspace);
        }
        // Reparse points/junctions/symlinks are not admitted at staging root,
        // workspace or synthetic canary, even if their canonical target is safe.
        for target in [root, synthetic, &synthetic.join(CANARY_FILE)] {
            let metadata = fs::symlink_metadata(target).map_err(|_| PlanDenied::UnsafeWorkspace)?;
            if metadata.file_type().is_symlink() {
                return Err(PlanDenied::UnsafeWorkspace);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(PlanDenied::UnsafeWorkspace);
                }
            }
        }
        let mut entries = fs::read_dir(&workspace).map_err(|_| PlanDenied::UnsafeWorkspace)?;
        let entry = entries
            .next()
            .ok_or(PlanDenied::UnsafeWorkspace)?
            .map_err(|_| PlanDenied::UnsafeWorkspace)?;
        if entries.next().is_some()
            || entry.file_name() != CANARY_FILE
            || !entry
                .file_type()
                .map_err(|_| PlanDenied::UnsafeWorkspace)?
                .is_file()
            || fs::read(entry.path()).map_err(|_| PlanDenied::UnsafeWorkspace)? != CANARY_BYTES
        {
            return Err(PlanDenied::UnsafeWorkspace);
        }
        let canonical_path = workspace.to_str().ok_or(PlanDenied::UnsafePath)?;
        // Windows canonicalize returns an extended-length \\?\ path which
        // Docker Desktop does not accept. Strip only that presentation prefix,
        // after the real path and reparse-point checks have already passed.
        // Reject UNC/device paths; accept only an absolute drive-letter path.
        #[cfg(windows)]
        let path = {
            let normal = canonical_path
                .strip_prefix("\\\\?\\")
                .unwrap_or(canonical_path);
            let bytes = normal.as_bytes();
            if bytes.len() < 4
                || !bytes[0].is_ascii_alphabetic()
                || bytes[1] != b':'
                || bytes[2] != b'\\'
            {
                return Err(PlanDenied::UnsafePath);
            }
            normal
        };
        #[cfg(not(windows))]
        let path = canonical_path;
        // Docker --mount uses commas to separate sub-options. Never allow
        // path data to inject a second mount/target.
        if path.contains([',', '\r', '\n', '\0']) {
            return Err(PlanDenied::UnsafePath);
        }
        let mut random = [0u8; 16];
        fill(&mut random).map_err(|_| PlanDenied::RandomUnavailable)?;
        let name = format!(
            "nv13-codex-probe-{}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        // Each flag is passed as one literal argv member, never via cmd /c,
        // PowerShell, bash or a user-supplied Docker subcommand.
        let mut args = vec![
            "run".into(),
            "--rm".into(),
            "--pull".into(),
            "never".into(),
            "--name".into(),
            name.clone().into(),
            "--label".into(),
            format!("nv13.fixture.id={name}").into(),
            "--network".into(),
            "none".into(),
            "--read-only".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--pids-limit".into(),
            "8".into(),
            "--memory".into(),
            "256m".into(),
            "--cpus".into(),
            "0.5".into(),
            "--user".into(),
            "65534:65534".into(),
            "--env".into(),
            "HOME=/nonexistent".into(),
            "--env".into(),
            "CODEX_HOME=/nonexistent".into(),
            "--mount".into(),
            format!("type=bind,source={path},target=/workspace,readonly").into(),
            "--workdir".into(),
            "/workspace".into(),
            "--entrypoint".into(),
            "/usr/local/bin/codex".into(),
            image.into(),
        ];
        args.extend(probe.argv().iter().map(OsString::from));
        Ok(Self { name, argv: args })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::Builder;

    fn fixture() -> tempfile::TempDir {
        let dir = Builder::new()
            .prefix("nv13-codex-fixture-")
            .tempdir()
            .unwrap();
        fs::create_dir(dir.path().join("workspace")).unwrap();
        fs::write(dir.path().join("workspace").join(CANARY_FILE), CANARY_BYTES).unwrap();
        dir
    }
    fn text(plan: &DockerProbePlan) -> Vec<String> {
        plan.argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }
    #[test]
    fn only_digest_pinned_minimal_no_network_no_secrets_probe_plan() {
        let root = fixture();
        let p = DockerProbePlan::build(&root.path().join("workspace"), IMAGE_ID, Probe::Version)
            .unwrap();
        let a = text(&p);
        assert!(p.name.starts_with("nv13-codex-probe-"));
        assert_eq!(p.name.len(), "nv13-codex-probe-".len() + 32);
        assert_eq!(&a[0..4], &["run", "--rm", "--pull", "never"]);
        for pair in [
            ["--network", "none"],
            ["--cap-drop", "ALL"],
            ["--security-opt", "no-new-privileges"],
            ["--pids-limit", "8"],
            ["--memory", "256m"],
            ["--user", "65534:65534"],
        ] {
            assert!(a.windows(2).any(|window| window == pair), "{pair:?}");
        }
        assert!(a.contains(&"--read-only".into()));
        assert!(a.contains(&"--mount".into()));
        assert!(a.contains(&IMAGE_ID.into()));
        assert_eq!(a.last().unwrap(), "--version");
        assert!(!a.iter().any(|v| v.contains("codex exec -")
            || v.contains("--privileged")
            || v.contains("seccomp=unconfined")
            || v.contains("SYS_ADMIN")
            || v.contains("ANTHROPIC_API_KEY")
            || v.contains("OPENAI_API_KEY")));
    }
    #[test]
    fn no_mutable_image_names_or_user_workspaces_can_be_selected() {
        let root = fixture();
        let workspace = root.path().join("workspace");
        for image in [
            "moss-nv13-codex-test:0.162.1",
            "ubuntu:latest",
            "sha256:00",
            "docker.io/openai/codex:latest",
            "",
            "sha256:baa32539eac9ecca",
        ] {
            assert_eq!(
                DockerProbePlan::build(&workspace, image, Probe::Version).err(),
                Some(PlanDenied::WrongImage)
            );
        }
        assert_eq!(
            DockerProbePlan::build(root.path(), IMAGE_ID, Probe::Version).err(),
            Some(PlanDenied::UnsafeWorkspace)
        );
        let outside = tempfile::tempdir().unwrap();
        assert_eq!(
            DockerProbePlan::build(outside.path(), IMAGE_ID, Probe::Version).err(),
            Some(PlanDenied::UnsafeWorkspace)
        );
        fs::write(workspace.join("secrets.env"), "fake_secret=value").unwrap();
        assert_eq!(
            DockerProbePlan::build(&workspace, IMAGE_ID, Probe::Version).err(),
            Some(PlanDenied::UnsafeWorkspace)
        );
    }

    /// Explicit opt-in: Docker runs pinned Codex --version, never a model call.
    /// Only synthetic canary is mounted and Docker networking stays disabled.
    #[cfg(windows)]
    #[test]
    #[ignore = "explicit opt-in: creates no-network synthetic Docker container"]
    fn actual_pinned_docker_probe_without_model_or_credentials() {
        use std::process::Command;
        let docker = std::env::var("NV13_DOCKER_TEST_EXE")
            .expect("NV13_DOCKER_TEST_EXE absolute docker.exe path required");
        assert!(Path::new(&docker).is_absolute() && Path::new(&docker).is_file());
        let dir = fixture();
        let plan = DockerProbePlan::build(&dir.path().join("workspace"), IMAGE_ID, Probe::Version)
            .unwrap();
        struct OwnedContainer<'a> {
            docker: &'a str,
            name: String,
        }
        impl Drop for OwnedContainer<'_> {
            fn drop(&mut self) {
                let result = Command::new(self.docker)
                    .args([
                        "inspect",
                        "--format",
                        "{{index .Config.Labels \"nv13.fixture.id\"}}",
                        &self.name,
                    ])
                    .output();
                if let Ok(result) = result {
                    if result.status.success()
                        && String::from_utf8_lossy(&result.stdout).trim() == self.name
                    {
                        let _ = Command::new(self.docker)
                            .args(["rm", "--force", &self.name])
                            .output();
                    }
                }
            }
        }
        let _cleanup = OwnedContainer {
            docker: &docker,
            name: plan.name.clone(),
        };
        let mut command = Command::new(&docker);
        command.env_clear();
        for name in [
            "SystemRoot",
            "WINDIR",
            "PATH",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "PROGRAMDATA",
            "TEMP",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let result = command
            .args(&plan.argv)
            .output()
            .expect("Docker CLI launch failed");
        let stdout = String::from_utf8_lossy(&result.stdout);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            result.status.success(),
            "Docker fixture failed: {stdout} {stderr}"
        );
        assert!(stdout.contains("codex-cli 0.162.1"), "{stdout}");
        assert!(
            !stdout.contains("model response") && !stdout.contains("NV13_FAKE_MODEL_REPLY_ONLY")
        );
    }
    #[test]
    fn only_offline_non_model_probe_argv_is_generated() {
        let root = fixture();
        let workspace = root.path().join("workspace");
        for (probe, tail) in [
            (Probe::Version, vec!["--version"]),
            (Probe::MainHelp, vec!["--help"]),
            (Probe::ExecHelp, vec!["exec", "--help"]),
        ] {
            let plan = DockerProbePlan::build(&workspace, IMAGE_ID, probe).unwrap();
            let args = text(&plan);
            assert!(args.ends_with(&tail.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
            assert!(
                !args
                    .iter()
                    .any(|v| v == "-" || v == "--dangerously-bypass-approvals-and-sandbox")
            );
        }
    }
}
