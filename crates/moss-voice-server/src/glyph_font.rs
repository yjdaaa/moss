//! 小智设备屏幕中文字库与 `glyph_push` v1 动态字模补齐（技术书 n81，2026-11-12）。
//!
//! 核实自 `78/xiaozhi-esp32`（`docs/glyph-push_zh.md`、`main/assets.cc`）与 `78/xiaozhi-fonts`
//! （`scripts/glyph_provider.py`，提交 `d45dbc64052d57048f20ab1770074172ce9eb53b`）：
//! 1. 小智 2.5+ 固件链接在程序分区里的 `BUILTIN_TEXT_FONT` 为 `basic` 字库（仅含 ASCII、Latin-1 与固件静态界面词条共 890 字），
//!    几千字日常汉字放在 `assets` 分区的 `font_noto_sans_common_<size>_<bpp>.bin` 中；
//! 2. 没有导入外部 `base-assets.bin` 时，MOSS 生成的唤醒词 / 全屏画风资源包自动内置匹配规格的 `common` CBIN 字库与
//!    `text_font_meta`，避免覆盖设备资源分区后丢失中文字库；
//! 3. 对已经刷了旧无字库资源包（握手报告 `features.glyph_push = true` 且 `text_font.charset = "basic"`）的在线设备，
//!    MOSS 直接从同源 `noto-v1` CBIN 字库提取 `basic` 缺失的汉字位图，随 `stt` 与 `tts(sentence_start)` 下发，
//!    无需重启或重刷资源包即可完整显示对话字幕。

use std::collections::BTreeSet;
use std::sync::OnceLock;

use moss_xiaozhi_proto::{GlyphItem, GlyphPushPayload, TextFontCapability};
use serde_json::{Value, json};

use crate::goggle_bot::base64_encode;
use crate::wake_assets::AssetEntry;

pub const FONT_BUNDLE_ID: &str = "noto-v1";
const MAX_GLYPHS: usize = 64;
const MAX_BITMAP_BYTES: usize = 64 * 1024;

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

struct CmapEntry {
    start: u32,
    length: u16,
    glyph_start: u16,
    unicode_abs: usize,
    glyph_offset_abs: usize,
    count: u16,
    kind: u8,
}

struct ParsedCbin {
    data: &'static [u8],
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
    fn parse(data: &'static [u8]) -> Option<Self> {
        let dsc = u32_le(data, 24)? as usize;
        let bitmap_offset = u32_le(data, dsc)? as usize;
        let glyph_dsc_offset = u32_le(data, dsc.checked_add(4)?)? as usize;
        let cmap_offset = u32_le(data, dsc.checked_add(8)?)? as usize;
        let bitfield = u16_le(data, dsc.checked_add(18)?)?;
        let stride = *data.get(dsc.checked_add(20)?)?;

        let bitmap_abs = dsc.checked_add(bitmap_offset)?;
        let glyph_dsc_abs = dsc.checked_add(glyph_dsc_offset)?;
        let cmap_abs = dsc.checked_add(cmap_offset)?;
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
            data,
            bitmap_abs,
            glyph_dsc_abs,
            bpp,
            cmaps,
        })
    }

    fn sparse_index(&self, cmap: &CmapEntry, relative: u16) -> Option<usize> {
        let count = usize::from(cmap.count);
        if count == 0 || cmap.unicode_abs == 0 {
            return None;
        }
        let mut lo = 0usize;
        let mut hi = count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let val = u16_le(self.data, cmap.unicode_abs.checked_add(mid * 2)?)?;
            match val.cmp(&relative) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    fn glyph_id(&self, codepoint: u32) -> Option<usize> {
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
                            *self
                                .data
                                .get(cmap.glyph_offset_abs.checked_add(relative as usize)?)?,
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
                                self.data,
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
        let glyph_id = self.glyph_id(codepoint)?;
        let offset = self.glyph_dsc_abs.checked_add(glyph_id.checked_mul(16)?)?;
        let bitmap_index = u32_le(self.data, offset)? as usize;
        let adv_w = u32_le(self.data, offset.checked_add(4)?)?;
        let box_w = u16_le(self.data, offset.checked_add(8)?)?;
        let box_h = u16_le(self.data, offset.checked_add(10)?)?;
        let ofs_x = i16_le(self.data, offset.checked_add(12)?)?;
        let ofs_y = i16_le(self.data, offset.checked_add(14)?)?;
        if box_w > 64 || box_h > 64 {
            return None;
        }
        let byte_count =
            (usize::from(box_w) * usize::from(box_h) * usize::from(self.bpp)).div_ceil(8);
        let bmp_start = self.bitmap_abs.checked_add(bitmap_index)?;
        let bmp_end = bmp_start.checked_add(byte_count)?;
        let bitmap = self.data.get(bmp_start..bmp_end)?;
        Some((
            GlyphItem {
                codepoint,
                adv_w,
                box_w,
                box_h,
                ofs_x,
                ofs_y,
                bitmap: base64_encode(bitmap),
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
            .get_or_init(|| ParsedCbin::parse(COMMON_14_1))
            .as_ref(),
        (16, 4) => F16_4
            .get_or_init(|| ParsedCbin::parse(COMMON_16_4))
            .as_ref(),
        (20, 4) => F20_4
            .get_or_init(|| ParsedCbin::parse(COMMON_20_4))
            .as_ref(),
        (30, 4) => F30_4
            .get_or_init(|| ParsedCbin::parse(COMMON_30_4))
            .as_ref(),
        _ => None,
    }
}

/// 根据设备在 `hello` 中声明的字库能力，为待显示文本生成缺失字符的 `glyph_push` 负载。
///
/// 仅当 `bundle == "noto-v1"` 且 `charset == "basic"`、规格受支持且存在缺失的可渲染字符时返回 `Some`；
/// 已装好 `common` 字库或未开启 `glyph_push` 的设备恒返回 `None`。
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
mod tests {
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
}
