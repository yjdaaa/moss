//! NV13 read-only CLI policy and inert execution plan.
//! A help fixture is not proof of an actual sandbox; no CLI is launched here.
use std::collections::BTreeMap;
use std::path::{Component,Path,PathBuf};
use serde::{Deserialize,Serialize};
use sha2::{Digest,Sha256};

// NV13 Windows suspended-spawn + Job Object harness; production AgentRunner
// remains DisabledRunner until independent read/network sandbox isolation passes.
#[cfg(windows)]
mod windows_process;
// Offline fake-upstream HTTP request policy. Not a production model proxy.
mod model_proxy;
// Digest-pinned, synthetic-only Docker invocation plan; not production runner.
mod docker_plan;
mod engine;
pub use engine::{AgentCoordinator,CapabilitySnapshot,AgentRunner};

pub const MAX_TASK_CHARS:usize=4000;
pub const MAX_OUTPUT_BYTES:usize=256*1024;
pub const MAX_OUTPUT_LINE_BYTES:usize=64*1024;
pub const MAX_RUN_SECONDS:u64=300;
pub const MAX_RUN_RECORDS:usize=50;

#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum Provider { Codex, ClaudeCode }
impl Provider {
 pub fn as_str(self)->&'static str{match self{Self::Codex=>"codex",Self::ClaudeCode=>"claude_code"}}
 pub fn expected_binary(self)->&'static str{match self{Self::Codex=>"codex.exe",Self::ClaudeCode=>"claude.exe"}}
}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum AgentError {Disabled,PathRejected,CapabilityMissing,TaskInvalid,SandboxUnverified,ConfigChanged,PermissionExpired,AlreadyUsed}
impl AgentError {
 pub fn code(self)->&'static str{match self {
  Self::Disabled=>"agent_disabled",Self::PathRejected=>"agent_path_denied",
  Self::CapabilityMissing=>"agent_capability_missing",Self::TaskInvalid=>"agent_task_invalid",
  Self::SandboxUnverified=>"agent_sandbox_unavailable",Self::ConfigChanged=>"agent_config_changed",
  Self::PermissionExpired=>"agent_permission_expired",Self::AlreadyUsed=>"agent_approval_consumed"
 }}
}
/// Parsed help only certifies listed flags for the exact known CLI version.
/// It does not authorize running a command or enable a provider.
pub fn parse_help(provider:Provider,version:&str,help:&str)->Result<(),AgentError>{
 let correct=match provider{
   Provider::Codex=>version.trim()=="codex-cli 0.157.1",
   Provider::ClaudeCode=>matches!(version.trim(),"2.1.283"|"claude 2.1.283"|"Claude Code 2.1.283"),
 };
 if !correct{return Err(AgentError::CapabilityMissing);}
 let flags:&[&str]=match provider{
  Provider::Codex=>&["--ask-for-approval","--sandbox","--ignore-user-config","--ignore-rules","--ephemeral","--skip-git-repo-check","--cd","--json","--color"],
  Provider::ClaudeCode=>&["--print","--restricted","--permission-mode","--permission-prompts","--tools","--strict-mcp-config","--mcp-config","--setting-sources","--no-session-persistence","--output-format","--verbose"]
 };
 if flags.iter().any(|flag|!help.contains(flag)){return Err(AgentError::CapabilityMissing);}
 Ok(())
}
/// Canonicalize existing, explicitly selected workspaces. Do not follow
/// symlinks or junctions, or allow the workspace above/below protected roots.
pub fn workspace(path:&Path,sensitive_roots:&[PathBuf])->Result<PathBuf,AgentError>{
 if !path.is_absolute(){return Err(AgentError::PathRejected);}
 let canonical=path.canonicalize().map_err(|_|AgentError::PathRejected)?;
 if !canonical.is_dir()||canonical.parent().is_none(){return Err(AgentError::PathRejected);}
 for root in sensitive_roots {
    let protected=root.canonicalize().map_err(|_|AgentError::PathRejected)?;
    if canonical.starts_with(&protected)||protected.starts_with(&canonical){return Err(AgentError::PathRejected);}
 }
 if let Ok(home)=std::env::var("USERPROFILE").or_else(|_|std::env::var("HOME")){
    if let Ok(home)=PathBuf::from(home).canonicalize(){
       if canonical==home||home.starts_with(&canonical){return Err(AgentError::PathRejected);}
    }
 }
 for key in ["SystemRoot","WINDIR"]{
    if let Ok(value)=std::env::var(key){
      if let Ok(root)=PathBuf::from(value).canonicalize(){
         if canonical.starts_with(&root)||root.starts_with(&canonical){return Err(AgentError::PathRejected);}
      }
    }
 }
 let forbidden=[".env",".mcp.json",".claude",".codex",".ssh","app_secrets","secrets","moss","moss-rust","program files","windows","system32"];
 if canonical.components().any(|component|match component {
    Component::Normal(part)=>forbidden.contains(&part.to_string_lossy().to_ascii_lowercase().as_str()),
    _=>false,
 }){return Err(AgentError::PathRejected);}
 let mut current=PathBuf::new();
 for component in path.components(){
   current.push(component.as_os_str());
   let meta=std::fs::symlink_metadata(&current).map_err(|_|AgentError::PathRejected)?;
   if meta.file_type().is_symlink(){return Err(AgentError::PathRejected);}
   #[cfg(windows)]{
    use std::os::windows::fs::MetadataExt;
    if meta.file_attributes()&0x400!=0{return Err(AgentError::PathRejected);}
   }
 }
 Ok(canonical)
}
pub fn executable(path:&Path,provider:Provider)->Result<PathBuf,AgentError>{
 if !path.is_absolute(){return Err(AgentError::PathRejected);}
 let canonical=path.canonicalize().map_err(|_|AgentError::PathRejected)?;
 let name=canonical.file_name().and_then(|s|s.to_str()).unwrap_or("");
 if !name.eq_ignore_ascii_case(provider.expected_binary()){return Err(AgentError::PathRejected);}
 let meta=std::fs::symlink_metadata(path).map_err(|_|AgentError::PathRejected)?;
 if meta.file_type().is_symlink()||!meta.is_file(){return Err(AgentError::PathRejected);}
 #[cfg(windows)]{
   use std::os::windows::fs::MetadataExt;
   if meta.file_attributes()&0x400!=0{return Err(AgentError::PathRejected);}
 }
 Ok(canonical)
}
#[derive(Debug,Clone)]
pub struct ReadOnlyPlan{
 pub provider:Provider,
 pub executable:PathBuf,
 pub workspace:PathBuf,
 pub argv:Vec<String>,
 pub stdin:String,
 pub env:BTreeMap<String,String>,
 pub config_revision:String,
}
pub fn build_plan(provider:Provider,binary:PathBuf,workdir:PathBuf,task:&str,version:&str,help:&str)->Result<ReadOnlyPlan,AgentError>{
 if task.trim().is_empty()||task.chars().count()>MAX_TASK_CHARS{return Err(AgentError::TaskInvalid);}
 parse_help(provider,version,help)?;
 let argv:Vec<String>=match provider{
   Provider::Codex=>vec!["--ask-for-approval","never","exec","--sandbox","read-only","--ignore-user-config","--ignore-rules","--ephemeral","--skip-git-repo-check","--cd",workdir.to_str().ok_or(AgentError::PathRejected)?,"--json","--color","never","-"],
   Provider::ClaudeCode=>vec!["--print","--restricted","--permission-mode","plan","--permission-prompts","none","--tools","Read,Glob,Grep","--strict-mcp-config","--mcp-config","{\"mcpServers\":{}}","--setting-sources","","--no-session-persistence","--output-format","stream-json","--verbose"],
 }.into_iter().map(str::to_owned).collect();
 // The eventual runner must clear inherited env and set ONLY these explicitly.
 let mut env=BTreeMap::new();
 if cfg!(windows){
   for key in ["SystemRoot","WINDIR","TEMP","TMP","USERPROFILE","APPDATA","LOCALAPPDATA"]{
      if let Ok(value)=std::env::var(key){env.insert(key.into(),value);}
   }
 }
 let mut hash=Sha256::new();
 for value in [provider.as_str(),binary.to_string_lossy().as_ref(),workdir.to_string_lossy().as_ref(),version] {
   hash.update(value.as_bytes());hash.update([0]);
 }
 for value in &argv{hash.update(value.as_bytes());hash.update([0]);}
 Ok(ReadOnlyPlan{provider,executable:binary,workspace:workdir,argv,stdin:task.to_owned(),env,config_revision:format!("{:x}",hash.finalize())})
}
pub fn intent_digest(plan:&ReadOnlyPlan)->String{
 let mut sha=Sha256::new();
 for value in [plan.provider.as_str(),plan.config_revision.as_str(),plan.stdin.as_str()]{
   sha.update(value.as_bytes());sha.update([0]);
 }
 format!("{:x}",sha.finalize())
}
/// Separate real-CLI sandbox tests (not help) are required before enabling.
pub const REAL_CLI_SANDBOX_VERIFIED:bool=false;

#[cfg(test)]
mod tests{
 use super::*;
 #[test]fn exact_cli_capability_fixture_required(){
  let flags="--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
  assert!(parse_help(Provider::Codex,"codex-cli 0.157.1",flags).is_ok());
  assert_eq!(parse_help(Provider::Codex,"codex-cli 0.157.1","--sandbox"),Err(AgentError::CapabilityMissing));
  assert_eq!(parse_help(Provider::Codex,"codex-cli 0.158.0",flags),Err(AgentError::CapabilityMissing));
  assert!(!REAL_CLI_SANDBOX_VERIFIED);
 }
 #[test]fn task_is_stdin_never_shell_argument(){
  let flags="--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
  let task="say \"hello\"\n$(Remove-Item *)\n'.exe -evil";
  let plan=build_plan(Provider::Codex,"C:/tmp/codex.exe".into(),"C:/tmp/isolated".into(),task,"codex-cli 0.157.1",flags).unwrap();
  assert_eq!(plan.stdin,task);
  assert!(!plan.argv.join(" ").contains(task));
  assert_eq!(plan.argv.last().unwrap(),"-");
  assert!(!plan.env.keys().any(|k|k.contains("MOSS")||k.contains("MCP")));
  let mut changed=plan.clone();changed.stdin.push('!');
  assert_ne!(intent_digest(&plan),intent_digest(&changed));
 }
 #[test]fn forbidden_workspace_and_escape(){
  let root=tempfile::tempdir().unwrap();
  let secret=root.path().join("secrets");std::fs::create_dir(&secret).unwrap();
  let cwd=root.path().join("agent-workspace");std::fs::create_dir(&cwd).unwrap();
  assert!(workspace(&cwd,&[secret.clone()]).is_ok());
  assert_eq!(workspace(&secret,&[secret.clone()]),Err(AgentError::PathRejected));
  assert_eq!(workspace(root.path(),&[cwd]),Err(AgentError::PathRejected));
 }
}
