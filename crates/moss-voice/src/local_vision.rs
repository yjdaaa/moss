//! 本地视觉模型运行时（2026-11-08 n74）：用 MOSS 自动下载的 llama.cpp 推理引擎（与 OneJev 共用一份）
//! 启动 `llama-server` + 视觉投影（mmproj），只监听本机回环地址，提供 OpenAI 兼容的 `/v1/chat/completions`。
//! 视觉理解直接把它当成一个本机地址的看图模型调用，照片不出电脑、不需要密钥。
//!
//! 与 OneJev 分开运行：Jev 决策网关单槽串行，看一张图要数秒，混在一起会拖慢每一轮对话。

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::onejev::{
    DetectedAccelerator, LlamaLog, configure_no_window, ensure_llama_runtime, probe_http_health,
    probe_llama_accelerator_with_cancel, with_log_hint,
};
use crate::supervisor::JobHandle;

/// 请求里用的模型名（llama-server 只加载一个模型，名字仅作标识）。
pub const LOCAL_VISION_MODEL_NAME: &str = "moss-local-vision";

/// 上下文长度：一张图最多 1024 个视觉 token，加问题与 512 token 回答绰绰有余。
const CONTEXT_TOKENS: u32 = 4096;
/// 单张图的视觉 token 上限：越大越清楚也越慢，1024 足够认材料、读铭牌上的字。
const IMAGE_MAX_TOKENS: u32 = 1024;

pub struct LocalVisionServer {
    model_id: String,
    port: u16,
    accelerator: DetectedAccelerator,
    child: Child,
    _job: Option<JobHandle>,
    log: LlamaLog,
}

impl LocalVisionServer {
    /// 启动并等到可用（首次会下载推理引擎；加载权重 2B 约十几秒、4B 更久）。`cancel` 置位时尽快放弃。
    pub fn start(
        model_id: &str,
        model_dir: &Path,
        models_root: &Path,
        mirror_hint: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        let (gguf_name, mmproj_name) = crate::local_models::vision_model_files(model_id)
            .ok_or_else(|| format!("未知的本地视觉模型：{model_id}"))?;
        let gguf_path = model_dir.join(gguf_name);
        let mmproj_path = model_dir.join(mmproj_name);
        if !gguf_path.is_file() || !mmproj_path.is_file() {
            return Err("本地视觉模型还没下载完整，请先在“本地模型”里下载".into());
        }
        let llama_bin = ensure_llama_runtime(models_root, mirror_hint, cancel, &|_| {})?;
        let cancelled = || cancel.load(Ordering::SeqCst);
        if cancelled() {
            return Err("本地视觉模型启动已取消".into());
        }
        let accelerator = probe_llama_accelerator_with_cancel(&llama_bin, true, Some(cancel));
        let port = {
            let socket = TcpListener::bind(("127.0.0.1", 0))
                .map_err(|e| format!("分配本地视觉端口失败：{e}"))?;
            socket
                .local_addr()
                .map_err(|e| format!("读取本地视觉端口失败：{e}"))?
                .port()
        };
        let mut cmd = Command::new(&llama_bin);
        cmd.args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-c", &CONTEXT_TOKENS.to_string(), "-np", "1"])
            .args(["--image-max-tokens", &IMAGE_MAX_TOKENS.to_string()])
            .arg("-m")
            .arg(&gguf_path)
            .arg("--mmproj")
            .arg(&mmproj_path);
        if accelerator.kind.uses_gpu() {
            cmd.args(["-ngl", "all", "-fa", "auto"]);
        } else {
            cmd.args(["-ngl", "0", "--no-mmproj-offload"]);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        configure_no_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动本地视觉推理进程失败：{e}"))?;
        let job = JobHandle::for_child(&child);
        let log = LlamaLog::default();
        if let Some(stderr) = child.stderr.take() {
            log.drain(stderr);
        }
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let deadline = Instant::now()
            + if model_id == "qwen3_vl_4b" {
                Duration::from_secs(90)
            } else {
                Duration::from_secs(60)
            };
        loop {
            if cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err("本地视觉模型启动已取消".into());
            }
            if let Ok(Some(status)) = child.try_wait() {
                std::thread::sleep(Duration::from_millis(100));
                return Err(with_log_hint(
                    format!(
                        "本地视觉推理进程提前退出（状态码 {:?}，{}）",
                        status.code(),
                        accelerator.display_text()
                    ),
                    &log,
                ));
            }
            if probe_http_health(addr, Duration::from_millis(150)) {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(with_log_hint(
                    "本地视觉模型加载超时（首次把权重读进内存较慢，请稍后重试）".into(),
                    &log,
                ));
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        Ok(Self {
            model_id: model_id.to_owned(),
            port,
            accelerator,
            child,
            _job: job,
            log,
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// 视觉理解要填的地址（OpenAI 兼容基础地址）。
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn accelerator_text(&self) -> String {
        self.accelerator.display_text()
    }

    /// 推理进程是否还活着；意外退出时返回最近的错误输出。
    pub fn check_alive(&mut self) -> Result<(), String> {
        match self.child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => Err(with_log_hint(
                format!("本地视觉推理进程已退出（状态码 {:?}）", status.code()),
                &self.log,
            )),
            Err(error) => Err(format!("无法确认本地视觉推理进程状态：{error}")),
        }
    }
}

impl Drop for LocalVisionServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    /// 真实模型冒烟测试（默认跳过，需要事先下载模型）：
    /// `MOSS_LOCAL_VISION_ROOT=<models 目录> MOSS_LOCAL_VISION_IMAGES=<a.png,b.png> cargo test -p moss-voice local_vision -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_model_answers_questions_about_images() {
        let root = std::path::PathBuf::from(
            std::env::var("MOSS_LOCAL_VISION_ROOT").expect("MOSS_LOCAL_VISION_ROOT"),
        );
        let model =
            std::env::var("MOSS_LOCAL_VISION_MODEL").unwrap_or_else(|_| "qwen3_vl_2b".into());
        let images = std::env::var("MOSS_LOCAL_VISION_IMAGES").expect("MOSS_LOCAL_VISION_IMAGES");
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let server = LocalVisionServer::start(&model, &root.join(&model), &root, None, &cancel)
            .expect("本地视觉模型应能启动");
        println!(
            "加载用时 {:?}，{}，{}",
            started.elapsed(),
            server.accelerator_text(),
            server.base_url()
        );
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .unwrap();
        for path in images.split(',') {
            let bytes = std::fs::read(path).unwrap();
            let data = format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            );
            for question in [
                "用一两句话描述这张图里有什么。",
                "图里能看到哪些中文字？只列出最主要的几个。",
            ] {
                let t0 = Instant::now();
                let reply: serde_json::Value = client
                    .post(format!("{}/chat/completions", server.base_url()))
                    .json(&serde_json::json!({
                        "model": LOCAL_VISION_MODEL_NAME,
                        "stream": false,
                        "max_tokens": 200,
                        "messages": [{"role": "user", "content": [
                            {"type": "text", "text": question},
                            {"type": "image_url", "image_url": {"url": data}}
                        ]}]
                    }))
                    .send()
                    .unwrap()
                    .json()
                    .unwrap();
                let text = reply["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                println!("[{path}] {question}\n  → ({:?}) {text}", t0.elapsed());
                assert!(!text.trim().is_empty(), "模型应返回文字：{reply}");
            }
        }
    }
}
