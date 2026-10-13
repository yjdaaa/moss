//! NV13 host-owned approval bridge. Production remains inert until OS sandbox
//! negative tests have independently passed; help/version checks are insufficient.
use std::path::{Path,PathBuf};
use std::sync::{Arc,Mutex,Weak,atomic::{AtomicBool,Ordering}};
use laser_api::{ApiService,approval::{ApprovalError,NewApproval,approval_digest},agent_runs::AgentRuns};
use laser_contracts::{ApprovalConnection,ApprovalKind,ApprovalRecord,ApprovalSource,CallerContext,ToolCallResult,audit_client};
use laser_runtime::{RuntimeDb,AgentRunRecord};
use serde_json::{Value,json};
use super::{Provider,ReadOnlyPlan,AgentError,build_plan,executable,workspace,intent_digest,MAX_RUN_RECORDS};

#[derive(Clone)]
pub struct CapabilitySnapshot{pub version:String,pub help:String}
pub trait AgentRunner:Send+Sync{
    /// This is only called after atomic desktop approval consumption.
    /// Real subprocess implementation stays disabled until separately authorized
    /// and verified with negative sandbox, process-tree and revocation tests.
    fn run(&self,plan:&ReadOnlyPlan,alive:&dyn Fn()->bool)->Result<String,AgentError>;
}
struct DisabledRunner;
impl AgentRunner for DisabledRunner{
 fn run(&self,_:&ReadOnlyPlan,_:&dyn Fn()->bool)->Result<String,AgentError>{Err(AgentError::SandboxUnverified)}
}
pub struct AgentCoordinator {
    api:Weak<ApiService>,
    db:Arc<Mutex<RuntimeDb>>,
    settings_file:PathBuf,
    sensitive:Vec<PathBuf>,
    sandbox_verified:bool,
    snapshots:[Option<CapabilitySnapshot>;2],
    runner:Arc<dyn AgentRunner>,
}
impl AgentCoordinator {
    pub fn attach(api:&Arc<ApiService>,db:Arc<Mutex<RuntimeDb>>,settings_file:PathBuf,
                  sensitive:Vec<PathBuf>)->Result<Arc<Self>,String>{
        let manager=Arc::new(Self {
            api:Arc::downgrade(api),db,settings_file,sensitive,
            sandbox_verified:super::REAL_CLI_SANDBOX_VERIFIED,
            snapshots:[None,None],runner:Arc::new(DisabledRunner),
        });
        api.set_agent_runs(manager.clone())?;
        Ok(manager)
    }
    fn configuration(&self)->moss_voice::settings::BuiltinAgents{
       moss_voice::settings::load(&self.settings_file).builtin_agents
    }
    fn snapshot(&self,kind:Provider)->Result<&CapabilitySnapshot,AgentError>{
       self.snapshots[match kind{Provider::Codex=>0,Provider::ClaudeCode=>1}]
          .as_ref().ok_or(AgentError::CapabilityMissing)
    }
    fn prepare(&self,kind:Provider,task:&str)->Result<ReadOnlyPlan,AgentError>{
       if !self.sandbox_verified{return Err(AgentError::SandboxUnverified);}
       let config=self.configuration();
       if !config.enabled{return Err(AgentError::Disabled);}
       let cli=match kind{Provider::Codex=>&config.codex_executable,Provider::ClaudeCode=>&config.claude_executable};
       let executable=executable(Path::new(cli),kind)?;
       let workspace=workspace(Path::new(&config.workspace),&self.sensitive)?;
       let snapshot=self.snapshot(kind)?;
       build_plan(kind,executable,workspace,task,&snapshot.version,&snapshot.help)
    }
    fn kind_from(name:&str)->Result<Provider,AgentError>{
       match name{"agent_codex"=>Ok(Provider::Codex),"agent_claude_code"=>Ok(Provider::ClaudeCode),_=>Err(AgentError::Disabled)}
    }
    fn err_code(error:AgentError)->String {error.code().to_owned()}
    fn prepare_record(&self,record:&ApprovalRecord)->Result<(ReadOnlyPlan,CallerContext,Arc<AtomicBool>),ApprovalError>{
       if record.kind!=ApprovalKind::AgentRun||record.source!=ApprovalSource::Voice {
           return Err(ApprovalError::Mismatch);
       }
       let hidden=record.request.as_ref().ok_or(ApprovalError::Mismatch)?;
       let device=hidden.get("device_id").and_then(Value::as_str).ok_or(ApprovalError::Mismatch)?;
       let session=hidden.get("voice_session_id").and_then(Value::as_str).ok_or(ApprovalError::Mismatch)?;
       let task=hidden.get("task").and_then(Value::as_str).ok_or(ApprovalError::Mismatch)?;
       let provider=Self::kind_from(&record.target_record).map_err(|_|ApprovalError::Mismatch)?;
       if record.session_id.as_deref()!=Some(device) {
           return Err(ApprovalError::Mismatch);
       }
       let ctx=CallerContext::Voice{device_id:device.into(),session_id:session.into()};
       let api=self.api.upgrade().ok_or(ApprovalError::Mismatch)?;
       let permit=api.agent_voice_permit(&ctx)?;
       if !permit.load(Ordering::Acquire){return Err(ApprovalError::Expired);}
       let plan=self.prepare(provider,task).map_err(|_|ApprovalError::Mismatch)?;
       let expected=ApprovalConnection::Agent{provider:provider.as_str().into(),fingerprint:plan.config_revision.clone()};
       if record.connection.as_ref()!=Some(&expected)
          ||hidden.get("config_revision").and_then(Value::as_str)!=Some(plan.config_revision.as_str())
          ||hidden.get("task_digest").and_then(Value::as_str)!=Some(intent_digest(&plan).as_str()){
           return Err(ApprovalError::Mismatch);
       }
       Ok((plan,ctx,permit))
    }
    fn response(status:u16,message:&str,code:&str)->(u16,Value){
       (status,json!({"success":false,"result":message,"detail":{"error_code":code}}))
    }
    #[cfg(test)]
    fn fake_for_tests(api:&Arc<ApiService>,db:Arc<Mutex<RuntimeDb>>,settings_file:PathBuf,
          sensitive:Vec<PathBuf>,snapshots:[Option<CapabilitySnapshot>;2],runner:Arc<dyn AgentRunner>)->Arc<Self>{
       Arc::new(Self{api:Arc::downgrade(api),db,settings_file,sensitive,sandbox_verified:true,snapshots,runner})
    }
}
impl AgentRuns for AgentCoordinator{
 fn tools(&self)->Value{
    let names=[("agent_codex",Provider::Codex),("agent_claude_code",Provider::ClaudeCode)];
    let tools=names.iter().filter(|(_,kind)|self.prepare(*kind,"测试").is_ok()).map(|(name,_)|{
        json!({"name":name,"description":"对选定工作区发起需桌面批准的只读 Agent 任务；使用已安装 CLI 账户，可能产生模型费用。",
            "inputSchema":{"type":"object","properties":{"task":{"type":"string","maxLength":4000}},"required":["task"],"additionalProperties":false}})
    }).collect::<Vec<_>>();
    json!({"tools":tools})
 }
 fn call(&self,context:&CallerContext,name:&str,arguments:&Value)->ToolCallResult{
    let Some(api)=self.api.upgrade() else{return ToolCallResult::err("MOSS 已关闭")};
    let permit=match api.agent_voice_permit(context){Ok(p)=>p,Err(_)=>return ToolCallResult::err("设备授权已失效")};
    let provider=match Self::kind_from(name){Ok(p)=>p,Err(_)=>return ToolCallResult::err("Agent 工具不存在")};
    let Some(task)=arguments.get("task").and_then(Value::as_str) else{return ToolCallResult::err("请提供 Agent 任务描述")};
    let plan=match self.prepare(provider,task){Ok(plan)=>plan,Err(e)=>return ToolCallResult::err(Self::err_code(e))};
    let CallerContext::Voice{device_id,session_id}=context else{return ToolCallResult::err("必须来自授权的语音设备")};
    let Some(service)=api.agent_approvals() else{return ToolCallResult::err("桌面审批服务不可用")};
    let active=match service.list(Some(laser_contracts::ApprovalStatus::Pending),512){Ok(active)=>active,Err(_)=>return ToolCallResult::err("无法验证审批队列")};
    if active.len()>=256||active.iter().filter(|r|r.source==ApprovalSource::Voice&&r.session_id.as_deref()==Some(device_id)).count()>=16{
        return ToolCallResult::err("待审批队列已满");
    }
    let request=json!({"device_id":device_id,"voice_session_id":session_id,"task":task,
        "workspace":plan.workspace.to_string_lossy(),"executable":plan.executable.to_string_lossy(),
        "version":self.snapshot(provider).map(|s|s.version.as_str()).unwrap_or(""),
        "config_revision":plan.config_revision,"task_digest":intent_digest(&plan)})
      .as_object().cloned().unwrap_or_default();
    let summary=format!("请求 {} 在受限目录执行只读任务：{}；使用本机 CLI 账户，可能产生费用",
       provider.as_str(),task.chars().take(110).collect::<String>());
    let record=match service.create(NewApproval{
        owner:Some(json!([device_id,session_id]).to_string()),voice_permit:Some(permit),
        kind:ApprovalKind::AgentRun,target_record:name.into(),source:ApprovalSource::Voice,
        session_id:Some(device_id.clone()),request_id:None,summary,
        facts:request.clone(),connection:ApprovalConnection::Agent{
             provider:provider.as_str().into(),fingerprint:plan.config_revision.clone()},
        request,client:audit_client(context),removed_fields:Vec::new(),
    }){Ok(r)=>r,Err(e)=>return ToolCallResult::err(e.message())};
    let speech="已发送到电脑待批准，目前没有启动任何 Agent 程序。";
    ToolCallResult::with_detail(true,speech,json!({"result":{"status":"approval_required","approval_id":record.approval_id,"speech":speech}}).to_string())
 }
 fn current_digest(&self,record:&ApprovalRecord)->Result<String,ApprovalError>{
     let (plan,_,_)=self.prepare_record(record)?;
     let facts=record.request.as_ref().ok_or(ApprovalError::Mismatch)?;
     Ok(approval_digest(record.kind,&record.target_record,facts,&ApprovalConnection::Agent{
        provider:plan.provider.as_str().into(),fingerprint:plan.config_revision,
     }))
 }
 fn execute(&self,record:&ApprovalRecord)->(u16,Value){
     let (plan,ctx,permit)=match self.prepare_record(record){
         Ok(p)=>p,Err(e)=>return Self::response(e.http_status(),&e.message(),e.code()),
     };
     let Some(api)=self.api.upgrade() else{return Self::response(503,"MOSS 已停止","agent_unavailable")};
     let Some(approvals)=api.agent_approvals() else{return Self::response(503,"审批服务不可用","agent_unavailable")};
     let digest=approval_digest(record.kind,&record.target_record,record.request.as_ref().unwrap(),&ApprovalConnection::Agent{
         provider:plan.provider.as_str().into(),fingerprint:plan.config_revision.clone(),
     });
     let id=format!("agentrun-{}",record.approval_id);
     let run=AgentRunRecord{id:id.clone(),approval_id:record.approval_id.clone(),
        provider:plan.provider.as_str().into(),workspace:plan.workspace.to_string_lossy().into_owned(),
        task_digest:intent_digest(&plan),config_revision:plan.config_revision.clone(),
        status:"running".into(),output:String::new(),truncated:false,
        started_at:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_secs_f64()).unwrap_or(0.0),
        finished_at:None,error_code:None};
     if let Err(e)=approvals.consume_agent_run(&record.approval_id,&digest,&run,&permit){
         return Self::response(e.http_status(),&e.message(),e.code());
     }
     // No real-process runner is registered in production. Even a forged
     // approved record cannot bypass the sandbox_verified gate.
     let alive=||permit.load(Ordering::Acquire)&&api.voice_session_allowed(&ctx_device(&ctx),&ctx_session(&ctx))
         &&self.prepare(plan.provider,&plan.stdin).is_ok_and(|current|current.config_revision==plan.config_revision);
     let result=self.runner.run(&plan,&alive);
     let (status,output,code)=match result{
       Ok(output) if alive()=>("completed",output,None),
       Ok(_)=>("cancelled",String::new(),Some("agent_permission_expired")),
       Err(error)=>("failed",String::new(),Some(error.code())),
     };
     let (truncated,output)=bounded_text(&output,256*1024);
     if approvals.finish_agent_run(&id,status,&output,truncated,code).is_err(){
        return Self::response(500,"执行结果无法安全记账","agent_result_unknown");
     }
     if status=="completed" {
        (200,json!({"success":true,"result":"已完成只读 Agent 任务，请在电脑查看有界执行记录"}))
     } else {
        Self::response(409,"只读 Agent 任务未成功完成",code.unwrap_or("agent_failed"))
     }
 }
 fn status(&self)->Value{
    let config=self.configuration();
    let records=self.db.lock().ok().and_then(|db|db.agent_runs(MAX_RUN_RECORDS).ok()).unwrap_or_default();
    let safe=records.into_iter().map(|r|json!({"id":r.id,"approval_id":r.approval_id,"provider":r.provider,
        "workspace_name":Path::new(&r.workspace).file_name().and_then(|s|s.to_str()).unwrap_or(""),
        "status":r.status,"output":r.output,"truncated":r.truncated,"started_at":r.started_at,
        "finished_at":r.finished_at,"error_code":r.error_code})).collect::<Vec<_>>();
    json!({"enabled":config.enabled && self.sandbox_verified,
        "configured":config.enabled,"sandbox_verified":self.sandbox_verified,
        "providers":[],"runs":safe,"reason":if self.sandbox_verified {"configured_or_pending"}else{"agent_sandbox_unavailable"}})
 }
}
fn ctx_device(ctx:&CallerContext)->String{
   match ctx{CallerContext::Voice{device_id,..}=>device_id.clone(),_=>String::new()}
}
fn ctx_session(ctx:&CallerContext)->String{
   match ctx{CallerContext::Voice{session_id,..}=>session_id.clone(),_=>String::new()}
}
fn bounded_text(text:&str,limit:usize)->(bool,String){
    if text.len()<=limit{return (false,text.into());}
    let boundary=text.char_indices().map(|(n,_)|n).take_while(|n|*n<=limit).last().unwrap_or(0);
    (true,text[..boundary].into())
}
#[cfg(test)]
mod tests{
    use super::*;
    use std::sync::atomic::{AtomicUsize,Ordering as AtomicOrdering};
    use laser_api::approval::ApprovalService;
    use laser_contracts::{ApprovalStatus,builtin_material_library};

    struct FakeRunner{calls:AtomicUsize}
    impl AgentRunner for FakeRunner{
      fn run(&self,plan:&ReadOnlyPlan,alive:&dyn Fn()->bool)->Result<String,AgentError>{
          assert!(alive(),"取消/撤销后绝不能启动任务");
          assert_eq!(plan.provider,Provider::Codex);
          assert_eq!(plan.argv.last().map(String::as_str),Some("-"));
          assert!(!plan.argv.iter().any(|arg|arg.contains("read README")));
          self.calls.fetch_add(1,AtomicOrdering::SeqCst);
          Ok("假执行器已读取文件列表".into())
      }
    }

    fn approval_http(api:&ApiService,path:&str,digest:&str)->(u16,Value){
        let body=json!({"digest":digest}).to_string();
        let req=format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",body.len(),body);
        let response=api.handle_http_with_ctx(req.as_bytes(),CallerContext::Desktop);
        let cut=response.windows(4).position(|w|w==b"\r\n\r\n").unwrap();
        let status=String::from_utf8_lossy(&response[..cut]).split_whitespace().nth(1).unwrap().parse().unwrap();
        (status,serde_json::from_slice(&response[cut+4..]).unwrap())
    }

    #[test]
    fn fake_runner_is_not_spawned_until_trusted_desktop_approval_then_only_once(){
        let dir=tempfile::tempdir().unwrap();
        let db=Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let approvals=Arc::new(ApprovalService::new(db.clone()));
        let api=Arc::new(ApiService::new(
            laser_runtime::LaserSettings{
                default_connection_mode:"network".into(),engraving_mode:"raster".into(),
                web_host:"127.0.0.1".into(),web_port:9666,material_params_file:None
            },builtin_material_library(),
        ).with_runtime_db(db.clone()).with_approval_service(approvals.clone()));
        let device="agent-device";let session="agent-session";
        db.lock().unwrap().voice_device_seen(device,"test-client","","","",100.0).unwrap();
        api.set_voice_device_status(device,laser_runtime::VOICE_DEVICE_ALLOWED,101.0).unwrap();
        let permit=api.register_voice_session(device,"test-client",session).unwrap();

        let workdir=dir.path().join("agent-workspace");std::fs::create_dir(&workdir).unwrap();
        let sensitive=dir.path().join("private-data");std::fs::create_dir(&sensitive).unwrap();
        let binary=dir.path().join("codex.exe");std::fs::write(&binary,b"not-executed-fixture").unwrap();
        let mut settings=moss_voice::settings::VoiceSettings::default();
        settings.builtin_agents=moss_voice::settings::BuiltinAgents{
            enabled:true,workspace:workdir.to_string_lossy().into_owned(),
            codex_executable:binary.to_string_lossy().into_owned(),claude_executable:String::new(),
        };
        let path=dir.path().join("settings.json");
        moss_voice::settings::save(&path,&settings).unwrap();
        let help="--ask-for-approval --sandbox --ignore-user-config --ignore-rules --ephemeral --skip-git-repo-check --cd --json --color";
        let fake=Arc::new(FakeRunner{calls:AtomicUsize::new(0)});
        let manager=AgentCoordinator::fake_for_tests(&api,db.clone(),path.clone(),vec![sensitive],
           [Some(CapabilitySnapshot{version:"codex-cli 0.157.1".into(),help:help.into()}),None],
           fake.clone());
        api.set_agent_runs(manager.clone()).unwrap();
        let ctx=CallerContext::Voice{device_id:device.into(),session_id:session.into()};
        assert_eq!(manager.tools()["tools"].as_array().unwrap().len(),1);
        let task="read README and summarize; do not write";
        let sent=api.call_agent_tool(&ctx,"agent_codex",&json!({"task":task}));
        assert!(sent.success,"{}",sent.result);
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst),0);
        let pending=approvals.list(Some(ApprovalStatus::Pending),5).unwrap();
        assert_eq!(pending.len(),1);
        assert_eq!(pending[0].kind,ApprovalKind::AgentRun);
        let id=&pending[0].approval_id;
        assert_eq!(approval_http(&api,&format!("/v1/approvals/{id}/approve"),"not-the-digest").0,409);
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst),0);
        let (status,result)=approval_http(&api,&format!("/v1/approvals/{id}/approve"),&pending[0].digest);
        assert_eq!(status,200,"{}",result);
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst),1);
        assert_eq!(db.lock().unwrap().agent_runs(50).unwrap()[0].status,"completed");
        assert_ne!(approval_http(&api,&format!("/v1/approvals/{id}/approve"),&pending[0].digest).0,200);
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst),1);

        // Change configuration between preparing and approval: digest recalculation must fail.
        let pending2=api.call_agent_tool(&ctx,"agent_codex",&json!({"task":"read another file"}));
        assert!(pending2.success);
        let remaining=approvals.list(Some(ApprovalStatus::Pending),5).unwrap();
        assert_eq!(remaining.len(),1);
        settings.builtin_agents.workspace=dir.path().join("missing-workspace").to_string_lossy().into_owned();
        moss_voice::settings::save(&path,&settings).unwrap();
        assert_ne!(approval_http(&api,&format!("/v1/approvals/{}/approve",remaining[0].approval_id),&remaining[0].digest).0,200);
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst),1);

        permit.store(false,AtomicOrdering::Release);
        assert!(!api.voice_session_allowed(device,session));
        assert!(!api.call_agent_tool(&ctx,"agent_codex",&json!({"task":"should be denied"})).success);
        assert_eq!(fake.calls.load(AtomicOrdering::SeqCst),1);
    }
}

