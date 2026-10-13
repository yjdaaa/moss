//! Explicit, short-lived enrollment using device PCM. No raw audio persists.
//! Recognition and authentication remain disabled until real calibration.
use std::{sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}},time::{Duration,Instant}};
use serde::Serialize;
use sha2::{Digest,Sha256};
use tokio::sync::Semaphore;

pub trait EnrollmentBackend:Send+Sync {
    fn embedding(&self,pcm:&[i16],generation:u64)->Result<Vec<f32>,String>;
    fn save(&self,name:&str,clips:&[Vec<f32>])->Result<(),String>;
}
struct Capture {device:String,session:String,name:String,permit:Arc<AtomicBool>,expires:Instant,generation:u64,clips:Vec<Vec<f32>>,hashes:Vec<[u8;32]>,busy:bool}
struct State {generation:u64,capture:Option<Capture>,message:&'static str}
#[derive(Serialize)]
pub struct EnrollmentStatus {pub active:bool,pub collected:usize,pub processing:bool,pub message:&'static str,pub recognition_enabled:bool}
pub struct SpeakerEnrollment {backend:Arc<dyn EnrollmentBackend>,state:Mutex<State>,worker:Arc<Semaphore>}
impl SpeakerEnrollment {
    pub fn new(backend:Arc<dyn EnrollmentBackend>)->Arc<Self>{Arc::new(Self{backend,state:Mutex::new(State{generation:0,capture:None,message:"未登记"}),worker:Arc::new(Semaphore::new(1))})}
    pub fn status(&self)->EnrollmentStatus {
        let mut state=self.state.lock().expect("speaker state");
        if state.capture.as_ref().is_some_and(|c|!c.permit.load(Ordering::Acquire)||c.expires<=Instant::now()) {state.generation+=1;state.capture=None;state.message="登记过期或会话撤销";}
        let capture=state.capture.as_ref();
        EnrollmentStatus{active:capture.is_some(),collected:capture.map_or(0,|c|c.clips.len()),processing:capture.is_some_and(|c|c.busy)||self.worker.available_permits()==0,message:state.message,recognition_enabled:false}
    }
    pub fn begin(&self,device:&str,session:&str,name:&str,permit:Arc<AtomicBool>,confirmed:bool)->Result<(),&'static str>{
        if !confirmed || !permit.load(Ordering::Acquire) || device.is_empty() || session.is_empty()
            || name.is_empty() || name.trim()!=name || name.chars().count()>64 || name.chars().any(char::is_control){return Err("speaker_enrollment_invalid");}
        let mut state=self.state.lock().map_err(|_|"speaker_unavailable")?;
        if self.worker.available_permits()==0{return Err("speaker_worker_finishing");}
        if state.capture.as_ref().is_some_and(|c|c.expires>Instant::now() && c.permit.load(Ordering::Acquire)){return Err("speaker_enrollment_busy");}
        state.generation=state.generation.wrapping_add(1);
        state.capture=Some(Capture{device:device.into(),session:session.into(),name:name.into(),permit,expires:Instant::now()+Duration::from_secs(90),generation:state.generation,clips:vec![],hashes:vec![],busy:false});
        state.message="请说三段不同的完整句子，每段2～10秒";
        Ok(())
    }
    pub fn cancel(&self)->Result<(),&'static str>{let mut state=self.state.lock().map_err(|_|"speaker_unavailable")?;state.generation=state.generation.wrapping_add(1);state.capture=None;state.message="已取消，未保存片段";Ok(())}
    /// Called independently of ASR; never waits on the stop channel.
    pub fn submit(self:&Arc<Self>,device:&str,session:&str,pcm:&[i16],rate:u32){
        let Ok(mut state)=self.state.lock() else{return;};
        let Some(capture)=state.capture.as_mut() else{return;};
        if capture.device!=device || capture.session!=session || capture.busy
            || !capture.permit.load(Ordering::Acquire) || capture.expires<=Instant::now(){return;}
        // Restrict duration, clipping and minimum acoustic energy. Multiple
        // simultaneous speakers cannot be reliably screened by this rule.
        if ![16000,24000,48000].contains(&rate) || pcm.len()<(rate as usize)*2 || pcm.len()>(rate as usize)*10
            || pcm.iter().filter(|v|v.unsigned_abs()>=32000).count()*100>pcm.len()
            || pcm.iter().map(|v|f64::from(*v).powi(2)).sum::<f64>()/(pcm.len() as f64)<10000. {
            state.message="片段长度、音量或削波不合格，请重说";return;
        }
        let mut digest=Sha256::new();digest.update(rate.to_le_bytes());for value in pcm{digest.update(value.to_le_bytes());}
        let hash:[u8;32]=digest.finalize().into();
        if capture.hashes.contains(&hash){state.message="重复录音片段被拒绝，请说不同句子";return;}
        let Ok(worker)=self.worker.clone().try_acquire_owned() else{state.message="旧任务正在收尾，请稍后重说";return;};
        capture.hashes.push(hash);
        capture.busy=true;let generation=capture.generation;
        let pcm=if rate==16000 {pcm.to_vec()} else {crate::audio::Resampler::new(rate,16000).process(pcm)};
        let this=self.clone();drop(state);
        tokio::spawn(async move {
            let backend=this.backend.clone();
            let job=tokio::task::spawn_blocking(move||{let _worker=worker;backend.embedding(&pcm,generation)});
            let result=tokio::time::timeout(Duration::from_millis(1500),job).await;
            let Ok(mut state)=this.state.lock() else{return;};
            let Some(capture)=state.capture.as_mut() else{return;};
            if capture.generation!=generation{return;}
            if !capture.permit.load(Ordering::Acquire) || capture.expires<=Instant::now(){state.capture=None;state.message="登记过期或会话撤销，未保存";return;}
            capture.busy=false;
            let Ok(Ok(Ok(vector)))=result else{state.message="声纹计算失败或超时；等旧任务收尾后再登记";state.capture=None;return;};
            if vector.len()!=192 || vector.iter().any(|v|!v.is_finite()){state.message="声纹向量无效";state.capture=None;return;}
            capture.clips.push(vector);
            if capture.clips.len()==3 {
                // Hold enrollment lock through the atomic local DB commit:
                // cancel cannot return before an earlier save has completed.
                let result=this.backend.save(&capture.name,&capture.clips);
                state.capture=None;state.message=if result.is_ok(){"已保存三段一致声纹；身份识别仍关闭"}else{"三段不一致、名称重复或保存失败，未登记"};
            } else {state.message="片段已采集，请继续下一句";}
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake{saved:Mutex<usize>,delay:Duration}
    impl EnrollmentBackend for Fake {
        fn embedding(&self,_:&[i16],_:u64)->Result<Vec<f32>,String>{std::thread::sleep(self.delay);Ok(vec![1.;192])}
        fn save(&self,_:&str,clips:&[Vec<f32>])->Result<(),String>{assert_eq!(clips.len(),3);*self.saved.lock().unwrap()+=1;Ok(())}
    }
    async fn wait_finished(service:&SpeakerEnrollment){
        tokio::time::timeout(Duration::from_secs(2),async {
            while service.status().processing {tokio::time::sleep(Duration::from_millis(2)).await;}
        }).await.expect("synthetic extraction must complete");
    }
    #[tokio::test]
    async fn capture_is_explicit_session_bound_three_clips_and_no_identity_grant(){
        let backend=Arc::new(Fake{saved:Mutex::new(0),delay:Duration::ZERO});let service=SpeakerEnrollment::new(backend.clone());
        let permit=Arc::new(AtomicBool::new(true));
        assert!(service.begin("d","s","人",permit.clone(),false).is_err());
        service.begin("d","s","人",permit,true).unwrap();
        service.submit("other","s",&vec![200;32000],16000);assert_eq!(service.status().collected,0);
        for i in 0..3 {service.submit("d","s",&vec![200+i;32000],16000);wait_finished(&service).await;}
        assert_eq!(*backend.saved.lock().unwrap(),1);assert!(!service.status().active);assert!(!service.status().recognition_enabled);
    }
    #[tokio::test]
    async fn revoked_or_cancelled_embedding_cannot_save_or_enter_new_capture(){
        let backend=Arc::new(Fake{saved:Mutex::new(0),delay:Duration::from_millis(60)});let service=SpeakerEnrollment::new(backend.clone());
        let permit=Arc::new(AtomicBool::new(true));service.begin("d","s","甲",permit.clone(),true).unwrap();
        service.submit("d","s",&vec![200;32000],16000);service.cancel().unwrap();
        assert!(service.begin("d","s","乙",permit.clone(),true).is_err());
        tokio::time::sleep(Duration::from_millis(100)).await;assert_eq!(service.status().collected,0);
        service.begin("d","s","乙",permit.clone(),true).unwrap();
        service.submit("d","s",&vec![200;32000],16000);permit.store(false,Ordering::Release);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!service.status().active);assert_eq!(*backend.saved.lock().unwrap(),0);
    }

    #[tokio::test]
    async fn resampling_quality_duplicate_and_timeout_are_bounded(){
        let backend=Arc::new(Fake{saved:Mutex::new(0),delay:Duration::ZERO});let service=SpeakerEnrollment::new(backend.clone());
        let permit=Arc::new(AtomicBool::new(true));service.begin("d","s","甲",permit,true).unwrap();
        for (rate,pcm) in [(24000,vec![200;48000]),(48000,vec![210;96000])] {
            service.submit("d","s",&pcm,rate);wait_finished(&service).await;
        }
        assert_eq!(service.status().collected,2);
        service.submit("d","s",&vec![210;96000],48000);assert_eq!(service.status().collected,2);
        for pcm in [vec![0;32000],vec![32767;32000],vec![200;31999]] {service.submit("d","s",&pcm,16000);assert_eq!(service.status().collected,2);}
        service.cancel().unwrap();assert_eq!(*backend.saved.lock().unwrap(),0);
        let slow=SpeakerEnrollment::new(Arc::new(Fake{saved:Mutex::new(0),delay:Duration::from_millis(1800)}));
        let permit=Arc::new(AtomicBool::new(true));slow.begin("d","s","甲",permit.clone(),true).unwrap();slow.submit("d","s",&vec![200;32000],16000);
        tokio::time::sleep(Duration::from_millis(1600)).await;
        assert!(!slow.status().active);assert!(slow.status().processing);
        assert_eq!(slow.begin("d","s","乙",permit.clone(),true),Err("speaker_worker_finishing"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        slow.begin("d","s","乙",permit,true).unwrap();
    }
}
