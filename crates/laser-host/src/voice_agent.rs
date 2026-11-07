//! NV13 Agent CLI policy, cross-platform CLI auto-discovery, and execution plan.
//! 2026-10-15 用户决定：允许在选定工作目录内修改文件；只在严格模式开启时运行（Agent 即使调用
//! 本机 MOSS 接口也动不了激光机），每次运行都要桌面批准。CLI 版本不再锁死，但必须具备下列安全参数。
//! 2026-10-25 用户决定：开通 macOS / Unix 进程组隔离运行器，并支持本机自动读取已安装的 Codex / Claude Code（换设备或升级版本自动适配）。
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

// NV13 Windows suspended-spawn + Job Object harness.
#[cfg(windows)]
mod windows_process;
// NV13 Unix / macOS process-group + pre-exec gate harness.
#[cfg(unix)]
mod unix_process;
// Offline fake-upstream HTTP request policy. Not a production model proxy.
#[cfg(test)]
mod model_proxy;
// Digest-pinned, synthetic-only Docker invocation plan; not production runner.
#[cfg(test)]
mod docker_plan;
mod engine;
pub use engine::{AgentCoordinator, AgentRunner, CapabilitySnapshot};

pub const MAX_TASK_CHARS: usize = 4000;
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
pub const MAX_OUTPUT_LINE_BYTES: usize = 64 * 1024;
pub const MAX_RUN_SECONDS: u64 = 300;
pub const MAX_RUN_RECORDS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    ClaudeCode,
}
impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude_code",
        }
    }
    pub fn expected_binary(self) -> &'static str {
        if cfg!(windows) {
            match self {
                Self::Codex => "codex.exe",
                Self::ClaudeCode => "claude.exe",
            }
        } else {
            match self {
                Self::Codex => "codex",
                Self::ClaudeCode => "claude",
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliSource {
    Configured,
    Auto,
    AutoFallback,
}
impl CliSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Auto => "auto",
            Self::AutoFallback => "auto_fallback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentError {
    Disabled,
    PathRejected,
    CapabilityMissing,
    TaskInvalid,
    SandboxUnverified,
    ConfigChanged,
    PermissionExpired,
    AlreadyUsed,
    StrictModeRequired,
    RunFailed,
    TimedOut,
}
impl AgentError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Disabled => "agent_disabled",
            Self::PathRejected => "agent_path_denied",
            Self::CapabilityMissing => "agent_capability_missing",
            Self::TaskInvalid => "agent_task_invalid",
            Self::SandboxUnverified => "agent_sandbox_unavailable",
            Self::ConfigChanged => "agent_config_changed",
            Self::PermissionExpired => "agent_permission_expired",
            Self::AlreadyUsed => "agent_approval_consumed",
            Self::StrictModeRequired => "agent_strict_mode_required",
            Self::RunFailed => "agent_run_failed",
            Self::TimedOut => "agent_timeout",
        }
    }
}
/// 帮助文本按 token 边界确认安全参数：token 只保留字母数字与 `-` / `_` / `.`，
/// `--tools` 不得被 `--tools-legacy` / `--permission-tools` 这类改名或更长前缀冒名顶替
/// （2026-10-17 审查发现旧实现用子串匹配）。
fn lists_flag(help: &str, flag: &str) -> bool {
    help.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
        .any(|token| token == flag)
}
/// Help text must list every safety flag the plan relies on. The version is recorded in the
/// plan and approval but no longer pinned (2026-10-15): a CLI missing a flag is refused.
pub fn parse_help(provider: Provider, version: &str, help: &str) -> Result<(), AgentError> {
    let version = version.trim();
    let named = match provider {
        Provider::Codex => version.starts_with("codex-cli "),
        Provider::ClaudeCode => {
            version.contains("Claude Code")
                || version
                    .split_whitespace()
                    .next()
                    .is_some_and(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        }
    };
    if !named || version.len() > 128 {
        return Err(AgentError::CapabilityMissing);
    }
    let flags: &[&str] = match provider {
        Provider::Codex => &[
            "--ask-for-approval",
            "--sandbox",
            "--ignore-user-config",
            "--ignore-rules",
            "--ephemeral",
            "--skip-git-repo-check",
            "--cd",
            "--json",
            "--color",
        ],
        Provider::ClaudeCode => &[
            "--print",
            "--restricted",
            "--permission-mode",
            "--permission-prompts",
            "--tools",
            "--strict-mcp-config",
            "--mcp-config",
            "--setting-sources",
            "--no-session-persistence",
            "--output-format",
            "--verbose",
        ],
    };
    if flags.iter().any(|flag| !lists_flag(help, flag)) {
        return Err(AgentError::CapabilityMissing);
    }
    Ok(())
}
/// Canonicalize existing, explicitly selected workspaces. Do not follow
/// symlinks or junctions, or allow the workspace above/below protected roots.
pub fn workspace(path: &Path, sensitive_roots: &[PathBuf]) -> Result<PathBuf, AgentError> {
    if !path.is_absolute() {
        return Err(AgentError::PathRejected);
    }
    let canonical = path.canonicalize().map_err(|_| AgentError::PathRejected)?;
    if !canonical.is_dir() || canonical.parent().is_none() {
        return Err(AgentError::PathRejected);
    }
    for root in sensitive_roots {
        let protected = root.canonicalize().map_err(|_| AgentError::PathRejected)?;
        if canonical.starts_with(&protected) || protected.starts_with(&canonical) {
            return Err(AgentError::PathRejected);
        }
    }
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"))
        && let Ok(home) = PathBuf::from(home).canonicalize()
        && (canonical == home || home.starts_with(&canonical))
    {
        return Err(AgentError::PathRejected);
    }
    for key in ["SystemRoot", "WINDIR"] {
        if let Ok(value) = std::env::var(key)
            && let Ok(root) = PathBuf::from(value).canonicalize()
            && (canonical.starts_with(&root) || root.starts_with(&canonical))
        {
            return Err(AgentError::PathRejected);
        }
    }
    let forbidden = [
        ".env",
        ".mcp.json",
        ".claude",
        ".codex",
        ".ssh",
        "app_secrets",
        "secrets",
        "moss",
        "moss-rust",
        "program files",
        "windows",
        "system32",
        "system",
        "library",
        "applications",
    ];
    if canonical.components().any(|component| match component {
        Component::Normal(part) => {
            forbidden.contains(&part.to_string_lossy().to_ascii_lowercase().as_str())
        }
        _ => false,
    }) {
        return Err(AgentError::PathRejected);
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        #[cfg(target_os = "macos")]
        if current == Path::new("/var") || current == Path::new("/tmp") {
            continue;
        }
        let meta = std::fs::symlink_metadata(&current).map_err(|_| AgentError::PathRejected)?;
        if meta.file_type().is_symlink() {
            return Err(AgentError::PathRejected);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(AgentError::PathRejected);
            }
        }
    }
    Ok(canonical)
}

fn is_script_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|s| s.to_str())
        && matches!(
            ext.to_ascii_lowercase().as_str(),
            "cmd" | "bat" | "ps1" | "sh" | "bash" | "zsh" | "js" | "mjs" | "cjs" | "py" | "lnk"
        )
    {
        return true;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return true;
    };
    let mut head = [0u8; 2];
    matches!(file.read(&mut head),Ok(2) if head==*b"#!")
}

#[cfg_attr(not(unix), allow(dead_code))]
fn is_claude_versioned_binary(canonical: &Path) -> bool {
    let parts: Vec<_> = canonical.components().rev().take(3).collect();
    if parts.len() < 3 {
        return false;
    }
    let (Component::Normal(ver), Component::Normal(versions), Component::Normal(claude)) =
        (parts[0], parts[1], parts[2])
    else {
        return false;
    };
    let Some(ver) = ver.to_str() else {
        return false;
    };
    claude.to_string_lossy().eq_ignore_ascii_case("claude")
        && versions.to_string_lossy().eq_ignore_ascii_case("versions")
        && ver.chars().next().is_some_and(|c| c.is_ascii_digit())
        && ver
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

fn codex_platform_specs(os: &str, arch: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    match (os, arch) {
        ("windows", "aarch64") => vec![
            (
                "@openai/codex-win32-arm64",
                "aarch64-pc-windows-msvc",
                "codex.exe",
            ),
            (
                "@openai/codex-win32-x64",
                "x86_64-pc-windows-msvc",
                "codex.exe",
            ),
        ],
        ("windows", _) => vec![(
            "@openai/codex-win32-x64",
            "x86_64-pc-windows-msvc",
            "codex.exe",
        )],
        ("macos", "aarch64") => vec![
            (
                "@openai/codex-darwin-arm64",
                "aarch64-apple-darwin",
                "codex",
            ),
            ("@openai/codex-darwin-x64", "x86_64-apple-darwin", "codex"),
        ],
        ("macos", _) => vec![("@openai/codex-darwin-x64", "x86_64-apple-darwin", "codex")],
        (_, "aarch64") => vec![(
            "@openai/codex-linux-arm64",
            "aarch64-unknown-linux-musl",
            "codex",
        )],
        _ => vec![(
            "@openai/codex-linux-x64",
            "x86_64-unknown-linux-musl",
            "codex",
        )],
    }
}

fn codex_vendor_candidates_under(node_modules: &Path, os: &str, arch: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (pkg, triple, bin) in codex_platform_specs(os, arch) {
        let pkg_parts: Vec<&str> = pkg.split('/').collect();
        let hoisted = pkg_parts
            .iter()
            .fold(node_modules.to_path_buf(), |acc, p| acc.join(p));
        let nested = pkg_parts.iter().fold(
            node_modules
                .join("@openai")
                .join("codex")
                .join("node_modules"),
            |acc, p| acc.join(p),
        );
        let direct = node_modules.join("@openai").join("codex");
        for root in [hoisted, nested, direct] {
            out.push(root.join("vendor").join(triple).join("bin").join(bin));
            out.push(root.join("vendor").join(triple).join("codex").join(bin));
        }
    }
    out
}

#[cfg(unix)]
fn resolve_unix_codex_wrapper(path: &Path) -> Option<PathBuf> {
    // Support `/opt/homebrew/bin/codex` or `~/.nvm/.../bin/codex` symlinked to `@openai/codex/bin/codex.js`.
    let mut roots = Vec::new();
    if let Some(bin_dir) = path.parent() {
        roots.push(bin_dir.join("node_modules"));
        if let Some(prefix) = bin_dir.parent() {
            roots.push(prefix.join("lib").join("node_modules"));
        }
    }
    if let Ok(real) = path.canonicalize() {
        // `<node_modules>/@openai/codex/bin/codex.js` -> parent x3 is `<node_modules>`
        if let Some(pkg_dir) = real.parent().and_then(Path::parent) {
            roots.push(pkg_dir.join("node_modules"));
            if let Some(nm) = pkg_dir.parent().and_then(Path::parent) {
                roots.push(nm.to_path_buf());
            }
        }
    }
    for nm in roots {
        for candidate in
            codex_vendor_candidates_under(&nm, std::env::consts::OS, std::env::consts::ARCH)
        {
            if let Ok(canonical) = candidate.canonicalize()
                && let Ok(meta) = std::fs::symlink_metadata(&canonical)
                && meta.is_file()
                && !meta.file_type().is_symlink()
                && !is_script_file(&canonical)
            {
                return Some(canonical);
            }
        }
    }
    None
}

pub fn executable(path: &Path, provider: Provider) -> Result<PathBuf, AgentError> {
    if !path.is_absolute() {
        return Err(AgentError::PathRejected);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path).map_err(|_| AgentError::PathRejected)?;
        if meta.file_type().is_symlink() || !meta.is_file() || meta.file_attributes() & 0x400 != 0 {
            return Err(AgentError::PathRejected);
        }
        let canonical = path.canonicalize().map_err(|_| AgentError::PathRejected)?;
        let name = canonical.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if !name.eq_ignore_ascii_case(provider.expected_binary()) || is_script_file(&canonical) {
            return Err(AgentError::PathRejected);
        }
        Ok(canonical)
    }
    #[cfg(unix)]
    {
        if provider == Provider::Codex
            && let Some(resolved) = resolve_unix_codex_wrapper(path)
        {
            return Ok(resolved);
        }
        let canonical = path.canonicalize().map_err(|_| AgentError::PathRejected)?;
        let meta = std::fs::symlink_metadata(&canonical).map_err(|_| AgentError::PathRejected)?;
        if meta.file_type().is_symlink() || !meta.is_file() || is_script_file(&canonical) {
            return Err(AgentError::PathRejected);
        }
        let name = canonical.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let name_ok = match provider {
            Provider::Codex => name == "codex" || name.starts_with("codex-"),
            Provider::ClaudeCode => {
                name == "claude"
                    || name.starts_with("claude-")
                    || is_claude_versioned_binary(&canonical)
            }
        };
        if !name_ok {
            return Err(AgentError::PathRejected);
        }
        Ok(canonical)
    }
}

fn collect_sorted_subdirs(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    dirs
}

fn push_unique_dir(dir: PathBuf, out: &mut Vec<PathBuf>) {
    if dir.is_absolute() && !out.iter().any(|existing| existing == &dir) {
        out.push(dir);
    }
}

/// 纯文件系统扫描候选 CLI 路径（不启动任何子进程，耗时毫秒级；支持单元测试注入虚拟目录）。
pub(crate) fn detect_cli_candidates_in(
    provider: Provider,
    os: &str,
    arch: &str,
    path_dirs: &[PathBuf],
    home: Option<&Path>,
    appdata: Option<&Path>,
    local_appdata: Option<&Path>,
    program_files: Option<&Path>,
    unix_extra_bins: &[PathBuf],
) -> Vec<PathBuf> {
    let is_windows = os == "windows";
    let expected = match (is_windows, provider) {
        (true, Provider::Codex) => "codex.exe",
        (true, Provider::ClaudeCode) => "claude.exe",
        (false, Provider::Codex) => "codex",
        (false, Provider::ClaudeCode) => "claude",
    };
    let mut bin_dirs = Vec::new();
    if let Some(home) = home {
        for sub in [
            home.join(".local").join("bin"),
            home.join(".cargo").join("bin"),
            home.join(".bun").join("bin"),
            home.join(".volta").join("bin"),
            home.join(".npm-global").join("bin"),
        ] {
            push_unique_dir(sub, &mut bin_dirs);
        }
    }
    if is_windows {
        if let Some(roaming) = appdata {
            push_unique_dir(roaming.join("npm"), &mut bin_dirs);
            for ver in collect_sorted_subdirs(&roaming.join("fnm").join("node-versions")) {
                push_unique_dir(ver.join("installation"), &mut bin_dirs);
            }
            for ver in collect_sorted_subdirs(&roaming.join("nvm")) {
                push_unique_dir(ver, &mut bin_dirs);
            }
        }
        if let Some(local) = local_appdata {
            push_unique_dir(local.join("pnpm"), &mut bin_dirs);
            push_unique_dir(local.join("Volta").join("bin"), &mut bin_dirs);
            for ver in collect_sorted_subdirs(&local.join("fnm").join("node-versions")) {
                push_unique_dir(ver.join("installation"), &mut bin_dirs);
            }
            for ver in collect_sorted_subdirs(&local.join("nvm")) {
                push_unique_dir(ver, &mut bin_dirs);
            }
        }
        if let Some(pf) = program_files {
            push_unique_dir(pf.join("nodejs"), &mut bin_dirs);
        }
    } else {
        for extra in unix_extra_bins {
            push_unique_dir(extra.clone(), &mut bin_dirs);
        }
        if let Some(home) = home {
            push_unique_dir(home.join(".claude").join("local"), &mut bin_dirs);
            for ver in collect_sorted_subdirs(&home.join(".nvm").join("versions").join("node")) {
                push_unique_dir(ver.join("bin"), &mut bin_dirs);
            }
            for fnm_root in [
                home.join("Library")
                    .join("Application Support")
                    .join("fnm")
                    .join("node-versions"),
                home.join(".local")
                    .join("share")
                    .join("fnm")
                    .join("node-versions"),
            ] {
                for ver in collect_sorted_subdirs(&fnm_root) {
                    push_unique_dir(ver.join("installation").join("bin"), &mut bin_dirs);
                }
            }
            push_unique_dir(home.join("Library").join("pnpm"), &mut bin_dirs);
            push_unique_dir(
                home.join(".local").join("share").join("pnpm"),
                &mut bin_dirs,
            );
        }
    }
    for dir in path_dirs {
        push_unique_dir(dir.clone(), &mut bin_dirs);
    }

    let mut candidates = Vec::new();
    for dir in &bin_dirs {
        candidates.push(dir.join(expected));
        if provider == Provider::Codex {
            for nm in [
                Some(dir.join("node_modules")),
                dir.parent().map(|p| p.join("lib").join("node_modules")),
            ]
            .into_iter()
            .flatten()
            {
                candidates.extend(codex_vendor_candidates_under(&nm, os, arch));
            }
        }
    }
    if !is_windows
        && provider == Provider::ClaudeCode
        && let Some(home) = home
    {
        let versions_dir = home
            .join(".local")
            .join("share")
            .join("claude")
            .join("versions");
        if let Ok(entries) = std::fs::read_dir(&versions_dir) {
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
            candidates.extend(files);
        }
    }
    candidates
}

/// 自动识别当前设备上已安装的官方 Codex / Claude Code 原生可执行文件。
pub fn detect_cli(provider: Provider) -> Option<PathBuf> {
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|raw| std::env::split_paths(&raw).collect())
        .unwrap_or_default();
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    let local_appdata = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    let unix_bins = [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    for candidate in detect_cli_candidates_in(
        provider,
        std::env::consts::OS,
        std::env::consts::ARCH,
        &path_dirs,
        home.as_deref(),
        appdata.as_deref(),
        local_appdata.as_deref(),
        program_files.as_deref(),
        &unix_bins,
    ) {
        if let Ok(valid) = executable(&candidate, provider) {
            return Some(valid);
        }
    }
    None
}

/// 判断路径是否明显来自另一操作系统（例如把 Windows 下的 `C:\...` 带到 Mac，或把 `/opt/...` 带到 Windows）。
pub fn is_foreign_os_path(raw: &str) -> bool {
    let bytes = raw.trim().as_bytes();
    #[cfg(unix)]
    return bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    #[cfg(windows)]
    return bytes.starts_with(b"/") && !bytes.starts_with(b"//");
    #[cfg(not(any(windows, unix)))]
    return false;
}

fn is_missing_or_foreign_os_path(raw: &str) -> bool {
    if is_foreign_os_path(raw) {
        return true;
    }
    let path = Path::new(raw);
    path.is_absolute() && !path.exists()
}

/// 解析 CLI 可执行文件：留空时自动读取本机已安装的 CLI；若原路径来自旧设备或已升级删除，也自动回退到本机 CLI；
/// 但若用户填写的本机路径存在却不合法（如指向脚本或其他程序），仍严格拒绝。
pub(crate) fn resolve_with_detector(
    configured: &str,
    provider: Provider,
    detector: &dyn Fn(Provider) -> Option<PathBuf>,
) -> Result<(PathBuf, CliSource), AgentError> {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        let found = detector(provider).ok_or(AgentError::PathRejected)?;
        return executable(&found, provider).map(|exe| (exe, CliSource::Auto));
    }
    match executable(Path::new(trimmed), provider) {
        Ok(exe) => Ok((exe, CliSource::Configured)),
        Err(err) if is_missing_or_foreign_os_path(trimmed) => {
            if let Some(found) = detector(provider)
                && let Ok(exe) = executable(&found, provider)
            {
                return Ok((exe, CliSource::AutoFallback));
            }
            Err(err)
        }
        Err(err) => Err(err),
    }
}

#[derive(Debug, Clone)]
pub struct AgentPlan {
    pub provider: Provider,
    pub executable: PathBuf,
    pub workspace: PathBuf,
    pub argv: Vec<String>,
    pub stdin: String,
    pub env: BTreeMap<String, String>,
    pub config_revision: String,
}
/// 运行参数：Codex 用 workspace-write 沙箱（只允许写工作目录）；Claude Code 用 --restricted
/// （去掉 Bash 等执行命令的工具，文件工具限制在工作目录）且只开放读写文件的工具。
pub fn build_plan(
    provider: Provider,
    binary: PathBuf,
    workdir: PathBuf,
    task: &str,
    version: &str,
    help: &str,
) -> Result<AgentPlan, AgentError> {
    if task.trim().is_empty() || task.chars().count() > MAX_TASK_CHARS {
        return Err(AgentError::TaskInvalid);
    }
    parse_help(provider, version, help)?;
    let argv: Vec<String> = match provider {
        Provider::Codex => vec![
            "--ask-for-approval",
            "never",
            "exec",
            "--sandbox",
            "workspace-write",
            "--ignore-user-config",
            "--ignore-rules",
            "--ephemeral",
            "--skip-git-repo-check",
            "--cd",
            workdir.to_str().ok_or(AgentError::PathRejected)?,
            "--json",
            "--color",
            "never",
            "-",
        ],
        Provider::ClaudeCode => vec![
            "--print",
            "--restricted",
            "--permission-mode",
            "acceptEdits",
            "--permission-prompts",
            "none",
            "--tools",
            "Read,Glob,Grep,Edit,Write",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--setting-sources",
            "",
            "--no-session-persistence",
            "--output-format",
            "stream-json",
            "--verbose",
        ],
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    // The eventual runner must clear inherited env and set ONLY these explicitly.
    let env = minimal_env();
    let mut hash = Sha256::new();
    for value in [
        provider.as_str(),
        binary.to_string_lossy().as_ref(),
        workdir.to_string_lossy().as_ref(),
        version,
    ] {
        hash.update(value.as_bytes());
        hash.update([0]);
    }
    for value in &argv {
        hash.update(value.as_bytes());
        hash.update([0]);
    }
    Ok(AgentPlan {
        provider,
        executable: binary,
        workspace: workdir,
        argv,
        stdin: task.to_owned(),
        env,
        config_revision: format!("{:x}", hash.finalize()),
    })
}
pub fn intent_digest(plan: &AgentPlan) -> String {
    let mut sha = Sha256::new();
    for value in [
        plan.provider.as_str(),
        plan.config_revision.as_str(),
        plan.stdin.as_str(),
    ] {
        sha.update(value.as_bytes());
        sha.update([0]);
    }
    format!("{:x}", sha.finalize())
}

/// 传给本机 CLI 的最小环境变量集合：只够系统与 CLI 自己启动（Codex 在沙箱里执行命令需要
/// PATH / PATHEXT / ComSpec / SHELL / TMPDIR），不含任何 MOSS / MCP / 会话 / 云账号变量。
/// 运行器的环境白名单直接引用本常量，避免两边漂移。
pub(crate) const CLI_ENV_KEYS: [&str; 18] = [
    "SystemRoot",
    "WINDIR",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PATH",
    "PATHEXT",
    "ComSpec",
    "HOMEDRIVE",
    "HOMEPATH",
    "LANG",
    "USER",
    "LOGNAME",
    "SHELL",
];
/// 运行与探测用的最小环境变量（与计划里的一致），不带任何 MOSS / MCP 变量。
fn minimal_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in CLI_ENV_KEYS {
        if let Ok(value) = std::env::var(key)
            && !value.contains('\0')
            && value.len() <= 4096
        {
            env.insert(key.into(), value);
        }
    }
    #[cfg(unix)]
    {
        // Finder / Dock 启动的 MOSS.app 默认只有 /usr/bin:/bin:/usr/sbin:/sbin，补上常见用户与 Homebrew bin。
        let mut parts: Vec<PathBuf> = env
            .get("PATH")
            .map(|p| std::env::split_paths(p).collect())
            .unwrap_or_default();
        let mut extras = vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        if let Some(home) = env.get("HOME") {
            extras.insert(0, PathBuf::from(home).join(".local").join("bin"));
        }
        for extra in extras {
            if extra.is_dir() && !parts.iter().any(|p| p == &extra) {
                parts.push(extra);
            }
        }
        if let Ok(joined) = std::env::join_paths(&parts)
            && let Some(s) = joined.to_str()
            && s.len() <= 4096
        {
            env.insert("PATH".into(), s.to_owned());
        }
    }
    env
}

/// 读取 CLI 版本与帮助文本：只运行 `--version` / `--help`（Codex 另读 `exec --help`），15 秒超时，不执行任务。
pub(crate) fn probe_cli(kind: Provider, exe: &Path) -> Option<engine::CapabilitySnapshot> {
    let version = run_probe(exe, &["--version"])?
        .lines()
        .next()?
        .trim()
        .to_owned();
    let help = match kind {
        Provider::Codex => run_probe(exe, &["--help"])? + &run_probe(exe, &["exec", "--help"])?,
        Provider::ClaudeCode => run_probe(exe, &["--help"])?,
    };
    Some(engine::CapabilitySnapshot { version, help })
}
/// 运行一次 CLI 探测命令并取回全部输出。stdout / stderr 在等待退出之前就并发读取；
/// Windows 放进 kill-on-close Job Object，Unix 放进独立进程组并在超时/结束时整组清理。
fn run_probe(exe: &Path, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .env_clear()
        .envs(minimal_env())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.take(256 * 1024).read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(stderr) = stderr {
            let _ = stderr.take(64 * 1024).read_to_string(&mut text);
        }
        text
    });
    #[cfg(windows)]
    let confined = windows_process::confine_running_process(child.id());
    #[cfg(unix)]
    let confined = unix_process::confine_running_group(child.id());
    let started = std::time::Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < std::time::Duration::from_secs(15) => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break;
            }
        }
    }
    let text = out.join().ok()? + &err.join().ok()?;
    #[cfg(any(windows, unix))]
    drop(confined);
    if timed_out {
        return None;
    }
    Some(text)
}
/// 从 CLI 的 JSON 行输出里取最终回答（Claude Code stream-json 的 `result`，Codex --json 最后一条
/// `agent_message`）；取不到时原样返回。
pub fn final_answer(provider: Provider, raw: &str) -> String {
    let lines = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok());
    let found = match provider {
        Provider::ClaudeCode => lines
            .filter(|v| v["type"] == "result")
            .filter_map(|v| v["result"].as_str().map(str::to_owned))
            .next_back(),
        Provider::Codex => lines
            .filter_map(|v| {
                if v["item"]["type"] == "agent_message" {
                    v["item"]["text"].as_str().map(str::to_owned)
                } else if v["msg"]["type"] == "agent_message" {
                    v["msg"]["message"].as_str().map(str::to_owned)
                } else {
                    None
                }
            })
            .next_back(),
    };
    found
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| raw.to_owned())
}

/// 生产执行器：Windows 上用 Job Object 启动 CLI（挂起创建 → 加入 Job → 恢复；结束时整棵进程树一起结束）。
#[cfg(windows)]
struct JobRunner;
#[cfg(windows)]
impl engine::AgentRunner for JobRunner {
    fn run(
        &self,
        plan: &AgentPlan,
        alive: &dyn Fn() -> bool,
    ) -> Result<String, (AgentError, String)> {
        let never = std::sync::atomic::AtomicBool::new(false);
        let result = windows_process::run_fixture_process(
            &plan.executable,
            &plan.argv,
            &plan.workspace,
            &plan.env,
            plan.stdin.as_bytes(),
            alive,
            &never,
            std::time::Duration::from_secs(MAX_RUN_SECONDS),
        )
        .map_err(|_| (AgentError::RunFailed, String::new()))?;
        let output = final_answer(plan.provider, &result.output);
        match result.finish {
            windows_process::Finish::Completed => Ok(output),
            windows_process::Finish::Cancelled => Err((AgentError::PermissionExpired, output)),
            windows_process::Finish::TimedOut => Err((AgentError::TimedOut, output)),
            windows_process::Finish::ExitFailure(_) => Err((AgentError::RunFailed, output)),
        }
    }
}

/// 生产执行器：Unix / macOS 上用独立进程组 + pre-exec 门禁管道启动 CLI，结束/取消/超时时 `kill(-pgid, SIGKILL)` 整组收口。
#[cfg(unix)]
struct UnixGroupRunner;
#[cfg(unix)]
impl engine::AgentRunner for UnixGroupRunner {
    fn run(
        &self,
        plan: &AgentPlan,
        alive: &dyn Fn() -> bool,
    ) -> Result<String, (AgentError, String)> {
        let never = std::sync::atomic::AtomicBool::new(false);
        let result = unix_process::run_fixture_process(
            &plan.executable,
            &plan.argv,
            &plan.workspace,
            &plan.env,
            plan.stdin.as_bytes(),
            alive,
            &never,
            std::time::Duration::from_secs(MAX_RUN_SECONDS),
        )
        .map_err(|_| (AgentError::RunFailed, String::new()))?;
        let output = final_answer(plan.provider, &result.output);
        match result.finish {
            unix_process::Finish::Completed => Ok(output),
            unix_process::Finish::Cancelled => Err((AgentError::PermissionExpired, output)),
            unix_process::Finish::TimedOut => Err((AgentError::TimedOut, output)),
            unix_process::Finish::ExitFailure(_) => Err((AgentError::RunFailed, output)),
        }
    }
}

#[cfg(not(any(windows, unix)))]
struct UnsupportedRunner;
#[cfg(not(any(windows, unix)))]
impl engine::AgentRunner for UnsupportedRunner {
    fn run(&self, _: &AgentPlan, _: &dyn Fn() -> bool) -> Result<String, (AgentError, String)> {
        Err((AgentError::SandboxUnverified, String::new()))
    }
}
pub(crate) fn production_runner() -> std::sync::Arc<dyn engine::AgentRunner> {
    #[cfg(windows)]
    {
        std::sync::Arc::new(JobRunner)
    }
    #[cfg(unix)]
    {
        std::sync::Arc::new(UnixGroupRunner)
    }
    #[cfg(not(any(windows, unix)))]
    {
        std::sync::Arc::new(UnsupportedRunner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_must_list_every_safety_flag_but_version_is_not_pinned() {
        let flags = "--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        assert!(parse_help(Provider::Codex, "codex-cli 0.157.1", flags).is_ok());
        assert!(
            parse_help(Provider::Codex, "codex-cli 0.158.0", flags).is_ok(),
            "版本升级后只要安全参数还在就能用"
        );
        assert_eq!(
            parse_help(Provider::Codex, "codex-cli 0.157.1", "--sandbox"),
            Err(AgentError::CapabilityMissing)
        );
        assert_eq!(
            parse_help(Provider::Codex, "something else", flags),
            Err(AgentError::CapabilityMissing)
        );
        let claude = "--print --restricted --permission-mode --permission-prompts --tools --strict-mcp-config --mcp-config --setting-sources --no-session-persistence --output-format --verbose";
        assert!(parse_help(Provider::ClaudeCode, "2.1.283 (Claude Code)", claude).is_ok());
        assert_eq!(
            parse_help(Provider::ClaudeCode, "2.1.283 (Claude Code)", "--print"),
            Err(AgentError::CapabilityMissing),
            "缺 --restricted 就不运行"
        );
    }
    #[test]
    fn final_answer_is_extracted_from_cli_json_lines() {
        let claude = "{\"type\":\"system\"}\n{\"type\":\"result\",\"result\":\"已修改 README\"}\n";
        assert_eq!(final_answer(Provider::ClaudeCode, claude), "已修改 README");
        let codex = "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"第一句\"}}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"完成\"}}\n";
        assert_eq!(final_answer(Provider::Codex, codex), "完成");
        assert_eq!(
            final_answer(Provider::Codex, "plain text"),
            "plain text",
            "取不到时原样返回"
        );
    }
    #[test]
    fn write_mode_stays_inside_workspace() {
        let codex = "--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        let plan = build_plan(
            Provider::Codex,
            "C:/tmp/codex.exe".into(),
            "C:/tmp/isolated".into(),
            "fix",
            "codex-cli 0.157.1",
            codex,
        )
        .unwrap();
        assert!(
            plan.argv
                .windows(2)
                .any(|w| w == ["--sandbox", "workspace-write"])
        );
        assert!(!plan.argv.iter().any(|a| a.contains("danger")));
        let claude = "--print --restricted --permission-mode --permission-prompts --tools --strict-mcp-config --mcp-config --setting-sources --no-session-persistence --output-format --verbose";
        let plan = build_plan(
            Provider::ClaudeCode,
            "C:/tmp/claude.exe".into(),
            "C:/tmp/isolated".into(),
            "fix",
            "2.1.283 (Claude Code)",
            claude,
        )
        .unwrap();
        assert!(plan.argv.contains(&"--restricted".to_owned()));
        assert!(
            plan.argv
                .windows(2)
                .any(|w| w == ["--tools", "Read,Glob,Grep,Edit,Write"]),
            "不开放 Bash / 网络工具"
        );
    }
    #[test]
    fn task_is_stdin_never_shell_argument() {
        let flags = "--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        let task = "say \"hello\"\n$(Remove-Item *)\n'.exe -evil";
        let plan = build_plan(
            Provider::Codex,
            "C:/tmp/codex.exe".into(),
            "C:/tmp/isolated".into(),
            task,
            "codex-cli 0.157.1",
            flags,
        )
        .unwrap();
        assert_eq!(plan.stdin, task);
        assert!(!plan.argv.join(" ").contains(task));
        assert_eq!(plan.argv.last().unwrap(), "-");
        assert!(
            !plan
                .env
                .keys()
                .any(|k| k.contains("MOSS") || k.contains("MCP"))
        );
        let mut changed = plan.clone();
        changed.stdin.push('!');
        assert_ne!(intent_digest(&plan), intent_digest(&changed));
    }
    #[test]
    fn forbidden_workspace_and_escape() {
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("secrets");
        std::fs::create_dir(&secret).unwrap();
        let cwd = root.path().join("agent-workspace");
        std::fs::create_dir(&cwd).unwrap();
        assert!(workspace(&cwd, std::slice::from_ref(&secret)).is_ok());
        assert_eq!(
            workspace(&secret, std::slice::from_ref(&secret)),
            Err(AgentError::PathRejected)
        );
        assert_eq!(
            workspace(root.path(), &[cwd]),
            Err(AgentError::PathRejected)
        );
    }
    #[test]
    fn runner_environment_whitelist_accepts_every_key_the_plan_sets() {
        let flags = "--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        let plan = build_plan(
            Provider::Codex,
            "C:/tmp/codex.exe".into(),
            "C:/tmp/isolated".into(),
            "fix",
            "codex-cli 0.157.1",
            flags,
        )
        .unwrap();
        #[cfg(windows)]
        assert!(
            windows_process::environment_block(&plan.env).is_ok(),
            "运行器必须接受计划里的每一个环境变量，否则真实 CLI 一律 agent_run_failed（2026-10-16 实机发现）"
        );
        #[cfg(unix)]
        assert!(
            unix_process::validate_environment(&plan.env).is_ok(),
            "Unix 运行器必须接受计划里的每一个环境变量"
        );
        assert_eq!(
            minimal_env().keys().collect::<Vec<_>>(),
            plan.env.keys().collect::<Vec<_>>(),
            "探测与实际运行必须用同一套环境变量"
        );
        for forbidden in ["MOSS_API_TOKEN", "MCP_URL", "MOSS_DEVICE_TOKEN"] {
            let mut env = plan.env.clone();
            env.insert(forbidden.into(), "x".into());
            #[cfg(windows)]
            assert!(
                windows_process::environment_block(&env).is_err(),
                "不得把 MOSS / MCP 凭据传给 CLI"
            );
            #[cfg(unix)]
            assert!(
                unix_process::validate_environment(&env).is_err(),
                "不得把 MOSS / MCP 凭据传给 CLI"
            );
        }
    }
    #[test]
    fn detect_cli_candidates_supports_windows_fnm_and_macos_homebrew_nvm_layouts() {
        let temp = tempfile::tempdir().unwrap();
        // 1. Windows fnm + .local/bin layout
        let win_home = temp.path().join("win-home");
        let win_roaming = temp.path().join("roaming");
        let win_claude = win_home.join(".local").join("bin").join("claude.exe");
        let win_codex = win_roaming
            .join("fnm")
            .join("node-versions")
            .join("v24.20.0")
            .join("installation")
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("node_modules")
            .join("@openai")
            .join("codex-win32-x64")
            .join("vendor")
            .join("x86_64-pc-windows-msvc")
            .join("bin")
            .join("codex.exe");
        std::fs::create_dir_all(win_claude.parent().unwrap()).unwrap();
        std::fs::create_dir_all(win_codex.parent().unwrap()).unwrap();
        std::fs::write(&win_claude, b"MZ-native").unwrap();
        std::fs::write(&win_codex, b"MZ-native").unwrap();

        let win_codex_list = detect_cli_candidates_in(
            Provider::Codex,
            "windows",
            "x86_64",
            &[],
            Some(&win_home),
            Some(&win_roaming),
            None,
            None,
            &[],
        );
        assert!(win_codex_list.contains(&win_codex), "{win_codex_list:?}");
        let win_claude_list = detect_cli_candidates_in(
            Provider::ClaudeCode,
            "windows",
            "x86_64",
            &[],
            Some(&win_home),
            Some(&win_roaming),
            None,
            None,
            &[],
        );
        assert!(win_claude_list.contains(&win_claude), "{win_claude_list:?}");

        // 2. macOS Apple Silicon Homebrew + nvm + ~/.local/share/claude/versions/<ver> layout
        let mac_home = temp.path().join("mac-home");
        let brew_bin = temp.path().join("opt").join("homebrew").join("bin");
        let mac_codex = mac_home
            .join(".nvm")
            .join("versions")
            .join("node")
            .join("v22.14.0")
            .join("lib")
            .join("node_modules")
            .join("@openai")
            .join("codex-darwin-arm64")
            .join("vendor")
            .join("aarch64-apple-darwin")
            .join("bin")
            .join("codex");
        let mac_claude_ver = mac_home
            .join(".local")
            .join("share")
            .join("claude")
            .join("versions")
            .join("2.1.283");
        std::fs::create_dir_all(&brew_bin).unwrap();
        std::fs::create_dir_all(mac_codex.parent().unwrap()).unwrap();
        std::fs::create_dir_all(mac_claude_ver.parent().unwrap()).unwrap();
        std::fs::write(&mac_codex, b"\xcf\xfa\xed\xfe").unwrap();
        std::fs::write(&mac_claude_ver, b"\xcf\xfa\xed\xfe").unwrap();

        let mac_codex_list = detect_cli_candidates_in(
            Provider::Codex,
            "macos",
            "aarch64",
            &[],
            Some(&mac_home),
            None,
            None,
            None,
            std::slice::from_ref(&brew_bin),
        );
        assert!(mac_codex_list.contains(&mac_codex), "{mac_codex_list:?}");
        let mac_claude_list = detect_cli_candidates_in(
            Provider::ClaudeCode,
            "macos",
            "aarch64",
            &[],
            Some(&mac_home),
            None,
            None,
            None,
            &[brew_bin],
        );
        assert!(
            mac_claude_list.contains(&mac_claude_ver),
            "{mac_claude_list:?}"
        );
        assert!(is_claude_versioned_binary(&mac_claude_ver));
    }
    #[test]
    fn resolve_with_detector_auto_reads_and_recovers_after_switching_devices() {
        let temp = tempfile::tempdir().unwrap();
        let real_bin = temp.path().join(Provider::Codex.expected_binary());
        std::fs::write(&real_bin, b"native-binary").unwrap();
        let script_bin = temp
            .path()
            .join(format!("script-{}", Provider::Codex.expected_binary()));
        std::fs::write(&script_bin, b"#!/usr/bin/env node\nconsole.log(1);\n").unwrap();
        assert!(is_script_file(&script_bin), "shebang 脚本必须被识别并拦截");

        let detector = |kind: Provider| (kind == Provider::Codex).then(|| real_bin.clone());
        // 1. 留空自动读取本机 CLI
        let (resolved, source) = resolve_with_detector("", Provider::Codex, &detector).unwrap();
        assert_eq!(
            (resolved.as_path(), source),
            (real_bin.canonicalize().unwrap().as_path(), CliSource::Auto)
        );
        // 2. 换设备（旧路径不存在或属于另一操作系统）自动切换到本机 CLI
        let foreign = if cfg!(windows) {
            "/opt/homebrew/bin/codex"
        } else {
            r"C:\Users\alice\codex.exe"
        };
        let (resolved_fb, source_fb) =
            resolve_with_detector(foreign, Provider::Codex, &detector).unwrap();
        assert_eq!(
            (resolved_fb.as_path(), source_fb),
            (
                real_bin.canonicalize().unwrap().as_path(),
                CliSource::AutoFallback
            )
        );
        // 3. 本机真实存在但不合法的文件绝不静默回退
        assert_eq!(
            resolve_with_detector(&script_bin.to_string_lossy(), Provider::Codex, &detector),
            Err(AgentError::PathRejected)
        );
    }
    #[test]
    fn help_flags_match_on_token_boundaries_not_substrings() {
        // CLI 把安全参数改名后（如 --tools → --tools-legacy），子串匹配会冒名通过，token 匹配必须拒绝
        let renamed = "--print --restricted --permission-mode --permission-prompts --tools-legacy --strict-mcp-config --mcp-config --setting-sources --no-session-persistence --output-format --verbose";
        assert_eq!(
            parse_help(Provider::ClaudeCode, "2.1.283 (Claude Code)", renamed),
            Err(AgentError::CapabilityMissing),
            "--tools 改名后不得再用子串冒名通过"
        );
        // 真实帮助文本里参数带标点、占位符（`--tools <t>,` / `--permission-mode=<mode>`）也必须识别
        let punctuated = "--print, --restricted --permission-mode=<mode> --permission-prompts never --tools(Read,Glob,Grep,Edit,Write) --strict-mcp-config --mcp-config --setting-sources --no-session-persistence --output-format --verbose";
        assert!(
            parse_help(Provider::ClaudeCode, "2.1.283 (Claude Code)", punctuated).is_ok(),
            "参数出现在标点/占位符中不得漏判"
        );
        assert!(lists_flag("--cd <dir>  workdir", "--cd") && !lists_flag("workdir--cd", "--cd"));
    }
    /// 探测必须并发读取管道：子进程输出超过管道缓冲（约 64KB）时，旧实现“先等退出再读”
    /// 会把子进程堵死并按 15 秒超时误判 `agent_capability_missing`。夹具用本测试二进制当假 CLI。
    #[test]
    fn probe_reads_more_than_a_pipe_buffer_of_output() {
        const NAME: &str = "voice_agent::tests::probe_reads_more_than_a_pipe_buffer_of_output";
        // 夹具角色：被 `--exact` 指名运行时，打印超过管道缓冲的输出后正常退出
        if std::env::args().nth(2).as_deref() == Some(NAME) {
            let line = format!("NV13_PROBE_DRAIN {}", "x".repeat(56));
            for _ in 0..2048 {
                println!("{line}");
            }
            println!("NV13_PROBE_DRAIN_TAIL");
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let text = run_probe(&exe, &["--exact", NAME, "--nocapture"])
            .expect("输出超过管道缓冲时探测必须并发读取，而不是 15 秒超时误判不可用");
        assert!(
            text.contains("NV13_PROBE_DRAIN_TAIL"),
            "必须读到夹具输出的末尾"
        );
    }
    // ==== 手动实机验证（默认被 #[ignore] 跳过，且要求显式给出环境变量，与 NV13_DOCKER_TEST_EXE 同类探针）====
    fn manual_real_plan() -> AgentPlan {
        let provider = match std::env::var("MOSS_REAL_PROVIDER")
            .unwrap_or_else(|_| "codex".into())
            .as_str()
        {
            "claude_code" | "claude" => Provider::ClaudeCode,
            _ => Provider::Codex,
        };
        let exe = std::path::PathBuf::from(std::env::var("MOSS_REAL_CLI").expect("MOSS_REAL_CLI"));
        let dir = workspace(
            std::path::Path::new(
                &std::env::var("MOSS_REAL_WORKSPACE").expect("MOSS_REAL_WORKSPACE"),
            ),
            &[],
        )
        .expect("workspace");
        let task = std::env::var("MOSS_REAL_TASK").expect("MOSS_REAL_TASK");
        let binary = executable(&exe, provider).expect("cli path");
        let snapshot = probe_cli(provider, &binary).expect("probe");
        println!(
            "PROBE provider={:?} version={} helplen={}",
            provider,
            snapshot.version,
            snapshot.help.len()
        );
        let plan = build_plan(
            provider,
            binary,
            dir,
            &task,
            &snapshot.version,
            &snapshot.help,
        )
        .expect("plan");
        println!("ARGV {:?}", plan.argv);
        plan
    }
    #[test]
    #[ignore = "真实 CLI 手动验证：调用真实账号、会修改工作目录，需显式 --ignored"]
    fn manual_real_cli_roundtrip() {
        let plan = manual_real_plan();
        let started = std::time::Instant::now();
        match production_runner().run(&plan, &|| true) {
            Ok(output) => println!("REAL_CLI_OK elapsed={:?}\n{output}", started.elapsed()),
            Err((error, output)) => println!(
                "REAL_CLI_ERR {} elapsed={:?}\n{}",
                error.code(),
                started.elapsed(),
                output
            ),
        }
    }
    #[test]
    #[ignore = "真实 CLI 手动停止验证：调用真实账号，需显式 --ignored"]
    fn manual_real_cli_stop() {
        let plan = manual_real_plan();
        let started = std::time::Instant::now();
        let alive = || started.elapsed() < std::time::Duration::from_secs(8);
        match production_runner().run(&plan, &alive) {
            Ok(output) => println!(
                "REAL_STOP_UNEXPECTED_COMPLETION elapsed={:?}\n{output}",
                started.elapsed()
            ),
            Err((error, output)) => println!(
                "REAL_STOP_ERR {} elapsed={:?}\n{}",
                error.code(),
                started.elapsed(),
                output
            ),
        }
    }
}
