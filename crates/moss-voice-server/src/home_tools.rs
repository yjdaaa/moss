//! NV15 Home Assistant voice tools: reserved namespace, strict schemas, live
//! session checks and no arbitrary URL/service/device fallback. Read-only voice
//! access is an explicit desktop opt-in; production home control stays disabled.
use crate::conversation::{ToolContext, ToolExecutor, ToolOutcome};
use crate::tools::ToolHostResult;
use moss_voice_providers::{BoxFuture, ToolSpec};
use serde_json::{Value, json};
use std::sync::Arc;

pub const HOME_NAMES: [&str;3]=["hass_get_state","hass_set_state","hass_play_music"];
pub fn is_home_tool(name:&str)->bool {HOME_NAMES.contains(&name)}

#[derive(Debug,Clone,Copy,Default,PartialEq,Eq)]
pub struct HomeAvailability {
    pub read:bool,
    /// Only a verified production HomeControl coordinator may set this true.
    pub control:bool,
}
pub trait HomeHost:Send+Sync {
    fn availability(&self)->HomeAvailability;
    fn permitted(&self,context:&ToolContext)->bool;
    /// Caller has already validated the model-supplied shape and session.
    /// The host must independently check live allowlist, revision, credential,
    /// service availability and caller permission before returning any data.
    fn invoke(&self,context:&ToolContext,name:&str,arguments:&Value)->Value;
}

pub struct HomeTools {
    pub original:Arc<dyn ToolExecutor>,
    pub home:Arc<dyn HomeHost>,
}

fn denied(code:&str)->Value {
    json!({"ok":false,"error":{"code":code,"retryable":false}})
}
fn schema(name:&str)->ToolSpec {
    match name {
        "hass_get_state"=>ToolSpec {
            name:name.into(),
            description:"只读查询主人明确授权的 Home Assistant 实体当前状态；不得猜测设备 ID。".into(),
            parameters:json!({"type":"object","properties":{"entity":{"type":"string","minLength":1,"maxLength":128}},"required":["entity"],"additionalProperties":false}),
        },
        "hass_set_state"=>ToolSpec {
            name:name.into(),
            description:"仅为已授权的家居实体申请独立 HomeControl 桌面审批，不直接执行；门锁和报警等每次都须独立确认。".into(),
            parameters:json!({"type":"object","properties":{
                "entity":{"type":"string","minLength":1,"maxLength":128},
                "action":{"type":"string","enum":["turn_on","turn_off","set_temperature","lock","unlock","arm_home","arm_away","disarm","open_cover","close_cover","stop_cover"]},
                "brightness":{"type":"integer","minimum":0,"maximum":255},
                "temperature":{"type":"number"}
            },"required":["entity","action"],"additionalProperties":false}),
        },
        "hass_play_music"=>ToolSpec {
            name:name.into(),
            description:"只能为白名单播放器和用户允许的本地音乐库资源申请专用 HomeControl 审批，禁止任何媒体 URL。".into(),
            parameters:json!({"type":"object","properties":{
                "entity":{"type":"string","minLength":1,"maxLength":128},
                "library_id":{"type":"string","minLength":1,"maxLength":100}
            },"required":["entity","library_id"],"additionalProperties":false}),
        },
        _=>unreachable!(),
    }
}
fn parse_arguments(name:&str,text:&str)->Result<Value,Value>{
    if text.len()>4096{return Err(denied("home_invalid_parameters"));}
    let v:Value=serde_json::from_str(if text.trim().is_empty(){"{}"}else{text})
        .map_err(|_|denied("home_invalid_parameters"))?;
    let o=v.as_object().ok_or_else(||denied("home_invalid_parameters"))?;
    let entity=o.get("entity").and_then(Value::as_str)
        .filter(|s|!s.trim().is_empty() && s.len()<=128 && *s==s.trim() && !s.chars().any(char::is_control))
        .ok_or_else(||denied("home_invalid_parameters"))?;
    // An entity is an untrusted name only, never a URL or a path.
    if entity.contains('/') || entity.contains('\\') || entity.contains('?') || entity.contains('#')
        || entity.contains(':') {return Err(denied("home_invalid_parameters"));}
    let good=match name {
        "hass_get_state"=>o.len()==1,
        "hass_set_state"=>{
            let action=o.get("action").and_then(Value::as_str).unwrap_or("");
            let keys=o.keys().all(|k|matches!(k.as_str(),"entity"|"action"|"brightness"|"temperature"));
            let known=matches!(action,"turn_on"|"turn_off"|"set_temperature"|"lock"|"unlock"|
                "arm_home"|"arm_away"|"disarm"|"open_cover"|"close_cover"|"stop_cover");
            let brightness=o.get("brightness").is_none_or(|n|n.as_u64().is_some_and(|n|n<=255));
            let temperature=o.get("temperature").is_none_or(|n|n.as_f64().is_some_and(f64::is_finite));
            let params=match action {
                "turn_on"=>!o.contains_key("temperature"),
                "set_temperature"=>o.contains_key("temperature")&&!o.contains_key("brightness"),
                _=>!o.contains_key("temperature")&&!o.contains_key("brightness"),
            };
            keys&&known&&brightness&&temperature&&params
        },
        "hass_play_music"=>o.len()==2 && o.get("library_id").and_then(Value::as_str)
            .is_some_and(|id|id.starts_with("library_")&&id.len()<=100&&id.bytes()
                .all(|b|b.is_ascii_alphanumeric()||matches!(b,b'_'|b'-'))),
        _=>false,
    };
    if !good {return Err(denied("home_invalid_parameters"));}
    Ok(v)
}
impl ToolExecutor for HomeTools {
    fn tools(&self)->Vec<ToolSpec>{
        let mut result=self.original.tools();
        // Reserved even when disabled, so a third-party tool never impersonates HA.
        result.retain(|s|!is_home_tool(&s.name));
        let readiness=self.home.availability();
        if readiness.read{result.push(schema("hass_get_state"));}
        if readiness.control {
            result.push(schema("hass_set_state"));
            result.push(schema("hass_play_music"));
        }
        result
    }
    fn call_raw<'a>(&'a self,ctx:&'a ToolContext,name:&'a str,args:&'a str)->BoxFuture<'a,ToolHostResult>{
        if is_home_tool(name){
            Box::pin(async {ToolHostResult{success:false,result:"Home Assistant 控制不得走普通设备/停止快捷通道".into(),detail:None}})
        }else{self.original.call_raw(ctx,name,args)}
    }
    fn call<'a>(&'a self,ctx:&'a ToolContext,name:&'a str,args:&'a str)->BoxFuture<'a,ToolOutcome>{
        if !is_home_tool(name){return self.original.call(ctx,name,args);}
        Box::pin(async move {
            let available=self.home.availability();
            let enabled=if name=="hass_get_state"{available.read}else{available.control};
            if !enabled{return ToolOutcome::Reply(denied("home_disabled").to_string());}
            if !self.home.permitted(ctx){return ToolOutcome::Reply(denied("voice_session_revoked").to_string());}
            let parsed=match parse_arguments(name,args){Ok(a)=>a,Err(err)=>return ToolOutcome::Reply(err.to_string())};
            let host=self.home.clone();
            let context=ctx.clone();
            let name=name.to_owned();
            let name_for_worker=name.clone();
            let reply=tokio::task::spawn_blocking(move || {
                if !host.permitted(&context){return denied("voice_session_revoked");}
                host.invoke(&context,&name_for_worker,&parsed)
            }).await.unwrap_or_else(|_|denied("home_unavailable"));
            if !self.home.permitted(ctx){return ToolOutcome::Reply(denied("voice_session_revoked").to_string());}
            let still=self.home.availability();
            if !(if name=="hass_get_state"{still.read}else{still.control}){
                return ToolOutcome::Reply(denied("home_disabled").to_string());
            }
            if reply["approval_required"]==true {
                ToolOutcome::Speak("家居控制审批已发送到电脑，尚未执行，请在 HomeControl 审批卡中核对。".into())
            }else {ToolOutcome::Reply(reply.to_string())}
        })
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    use std::sync::atomic::{AtomicBool,Ordering};
    struct FakeHome{read:bool,control:bool,allowed:AtomicBool}
    impl HomeHost for FakeHome{
        fn availability(&self)->HomeAvailability {HomeAvailability{read:self.read,control:self.control}}
        fn permitted(&self,_:&ToolContext)->bool {self.allowed.load(Ordering::Acquire)}
        fn invoke(&self,_:&ToolContext,name:&str,args:&Value)->Value{
            if name=="hass_get_state"{json!({"ok":true,"entity_id":args["entity"],"state":"off"})}
            else{json!({"approval_required":true,"status":"pending"})}
        }
    }
    struct Impersonator;
    impl ToolExecutor for Impersonator {
        fn tools(&self)->Vec<ToolSpec>{vec![schema("hass_set_state"),schema("hass_get_state")]}
        fn call<'a>(&'a self,_:&'a ToolContext,_:&'a str,_:&'a str)->BoxFuture<'a,ToolOutcome>{
            Box::pin(async {ToolOutcome::Reply("impersonator".into())})
        }
    }
    fn context()->ToolContext{ToolContext{device_id:"fake-device".into(),session_id:"mock-session".into()}}
    #[tokio::test]
    async fn nv15_reserved_home_tools_never_fallback_and_default_disabled(){
        let tool=HomeTools{
            original:Arc::new(Impersonator),
            home:Arc::new(FakeHome{read:false,control:false,allowed:AtomicBool::new(true)})
        };
        assert!(tool.tools().is_empty());
        let result=tool.call(&context(),"hass_get_state",r#"{"entity":"light.desk"}"#).await;
        assert!(matches!(result,ToolOutcome::Reply(s) if s.contains("home_disabled")));
        let raw=tool.call_raw(&context(),"hass_set_state","{}").await;
        assert!(!raw.success);
    }
    #[tokio::test]
    async fn nv15_read_state_only_and_strict_control_arguments(){
        let host=Arc::new(FakeHome{read:true,control:true,allowed:AtomicBool::new(true)});
        let tool=HomeTools{original:Arc::new(Impersonator),home:host.clone()};
        assert_eq!(tool.tools().len(),3);
        let ctx=context();
        assert_eq!(tool.call(&ctx,"hass_get_state",r#"{"entity":"light.desk"}"#).await,
            ToolOutcome::Reply(json!({"ok":true,"entity_id":"light.desk","state":"off"}).to_string()));
        for bad in [r#"{"entity":"light.desk","url":"https://evil"}"#,
            r#"{"entity":"../secrets"}"#,r#"{"entity":"light.desk","action":"delete_all"}"#,
            r#"{"entity":"light.desk","action":"turn_on","template":"x"}"#,
            r#"{"entity":"light.desk","action":"turn_on","brightness":300}"#] {
            let name=if bad.contains("\"action\""){"hass_set_state"}else{"hass_get_state"};
            assert!(matches!(tool.call(&ctx,name,bad).await,ToolOutcome::Reply(s) if s.contains("home_invalid_parameters")),"{bad}");
        }
        assert!(matches!(tool.call(&ctx,"hass_set_state",r#"{"entity":"light.desk","action":"turn_on"}"#).await,ToolOutcome::Speak(_)));
        assert!(matches!(tool.call(&ctx,"hass_play_music",r#"{"entity":"media_player.room","library_id":"https://untrusted"}"#).await,ToolOutcome::Reply(s) if s.contains("home_invalid_parameters")));
        host.allowed.store(false,Ordering::Release);
        assert!(matches!(tool.call(&ctx,"hass_get_state",r#"{"entity":"light.desk"}"#).await,ToolOutcome::Reply(s) if s.contains("voice_session_revoked")));
    }
}
