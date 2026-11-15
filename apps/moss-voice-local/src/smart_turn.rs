// SPDX-License-Identifier: GPL-3.0-or-later
//! 尾音语义判停（`pipecat-ai/smart-turn-v3`）：
//! 计算与 HuggingFace `WhisperFeatureExtractor(chunk_length=8, n_mels=80, do_normalize=True)`
//! 一致的 `[1, 80, 800]` log-mel 特征，并复用 `sherpa-onnx` 静态链接的 ONNX Runtime C API
//! 执行 `smart-turn-v3.2-cpu.onnx` 推理。

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const SAMPLE_RATE_16K: usize = 16_000;
const CHUNK_SAMPLES: usize = 8 * SAMPLE_RATE_16K; // 128_000 (8.0s)
const N_FFT: usize = 400;
const HALF_N_FFT: usize = N_FFT / 2; // 200
const HOP_LENGTH: usize = 160;
const N_FRAMES: usize = 800; // torch.stft(..., center=True)[..., :-1]
const N_FREQ_BINS: usize = N_FFT / 2 + 1; // 201
const N_MELS: usize = 80;
const ORT_API_VERSION: u32 = 14;
const ORT_LOGGING_LEVEL_ERROR: c_int = 3;
const ORT_ENABLE_ALL: c_int = 99;
const ORT_ARENA_ALLOCATOR: c_int = 1;
const ORT_MEM_TYPE_DEFAULT: c_int = 0;
const ONNX_TENSOR_FLOAT: c_int = 1;
const SMART_TURN_MODEL_FILE: &str = "smart-turn-v3.2-cpu.onnx";

// Verified OrtApi vtable indices (identical across ORT v1.14 .. v1.22):
const SLOT_GET_ERROR_CODE: usize = 1;
const SLOT_GET_ERROR_MESSAGE: usize = 2;
const SLOT_CREATE_ENV: usize = 3;
const SLOT_CREATE_SESSION_FROM_ARRAY: usize = 8;
const SLOT_RUN: usize = 9;
const SLOT_CREATE_SESSION_OPTIONS: usize = 10;
const SLOT_SET_SESSION_GRAPH_OPT_LEVEL: usize = 23;
const SLOT_SET_INTRA_OP_THREADS: usize = 24;
const SLOT_SET_INTER_OP_THREADS: usize = 25;
const SLOT_SESSION_GET_INPUT_NAME: usize = 36;
const SLOT_SESSION_GET_OUTPUT_NAME: usize = 37;
const SLOT_CREATE_TENSOR_WITH_DATA: usize = 49;
const SLOT_GET_TENSOR_MUTABLE_DATA: usize = 51;
const SLOT_GET_TENSOR_ELEMENT_TYPE: usize = 60;
const SLOT_GET_TENSOR_SHAPE_ELEMENT_COUNT: usize = 64;
const SLOT_GET_TENSOR_TYPE_AND_SHAPE: usize = 65;
const SLOT_CREATE_CPU_MEMORY_INFO: usize = 69;
const SLOT_ALLOCATOR_FREE: usize = 76;
const SLOT_GET_ALLOCATOR_DEFAULT: usize = 78;
const SLOT_RELEASE_ENV: usize = 92;
const SLOT_RELEASE_STATUS: usize = 93;
const SLOT_RELEASE_MEMORY_INFO: usize = 94;
const SLOT_RELEASE_SESSION: usize = 95;
const SLOT_RELEASE_VALUE: usize = 96;
const SLOT_RELEASE_TENSOR_SHAPE_INFO: usize = 99;
const SLOT_RELEASE_SESSION_OPTIONS: usize = 100;

#[repr(C)]
struct OrtApiBase {
    get_api: Option<unsafe extern "system" fn(version: u32) -> *const *const c_void>,
    get_version_string: Option<unsafe extern "system" fn() -> *const c_char>,
}

#[repr(C)]
struct OrtEnv {
    _private: [u8; 0],
}
#[repr(C)]
struct OrtStatus {
    _private: [u8; 0],
}
#[repr(C)]
struct OrtMemoryInfo {
    _private: [u8; 0],
}
#[repr(C)]
struct OrtSession {
    _private: [u8; 0],
}
#[repr(C)]
struct OrtSessionOptions {
    _private: [u8; 0],
}
#[repr(C)]
struct OrtValue {
    _private: [u8; 0],
}
#[repr(C)]
struct OrtAllocator {
    _private: [u8; 0],
}
#[repr(C)]
struct OrtTensorTypeAndShapeInfo {
    _private: [u8; 0],
}

unsafe extern "system" {
    fn OrtGetApiBase() -> *const OrtApiBase;
}

type FnGetErrorCode = unsafe extern "system" fn(*const OrtStatus) -> c_int;
type FnGetErrorMessage = unsafe extern "system" fn(*const OrtStatus) -> *const c_char;
type FnCreateEnv =
    unsafe extern "system" fn(c_int, *const c_char, *mut *mut OrtEnv) -> *mut OrtStatus;
type FnCreateSessionFromArray = unsafe extern "system" fn(
    *const OrtEnv,
    *const c_void,
    usize,
    *const OrtSessionOptions,
    *mut *mut OrtSession,
) -> *mut OrtStatus;
type FnRun = unsafe extern "system" fn(
    *mut OrtSession,
    *const c_void,
    *const *const c_char,
    *const *const OrtValue,
    usize,
    *const *const c_char,
    usize,
    *mut *mut OrtValue,
) -> *mut OrtStatus;
type FnCreateSessionOptions =
    unsafe extern "system" fn(*mut *mut OrtSessionOptions) -> *mut OrtStatus;
type FnSetSessionGraphOptLevel =
    unsafe extern "system" fn(*mut OrtSessionOptions, c_int) -> *mut OrtStatus;
type FnSetThreads = unsafe extern "system" fn(*mut OrtSessionOptions, c_int) -> *mut OrtStatus;
type FnSessionGetName = unsafe extern "system" fn(
    *const OrtSession,
    usize,
    *mut OrtAllocator,
    *mut *mut c_char,
) -> *mut OrtStatus;
type FnCreateTensorWithData = unsafe extern "system" fn(
    *const OrtMemoryInfo,
    *mut c_void,
    usize,
    *const i64,
    usize,
    c_int,
    *mut *mut OrtValue,
) -> *mut OrtStatus;
type FnGetTensorMutableData =
    unsafe extern "system" fn(*mut OrtValue, *mut *mut c_void) -> *mut OrtStatus;
type FnGetTensorElementType =
    unsafe extern "system" fn(*const OrtTensorTypeAndShapeInfo, *mut c_int) -> *mut OrtStatus;
type FnGetTensorShapeCount =
    unsafe extern "system" fn(*const OrtTensorTypeAndShapeInfo, *mut usize) -> *mut OrtStatus;
type FnGetTensorTypeAndShape = unsafe extern "system" fn(
    *const OrtValue,
    *mut *mut OrtTensorTypeAndShapeInfo,
) -> *mut OrtStatus;
type FnCreateCpuMemoryInfo =
    unsafe extern "system" fn(c_int, c_int, *mut *mut OrtMemoryInfo) -> *mut OrtStatus;
type FnAllocatorFree = unsafe extern "system" fn(*mut OrtAllocator, *mut c_void) -> *mut OrtStatus;
type FnGetAllocatorDefault = unsafe extern "system" fn(*mut *mut OrtAllocator) -> *mut OrtStatus;
type FnReleaseEnv = unsafe extern "system" fn(*mut OrtEnv);
type FnReleaseStatus = unsafe extern "system" fn(*mut OrtStatus);
type FnReleaseMemoryInfo = unsafe extern "system" fn(*mut OrtMemoryInfo);
type FnReleaseSession = unsafe extern "system" fn(*mut OrtSession);
type FnReleaseValue = unsafe extern "system" fn(*mut OrtValue);
type FnReleaseTensorShapeInfo = unsafe extern "system" fn(*mut OrtTensorTypeAndShapeInfo);
type FnReleaseSessionOptions = unsafe extern "system" fn(*mut OrtSessionOptions);

macro_rules! ort_fn {
    ($api:expr, $slot:expr, $ty:ty) => {
        std::mem::transmute::<*const c_void, $ty>(*$api.add($slot))
    };
}

unsafe fn check_status(api: *const *const c_void, status: *mut OrtStatus) -> Result<(), String> {
    if status.is_null() {
        return Ok(());
    }
    let (code, message) = unsafe {
        let get_code = ort_fn!(api, SLOT_GET_ERROR_CODE, FnGetErrorCode);
        let get_msg = ort_fn!(api, SLOT_GET_ERROR_MESSAGE, FnGetErrorMessage);
        let release = ort_fn!(api, SLOT_RELEASE_STATUS, FnReleaseStatus);
        let code = get_code(status);
        let msg_ptr = get_msg(status);
        let message = if msg_ptr.is_null() {
            format!("ONNX Runtime 错误（code={code}）")
        } else {
            CStr::from_ptr(msg_ptr).to_string_lossy().into_owned()
        };
        release(status);
        (code, message)
    };
    let _ = code;
    Err(message)
}

struct OrtSessionOptionsGuard {
    api: *const *const c_void,
    ptr: *mut OrtSessionOptions,
}

impl Drop for OrtSessionOptionsGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                let release = ort_fn!(
                    self.api,
                    SLOT_RELEASE_SESSION_OPTIONS,
                    FnReleaseSessionOptions
                );
                release(self.ptr);
            }
        }
    }
}

struct OrtValueGuard {
    api: *const *const c_void,
    ptr: *mut OrtValue,
}

impl Drop for OrtValueGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                let release = ort_fn!(self.api, SLOT_RELEASE_VALUE, FnReleaseValue);
                release(self.ptr);
            }
        }
    }
}

struct OrtShapeInfoGuard {
    api: *const *const c_void,
    ptr: *mut OrtTensorTypeAndShapeInfo,
}

impl Drop for OrtShapeInfoGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                let release = ort_fn!(
                    self.api,
                    SLOT_RELEASE_TENSOR_SHAPE_INFO,
                    FnReleaseTensorShapeInfo
                );
                release(self.ptr);
            }
        }
    }
}

pub struct SmartTurnModel {
    api: *const *const c_void,
    env: *mut OrtEnv,
    mem_info: *mut OrtMemoryInfo,
    session: *mut OrtSession,
    input_name: CString,
    output_name: CString,
}

// SAFETY: SmartTurnModel is exclusively owned and used on one worker thread.
unsafe impl Send for SmartTurnModel {}

impl Drop for SmartTurnModel {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                ort_fn!(self.api, SLOT_RELEASE_SESSION, FnReleaseSession)(self.session);
            }
            if !self.mem_info.is_null() {
                ort_fn!(self.api, SLOT_RELEASE_MEMORY_INFO, FnReleaseMemoryInfo)(self.mem_info);
            }
            if !self.env.is_null() {
                ort_fn!(self.api, SLOT_RELEASE_ENV, FnReleaseEnv)(self.env);
            }
        }
    }
}

fn resolve_model_file(path: &Path) -> Result<PathBuf, String> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if path.is_dir() {
        let candidate = path.join(SMART_TURN_MODEL_FILE);
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(format!("尾音判停模型文件不存在：{}", candidate.display()));
    }
    Err(format!("尾音判停模型不存在：{}", path.display()))
}

unsafe fn query_io_name(
    api: *const *const c_void,
    session: *const OrtSession,
    allocator: *mut OrtAllocator,
    is_input: bool,
    fallback: &str,
) -> Result<CString, String> {
    let slot = if is_input {
        SLOT_SESSION_GET_INPUT_NAME
    } else {
        SLOT_SESSION_GET_OUTPUT_NAME
    };
    let mut name_ptr: *mut c_char = std::ptr::null_mut();
    unsafe {
        let getter = ort_fn!(api, slot, FnSessionGetName);
        let free = ort_fn!(api, SLOT_ALLOCATOR_FREE, FnAllocatorFree);
        let status = getter(session, 0, allocator, &mut name_ptr);
        if check_status(api, status).is_err() || name_ptr.is_null() {
            return CString::new(fallback).map_err(|e| format!("无效张量名称：{e}"));
        }
        let name = CStr::from_ptr(name_ptr).to_owned();
        let _ = free(allocator, name_ptr as *mut c_void);
        Ok(name)
    }
}

impl SmartTurnModel {
    pub fn load(path: &Path) -> Result<Self, String> {
        let model_file = resolve_model_file(path)?;
        let bytes = std::fs::read(&model_file)
            .map_err(|err| format!("读取尾音判停模型失败（{}）：{err}", model_file.display()))?;

        let base = unsafe { OrtGetApiBase() };
        if base.is_null() {
            return Err("无法获取 ONNX Runtime API".into());
        }
        let get_api = unsafe { (*base).get_api }.ok_or("ONNX Runtime GetApi 为空")?;
        let api = unsafe { get_api(ORT_API_VERSION) };
        if api.is_null() {
            return Err("ONNX Runtime 版本不兼容".into());
        }

        let log_id = CString::new("moss-smart-turn").unwrap();
        let mut env: *mut OrtEnv = std::ptr::null_mut();
        unsafe {
            check_status(
                api,
                ort_fn!(api, SLOT_CREATE_ENV, FnCreateEnv)(
                    ORT_LOGGING_LEVEL_ERROR,
                    log_id.as_ptr(),
                    &mut env,
                ),
            )?;
        }

        let mut mem_info: *mut OrtMemoryInfo = std::ptr::null_mut();
        if let Err(err) = unsafe {
            check_status(
                api,
                ort_fn!(api, SLOT_CREATE_CPU_MEMORY_INFO, FnCreateCpuMemoryInfo)(
                    ORT_ARENA_ALLOCATOR,
                    ORT_MEM_TYPE_DEFAULT,
                    &mut mem_info,
                ),
            )
        } {
            unsafe { ort_fn!(api, SLOT_RELEASE_ENV, FnReleaseEnv)(env) };
            return Err(format!("创建 ORT 内存描述失败：{err}"));
        }

        let mut opts_ptr: *mut OrtSessionOptions = std::ptr::null_mut();
        if let Err(err) = unsafe {
            check_status(
                api,
                ort_fn!(api, SLOT_CREATE_SESSION_OPTIONS, FnCreateSessionOptions)(&mut opts_ptr),
            )
        } {
            unsafe {
                ort_fn!(api, SLOT_RELEASE_MEMORY_INFO, FnReleaseMemoryInfo)(mem_info);
                ort_fn!(api, SLOT_RELEASE_ENV, FnReleaseEnv)(env);
            }
            return Err(format!("创建 ORT 会话选项失败：{err}"));
        }
        let opts = OrtSessionOptionsGuard { api, ptr: opts_ptr };

        let setup_res = (|| -> Result<(*mut OrtSession, CString, CString), String> {
            unsafe {
                check_status(
                    api,
                    ort_fn!(
                        api,
                        SLOT_SET_SESSION_GRAPH_OPT_LEVEL,
                        FnSetSessionGraphOptLevel
                    )(opts.ptr, ORT_ENABLE_ALL),
                )?;
                check_status(
                    api,
                    ort_fn!(api, SLOT_SET_INTRA_OP_THREADS, FnSetThreads)(opts.ptr, 2),
                )?;
                check_status(
                    api,
                    ort_fn!(api, SLOT_SET_INTER_OP_THREADS, FnSetThreads)(opts.ptr, 1),
                )?;
            }
            let mut session: *mut OrtSession = std::ptr::null_mut();
            unsafe {
                check_status(
                    api,
                    ort_fn!(
                        api,
                        SLOT_CREATE_SESSION_FROM_ARRAY,
                        FnCreateSessionFromArray
                    )(
                        env,
                        bytes.as_ptr() as *const c_void,
                        bytes.len(),
                        opts.ptr,
                        &mut session,
                    ),
                )?;
            }
            let mut allocator: *mut OrtAllocator = std::ptr::null_mut();
            let allocator_ok = unsafe {
                check_status(
                    api,
                    ort_fn!(api, SLOT_GET_ALLOCATOR_DEFAULT, FnGetAllocatorDefault)(&mut allocator),
                )
            }
            .is_ok();
            let (input_name, output_name) = if allocator_ok && !allocator.is_null() {
                (
                    unsafe { query_io_name(api, session, allocator, true, "input_features") }?,
                    unsafe { query_io_name(api, session, allocator, false, "logits") }?,
                )
            } else {
                (
                    CString::new("input_features").unwrap(),
                    CString::new("logits").unwrap(),
                )
            };
            Ok((session, input_name, output_name))
        })();

        match setup_res {
            Ok((session, input_name, output_name)) => Ok(Self {
                api,
                env,
                mem_info,
                session,
                input_name,
                output_name,
            }),
            Err(err) => {
                unsafe {
                    ort_fn!(api, SLOT_RELEASE_MEMORY_INFO, FnReleaseMemoryInfo)(mem_info);
                    ort_fn!(api, SLOT_RELEASE_ENV, FnReleaseEnv)(env);
                }
                Err(format!("加载尾音判停模型失败：{err}"))
            }
        }
    }

    pub fn predict(&self, pcm_i16: &[i16], sample_rate: u32) -> Result<(f32, bool), String> {
        if pcm_i16.is_empty() {
            return Ok((1.0, true));
        }
        let mut features = extract_whisper_log_mel(pcm_i16, sample_rate);
        let input_dims: [i64; 3] = [1, N_MELS as i64, N_FRAMES as i64];

        let mut input_val: *mut OrtValue = std::ptr::null_mut();
        unsafe {
            check_status(
                self.api,
                ort_fn!(
                    self.api,
                    SLOT_CREATE_TENSOR_WITH_DATA,
                    FnCreateTensorWithData
                )(
                    self.mem_info,
                    features.as_mut_ptr() as *mut c_void,
                    std::mem::size_of_val(features.as_slice()),
                    input_dims.as_ptr(),
                    input_dims.len(),
                    ONNX_TENSOR_FLOAT,
                    &mut input_val,
                ),
            )?;
        }
        let _input_guard = OrtValueGuard {
            api: self.api,
            ptr: input_val,
        };

        let input_names = [self.input_name.as_ptr()];
        let inputs = [input_val as *const OrtValue];
        let output_names = [self.output_name.as_ptr()];
        let mut output_val: *mut OrtValue = std::ptr::null_mut();
        unsafe {
            check_status(
                self.api,
                ort_fn!(self.api, SLOT_RUN, FnRun)(
                    self.session,
                    std::ptr::null(),
                    input_names.as_ptr(),
                    inputs.as_ptr(),
                    1,
                    output_names.as_ptr(),
                    1,
                    &mut output_val,
                ),
            )?;
        }
        let output_guard = OrtValueGuard {
            api: self.api,
            ptr: output_val,
        };
        if output_guard.ptr.is_null() {
            return Err("尾音判停推理返回空张量".into());
        }

        let mut shape_info: *mut OrtTensorTypeAndShapeInfo = std::ptr::null_mut();
        unsafe {
            check_status(
                self.api,
                ort_fn!(
                    self.api,
                    SLOT_GET_TENSOR_TYPE_AND_SHAPE,
                    FnGetTensorTypeAndShape
                )(output_guard.ptr, &mut shape_info),
            )?;
        }
        let shape_guard = OrtShapeInfoGuard {
            api: self.api,
            ptr: shape_info,
        };
        let mut elem_type: c_int = 0;
        let mut elem_count: usize = 0;
        unsafe {
            check_status(
                self.api,
                ort_fn!(
                    self.api,
                    SLOT_GET_TENSOR_ELEMENT_TYPE,
                    FnGetTensorElementType
                )(shape_guard.ptr, &mut elem_type),
            )?;
            check_status(
                self.api,
                ort_fn!(
                    self.api,
                    SLOT_GET_TENSOR_SHAPE_ELEMENT_COUNT,
                    FnGetTensorShapeCount
                )(shape_guard.ptr, &mut elem_count),
            )?;
        }
        if elem_type != ONNX_TENSOR_FLOAT || elem_count == 0 {
            return Err(format!(
                "尾音判停输出张量格式异常（type={elem_type}, count={elem_count}）"
            ));
        }

        let mut data_ptr: *mut c_void = std::ptr::null_mut();
        unsafe {
            check_status(
                self.api,
                ort_fn!(
                    self.api,
                    SLOT_GET_TENSOR_MUTABLE_DATA,
                    FnGetTensorMutableData
                )(output_guard.ptr, &mut data_ptr),
            )?;
        }
        if data_ptr.is_null() {
            return Err("尾音判停输出数据指针为空".into());
        }
        let raw = unsafe { *(data_ptr as *const f32) };
        if !raw.is_finite() {
            return Err("尾音判停输出为非有限数值".into());
        }
        let prob = raw.clamp(0.0, 1.0);
        Ok((prob, prob >= 0.5))
    }
}

// ---------------------------------------------------------------- Whisper 80-Mel 提取

struct WhisperMelTables {
    window: [f32; N_FFT],
    mel_filters: Vec<f32>, // [N_MELS * N_FREQ_BINS]
    tw_25_re: [[f32; 25]; 25],
    tw_25_im: [[f32; 25]; 25],
    tw_400_re: [[f32; 25]; 16],
    tw_400_im: [[f32; 25]; 16],
    tw_16_re: [[f32; 16]; 16],
    tw_16_im: [[f32; 16]; 16],
}

fn hertz_to_mel_slaney(freq: f64) -> f64 {
    const MIN_LOG_HERTZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = 15.0;
    const F_SP: f64 = 200.0 / 3.0;
    if freq >= MIN_LOG_HERTZ {
        let logstep = (6.4_f64).ln() / 27.0;
        MIN_LOG_MEL + (freq / MIN_LOG_HERTZ).ln() / logstep
    } else {
        freq / F_SP
    }
}

fn mel_to_hertz_slaney(mel: f64) -> f64 {
    const MIN_LOG_HERTZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = 15.0;
    const F_SP: f64 = 200.0 / 3.0;
    if mel >= MIN_LOG_MEL {
        let logstep = (6.4_f64).ln() / 27.0;
        MIN_LOG_HERTZ * (logstep * (mel - MIN_LOG_MEL)).exp()
    } else {
        mel * F_SP
    }
}

fn mel_tables() -> &'static WhisperMelTables {
    static TABLES: OnceLock<WhisperMelTables> = OnceLock::new();
    TABLES.get_or_init(|| {
        use std::f64::consts::PI;

        let mut window = [0.0f32; N_FFT];
        for (i, w) in window.iter_mut().enumerate() {
            *w = (0.5 * (1.0 - (2.0 * PI * i as f64 / N_FFT as f64).cos())) as f32;
        }

        // Slaney mel filterbank (sr=16000, n_fft=400, n_mels=80, 0..8000 Hz)
        let mel_min = hertz_to_mel_slaney(0.0);
        let mel_max = hertz_to_mel_slaney(8000.0);
        let mut filter_freqs = Vec::with_capacity(N_MELS + 2);
        for i in 0..(N_MELS + 2) {
            let mel = mel_min + (mel_max - mel_min) * (i as f64) / ((N_MELS + 1) as f64);
            filter_freqs.push(mel_to_hertz_slaney(mel));
        }

        let mut mel_filters = vec![0.0f32; N_MELS * N_FREQ_BINS];
        for m in 0..N_MELS {
            let left = filter_freqs[m];
            let center = filter_freqs[m + 1];
            let right = filter_freqs[m + 2];
            let enorm = 2.0 / (right - left);
            for k in 0..N_FREQ_BINS {
                let freq = (k as f64) * (SAMPLE_RATE_16K as f64) / (N_FFT as f64);
                let lower = (freq - left) / (center - left);
                let upper = (right - freq) / (right - center);
                let weight = lower.min(upper).max(0.0) * enorm;
                mel_filters[m * N_FREQ_BINS + k] = weight as f32;
            }
        }

        // 16 x 25 Cooley-Tukey 400-point DFT twiddle factors
        let mut tw_25_re = [[0.0f32; 25]; 25];
        let mut tw_25_im = [[0.0f32; 25]; 25];
        for k2 in 0..25 {
            for n2 in 0..25 {
                let angle = -2.0 * PI * ((n2 * k2) as f64) / 25.0;
                tw_25_re[k2][n2] = angle.cos() as f32;
                tw_25_im[k2][n2] = angle.sin() as f32;
            }
        }

        let mut tw_400_re = [[0.0f32; 25]; 16];
        let mut tw_400_im = [[0.0f32; 25]; 16];
        for n1 in 0..16 {
            for k2 in 0..25 {
                let angle = -2.0 * PI * ((n1 * k2) as f64) / 400.0;
                tw_400_re[n1][k2] = angle.cos() as f32;
                tw_400_im[n1][k2] = angle.sin() as f32;
            }
        }

        let mut tw_16_re = [[0.0f32; 16]; 16];
        let mut tw_16_im = [[0.0f32; 16]; 16];
        for k1 in 0..16 {
            for n1 in 0..16 {
                let angle = -2.0 * PI * ((n1 * k1) as f64) / 16.0;
                tw_16_re[k1][n1] = angle.cos() as f32;
                tw_16_im[k1][n1] = angle.sin() as f32;
            }
        }

        WhisperMelTables {
            window,
            mel_filters,
            tw_25_re,
            tw_25_im,
            tw_400_re,
            tw_400_im,
            tw_16_re,
            tw_16_im,
        }
    })
}

/// Resample PCM i16 to 16kHz f32, truncate to last 8s or left-pad zeros to 128_000, then normalize.
fn prepare_8s_normalized_audio(pcm_i16: &[i16], sample_rate: u32) -> Vec<f32> {
    let mono_16k: Vec<f32> = if sample_rate == SAMPLE_RATE_16K as u32 || sample_rate == 0 {
        pcm_i16.iter().map(|&s| s as f32 / 32768.0).collect()
    } else {
        let out_len =
            (((pcm_i16.len() as u64) * (SAMPLE_RATE_16K as u64)) / (sample_rate as u64)) as usize;
        if out_len == 0 {
            Vec::new()
        } else {
            let ratio = sample_rate as f64 / SAMPLE_RATE_16K as f64;
            (0..out_len)
                .map(|i| {
                    let pos = i as f64 * ratio;
                    let idx = (pos as usize).min(pcm_i16.len() - 1);
                    let next = (idx + 1).min(pcm_i16.len() - 1);
                    let frac = (pos - idx as f64) as f32;
                    let a = pcm_i16[idx] as f32 / 32768.0;
                    let b = pcm_i16[next] as f32 / 32768.0;
                    a + (b - a) * frac
                })
                .collect()
        }
    };

    let mut buffer = vec![0.0f32; CHUNK_SAMPLES];
    if mono_16k.len() >= CHUNK_SAMPLES {
        let start = mono_16k.len() - CHUNK_SAMPLES;
        buffer.copy_from_slice(&mono_16k[start..]);
    } else {
        let pad = CHUNK_SAMPLES - mono_16k.len();
        buffer[pad..].copy_from_slice(&mono_16k);
    }

    let mean = buffer.iter().map(|&x| f64::from(x)).sum::<f64>() / (CHUNK_SAMPLES as f64);
    let var = buffer
        .iter()
        .map(|&x| {
            let d = f64::from(x) - mean;
            d * d
        })
        .sum::<f64>()
        / (CHUNK_SAMPLES as f64);
    let inv_std = 1.0 / (var + 1e-7).sqrt();
    for sample in &mut buffer {
        *sample = ((f64::from(*sample) - mean) * inv_std) as f32;
    }
    buffer
}

/// Computes squared magnitude spectrum `|X[k]|^2` for `k in 0..=200` on a 400-sample real frame.
fn power_spectrum_400(
    frame: &[f32; N_FFT],
    tables: &WhisperMelTables,
    out: &mut [f32; N_FREQ_BINS],
) {
    // Stage 1: Z[n1][k2] = sum_{n2=0..24} x[n1 + 16*n2] * W_25^{n2*k2}
    // Using conjugate symmetry for real input: k2 in 0..=12, then Z[n1][25 - k2] = conj(Z[n1][k2]).
    let mut z_re = [[0.0f32; 25]; 16];
    let mut z_im = [[0.0f32; 25]; 16];
    for n1 in 0..16 {
        let mut sum0 = 0.0f32;
        for n2 in 0..25 {
            sum0 += frame[n1 + 16 * n2];
        }
        z_re[n1][0] = sum0;
        for k2 in 1..=12 {
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            let cos_row = &tables.tw_25_re[k2];
            let sin_row = &tables.tw_25_im[k2];
            for n2 in 0..25 {
                let x = frame[n1 + 16 * n2];
                re += x * cos_row[n2];
                im += x * sin_row[n2];
            }
            z_re[n1][k2] = re;
            z_im[n1][k2] = im;
            z_re[n1][25 - k2] = re;
            z_im[n1][25 - k2] = -im;
        }
    }

    // Apply W_400^{n1 * k2} twiddle factors
    for n1 in 1..16 {
        for k2 in 1..25 {
            let xr = z_re[n1][k2];
            let xi = z_im[n1][k2];
            let tr = tables.tw_400_re[n1][k2];
            let ti = tables.tw_400_im[n1][k2];
            z_re[n1][k2] = xr * tr - xi * ti;
            z_im[n1][k2] = xr * ti + xi * tr;
        }
    }

    // Stage 2: k = 25 * k1 + k2 <= 200 (since 25 * 8 = 200, k1 in 0..=8)
    for k2 in 0..25 {
        for k1 in 0..=8 {
            let k = 25 * k1 + k2;
            if k >= N_FREQ_BINS {
                break;
            }
            let cos_row = &tables.tw_16_re[k1];
            let sin_row = &tables.tw_16_im[k1];
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            for n1 in 0..16 {
                let zr = z_re[n1][k2];
                let zi = z_im[n1][k2];
                let cr = cos_row[n1];
                let ci = sin_row[n1];
                re += zr * cr - zi * ci;
                im += zr * ci + zi * cr;
            }
            out[k] = re * re + im * im;
        }
    }
}

/// Extract `[N_MELS, N_FRAMES]` = `[80, 800]` log-mel spectrogram matching `WhisperFeatureExtractor`.
pub fn extract_whisper_log_mel(pcm_i16: &[i16], sample_rate: u32) -> Vec<f32> {
    let tables = mel_tables();
    let audio = prepare_8s_normalized_audio(pcm_i16, sample_rate);

    // Reflect pad HALF_N_FFT (200) on both sides -> CHUNK_SAMPLES + 400 samples
    let padded_len = CHUNK_SAMPLES + 2 * HALF_N_FFT;
    let mut padded = Vec::with_capacity(padded_len);
    for i in (1..=HALF_N_FFT).rev() {
        padded.push(audio[i]);
    }
    padded.extend_from_slice(&audio);
    for i in 1..=HALF_N_FFT {
        padded.push(audio[CHUNK_SAMPLES - 1 - i]);
    }

    let mut out = vec![0.0f32; N_MELS * N_FRAMES];
    let mut frame_buf = [0.0f32; N_FFT];
    let mut power_buf = [0.0f32; N_FREQ_BINS];
    let mut global_max = f32::NEG_INFINITY;

    for t in 0..N_FRAMES {
        let start = t * HOP_LENGTH;
        for i in 0..N_FFT {
            frame_buf[i] = padded[start + i] * tables.window[i];
        }
        power_spectrum_400(&frame_buf, tables, &mut power_buf);
        for m in 0..N_MELS {
            let filter_row = &tables.mel_filters[m * N_FREQ_BINS..(m + 1) * N_FREQ_BINS];
            let mut energy = 0.0f32;
            for k in 0..N_FREQ_BINS {
                energy += filter_row[k] * power_buf[k];
            }
            let log_val = energy.max(1e-10).log10();
            if log_val > global_max {
                global_max = log_val;
            }
            out[m * N_FRAMES + t] = log_val;
        }
    }

    let clamp_min = global_max - 8.0;
    for val in &mut out {
        let clamped = (*val).max(clamp_min);
        *val = (clamped + 4.0) / 4.0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn fast_power_spectrum_400_matches_naive_dft() {
        let tables = mel_tables();
        let mut frame = [0.0f32; N_FFT];
        for (i, slot) in frame.iter_mut().enumerate() {
            let t = i as f64 / SAMPLE_RATE_16K as f64;
            *slot = (0.7 * (2.0 * PI * 440.0 * t).sin()
                + 0.3 * (2.0 * PI * 1850.0 * t).cos()
                + 0.1 * ((i as f64) * 0.17).sin()) as f32
                * tables.window[i];
        }

        let mut fast_out = [0.0f32; N_FREQ_BINS];
        power_spectrum_400(&frame, tables, &mut fast_out);

        for (k, &fast_val) in fast_out.iter().enumerate() {
            let mut re = 0.0f64;
            let mut im = 0.0f64;
            for (n, &x) in frame.iter().enumerate() {
                let angle = -2.0 * PI * ((n * k) as f64) / (N_FFT as f64);
                re += f64::from(x) * angle.cos();
                im += f64::from(x) * angle.sin();
            }
            let naive_power = re * re + im * im;
            let diff = (f64::from(fast_val) - naive_power).abs();
            let tol = 1e-4 * naive_power.max(1.0);
            assert!(
                diff <= tol,
                "bin {k} mismatch: fast={fast_val}, naive={naive_power}, diff={diff}",
            );
        }
    }

    #[test]
    fn whisper_log_mel_has_exact_shape_and_finite_normalized_values() {
        // 1.5s 440Hz tone at 16kHz -> left padded to 8s -> [80, 800] finite values in expected Whisper range
        let pcm: Vec<i16> = (0..24_000)
            .map(|i| {
                let s = (2.0 * PI * 440.0 * (i as f64) / 16_000.0).sin() * 12_000.0;
                s.round() as i16
            })
            .collect();
        let mel = extract_whisper_log_mel(&pcm, 16_000);
        assert_eq!(mel.len(), N_MELS * N_FRAMES);
        assert!(mel.iter().all(|v| v.is_finite()));
        let min_v = mel.iter().copied().fold(f32::INFINITY, f32::min);
        let max_v = mel.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max_v > min_v + 0.1,
            "mel spectrum should capture tone dynamic range"
        );
        assert!(
            max_v - min_v <= 2.01,
            "dynamic range clamped to (max-8..max)/4 <= 2.0"
        );

        // Resampling from 24kHz also stays finite and valid
        let mel_24k = extract_whisper_log_mel(&pcm[..16_000], 24_000);
        assert_eq!(mel_24k.len(), N_MELS * N_FRAMES);
        assert!(mel_24k.iter().all(|v| v.is_finite()));

        // Silent buffer stays finite (no NaN/Inf)
        let silent = vec![0i16; 3_200];
        let mel_silent = extract_whisper_log_mel(&silent, 16_000);
        assert_eq!(mel_silent.len(), N_MELS * N_FRAMES);
        assert!(mel_silent.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn slaney_mel_filters_sum_positively_in_speech_band() {
        let tables = mel_tables();
        for m in 0..N_MELS {
            let row_sum: f32 = tables.mel_filters[m * N_FREQ_BINS..(m + 1) * N_FREQ_BINS]
                .iter()
                .sum();
            assert!(row_sum > 0.0, "mel filter {m} must have positive support");
        }
    }
}
