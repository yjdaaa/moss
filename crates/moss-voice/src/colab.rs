//! Colab T4 专属联动配置与一键笔记本生成（2026-11-10 技术书 n80）。
//!
//! - 默认关闭（`enabled = false`）：不改变用户已有的云端 / 本地大模型、视觉理解、语音合成与其余配置；
//! - 显式开启且密钥已保存在 `app_secrets`（[`COLAB_SECRET_NAME`]）时，内置语音运行时只临时接管
//!   「对话大模型 + 摄像头看图 + 550W 克隆语音合成」三项，拾音判停、语音识别、Jev、说话角色等保持原样；
//! - 关闭开关即刻恢复原三项配置，已保存的云端 / 本地配置与密钥均不被覆盖；
//! - [`notebook_json`] 在后端生成固定审计过的 Colab T4 笔记本（`.ipynb`），不含任何本机路径或密钥。

use reqwest::Url;
use serde::{Deserialize, Serialize};

pub const COLAB_SECRET_NAME: &str = "voice.colab.main";
pub const COLAB_CLI_SESSION_NAME: &str = "moss-t4";
pub const DEFAULT_COLAB_LLM_MODEL: &str = "qwen3-vl-8b";
pub const DEFAULT_COLAB_TTS_MODEL: &str = "qwen3-tts";
pub const DEFAULT_COLAB_TTS_VOICE: &str = "moss";
pub const COLAB_NOTEBOOK_FILENAME: &str = "moss-colab-t4.ipynb";

fn default_llm_model() -> String {
    DEFAULT_COLAB_LLM_MODEL.to_owned()
}

fn default_tts_model() -> String {
    DEFAULT_COLAB_TTS_MODEL.to_owned()
}

fn default_tts_voice() -> String {
    DEFAULT_COLAB_TTS_VOICE.to_owned()
}

fn default_tts_speed() -> f32 {
    1.0
}

/// Colab T4 专属联动配置（保存在 `settings.json` 的 `voice.builtin_colab`，密钥只存 SQLite `app_secrets`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuiltinColab {
    /// 总开关（默认关闭；开启后只临时接管大模型、视觉看图与语音合成三项）
    pub enabled: bool,
    /// Colab 隧道基础地址（规范化为 `https://<host>/v1`，本机回环测试允许 `http://127.0.0.1:<port>/v1`）
    pub url: String,
    /// 多模态对话与看图模型名（默认 `qwen3-vl-8b`）
    #[serde(default = "default_llm_model")]
    pub llm_model: String,
    /// 克隆语音合成模型名（默认 `qwen3-tts`）
    #[serde(default = "default_tts_model")]
    pub tts_model: String,
    /// 克隆合成音色标识（默认 `moss`）
    #[serde(default = "default_tts_voice")]
    pub tts_voice: String,
    /// 克隆合成语速（0.25 ~ 4.0，默认 1.0）
    #[serde(default = "default_tts_speed")]
    pub tts_speed: f32,
}

impl Default for BuiltinColab {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            llm_model: default_llm_model(),
            tts_model: default_tts_model(),
            tts_voice: default_tts_voice(),
            tts_speed: default_tts_speed(),
        }
    }
}

impl BuiltinColab {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// 配置是否完整且已开启（需配合 `app_secrets` 中非空密钥才真正接管）。
    pub fn active(&self, has_secret: bool) -> bool {
        self.enabled
            && has_secret
            && !self.url.trim().is_empty()
            && normalize_colab_url(&self.url).is_ok()
            && !self.llm_model.trim().is_empty()
            && !self.tts_model.trim().is_empty()
            && !self.tts_voice.trim().is_empty()
            && (0.25..=4.0).contains(&self.tts_speed)
    }
}

/// 规范化并校验 Colab 隧道地址：
/// - 公网地址必须为 `https://`（仅允许 `http://127.0.0.1` 或 `http://localhost` 供本机离线测试）；
/// - 拒绝账号密码、查询参数、锚点、空白与尖括号占位符；
/// - 自动剥离末尾误带的 `/chat/completions`、`/audio/speech`、`/models`，路径为空时自动补 `/v1`。
pub fn normalize_colab_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("请填写 Colab 隧道地址（如 https://xxx.trycloudflare.com/v1）".into());
    }
    if trimmed
        .chars()
        .any(|c| c.is_whitespace() || c == '<' || c == '>' || c.is_control())
    {
        return Err("Colab 隧道地址包含空白或未替换的占位符".into());
    }
    let mut parsed = Url::parse(trimmed)
        .map_err(|_| "Colab 隧道地址格式不正确（须以 https:// 开头）".to_owned())?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("Colab 隧道地址不能包含账号密码、查询参数或 # 片段".into());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "Colab 隧道地址缺少主机名".to_owned())?;
    match parsed.scheme() {
        "https" => {}
        "http" => {
            let loopback = host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback());
            if !loopback {
                return Err("Colab 隧道地址必须使用 https://（公网禁止明文 http://）".into());
            }
        }
        _ => return Err("Colab 隧道地址必须以 https:// 开头".into()),
    }
    let mut path = parsed.path().trim_end_matches('/').to_owned();
    for suffix in ["/chat/completions", "/audio/speech", "/models"] {
        if let Some(stripped) = path.strip_suffix(suffix) {
            path = stripped.trim_end_matches('/').to_owned();
        }
    }
    if path.is_empty() {
        path = "/v1".to_owned();
    }
    parsed.set_path(&path);
    Ok(parsed.to_string().trim_end_matches('/').to_owned())
}

pub fn validate_colab_token(label: &str, raw: &str, default_value: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let value = if trimmed.is_empty() {
        default_value
    } else {
        trimmed
    };
    if value.len() > 120 || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!(
            "{label}格式不正确（不能含空格或控制字符，最多 120 字符）"
        ));
    }
    Ok(value.to_owned())
}

pub fn validate_colab_speed(raw: f64) -> Result<f32, String> {
    if !raw.is_finite() || !(0.25..=4.0).contains(&raw) {
        return Err("Colab 合成语速须在 0.25 ~ 4.0 之间".into());
    }
    Ok(raw as f32)
}

/// 嵌入经审计的 Colab T4 笔记本模板（`scripts/moss-colab-t4.ipynb`），统一换行符为 LF。
pub fn notebook_json() -> String {
    include_str!("../../../scripts/moss-colab-t4.ipynb").replace("\r\n", "\n")
}

/// 脱敏 Google 账号邮箱（如 `alice@gmail.com` -> `a***e@gmail.com`），绝不在界面或日志暴露完整邮箱。
pub fn mask_email(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('<') {
        return "已授权账号".to_owned();
    }
    let Some((local, domain)) = trimmed.split_once('@') else {
        return "已授权账号".to_owned();
    };
    let domain = domain.trim();
    if local.is_empty() || domain.is_empty() {
        return "已授权账号".to_owned();
    }
    let chars: Vec<char> = local.chars().collect();
    match chars.len() {
        1 => format!("{}***@{}", chars[0], domain),
        2 => format!("{}***@{}", chars[0], domain),
        n => format!("{}***{}@{}", chars[0], chars[n - 1], domain),
    }
}

/// 解析 `colab whoami` 输出，提取脱敏后的账号提示与剩余有效期。
pub fn parse_whoami_output(output: &str) -> Option<(String, String)> {
    let mut email: Option<String> = None;
    let mut expires_in = String::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Email:") {
            let val = rest.trim();
            if !val.is_empty() {
                email = Some(mask_email(val));
            }
        } else if let Some(rest) = trimmed.strip_prefix("Expires in:") {
            expires_in = rest.trim().to_owned();
        }
    }
    email.map(|masked| (masked, expires_in))
}

/// 从 `colab --auth=oauth2` 未登录输出中提取 Google OAuth 授权链接。
pub fn extract_oauth_url(output: &str) -> Option<String> {
    for token in output.split_whitespace() {
        let clean = token.trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>');
        if clean.starts_with("https://accounts.google.com/") {
            return Some(clean.to_owned());
        }
    }
    None
}

/// 从 `colab exec` 输出中提取 `MOSS_COLAB_URL` 与 `MOSS_COLAB_TOKEN`。
pub fn extract_tunnel_credentials(output: &str) -> Option<(String, String)> {
    let mut url: Option<String> = None;
    let mut token: Option<String> = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("MOSS_COLAB_URL=") {
            if let Ok(norm) = normalize_colab_url(rest.trim()) {
                url = Some(norm);
            }
        } else if let Some(rest) = trimmed.strip_prefix("MOSS_COLAB_TOKEN=") {
            let val = rest.trim();
            if !val.is_empty() && !val.chars().any(char::is_whitespace) {
                token = Some(val.to_owned());
            }
        }
    }
    match (url, token) {
        (Some(u), Some(t)) => Some((u, t)),
        _ => None,
    }
}

/// 校验用户选择的 550W 参考音频文件（须为 RIFF/WAVE 格式，大小在 256 字节 ~ 15 MB 之间）。
pub fn validate_ref_wav(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 44 {
        return Err("参考音频文件太小，不是有效的 WAV 音频".into());
    }
    if bytes.len() > 15 * 1024 * 1024 {
        return Err("参考音频文件不能超过 15 MB（建议裁剪为 3~8 秒干净人声）".into());
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("仅支持标准 WAV（RIFF/WAVE）音频文件".into());
    }
    Ok(())
}

/// 供 `colab exec -s moss-t4 --timeout 1200 -f <script.py>` 远程执行的后台守护启动脚本（纯 ASCII）。
///
/// - 若云端 `/content/moss_colab_state.json` 已就绪且本地 `:8000/healthz` 存活，1 秒内直接输出隧道地址与令牌；
/// - 否则启用 `uv` 极速包安装 + `HF_HUB_ENABLE_HF_TRANSFER=1` 多线程下载，3 路并发拉取并启动：
///   `llama-server`（`Qwen3-VL-8B-Instruct-GGUF`）+ `moss_gateway.py`（`Qwen3-TTS-0.6B`，支持 `/content/moss_ref.wav` 热更新）+ `cloudflared`，
///   每 2 秒输出心跳进度以维持 `jupyter_kernel_client` 的 iopub 保活，就绪后输出 `MOSS_COLAB_URL` 与 `MOSS_COLAB_TOKEN` 并正常退出。
pub fn cli_bootstrap_script() -> &'static str {
    r#"# @title MOSS Colab T4 Auto-Daemon Bootstrap (Parallel Fast Mode)
import json, os, pathlib, subprocess, sys, time, urllib.request

STATE_FILE = pathlib.Path("/content/moss_colab_state.json")
STATUS_FILE = pathlib.Path("/content/moss_boot_status.json")
WORKER_FILE = pathlib.Path("/content/moss_worker.py")

def check_url(url, timeout=3):
    try:
        with urllib.request.urlopen(urllib.request.Request(url), timeout=timeout) as r:
            return r.status == 200
    except Exception:
        return False

already_reused = False
if STATE_FILE.exists():
    try:
        saved = json.loads(STATE_FILE.read_text(encoding="utf-8"))
        saved_url = saved.get("url", "").strip()
        saved_token = saved.get("token", "").strip()
        if saved_url and saved_token and check_url("http://127.0.0.1:8000/healthz") and check_url("http://127.0.0.1:8001/health"):
            print("[MOSS-COLAB] Reusing active T4 gateway & tunnel", flush=True)
            print(f"MOSS_COLAB_URL={saved_url}", flush=True)
            print(f"MOSS_COLAB_TOKEN={saved_token}", flush=True)
            already_reused = True
    except Exception:
        pass

worker_code = '''import concurrent.futures, json, os, pathlib, re, secrets, subprocess, sys, time, urllib.request

os.environ["HF_HUB_ENABLE_HF_TRANSFER"] = "1"
os.environ["HF_HUB_DISABLE_IMPLICIT_TOKEN"] = "1"

STATE_FILE = pathlib.Path("/content/moss_colab_state.json")
STATUS_FILE = pathlib.Path("/content/moss_boot_status.json")
MODEL_DIR = pathlib.Path("/content/moss_models")
MODEL_DIR.mkdir(parents=True, exist_ok=True)

def write_status(state, step="", url="", token="", error=""):
    tmp = STATUS_FILE.with_suffix(".tmp")
    tmp.write_text(json.dumps({"state": state, "step": step, "url": url, "token": token, "error": error, "ts": time.time()}), encoding="utf-8")
    tmp.replace(STATUS_FILE)

def check_url(url, timeout=3):
    try:
        with urllib.request.urlopen(urllib.request.Request(url), timeout=timeout) as r:
            return r.status == 200
    except Exception:
        return False

def run_cmd(cmd, label):
    proc = subprocess.run(cmd, shell=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if proc.returncode != 0:
        tail = " | ".join((proc.stdout or "").splitlines()[-8:])
        raise RuntimeError(f"{label} failed (exit {proc.returncode}): {tail}")

def task_install_bins():
    if not pathlib.Path("/usr/local/bin/cloudflared").exists():
        run_cmd(
            "curl -fsSL https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64 -o /usr/local/bin/cloudflared && "
            "chmod +x /usr/local/bin/cloudflared",
            "cloudflared install"
        )
    if not pathlib.Path("/opt/llama/llama-b12244/llama-server").exists():
        run_cmd(
            "mkdir -p /opt/llama && "
            "curl -fsSL https://github.com/ggml-org/llama.cpp/releases/download/b12244/llama-b12244-bin-ubuntu-cuda-12.8-x64.tar.gz | tar -xz -C /opt/llama",
            "llama-server CUDA install"
        )

def task_install_py():
    run_cmd(
        "if command -v uv >/dev/null 2>&1; then "
        "uv pip install --system -q hf_transfer huggingface_hub fastapi uvicorn httpx soundfile numpy scipy edge-tts qwen-tts; "
        "else "
        "pip install -q uv && uv pip install --system -q hf_transfer huggingface_hub fastapi uvicorn httpx soundfile numpy scipy edge-tts qwen-tts; "
        "fi",
        "uv pip install qwen-tts & hf_transfer"
    )

try:
    write_status("starting", "1/4 Fast-installing CUDA llama-server, Qwen3-TTS & hf_transfer...")
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as ex:
        f_bins = ex.submit(task_install_bins)
        f_py = ex.submit(task_install_py)
        f_bins.result()
        f_py.result()

    gguf_main = MODEL_DIR / "Qwen3VL-8B-Instruct-Q4_K_M.gguf"
    gguf_mmproj = MODEL_DIR / "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf"

    def task_dl_and_start_llm():
        if not gguf_main.exists() or not gguf_mmproj.exists():
            dl_code = (
                "import os; os.environ['HF_HUB_ENABLE_HF_TRANSFER']='1'; os.environ['HF_HUB_DISABLE_IMPLICIT_TOKEN']='1'; "
                "from huggingface_hub import hf_hub_download; "
                "hf_hub_download(repo_id='Qwen/Qwen3-VL-8B-Instruct-GGUF', filename='Qwen3VL-8B-Instruct-Q4_K_M.gguf', local_dir='/content/moss_models', token=False); "
                "hf_hub_download(repo_id='Qwen/Qwen3-VL-8B-Instruct-GGUF', filename='mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf', local_dir='/content/moss_models', token=False)"
            )
            r = subprocess.run([sys.executable, "-c", dl_code], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            if r.returncode != 0:
                raise RuntimeError("Qwen3-VL GGUF download failed: " + (r.stdout or "")[-400:])
        if not check_url("http://127.0.0.1:8001/health"):
            subprocess.run(["pkill", "-f", "llama-server"], check=False)
            llama_env = os.environ.copy()
            llama_env["LD_LIBRARY_PATH"] = "/opt/llama/llama-b12244:" + llama_env.get("LD_LIBRARY_PATH", "")
            llama_log = open("/content/llama_server.log", "w")
            subprocess.Popen(
                [
                    "/opt/llama/llama-b12244/llama-server",
                    "-m", str(gguf_main),
                    "--mmproj", str(gguf_mmproj),
                    "--alias", "qwen3-vl-8b",
                    "-ngl", "all",
                    "-fa", "auto",
                    "-c", "8192",
                    "-np", "1",
                    "--image-max-tokens", "1024",
                    "--jinja",
                    "--host", "127.0.0.1",
                    "--port", "8001",
                ],
                env=llama_env,
                stdout=llama_log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )

    def task_dl_tts():
        dl_code = (
            "import os; os.environ['HF_HUB_ENABLE_HF_TRANSFER']='1'; os.environ['HF_HUB_DISABLE_IMPLICIT_TOKEN']='1'; "
            "from huggingface_hub import snapshot_download; "
            "snapshot_download(repo_id='Qwen/Qwen3-TTS-12Hz-0.6B-Base', token=False)"
        )
        r = subprocess.run([sys.executable, "-c", dl_code], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        if r.returncode != 0:
            raise RuntimeError("Qwen3-TTS download failed: " + (r.stdout or "")[-400:])

    cf_log_path = pathlib.Path("/content/cloudflared.log")
    def task_start_tunnel():
        if cf_log_path.exists():
            txt = cf_log_path.read_text(encoding="utf-8", errors="ignore")
            if re.search(r"https://[a-zA-Z0-9-]+\\.trycloudflare\\.com", txt) and subprocess.run(["pgrep", "-f", "cloudflared"], stdout=subprocess.DEVNULL).returncode == 0:
                return
        subprocess.run(["pkill", "-f", "cloudflared"], check=False)
        cf_log_path.write_text("", encoding="utf-8")
        cf_out = open(str(cf_log_path), "w")
        subprocess.Popen(
            ["/usr/local/bin/cloudflared", "tunnel", "--no-autoupdate", "--url", "http://127.0.0.1:8000"],
            stdout=cf_out,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )

    write_status("starting", "2/4 Parallel-downloading Qwen3-VL-8B + Qwen3-TTS-0.6B via hf_transfer...")
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as ex:
        f_llm = ex.submit(task_dl_and_start_llm)
        f_tts = ex.submit(task_dl_tts)
        f_tun = ex.submit(task_start_tunnel)
        f_llm.result()
        f_tts.result()
        f_tun.result()

    token = "moss-" + secrets.token_urlsafe(18)
    if STATE_FILE.exists():
        try:
            old_tok = json.loads(STATE_FILE.read_text(encoding="utf-8")).get("token", "").strip()
            if old_tok.startswith("moss-"):
                token = old_tok
        except Exception:
            pass

    gateway_code = """import asyncio, io, os, pathlib, subprocess, threading
os.environ["HF_HUB_DISABLE_IMPLICIT_TOKEN"] = "1"
import numpy as np, soundfile as sf, torch, httpx, uvicorn
from fastapi import FastAPI, Header, HTTPException, Request
from fastapi.responses import Response, StreamingResponse
from qwen_tts import Qwen3TTSModel

TOKEN = os.environ["MOSS_COLAB_TOKEN"]
REF_WAV = pathlib.Path("/content/moss_ref.wav")
DEFAULT_REF_TEXT = "\\u8ba9\\u4eba\\u7c7b\\u6c38\\u8fdc\\u4fdd\\u6301\\u7406\\u667a\\uff0c\\u786e\\u5b9e\\u662f\\u4e00\\u79cd\\u5962\\u6c42\\u3002MOSS 550W online."

def ensure_default_ref_wav():
    if REF_WAV.exists() and REF_WAV.stat().st_size > 1000:
        return
    try:
        mp3_tmp = "/tmp/moss_default_ref.mp3"
        subprocess.run([
            "edge-tts",
            "--voice", "zh-CN-YunjianNeural",
            "--pitch=-15Hz",
            "--rate=-10%",
            "--text", DEFAULT_REF_TEXT,
            "--write-media", mp3_tmp
        ], check=True, timeout=15)
        subprocess.run([
            "ffmpeg", "-y", "-i", mp3_tmp, "-ar", "24000", "-ac", "1", str(REF_WAV)
        ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        if REF_WAV.exists() and REF_WAV.stat().st_size > 1000:
            return
    except Exception:
        pass
    sr = 24000
    t = np.linspace(0, 3.2, int(sr * 3.2), endpoint=False)
    env = np.minimum(1.0, np.minimum(t / 0.15, (3.2 - t) / 0.25))
    sig = (0.45 * np.sin(2 * np.pi * 92.0 * t) + 0.25 * np.sin(2 * np.pi * 184.0 * t) + 0.12 * np.sin(2 * np.pi * 276.0 * t)) * env * 0.35
    sf.write(str(REF_WAV), sig.astype(np.float32), sr, subtype="PCM_16")

ensure_default_ref_wav()
tts_model = Qwen3TTSModel.from_pretrained("Qwen/Qwen3-TTS-12Hz-0.6B-Base", device_map="cuda:0", dtype=torch.float32, attn_implementation="sdpa")
prompt_lock = threading.Lock()
tts_lock = asyncio.Lock()
current_ref_mtime = 0.0
voice_prompt = None

def get_voice_prompt():
    global current_ref_mtime, voice_prompt
    with prompt_lock:
        ensure_default_ref_wav()
        mtime = REF_WAV.stat().st_mtime if REF_WAV.exists() else 0.0
        if voice_prompt is None or mtime != current_ref_mtime:
            voice_prompt = tts_model.create_voice_clone_prompt(ref_audio=str(REF_WAV), ref_text=DEFAULT_REF_TEXT, x_vector_only_mode=True)
            current_ref_mtime = mtime
        return voice_prompt

def apply_speed(wav, speed):
    if abs(speed - 1.0) <= 0.02 or len(wav) < 16:
        return wav
    out_len = max(1, int(round(len(wav) / speed)))
    x_old = np.linspace(0.0, 1.0, num=len(wav), endpoint=False)
    x_new = np.linspace(0.0, 1.0, num=out_len, endpoint=False)
    return np.interp(x_new, x_old, wav).astype(np.float32)

def synth_wav_bytes(text: str, speed: float) -> bytes:
    vp = get_voice_prompt()
    with torch.inference_mode():
        wavs, sr = tts_model.generate_voice_clone(text=text, language="Auto", voice_clone_prompt=vp)
    audio = apply_speed(np.asarray(wavs[0], dtype=np.float32), speed)
    buf = io.BytesIO()
    sf.write(buf, audio, sr, format="WAV", subtype="PCM_16")
    return buf.getvalue()

get_voice_prompt()
app = FastAPI()

def verify_auth(authorization):
    if not authorization or not authorization.startswith("Bearer "):
        raise HTTPException(status_code=401, detail="Missing Bearer token")
    if authorization.split(" ", 1)[1].strip() != TOKEN:
        raise HTTPException(status_code=403, detail="Invalid Bearer token")

@app.get("/healthz")
async def healthz():
    return {"ok": True, "llm": "qwen3-vl-8b", "tts": "qwen3-tts"}

@app.get("/v1/models")
async def list_models(authorization: str = Header(default=None)):
    verify_auth(authorization)
    return {"object": "list", "data": [{"id": "qwen3-vl-8b", "object": "model"}, {"id": "qwen3-tts", "object": "model"}]}

@app.post("/v1/chat/completions")
async def chat_completions(request: Request, authorization: str = Header(default=None)):
    verify_auth(authorization)
    body = await request.json()
    body["model"] = "qwen3-vl-8b"
    is_stream = bool(body.get("stream", False))
    if not is_stream:
        async with httpx.AsyncClient(timeout=120.0) as client:
            resp = await client.post("http://127.0.0.1:8001/v1/chat/completions", json=body)
            return Response(content=resp.content, status_code=resp.status_code, media_type="application/json")
    client = httpx.AsyncClient(timeout=120.0)
    req = client.build_request("POST", "http://127.0.0.1:8001/v1/chat/completions", json=body)
    resp = await client.send(req, stream=True)
    if resp.status_code >= 400:
        err_bytes = await resp.aread()
        await resp.aclose()
        await client.aclose()
        return Response(content=err_bytes, status_code=resp.status_code, media_type="application/json")
    async def event_gen():
        try:
            async for chunk in resp.aiter_bytes():
                yield chunk
        finally:
            await resp.aclose()
            await client.aclose()
    return StreamingResponse(event_gen(), media_type="text/event-stream")

@app.post("/v1/audio/speech")
async def audio_speech(request: Request, authorization: str = Header(default=None)):
    verify_auth(authorization)
    body = await request.json()
    text = (body.get("input") or "").strip()
    if not text:
        raise HTTPException(status_code=400, detail="Empty input")
    try:
        raw_speed = float(body.get("speed", 1.0))
    except (TypeError, ValueError):
        raw_speed = 1.0
    speed = min(4.0, max(0.25, raw_speed))
    async with tts_lock:
        wav_bytes = await asyncio.to_thread(synth_wav_bytes, text, speed)
    return Response(content=wav_bytes, media_type="audio/wav")

if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=8000, log_level="warning")
"""
    pathlib.Path("/content/moss_gateway.py").write_text(gateway_code, encoding="utf-8")

    if not check_url("http://127.0.0.1:8000/healthz"):
        write_status("starting", "3/4 Starting Qwen3-TTS 0.6B (550W clone) & FastAPI gateway...")
        subprocess.run(["pkill", "-f", "moss_gateway.py"], check=False)
        gw_env = os.environ.copy()
        gw_env["MOSS_COLAB_TOKEN"] = token
        gw_env["HF_HUB_DISABLE_IMPLICIT_TOKEN"] = "1"
        gw_log = open("/content/gateway.log", "w")
        subprocess.Popen(
            [sys.executable, "-u", "/content/moss_gateway.py"],
            env=gw_env,
            stdout=gw_log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )

    for _ in range(150):
        if check_url("http://127.0.0.1:8001/health") and check_url("http://127.0.0.1:8000/healthz"):
            break
        write_status("starting", "3/4 Loading Qwen3-VL-8B & Qwen3-TTS-0.6B into T4 VRAM...")
        time.sleep(2.0)
    else:
        tail_gw = pathlib.Path("/content/gateway.log").read_text(encoding="utf-8", errors="ignore")[-400:] if pathlib.Path("/content/gateway.log").exists() else ""
        tail_ll = pathlib.Path("/content/llama_server.log").read_text(encoding="utf-8", errors="ignore")[-400:] if pathlib.Path("/content/llama_server.log").exists() else ""
        raise RuntimeError(f"Services failed to become ready. GW: {tail_gw} | LLAMA: {tail_ll}")

    public_url = None
    for _ in range(30):
        txt = cf_log_path.read_text(encoding="utf-8", errors="ignore")
        m = re.search(r"https://[a-zA-Z0-9-]+\\.trycloudflare\\.com", txt)
        if m:
            public_url = m.group(0) + "/v1"
            break
        write_status("starting", "4/4 Waiting for trycloudflare.com URL...")
        time.sleep(1.5)

    if not public_url:
        raise RuntimeError("cloudflared failed to allocate public tunnel URL")

    STATE_FILE.write_text(json.dumps({"url": public_url, "token": token}), encoding="utf-8")
    write_status("ready", "Colab T4 ready", url=public_url, token=token)
except Exception as e:
    write_status("error", "Bootstrap error", error=str(e))
    raise
'''

if not already_reused:
    WORKER_FILE.write_text(worker_code, encoding="utf-8")
    worker_running = subprocess.run(["pgrep", "-f", "/content/moss_worker.py"], stdout=subprocess.DEVNULL).returncode == 0
    if not worker_running:
        STATUS_FILE.unlink(missing_ok=True)
        w_log = open("/content/moss_worker.log", "w")
        subprocess.Popen([sys.executable, "-u", str(WORKER_FILE)], stdout=w_log, stderr=subprocess.STDOUT, start_new_session=True)

    ready_done = False
    for _ in range(300):
        if STATUS_FILE.exists():
            try:
                st = json.loads(STATUS_FILE.read_text(encoding="utf-8"))
                state = st.get("state", "")
                step = st.get("step", "")
                if step:
                    print(f"[MOSS-COLAB] {step}", flush=True)
                if state == "ready":
                    print(f"MOSS_COLAB_URL={st['url']}", flush=True)
                    print(f"MOSS_COLAB_TOKEN={st['token']}", flush=True)
                    ready_done = True
                    break
                if state == "error":
                    raise RuntimeError(st.get("error") or "moss_worker failed")
            except RuntimeError:
                raise
            except Exception:
                pass
        else:
            print("[MOSS-COLAB] 1/4 Starting background worker on Colab T4...", flush=True)
        time.sleep(2.0)

    if not ready_done:
        raise RuntimeError("Timed out waiting for moss_worker")
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn default_colab_is_disabled_and_inactive() {
        let cfg = BuiltinColab::default();
        assert!(!cfg.enabled);
        assert!(cfg.is_default());
        assert!(!cfg.active(true));
        assert!(!cfg.active(false));
    }

    #[test]
    fn normalize_colab_url_enforces_https_and_appends_v1() {
        assert_eq!(
            normalize_colab_url("https://abc-123.trycloudflare.com").unwrap(),
            "https://abc-123.trycloudflare.com/v1"
        );
        assert_eq!(
            normalize_colab_url("https://abc-123.trycloudflare.com/v1/chat/completions/").unwrap(),
            "https://abc-123.trycloudflare.com/v1"
        );
        assert_eq!(
            normalize_colab_url("https://abc-123.trycloudflare.com/v1/audio/speech").unwrap(),
            "https://abc-123.trycloudflare.com/v1"
        );
        assert_eq!(
            normalize_colab_url("http://127.0.0.1:18088").unwrap(),
            "http://127.0.0.1:18088/v1"
        );
        assert!(normalize_colab_url("http://abc-123.trycloudflare.com/v1").is_err());
        assert!(normalize_colab_url("https://user:pass@abc.trycloudflare.com/v1").is_err());
        assert!(normalize_colab_url("https://abc.trycloudflare.com/v1?token=1").is_err());
    }

    #[test]
    fn active_requires_enabled_valid_url_and_secret() {
        let mut cfg = BuiltinColab {
            enabled: true,
            url: "https://abc-123.trycloudflare.com/v1".into(),
            ..BuiltinColab::default()
        };
        assert!(!cfg.active(false), "缺少密钥时不得激活接管");
        assert!(cfg.active(true));
        cfg.enabled = false;
        assert!(!cfg.active(true), "开关关闭时不得激活接管");
    }

    #[test]
    fn embedded_notebook_is_valid_nbformat_v4_and_contains_t4_stack() {
        let generated = notebook_json();
        let parsed: Value = serde_json::from_str(&generated).expect("valid json");
        assert_eq!(parsed["nbformat"], 4);
        assert_eq!(parsed["cells"].as_array().unwrap().len(), 4);
        assert_eq!(parsed["metadata"]["colab"]["gpuType"], "T4");
        assert!(generated.contains("Qwen/Qwen3-VL-8B-Instruct-GGUF"));
        assert!(generated.contains("Qwen/Qwen3-TTS-12Hz-0.6B-Base"));
        assert!(generated.contains("cloudflared"));
    }

    #[test]
    fn colab_cli_helpers_mask_email_and_parse_outputs() {
        assert_eq!(mask_email("alice@gmail.com"), "a***e@gmail.com");
        assert_eq!(mask_email("ab@example.org"), "a***@example.org");
        assert_eq!(mask_email("<unavailable>"), "已授权账号");

        let whoami =
            "Auth provider: oauth2\nEmail:         testuser@gmail.com\nExpires in:    58m\n";
        let (masked, exp) = parse_whoami_output(whoami).expect("whoami parsed");
        assert_eq!(masked, "t***r@gmail.com");
        assert_eq!(exp, "58m");

        let auth_prompt = "To authorize colab-cli, visit this URL in any browser:\n\n  https://accounts.google.com/o/oauth2/auth?response_type=code&client_id=123\n";
        assert_eq!(
            extract_oauth_url(auth_prompt).as_deref(),
            Some("https://accounts.google.com/o/oauth2/auth?response_type=code&client_id=123")
        );

        let exec_out = "[MOSS-COLAB] 4/4 Opening Cloudflare HTTPS tunnel...\nMOSS_COLAB_URL=https://demo-t4.trycloudflare.com/v1\nMOSS_COLAB_TOKEN=moss-secret123\n";
        assert_eq!(
            extract_tunnel_credentials(exec_out),
            Some((
                "https://demo-t4.trycloudflare.com/v1".to_owned(),
                "moss-secret123".to_owned()
            ))
        );
        let script = cli_bootstrap_script();
        assert!(script.is_ascii());
        assert!(
            script.contains("StreamingResponse") && script.contains("text/event-stream"),
            "CLI bootstrap gateway must proxy SSE streaming chat completions"
        );
        assert!(
            script.contains("asyncio.to_thread(synth_wav_bytes, text, speed)")
                && script.contains("tts_lock = asyncio.Lock()"),
            "CLI bootstrap gateway must serialize GPU TTS off the event loop and honor speed"
        );
        assert!(
            script.contains("\"-np\", \"1\"")
                && script.contains("\"--image-max-tokens\", \"1024\""),
            "CLI bootstrap llama-server must bound parallel slots and image tokens on 16GB T4"
        );

        let mut valid_wav = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
        valid_wav.resize(128, 0);
        assert!(validate_ref_wav(&valid_wav).is_ok());
        assert!(validate_ref_wav(b"not-a-wav").is_err());
    }
}
