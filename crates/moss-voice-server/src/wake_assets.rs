//! n61 设备唤醒词资源包（2026-11-06 用户决定：“把唤醒词改为moss”“直接帮我开发设置项”）。
//!
//! 小智固件（78/xiaozhi-esp32 v2 分区表，2.0.2 起）在开机时从 `assets` 分区读 `index.json`：
//! `srmodels` 指向打包好的语音模型，若其中有 MultiNet 模型就按 `multinet_model.commands`
//! 识别自定义唤醒词，否则用 WakeNet 固定唤醒词。资源包经设备自带的 user-only MCP 工具
//! `self.assets.set_download_url` 写下下载地址，重启后由设备自己下载、整块写入并校验。
//!
//! 打包格式与 `78/xiaozhi-assets-generator`（MIT）的 `SpiffsGenerator` / `WakenetModelPacker` 一致：
//! - 资源包：`u32 文件数 | u32 校验和(字节和 & 0xFFFF) | u32 数据长度 | 文件表 | 数据`，
//!   文件表每项 `name[32] u32 size u32 offset u16 width u16 height`，每个文件数据前有 `0x5A 0x5A`。
//! - `srmodels.bin`：`u32 模型数 | 每个模型 name[32] u32 文件数 {name[32] u32 start u32 len}… | 数据`。
//!
//! 本模块只做纯数据处理：不连设备、不写闪存。下发由宿主在私密窗口逐台确认后发起。

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

/// 16 MB 闪存的 v2 分区表 `assets` 为 8 MB；更大的包设备一定拒收。
pub const ASSETS_MAX_BYTES: usize = 8 * 1024 * 1024;
/// 8 MB 闪存的 v2 分区表 `assets` 只有 2 MB，装不下 MultiNet 模型。
pub const SMALL_ASSETS_PARTITION_BYTES: usize = 2 * 1024 * 1024;
/// 资源包下载地址的有效期。
pub const STAGED_TTL: Duration = Duration::from_secs(30 * 60);
/// 默认阈值与检测窗口，和官方生成器一致（滑块 20 → 0.2，3000 ms）。
pub const DEFAULT_THRESHOLD: f64 = 0.2;
pub const DEFAULT_DURATION_MS: u32 = 3000;
/// 唤醒词显示文字上限（字符）。
pub const MAX_TEXT_CHARS: usize = 16;
/// 拼音音节数范围：乐鑫建议 3～6 个音节；2 个音节也允许（如“mo si”），但误唤醒可能偏多。
pub const MIN_SYLLABLES: usize = 2;
pub const MAX_SYLLABLES: usize = 6;

const NAME_LEN: usize = 32;
const SRMODELS_FILE: &str = "srmodels.bin";
const INDEX_FILE: &str = "index.json";

const MN6_INFO: &[u8] = include_bytes!("../assets/esp-sr/mn6_cn/_MODEL_INFO_");
const MN6_DATA: &[u8] = include_bytes!("../assets/esp-sr/mn6_cn/mn6_data");
const MN6_INDEX: &[u8] = include_bytes!("../assets/esp-sr/mn6_cn/mn6_index");
const MN6_VOCAB: &[u8] = include_bytes!("../assets/esp-sr/mn6_cn/vocab");
const FST_CN: &[u8] = include_bytes!("../assets/esp-sr/fst/commands_cn.txt");
const FST_EN: &[u8] = include_bytes!("../assets/esp-sr/fst/commands_en.txt");
const WN9_INFO: &[u8] = include_bytes!("../assets/esp-sr/wn9_nihaoxiaozhi_tts/_MODEL_INFO_");
const WN9_DATA: &[u8] = include_bytes!("../assets/esp-sr/wn9_nihaoxiaozhi_tts/wn9_data");
const WN9_INDEX: &[u8] = include_bytes!("../assets/esp-sr/wn9_nihaoxiaozhi_tts/wn9_index");

/// 资源包里的一个文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    pub name: String,
    pub data: Vec<u8>,
    pub width: u16,
    pub height: u16,
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().expect("four bytes")))
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    bytes.get(at..at + 2).map(|b| u16::from_le_bytes(b.try_into().expect("two bytes")))
}

fn fixed_name(raw: &[u8]) -> Option<String> {
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    let name = std::str::from_utf8(&raw[..end]).ok()?;
    (!name.is_empty() && !name.chars().any(char::is_control)).then(|| name.to_owned())
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |sum, b| sum.wrapping_add(u32::from(*b))) & 0xFFFF
}

/// 解析资源包并逐项校验（头部长度、校验和、文件表边界、`ZZ` 魔数、重名）。
pub fn parse_assets(bytes: &[u8]) -> Result<Vec<AssetEntry>, &'static str> {
    if bytes.len() > ASSETS_MAX_BYTES {
        return Err("资源包超过 8 MB，设备装不下");
    }
    let (Some(count), Some(stored_sum), Some(len)) = (u32_at(bytes, 0), u32_at(bytes, 4), u32_at(bytes, 8)) else {
        return Err("不是小智资源包（文件太短）");
    };
    let (count, len) = (count as usize, len as usize);
    if count == 0 || count > 4096 || len != bytes.len() - 12 {
        return Err("不是小智资源包（头部长度不对）");
    }
    let body = &bytes[12..];
    if checksum(body) != stored_sum {
        return Err("资源包校验和不对，文件可能损坏");
    }
    let table = count * (NAME_LEN + 12);
    if table > body.len() {
        return Err("资源包文件表超出文件长度");
    }
    let mut seen = BTreeSet::new();
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let row = i * (NAME_LEN + 12);
        let name = fixed_name(&body[row..row + NAME_LEN]).ok_or("资源包里有无效的文件名")?;
        let size = u32_at(body, row + NAME_LEN).ok_or("资源包文件表损坏")? as usize;
        let offset = u32_at(body, row + NAME_LEN + 4).ok_or("资源包文件表损坏")? as usize;
        let width = u16_at(body, row + NAME_LEN + 8).ok_or("资源包文件表损坏")?;
        let height = u16_at(body, row + NAME_LEN + 10).ok_or("资源包文件表损坏")?;
        let start = table.checked_add(offset).ok_or("资源包文件表损坏")?;
        let end = start.checked_add(2).and_then(|s| s.checked_add(size)).ok_or("资源包文件表损坏")?;
        if end > body.len() || body[start] != 0x5A || body[start + 1] != 0x5A {
            return Err("资源包里的文件数据损坏");
        }
        if !seen.insert(name.clone()) {
            return Err("资源包里有重名文件");
        }
        entries.push(AssetEntry { name, data: body[start + 2..end].to_vec(), width, height });
    }
    Ok(entries)
}

/// 按官方生成器的顺序（先扩展名、再文件名）打包。
pub fn pack_assets(entries: &[AssetEntry]) -> Result<Vec<u8>, &'static str> {
    if entries.is_empty() {
        return Err("资源包不能为空");
    }
    let ext = |name: &str| name.rsplit_once('.').map_or("", |(_, e)| e).to_owned();
    let stem = |name: &str| name.rsplit_once('.').map_or(name, |(s, _)| s).to_owned();
    let mut sorted: Vec<&AssetEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| ext(&a.name).cmp(&ext(&b.name)).then_with(|| stem(&a.name).cmp(&stem(&b.name))));
    let mut table = Vec::with_capacity(sorted.len() * (NAME_LEN + 12));
    let mut data = Vec::new();
    for entry in &sorted {
        if entry.name.is_empty() || entry.name.len() > NAME_LEN {
            return Err("资源包文件名须为 1～32 字节");
        }
        let mut name = [0u8; NAME_LEN];
        name[..entry.name.len()].copy_from_slice(entry.name.as_bytes());
        table.extend_from_slice(&name);
        table.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        table.extend_from_slice(&(data.len() as u32).to_le_bytes());
        table.extend_from_slice(&entry.width.to_le_bytes());
        table.extend_from_slice(&entry.height.to_le_bytes());
        data.extend_from_slice(&[0x5A, 0x5A]);
        data.extend_from_slice(&entry.data);
    }
    let mut body = table;
    body.extend_from_slice(&data);
    let mut out = Vec::with_capacity(12 + body.len());
    out.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
    out.extend_from_slice(&checksum(&body).to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// 打包 `srmodels.bin`（与 ESP-SR `pack_model.py` 一致：模型、文件均按名字排序）。
pub fn pack_srmodels(models: &[(&str, &[(&str, &[u8])])]) -> Vec<u8> {
    let mut models: Vec<(&str, Vec<(&str, &[u8])>)> = models.iter().map(|(n, f)| (*n, f.to_vec())).collect();
    models.sort_by(|a, b| a.0.cmp(b.0));
    for (_, files) in &mut models {
        files.sort_by(|a, b| a.0.cmp(b.0));
    }
    let name = |s: &str| {
        let mut raw = [0u8; NAME_LEN];
        let bytes = s.as_bytes();
        raw[..bytes.len().min(NAME_LEN)].copy_from_slice(&bytes[..bytes.len().min(NAME_LEN)]);
        raw
    };
    let header: usize = 4 + models.iter().map(|(_, f)| NAME_LEN + 4 + f.len() * (NAME_LEN + 8)).sum::<usize>();
    let mut out = Vec::new();
    out.extend_from_slice(&(models.len() as u32).to_le_bytes());
    let mut offset = header;
    for (model, files) in &models {
        out.extend_from_slice(&name(model));
        out.extend_from_slice(&(files.len() as u32).to_le_bytes());
        for (file, data) in files {
            out.extend_from_slice(&name(file));
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            offset += data.len();
        }
    }
    for (_, files) in &models {
        for (_, data) in files {
            out.extend_from_slice(data);
        }
    }
    out
}

/// MultiNet6 中文模型认识的拼音音节（取自模型 `vocab`，形如 `▁mo\t-6.0`）。
pub fn pinyin_syllables() -> BTreeSet<&'static str> {
    std::str::from_utf8(MN6_VOCAB)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split('\t').next()?.strip_prefix('\u{2581}'))
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()))
        .collect()
}

/// 一个自定义唤醒词：显示文字 + 拼音命令词 + 阈值。
#[derive(Debug, Clone, PartialEq)]
pub struct WakeWord {
    pub text: String,
    pub command: String,
    pub threshold: f64,
}

/// 唤醒词的显示文字：1～16 字，无控制字符。
pub fn normalize_text(raw: &str) -> Result<String, String> {
    let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("请填写唤醒词".into());
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("唤醒词最多 {MAX_TEXT_CHARS} 个字"));
    }
    if text.chars().any(char::is_control) {
        return Err("唤醒词里不能有控制字符".into());
    }
    Ok(text)
}

/// 拼音命令词：小写、空格分隔、每个音节须是模型认识的拼音（不带声调），2～6 个音节。
pub fn normalize_command(raw: &str) -> Result<String, String> {
    let lower = raw.trim().to_ascii_lowercase();
    let syllables: Vec<&str> = lower.split_whitespace().collect();
    if syllables.len() < MIN_SYLLABLES || syllables.len() > MAX_SYLLABLES {
        return Err(format!("拼音需要 {MIN_SYLLABLES}～{MAX_SYLLABLES} 个音节，用空格隔开，例如 ni hao mo si"));
    }
    let known = pinyin_syllables();
    if let Some(bad) = syllables.iter().find(|s| !known.contains(**s)) {
        return Err(format!("“{bad}”不是模型认识的拼音音节（只写字母、不带声调，例如 mo si）"));
    }
    Ok(syllables.join(" "))
}

/// 阈值：0.05～0.95（官方生成器滑块 0～100 对应 0～1；太低容易误唤醒，太高不容易唤醒）。
pub fn normalize_threshold(raw: f64) -> Result<f64, String> {
    if !raw.is_finite() || !(0.05..=0.95).contains(&raw) {
        return Err("灵敏度阈值须在 0.05～0.95 之间".into());
    }
    Ok((raw * 100.0).round() / 100.0)
}

impl WakeWord {
    pub fn new(text: &str, command: &str, threshold: f64) -> Result<Self, String> {
        Ok(Self { text: normalize_text(text)?, command: normalize_command(command)?, threshold: normalize_threshold(threshold)? })
    }
}

/// 要装到设备上的唤醒词。
#[derive(Debug, Clone, PartialEq)]
pub enum WakeTarget {
    /// 自定义唤醒词（MultiNet6 中文模型，仅 ESP32-S3 / P4）
    Custom(WakeWord),
    /// 恢复官方“你好小智”（WakeNet9）
    Default,
}

fn srmodels_for(target: &WakeTarget) -> Vec<u8> {
    match target {
        WakeTarget::Custom(_) => pack_srmodels(&[
            ("fst", &[("commands_cn.txt", FST_CN), ("commands_en.txt", FST_EN)]),
            ("mn6_cn", &[("_MODEL_INFO_", MN6_INFO), ("mn6_data", MN6_DATA), ("mn6_index", MN6_INDEX), ("vocab", MN6_VOCAB)]),
        ]),
        WakeTarget::Default => {
            pack_srmodels(&[("wn9_nihaoxiaozhi_tts", &[("_MODEL_INFO_", WN9_INFO), ("wn9_data", WN9_DATA), ("wn9_index", WN9_INDEX)])])
        }
    }
}

/// 生成要下发的资源包。
///
/// 有基础包（用户导入的现有资源包，含字体、表情、背景）时只替换语音模型和 `index.json` 的
/// `srmodels` / `multinet_model`，其余文件与配置原样保留；没有基础包时只含模型和最小 `index.json`，
/// 设备屏幕的字体和表情会回到固件自带的。
pub fn build_package(base: Option<&[u8]>, target: &WakeTarget) -> Result<Vec<u8>, String> {
    let (mut entries, mut index) = match base {
        Some(bytes) => {
            let entries = parse_assets(bytes).map_err(str::to_owned)?;
            let raw = entries.iter().find(|e| e.name == INDEX_FILE).ok_or("基础资源包里没有 index.json")?;
            let index: Value = serde_json::from_slice(&raw.data).map_err(|_| "基础资源包的 index.json 不是有效 JSON")?;
            let Value::Object(index) = index else { return Err("基础资源包的 index.json 格式不对".into()) };
            if index.get("version").and_then(Value::as_f64).is_some_and(|v| v > 1.0) {
                return Err("基础资源包版本比 MOSS 认识的新，请换一个资源包".into());
            }
            (entries, index)
        }
        None => (Vec::new(), Map::from_iter([("version".to_owned(), json!(1))])),
    };
    let old_models = index.get("srmodels").and_then(Value::as_str).map(str::to_owned);
    entries.retain(|e| e.name != INDEX_FILE && e.name != SRMODELS_FILE && Some(&e.name) != old_models.as_ref());
    index.insert("srmodels".into(), json!(SRMODELS_FILE));
    match target {
        WakeTarget::Custom(word) => {
            index.insert(
                "multinet_model".into(),
                json!({
                    "language": "cn",
                    "duration": DEFAULT_DURATION_MS,
                    "threshold": word.threshold,
                    "commands": [{ "command": word.command, "text": word.text, "action": "wake" }],
                }),
            );
        }
        WakeTarget::Default => {
            index.remove("multinet_model");
        }
    }
    entries.push(AssetEntry { name: SRMODELS_FILE.into(), data: srmodels_for(target), width: 0, height: 0 });
    let index = serde_json::to_vec(&Value::Object(index)).map_err(|_| "生成 index.json 失败")?;
    entries.push(AssetEntry { name: INDEX_FILE.into(), data: index, width: 0, height: 0 });
    let package = pack_assets(&entries).map_err(str::to_owned)?;
    if package.len() > ASSETS_MAX_BYTES {
        return Err(format!("生成的资源包 {:.1} MB，超过设备资源分区 8 MB；请换一个更小的基础资源包", package.len() as f64 / 1048576.0));
    }
    Ok(package)
}

/// 基础资源包里能看到的摘要（给界面显示）。
pub fn describe_base(bytes: &[u8]) -> Result<Value, String> {
    let entries = parse_assets(bytes).map_err(str::to_owned)?;
    let index = entries
        .iter()
        .find(|e| e.name == INDEX_FILE)
        .and_then(|e| serde_json::from_slice::<Value>(&e.data).ok())
        .filter(Value::is_object)
        .ok_or("基础资源包里没有有效的 index.json")?;
    let wake = index["multinet_model"]["commands"][0]["text"].as_str().map(str::to_owned);
    Ok(json!({
        "files": entries.len(),
        "size": bytes.len(),
        "chip_model": index["chip_model"].as_str(),
        "text_font": index["text_font"].as_str(),
        "emoji_count": index["emoji_collection"].as_array().map_or(0, Vec::len),
        "wake_word": wake,
    }))
}

/// 一台设备待下载的资源包。
struct Staged {
    device_id: String,
    bytes: std::sync::Arc<Vec<u8>>,
    expires: Instant,
}

/// 服务端内存里的待下载资源包：令牌随机、只对一台设备、30 分钟过期；同一台设备新推送会顶掉旧的。
#[derive(Default)]
pub struct AssetsStore {
    staged: Mutex<HashMap<String, Staged>>,
}

impl AssetsStore {
    pub fn stage(&self, device_id: &str, bytes: Vec<u8>) -> Result<String, String> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|_| "无法生成下载令牌".to_owned())?;
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let mut map = self.staged.lock().map_err(|_| "资源包表锁失败".to_owned())?;
        let now = Instant::now();
        map.retain(|_, s| s.expires > now && s.device_id != device_id);
        map.insert(token.clone(), Staged { device_id: device_id.to_owned(), bytes: std::sync::Arc::new(bytes), expires: now + STAGED_TTL });
        Ok(token)
    }

    /// 按令牌取资源包：返回 (设备编号, 内容)。
    pub fn fetch(&self, token: &str) -> Option<(String, std::sync::Arc<Vec<u8>>)> {
        if !crate::firmware::token_ok(token) {
            return None;
        }
        let map = self.staged.lock().ok()?;
        map.get(token).filter(|s| s.expires > Instant::now()).map(|s| (s.device_id.clone(), s.bytes.clone()))
    }
}

pub(crate) fn router(shared: std::sync::Arc<crate::Shared>) -> axum::Router {
    use axum::extract::{ConnectInfo, Path, State};
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    async fn download(
        State(shared): State<std::sync::Arc<crate::Shared>>,
        ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
        Path(file): Path<String>,
    ) -> axum::response::Response {
        let token = file.strip_suffix(".bin").unwrap_or(&file);
        match shared.assets_store.fetch(token) {
            Some((device, bytes)) => {
                shared.state.event(format!("唤醒词资源包：设备 {device}（{}）开始下载 {} 字节", peer.ip(), bytes.len()));
                ([(header::CONTENT_TYPE, "application/octet-stream")], bytes.as_ref().clone()).into_response()
            }
            None => {
                shared.state.event(format!("唤醒词资源包下载被拒绝（{}）：令牌无效或已过期", peer.ip()));
                StatusCode::NOT_FOUND.into_response()
            }
        }
    }
    axum::Router::new().route("/xiaozhi/assets/{file}", axum::routing::get(download)).with_state(shared)
}

#[cfg(test)]
mod tests;
