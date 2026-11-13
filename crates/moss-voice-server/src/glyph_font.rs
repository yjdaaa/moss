//! 小智设备屏幕三层字库与 `glyph_push` v1 动态字模补齐（技术书 n81 / n83，2026-11-12）。
//!
//! 完全对齐官方 `78/xiaozhi-esp32`（`docs/glyph-push_zh.md`、`main/assets.cc`、`main/display/text_glyph_payload.cc`）
//! 与 `78/xiaozhi-fonts`（`scripts/glyph_provider.py`，提交 `d45dbc64052d57048f20ab1770074172ce9eb53b`）三层字库体系：
//! 1. 第 1 层 `basic`（890 字，焊死在固件里）：ASCII、Latin-1 与系统界面词，保底不黑屏；
//! 2. 第 2 层 `common`（6,845 字，刷在设备 `assets` 分区）：没有导入外部 `base-assets.bin` 时，MOSS 生成的唤醒词 /
//!    全屏画风资源包自动内置匹配规格（`14_1` / `16_4` / `20_4` / `30_4`）的 `common` CBIN 字库与 `text_font_meta`；
//! 3. 第 3 层 `full`（`noto-v1-full`，48,662 字，服务端简/繁/日/韩/泰/阿 7 套预编译 `.cbin` 分片，不下发整包）：
//!    设备握手报告 `features.glyph_push = true` 与本地已装字库（`basic` 或 `common`）后，MOSS 在每次下发 `stt` 与
//!    `tts(sentence_start)` 前比对设备字表；只要发现缺字（含已装 `common` 设备遇到“椴”“𠮷”等生僻字、繁体字或多语种），
//!    立即从服务端对应分片按需懒加载并抠出缺失点阵随消息下发（单句上限 64 字 / 64 KB）。`noto-v1-full` 尚未下载完成或离线时，
//!    自动对 `basic` 设备回退内置 `common` CBIN 字库；下载就位后热生效，无需重启服务。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use moss_xiaozhi_proto::{GlyphItem, GlyphPushPayload, TextFontCapability};
use serde_json::{Value, json};

use crate::goggle_bot::base64_encode;
use crate::wake_assets::AssetEntry;

pub const FONT_BUNDLE_ID: &str = "noto-v1";
const MAX_GLYPHS: usize = 64;
const MAX_BITMAP_BYTES: usize = 64 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SHARD_FILE_BYTES: u64 = 64 * 1024 * 1024;

const CMAP_FORMAT0_FULL: u8 = 0;
const CMAP_SPARSE_FULL: u8 = 1;
const CMAP_FORMAT0_TINY: u8 = 2;
const CMAP_SPARSE_TINY: u8 = 3;
const BITMAP_FORMAT_PLAIN: u8 = 0;

const BASIC_CHARSET_JSON: &[u8] = include_bytes!("../assets/xiaozhi-fonts/basic.json");
const COMMON_14_1: &[u8] = include_bytes!("../assets/xiaozhi-fonts/font_noto_sans_common_14_1.bin");
const COMMON_16_4: &[u8] = include_bytes!("../assets/xiaozhi-fonts/font_noto_sans_common_16_4.bin");
const COMMON_20_4: &[u8] = include_bytes!("../assets/xiaozhi-fonts/font_noto_sans_common_20_4.bin");
const COMMON_30_4: &[u8] = include_bytes!("../assets/xiaozhi-fonts/font_noto_sans_common_30_4.bin");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontProfile {
    pub size: u32,
    pub bpp: u8,
    pub file_name: &'static str,
    pub data: &'static [u8],
}

pub const PROFILES: [FontProfile; 4] = [
    FontProfile {
        size: 14,
        bpp: 1,
        file_name: "font_noto_sans_common_14_1.bin",
        data: COMMON_14_1,
    },
    FontProfile {
        size: 16,
        bpp: 4,
        file_name: "font_noto_sans_common_16_4.bin",
        data: COMMON_16_4,
    },
    FontProfile {
        size: 20,
        bpp: 4,
        file_name: "font_noto_sans_common_20_4.bin",
        data: COMMON_20_4,
    },
    FontProfile {
        size: 30,
        bpp: 4,
        file_name: "font_noto_sans_common_30_4.bin",
        data: COMMON_30_4,
    },
];

pub fn profile_by_spec(size: u32, bpp: u8) -> Option<FontProfile> {
    PROFILES
        .into_iter()
        .find(|p| p.size == size && p.bpp == bpp)
}

/// 为资源包选择匹配的内置 `common` 中文字库：
/// 优先采用设备 `hello` 报告过的 `(size, bpp)`；未知时单色屏用 `14_1`，小彩屏用 `16_4`，超大屏用 `30_4`，其余默认 `20_4`。
pub fn select_pack_font(
    preferred: Option<(u32, u8)>,
    screen: Option<(u32, u32)>,
    monochrome: bool,
) -> FontProfile {
    if let Some((size, bpp)) = preferred
        && let Some(found) = profile_by_spec(size, bpp)
    {
        return found;
    }
    if monochrome {
        return PROFILES[0];
    }
    match screen.map(|(w, h)| w.min(h)) {
        Some(short) if short < 200 => PROFILES[1],
        Some(short) if short >= 480 => PROFILES[3],
        _ => PROFILES[2],
    }
}

/// 构造写入资源包的文件项与 `index.json` 元数据（`text_font` + `text_font_meta`）。
pub fn pack_font_entry_and_meta(profile: FontProfile) -> (AssetEntry, String, Value) {
    let entry = AssetEntry {
        name: profile.file_name.to_owned(),
        data: profile.data.to_vec(),
        width: 0,
        height: 0,
    };
    let meta = json!({
        "bundle": FONT_BUNDLE_ID,
        "charset": "common",
        "size": profile.size,
        "bpp": profile.bpp,
    });
    (entry, profile.file_name.to_owned(), meta)
}

/// n84：内置 `common` 字库档位的确定性内容摘要（进程内只算一次），字库文件或规格变更时自动刷新资源包指纹。
pub fn font_profile_digest(profile: FontProfile) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    fn compute(p: FontProfile) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"moss-pack-font:v1\0");
        h.update(FONT_BUNDLE_ID.as_bytes());
        h.update([0]);
        h.update(p.file_name.as_bytes());
        h.update([0]);
        h.update(p.size.to_le_bytes());
        h.update([p.bpp]);
        h.update((p.data.len() as u64).to_le_bytes());
        h.update(p.data);
        h.finalize().into()
    }

    static DIGESTS: OnceLock<[([u8; 32], u32, u8); 4]> = OnceLock::new();
    let table = DIGESTS.get_or_init(|| PROFILES.map(|p| (compute(p), p.size, p.bpp)));
    table
        .iter()
        .find(|(_, s, b)| {
            *s == profile.size && *b == profile.bpp && {
                profile_by_spec(*s, *b).is_some_and(|known| std::ptr::eq(known.data, profile.data))
            }
        })
        .map(|(d, _, _)| *d)
        .unwrap_or_else(|| compute(profile))
}

/// 从持久化的 `SNAPSHOT_HELLO`（`hello_json`）还原设备最近一次上报的 `glyph_push` 字库规格（供 MQTT 常连开机尚未唤醒前恢复动态补字）。
pub fn text_font_from_hello_snapshot(hello_json: &str) -> Option<TextFontCapability> {
    let v: Value = serde_json::from_str(hello_json).ok()?;
    if v["glyph_push"].as_bool() != Some(true) {
        return None;
    }
    serde_json::from_value::<TextFontCapability>(v.get("text_font")?.clone()).ok()
}

struct CmapEntry {
    start: u32,
    length: u16,
    glyph_start: u16,
    unicode_abs: usize,
    glyph_offset_abs: usize,
    count: u16,
    kind: u8,
}

enum FontBytes {
    Static(&'static [u8]),
    Owned(Arc<[u8]>),
}

impl AsRef<[u8]> for FontBytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Static(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }
}

struct ParsedCbin {
    data: FontBytes,
    bitmap_abs: usize,
    glyph_dsc_abs: usize,
    bpp: u8,
    cmaps: Vec<CmapEntry>,
}

fn u16_le(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at.checked_add(2)?)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn i16_le(bytes: &[u8], at: usize) -> Option<i16> {
    bytes
        .get(at..at.checked_add(2)?)
        .map(|s| i16::from_le_bytes([s[0], s[1]]))
}

fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at.checked_add(4)?)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

impl ParsedCbin {
    fn parse_static(data: &'static [u8]) -> Option<Self> {
        Self::parse(FontBytes::Static(data))
    }

    fn parse_owned(data: Vec<u8>) -> Option<Self> {
        Self::parse(FontBytes::Owned(data.into()))
    }

    fn parse(storage: FontBytes) -> Option<Self> {
        let data = storage.as_ref();
        let dsc = u32_le(data, 24)? as usize;
        let bitmap_offset = u32_le(data, dsc)? as usize;
        let glyph_dsc_offset = u32_le(data, dsc.checked_add(4)?)? as usize;
        let cmap_offset = u32_le(data, dsc.checked_add(8)?)? as usize;
        let bitfield = u16_le(data, dsc.checked_add(18)?)?;
        let stride = *data.get(dsc.checked_add(20)?)?;

        let bitmap_abs = dsc.checked_add(bitmap_offset)?;
        let glyph_dsc_abs = dsc.checked_add(glyph_dsc_offset)?;
        let cmap_abs = dsc.checked_add(cmap_offset)?;
        if bitmap_abs > data.len() || glyph_dsc_abs > data.len() || cmap_abs > data.len() {
            return None;
        }
        let cmap_count = usize::from(bitfield & 0x1FF);
        let bpp = ((bitfield >> 9) & 0xF) as u8;
        let bitmap_format = ((bitfield >> 14) & 0x3) as u8;
        if !matches!(bpp, 1 | 4) || bitmap_format != BITMAP_FORMAT_PLAIN || stride != 0 {
            return None;
        }

        let mut cmaps = Vec::with_capacity(cmap_count);
        let mut ptr = cmap_abs;
        for _ in 0..cmap_count {
            let start = u32_le(data, ptr)?;
            let length = u16_le(data, ptr.checked_add(4)?)?;
            let glyph_start = u16_le(data, ptr.checked_add(6)?)?;
            let unicode_offset = u32_le(data, ptr.checked_add(8)?)? as usize;
            let glyph_offset = u32_le(data, ptr.checked_add(12)?)? as usize;
            let count = u16_le(data, ptr.checked_add(16)?)?;
            let kind = *data.get(ptr.checked_add(18)?)?;
            cmaps.push(CmapEntry {
                start,
                length,
                glyph_start,
                unicode_abs: if unicode_offset != 0 {
                    cmap_abs.checked_add(unicode_offset)?
                } else {
                    0
                },
                glyph_offset_abs: if glyph_offset != 0 {
                    cmap_abs.checked_add(glyph_offset)?
                } else {
                    0
                },
                count,
                kind,
            });
            ptr = ptr.checked_add(20)?;
        }

        Some(Self {
            data: storage,
            bitmap_abs,
            glyph_dsc_abs,
            bpp,
            cmaps,
        })
    }

    fn sparse_index(&self, cmap: &CmapEntry, relative: u16) -> Option<usize> {
        let bytes = self.data.as_ref();
        let count = usize::from(cmap.count);
        if count == 0 || cmap.unicode_abs == 0 {
            return None;
        }
        let mut lo = 0usize;
        let mut hi = count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let val = u16_le(bytes, cmap.unicode_abs.checked_add(mid * 2)?)?;
            match val.cmp(&relative) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    fn glyph_id(&self, codepoint: u32) -> Option<usize> {
        let bytes = self.data.as_ref();
        for cmap in &self.cmaps {
            if codepoint < cmap.start {
                continue;
            }
            let relative = codepoint - cmap.start;
            if relative >= u32::from(cmap.length) {
                continue;
            }
            let id = match cmap.kind {
                CMAP_FORMAT0_TINY => {
                    usize::from(cmap.glyph_start).checked_add(relative as usize)?
                }
                CMAP_FORMAT0_FULL => {
                    let offset = if cmap.glyph_offset_abs != 0 {
                        usize::from(
                            *bytes.get(cmap.glyph_offset_abs.checked_add(relative as usize)?)?,
                        )
                    } else {
                        relative as usize
                    };
                    usize::from(cmap.glyph_start).checked_add(offset)?
                }
                CMAP_SPARSE_TINY | CMAP_SPARSE_FULL => {
                    let Ok(rel16) = u16::try_from(relative) else {
                        continue;
                    };
                    let Some(index) = self.sparse_index(cmap, rel16) else {
                        continue;
                    };
                    if cmap.kind == CMAP_SPARSE_TINY {
                        usize::from(cmap.glyph_start).checked_add(index)?
                    } else {
                        let offset = if cmap.glyph_offset_abs != 0 {
                            usize::from(u16_le(
                                bytes,
                                cmap.glyph_offset_abs.checked_add(index * 2)?,
                            )?)
                        } else {
                            index
                        };
                        usize::from(cmap.glyph_start).checked_add(offset)?
                    }
                }
                _ => continue,
            };
            if id != 0 {
                return Some(id);
            }
            return None;
        }
        None
    }

    fn glyph(&self, codepoint: u32) -> Option<(GlyphItem, usize)> {
        if !(1..=0x10FFFF).contains(&codepoint) {
            return None;
        }
        let bytes = self.data.as_ref();
        let glyph_id = self.glyph_id(codepoint)?;
        let offset = self.glyph_dsc_abs.checked_add(glyph_id.checked_mul(16)?)?;
        let bitmap_index = u32_le(bytes, offset)? as usize;
        let adv_w = u32_le(bytes, offset.checked_add(4)?)?;
        let box_w = u16_le(bytes, offset.checked_add(8)?)?;
        let box_h = u16_le(bytes, offset.checked_add(10)?)?;
        let ofs_x = i16_le(bytes, offset.checked_add(12)?)?;
        let ofs_y = i16_le(bytes, offset.checked_add(14)?)?;
        if box_w > 64 || box_h > 64 {
            return None;
        }
        let byte_count =
            (usize::from(box_w) * usize::from(box_h) * usize::from(self.bpp)).div_ceil(8);
        let bitmap = if byte_count == 0 {
            String::new()
        } else {
            let bmp_start = self.bitmap_abs.checked_add(bitmap_index)?;
            let bmp_end = bmp_start.checked_add(byte_count)?;
            base64_encode(bytes.get(bmp_start..bmp_end)?)
        };
        Some((
            GlyphItem {
                codepoint,
                adv_w,
                box_w,
                box_h,
                ofs_x,
                ofs_y,
                bitmap,
            },
            byte_count,
        ))
    }
}

fn basic_charset() -> &'static BTreeSet<u32> {
    static BASIC: OnceLock<BTreeSet<u32>> = OnceLock::new();
    BASIC.get_or_init(|| {
        let value: Value = serde_json::from_slice(BASIC_CHARSET_JSON).unwrap_or_default();
        value["codepoints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_u64().and_then(|cp| u32::try_from(cp).ok()))
            .collect()
    })
}

fn parsed_font(size: u32, bpp: u8) -> Option<&'static ParsedCbin> {
    static F14_1: OnceLock<Option<ParsedCbin>> = OnceLock::new();
    static F16_4: OnceLock<Option<ParsedCbin>> = OnceLock::new();
    static F20_4: OnceLock<Option<ParsedCbin>> = OnceLock::new();
    static F30_4: OnceLock<Option<ParsedCbin>> = OnceLock::new();
    match (size, bpp) {
        (14, 1) => F14_1
            .get_or_init(|| ParsedCbin::parse_static(COMMON_14_1))
            .as_ref(),
        (16, 4) => F16_4
            .get_or_init(|| ParsedCbin::parse_static(COMMON_16_4))
            .as_ref(),
        (20, 4) => F20_4
            .get_or_init(|| ParsedCbin::parse_static(COMMON_20_4))
            .as_ref(),
        (30, 4) => F30_4
            .get_or_init(|| ParsedCbin::parse_static(COMMON_30_4))
            .as_ref(),
        _ => None,
    }
}

fn safe_relative_cbin(rel: &str) -> Option<PathBuf> {
    let path = Path::new(rel);
    if path.as_os_str().is_empty() || !rel.ends_with(".cbin") {
        return None;
    }
    if !path
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    if !path.components().any(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(path.to_path_buf())
}

fn normalize_ranges(mut raw: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    raw.retain(|(start, end)| start <= end);
    raw.sort_unstable_by_key(|&(start, _)| start);
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(raw.len());
    for (start, end) in raw {
        if let Some(last) = merged.last_mut()
            && start <= last.1.saturating_add(1)
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

struct ShardSpec {
    ranges: Vec<(u32, u32)>,
    profiles: HashMap<(u32, u8), PathBuf>,
}

impl ShardSpec {
    fn contains(&self, cp: u32) -> bool {
        let idx = self.ranges.partition_point(|&(start, _)| start <= cp);
        idx > 0 && cp <= self.ranges[idx - 1].1
    }
}

/// 对应官方 `78/xiaozhi-fonts` 的 `FullGlyphProvider`：按 `manifest.json` 的分片区间懒加载 `.cbin` 并缓存。
struct FullGlyphProvider {
    root: PathBuf,
    bundle: String,
    profiles: HashSet<(u32, u8)>,
    charsets: HashMap<String, BTreeSet<u32>>,
    shards: Vec<ShardSpec>,
    fonts: Mutex<HashMap<(usize, u32, u8), Arc<ParsedCbin>>>,
}

impl FullGlyphProvider {
    fn load(root: &Path) -> Option<Self> {
        let manifest_path = root.join("manifest.json");
        let meta = fs::metadata(&manifest_path).ok()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_MANIFEST_BYTES {
            return None;
        }
        let bytes = fs::read(&manifest_path).ok()?;
        let manifest: Value = serde_json::from_slice(&bytes).ok()?;
        if manifest.get("version").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        let bundle = manifest
            .get("bundle_id")
            .and_then(Value::as_str)?
            .trim()
            .to_owned();
        if bundle.is_empty() {
            return None;
        }

        let mut profile_by_name: HashMap<String, (u32, u8)> = HashMap::new();
        let mut profiles = HashSet::new();
        for item in manifest.get("profiles")?.as_array()? {
            let size = u32::try_from(item.get("size")?.as_u64()?).ok()?;
            let bpp = u8::try_from(item.get("bpp")?.as_u64()?).ok()?;
            if size == 0 || !matches!(bpp, 1 | 4) {
                continue;
            }
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{size}_{bpp}"));
            profile_by_name.insert(name, (size, bpp));
            profiles.insert((size, bpp));
        }
        if profiles.is_empty() {
            return None;
        }

        let mut charsets = HashMap::new();
        for (name, entry) in manifest.get("charsets")?.as_object()? {
            let Some(arr) = entry.get("codepoints").and_then(Value::as_array) else {
                continue;
            };
            let set: BTreeSet<u32> = arr
                .iter()
                .filter_map(|v| v.as_u64().and_then(|cp| u32::try_from(cp).ok()))
                .collect();
            charsets.insert(name.clone(), set);
        }
        if charsets.is_empty() {
            return None;
        }

        let mut shards = Vec::new();
        for item in manifest.get("shards")?.as_array()? {
            let ranges_arr = item.get("ranges")?.as_array()?;
            let mut raw_ranges = Vec::with_capacity(ranges_arr.len());
            for pair in ranges_arr {
                let arr = pair.as_array()?;
                if arr.len() != 2 {
                    return None;
                }
                let start = u32::try_from(arr[0].as_u64()?).ok()?;
                let end = u32::try_from(arr[1].as_u64()?).ok()?;
                raw_ranges.push((start, end));
            }
            let mut shard_profiles = HashMap::new();
            for (prof_name, rel_val) in item.get("profiles")?.as_object()? {
                let Some(&spec) = profile_by_name.get(prof_name) else {
                    continue;
                };
                let rel = safe_relative_cbin(rel_val.as_str()?)?;
                shard_profiles.insert(spec, rel);
            }
            shards.push(ShardSpec {
                ranges: normalize_ranges(raw_ranges),
                profiles: shard_profiles,
            });
        }

        Some(Self {
            root: root.to_path_buf(),
            bundle,
            profiles,
            charsets,
            shards,
            fonts: Mutex::new(HashMap::new()),
        })
    }

    fn supports(&self, cap: &TextFontCapability) -> bool {
        cap.bundle == self.bundle
            && self.charsets.contains_key(&cap.charset)
            && self.profiles.contains(&(cap.size, cap.bpp))
    }

    fn font_for(&self, codepoint: u32, size: u32, bpp: u8) -> Option<Arc<ParsedCbin>> {
        let (shard_idx, rel_path) = self
            .shards
            .iter()
            .enumerate()
            .find(|(_, shard)| shard.contains(codepoint))
            .and_then(|(idx, shard)| shard.profiles.get(&(size, bpp)).map(|p| (idx, p)))?;
        let key = (shard_idx, size, bpp);
        if let Ok(guard) = self.fonts.lock()
            && let Some(cached) = guard.get(&key)
        {
            return Some(cached.clone());
        }

        let full_path = self.root.join(rel_path);
        let meta = fs::metadata(&full_path).ok()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_SHARD_FILE_BYTES {
            return None;
        }
        let bytes = fs::read(&full_path).ok()?;
        let parsed = ParsedCbin::parse_owned(bytes)?;
        if parsed.bpp != bpp {
            return None;
        }
        let arc = Arc::new(parsed);
        if let Ok(mut guard) = self.fonts.lock() {
            let entry = guard.entry(key).or_insert_with(|| arc.clone());
            return Some(entry.clone());
        }
        Some(arc)
    }

    fn payload_for_text(&self, cap: &TextFontCapability, text: &str) -> Option<GlyphPushPayload> {
        if !self.supports(cap) || text.is_empty() {
            return None;
        }
        let device_charset = self.charsets.get(&cap.charset)?;
        let fallback_basic = (cap.charset == "basic")
            .then(|| parsed_font(cap.size, cap.bpp))
            .flatten();
        let mut seen = BTreeSet::new();
        let mut glyphs = Vec::new();
        let mut total_bytes = 0usize;

        for ch in text.chars() {
            let cp = ch as u32;
            if cp < 0x20 || device_charset.contains(&cp) || !seen.insert(cp) {
                continue;
            }
            let extracted = self
                .font_for(cp, cap.size, cap.bpp)
                .and_then(|font| font.glyph(cp))
                .or_else(|| fallback_basic.and_then(|font| font.glyph(cp)));
            let Some((item, byte_count)) = extracted else {
                continue;
            };
            if glyphs.len() >= MAX_GLYPHS
                || total_bytes.saturating_add(byte_count) > MAX_BITMAP_BYTES
            {
                break;
            }
            total_bytes += byte_count;
            glyphs.push(item);
        }

        if glyphs.is_empty() {
            return None;
        }
        Some(GlyphPushPayload {
            v: 1,
            bundle: self.bundle.clone(),
            size: cap.size,
            bpp: cap.bpp,
            glyphs,
        })
    }
}

struct LoadedFullBundle {
    manifest_stamp: (u64, Option<SystemTime>),
    provider: Arc<FullGlyphProvider>,
}

/// 服务端动态补字协调器：优先热加载本地 `noto-v1-full` 全量分片（覆盖 `basic` 与 `common` 设备缺字），
/// 未安装或离线时自动对 `basic` 设备回退内置 `common` 字库。
#[derive(Default)]
pub struct GlyphFontService {
    full_dir: Option<PathBuf>,
    state: Mutex<Option<LoadedFullBundle>>,
}

impl GlyphFontService {
    pub fn new(full_dir: Option<PathBuf>) -> Self {
        Self {
            full_dir,
            state: Mutex::new(None),
        }
    }

    fn full_provider(&self) -> Option<Arc<FullGlyphProvider>> {
        let dir = self.full_dir.as_deref()?;
        let manifest_path = dir.join("manifest.json");
        let Ok(meta) = fs::metadata(&manifest_path) else {
            if let Ok(mut guard) = self.state.lock() {
                *guard = None;
            }
            return None;
        };
        if !meta.is_file() {
            return None;
        }
        let stamp = (meta.len(), meta.modified().ok());
        if let Ok(guard) = self.state.lock()
            && let Some(loaded) = guard.as_ref()
            && loaded.manifest_stamp == stamp
        {
            return Some(loaded.provider.clone());
        }

        let Some(provider) = FullGlyphProvider::load(dir).map(Arc::new) else {
            if let Ok(mut guard) = self.state.lock() {
                *guard = None;
            }
            return None;
        };
        if let Ok(mut guard) = self.state.lock() {
            *guard = Some(LoadedFullBundle {
                manifest_stamp: stamp,
                provider: provider.clone(),
            });
        }
        Some(provider)
    }

    pub fn glyph_push_for_text(
        &self,
        capability: Option<&TextFontCapability>,
        text: &str,
    ) -> Option<GlyphPushPayload> {
        let cap = capability?;
        if cap.bundle != FONT_BUNDLE_ID
            || text.is_empty()
            || !matches!(cap.charset.as_str(), "basic" | "common")
        {
            return None;
        }
        // 快路径：纯 ASCII / basic 已有字无需查磁盘或分片
        if text
            .chars()
            .all(|ch| (ch as u32) < 0x20 || basic_charset().contains(&(ch as u32)))
        {
            return None;
        }
        if let Some(provider) = self.full_provider()
            && provider.supports(cap)
        {
            return provider.payload_for_text(cap, text);
        }
        glyph_push_for_text(Some(cap), text)
    }
}

/// 内置 `common` 兜底补字：无全量分片包时，仅对 `charset == "basic"` 的设备补齐日常汉字。
pub fn glyph_push_for_text(
    capability: Option<&TextFontCapability>,
    text: &str,
) -> Option<GlyphPushPayload> {
    let cap = capability?;
    if cap.bundle != FONT_BUNDLE_ID || cap.charset != "basic" || text.is_empty() {
        return None;
    }
    let font = parsed_font(cap.size, cap.bpp)?;
    let basic = basic_charset();
    let mut seen = BTreeSet::new();
    let mut glyphs = Vec::new();
    let mut total_bytes = 0usize;

    for ch in text.chars() {
        let cp = ch as u32;
        if cp < 0x20 || basic.contains(&cp) || !seen.insert(cp) {
            continue;
        }
        let Some((item, byte_count)) = font.glyph(cp) else {
            continue;
        };
        if glyphs.len() >= MAX_GLYPHS || total_bytes.saturating_add(byte_count) > MAX_BITMAP_BYTES {
            break;
        }
        total_bytes += byte_count;
        glyphs.push(item);
    }

    if glyphs.is_empty() {
        return None;
    }
    Some(GlyphPushPayload {
        v: 1,
        bundle: FONT_BUNDLE_ID.to_owned(),
        size: cap.size,
        bpp: cap.bpp,
        glyphs,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    fn sha256_hex(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn embedded_font_assets_match_upstream_sha256_and_parse_cleanly() {
        assert_eq!(
            sha256_hex(COMMON_14_1),
            "ccdfa4a7de73f584f0d7519be2d2785c4e9880446edf66c1635e676969c030ee"
        );
        assert_eq!(
            sha256_hex(COMMON_16_4),
            "6c801b34ec686e6e31223eceedea2efe5b0cf294b3556d91087a683c86a54384"
        );
        assert_eq!(
            sha256_hex(COMMON_20_4),
            "82f0d5e97e8423bd6d6ecd4d53e0aed742704565b1dcc21124f90658d7d4c40e"
        );
        assert_eq!(
            sha256_hex(COMMON_30_4),
            "b9690d5babd46d650aa06bbcae0acd36194fded289ce150453de8a421af9793d"
        );
        assert_eq!(
            sha256_hex(BASIC_CHARSET_JSON),
            "843573ff261fecbcd6b7091168bdcfde6d85a9376880d3d3d0e452232c650677"
        );
        assert_eq!(basic_charset().len(), 890);
        for profile in PROFILES {
            assert!(
                parsed_font(profile.size, profile.bpp).is_some(),
                "CBIN 解析失败: {}",
                profile.file_name
            );
        }
    }

    #[test]
    fn glyph_push_extracts_missing_chinese_for_basic_device_and_skips_common() {
        let basic_cap = TextFontCapability {
            bundle: "noto-v1".into(),
            charset: "basic".into(),
            size: 20,
            bpp: 4,
        };
        let text = "激光雕刻机准备就绪，椴木厚度3毫米，马上开始雕刻！";
        let payload = glyph_push_for_text(Some(&basic_cap), text).expect("应生成缺失汉字字模");
        assert_eq!(payload.v, 1);
        assert_eq!(payload.bundle, "noto-v1");
        assert_eq!(payload.size, 20);
        assert_eq!(payload.bpp, 4);
        assert!(!payload.glyphs.is_empty());

        // ASCII '3' 与固件界面词“设备”中的 '备' 已在 basic 字符集中，不应重复推送；“雕”“刻”“准”“绪”不在 basic 中，必须推送
        let pushed: BTreeSet<u32> = payload.glyphs.iter().map(|g| g.codepoint).collect();
        assert!(!pushed.contains(&('3' as u32)));
        assert!(!pushed.contains(&('备' as u32)));
        assert!(pushed.contains(&('雕' as u32)));
        assert!(pushed.contains(&('刻' as u32)));
        assert!(pushed.contains(&('准' as u32)));
        assert!(pushed.contains(&('绪' as u32)));

        for item in &payload.glyphs {
            assert!(item.box_w <= 64 && item.box_h <= 64);
            let expected_bytes =
                (usize::from(item.box_w) * usize::from(item.box_h) * 4).div_ceil(8);
            // Base64 长度核验
            assert_eq!(item.bitmap.len(), expected_bytes.div_ceil(3) * 4);
        }

        // 已装好 common 字库的设备不推冗余字模
        let common_cap = TextFontCapability {
            charset: "common".into(),
            ..basic_cap.clone()
        };
        assert!(glyph_push_for_text(Some(&common_cap), text).is_none());

        // 不兼容 bundle 不推
        let other_bundle = TextFontCapability {
            bundle: "custom-v2".into(),
            ..basic_cap
        };
        assert!(glyph_push_for_text(Some(&other_bundle), text).is_none());
    }

    pub(crate) struct SyntheticGlyph<'a> {
        pub codepoint: u32,
        pub adv_w: u32,
        pub box_w: u16,
        pub box_h: u16,
        pub ofs_x: i16,
        pub ofs_y: i16,
        pub bitmap: &'a [u8],
    }

    pub(crate) fn build_synthetic_cbin(bpp: u8, glyphs: &[SyntheticGlyph<'_>]) -> Vec<u8> {
        let dsc = 28usize;
        let dsc_len = 24usize;
        let cmap_offset = dsc_len;
        let cmap_len = glyphs.len() * 20;
        let glyph_dsc_offset = cmap_offset + cmap_len;
        let glyph_dsc_len = (glyphs.len() + 1) * 16;
        let bitmap_offset = glyph_dsc_offset + glyph_dsc_len;
        let total_bitmap: usize = glyphs.iter().map(|g| g.bitmap.len()).sum();

        let mut buf = vec![0u8; dsc + bitmap_offset + total_bitmap];
        buf[12..16].copy_from_slice(&24i32.to_le_bytes());
        buf[16..20].copy_from_slice(&3i32.to_le_bytes());
        buf[24..28].copy_from_slice(&(dsc as u32).to_le_bytes());

        buf[dsc..dsc + 4].copy_from_slice(&(bitmap_offset as u32).to_le_bytes());
        buf[dsc + 4..dsc + 8].copy_from_slice(&(glyph_dsc_offset as u32).to_le_bytes());
        buf[dsc + 8..dsc + 12].copy_from_slice(&(cmap_offset as u32).to_le_bytes());
        let bitfield = (glyphs.len() as u16 & 0x1FF) | ((u16::from(bpp)) << 9);
        buf[dsc + 18..dsc + 20].copy_from_slice(&bitfield.to_le_bytes());
        buf[dsc + 20] = 0;

        let cmap_abs = dsc + cmap_offset;
        let glyph_dsc_abs = dsc + glyph_dsc_offset;
        let bitmap_abs = dsc + bitmap_offset;
        let mut bmp_cursor = 0usize;

        for (i, g) in glyphs.iter().enumerate() {
            let glyph_id = (i + 1) as u16;
            let cptr = cmap_abs + i * 20;
            buf[cptr..cptr + 4].copy_from_slice(&g.codepoint.to_le_bytes());
            buf[cptr + 4..cptr + 6].copy_from_slice(&1u16.to_le_bytes());
            buf[cptr + 6..cptr + 8].copy_from_slice(&glyph_id.to_le_bytes());
            buf[cptr + 18] = CMAP_FORMAT0_TINY;

            let gptr = glyph_dsc_abs + usize::from(glyph_id) * 16;
            buf[gptr..gptr + 4].copy_from_slice(&(bmp_cursor as u32).to_le_bytes());
            buf[gptr + 4..gptr + 8].copy_from_slice(&g.adv_w.to_le_bytes());
            buf[gptr + 8..gptr + 10].copy_from_slice(&g.box_w.to_le_bytes());
            buf[gptr + 10..gptr + 12].copy_from_slice(&g.box_h.to_le_bytes());
            buf[gptr + 12..gptr + 14].copy_from_slice(&g.ofs_x.to_le_bytes());
            buf[gptr + 14..gptr + 16].copy_from_slice(&g.ofs_y.to_le_bytes());

            buf[bitmap_abs + bmp_cursor..bitmap_abs + bmp_cursor + g.bitmap.len()]
                .copy_from_slice(g.bitmap);
            bmp_cursor += g.bitmap.len();
        }
        buf
    }

    /// 在测试临时目录生成一套与官方 `noto-v1-full` 同结构的迷你全量分片包（含 `cjk_sc` 与 `cjk_jp` 分片），
    /// 覆盖 `common` 缺字代表字：“椴”（U+6934）、“𠮷”（U+20BB7）与日文汉字“桜”（U+685C）。
    pub(crate) fn write_synthetic_full_bundle(dir: &Path) {
        fs::create_dir_all(dir.join("20_4")).unwrap();
        let duan_bmp = vec![0xA5u8; (20 * 20 * 4usize).div_ceil(8)];
        let yoshi_bmp = vec![0x5Au8; (19 * 19 * 4usize).div_ceil(8)];
        let sakura_bmp = vec![0x3Cu8; (18 * 18 * 4usize).div_ceil(8)];

        let cjk_sc = build_synthetic_cbin(
            4,
            &[
                SyntheticGlyph {
                    codepoint: '椴' as u32,
                    adv_w: 320,
                    box_w: 20,
                    box_h: 20,
                    ofs_x: 0,
                    ofs_y: -3,
                    bitmap: &duan_bmp,
                },
                SyntheticGlyph {
                    codepoint: '𠮷' as u32,
                    adv_w: 320,
                    box_w: 19,
                    box_h: 19,
                    ofs_x: 0,
                    ofs_y: -2,
                    bitmap: &yoshi_bmp,
                },
            ],
        );
        let cjk_jp = build_synthetic_cbin(
            4,
            &[SyntheticGlyph {
                codepoint: '桜' as u32,
                adv_w: 320,
                box_w: 18,
                box_h: 18,
                ofs_x: 1,
                ofs_y: -1,
                bitmap: &sakura_bmp,
            }],
        );
        fs::write(dir.join("20_4/cjk_sc.cbin"), cjk_sc).unwrap();
        fs::write(dir.join("20_4/cjk_jp.cbin"), cjk_jp).unwrap();

        let basic_cps: Vec<u32> = basic_charset().iter().copied().collect();
        let mut common_cps = basic_cps.clone();
        for ch in [
            '激', '光', '雕', '刻', '木', '野', '家', '好', '开', '始', '吧',
        ] {
            common_cps.push(ch as u32);
        }
        common_cps.sort_unstable();
        common_cps.dedup();

        let manifest = json!({
            "version": 1,
            "bundle_id": "noto-v1",
            "coverage_count": 3,
            "profiles": [
                { "name": "20_4", "size": 20, "bpp": 4 }
            ],
            "charsets": {
                "basic": { "charset": "basic", "count": basic_cps.len(), "codepoints": basic_cps },
                "common": { "charset": "common", "count": common_cps.len(), "codepoints": common_cps }
            },
            "shards": [
                {
                    "id": "cjk_sc",
                    "count": 2,
                    "ranges": [[0x6934u32, 0x6934u32], [0x20BB7u32, 0x20BB7u32]],
                    "profiles": { "20_4": "20_4/cjk_sc.cbin" }
                },
                {
                    "id": "cjk_jp",
                    "count": 1,
                    "ranges": [[0x685Cu32, 0x685Cu32]],
                    "profiles": { "20_4": "20_4/cjk_jp.cbin" }
                }
            ]
        });
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn full_bundle_hot_loads_for_common_and_basic_devices_and_falls_back_on_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let full_dir = tmp.path().join("noto_v1_full");
        let service = GlyphFontService::new(Some(full_dir.clone()));

        let common_cap = TextFontCapability {
            bundle: "noto-v1".into(),
            charset: "common".into(),
            size: 20,
            bpp: 4,
        };
        let basic_cap = TextFontCapability {
            charset: "basic".into(),
            ..common_cap.clone()
        };
        let text = "𠮷野家桜激光雕刻椴木";

        // 1. 全量包未下载时：common 设备返回 None，basic 设备回退内置 common 字库补出日常汉字
        assert!(
            service
                .glyph_push_for_text(Some(&common_cap), text)
                .is_none()
        );
        let fallback = service
            .glyph_push_for_text(Some(&basic_cap), text)
            .expect("basic 设备应回退内置 common");
        let fallback_cps: BTreeSet<u32> = fallback.glyphs.iter().map(|g| g.codepoint).collect();
        assert!(fallback_cps.contains(&('雕' as u32)));
        assert!(!fallback_cps.contains(&('椴' as u32)));

        // 2. 全量包下载就位后热生效（无需重建 service）：common 设备只抠缺失的“𠮷”“桜”“椴”，不重复推“激光雕刻木”
        write_synthetic_full_bundle(&full_dir);
        let pushed_common = service
            .glyph_push_for_text(Some(&common_cap), text)
            .expect("common 设备遇到生僻字与日文应动态补齐");
        let common_cps: Vec<u32> = pushed_common.glyphs.iter().map(|g| g.codepoint).collect();
        assert_eq!(
            common_cps,
            vec!['𠮷' as u32, '桜' as u32, '椴' as u32],
            "只按出现顺序抠出 common 字表之外的缺字"
        );
        assert_eq!(pushed_common.glyphs[0].box_w, 19);
        assert_eq!(pushed_common.glyphs[2].box_w, 20);

        // basic 设备同时获得全量包中的“𠮷/桜/椴”与内置 common 兜底的日常汉字
        let pushed_basic = service
            .glyph_push_for_text(Some(&basic_cap), text)
            .expect("basic 设备同时补齐生僻字与日常字");
        let basic_pushed_cps: BTreeSet<u32> =
            pushed_basic.glyphs.iter().map(|g| g.codepoint).collect();
        assert!(basic_pushed_cps.contains(&('𠮷' as u32)));
        assert!(basic_pushed_cps.contains(&('桜' as u32)));
        assert!(basic_pushed_cps.contains(&('椴' as u32)));
        assert!(basic_pushed_cps.contains(&('雕' as u32)));

        // 3. 用户删除全量包后：缓存释放并平滑回退，不崩溃
        fs::remove_dir_all(&full_dir).unwrap();
        assert!(
            service
                .glyph_push_for_text(Some(&common_cap), text)
                .is_none()
        );
        assert!(
            service
                .glyph_push_for_text(Some(&basic_cap), text)
                .is_some()
        );
    }

    #[test]
    fn full_bundle_rejects_path_traversal_in_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let full_dir = tmp.path().join("noto_v1_full");
        write_synthetic_full_bundle(&full_dir);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(full_dir.join("manifest.json")).unwrap()).unwrap();
        manifest["shards"][0]["profiles"]["20_4"] = json!("../escape.cbin");
        fs::write(
            full_dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(FullGlyphProvider::load(&full_dir).is_none());
    }

    fn canonical_json_bytes(value: &Value) -> Vec<u8> {
        fn sort_value(v: &Value) -> Value {
            match v {
                Value::Object(map) => {
                    let mut keys: Vec<_> = map.keys().collect();
                    keys.sort();
                    let mut out = serde_json::Map::new();
                    for k in keys {
                        out.insert(k.clone(), sort_value(&map[k]));
                    }
                    Value::Object(out)
                }
                Value::Array(arr) => Value::Array(arr.iter().map(sort_value).collect()),
                other => other.clone(),
            }
        }
        serde_json::to_vec(&sort_value(value)).unwrap()
    }

    /// 与上游 `78/xiaozhi-fonts/scripts/glyph_provider.py` 对真实 `noto-v1-full` 7 大分片 × 4 档字号的逐字节核对：
    /// `MOSS_TEST_NOTO_FULL_DIR=<path> cargo test -p moss-voice-server real_noto_v1_full -- --ignored`
    #[test]
    #[ignore]
    fn real_noto_v1_full_matches_upstream_glyph_provider_across_all_7_shards() {
        let dir = PathBuf::from(
            std::env::var("MOSS_TEST_NOTO_FULL_DIR").expect("请设置 MOSS_TEST_NOTO_FULL_DIR"),
        );
        let service = GlyphFontService::new(Some(dir));
        let text =
            "\u{0108}\u{20DD}\u{F907}\u{F904}\u{1100}\u{0E03}\u{0600}\u{20BB7}\u{6934}\u{96D5}";
        let expected = [
            (
                14u32,
                1u8,
                "a70352ea2548503f87425daeaa97e8dd3cadbcdb8b258120c1db26d40eae41fd",
            ),
            (
                16u32,
                4u8,
                "9bf7a4dbf9a77809bfe426936e8e37331dc7b2dab2fa40ff6deddc0b04894078",
            ),
            (
                20u32,
                4u8,
                "7fff800226ad25648d62d7e859f9d7b9ecf148a90028850b2f9d24069114a34b",
            ),
            (
                30u32,
                4u8,
                "1d274d4bf0fa3c831cc3c00908067ed1866eaaa1163cb0396cbc2fbfc62be56e",
            ),
        ];
        for (size, bpp, expected_sha) in expected {
            let cap = TextFontCapability {
                bundle: "noto-v1".into(),
                charset: "common".into(),
                size,
                bpp,
            };
            let payload = service.glyph_push_for_text(Some(&cap), text).unwrap();
            assert_eq!(payload.glyphs.len(), 9);
            let val = serde_json::to_value(&payload).unwrap();
            assert_eq!(
                sha256_hex(&canonical_json_bytes(&val)),
                expected_sha,
                "{size}_{bpp}"
            );
        }
    }
}
