//! NV13 host-owned approval bridge（2026-10-15 起生产启用；2026-10-25 开通 macOS / Unix 并支持本机自动读取 CLI）。
//! 每次运行都要桌面批准；只在严格模式开启时运行（Agent 即使调用本机 MOSS 接口也动不了激光机）；
//! 批准后在后台线程运行，进程树受系统级隔离（Windows Job Object / Unix 独立进程组）、有时长与输出上限，可随时停止。
//! 剩余风险（已告知用户）：CLI 自身的沙箱不能阻止读取工作目录外的文件。
use super::{
    AgentError, AgentPlan, MAX_RUN_RECORDS, Provider, build_plan, intent_digest,
    resolve_with_detector, workspace,
};
use laser_api::{
    ApiService,
    agent_runs::AgentRuns,
    approval::{ApprovalError, NewApproval, approval_digest},
};
use laser_contracts::{
    ApprovalConnection, ApprovalKind, ApprovalRecord, ApprovalSource, CallerContext,
    ToolCallResult, audit_client,
};
use laser_runtime::{AgentRunRecord, RuntimeDb};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub struct CapabilitySnapshot {
    pub version: String,
    pub help: String,
}
pub trait AgentRunner: Send + Sync {
    /// Only called after atomic desktop approval consumption. `alive` turns false on
    /// stop, revocation, strict-mode off or configuration change. Err carries output so far.
    fn run(
        &self,
        plan: &AgentPlan,
        alive: &dyn Fn() -> bool,
    ) -> Result<String, (AgentError, String)>;
}
/// 读取 CLI 的版本与帮助文本（只运行 --version / --help，不执行任务）。
pub type CapabilityProbe = Arc<dyn Fn(Provider, &Path) -> Option<CapabilitySnapshot> + Send + Sync>;
/// 自动扫描本机已安装的 CLI 可执行文件（不启动子进程）。
pub type CliDetector = Arc<dyn Fn(Provider) -> Option<PathBuf> + Send + Sync>;
type CachedSnapshot = (
    Option<std::time::SystemTime>,
    u64,
    std::time::Instant,
    Option<CapabilitySnapshot>,
);

pub struct AgentCoordinator {
    api: Weak<ApiService>,
    db: Arc<Mutex<RuntimeDb>>,
    settings_file: PathBuf,
    sensitive: Vec<PathBuf>,
    probe: CapabilityProbe,
    detector: CliDetector,
    /// 按可执行文件路径缓存；文件大小或修改时间变了就重新探测
    snapshots: Mutex<HashMap<PathBuf, CachedSnapshot>>,
    runner: Arc<dyn AgentRunner>,
    /// 正在运行的任务：运行编号 → 停止标志
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
    me: Weak<Self>,
}
impl AgentCoordinator {
    pub fn attach(
        api: &Arc<ApiService>,
        db: Arc<Mutex<RuntimeDb>>,
        settings_file: PathBuf,
        sensitive: Vec<PathBuf>,
    ) -> Result<Arc<Self>, String> {
        let manager = Arc::new_cyclic(|me| Self {
            api: Arc::downgrade(api),
            db,
            settings_file,
            sensitive,
            probe: Arc::new(super::probe_cli),
            detector: Arc::new(super::detect_cli),
            snapshots: Mutex::new(HashMap::new()),
            runner: super::production_runner(),
            running: Mutex::new(HashMap::new()),
            me: me.clone(),
        });
        api.set_agent_runs(manager.clone())?;
        Ok(manager)
    }
    fn configuration(&self) -> moss_voice::settings::BuiltinAgents {
        moss_voice::settings::load(&self.settings_file).builtin_agents
    }
    fn snapshot(&self, kind: Provider, exe: &Path) -> Result<CapabilitySnapshot, AgentError> {
        let meta = std::fs::metadata(exe).map_err(|_| AgentError::CapabilityMissing)?;
        let key = (meta.modified().ok(), meta.len());
        {
            let cache = self
                .snapshots
                .lock()
                .map_err(|_| AgentError::CapabilityMissing)?;
            if let Some((modified, len, cached_at, snapshot)) = cache.get(exe) {
                if (*modified, *len) == key {
                    if let Some(snap) = snapshot {
                        return Ok(snap.clone());
                    }
                    if cached_at.elapsed() < std::time::Duration::from_secs(5) {
                        return Err(AgentError::CapabilityMissing);
                    }
                }
            }
        }
        let probed = (self.probe)(kind, exe);
        if let Ok(after) = std::fs::metadata(exe)
            && (after.modified().ok(), after.len()) == key
            && let Ok(mut cache) = self.snapshots.lock()
        {
            cache.insert(
                exe.to_path_buf(),
                (key.0, key.1, std::time::Instant::now(), probed.clone()),
            );
        }
        probed.ok_or(AgentError::CapabilityMissing)
    }
    fn prepare(&self, kind: Provider, task: &str) -> Result<AgentPlan, AgentError> {
        let config = self.configuration();
        if !config.enabled {
            return Err(AgentError::Disabled);
        }
        let api = self.api.upgrade().ok_or(AgentError::Disabled)?;
        if !api.strict_mode() {
            return Err(AgentError::StrictModeRequired);
        }
        let cli = match kind {
            Provider::Codex => &config.codex_executable,
            Provider::ClaudeCode => &config.claude_executable,
        };
        let (executable, _source) = resolve_with_detector(cli, kind, &*self.detector)?;
        let workspace = workspace(Path::new(&config.workspace), &self.sensitive)?;
        let snapshot = self.snapshot(kind, &executable)?;
        build_plan(
            kind,
            executable,
            workspace,
            task,
            &snapshot.version,
            &snapshot.help,
        )
    }
    fn kind_from(name: &str) -> Result<Provider, AgentError> {
        match name {
            "agent_codex" => Ok(Provider::Codex),
            "agent_claude_code" => Ok(Provider::ClaudeCode),
            _ => Err(AgentError::Disabled),
        }
    }
    fn err_code(error: AgentError) -> String {
        error.code().to_owned()
    }
    fn prepare_record(
        &self,
        record: &ApprovalRecord,
    ) -> Result<(AgentPlan, CallerContext, Arc<AtomicBool>), ApprovalError> {
        if record.kind != ApprovalKind::AgentRun || record.source != ApprovalSource::Voice {
            return Err(ApprovalError::Mismatch);
        }
        let hidden = record.request.as_ref().ok_or(ApprovalError::Mismatch)?;
        let device = hidden
            .get("device_id")
            .and_then(Value::as_str)
            .ok_or(ApprovalError::Mismatch)?;
        let session = hidden
            .get("voice_session_id")
            .and_then(Value::as_str)
            .ok_or(ApprovalError::Mismatch)?;
        let task = hidden
            .get("task")
            .and_then(Value::as_str)
            .ok_or(ApprovalError::Mismatch)?;
        let provider =
            Self::kind_from(&record.target_record).map_err(|_| ApprovalError::Mismatch)?;
        if record.session_id.as_deref() != Some(device) {
            return Err(ApprovalError::Mismatch);
        }
        let ctx = CallerContext::Voice {
            device_id: device.into(),
            session_id: session.into(),
        };
        let api = self.api.upgrade().ok_or(ApprovalError::Mismatch)?;
        let permit = api.agent_voice_permit(&ctx)?;
        if !permit.load(Ordering::Acquire) {
            return Err(ApprovalError::Expired);
        }
        let plan = self
            .prepare(provider, task)
            .map_err(|_| ApprovalError::Mismatch)?;
        let expected = ApprovalConnection::Agent {
            provider: provider.as_str().into(),
            fingerprint: plan.config_revision.clone(),
        };
        if record.connection.as_ref() != Some(&expected)
            || hidden.get("config_revision").and_then(Value::as_str)
                != Some(plan.config_revision.as_str())
            || hidden.get("task_digest").and_then(Value::as_str)
                != Some(intent_digest(&plan).as_str())
        {
            return Err(ApprovalError::Mismatch);
        }
        Ok((plan, ctx, permit))
    }
    fn response(status: u16, message: &str, code: &str) -> (u16, Value) {
        (
            status,
            json!({"success":false,"result":message,"detail":{"error_code":code}}),
        )
    }
    /// 停止正在运行的任务（进程树随系统隔离组一起结束）。
    pub fn stop(&self, run_id: &str) -> bool {
        self.running
            .lock()
            .ok()
            .and_then(|running| running.get(run_id).cloned())
            .is_some_and(|flag| {
                flag.store(true, Ordering::Release);
                true
            })
    }
    /// 每个已配置或本机自动读取到的 CLI 现在能不能用（不能用时给出原因代码）。
    fn providers(&self) -> Vec<Value> {
        let config = self.configuration();
        [(Provider::Codex,&config.codex_executable),(Provider::ClaudeCode,&config.claude_executable)].into_iter()
         .filter(|(kind,path)|!path.trim().is_empty()||(self.detector)(*kind).is_some())
         .map(|(kind,path)|{
            let resolved=resolve_with_detector(path,kind,&*self.detector).ok();
            let source=resolved.as_ref().map(|(_,src)|src.as_str()).unwrap_or("configured");
            let exe_str=resolved.as_ref().map(|(exe,_)|exe.to_string_lossy().into_owned());
            let version=resolved.as_ref().and_then(|(exe,_)|self.snapshot(kind,exe).ok()).map(|s|s.version);
            match self.prepare(kind,"检测") {
              Ok(_)=>json!({"provider":kind.as_str(),"ready":true,"version":version,"source":source,"executable":exe_str}),
              Err(error)=>json!({"provider":kind.as_str(),"ready":false,"version":version,"source":source,"executable":exe_str,"error_code":error.code()}),
            }
         }).collect()
    }
    #[cfg(test)]
    fn fake_for_tests(
        api: &Arc<ApiService>,
        db: Arc<Mutex<RuntimeDb>>,
        settings_file: PathBuf,
        sensitive: Vec<PathBuf>,
        snapshots: [Option<CapabilitySnapshot>; 2],
        runner: Arc<dyn AgentRunner>,
    ) -> Arc<Self> {
        Self::fake_for_tests_with_detector(
            api,
            db,
            settings_file,
            sensitive,
            snapshots,
            Arc::new(|_| None),
            runner,
        )
    }
    #[cfg(test)]
    fn fake_for_tests_with_detector(
        api: &Arc<ApiService>,
        db: Arc<Mutex<RuntimeDb>>,
        settings_file: PathBuf,
        sensitive: Vec<PathBuf>,
        snapshots: [Option<CapabilitySnapshot>; 2],
        detector: CliDetector,
        runner: Arc<dyn AgentRunner>,
    ) -> Arc<Self> {
        let probe: CapabilityProbe = Arc::new(move |kind, _: &Path| {
            snapshots[match kind {
                Provider::Codex => 0,
                Provider::ClaudeCode => 1,
            }]
            .clone()
        });
        Arc::new_cyclic(|me| Self {
            api: Arc::downgrade(api),
            db,
            settings_file,
            sensitive,
            probe,
            detector,
            snapshots: Mutex::new(HashMap::new()),
            runner,
            running: Mutex::new(HashMap::new()),
            me: me.clone(),
        })
    }
}
impl AgentRuns for AgentCoordinator {
    fn tools(&self) -> Value {
        let names = [
            ("agent_codex", Provider::Codex),
            ("agent_claude_code", Provider::ClaudeCode),
        ];
        let tools=names.iter().filter(|(_,kind)|self.prepare(*kind,"测试").is_ok()).map(|(name,_)|{
        json!({"name":name,"description":"让本机 Agent 在选定的工作目录里完成任务（可以读写该目录的文件），每次都要在电脑上批准；使用已安装 CLI 的账户，可能产生模型费用。",
            "inputSchema":{"type":"object","properties":{"task":{"type":"string","maxLength":4000}},"required":["task"],"additionalProperties":false}})
    }).collect::<Vec<_>>();
        json!({"tools":tools})
    }
    fn call(&self, context: &CallerContext, name: &str, arguments: &Value) -> ToolCallResult {
        let Some(api) = self.api.upgrade() else {
            return ToolCallResult::err("MOSS 已关闭");
        };
        let permit = match api.agent_voice_permit(context) {
            Ok(p) => p,
            Err(_) => return ToolCallResult::err("设备授权已失效"),
        };
        let provider = match Self::kind_from(name) {
            Ok(p) => p,
            Err(_) => return ToolCallResult::err("Agent 工具不存在"),
        };
        let Some(task) = arguments.get("task").and_then(Value::as_str) else {
            return ToolCallResult::err("请提供 Agent 任务描述");
        };
        let plan = match self.prepare(provider, task) {
            Ok(plan) => plan,
            Err(e) => return ToolCallResult::err(Self::err_code(e)),
        };
        let CallerContext::Voice {
            device_id,
            session_id,
        } = context
        else {
            return ToolCallResult::err("必须来自授权的语音设备");
        };
        let Some(service) = api.agent_approvals() else {
            return ToolCallResult::err("桌面审批服务不可用");
        };
        let active = match service.list(Some(laser_contracts::ApprovalStatus::Pending), 512) {
            Ok(active) => active,
            Err(_) => return ToolCallResult::err("无法验证审批队列"),
        };
        if active.len() >= 256
            || active
                .iter()
                .filter(|r| {
                    r.source == ApprovalSource::Voice && r.session_id.as_deref() == Some(device_id)
                })
                .count()
                >= 16
        {
            return ToolCallResult::err("待审批队列已满");
        }
        let snapshot_version = self
            .snapshot(provider, &plan.executable)
            .map(|s| s.version)
            .unwrap_or_default();
        let request = json!({"device_id":device_id,"voice_session_id":session_id,"task":task,
        "workspace":plan.workspace.to_string_lossy(),"executable":plan.executable.to_string_lossy(),
        "version":snapshot_version,
        "config_revision":plan.config_revision,"task_digest":intent_digest(&plan)})
        .as_object()
        .cloned()
        .unwrap_or_default();
        let summary = format!(
            "请求 {} 在工作目录 {} 执行任务（可修改该目录的文件）：{}；使用本机 CLI 账户，可能产生费用",
            provider.as_str(),
            plan.workspace
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(""),
            task.chars().take(110).collect::<String>()
        );
        let record = match service.create(NewApproval {
            owner: Some(json!([device_id, session_id]).to_string()),
            voice_permit: Some(permit),
            kind: ApprovalKind::AgentRun,
            target_record: name.into(),
            source: ApprovalSource::Voice,
            session_id: Some(device_id.clone()),
            request_id: None,
            summary,
            facts: request.clone(),
            connection: ApprovalConnection::Agent {
                provider: provider.as_str().into(),
                fingerprint: plan.config_revision.clone(),
            },
            request,
            client: audit_client(context),
            removed_fields: Vec::new(),
        }) {
            Ok(r) => r,
            Err(e) => return ToolCallResult::err(e.message()),
        };
        let speech = "已发送到电脑待批准，批准前不会执行 Agent 任务。";
        ToolCallResult::with_detail(true,speech,json!({"result":{"status":"approval_required","approval_id":record.approval_id,"speech":speech}}).to_string())
    }
    fn current_digest(&self, record: &ApprovalRecord) -> Result<String, ApprovalError> {
        let (plan, _, _) = self.prepare_record(record)?;
        let facts = record.request.as_ref().ok_or(ApprovalError::Mismatch)?;
        Ok(approval_digest(
            record.kind,
            &record.target_record,
            facts,
            &ApprovalConnection::Agent {
                provider: plan.provider.as_str().into(),
                fingerprint: plan.config_revision,
            },
        ))
    }
    fn execute(&self, record: &ApprovalRecord) -> (u16, Value) {
        let (plan, ctx, permit) = match self.prepare_record(record) {
            Ok(p) => p,
            Err(e) => return Self::response(e.http_status(), &e.message(), e.code()),
        };
        let Some(api) = self.api.upgrade() else {
            return Self::response(503, "MOSS 已停止", "agent_unavailable");
        };
        if api.agent_approvals().is_none() {
            return Self::response(503, "审批服务不可用", "agent_unavailable");
        }
        let Some(me) = self.me.upgrade() else {
            return Self::response(503, "Agent 服务不可用", "agent_unavailable");
        };
        let digest = approval_digest(
            record.kind,
            &record.target_record,
            record.request.as_ref().unwrap(),
            &ApprovalConnection::Agent {
                provider: plan.provider.as_str().into(),
                fingerprint: plan.config_revision.clone(),
            },
        );
        let id = format!("agentrun-{}", record.approval_id);
        let run = AgentRunRecord {
            id: id.clone(),
            approval_id: record.approval_id.clone(),
            provider: plan.provider.as_str().into(),
            workspace: plan.workspace.to_string_lossy().into_owned(),
            task_digest: intent_digest(&plan),
            config_revision: plan.config_revision.clone(),
            status: "running".into(),
            output: String::new(),
            truncated: false,
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
            finished_at: None,
            error_code: None,
        };
        if let Err(e) = api
            .agent_approvals()
            .map_or(Err(ApprovalError::Mismatch), |approvals| {
                approvals.consume_agent_run(&record.approval_id, &digest, &run, &permit)
            })
        {
            return Self::response(e.http_status(), &e.message(), e.code());
        }
        let stop = Arc::new(AtomicBool::new(false));
        if let Ok(mut running) = self.running.lock() {
            running.insert(id.clone(), stop.clone());
        }
        // 批准后在后台运行，审批卡不必等最长 5 分钟；结果写进运行记录，在语音中心 Agent 卡查看。
        // 批准后在后台运行，审批卡不必等最长 5 分钟；结果写进运行记录，在语音中心 Agent 卡查看。
        // spawn 失败时闭包被丢弃，api 句柄不归还，先克隆一份用于失败收口。
        let api_on_spawn_failure = api.clone();
        let id_on_spawn_failure = id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("agent-{id}"))
            .spawn(move || {
                // 停止与语音授权是原子位，每次轮询立即检查；会话、严格模式与配置（工作目录、CLI、开关）
                // 每秒核对一次，改了就停。核对全部收进这 1 秒窗口，避免运行器 40ms 轮询反复抢全局 DB 锁
                // （2026-10-17 审查发现；首次调用立即做全套核对）。
                let checked_at = std::cell::Cell::new(None::<std::time::Instant>);
                let checks_ok = std::cell::Cell::new(false);
                let alive = || {
                    if stop.load(Ordering::Acquire) || !permit.load(Ordering::Acquire) {
                        return false;
                    }
                    if checked_at.get().is_some_and(|checked| {
                        checked.elapsed() < std::time::Duration::from_secs(1)
                    }) {
                        return checks_ok.get();
                    }
                    checked_at.set(Some(std::time::Instant::now()));
                    let ok = api.voice_session_allowed(&ctx_device(&ctx), &ctx_session(&ctx))
                        && api.strict_mode()
                        && me
                            .prepare(plan.provider, &plan.stdin)
                            .is_ok_and(|current| current.config_revision == plan.config_revision);
                    checks_ok.set(ok);
                    ok
                };
                let result = me.runner.run(&plan, &alive);
                let stopped = stop.load(Ordering::Acquire);
                let (status, output, code) = match result {
                    Ok(output) if alive() => ("completed", output, None),
                    Ok(output) => (
                        "cancelled",
                        output,
                        Some(if stopped {
                            "agent_stopped"
                        } else {
                            "agent_permission_expired"
                        }),
                    ),
                    Err((error, output)) => (
                        if stopped { "cancelled" } else { "failed" },
                        output,
                        Some(if stopped {
                            "agent_stopped"
                        } else {
                            error.code()
                        }),
                    ),
                };
                let (truncated, output) = bounded_text(&output, 256 * 1024);
                if let Some(approvals) = api.agent_approvals() {
                    let _ = approvals.finish_agent_run(&id, status, &output, truncated, code);
                }
                if let Ok(mut running) = me.running.lock() {
                    running.remove(&id);
                }
            });
        if spawned.is_err() {
            // 运行记录已经以 running 写入：必须当场收口，否则本次进程内既停不了也永远显示运行中
            // （2026-10-17 审查发现；此前只返回 500，把记录留在 running）。
            if let Ok(mut running) = self.running.lock() {
                running.remove(&id_on_spawn_failure);
            }
            if let Some(approvals) = api_on_spawn_failure.agent_approvals() {
                let _ = approvals.finish_agent_run(
                    &id_on_spawn_failure,
                    "failed",
                    "",
                    false,
                    Some("agent_thread_spawn_failed"),
                );
            }
            return Self::response(
                500,
                "无法启动 Agent 任务线程，已记为失败",
                "agent_thread_spawn_failed",
            );
        }
        (
            200,
            json!({"success":true,"result":"已开始运行 Agent 任务；完成后在语音中心的 Agent 卡查看结果，运行中可以随时停止"}),
        )
    }
    fn stop(&self, run_id: &str) -> bool {
        AgentCoordinator::stop(self, run_id)
    }
    fn status(&self) -> Value {
        let config = self.configuration();
        let strict = self.api.upgrade().is_some_and(|api| api.strict_mode());
        let running = self
            .running
            .lock()
            .map(|r| r.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let records = self
            .db
            .lock()
            .ok()
            .and_then(|db| db.agent_runs(MAX_RUN_RECORDS).ok())
            .unwrap_or_default();
        let safe = records
            .into_iter()
            .map(|r| {
                json!({"id":r.id,"approval_id":r.approval_id,"provider":r.provider,
        "workspace_name":Path::new(&r.workspace).file_name().and_then(|s|s.to_str()).unwrap_or(""),
        "status":r.status,"output":r.output,"truncated":r.truncated,"started_at":r.started_at,
        "finished_at":r.finished_at,"error_code":r.error_code,"stoppable":running.contains(&r.id)})
            })
            .collect::<Vec<_>>();
        let detected = json!({
            "codex_executable":(self.detector)(Provider::Codex).map(|p|p.to_string_lossy().into_owned()),
            "claude_executable":(self.detector)(Provider::ClaudeCode).map(|p|p.to_string_lossy().into_owned()),
        });
        json!({"enabled":config.enabled && strict,"configured":config.enabled,"strict_mode":strict,
        "detected":detected,"providers":self.providers(),"runs":safe,"reason":if strict {"configured_or_pending"}else{"agent_strict_mode_required"}})
    }
}
fn ctx_device(ctx: &CallerContext) -> String {
    match ctx {
        CallerContext::Voice { device_id, .. } => device_id.clone(),
        _ => String::new(),
    }
}
fn ctx_session(ctx: &CallerContext) -> String {
    match ctx {
        CallerContext::Voice { session_id, .. } => session_id.clone(),
        _ => String::new(),
    }
}
fn bounded_text(text: &str, limit: usize) -> (bool, String) {
    if text.len() <= limit {
        return (false, text.into());
    }
    let boundary = text
        .char_indices()
        .map(|(n, _)| n)
        .take_while(|n| *n <= limit)
        .last()
        .unwrap_or(0);
    (true, text[..boundary].into())
}
#[cfg(test)]
mod tests {
    use super::*;
    use laser_api::approval::ApprovalService;
    use laser_contracts::{ApprovalStatus, builtin_material_library};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    /// 运行到被停止为止的假执行器：验证“停止”能让正在运行的任务结束并记为已停止。
    struct BlockingRunner;
    impl AgentRunner for BlockingRunner {
        fn run(
            &self,
            _: &AgentPlan,
            alive: &dyn Fn() -> bool,
        ) -> Result<String, (AgentError, String)> {
            let started = std::time::Instant::now();
            while alive() && started.elapsed() < std::time::Duration::from_secs(5) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err((AgentError::PermissionExpired, "partial output".into()))
        }
    }
    struct FakeRunner {
        calls: AtomicUsize,
    }
    impl AgentRunner for FakeRunner {
        fn run(
            &self,
            plan: &AgentPlan,
            alive: &dyn Fn() -> bool,
        ) -> Result<String, (AgentError, String)> {
            assert!(alive(), "取消/撤销后绝不能启动任务");
            assert_eq!(plan.provider, Provider::Codex);
            assert_eq!(plan.argv.last().map(String::as_str), Some("-"));
            assert!(!plan.argv.iter().any(|arg| arg.contains("read README")));
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            Ok("假执行器已读取文件列表".into())
        }
    }

    fn approval_http(api: &ApiService, path: &str, digest: &str) -> (u16, Value) {
        let body = json!({"digest":digest}).to_string();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = api.handle_http_with_ctx(req.as_bytes(), CallerContext::Desktop);
        let cut = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let status = String::from_utf8_lossy(&response[..cut])
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        (
            status,
            serde_json::from_slice(&response[cut + 4..]).unwrap(),
        )
    }

    /// 运行中的任务可以在桌面停止，记录为已停止并保留已有输出（2026-10-15）。
    #[test]
    fn running_task_can_be_stopped_from_desktop() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let approvals = Arc::new(ApprovalService::new(db.clone()));
        let api = Arc::new(
            ApiService::new(
                laser_runtime::LaserSettings {
                    default_connection_mode: "network".into(),
                    engraving_mode: "raster".into(),
                    web_host: "127.0.0.1".into(),
                    web_port: 9666,
                    material_params_file: None,
                },
                builtin_material_library(),
            )
            .with_runtime_db(db.clone())
            .with_approval_service(approvals.clone())
            .with_strict_mode(true),
        );
        db.lock()
            .unwrap()
            .voice_device_seen("d", "c", "", "", "", 100.0)
            .unwrap();
        api.set_voice_device_status("d", laser_runtime::VOICE_DEVICE_ALLOWED, 101.0)
            .unwrap();
        let _permit = api.register_voice_session("d", "c", "s").unwrap();
        let workdir = dir.path().join("agent-workspace");
        std::fs::create_dir(&workdir).unwrap();
        let binary = dir.path().join(Provider::Codex.expected_binary());
        std::fs::write(&binary, b"fixture").unwrap();
        let mut settings = moss_voice::settings::VoiceSettings::default();
        settings.builtin_agents = moss_voice::settings::BuiltinAgents {
            enabled: true,
            workspace: workdir.to_string_lossy().into_owned(),
            codex_executable: binary.to_string_lossy().into_owned(),
            claude_executable: String::new(),
        };
        let path = dir.path().join("settings.json");
        moss_voice::settings::save(&path, &settings).unwrap();
        let help = "--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        let manager = AgentCoordinator::fake_for_tests(
            &api,
            db.clone(),
            path,
            vec![],
            [
                Some(CapabilitySnapshot {
                    version: "codex-cli 0.157.1".into(),
                    help: help.into(),
                }),
                None,
            ],
            Arc::new(BlockingRunner),
        );
        api.set_agent_runs(manager.clone()).unwrap();
        let ctx = CallerContext::Voice {
            device_id: "d".into(),
            session_id: "s".into(),
        };
        assert!(
            api.call_agent_tool(&ctx, "agent_codex", &json!({"task":"long task"}))
                .success
        );
        let pending = approvals.list(Some(ApprovalStatus::Pending), 5).unwrap();
        assert_eq!(
            approval_http(
                &api,
                &format!("/v1/approvals/{}/approve", pending[0].approval_id),
                &pending[0].digest
            )
            .0,
            200
        );
        let run_id = format!("agentrun-{}", pending[0].approval_id);
        assert_eq!(manager.status()["runs"][0]["stoppable"], true);
        assert!(manager.stop(&run_id));
        let started = std::time::Instant::now();
        while db.lock().unwrap().agent_runs(50).unwrap()[0].status == "running" {
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let run = &db.lock().unwrap().agent_runs(50).unwrap()[0];
        assert_eq!(
            (
                run.status.as_str(),
                run.error_code.as_deref(),
                run.output.as_str()
            ),
            ("cancelled", Some("agent_stopped"), "partial output")
        );
        assert!(!manager.stop(&run_id), "已结束的任务不能再停止");
    }

    /// 设备授权在数据库侧被撤销后，运行中的任务必须被 1 秒节流窗口内的核对停下并记为失败
    /// （2026-10-17：会话/严格模式/配置核对改为每秒一次，此测试防止 DB 级撤销被漏检）。
    #[test]
    fn revoked_device_stops_a_running_task_within_the_throttled_window() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let approvals = Arc::new(ApprovalService::new(db.clone()));
        let api = Arc::new(
            ApiService::new(
                laser_runtime::LaserSettings {
                    default_connection_mode: "network".into(),
                    engraving_mode: "raster".into(),
                    web_host: "127.0.0.1".into(),
                    web_port: 9666,
                    material_params_file: None,
                },
                builtin_material_library(),
            )
            .with_runtime_db(db.clone())
            .with_approval_service(approvals.clone())
            .with_strict_mode(true),
        );
        db.lock()
            .unwrap()
            .voice_device_seen("d", "c", "", "", "", 100.0)
            .unwrap();
        api.set_voice_device_status("d", laser_runtime::VOICE_DEVICE_ALLOWED, 101.0)
            .unwrap();
        let _permit = api.register_voice_session("d", "c", "s").unwrap();
        let workdir = dir.path().join("agent-workspace");
        std::fs::create_dir(&workdir).unwrap();
        let binary = dir.path().join(Provider::Codex.expected_binary());
        std::fs::write(&binary, b"fixture").unwrap();
        let mut settings = moss_voice::settings::VoiceSettings::default();
        settings.builtin_agents = moss_voice::settings::BuiltinAgents {
            enabled: true,
            workspace: workdir.to_string_lossy().into_owned(),
            codex_executable: binary.to_string_lossy().into_owned(),
            claude_executable: String::new(),
        };
        let path = dir.path().join("settings.json");
        moss_voice::settings::save(&path, &settings).unwrap();
        let help = "--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        let manager = AgentCoordinator::fake_for_tests(
            &api,
            db.clone(),
            path,
            vec![],
            [
                Some(CapabilitySnapshot {
                    version: "codex-cli 0.157.1".into(),
                    help: help.into(),
                }),
                None,
            ],
            Arc::new(BlockingRunner),
        );
        api.set_agent_runs(manager.clone()).unwrap();
        let ctx = CallerContext::Voice {
            device_id: "d".into(),
            session_id: "s".into(),
        };
        assert!(
            api.call_agent_tool(&ctx, "agent_codex", &json!({"task":"long task"}))
                .success
        );
        let pending = approvals.list(Some(ApprovalStatus::Pending), 5).unwrap();
        assert_eq!(
            approval_http(
                &api,
                &format!("/v1/approvals/{}/approve", pending[0].approval_id),
                &pending[0].digest
            )
            .0,
            200
        );
        assert_eq!(
            db.lock().unwrap().agent_runs(50).unwrap()[0].status,
            "running"
        );
        // 运行中撤销设备授权：不碰停止位与语音原子位，只有 DB 状态变化
        api.set_voice_device_status("d", laser_runtime::VOICE_DEVICE_REVOKED, 150.0)
            .unwrap();
        let started = std::time::Instant::now();
        while db.lock().unwrap().agent_runs(50).unwrap()[0].status == "running" {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "节流核对没有在预算内停下任务"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let run = &db.lock().unwrap().agent_runs(50).unwrap()[0];
        assert_eq!(
            (run.status.as_str(), run.error_code.as_deref()),
            ("failed", Some("agent_permission_expired"))
        );
    }

    #[test]
    fn fake_runner_is_not_spawned_until_trusted_desktop_approval_then_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let approvals = Arc::new(ApprovalService::new(db.clone()));
        let api = Arc::new(
            ApiService::new(
                laser_runtime::LaserSettings {
                    default_connection_mode: "network".into(),
                    engraving_mode: "raster".into(),
                    web_host: "127.0.0.1".into(),
                    web_port: 9666,
                    material_params_file: None,
                },
                builtin_material_library(),
            )
            .with_runtime_db(db.clone())
            .with_approval_service(approvals.clone()),
        );
        let device = "agent-device";
        let session = "agent-session";
        db.lock()
            .unwrap()
            .voice_device_seen(device, "test-client", "", "", "", 100.0)
            .unwrap();
        api.set_voice_device_status(device, laser_runtime::VOICE_DEVICE_ALLOWED, 101.0)
            .unwrap();
        let permit = api
            .register_voice_session(device, "test-client", session)
            .unwrap();

        let workdir = dir.path().join("agent-workspace");
        std::fs::create_dir(&workdir).unwrap();
        let sensitive = dir.path().join("private-data");
        std::fs::create_dir(&sensitive).unwrap();
        let binary = dir.path().join(Provider::Codex.expected_binary());
        std::fs::write(&binary, b"not-executed-fixture").unwrap();
        let mut settings = moss_voice::settings::VoiceSettings::default();
        settings.builtin_agents = moss_voice::settings::BuiltinAgents {
            enabled: true,
            workspace: workdir.to_string_lossy().into_owned(),
            codex_executable: binary.to_string_lossy().into_owned(),
            claude_executable: String::new(),
        };
        let path = dir.path().join("settings.json");
        moss_voice::settings::save(&path, &settings).unwrap();
        let help = "--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        let fake = Arc::new(FakeRunner {
            calls: AtomicUsize::new(0),
        });
        let manager = AgentCoordinator::fake_for_tests(
            &api,
            db.clone(),
            path.clone(),
            vec![sensitive],
            [
                Some(CapabilitySnapshot {
                    version: "codex-cli 0.157.1".into(),
                    help: help.into(),
                }),
                None,
            ],
            fake.clone(),
        );
        api.set_agent_runs(manager.clone()).unwrap();
        let ctx = CallerContext::Voice {
            device_id: device.into(),
            session_id: session.into(),
        };
        // 严格模式关闭时不提供 Agent 工具（2026-10-15：Agent 只在严格模式下运行）
        assert_eq!(manager.tools()["tools"].as_array().unwrap().len(), 0);
        assert_eq!(manager.status()["reason"], "agent_strict_mode_required");
        api.set_strict_mode(true);
        assert_eq!(manager.tools()["tools"].as_array().unwrap().len(), 1);
        let task = "read README and summarize; do not write";
        let sent = api.call_agent_tool(&ctx, "agent_codex", &json!({"task":task}));
        assert!(sent.success, "{}", sent.result);
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst), 0);
        let pending = approvals.list(Some(ApprovalStatus::Pending), 5).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, ApprovalKind::AgentRun);
        let id = &pending[0].approval_id;
        assert_eq!(
            approval_http(
                &api,
                &format!("/v1/approvals/{id}/approve"),
                "not-the-digest"
            )
            .0,
            409
        );
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst), 0);
        let (status, result) = approval_http(
            &api,
            &format!("/v1/approvals/{id}/approve"),
            &pending[0].digest,
        );
        assert_eq!(status, 200, "{}", result);
        // 批准后在后台运行：等运行记录写完
        let started = std::time::Instant::now();
        while db.lock().unwrap().agent_runs(50).unwrap()[0].status == "running" {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "后台任务没有结束"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            db.lock().unwrap().agent_runs(50).unwrap()[0].status,
            "completed"
        );
        assert_ne!(
            approval_http(
                &api,
                &format!("/v1/approvals/{id}/approve"),
                &pending[0].digest
            )
            .0,
            200
        );
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst), 1);

        // Change configuration between preparing and approval: digest recalculation must fail.
        let pending2 =
            api.call_agent_tool(&ctx, "agent_codex", &json!({"task":"read another file"}));
        assert!(pending2.success);
        let remaining = approvals.list(Some(ApprovalStatus::Pending), 5).unwrap();
        assert_eq!(remaining.len(), 1);
        settings.builtin_agents.workspace = dir
            .path()
            .join("missing-workspace")
            .to_string_lossy()
            .into_owned();
        moss_voice::settings::save(&path, &settings).unwrap();
        assert_ne!(
            approval_http(
                &api,
                &format!("/v1/approvals/{}/approve", remaining[0].approval_id),
                &remaining[0].digest
            )
            .0,
            200
        );
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst), 1);

        permit.store(false, AtomicOrdering::Release);
        assert!(!api.voice_session_allowed(device, session));
        assert!(
            !api.call_agent_tool(&ctx, "agent_codex", &json!({"task":"should be denied"}))
                .success
        );
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn auto_detected_and_switched_device_clis_work_with_approval_binding() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let approvals = Arc::new(ApprovalService::new(db.clone()));
        let api = Arc::new(
            ApiService::new(
                laser_runtime::LaserSettings {
                    default_connection_mode: "network".into(),
                    engraving_mode: "raster".into(),
                    web_host: "127.0.0.1".into(),
                    web_port: 9666,
                    material_params_file: None,
                },
                builtin_material_library(),
            )
            .with_runtime_db(db.clone())
            .with_approval_service(approvals.clone())
            .with_strict_mode(true),
        );
        db.lock()
            .unwrap()
            .voice_device_seen("d", "c", "", "", "", 100.0)
            .unwrap();
        api.set_voice_device_status("d", laser_runtime::VOICE_DEVICE_ALLOWED, 101.0)
            .unwrap();
        let _permit = api.register_voice_session("d", "c", "s").unwrap();

        let workdir = dir.path().join("agent-workspace");
        std::fs::create_dir(&workdir).unwrap();
        let auto_codex = dir.path().join(Provider::Codex.expected_binary());
        std::fs::write(&auto_codex, b"native-codex").unwrap();
        let mut settings = moss_voice::settings::VoiceSettings::default();
        // 留空 codex_executable：验证本机自动读取；claude_executable 填异构设备路径：验证换设备自动适配
        let auto_claude = dir.path().join(Provider::ClaudeCode.expected_binary());
        std::fs::write(&auto_claude, b"native-claude").unwrap();
        let foreign_claude = if cfg!(windows) {
            "/opt/homebrew/bin/claude"
        } else {
            r"C:\Users\alice\.local\bin\claude.exe"
        };
        settings.builtin_agents = moss_voice::settings::BuiltinAgents {
            enabled: true,
            workspace: workdir.to_string_lossy().into_owned(),
            codex_executable: String::new(),
            claude_executable: foreign_claude.into(),
        };
        let path = dir.path().join("settings.json");
        moss_voice::settings::save(&path, &settings).unwrap();
        let codex_help = "--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        let claude_help = "--print --restricted --permission-mode --permission-prompts --tools --strict-mcp-config --mcp-config --setting-sources --no-session-persistence --output-format --verbose";
        let detector_codex = auto_codex.clone();
        let detector_claude = auto_claude.clone();
        let detector: CliDetector = Arc::new(move |kind| match kind {
            Provider::Codex => Some(detector_codex.clone()),
            Provider::ClaudeCode => Some(detector_claude.clone()),
        });
        let fake = Arc::new(FakeRunner {
            calls: AtomicUsize::new(0),
        });
        let manager = AgentCoordinator::fake_for_tests_with_detector(
            &api,
            db.clone(),
            path,
            vec![],
            [
                Some(CapabilitySnapshot {
                    version: "codex-cli 0.157.1".into(),
                    help: codex_help.into(),
                }),
                Some(CapabilitySnapshot {
                    version: "2.1.283 (Claude Code)".into(),
                    help: claude_help.into(),
                }),
            ],
            detector,
            fake.clone(),
        );
        api.set_agent_runs(manager.clone()).unwrap();

        let status = manager.status();
        assert_eq!(
            status["detected"]["codex_executable"],
            auto_codex.to_string_lossy().as_ref()
        );
        assert_eq!(status["providers"][0]["source"], "auto");
        assert_eq!(status["providers"][0]["ready"], true);
        assert_eq!(status["providers"][1]["source"], "auto_fallback");
        assert_eq!(status["providers"][1]["ready"], true);
        assert_eq!(manager.tools()["tools"].as_array().unwrap().len(), 2);

        let ctx = CallerContext::Voice {
            device_id: "d".into(),
            session_id: "s".into(),
        };
        assert!(
            api.call_agent_tool(&ctx, "agent_codex", &json!({"task":"auto-detected task"}))
                .success
        );
        let pending = approvals.list(Some(ApprovalStatus::Pending), 5).unwrap();
        assert_eq!(
            pending[0].request.as_ref().unwrap()["executable"],
            auto_codex
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(
            approval_http(
                &api,
                &format!("/v1/approvals/{}/approve", pending[0].approval_id),
                &pending[0].digest
            )
            .0,
            200
        );
        let started = std::time::Instant::now();
        while db.lock().unwrap().agent_runs(50).unwrap()[0].status == "running" {
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst), 1);
    }
    #[test]
    fn snapshot_drops_lock_during_probe_caches_none_and_invalidates_on_binary_change() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let approvals = Arc::new(ApprovalService::new(db.clone()));
        let api = Arc::new(
            ApiService::new(
                laser_runtime::LaserSettings {
                    default_connection_mode: "network".into(),
                    engraving_mode: "raster".into(),
                    web_host: "127.0.0.1".into(),
                    web_port: 9666,
                    material_params_file: None,
                },
                builtin_material_library(),
            )
            .with_runtime_db(db.clone())
            .with_approval_service(approvals)
            .with_strict_mode(true),
        );
        let workdir = dir.path().join("agent-workspace");
        std::fs::create_dir(&workdir).unwrap();
        let binary = dir.path().join(Provider::Codex.expected_binary());
        std::fs::write(&binary, b"broken-v1").unwrap();
        let mut settings = moss_voice::settings::VoiceSettings::default();
        settings.builtin_agents = moss_voice::settings::BuiltinAgents {
            enabled: true,
            workspace: workdir.to_string_lossy().into_owned(),
            codex_executable: binary.to_string_lossy().into_owned(),
            claude_executable: String::new(),
        };
        let path = dir.path().join("settings.json");
        moss_voice::settings::save(&path, &settings).unwrap();

        let probe_calls = Arc::new(AtomicUsize::new(0));
        let allow_success = Arc::new(AtomicBool::new(false));
        let weak_holder: Arc<Mutex<Option<Weak<AgentCoordinator>>>> = Arc::new(Mutex::new(None));
        let calls_ref = probe_calls.clone();
        let allow_ref = allow_success.clone();
        let weak_ref = weak_holder.clone();
        let codex_help = "--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        let probe: CapabilityProbe = Arc::new(move |_, _| {
            calls_ref.fetch_add(1, AtomicOrdering::SeqCst);
            if let Some(coord) = weak_ref.lock().unwrap().as_ref().and_then(Weak::upgrade) {
                assert!(
                    coord.snapshots.try_lock().is_ok(),
                    "snapshots lock must NOT be held while probe subprocesses run"
                );
            }
            if allow_ref.load(AtomicOrdering::Acquire) {
                Some(CapabilitySnapshot {
                    version: "codex-cli 0.159.0".into(),
                    help: codex_help.into(),
                })
            } else {
                None
            }
        });
        let detector: CliDetector = Arc::new(|_| None);
        let runner: Arc<dyn AgentRunner> = Arc::new(FakeRunner {
            calls: AtomicUsize::new(0),
        });
        let manager = Arc::new_cyclic(|me| AgentCoordinator {
            api: Arc::downgrade(&api),
            db: db.clone(),
            settings_file: path,
            sensitive: vec![],
            probe,
            detector,
            snapshots: Mutex::new(HashMap::new()),
            runner,
            running: Mutex::new(HashMap::new()),
            me: me.clone(),
        });
        *weak_holder.lock().unwrap() = Some(Arc::downgrade(&manager));
        api.set_agent_runs(manager.clone()).unwrap();

        // Unchanged broken CLI: multiple status() and tools() calls invoke probe only once
        let s1 = manager.status();
        let s2 = manager.status();
        let _ = manager.tools();
        assert_eq!(probe_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(s1["providers"][0]["ready"], false);
        assert_eq!(s2["providers"][0]["error_code"], "agent_capability_missing");

        // Upgrade CLI (file length changes): cache invalidates and re-probes once
        allow_success.store(true, AtomicOrdering::Release);
        std::fs::write(&binary, b"working-upgraded-cli-binary-v2").unwrap();
        let s3 = manager.status();
        let s4 = manager.status();
        assert_eq!(probe_calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(s3["providers"][0]["ready"], true);
        assert_eq!(s4["providers"][0]["version"], "codex-cli 0.159.0");

        // Touch mtime without changing file length: cache invalidates and re-probes once
        let old_mtime = std::fs::metadata(&binary).unwrap().modified().unwrap();
        std::fs::File::options()
            .write(true)
            .open(&binary)
            .unwrap()
            .set_modified(old_mtime + std::time::Duration::from_secs(60))
            .unwrap();
        let _ = manager.status();
        let _ = manager.status();
        assert_eq!(probe_calls.load(AtomicOrdering::SeqCst), 3);
    }
}
