//! NV12 opt-in device vision HTTP endpoint. No camera access or image persistence.
use std::collections::HashSet;
use std::io::Cursor;
use std::sync::{Arc,Mutex};
use std::time::Duration;

use axum::{Extension,Router,Json,middleware};
use axum::extract::{State,Multipart,DefaultBodyLimit};
use axum::http::{Request,StatusCode,header::{CACHE_CONTROL,RETRY_AFTER}};
use axum::middleware::Next;
use axum::response::{IntoResponse,Response};
use axum::routing::post;
use image::{ImageReader,ImageFormat,Limits};
use moss_voice_providers::vision::{VisionProvider,VisionFailure};
use serde_json::{json,Value};
use tokio::sync::{Semaphore,OwnedSemaphorePermit};

use crate::{DeviceRegistry,Shared,unix_now};

pub const MAX_IMAGE_BYTES:usize=5*1024*1024;
const MAX_BODY_BYTES:usize=MAX_IMAGE_BYTES+64*1024;
const MAX_QUESTION_CHARS:usize=2000;
const MODEL_TIMEOUT:Duration=Duration::from_secs(30);
/// 桌面端「摄像头」页发起抓拍时写入 `question` 的内部前缀；命中有效一次性票据时不调用外部视觉模型。
pub const CAMERA_PREVIEW_PREFIX:&str="__moss_camera_preview__:";
const CAMERA_TICKET_TTL_SECS:u64=15;
const MAX_CAMERA_DEVICES:usize=8;

/// The host rechecks settings/credentials on every call; never holds a globally
/// privileged vision model or shares its key with the text LLM.
pub struct VisionSession {
    pub provider:Arc<dyn VisionProvider>,
    pub generation:u64,
}
pub trait VisionFactory:Send+Sync {
    fn ready(&self)->bool;
    fn open(&self)->Option<VisionSession>;
    fn still_current(&self,generation:u64)->bool;
    /// 是否把这张照片留进该设备的对话上下文（2026-10-16，默认否）。
    fn keep_in_history(&self)->bool{false}
}

#[derive(Default)]
struct ActiveDevices(Mutex<HashSet<String>>);
pub struct VisionQuotas {
    total:Arc<Semaphore>,
    devices:Arc<ActiveDevices>,
}
impl Default for VisionQuotas {
    fn default()->Self{Self{total:Arc::new(Semaphore::new(2)),devices:Arc::new(ActiveDevices::default())}}
}
pub struct QuotaGuard {
    _total:OwnedSemaphorePermit,
    device:String,
    devices:Arc<ActiveDevices>,
}
impl Drop for QuotaGuard {
    fn drop(&mut self){
        if let Ok(mut active)=self.devices.0.lock(){active.remove(&self.device);}
    }
}
impl VisionQuotas {
    fn take(&self,device:&str)->Option<QuotaGuard>{
        let total=self.total.clone().try_acquire_owned().ok()?;
        let mut active=self.devices.0.lock().ok()?;
        if !active.insert(device.to_owned()){return None;}
        drop(active);
        Some(QuotaGuard{_total:total,device:device.to_owned(),devices:self.devices.clone()})
    }
}

/// 2026-10-16：视觉卡片打开“把照片留在对话上下文里”时，这一张照片暂存在这里，
/// 由同一台设备的会话在下一轮开始时取走，作为一条多模态用户消息接进对话历史。
/// 只在内存、按设备只留最新一张、总量与单张大小都有上限；不落盘、不进日志与事件。
#[derive(Debug, Default)]
pub struct VisionPhotos(Mutex<std::collections::HashMap<String,Photo>>);

#[derive(Debug, Clone)]
pub struct Photo {
    pub question:String,
    pub text:String,
    pub data_url:String,
}

/// 单张照片 data URL 的上限；超过就不留进上下文，避免内存与每轮费用失控。
const MAX_PHOTO_DATA_URL:usize=4*1024*1024;
const MAX_PHOTO_DEVICES:usize=8;

impl Photo {
    /// 转成一条多模态用户消息：只复述问题与当时的解释，不伪造新内容。
    pub fn into_message(self)->moss_voice_providers::ChatMessage {
        let question=self.question.chars().take(200).collect::<String>();
        let text=self.text.chars().take(400).collect::<String>();
        moss_voice_providers::ChatMessage::photo(
            format!("（用户刚用摄像头拍了照片，问的是：{question}。当时的解释：{text}）"),
            self.data_url,
        )
    }
}

impl VisionPhotos {
    pub fn put(&self,device:&str,photo:Photo){
        if photo.data_url.len()>MAX_PHOTO_DATA_URL{return;}
        let Ok(mut photos)=self.0.lock() else{return};
        if photos.len()>=MAX_PHOTO_DEVICES&&!photos.contains_key(device){return;}
        photos.insert(device.to_owned(),photo);
    }
    /// 取走这台设备待接进上下文的照片（只交付一次）。
    pub fn take(&self,device:&str)->Option<Photo>{
        self.0.lock().ok()?.remove(device)
    }

    /// 预览待接进下一轮上下文的照片文本摘要（不消费照片、不克隆大图字节），供 Jev 预热对齐正式轮历史。
    pub fn peek_message(&self, device: &str) -> Option<moss_voice_providers::ChatMessage> {
        let photos = self.0.lock().ok()?;
        let photo = photos.get(device)?;
        Some(
            Photo {
                question: photo.question.clone(),
                text: photo.text.clone(),
                data_url: String::new(),
            }
            .into_message(),
        )
    }
}

/// 桌面端「摄像头」标签页的内存帧：每台设备只留最新一张、总台数与单张字节受限，
/// 不落盘、不写 SQLite、不进事件日志；撤销设备或停止内置语音服务时立即销毁。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CameraFrame {
    pub seq: u64,
    pub captured_at: u64,
    pub source: String,
    pub question: String,
    pub summary: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: usize,
    pub data_url: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CameraFrameSnapshot {
    pub unchanged: bool,
    pub pending: bool,
    pub last_error: Option<String>,
    pub frame_seq: Option<u64>,
    pub frame: Option<CameraFrame>,
}

pub struct CameraFrameInput {
    pub source: &'static str,
    pub question: String,
    pub summary: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: usize,
    pub data_url: String,
}

struct PendingCapture {
    ticket: String,
    expires_at: u64,
}

#[derive(Default)]
struct CameraPreviewInner {
    next_seq: u64,
    frames: std::collections::HashMap<String, CameraFrame>,
    tickets: std::collections::HashMap<String, PendingCapture>,
    errors: std::collections::HashMap<String, String>,
}

#[derive(Default)]
pub struct CameraPreviewStore(Mutex<CameraPreviewInner>);

fn constant_time_ticket_eq(a: &str, b: &str) -> bool {
    if a.len() != 32 || b.len() != 32 {
        return false;
    }
    a.as_bytes()
        .iter()
        .zip(b.as_bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

impl CameraPreviewStore {
    pub fn issue_ticket(&self, device: &str, now: u64) -> Result<(String, u64), String> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|_| "无法生成摄像头抓拍票据".to_owned())?;
        let ticket: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let mut inner = self.0.lock().map_err(|_| "摄像头预览状态锁失败".to_owned())?;
        inner.tickets.retain(|_, item| now <= item.expires_at);
        if inner.tickets.len() >= MAX_CAMERA_DEVICES && !inner.tickets.contains_key(device) {
            if let Some(oldest) = inner
                .tickets
                .iter()
                .min_by_key(|(_, item)| item.expires_at)
                .map(|(k, _)| k.clone())
            {
                inner.tickets.remove(&oldest);
            }
        }
        inner.next_seq = inner.next_seq.saturating_add(1);
        let seq = inner.next_seq;
        inner.errors.remove(device);
        inner.tickets.insert(
            device.to_owned(),
            PendingCapture {
                ticket: ticket.clone(),
                expires_at: now.saturating_add(CAMERA_TICKET_TTL_SECS),
            },
        );
        Ok((ticket, seq))
    }

    pub fn has_active_ticket(&self, device: &str, now: u64) -> bool {
        let Ok(inner) = self.0.lock() else {
            return false;
        };
        inner
            .tickets
            .get(device)
            .is_some_and(|item| now <= item.expires_at)
    }

    pub fn consume_ticket(&self, device: &str, candidate: &str, now: u64) -> bool {
        let Ok(mut inner) = self.0.lock() else {
            return false;
        };
        let Some(existing) = inner.tickets.get(device) else {
            return false;
        };
        if now > existing.expires_at {
            inner.tickets.remove(device);
            return false;
        }
        if !constant_time_ticket_eq(&existing.ticket, candidate) {
            return false;
        }
        inner.tickets.remove(device);
        true
    }

    pub fn put_frame(&self, device: &str, input: CameraFrameInput, now: u64) -> Option<u64> {
        if input.data_url.is_empty() || input.data_url.len() > MAX_PHOTO_DATA_URL {
            return None;
        }
        let Ok(mut inner) = self.0.lock() else {
            return None;
        };
        if inner.frames.len() >= MAX_CAMERA_DEVICES && !inner.frames.contains_key(device) {
            if let Some(oldest) = inner
                .frames
                .iter()
                .min_by_key(|(_, item)| item.seq)
                .map(|(k, _)| k.clone())
            {
                inner.frames.remove(&oldest);
            }
        }
        inner.next_seq = inner.next_seq.saturating_add(1);
        let seq = inner.next_seq;
        inner.errors.remove(device);
        inner.frames.insert(
            device.to_owned(),
            CameraFrame {
                seq,
                captured_at: now,
                source: input.source.to_owned(),
                question: input.question,
                summary: input.summary,
                mime: input.mime,
                width: input.width,
                height: input.height,
                size_bytes: input.size_bytes,
                data_url: input.data_url,
            },
        );
        Some(seq)
    }

    pub fn record_error(&self, device: &str, matching_ticket: Option<&str>, message: String) {
        let Ok(mut inner) = self.0.lock() else {
            return;
        };
        match matching_ticket {
            Some(expected) => {
                if inner
                    .tickets
                    .get(device)
                    .is_some_and(|item| constant_time_ticket_eq(&item.ticket, expected))
                {
                    inner.tickets.remove(device);
                }
            }
            None => {
                inner.tickets.remove(device);
            }
        }
        if inner.errors.len() >= MAX_CAMERA_DEVICES && !inner.errors.contains_key(device) {
            inner.errors.clear();
        }
        let clean: String = message.chars().take(300).collect();
        if !clean.trim().is_empty() {
            inner.errors.insert(device.to_owned(), clean);
        }
    }

    pub fn clear_device(&self, device: &str) {
        if let Ok(mut inner) = self.0.lock() {
            inner.frames.remove(device);
            inner.tickets.remove(device);
            inner.errors.remove(device);
        }
    }

    pub fn snapshot(&self, device: &str, after_seq: Option<u64>, now: u64) -> CameraFrameSnapshot {
        let Ok(mut inner) = self.0.lock() else {
            return CameraFrameSnapshot {
                unchanged: false,
                pending: false,
                last_error: None,
                frame_seq: None,
                frame: None,
            };
        };
        if inner
            .tickets
            .get(device)
            .is_some_and(|item| now > item.expires_at)
        {
            inner.tickets.remove(device);
            inner.errors.insert(
                device.to_owned(),
                "等待设备回传画面超时，请确认摄像头排线正常且电脑防火墙未拦截语音服务端口".into(),
            );
        }
        let pending = inner.tickets.contains_key(device);
        let last_error = inner.errors.get(device).cloned();
        match inner.frames.get(device) {
            Some(frame) if after_seq == Some(frame.seq) => CameraFrameSnapshot {
                unchanged: true,
                pending,
                last_error,
                frame_seq: Some(frame.seq),
                frame: None,
            },
            Some(frame) => CameraFrameSnapshot {
                unchanged: false,
                pending,
                last_error,
                frame_seq: Some(frame.seq),
                frame: Some(frame.clone()),
            },
            None => CameraFrameSnapshot {
                unchanged: false,
                pending,
                last_error,
                frame_seq: None,
                frame: None,
            },
        }
    }
}

#[derive(Clone)]
pub struct HttpDevicePermit {
    pub device:String,
    client:String,
    generation:u64,
    registry:Arc<dyn DeviceRegistry>,
}
impl HttpDevicePermit {
    async fn current(&self)->bool{
        let registry=self.registry.clone();
        let device=self.device.clone();
        let client=self.client.clone();
        matches!(tokio::task::spawn_blocking(move||registry.authorize_http(&device,&client)).await,Ok(Ok(Some(revision))) if revision==self.generation)
    }
}
fn response(code:StatusCode,body:Value)->Response {
    let mut response=(code,Json(body)).into_response();
    response.headers_mut().insert(CACHE_CONTROL,"no-store".parse().expect("static header"));
    response
}
fn fail(status:StatusCode,code:&str)->Response{
    response(status,json!({"success":false,"error":{"code":code,"message":"视觉请求未完成，请检查授权、配置和图片格式"}}))
}
fn retry()->Response{
    let mut response=fail(StatusCode::TOO_MANY_REQUESTS,"vision_busy");
    response.headers_mut().insert(RETRY_AFTER,"1".parse().expect("static retry-after"));
    response
}

/// Runs BEFORE multipart extraction so invalid credentials never consume body.
async fn http_guard(
    State(shared):State<Arc<Shared>>,
    mut request:Request<axum::body::Body>,
    next:Next,
)->Response{
    let (device,client,token)={
        let header=|name:&str|request.headers().get(name).and_then(|v|v.to_str().ok()).unwrap_or("").trim().to_owned();
        (header("device-id"),header("client-id"),header("authorization"))
    };
    let now=unix_now();
    if device.is_empty()||client.is_empty()||token.is_empty()
       ||device.len()>128||client.len()>128||token.len()>512
       ||!shared.signer.verify(&token,&client,&device,now){
        return fail(StatusCode::UNAUTHORIZED,"vision_unauthorized");
    }
    let registry=shared.registry.clone();
    let check_device=device.clone();let check_client=client.clone();
    let Ok(Ok(Some(generation)))=tokio::task::spawn_blocking(move||registry.authorize_http(&check_device,&check_client)).await else{
        return fail(StatusCode::FORBIDDEN,"vision_forbidden");
    };
    let has_preview_ticket=shared.camera_preview.has_active_ticket(&device,now);
    let permit=HttpDevicePermit{device,client,generation,registry:shared.registry.clone()};
    // Settings/SQLite access belongs on a blocking worker, not an audio thread.
    if !has_preview_ticket {
        let factory=shared.config.vision.clone();
        if !matches!(tokio::task::spawn_blocking(move||factory.is_some_and(|factory|factory.ready())).await,Ok(true)){
            return fail(StatusCode::FORBIDDEN,"vision_disabled");
        }
    }
    request.extensions_mut().insert(permit);
    let mut response=next.run(request).await;
    response.headers_mut().insert(CACHE_CONTROL,"no-store".parse().expect("static header"));
    response
}

pub(crate) fn router(shared:Arc<Shared>)->Router{
    Router::new()
      .route("/mcp/vision/explain",post(explain)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES)))
      .route_layer(middleware::from_fn_with_state(shared.clone(),http_guard))
      .with_state(shared)
}

fn validate_image(bytes:&[u8],mime:&str)->Result<(u32,u32),StatusCode>{
    if bytes.is_empty()||bytes.len()>MAX_IMAGE_BYTES{return Err(StatusCode::PAYLOAD_TOO_LARGE);}
    let expect=match mime{
        "image/png"=>ImageFormat::Png,
        "image/jpeg"=>ImageFormat::Jpeg,
        _=>return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    };
    if image::guess_format(bytes).ok()!=Some(expect){return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);}
    let image=ImageReader::with_format(Cursor::new(bytes),expect);
    let (w,h)=image.into_dimensions().map_err(|_|StatusCode::UNPROCESSABLE_ENTITY)?;
    if w==0||h==0||w>4096||h>4096||(w as u64*h as u64)>16_000_000{
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let mut image=ImageReader::with_format(Cursor::new(bytes),expect);
    let mut limits=Limits::default();
    limits.max_image_width=Some(4096);
    limits.max_image_height=Some(4096);
    limits.max_alloc=Some(96*1024*1024);
    image.limits(limits);
    image.decode().map_err(|_|StatusCode::UNPROCESSABLE_ENTITY)?;
    Ok((w,h))
}
fn image_error(status:StatusCode)->Response{
    let code=match status{
        StatusCode::PAYLOAD_TOO_LARGE=>"vision_image_too_large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE=>"vision_image_format",
        _=>"vision_image_invalid",
    };
    fail(status,code)
}
#[derive(Debug)]
enum GuardedVisionError { Timeout, Revoked, Disabled }
// Dropping this future also drops the provider request. Authorization/config
// revocation must not leave a 30-second model request running with old access.
async fn guarded_explain(
    snapshot:&VisionSession,permit:&HttpDevicePermit,factory:Arc<dyn VisionFactory>,
    question:&str,mime:&str,bytes:&[u8],timeout:Duration,
)->Result<Result<String,VisionFailure>,GuardedVisionError>{
    let task=snapshot.provider.explain(question,mime,bytes);
    tokio::pin!(task);
    let deadline=tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut checks=tokio::time::interval(Duration::from_millis(100));
    checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result=&mut task=>return Ok(result),
            _=&mut deadline=>return Err(GuardedVisionError::Timeout),
            _=checks.tick()=>{
                if !permit.current().await{return Err(GuardedVisionError::Revoked);}
                let factory=factory.clone();let generation=snapshot.generation;
                if !matches!(tokio::task::spawn_blocking(move||factory.still_current(generation)).await,Ok(true)) {
                    return Err(GuardedVisionError::Disabled);
                }
            }
        }
    }
}
async fn explain(
    State(shared):State<Arc<Shared>>,
    Extension(permit):Extension<HttpDevicePermit>,
    mut multipart:Multipart,
)->Response{
    let Some(guard)=shared.vision_quotas.take(&permit.device) else{return retry()};
    let _guard=guard;
    let mut question:Option<String>=None;
    let mut image:Option<(String,Vec<u8>)>=None;
    let mut count=0;
    loop {
        let field=match multipart.next_field().await{
            Ok(field)=>field,
            Err(_)=>return fail(StatusCode::UNPROCESSABLE_ENTITY,"vision_multipart_invalid"),
        };
        let Some(field)=field else{break};
        count+=1;
        if count>2{return fail(StatusCode::UNPROCESSABLE_ENTITY,"vision_multipart_fields");}
        match field.name(){
            Some("question") if question.is_none()=>{
                let text=match field.text().await{Ok(text)=>text,Err(_)=>return fail(StatusCode::UNPROCESSABLE_ENTITY,"vision_question_invalid")};
                if text.trim().is_empty()||text.chars().count()>MAX_QUESTION_CHARS {
                    return fail(StatusCode::UNPROCESSABLE_ENTITY,"vision_question_invalid");
                }
                question=Some(text);
            },
            // 上游 xiaozhi-esp32 固件的 `Esp32Camera::Explain` 用 `name="file"; filename="camera.jpg"`；
            // 本服务早期只按 `name="image"` 实现，两种都接受，否则真机 upload 会被 422 拒绝（2026-10-16 对照固件源码修复）。
            Some("image")|Some("file") if image.is_none()=>{
                let mime=field.content_type().unwrap_or("").to_owned();
                if !matches!(mime.as_str(),"image/png"|"image/jpeg"){
                    return fail(StatusCode::UNSUPPORTED_MEDIA_TYPE,"vision_image_format");
                }
                let bytes=match field.bytes().await{Ok(bytes)=>bytes,Err(_)=>return image_error(StatusCode::PAYLOAD_TOO_LARGE)};
                if bytes.len()>MAX_IMAGE_BYTES{return image_error(StatusCode::PAYLOAD_TOO_LARGE);}
                image=Some((mime,bytes.to_vec()));
            },
            _=>return fail(StatusCode::UNPROCESSABLE_ENTITY,"vision_multipart_fields"),
        }
    }
    let (Some(question),Some((mime,bytes)))=(question,image) else{
        return fail(StatusCode::UNPROCESSABLE_ENTITY,"vision_multipart_missing");
    };
    // CPU/image decoding is also isolated from the audio event loop.
    let validated=tokio::task::spawn_blocking({
        let owned=bytes.clone();let mime=mime.clone();
        move||validate_image(&owned,&mime)
    }).await;
    let (w,h)=match validated {
        Ok(Ok(dim))=>dim,
        Ok(Err(status))=>return image_error(status),
        Err(_)=>return fail(StatusCode::UNPROCESSABLE_ENTITY,"vision_image_invalid"),
    };
    if !permit.current().await{return fail(StatusCode::FORBIDDEN,"vision_revoked");}
    if let Some(raw_ticket)=question.trim().strip_prefix(CAMERA_PREVIEW_PREFIX){
        let now=unix_now();
        if !shared.camera_preview.consume_ticket(&permit.device,raw_ticket.trim(),now){
            return fail(StatusCode::FORBIDDEN,"vision_disabled");
        }
        let url=moss_voice_providers::vision::data_url(&mime,&bytes);
        if shared.camera_preview.put_frame(&permit.device,CameraFrameInput{
            source:"preview",
            question:String::new(),
            summary:String::new(),
            mime,
            width:w,
            height:h,
            size_bytes:bytes.len(),
            data_url:url,
        },now).is_none(){
            return image_error(StatusCode::PAYLOAD_TOO_LARGE);
        }
        return response(StatusCode::OK,json!({"success":true,"text":"画面已同步到 MOSS 桌面端"}));
    }
    let Some(factory)=shared.config.vision.clone() else{return fail(StatusCode::FORBIDDEN,"vision_disabled")};
    let factory_for_open=factory.clone();
    let Ok(Some(snapshot))=tokio::task::spawn_blocking(move||factory_for_open.open()).await else{
        return fail(StatusCode::FORBIDDEN,"vision_disabled");
    }; 
    // 只有用户显式打开“把照片留在对话上下文里”时才需要这张照片的 data URL。
    let keep={let factory=factory.clone();tokio::task::spawn_blocking(move||factory.keep_in_history()).await.ok().unwrap_or(false)};
    let result=guarded_explain(&snapshot,&permit,factory.clone(),&question,&mime,&bytes,MODEL_TIMEOUT).await;
    if !permit.current().await{return fail(StatusCode::FORBIDDEN,"vision_revoked");}
    let generation=snapshot.generation;
    if !matches!(tokio::task::spawn_blocking(move||factory.still_current(generation)).await,Ok(true)){
        return fail(StatusCode::FORBIDDEN,"vision_disabled");
    }
    match result{
        Ok(Ok(text)) if !text.trim().is_empty()=>{
            let clean_text:String=text.chars().take(2000).collect();
            let url=moss_voice_providers::vision::data_url(&mime,&bytes);
            if keep{
                // 2026-10-16：只有用户明确打开“把照片留在对话上下文里”才交给会话；
                // 只留内存、按设备只留最新一张，不落盘、不进日志与事件。
                shared.vision_photos.put(&permit.device,Photo{
                    question:question.chars().take(200).collect(),
                    text:clean_text.chars().take(400).collect(),
                    data_url:url.clone(),
                });
            }
            shared.camera_preview.put_frame(&permit.device,CameraFrameInput{
                source:"voice",
                question:question.chars().take(200).collect(),
                summary:clean_text.chars().take(600).collect(),
                mime,
                width:w,
                height:h,
                size_bytes:bytes.len(),
                data_url:url,
            },unix_now());
            response(StatusCode::OK,json!({"success":true,"text":clean_text}))
        },
        Err(GuardedVisionError::Revoked)=>fail(StatusCode::FORBIDDEN,"vision_revoked"),
        Err(GuardedVisionError::Disabled)=>fail(StatusCode::FORBIDDEN,"vision_disabled"),
        Err(GuardedVisionError::Timeout)|Ok(Err(VisionFailure::Timeout))=>fail(StatusCode::GATEWAY_TIMEOUT,"vision_model_timeout"),
        _=>fail(StatusCode::BAD_GATEWAY,"vision_model_error"),
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::net::{IpAddr,Ipv4Addr};
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool,AtomicU64,AtomicUsize,Ordering};
    use moss_voice_providers::BoxFuture;
    use moss_xiaozhi_proto::TokenSigner;
    use crate::{DeviceRegistry,DeviceStatus,DeviceVisit,ServerConfig,VoiceServer};

    const DEVICE:&str="vision-test-device";
    const CLIENT:&str="vision-test-client";
    const SECRET:&[u8]=b"test-vision-device-signing-key-0001";

    struct TestRegistry {
        allowed:AtomicBool,
        generation:AtomicU64,
        seen:AtomicUsize,
    }
    impl TestRegistry {
        fn new()->Self{Self{allowed:AtomicBool::new(true),generation:AtomicU64::new(1),seen:AtomicUsize::new(0)}}
        fn update(&self,allowed:bool){
            self.allowed.store(allowed,Ordering::Release);
            self.generation.fetch_add(1,Ordering::AcqRel);
        }
    }
    impl DeviceRegistry for TestRegistry {
        fn seen(&self,_:&DeviceVisit)->Result<DeviceStatus,String>{
            self.seen.fetch_add(1,Ordering::SeqCst);
            Ok(if self.allowed.load(Ordering::Acquire){DeviceStatus::Allowed}else{DeviceStatus::Revoked})
        }
        fn token_secret(&self)->Result<Vec<u8>,String>{Ok(SECRET.to_vec())}
        fn open_session(&self,_:&str,_:&str,_:&str)->Result<Arc<AtomicBool>,String>{
            Ok(Arc::new(AtomicBool::new(true)))
        }
        fn close_session(&self,_:&str){}
        fn authorize_http(&self,device:&str,client:&str)->Result<Option<u64>,String>{
            Ok((device==DEVICE&&client==CLIENT&&self.allowed.load(Ordering::Acquire))
                .then(||self.generation.load(Ordering::Acquire)))
        }
    }

    struct FakeProvider {
        calls:AtomicUsize,
        wait:Duration,
    }
    impl VisionProvider for FakeProvider {
        fn explain<'a>(&'a self,question:&'a str,mime:&'a str,data:&'a [u8])
            ->BoxFuture<'a,Result<String,VisionFailure>>{
            Box::pin(async move{
                assert_eq!(question,"这是什么？");
                assert!(matches!(mime,"image/png"|"image/jpeg"),"只允许两种图片格式：{mime}");
                assert!(!data.is_empty());
                self.calls.fetch_add(1,Ordering::SeqCst);
                tokio::time::sleep(self.wait).await;
                Ok("这是一张测试图片。".into())
            })
        }
    }
    struct TestFactory {
        active:AtomicBool,
        revision:AtomicU64,
        provider:Arc<FakeProvider>,
    }
    impl VisionFactory for TestFactory {
        fn ready(&self)->bool{self.active.load(Ordering::Acquire)}
        fn open(&self)->Option<VisionSession>{
            self.ready().then(||VisionSession{
                provider:self.provider.clone(),
                generation:self.revision.load(Ordering::Acquire),
            })
        }
        fn still_current(&self,revision:u64)->bool{
            self.ready()&&self.revision.load(Ordering::Acquire)==revision
        }
    }
    fn start_server(delay_ms:u64)->(VoiceServer,Arc<TestRegistry>,Arc<TestFactory>){
        let registry=Arc::new(TestRegistry::new());
        let factory=Arc::new(TestFactory{
            active:AtomicBool::new(true),revision:AtomicU64::new(1),
            provider:Arc::new(FakeProvider{calls:AtomicUsize::new(0),wait:Duration::from_millis(delay_ms)}),
        });
        let server=VoiceServer::start(ServerConfig{
            bind:IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port:0,http_port:0,
            vision:Some(factory.clone()),..ServerConfig::default()
        },registry.clone()).unwrap();
        (server,registry,factory)
    }
    fn token()->String{TokenSigner::new(SECRET.to_vec()).issue(CLIENT,DEVICE,crate::unix_now())}
    fn png()->Vec<u8>{
        let mut bytes=Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(8,8).write_to(&mut bytes,ImageFormat::Png).unwrap();
        bytes.into_inner()
    }
    fn jpeg()->Vec<u8>{
        let mut bytes=Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8,8).write_to(&mut bytes,ImageFormat::Jpeg).unwrap();
        bytes.into_inner()
    }
    /// 上游固件 `Esp32Camera::Explain` 的真实报文形状：question 段 + `name="file"; filename="camera.jpg"` 段。
    fn firmware_multipart(image:&[u8])->Vec<u8>{
        let mut body=Vec::new();
        part(&mut body,"question",None,"这是什么？".as_bytes());
        body.extend_from_slice(b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"camera.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n");
        body.extend_from_slice(image);
        body.extend_from_slice(b"\r\n--boundary--\r\n");
        body
    }
    fn part(body:&mut Vec<u8>,name:&str,mime:Option<&str>,value:&[u8]){
        let mime=mime.map(|s|format!("; filename=\"test.png\"\r\nContent-Type: {s}")).unwrap_or_default();
        body.extend_from_slice(format!("--boundary\r\nContent-Disposition: form-data; name=\"{name}\"{mime}\r\n\r\n").as_bytes());
        body.extend_from_slice(value);
        body.extend_from_slice(b"\r\n");
    }
    fn multipart(image:&[u8],mime:&str,duplicate:bool)->Vec<u8>{
        let mut body=Vec::new();
        part(&mut body,"question",None,"这是什么？".as_bytes());
        part(&mut body,"image",Some(mime),image);
        if duplicate{part(&mut body,"image",Some(mime),image);}
        body.extend_from_slice(b"--boundary--\r\n");
        body
    }
    fn post(port:u16,token:Option<&str>,body:Vec<u8>)->reqwest::blocking::Response{
        let client=reqwest::blocking::Client::builder().timeout(Duration::from_secs(5)).build().unwrap();
        let mut request=client.post(format!("http://127.0.0.1:{port}/mcp/vision/explain"))
            .header("Device-Id",DEVICE).header("Client-Id",CLIENT)
            .header("Content-Type","multipart/form-data; boundary=boundary")
            .body(body);
        if let Some(token)=token{request=request.header("Authorization",format!("Bearer {token}"));}
        request.send().unwrap()
    }

    #[test]
    fn authorized_synthetic_image_and_denied_requests(){
        let (server,registry,factory)=start_server(0);
        let body=multipart(&png(),"image/png",false);
        assert_eq!(post(server.http_port(),None,body.clone()).status().as_u16(),401);
        assert_eq!(post(server.http_port(),Some("invalid"),body.clone()).status().as_u16(),401);
        let good=token();
        let response=post(server.http_port(),Some(&good),body.clone());
        assert_eq!(response.status().as_u16(),200);
        assert_eq!(response.headers().get("cache-control").unwrap(),"no-store");
        assert_eq!(response.json::<Value>().unwrap()["text"],"这是一张测试图片。");
        assert_eq!(registry.seen.load(Ordering::SeqCst),0,"HTTP must not touch OTA device registration");
        registry.update(false);
        assert_eq!(post(server.http_port(),Some(&good),body.clone()).status().as_u16(),403);
        registry.update(true);
        factory.active.store(false,Ordering::Release);
        assert_eq!(post(server.http_port(),Some(&good),body).status().as_u16(),403);
        assert_eq!(factory.provider.calls.load(Ordering::SeqCst),1);
    }
    /// 上游固件用 `name="file"` 上传，只认 `image` 会让每一台真机都被 422 拒绝。
    #[test]
    fn upstream_firmware_file_field_is_accepted(){
        let (server,_,factory)=start_server(0);
        let token=token();
        let response=post(server.http_port(),Some(&token),firmware_multipart(&jpeg()));
        assert_eq!(response.status().as_u16(),200,"固件报文（name=\"file\"）必须被接受");
        assert_eq!(response.json::<Value>().unwrap()["text"],"这是一张测试图片。");
        assert_eq!(factory.provider.calls.load(Ordering::SeqCst),1);
        // 未知字段名仍然拒绝
        let mut body=Vec::new();
        part(&mut body,"question",None,"这是什么？".as_bytes());
        part(&mut body,"photo",Some("image/jpeg"),&jpeg());
        body.extend_from_slice(b"--boundary--\r\n");
        assert_eq!(post(server.http_port(),Some(&token),body).status().as_u16(),422);
    }

    #[test]
    fn reject_duplicate_missing_bad_mime_and_image_size(){
        let (server,_,factory)=start_server(0);
        let token=token();let bytes=png();
        assert_eq!(post(server.http_port(),Some(&token),multipart(&bytes,"image/png",true)).status().as_u16(),422);
        assert_eq!(post(server.http_port(),Some(&token),multipart(&bytes,"image/jpeg",false)).status().as_u16(),415);
        let huge=vec![42u8;MAX_IMAGE_BYTES+1];
        assert_eq!(post(server.http_port(),Some(&token),multipart(&huge,"image/png",false)).status().as_u16(),413);
        for (question,image,extra) in [
            (Some(""),true,None),(Some("这是什么？"),false,None),
            (None,true,None),(Some("这是什么？"),true,Some("question")),
            (Some("这是什么？"),true,Some("other")),
        ] {
            let mut body=Vec::new();
            if let Some(question)=question{part(&mut body,"question",None,question.as_bytes());}
            if image{part(&mut body,"image",Some("image/png"),&bytes);}
            if let Some(extra)=extra{part(&mut body,extra,None,b"extra");}
            body.extend_from_slice(b"--boundary--\r\n");
            assert_eq!(post(server.http_port(),Some(&token),body).status().as_u16(),422);
        }
        let mut body=Vec::new();
        part(&mut body,"question",None,"问".repeat(MAX_QUESTION_CHARS+1).as_bytes());
        part(&mut body,"image",Some("image/png"),&bytes);body.extend_from_slice(b"--boundary--\r\n");
        assert_eq!(post(server.http_port(),Some(&token),body).status().as_u16(),422);
        assert_eq!(post(server.http_port(),Some(&token),multipart(&bytes[..bytes.len()-12],"image/png",false)).status().as_u16(),422);
        let mut wide=Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(4097,1).write_to(&mut wide,ImageFormat::Png).unwrap();
        assert_eq!(post(server.http_port(),Some(&token),multipart(&wide.into_inner(),"image/png",false)).status().as_u16(),422);
        assert_eq!(factory.provider.calls.load(Ordering::SeqCst),0,"invalid multipart/images must not call model");
    }

    #[test]
    fn model_deadline_and_cancel_release_quota(){
        let runtime=tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let registry=Arc::new(TestRegistry::new());
        let factory=Arc::new(TestFactory{active:AtomicBool::new(true),revision:AtomicU64::new(1),provider:Arc::new(FakeProvider{calls:AtomicUsize::new(0),wait:Duration::from_secs(5)})});
        let permit=HttpDevicePermit{device:DEVICE.into(),client:CLIENT.into(),generation:1,registry};
        let quotas=Arc::new(VisionQuotas::default());
        runtime.block_on(async {
            let guard=quotas.take(DEVICE).unwrap();
            let snapshot=factory.open().unwrap();let bytes=png();
            assert!(matches!(guarded_explain(&snapshot,&permit,factory.clone(),"这是什么？","image/png",&bytes,Duration::from_millis(30)).await,Err(GuardedVisionError::Timeout)));
            drop(guard);assert!(quotas.take(DEVICE).is_some());
            let caps=quotas.clone();let factory=factory.clone();
            let (acquired,ready)=tokio::sync::oneshot::channel();
            let task=tokio::spawn(async move {
                let _guard=caps.take(DEVICE).unwrap();
                acquired.send(()).unwrap();
                let snapshot=factory.open().unwrap();
                guarded_explain(&snapshot,&permit,factory,"这是什么？","image/png",&png(),MODEL_TIMEOUT).await
            });
            tokio::time::timeout(Duration::from_secs(2),ready).await.unwrap().unwrap();
            task.abort();assert!(task.await.unwrap_err().is_cancelled());
            assert!(quotas.take(DEVICE).is_some(),"cancelled handler must release per-device and global quota");
        });
    }

    #[test]
    fn revocation_then_reallow_cannot_resurrect_inflight_response(){
        let (server,registry,factory)=start_server(300);
        let port=server.http_port();
        let t=token();let body=multipart(&png(),"image/png",false);
        let worker=std::thread::spawn(move||post(port,Some(&t),body).status().as_u16());
        for _ in 0..100 {
            if factory.provider.calls.load(Ordering::SeqCst)>0{break;}
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(factory.provider.calls.load(Ordering::SeqCst),1);
        registry.update(false);
        registry.update(true);
        assert_eq!(worker.join().unwrap(),403);
    }
}

#[cfg(test)]mod tests{
    use super::*;
    #[test]fn quotas_deny_same_device_and_global_limit_and_release(){
        let caps=VisionQuotas::default();
        let a=caps.take("a").unwrap();
        assert!(caps.take("a").is_none());
        let b=caps.take("b").unwrap();
        assert!(caps.take("c").is_none());
        drop(a);assert!(caps.take("a").is_some());
        drop(b);
    }
    #[test]fn reject_bad_mime_and_overlimit_before_decode(){
        assert_eq!(validate_image(&vec![0;MAX_IMAGE_BYTES+1],"image/png"),Err(StatusCode::PAYLOAD_TOO_LARGE));
        assert_eq!(validate_image(b"not png","image/png"),Err(StatusCode::UNSUPPORTED_MEDIA_TYPE));
        assert_eq!(validate_image(b"jpeg bytes","image/svg+xml"),Err(StatusCode::UNSUPPORTED_MEDIA_TYPE));
    }
    /// 2026-10-16：照片只在内存、按设备只留最新一张、取走即删；转成的一条多模态用户消息
    /// 只复述问题与当时解释，不伪造内容。
    #[test]fn photo_store_keeps_latest_per_device_and_is_taken_once(){
        let photos=VisionPhotos::default();
        photos.put("d1",Photo{question:"这是什么".into(),text:"一张桌子".into(),data_url:"data:image/jpeg;base64,QQ==".into()});
        photos.put("d1",Photo{question:"换个角度".into(),text:"还是桌子".into(),data_url:"data:image/jpeg;base64,Qg==".into()});
        let photo=photos.take("d1").expect("应能取到最新一张");
        assert_eq!(photo.question,"换个角度","同一设备只保留最新一张");
        assert!(photos.take("d1").is_none(),"只交付一次");
        assert!(photos.take("other").is_none(),"不串设备");
        let message=Photo{question:"这是什么".into(),text:"一张桌子".into(),data_url:"data:image/jpeg;base64,QQ==".into()}.into_message();
        assert_eq!(message.role,moss_voice_providers::Role::User);
        assert_eq!(message.images.len(),1);
        assert!(message.content.as_deref().unwrap_or("").contains("一张桌子"));
        // 超过上限（约 4 MiB 的 data URL）就不留，避免内存与每轮费用失控
        photos.put("big",Photo{question:"q".into(),text:"t".into(),data_url:"x".repeat(MAX_PHOTO_DATA_URL+1)});
        assert!(photos.take("big").is_none());
    }
}
