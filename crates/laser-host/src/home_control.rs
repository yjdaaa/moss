//! NV15 trusted desktop coordinator. Defaults to no control grant. Voice can
//! only create an inert plan; a private desktop command must approve each plan.
use std::{collections::HashMap,sync::{Arc,Mutex,Weak,atomic::{AtomicBool,Ordering}}};
use laser_api::approval::{ApprovalService,NewApproval};
use laser_contracts::{CallerContext,ApprovalKind,ApprovalSource,ApprovalConnection,ApprovalStatus};
use moss_voice::{settings::BuiltinHomeAssistant,home_readonly};
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use crate::{HostApplication,voice_home::{HomeCatalog,HomeAction,HomeControlPlan,AllowedEntity,HomePolicy}};
use crate::voice_home::{approved::{HomeControlTransport,dispatch_approved},https_transport::HttpsHomeTransport};

const GRANT:&str="voice.homeassistant.voice_control_consent";
/// Actual control stays disabled until coordinator and transport acceptance.
const PRODUCTION_CONTROL_READY:bool=false;
fn now()->f64{std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()}
#[derive(Clone)]
struct Pending { device:String,session:String,permit:Arc<AtomicBool>,active:Arc<AtomicBool>,fingerprint:String,plan:HomeControlPlan }
pub struct HomeControlCoordinator { host:Weak<HostApplication>, pending:Mutex<HashMap<String,Pending>>, send_fence:Mutex<()>,
    #[cfg(test)] test_fixture:Option<TestFixture>,
}
#[cfg(test)]
struct TestFixture {discovery:home_readonly::Discovery,transport:Mutex<Option<Box<dyn HomeControlTransport+Send>>>}
impl HomeControlCoordinator {
    pub fn attach(host:&Arc<HostApplication>)->Arc<Self>{Arc::new(Self{host:Arc::downgrade(host),pending:Mutex::new(HashMap::new()),send_fence:Mutex::new(()),
        #[cfg(test)] test_fixture:None,
    })}
    fn control_ready(&self)->bool{
        #[cfg(test)] if self.test_fixture.is_some(){return true;}
        PRODUCTION_CONTROL_READY
    }
    /// Stop queued approval execution immediately, even if an earlier network
    /// request is waiting. Does not revoke the unrelated voice session permit.
    pub fn invalidate_pending(&self)->Result<(),&'static str>{
        let mut pending=self.pending.lock().map_err(|_|"home_unavailable")?;
        for item in pending.values(){item.active.store(false,Ordering::Release);}
        pending.clear();
        Ok(())
    }
    pub fn invalidate_request(&self,id:&str)->Result<(),&'static str>{
        let mut pending=self.pending.lock().map_err(|_|"home_unavailable")?;
        if let Some(item)=pending.remove(id){item.active.store(false,Ordering::Release);}
        Ok(())
    }
    /// Serialize an explicit cancellation with the last permitted POST.
    /// If a POST already started, the approval remains consumed and cannot
    /// be retroactively declared unsent.
    pub fn fence_request_change<T>(&self,id:&str,change:impl FnOnce()->T)->Result<T,&'static str>{
        let _fence=self.send_fence.lock().map_err(|_|"home_unavailable")?;
        self.invalidate_request(id)?;
        Ok(change())
    }
    /// Linearize credential/config changes against the final permitted POST
    /// send. Already-in-flight sends cannot be recalled; their result must
    /// be revalidated and reported unknown on revocation.
    pub fn fence_sensitive_change<T>(&self,change:impl FnOnce()->T)->Result<T,&'static str>{
        let _fence=self.send_fence.lock().map_err(|_|"home_unavailable")?;
        self.invalidate_pending()?;
        Ok(change())
    }
    fn profile(&self)->Result<(BuiltinHomeAssistant,u64,u64,String),&'static str>{
        let host=self.host.upgrade().ok_or("home_disabled")?;
        let (config,policy)=moss_voice::settings::read_home_offline(&host.runtime_root.path.join("settings.json")).map_err(|_|"home_invalid")?;
        config.validate_offline().map_err(|_|"home_invalid")?;
        let db=host.db.lock().map_err(|_|"home_unavailable")?;
        let (present,credential)=db.home_token_status().map_err(|_|"home_unavailable")?;
        let grant=db.get_secret(GRANT).map_err(|_|"home_unavailable")?.ok_or("home_disabled")?;
        let prefix=format!("{policy}:{credential}:");
        let nonce=grant.strip_prefix(&prefix).filter(|v|v.len()==64 && v.bytes().all(|b|b.is_ascii_hexdigit()));
        if !present || policy==0 || credential==0 || config.base_url.is_empty() || !config.entities.iter().any(|e|e.enabled)
            || nonce.is_none() {return Err("home_disabled");}
        Ok((config,policy,credential,grant))
    }
    pub fn enabled(&self)->bool{self.control_ready() && self.profile().is_ok()}
    pub fn consent(&self,enabled:bool,policy:u64,credential:u64,confirmed:bool)->Result<(),&'static str>{
        if enabled && !self.control_ready(){return Err("home_control_disabled");}
        let host=self.host.upgrade().ok_or("home_disabled")?;
        if enabled && !confirmed{return Err("home_confirmation_required");}
        let _fence=self.send_fence.lock().map_err(|_|"home_unavailable")?;
        self.invalidate_pending()?;
        let (config,revision)=moss_voice::settings::read_home_offline(&host.runtime_root.path.join("settings.json")).map_err(|_|"home_invalid")?;
        config.validate_offline().map_err(|_|"home_invalid")?;
        let db=host.db.lock().map_err(|_|"home_unavailable")?;
        let (present,current)=db.home_token_status().map_err(|_|"home_unavailable")?;
        if revision!=policy || current!=credential{return Err("home_conflict");}
        if enabled {
            if !present || revision==0 || current==0 || config.base_url.is_empty() || !config.entities.iter().any(|e|e.enabled){return Err("home_disabled");}
            let mut nonce=[0u8;32];getrandom::fill(&mut nonce).map_err(|_|"home_unavailable")?;
            let nonce=nonce.iter().map(|v|format!("{v:02x}")).collect::<String>();
            db.set_secret(GRANT,&format!("{policy}:{credential}:{nonce}"),now()).map_err(|_|"home_unavailable")?;
        } else {db.delete_secret(GRANT).map_err(|_|"home_unavailable")?;}
        Ok(())
    }
    fn fingerprint(profile:&(BuiltinHomeAssistant,u64,u64,String))->String {
        format!("{:x}",Sha256::digest(serde_json::to_vec(profile).expect("validated settings serialize")))
    }
    fn catalog(config:&BuiltinHomeAssistant,discovery:&home_readonly::Discovery)->Result<HomeCatalog,&'static str>{
        let states=discovery.entities.iter().map(|e|json!({"entity_id":e.entity_id,"state":e.state,"last_updated":e.last_updated,"attributes":e.attributes})).collect::<Vec<_>>();
        let mut groups=std::collections::BTreeMap::<String,serde_json::Map<String,Value>>::new();
        for entity in &discovery.entities {
            let domain=entity.entity_id.split_once('.').ok_or("home_invalid")?.0;
            for service in &entity.actions_available {groups.entry(domain.into()).or_default().insert(service.clone(),json!({}));}
        }
        let services=groups.into_iter().map(|(domain,services)|json!({"domain":domain,"services":services})).collect::<Vec<_>>();
        let allowlist=config.entities.iter().map(|e|AllowedEntity{entity_id:e.entity_id.clone(),alias:e.alias.clone(),enabled:e.enabled,policy:HomePolicy::Ask,media:e.media.iter().map(|m|crate::voice_home::AllowedMedia{id:m.id.clone(),media_type:m.media_type.clone()}).collect()}).collect();
        HomeCatalog::from_snapshots(&serde_json::to_vec(&states).map_err(|_|"home_invalid")?,&serde_json::to_vec(&services).map_err(|_|"home_invalid")?,allowlist,discovery.policy_revision,discovery.credential_revision).map_err(|_|"home_invalid")
    }
    fn sync(&self,profile:&(BuiltinHomeAssistant,u64,u64,String))->Result<(HomeCatalog,String),&'static str>{
        let host=self.host.upgrade().ok_or("home_disabled")?;
        let token=host.db.lock().map_err(|_|"home_unavailable")?.home_token_for_connection().map_err(|_|"home_unavailable")?.ok_or("home_unauthorized")?;
        #[cfg(test)] if let Some(fixture)=&self.test_fixture {
            if fixture.discovery.policy_revision!=profile.1 || fixture.discovery.credential_revision!=profile.2 || self.profile()?!=*profile{return Err("home_conflict");}
            return Ok((Self::catalog(&profile.0,&fixture.discovery)?,token));
        }
        // Do not send a second GET or expose an old snapshot if the token,
        // independent control grant or allowlist is revoked while GET is pending.
        let discovery=home_readonly::discover_guarded(&profile.0,&token,profile.1,profile.2,
            ||self.profile().is_ok_and(|current| current==*profile))
            .map_err(|_|"home_unavailable")?;
        if self.profile()?!=*profile{return Err("home_conflict");}
        Ok((Self::catalog(&profile.0,&discovery)?,token))
    }
    fn action(name:&str,args:&Value)->Result<(HomeAction,Value),&'static str>{
        if name=="hass_play_music" {
            let id=args["library_id"].as_str().ok_or("home_invalid_parameters")?;
            return Ok((HomeAction::PlayMedia,json!({"library_id":id})));
        }
        if name!="hass_set_state"{return Err("home_invalid_parameters");}
        let action=match args["action"].as_str().unwrap_or("") {
            "turn_on"=>HomeAction::TurnOn,"turn_off"=>HomeAction::TurnOff,"set_temperature"=>HomeAction::SetTemperature,
            "lock"=>HomeAction::Lock,"unlock"=>HomeAction::Unlock,"arm_home"=>HomeAction::ArmHome,"arm_away"=>HomeAction::ArmAway,
            "disarm"=>HomeAction::Disarm,"open_cover"=>HomeAction::OpenCover,"close_cover"=>HomeAction::CloseCover,"stop_cover"=>HomeAction::StopCover,
            _=>return Err("home_invalid_parameters"),
        };
        let mut params=args.as_object().cloned().ok_or("home_invalid_parameters")?;
        params.remove("entity");params.remove("action");
        Ok((action,Value::Object(params)))
    }
    pub fn prepare(&self,device:&str,session:&str,name:&str,args:&Value)->Result<Value,&'static str>{
        if !self.control_ready(){return Err("home_control_disabled");}
        let host=self.host.upgrade().ok_or("home_disabled")?;
        let context=CallerContext::Voice{device_id:device.into(),session_id:session.into()};
        let permit=host.api_service.external_voice_permit(&context).map_err(|_|"voice_session_revoked")?;
        let profile=self.profile()?;
        let (catalog,_)=self.sync(&profile)?;
        let (action,params)=Self::action(name,args)?;
        let plan=catalog.plan(args["entity"].as_str().unwrap_or(""),action,&params,&permit).map_err(|_|"home_invalid_parameters")?;
        let fingerprint=Self::fingerprint(&profile);
        let service=ApprovalService::new(host.db.clone());
        let mut pending=self.pending.lock().map_err(|_|"home_unavailable")?;
        pending.retain(|id,item|item.permit.load(Ordering::Acquire) && item.active.load(Ordering::Acquire) && service.get(id).is_ok_and(|r|r.status==ApprovalStatus::Pending && r.expires_at>now()));
        if pending.len()>=64{return Err("home_capacity");}
        let facts=json!({"path":plan.service_path,"body":plan.body,"policy":plan.policy_revision,"credential":plan.credential_revision}).as_object().expect("object").clone();
        let record=service.create(NewApproval{owner:Some(session.into()),voice_permit:Some(permit.clone()),kind:ApprovalKind::HomeControl,target_record:plan.entity_id.clone(),source:ApprovalSource::Voice,session_id:Some(device.into()),request_id:None,summary:format!("家居动作 {} → {}，每次需电脑确认",plan.service_path,plan.entity_id),facts:facts.clone(),connection:ApprovalConnection::Home{fingerprint:fingerprint.clone()},request:facts,client:format!("voice:{device}"),removed_fields:vec![]}).map_err(|_|"home_approval_failed")?;
        pending.insert(record.approval_id.clone(),Pending{device:device.into(),session:session.into(),permit,active:Arc::new(AtomicBool::new(true)),fingerprint,plan});
        Ok(json!({"approval_required":true,"approval_id":record.approval_id,"status":"pending"}))
    }
    pub fn can_approve(&self,id:&str)->bool{
        if !self.control_ready(){return false;}
        let Ok(profile)=self.profile() else{return false;};
        let Ok(pending)=self.pending.lock() else{return false;};
        let Some(item)=pending.get(id) else{return false;};
        let Some(host)=self.host.upgrade() else{return false;};
        item.permit.load(Ordering::Acquire) && item.active.load(Ordering::Acquire) && item.fingerprint==Self::fingerprint(&profile)
            && host.api_service.voice_session_allowed(&item.device,&item.session)
            && ApprovalService::new(host.db.clone()).get(id).is_ok_and(|r|r.status==ApprovalStatus::Pending && r.expires_at>now())
    }
    pub fn approve(&self,id:&str,digest:&str,confirmed:bool)->Result<Value,&'static str>{
        // Production HA control is not security-accepted. Never consume an
        // approval or construct a network client while this gate is closed.
        if !self.control_ready(){return Err("home_control_disabled");}
        if !confirmed{return Err("home_confirmation_required");}
        let host=self.host.upgrade().ok_or("home_disabled")?;
        let item=self.pending.lock().map_err(|_|"home_unavailable")?.get(id).cloned().ok_or("home_approval_unavailable")?;
        let profile=self.profile()?;
        if Self::fingerprint(&profile)!=item.fingerprint || !item.permit.load(Ordering::Acquire) || !item.active.load(Ordering::Acquire)
            || !host.api_service.voice_session_allowed(&item.device,&item.session){return Err("home_conflict");}
        let (catalog,token)=self.sync(&profile)?;
        catalog.revalidate_plan(&item.plan,&item.permit).map_err(|_|"home_conflict")?;
        #[cfg(not(test))]
        let mut network:Box<dyn HomeControlTransport>=Box::new(HttpsHomeTransport::new(&profile.0.base_url,&token).map_err(|_|"home_invalid")?);
        #[cfg(test)]
        let mut network:Box<dyn HomeControlTransport>=if let Some(fixture)=&self.test_fixture {
            fixture.transport.lock().map_err(|_|"home_unavailable")?.take().ok_or("home_test_transport_missing")?
        }else{Box::new(HttpsHomeTransport::new(&profile.0.base_url,&token).map_err(|_|"home_invalid")?)};
        let valid=||self.profile().is_ok_and(|now|now==profile) && item.permit.load(Ordering::Acquire) && item.active.load(Ordering::Acquire)
            && host.api_service.voice_session_allowed(&item.device,&item.session);
        struct Guarded<'a,F:Fn()->bool>{network:&'a mut dyn HomeControlTransport,valid:F,send_fence:&'a Mutex<()>}
        impl<F:Fn()->bool> HomeControlTransport for Guarded<'_,F>{
            fn submit_once(&mut self,path:&str,body:&Value)->Result<(),()>{
                let _fence=self.send_fence.lock().map_err(|_|())?;
                if !(self.valid)(){return Err(());}
                self.network.submit_once(path,body)
            }
            fn read_target_state(&mut self,entity:&str,budget:std::time::Duration)->Result<String,()>{if !(self.valid)(){return Err(());}let result=self.network.read_target_state(entity,budget);if !(self.valid)(){return Err(());}result}
        }
        let service=ApprovalService::new(host.db.clone());
        if !valid(){return Err("home_conflict");}
        let facts=json!({"path":item.plan.service_path,"body":item.plan.body,"policy":item.plan.policy_revision,"credential":item.plan.credential_revision}).as_object().expect("object").clone();
        let current=laser_api::approval::approval_digest(ApprovalKind::HomeControl,&item.plan.entity_id,&facts,&ApprovalConnection::Home{fingerprint:item.fingerprint.clone()});
        service.approve(id,digest,&current).map_err(|_|"home_approval_failed")?;
        let result=dispatch_approved(&service,id,&item.device,&item.fingerprint,&catalog,&item.plan,&item.active,&mut Guarded{network:network.as_mut(),valid,send_fence:&self.send_fence});
        self.pending.lock().map_err(|_|"home_unavailable")?.remove(id);
        result.map_err(|_|"home_approval_failed")?;
        let record=service.get(id).map_err(|_|"home_unavailable")?;
        Ok(json!({"approval_id":id,"status":record.status.as_str(),"outcome":record.outcome}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{mpsc, atomic::AtomicUsize},
        thread,
    };

    fn coordinator() -> Arc<HomeControlCoordinator> {
        Arc::new(HomeControlCoordinator {
            host:Weak::new(),pending:Mutex::new(HashMap::new()),send_fence:Mutex::new(()),test_fixture:None,
        })
    }
    fn add_fake_pending(
        coordinator:&HomeControlCoordinator,id:&str,active:Arc<AtomicBool>,
        voice:Arc<AtomicBool>,
    ) {
        let plan=HomeControlPlan {
            action:HomeAction::TurnOn, arguments:json!({}),
            service_path:"/api/services/light/turn_on".into(),
            body:json!({"entity_id":"light.synthetic"}), entity_id:"light.synthetic".into(),
            policy_revision:1, credential_revision:1, requires_dedicated_approval:true,
        };
        coordinator.pending.lock().unwrap().insert(id.to_owned(),Pending{
            device:"fake-device".into(),session:"fake-session".into(),
            permit:voice,active,fingerprint:"fake".into(),plan
        });
    }

    #[test]
    fn nv15_invalidation_does_not_revoke_unrelated_voice_session() {
        let manager=coordinator();
        let voice=Arc::new(AtomicBool::new(true));
        let first=Arc::new(AtomicBool::new(true));
        let second=Arc::new(AtomicBool::new(true));
        add_fake_pending(&manager,"first",first.clone(),voice.clone());
        add_fake_pending(&manager,"second",second.clone(),voice.clone());
        manager.invalidate_request("first").unwrap();
        assert!(!first.load(Ordering::Acquire));
        assert!(second.load(Ordering::Acquire));
        assert!(voice.load(Ordering::Acquire));
        manager.invalidate_pending().unwrap();
        assert!(!second.load(Ordering::Acquire));
        assert!(voice.load(Ordering::Acquire));
        assert!(manager.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn nv15_revocation_cannot_pass_final_post_send_fence() {
        let manager=coordinator();
        let voice=Arc::new(AtomicBool::new(true));
        let active=Arc::new(AtomicBool::new(true));
        add_fake_pending(&manager,"one",active.clone(),voice.clone());
        let entered=manager.send_fence.lock().unwrap();
        let (tx,rx)=mpsc::channel();
        let gate=manager.clone();
        let mutator=thread::spawn(move||{
            gate.fence_sensitive_change(||{
                tx.send("credential-revoked").unwrap();
            }).unwrap();
        });
        // The occupied send fence linearizes an already-started send before
        // revocation; invalidation and credential mutation share the fence.
        assert!(active.load(Ordering::Acquire));
        assert!(rx.try_recv().is_err(),"mutation cannot pass an occupied fence");
        drop(entered);
        mutator.join().unwrap();
        assert!(!active.load(Ordering::Acquire));
        assert_eq!(rx.try_recv().unwrap(),"credential-revoked");
        assert!(voice.load(Ordering::Acquire));
        // A synthetic sender after the fence cannot issue a command, even if
        // the original independent voice permit is still allowed.
        let sends=AtomicUsize::new(0);
        {
            let _guard=manager.send_fence.lock().unwrap();
            if active.load(Ordering::Acquire) && voice.load(Ordering::Acquire) {
                sends.fetch_add(1,Ordering::AcqRel);
            }
        }
        assert_eq!(sends.load(Ordering::Acquire),0);
    }

    #[test]
    fn nv15_private_cancel_serializes_and_disallows_stale_approval_send() {
        let manager=coordinator();
        let voice=Arc::new(AtomicBool::new(true));
        let item=Arc::new(AtomicBool::new(true));
        add_fake_pending(&manager,"approval-A",item.clone(),voice.clone());
        let _other=Arc::new(AtomicBool::new(true));
        add_fake_pending(&manager,"approval-B",_other.clone(),voice.clone());
        let guard=manager.send_fence.lock().unwrap();
        let first=manager.clone();
        let cancellation=thread::spawn(move||{
            first.fence_request_change("approval-A",||"cancelled").unwrap()
        });
        assert!(item.load(Ordering::Acquire));
        assert!(_other.load(Ordering::Acquire),"another approval stays independent");
        drop(guard);
        assert_eq!(cancellation.join().unwrap(),"cancelled");
        assert!(!item.load(Ordering::Acquire));
        assert!(manager.pending.lock().unwrap().get("approval-A").is_none());
        assert!(manager.pending.lock().unwrap().get("approval-B").is_some());
    }

    #[test]
    fn production_gate_rejects_grant_plan_and_approval_before_host_access() {
        let manager=coordinator();
        assert!(!manager.enabled());
        assert!(!manager.can_approve("missing"));
        assert_eq!(manager.consent(true,1,1,true),Err("home_control_disabled"));
        assert_eq!(manager.prepare("fake","fake","hass_set_state",&json!({})),Err("home_control_disabled"));
        assert_eq!(manager.approve("fake","fake",true),Err("home_control_disabled"));
        assert!(manager.pending.lock().unwrap().is_empty());
    }

    #[derive(Default)]
    struct Calls {sends:usize,reads:usize}
    struct FakeTransport {calls:Arc<Mutex<Calls>>,fail:bool}
    impl HomeControlTransport for FakeTransport {
        fn submit_once(&mut self,path:&str,body:&Value)->Result<(),()>{
            assert_eq!(path,"/api/services/light/turn_on");
            assert_eq!(body,&json!({"entity_id":"light.desk"}));
            self.calls.lock().unwrap().sends+=1;
            if self.fail{Err(())}else{Ok(())}
        }
        fn read_target_state(&mut self,id:&str,budget:std::time::Duration)->Result<String,()>{
            assert_eq!(id,"light.desk");assert!(!budget.is_zero());
            self.calls.lock().unwrap().reads+=1;Ok("on".into())
        }
    }
    struct Integrated {
        coordinator:Arc<HomeControlCoordinator>,host:Arc<HostApplication>,calls:Arc<Mutex<Calls>>,
        _root:tempfile::TempDir,
    }
    fn integrated(fail:bool)->Integrated {
        use moss_voice::settings::{BuiltinHomeEntity,HomeApprovalPreference};
        let root=tempfile::tempdir().unwrap();
        let host=Arc::new(HostApplication::init_with_runtime_dir(Some(root.path())).unwrap());
        let config=BuiltinHomeAssistant{enabled:false,base_url:"https://ha.example.test/".into(),entities:vec![BuiltinHomeEntity{entity_id:"light.desk".into(),alias:"台灯".into(),enabled:true,policy:HomeApprovalPreference::Ask,media:vec![]}]};
        let policy=moss_voice::settings::save_home_offline(&host.runtime_root.path.join("settings.json"),&config,0).unwrap();
        let credential={let db=host.db.lock().unwrap();
            db.voice_device_seen("synthetic-device","synthetic-client","","","",now()).unwrap();
            let revision=db.update_home_token_if_revision(0,Some("synthetic-home-token-not-real-credentials"),now()).unwrap().unwrap();
            db.set_secret(GRANT,&format!("{policy}:{revision}:{}","a".repeat(64)),now()).unwrap();revision
        };
        host.api_service.set_voice_device_status("synthetic-device",laser_runtime::VOICE_DEVICE_ALLOWED,now()).unwrap();
        host.api_service.register_voice_session("synthetic-device","synthetic-client","synthetic-session").unwrap();
        let discovery=home_readonly::Discovery{policy_revision:policy,credential_revision:credential,entities:vec![home_readonly::DiscoveredEntity{entity_id:"light.desk".into(),state:"off".into(),last_updated:"synthetic-now".into(),attributes:json!({}),actions_available:vec!["turn_on".into(),"turn_off".into()]}]};
        let calls=Arc::new(Mutex::new(Calls::default()));
        let coordinator=Arc::new(HomeControlCoordinator{host:Arc::downgrade(&host),pending:Mutex::new(HashMap::new()),send_fence:Mutex::new(()),test_fixture:Some(TestFixture{discovery,transport:Mutex::new(Some(Box::new(FakeTransport{calls:calls.clone(),fail})))})});
        Integrated{coordinator,host,calls,_root:root}
    }
    fn request(f:&Integrated)->(String,String){
        let reply=f.coordinator.prepare("synthetic-device","synthetic-session","hass_set_state",&json!({"entity":"台灯","action":"turn_on"})).unwrap();
        let id=reply["approval_id"].as_str().unwrap().to_owned();
        let record=ApprovalService::new(f.host.db.clone()).get(&id).unwrap();
        assert_eq!(record.status,ApprovalStatus::Pending);
        assert_eq!(f.calls.lock().unwrap().sends,0);
        (id,record.digest)
    }
    #[test]
    fn integrated_home_plan_approval_consumption_and_replay_use_real_sqlite(){
        let f=integrated(false);let (id,digest)=request(&f);
        assert!(f.coordinator.can_approve(&id));
        let reply=f.coordinator.approve(&id,&digest,true).unwrap();
        assert_eq!(reply["status"],"executed");
        let calls=f.calls.lock().unwrap();assert_eq!(calls.sends,1);assert_eq!(calls.reads,1);drop(calls);
        assert!(!f.coordinator.can_approve(&id));
        assert!(f.coordinator.approve(&id,&digest,true).is_err());
        assert_eq!(f.calls.lock().unwrap().sends,1);
        assert!(ApprovalService::new(f.host.db.clone()).get(&id).unwrap().outcome.is_some());
    }
    #[test]
    fn integrated_home_private_cancel_revoke_and_digest_reject_before_any_send(){
        let f=integrated(false);let (id,digest)=request(&f);
        let service=ApprovalService::new(f.host.db.clone());
        let cancelled=f.coordinator.fence_request_change(&id,||service.cancel_home_control(&id,&digest)).unwrap().unwrap();
        assert_eq!(cancelled.status,ApprovalStatus::Rejected);
        assert!(f.coordinator.approve(&id,&digest,true).is_err());
        assert_eq!(f.calls.lock().unwrap().sends,0);
        let f=integrated(false);let (id,_digest)=request(&f);
        assert!(f.coordinator.approve(&id,&"0".repeat(64),true).is_err());
        assert_eq!(f.calls.lock().unwrap().sends,0);
        // Different fixture so its fake transport remains available for the
        // revocation assertion, independent of a failed digest attempt.
        let f=integrated(false);let (id,digest)=request(&f);
        f.host.api_service.set_voice_device_status("synthetic-device",laser_runtime::VOICE_DEVICE_REVOKED,now()).unwrap();
        assert!(!f.coordinator.can_approve(&id));
        assert!(f.coordinator.approve(&id,&digest,true).is_err());
        assert_eq!(f.calls.lock().unwrap().sends,0);
    }
    #[test]
    fn integrated_home_unknown_delivery_is_persisted_and_never_replayed(){
        let f=integrated(true);let (id,digest)=request(&f);
        let reply=f.coordinator.approve(&id,&digest,true).unwrap();
        assert_eq!(reply["status"],"failed");
        assert_eq!(f.calls.lock().unwrap().sends,1);assert_eq!(f.calls.lock().unwrap().reads,0);
        let record=ApprovalService::new(f.host.db.clone()).get(&id).unwrap();
        assert_eq!(record.outcome.unwrap().phase,Some(laser_contracts::ApprovalPhase::Unknown));
        assert!(f.coordinator.approve(&id,&digest,true).is_err());assert_eq!(f.calls.lock().unwrap().sends,1);
    }
}

