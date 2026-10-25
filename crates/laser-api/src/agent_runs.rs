//! NV13: separate host-owned AgentRun authorization/execution boundary.
//! This deliberately does not use ExternalTools: an Agent approval can never be
//! replayed as a third-party MCP call or a laser command.
use crate::approval::{ApprovalError, ApprovalService};
use laser_contracts::{ApprovalRecord, CallerContext, ToolCallResult};
use serde_json::{Value,json};
use std::sync::{Arc,atomic::AtomicBool};

pub trait AgentRuns:Send+Sync {
    fn tools(&self) -> Value;
    fn call(&self,context:&CallerContext,name:&str,arguments:&Value)->ToolCallResult;
    fn current_digest(&self,record:&ApprovalRecord)->Result<String,ApprovalError>;
    fn execute(&self,record:&ApprovalRecord)->(u16,Value);
    fn status(&self)->Value;
    /// 停止正在运行的任务；没有这个任务时返回 false。
    fn stop(&self,_run_id:&str)->bool{false}
}
impl crate::ApiService {
    pub fn set_agent_runs(&self,agent:Arc<dyn AgentRuns>)->Result<(),String>{
        self.agent_runs.set(agent).map_err(|_|"AgentRun 已初始化".into())
    }
    pub fn agent_tools_list(&self)->Value{
        self.agent_runs.get().map(|agent|agent.tools()).unwrap_or_else(||json!({"tools":[]}))
    }
    pub fn agent_status(&self)->Value{
        self.agent_runs.get().map(|agent|agent.status()).unwrap_or_else(||json!({"enabled":false,"providers":[],"reason":"agent_sandbox_unavailable"}))
    }
    pub fn call_agent_tool(&self,context:&CallerContext,name:&str,arguments:&Value)->ToolCallResult{
        if !matches!(context,CallerContext::Voice{..})||!self.voice_caller_allowed(context){
            return ToolCallResult::err("Agent 任务仅接受已经授权的小智语音会话");
        }
        self.agent_runs.get().map(|agent|agent.call(context,name,arguments))
            .unwrap_or_else(||ToolCallResult::err("Agent 功能未启用"))
    }
    pub fn agent_voice_permit(&self,ctx:&CallerContext)->Result<Arc<AtomicBool>,ApprovalError>{
        if !self.voice_caller_allowed(ctx){return Err(ApprovalError::Expired);}
        self.voice_permit(ctx).map_err(|_|ApprovalError::Expired)?.ok_or(ApprovalError::Expired)
    }
    pub fn agent_approvals(&self)->Option<&ApprovalService>{self.approvals.as_deref()}
    pub fn agent_stop(&self,run_id:&str)->bool{
        self.agent_runs.get().is_some_and(|agent|agent.stop(run_id))
    }
}
