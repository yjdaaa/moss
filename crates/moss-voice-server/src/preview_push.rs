//! 小智彩屏雕刻预览与实时加工副屏 HUD 渲染服务（适配 `self.screen.preview_image` 与 `LODEPNG`）。
//!
//! 针对立创·实战派 ESP32-S3（`320×240` IPS ST7789）及全系 LVGL 彩屏的像素级兼容约定
//! （核实自 `78/xiaozhi-esp32/main/display/lcd_display.cc:932-934, 1103-1106`）：
//! 1. 官方固件经典布局中 `preview_image_` 控件尺寸固定为 `(width_ / 2, height_ / 2)`，且按
//!    `scale = 128 * width_ / img_w`（LVGL 256 = 1.0x）缩放；因此输出精确等于 `(width_ / 2, height_ / 2)`
//!    （立创实战派 `320×240` 对应 `160×120`）时，固件缩放因子恰为 `256`（1:1 原生点对点零插值、零上下裁切）！
//! 2. 激光预览原图多为白底或透明底的 1px 细线，直接缩小会断线发虚；本模块自动识别底色、提取前景刀路并做
//!    3×3 形态学加粗与发光描边，叠加曜石黑底板、MOSS 四角瞄准框与底部 5×7 像素标尺（尺寸/功率/速度）。
//! 3. 加工发送期间可生成超轻量（约 2~5 KB）的 MOSS 太空舱实时进度 HUD PNG，携带精确 `Content-Length`。

use std::collections::{BTreeMap, VecDeque};
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock};

use crate::goggle_bot::{
    BotPose, ScreenCardStyle, base64_encode, draw_cute_frame, draw_goggle_bot,
};
use crate::{Shared, unix_now};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use image::codecs::gif::{GifEncoder, Repeat};
use image::imageops::FilterType;
use image::{Delay, Frame, ImageFormat, ImageReader, Limits, Rgb, RgbImage, Rgba, RgbaImage};
use serde::Serialize;
pub const MAX_SOURCE_IMAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_THUMBNAIL_PNG_BYTES: usize = 128 * 1024;
/// 立创·实战派 ESP32-S3（320×240）对应的默认 1:1 预览框像素尺寸（160×120）。
pub const DEFAULT_CANVAS_W: u32 = 160;
pub const DEFAULT_CANVAS_H: u32 = 120;
const MAX_PREVIEW_ENTRIES: usize = 12;
const PREVIEW_TTL_SECS: u64 = 300;

/// 根据 `self.screen.get_info` 上报的屏幕宽高推导 1:1 点对点预览画布尺寸 `(width / 2, height / 2)`。
pub fn optimal_canvas_for_screen(screen_size: Option<(u32, u32)>) -> (u32, u32) {
    match screen_size {
        Some((w, h)) if w >= 120 && h >= 120 => ((w / 2).clamp(80, 240), (h / 2).clamp(80, 240)),
        _ => (DEFAULT_CANVAS_W, DEFAULT_CANVAS_H),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobHudSpec {
    pub percent: u8,
    pub lines_sent: u64,
    pub total_lines: u64,
    pub power: Option<i64>,
    pub feed_rate: Option<i64>,
    pub eta_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalCardSpec {
    pub kind_ascii: String,
    pub remaining_secs: u64,
    pub total_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalBadgeSpec {
    pub completed: bool,
    pub lines: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MusicCardSpec {
    pub title_ascii: String,
}
#[derive(Clone)]
struct PreviewItem {
    token: String,
    png_bytes: Arc<Vec<u8>>,
    expires_at: u64,
}

#[derive(Default)]
pub struct PreviewStore {
    items: Mutex<VecDeque<PreviewItem>>,
}

fn random_token_hex() -> Result<String, String> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|_| "无法生成预览图随机令牌".to_owned())?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn token_ok(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 极简内置 5×7 ASCII 位图字模（0-9, A-Z, %, ., x, /, :, -, 空格），零外部字体依赖。
fn glyph_5x7(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b00000,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '%' => [
            0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011,
        ],
        '!' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
        '?' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b00100, 0b00000, 0b00100,
        ],
        '+' => [
            0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        ':' => [
            0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000,
        ],
        '/' => [
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        _ => [0; 7],
    }
}

fn put_px(img: &mut RgbImage, x: i32, y: i32, color: Rgb<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, color);
    }
}

fn draw_text_5x7(img: &mut RgbImage, x0: i32, y0: i32, scale: i32, text: &str, color: Rgb<u8>) {
    let mut cx = x0;
    let step = 6 * scale;
    for ch in text.chars() {
        if cx + 5 * scale > img.width() as i32 {
            break;
        }
        let rows = glyph_5x7(ch);
        for (ry, bits) in rows.iter().enumerate() {
            for rx in 0..5 {
                if (bits >> (4 - rx)) & 1 == 1 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            put_px(
                                img,
                                cx + rx * scale + sx,
                                y0 + (ry as i32) * scale + sy,
                                color,
                            );
                        }
                    }
                }
            }
        }
        cx += step;
    }
}

fn text_width_px(text: &str, scale: i32) -> i32 {
    let count = text.chars().count() as i32;
    if count <= 0 {
        0
    } else {
        count * 6 * scale - scale
    }
}

fn draw_moss_frame(img: &mut RgbImage, accent: Rgb<u8>) {
    let w = img.width() as i32;
    let h = img.height() as i32;
    let border = Rgb([28, 38, 56]);
    for x in 0..w {
        put_px(img, x, 0, border);
        put_px(img, x, h - 1, border);
    }
    for y in 0..h {
        put_px(img, 0, y, border);
        put_px(img, w - 1, y, border);
    }
    let tick = 6;
    for i in 0..tick {
        put_px(img, i, 0, accent);
        put_px(img, 0, i, accent);
        put_px(img, w - 1 - i, 0, accent);
        put_px(img, w - 1, i, accent);
        put_px(img, i, h - 1, accent);
        put_px(img, 0, h - 1 - i, accent);
        put_px(img, w - 1 - i, h - 1, accent);
        put_px(img, w - 1, h - 1 - i, accent);
    }
}

fn encode_png_checked(rgb: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(6144);
    rgb.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|_| "编码屏幕预览 PNG 失败".to_owned())?;
    if out.len() > MAX_THUMBNAIL_PNG_BYTES {
        return Err("生成的屏幕预览缩略图过大，已跳过推送".into());
    }
    Ok(out)
}

/// 将任意激光预览图渲染为 1:1 匹配小智屏幕的刀路预览卡（支持 `MossHud` 与 `GoggleBot` 双画风）。
pub fn make_device_thumbnail_png_for_canvas(
    source_bytes: &[u8],
    canvas_w: u32,
    canvas_h: u32,
    ruler_ascii: Option<&str>,
) -> Result<Vec<u8>, String> {
    make_device_thumbnail_png_for_style(
        source_bytes,
        canvas_w,
        canvas_h,
        ruler_ascii,
        ScreenCardStyle::MossHud,
    )
}

pub fn make_device_thumbnail_png_for_style(
    source_bytes: &[u8],
    canvas_w: u32,
    canvas_h: u32,
    ruler_ascii: Option<&str>,
    style: ScreenCardStyle,
) -> Result<Vec<u8>, String> {
    if source_bytes.is_empty() {
        return Err("预览图文件为空".into());
    }
    if source_bytes.len() > MAX_SOURCE_IMAGE_BYTES {
        return Err("原预览图超过 16 MB 上限".into());
    }
    let cw = canvas_w.clamp(80, 240);
    let ch = canvas_h.clamp(80, 240);

    let mut reader = ImageReader::new(Cursor::new(source_bytes))
        .with_guessed_format()
        .map_err(|_| "无法识别预览图格式".to_owned())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);

    let decoded = reader
        .decode()
        .map_err(|_| "解码预览图失败（仅支持 PNG / JPEG）".to_owned())?;
    let src_rgba = decoded.to_rgba8();
    let (sw, sh) = src_rgba.dimensions();
    if sw == 0 || sh == 0 {
        return Err("预览图尺寸无效".into());
    }

    let ruler = ruler_ascii.map(str::trim).filter(|s| !s.is_empty());
    let bottom_band: u32 = if ruler.is_some() { 13 } else { 4 };
    let pad: u32 = 5;
    let avail_w = cw.saturating_sub(pad * 2).max(16);
    let avail_h = ch.saturating_sub(pad + bottom_band + 2).max(16);

    let scale = (avail_w as f64 / sw as f64).min(avail_h as f64 / sh as f64);
    let tw = ((sw as f64 * scale).round() as u32).clamp(1, avail_w);
    let th = ((sh as f64 * scale).round() as u32).clamp(1, avail_h);

    // 用 Triangle 滤波保留抗锯齿能量，避免 1px 细刀路在最近邻抽样中直接丢失
    let resized = image::imageops::resize(&src_rgba, tw, th, FilterType::Triangle);

    // 采样四角判断原图是否为浅色底（例如白底黑/蓝刀路图）
    let corners = [
        resized.get_pixel(0, 0),
        resized.get_pixel(tw - 1, 0),
        resized.get_pixel(0, th - 1),
        resized.get_pixel(tw - 1, th - 1),
    ];
    let avg_corner_alpha: u32 = corners.iter().map(|p| p[3] as u32).sum::<u32>() / 4;
    let avg_corner_luma: u32 = corners
        .iter()
        .map(|p| (p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8)
        .sum::<u32>()
        / 4;
    let light_opaque_bg = avg_corner_alpha > 200 && avg_corner_luma > 185;

    // 按缩小倍率补偿 1px 极细矢量线在插值缩小后的能量衰减（例如 800→136 缩小 6 倍时 1px 线原对比度会被稀释到 1/6）
    let gain_q4 = ((sw.max(sh) * 6) / tw.max(th).max(1)).clamp(4, 36);

    // 计算每个像素的前景刀路强度 (0..=255)
    let mut intensity = vec![0u8; (tw * th) as usize];
    for y in 0..th {
        for x in 0..tw {
            let p = resized.get_pixel(x, y);
            let a = p[3] as u32;
            let luma = (p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8;
            let raw_val = if a < 12 {
                0
            } else if light_opaque_bg {
                // 浅色底：越暗或饱和度越高，代表越强的刀路轨迹
                let max_c = p[0].max(p[1]).max(p[2]) as u32;
                let min_c = p[0].min(p[1]).min(p[2]) as u32;
                let sat = max_c.saturating_sub(min_c);
                let darkness = 255u32.saturating_sub(luma);
                (darkness.max(sat) * a) / 255
            } else {
                // 透明底或深色底：按不透明度与亮度提取刀路
                let boost = luma.max(150);
                (boost * a) / 255
            };
            let boosted = if raw_val >= 8 {
                ((raw_val * gain_q4) / 4).min(255) as u8
            } else {
                0
            };
            intensity[(y * tw + x) as usize] = if boosted >= 24 { boosted } else { 0 };
        }
    }

    // 1px 形态学膨胀发光层（让 1px 极细刀路在 2.0 寸屏上清晰不断线）
    let mut dilated = vec![0u8; (tw * th) as usize];
    for y in 0..th {
        for x in 0..tw {
            let mut max_n = 0u8;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0 && ny >= 0 && (nx as u32) < tw && (ny as u32) < th {
                        let v = intensity[(ny as u32 * tw + nx as u32) as usize];
                        let weighted = if dx == 0 && dy == 0 {
                            v
                        } else if dx == 0 || dy == 0 {
                            (v as u16 * 3 / 4) as u8
                        } else {
                            (v as u16 / 2) as u8
                        };
                        if weighted > max_n {
                            max_n = weighted;
                        }
                    }
                }
            }
            dilated[(y * tw + x) as usize] = max_n;
        }
    }

    let bg = if style == ScreenCardStyle::GoggleBot {
        Rgb([13, 17, 26])
    } else {
        Rgb([11, 14, 20])
    };
    let mut canvas = RgbImage::from_pixel(cw, ch, bg);
    if style == ScreenCardStyle::GoggleBot {
        draw_cute_frame(&mut canvas, Rgb([72, 250, 210]), Rgb([255, 118, 98]));
    } else {
        draw_moss_frame(&mut canvas, Rgb([0, 220, 255]));
    }
    let ox = ((cw - tw) / 2) as i32;
    let oy = ((ch.saturating_sub(bottom_band) - th) / 2).max(2) as i32;

    // 画出工件内包络微弱虚线参考框
    let guide = Rgb([26, 38, 54]);
    for gx in (0..tw as i32).step_by(3) {
        put_px(&mut canvas, ox + gx, oy, guide);
        put_px(&mut canvas, ox + gx, oy + th as i32 - 1, guide);
    }
    for gy in (0..th as i32).step_by(3) {
        put_px(&mut canvas, ox, oy + gy, guide);
        put_px(&mut canvas, ox + tw as i32 - 1, oy + gy, guide);
    }

    for y in 0..th {
        for x in 0..tw {
            let core = intensity[(y * tw + x) as usize] as u16;
            let glow = dilated[(y * tw + x) as usize] as u16;
            if glow == 0 {
                continue;
            }
            // 核心刀路呈高对比度激光青翠色 (#3CF8C4)，边缘呈深青辉光 (#106858)
            let t = if core >= 80 {
                (core.min(255) as u16 + 100).min(255)
            } else {
                (glow * 185 / 255).max(48)
            };
            let inv = 255 - t;
            let fg = if core >= 80 {
                [64u16, 250, 198]
            } else {
                [18u16, 168, 138]
            };
            let r = ((fg[0] * t + 11 * inv) / 255) as u8;
            let g = ((fg[1] * t + 14 * inv) / 255) as u8;
            let b = ((fg[2] * t + 20 * inv) / 255) as u8;
            put_px(&mut canvas, ox + x as i32, oy + y as i32, Rgb([r, g, b]));
        }
    }
    if style == ScreenCardStyle::GoggleBot && cw >= 136 && ch >= 96 {
        // 左上角探头看图纸的迷你护目镜小机器人徽章 (26×26)
        draw_goggle_bot(&mut canvas, 3, 2, 1, BotPose::IdleBlink, 0);
    }

    if let Some(label) = ruler {
        let max_chars = ((cw.saturating_sub(8)) / 6) as usize;
        let clipped: String = label.chars().take(max_chars).collect();
        let tw_px = text_width_px(&clipped, 1);
        let tx = ((cw as i32 - tw_px) / 2).max(4);
        let ty = ch as i32 - 10;
        draw_text_5x7(&mut canvas, tx, ty, 1, &clipped, Rgb([156, 178, 206]));
    }

    encode_png_checked(&canvas)
}

/// 兼容默认画布尺寸的缩略图生成入口。
pub fn make_device_thumbnail_png(source_bytes: &[u8]) -> Result<Vec<u8>, String> {
    make_device_thumbnail_png_for_canvas(source_bytes, DEFAULT_CANVAS_W, DEFAULT_CANVAS_H, None)
}

fn draw_moss_optical_eye(
    img: &mut RgbImage,
    cx: i32,
    cy: i32,
    r: i32,
    core: Rgb<u8>,
    ring: Rgb<u8>,
) {
    let r2_outer = r * r;
    let r2_outer_in = (r - 2).max(1).pow(2);
    let r2_mid = (r * 2 / 3).max(2).pow(2);
    let r2_core = (r / 3).max(2).pow(2);
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 <= r2_core {
                put_px(img, cx + dx, cy + dy, core);
            } else if d2 <= r2_mid && d2 >= (r * 2 / 3 - 1).max(1).pow(2) {
                put_px(img, cx + dx, cy + dy, ring);
            } else if d2 <= r2_outer && d2 >= r2_outer_in {
                put_px(img, cx + dx, cy + dy, Rgb([52, 70, 98]));
            }
        }
    }
    put_px(img, cx - r / 5, cy - r / 5, Rgb([255, 240, 240]));
}

fn draw_segmented_bar(
    img: &mut RgbImage,
    x0: i32,
    y0: i32,
    width: i32,
    height: i32,
    segments: i32,
    percent: u8,
    fg: Rgb<u8>,
) {
    let segs = segments.max(4);
    let filled = ((percent.min(100) as i32 * segs + 50) / 100).clamp(0, segs);
    let step = (width / segs).max(3);
    let seg_w = (step - 2).max(2);
    for s in 0..segs {
        let sx = x0 + s * step;
        let color = if s < filled { fg } else { Rgb([28, 36, 48]) };
        for yy in y0..(y0 + height) {
            for xx in sx..(sx + seg_w) {
                put_px(img, xx, yy, color);
            }
        }
    }
}

/// 生成激光加工实时副屏 HUD PNG（兼容默认硬核太空舱风格）。
pub fn make_job_hud_png(
    spec: &JobHudSpec,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    make_job_hud_png_for_style(spec, ScreenCardStyle::MossHud, 0, canvas_w, canvas_h)
}

/// 生成支持双画风（`MossHud` 太空舱 / `GoggleBot` 戴防激光护目镜小机器人）的实时加工副屏 HUD PNG。
pub fn make_job_hud_png_for_style(
    spec: &JobHudSpec,
    style: ScreenCardStyle,
    frame: u8,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    let cw = canvas_w.clamp(96, 240);
    let ch = canvas_h.clamp(80, 240);
    let pct = spec.percent.min(100);

    let mut p_parts = Vec::new();
    if let Some(p) = spec.power {
        p_parts.push(format!("S{p}"));
    }
    if let Some(f) = spec.feed_rate {
        p_parts.push(format!("F{f}"));
    }
    let sf_line = if p_parts.is_empty() {
        "CNC ACTIVE".to_owned()
    } else {
        p_parts.join(" ")
    };

    if style == ScreenCardStyle::GoggleBot {
        let mut canvas = RgbImage::from_pixel(cw, ch, Rgb([12, 16, 26]));
        draw_cute_frame(&mut canvas, Rgb([72, 250, 210]), Rgb([255, 118, 98]));
        // 左侧绘制拉下防激光护目镜认真雕刻的小机器人 (52×52)
        draw_goggle_bot(
            &mut canvas,
            6,
            (ch as i32 - 60) / 2,
            2,
            BotPose::GogglesEngrave,
            frame,
        );
        let rx = 64i32;
        draw_text_5x7(&mut canvas, rx, 9, 1, "GOGGLES ON!", Rgb([255, 146, 72]));
        let pct_str = format!("{pct}%");
        draw_text_5x7(&mut canvas, rx, 22, 3, &pct_str, Rgb([72, 250, 210]));
        draw_segmented_bar(
            &mut canvas,
            rx,
            49,
            (cw as i32 - rx - 8).max(36),
            8,
            10,
            pct,
            Rgb([52, 240, 172]),
        );
        let line1 = if spec.total_lines > 0 {
            format!("{}/{}L", spec.lines_sent, spec.total_lines)
        } else {
            "LASER BUSY".to_owned()
        };
        let max_r = ((cw as i32 - rx - 6).max(6) / 6) as usize;
        let c1: String = line1.chars().take(max_r).collect();
        draw_text_5x7(
            &mut canvas,
            rx,
            ch as i32 - 26,
            1,
            &c1,
            Rgb([190, 206, 228]),
        );
        let line2 = match spec.eta_seconds {
            Some(eta) => format!("ETA {:02}:{:02}", eta / 60, eta % 60),
            None => sf_line,
        };
        let c2: String = line2.chars().take(max_r).collect();
        draw_text_5x7(&mut canvas, rx, ch as i32 - 14, 1, &c2, Rgb([0, 220, 255]));
        return encode_png_checked(&canvas);
    }

    let mut canvas = RgbImage::from_pixel(cw, ch, Rgb([9, 12, 18]));
    draw_moss_frame(&mut canvas, Rgb([255, 68, 56]));

    // 顶部红色 MOSS 光学指示点 + 标题 "MOSS LASER"
    let dot_c = if frame.is_multiple_of(2) {
        Rgb([255, 59, 48])
    } else {
        Rgb([255, 126, 110])
    };
    for dy in -2i32..=2 {
        for dx in -2i32..=2 {
            if dx * dx + dy * dy <= 4 {
                put_px(&mut canvas, 10 + dx, 10 + dy, dot_c);
            }
        }
    }
    draw_text_5x7(&mut canvas, 18, 7, 1, "MOSS LASER", Rgb([220, 228, 242]));

    let pct_str = format!("{pct}%");
    let pct_w = text_width_px(&pct_str, 3);
    let pct_x = ((cw as i32 - pct_w) / 2).max(6);
    let pct_y = (ch as i32 / 2) - 20;
    draw_text_5x7(&mut canvas, pct_x, pct_y, 3, &pct_str, Rgb([52, 245, 168]));
    draw_segmented_bar(
        &mut canvas,
        10,
        pct_y + 27,
        (cw as i32 - 20).max(32),
        9,
        16,
        pct,
        Rgb([42, 232, 156]),
    );

    let line1 = match (spec.total_lines > 0, spec.eta_seconds) {
        (true, Some(eta)) => format!(
            "{}/{}L {:02}:{:02}",
            spec.lines_sent,
            spec.total_lines,
            eta / 60,
            eta % 60
        ),
        (true, None) => format!("{}/{} LINES", spec.lines_sent, spec.total_lines),
        (false, _) => "STREAMING GCODE".to_owned(),
    };
    let max_chars = ((cw.saturating_sub(12)) / 6) as usize;
    let line1_clip: String = line1.chars().take(max_chars).collect();
    draw_text_5x7(
        &mut canvas,
        10,
        ch as i32 - 21,
        1,
        &line1_clip,
        Rgb([178, 194, 216]),
    );
    let line2_clip: String = sf_line.chars().take(max_chars).collect();
    draw_text_5x7(
        &mut canvas,
        10,
        ch as i32 - 11,
        1,
        &line2_clip,
        Rgb([0, 212, 255]),
    );

    encode_png_checked(&canvas)
}

/// ① 生成「待电脑审批警戒卡」PNG（严格模式下等待用户在桌面点击批准，显示倒计时条与动态图标）。
pub fn make_approval_card_png(
    spec: &ApprovalCardSpec,
    style: ScreenCardStyle,
    frame: u8,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    let cw = canvas_w.clamp(96, 240);
    let ch = canvas_h.clamp(80, 240);
    let total = spec.total_secs.max(1);
    let rem = spec.remaining_secs.min(total);
    let pct = ((rem * 100) / total).min(100) as u8;
    let amber = Rgb([255, 178, 56]);
    let red = Rgb([255, 72, 62]);

    if style == ScreenCardStyle::GoggleBot {
        let mut canvas = RgbImage::from_pixel(cw, ch, Rgb([20, 15, 24]));
        draw_cute_frame(&mut canvas, amber, red);
        draw_goggle_bot(
            &mut canvas,
            6,
            (ch as i32 - 56) / 2,
            2,
            BotPose::ApprovalWave,
            frame,
        );
        let rx = 64i32;
        draw_text_5x7(&mut canvas, rx, 12, 1, "CHECK PC!", amber);
        draw_text_5x7(&mut canvas, rx, 26, 2, "CONFIRM", Rgb([255, 236, 200]));
        draw_segmented_bar(
            &mut canvas,
            rx,
            48,
            (cw as i32 - rx - 8).max(36),
            8,
            10,
            pct,
            amber,
        );
        let wait_str = format!("WAIT {}S", spec.remaining_secs);
        draw_text_5x7(
            &mut canvas,
            rx,
            ch as i32 - 26,
            1,
            &wait_str,
            Rgb([255, 198, 112]),
        );
        let kind_clip: String = spec.kind_ascii.chars().take(14).collect();
        draw_text_5x7(
            &mut canvas,
            rx,
            ch as i32 - 14,
            1,
            &kind_clip,
            Rgb([188, 174, 196]),
        );
        return encode_png_checked(&canvas);
    }

    let mut canvas = RgbImage::from_pixel(cw, ch, Rgb([16, 11, 16]));
    draw_moss_frame(&mut canvas, amber);
    let core_c = if frame.is_multiple_of(2) { red } else { amber };
    draw_moss_optical_eye(&mut canvas, 28, ch as i32 / 2 - 4, 17, core_c, amber);
    let rx = 54i32;
    draw_text_5x7(&mut canvas, rx, 12, 1, "MOSS GATE", amber);
    draw_text_5x7(&mut canvas, rx, 26, 2, "APPROVAL", Rgb([255, 236, 214]));
    draw_segmented_bar(
        &mut canvas,
        rx,
        48,
        (cw as i32 - rx - 10).max(36),
        8,
        12,
        pct,
        amber,
    );
    let wait_str = format!("CHECK PC {}S", spec.remaining_secs);
    draw_text_5x7(
        &mut canvas,
        rx,
        ch as i32 - 25,
        1,
        &wait_str,
        Rgb([255, 196, 118]),
    );
    let kind_clip: String = spec.kind_ascii.chars().take(16).collect();
    draw_text_5x7(
        &mut canvas,
        rx,
        ch as i32 - 13,
        1,
        &kind_clip,
        Rgb([176, 184, 202]),
    );
    encode_png_checked(&canvas)
}

/// ② 生成「完工庆祝 / 急停断点 5 秒徽章卡」PNG（利用官方固件 5 秒自动隐藏特性，展示 5 秒后自动优雅淡出）。
pub fn make_terminal_badge_png(
    spec: &TerminalBadgeSpec,
    style: ScreenCardStyle,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    make_terminal_badge_png_frame(spec, style, 1, canvas_w, canvas_h)
}

pub fn make_terminal_badge_png_frame(
    spec: &TerminalBadgeSpec,
    style: ScreenCardStyle,
    frame: u8,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    let cw = canvas_w.clamp(96, 240);
    let ch = canvas_h.clamp(80, 240);
    let emerald = Rgb([52, 245, 168]);
    let crimson = Rgb([255, 82, 72]);

    if style == ScreenCardStyle::GoggleBot {
        let bg = if spec.completed {
            Rgb([10, 22, 22])
        } else {
            Rgb([22, 14, 18])
        };
        let mut canvas = RgbImage::from_pixel(cw, ch, bg);
        let accent = if spec.completed { emerald } else { crimson };
        draw_cute_frame(&mut canvas, accent, Rgb([255, 208, 68]));
        let pose = if spec.completed {
            BotPose::CelebrateDone
        } else {
            BotPose::HaltedShield
        };
        draw_goggle_bot(&mut canvas, 6, (ch as i32 - 56) / 2, 2, pose, frame);
        let rx = 64i32;
        if spec.completed {
            draw_text_5x7(&mut canvas, rx, 16, 1, "GREAT JOB!", Rgb([255, 214, 84]));
            draw_text_5x7(&mut canvas, rx, 32, 2, "DONE!", emerald);
            let l_str = format!("{} LINES", spec.lines);
            draw_text_5x7(&mut canvas, rx, 58, 1, &l_str, Rgb([194, 232, 218]));
            draw_text_5x7(&mut canvas, rx, 72, 1, "100% READY", Rgb([0, 220, 255]));
        } else {
            draw_text_5x7(&mut canvas, rx, 16, 1, "SAFETY STOP", Rgb([255, 186, 76]));
            draw_text_5x7(&mut canvas, rx, 32, 2, "HALTED", crimson);
            let l_str = format!("AT {}L", spec.lines);
            draw_text_5x7(&mut canvas, rx, 58, 1, &l_str, Rgb([232, 194, 198]));
            draw_text_5x7(&mut canvas, rx, 72, 1, "CHECK CNC", Rgb([255, 178, 56]));
        }
        return encode_png_checked(&canvas);
    }

    let bg = if spec.completed {
        Rgb([8, 19, 17])
    } else {
        Rgb([20, 11, 13])
    };
    let mut canvas = RgbImage::from_pixel(cw, ch, bg);
    let accent = if spec.completed { emerald } else { crimson };
    draw_moss_frame(&mut canvas, accent);
    let eye_core = if frame.is_multiple_of(2) {
        accent
    } else if spec.completed {
        Rgb([142, 255, 214])
    } else {
        Rgb([255, 146, 98])
    };
    draw_moss_optical_eye(&mut canvas, 28, ch as i32 / 2, 17, eye_core, accent);
    let pulse_x = 54 + ((frame as i32 % 4) * 20);
    for dx in 0..10 {
        put_px(&mut canvas, pulse_x + dx, 88, accent);
    }
    let rx = 54i32;
    if spec.completed {
        draw_text_5x7(&mut canvas, rx, 16, 1, "MOSS LASER", Rgb([172, 226, 206]));
        draw_text_5x7(&mut canvas, rx, 32, 2, "COMPLETE", emerald);
        let l_str = format!("{} LINES OK", spec.lines);
        draw_text_5x7(&mut canvas, rx, 58, 1, &l_str, Rgb([210, 234, 224]));
        draw_text_5x7(&mut canvas, rx, 72, 1, "STANDBY IDLE", Rgb([0, 212, 255]));
    } else {
        draw_text_5x7(&mut canvas, rx, 16, 1, "MOSS GUARD", Rgb([255, 186, 82]));
        draw_text_5x7(&mut canvas, rx, 32, 2, "HALTED", crimson);
        let l_str = format!("STOP AT {}L", spec.lines);
        draw_text_5x7(&mut canvas, rx, 58, 1, &l_str, Rgb([236, 198, 202]));
        draw_text_5x7(
            &mut canvas,
            rx,
            72,
            1,
            "VERIFY MACHINE",
            Rgb([255, 178, 56]),
        );
    }
    encode_png_checked(&canvas)
}

/// ③ 生成「本地音乐像素黑胶唱机 / 小机器人听歌摇摆卡」PNG。
pub fn make_music_card_png(
    spec: &MusicCardSpec,
    style: ScreenCardStyle,
    frame: u8,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    let cw = canvas_w.clamp(96, 240);
    let ch = canvas_h.clamp(80, 240);
    let cyan = Rgb([0, 226, 255]);
    let pink = Rgb([255, 108, 168]);

    if style == ScreenCardStyle::GoggleBot {
        let mut canvas = RgbImage::from_pixel(cw, ch, Rgb([14, 14, 28]));
        draw_cute_frame(&mut canvas, cyan, pink);
        draw_goggle_bot(
            &mut canvas,
            6,
            (ch as i32 - 56) / 2,
            2,
            BotPose::MusicGroove,
            frame,
        );
        let rx = 64i32;
        draw_text_5x7(&mut canvas, rx, 14, 1, "MOSS AUDIO", pink);
        draw_text_5x7(&mut canvas, rx, 28, 2, "GROOVE", cyan);
        // 动态跳动的 8 柱像素音频频谱条
        let heights = match frame % 4 {
            0 => [6, 14, 10, 18, 8, 15, 11, 7],
            1 => [12, 8, 17, 10, 16, 9, 14, 11],
            2 => [8, 16, 12, 14, 18, 11, 7, 15],
            _ => [15, 10, 14, 8, 12, 17, 13, 9],
        };
        let base_y = 74i32;
        for (idx, &bh) in heights.iter().enumerate() {
            let bx = rx + (idx as i32) * 10;
            let bar_c = if idx % 2 == 0 {
                cyan
            } else {
                Rgb([72, 250, 210])
            };
            for yy in (base_y - bh)..base_y {
                for xx in bx..(bx + 6) {
                    put_px(&mut canvas, xx, yy, bar_c);
                }
            }
        }
        let clip: String = spec.title_ascii.chars().take(14).collect();
        draw_text_5x7(
            &mut canvas,
            rx,
            ch as i32 - 14,
            1,
            &clip,
            Rgb([208, 218, 240]),
        );
        return encode_png_checked(&canvas);
    }

    let mut canvas = RgbImage::from_pixel(cw, ch, Rgb([10, 13, 22]));
    draw_moss_frame(&mut canvas, cyan);
    // 左侧绘制旋转黑胶唱片 (r = 22)
    let vcx = 34i32;
    let vcy = ch as i32 / 2;
    for dy in -22i32..=22 {
        for dx in -22i32..=22 {
            let d2 = dx * dx + dy * dy;
            if d2 <= 9 {
                put_px(&mut canvas, vcx + dx, vcy + dy, Rgb([12, 14, 20]));
            } else if d2 <= 64 {
                put_px(&mut canvas, vcx + dx, vcy + dy, Rgb([255, 68, 56]));
            } else if d2 <= 484 {
                let groove = (196..=225).contains(&d2) || (324..=360).contains(&d2);
                let spin_hi = match frame % 4 {
                    0 => dx > 0 && dy < 0,
                    1 => dx > 0 && dy > 0,
                    2 => dx < 0 && dy > 0,
                    _ => dx < 0 && dy < 0,
                };
                let c = if groove && spin_hi {
                    cyan
                } else if groove {
                    Rgb([44, 58, 82])
                } else {
                    Rgb([22, 28, 40])
                };
                put_px(&mut canvas, vcx + dx, vcy + dy, c);
            }
        }
    }
    let rx = 64i32;
    draw_text_5x7(&mut canvas, rx, 14, 1, "NOW PLAYING", cyan);
    draw_text_5x7(&mut canvas, rx, 28, 2, "VINYL", Rgb([232, 240, 252]));
    let heights = match frame % 4 {
        0 => [8, 16, 11, 18, 9, 14, 12, 6],
        1 => [14, 9, 18, 11, 15, 8, 16, 10],
        2 => [9, 17, 12, 15, 18, 12, 8, 14],
        _ => [16, 11, 15, 9, 11, 17, 10, 8],
    };
    let base_y = 74i32;
    for (idx, &bh) in heights.iter().enumerate() {
        let bx = rx + (idx as i32) * 10;
        for yy in (base_y - bh)..base_y {
            for xx in bx..(bx + 6) {
                put_px(&mut canvas, xx, yy, Rgb([52, 245, 168]));
            }
        }
    }
    let clip: String = spec.title_ascii.chars().take(14).collect();
    draw_text_5x7(
        &mut canvas,
        rx,
        ch as i32 - 14,
        1,
        &clip,
        Rgb([182, 198, 222]),
    );
    encode_png_checked(&canvas)
}

/// 生成可在小智 `320×240` 屏幕上直接预览的「小 MOSS 护目镜小机器人」角色展示卡 PNG。
pub fn make_goggle_bot_showcase_png(
    pose: BotPose,
    frame: u8,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    let mut png = crate::goggle_bot::make_bot_showcase_card_png(pose, frame, canvas_w, canvas_h)?;
    if let Ok(decoded) = image::load_from_memory(&png) {
        let mut canvas = decoded.to_rgb8();
        let rx = 68i32;
        draw_text_5x7(&mut canvas, rx, 18, 1, "MOSS BOT", Rgb([255, 146, 72]));
        let tag = pose.slug().to_ascii_uppercase();
        draw_text_5x7(&mut canvas, rx, 34, 2, &tag, Rgb([72, 250, 210]));
        draw_text_5x7(
            &mut canvas,
            rx,
            62,
            1,
            "GOGGLES READY",
            Rgb([194, 212, 236]),
        );
        png = encode_png_checked(&canvas)?;
    }
    Ok(png)
}

/// 按指定画风（`MossHud` 硬核太空舱 / `GoggleBot` 可爱像素小机器人）生成任意场景的 1:1 卡片 PNG。
pub fn make_scene_showcase_png(
    pose: BotPose,
    style: ScreenCardStyle,
    frame: u8,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    let cw = canvas_w.clamp(96, 240);
    let ch = canvas_h.clamp(80, 240);
    match pose {
        BotPose::IdleBlink => {
            let cyan = Rgb([0, 224, 255]);
            let mint = Rgb([72, 250, 210]);
            if style == ScreenCardStyle::GoggleBot {
                let mut canvas = RgbImage::from_pixel(cw, ch, Rgb([12, 16, 26]));
                draw_cute_frame(&mut canvas, mint, Rgb([255, 126, 108]));
                draw_goggle_bot(
                    &mut canvas,
                    6,
                    (ch as i32 - 62) / 2,
                    2,
                    BotPose::IdleBlink,
                    frame,
                );
                let bx0 = 64i32;
                let by0 = 12i32;
                let bw = (cw as i32 - bx0 - 8).max(48);
                let bh = (ch as i32 - 32).max(44);
                for x in bx0..(bx0 + bw) {
                    put_px(&mut canvas, x, by0, Rgb([38, 56, 82]));
                    put_px(&mut canvas, x, by0 + bh, Rgb([38, 56, 82]));
                }
                for y in by0..(by0 + bh + 1) {
                    put_px(&mut canvas, bx0, y, Rgb([38, 56, 82]));
                    put_px(&mut canvas, bx0 + bw, y, Rgb([38, 56, 82]));
                }
                draw_text_5x7(&mut canvas, bx0 + 8, by0 + 10, 2, "MOSS", mint);
                let sweep_x = bx0 + 8 + ((frame as i32 % 4) * ((bw - 18).max(12) / 3));
                for dy in -2i32..=2 {
                    put_px(&mut canvas, sweep_x, by0 + 32 + dy, Rgb([255, 206, 82]));
                }
                for x in (bx0 + 6)..(bx0 + bw - 6) {
                    put_px(&mut canvas, x, by0 + 32, cyan);
                }
                draw_text_5x7(
                    &mut canvas,
                    bx0 + 6,
                    by0 + bh - 12,
                    1,
                    "PREVIEW 1:1",
                    Rgb([188, 208, 232]),
                );
                draw_text_5x7(
                    &mut canvas,
                    10,
                    ch as i32 - 12,
                    1,
                    "40X30MM S600 F1200",
                    cyan,
                );
                encode_png_checked(&canvas)
            } else {
                let mut canvas = RgbImage::from_pixel(cw, ch, Rgb([9, 12, 18]));
                draw_moss_frame(&mut canvas, Rgb([255, 68, 56]));
                let core_c = if frame.is_multiple_of(2) {
                    Rgb([255, 58, 48])
                } else {
                    Rgb([255, 118, 96])
                };
                draw_moss_optical_eye(
                    &mut canvas,
                    28,
                    ch as i32 / 2 - 4,
                    17,
                    core_c,
                    Rgb([255, 78, 66]),
                );
                let bx0 = 54i32;
                let by0 = 12i32;
                let bw = (cw as i32 - bx0 - 8).max(52);
                let bh = (ch as i32 - 32).max(44);
                for x in bx0..(bx0 + bw) {
                    put_px(&mut canvas, x, by0, Rgb([34, 48, 72]));
                    put_px(&mut canvas, x, by0 + bh, Rgb([34, 48, 72]));
                }
                for y in by0..(by0 + bh + 1) {
                    put_px(&mut canvas, bx0, y, Rgb([34, 48, 72]));
                    put_px(&mut canvas, bx0 + bw, y, Rgb([34, 48, 72]));
                }
                draw_text_5x7(
                    &mut canvas,
                    bx0 + 6,
                    by0 + 6,
                    1,
                    "MOSS PATH",
                    Rgb([168, 186, 212]),
                );
                draw_text_5x7(&mut canvas, bx0 + 10, by0 + 20, 2, "MOSS", cyan);
                let scan_x = bx0 + 8 + ((frame as i32 % 4) * ((bw - 18).max(12) / 3));
                for y in (by0 + 18)..(by0 + 38) {
                    put_px(&mut canvas, scan_x, y, Rgb([255, 72, 62]));
                }
                for x in (bx0 + 6)..(bx0 + bw - 6) {
                    put_px(&mut canvas, x, by0 + 42, Rgb([52, 245, 168]));
                }
                draw_text_5x7(
                    &mut canvas,
                    10,
                    ch as i32 - 12,
                    1,
                    "40X30MM S600 F1200",
                    cyan,
                );
                encode_png_checked(&canvas)
            }
        }
        BotPose::GogglesEngrave => {
            let pct = 62 + (frame % 4) * 3;
            let sent = 620 + (frame as u64 % 4) * 30;
            let eta = 16u64.saturating_sub(frame as u64 % 4);
            make_job_hud_png_for_style(
                &JobHudSpec {
                    percent: pct,
                    lines_sent: sent,
                    total_lines: 1000,
                    eta_seconds: Some(eta),
                    power: Some(600),
                    feed_rate: Some(1200),
                },
                style,
                frame,
                cw,
                ch,
            )
        }
        BotPose::ApprovalWave => {
            let rem = 118u64.saturating_sub((frame as u64 % 4) * 6);
            make_approval_card_png(
                &ApprovalCardSpec {
                    kind_ascii: "LASER SEND".into(),
                    remaining_secs: rem,
                    total_secs: 180,
                },
                style,
                frame,
                cw,
                ch,
            )
        }
        BotPose::MusicGroove => make_music_card_png(
            &MusicCardSpec {
                title_ascii: "MOSS GROOVE".into(),
            },
            style,
            frame,
            cw,
            ch,
        ),
        BotPose::CelebrateDone => make_terminal_badge_png_frame(
            &TerminalBadgeSpec {
                completed: true,
                lines: 1000,
            },
            style,
            frame,
            cw,
            ch,
        ),
        BotPose::HaltedShield => make_terminal_badge_png_frame(
            &TerminalBadgeSpec {
                completed: false,
                lines: 420,
            },
            style,
            frame,
            cw,
            ch,
        ),
    }
}

/// 编码单张 `160×120` 场景卡的 4 帧循环 GIF89a 动图。
pub fn encode_scene_showcase_gif(pose: BotPose, style: ScreenCardStyle) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(8192);
    {
        let mut encoder = GifEncoder::new_with_speed(&mut out, 10);
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|e| format!("设置场景 GIF 循环失败：{e}"))?;
        for f in 0..4u8 {
            let png = make_scene_showcase_png(pose, style, f, DEFAULT_CANVAS_W, DEFAULT_CANVAS_H)?;
            let decoded = image::load_from_memory(&png)
                .map_err(|e| format!("解码场景帧失败：{e}"))?
                .to_rgba8();
            let (w, h) = decoded.dimensions();
            let mut rgba = RgbaImage::new(w, h);
            for (x, y, p) in decoded.enumerate_pixels() {
                rgba.put_pixel(x, y, Rgba([p[0], p[1], p[2], 255]));
            }
            let frame = Frame::from_parts(rgba, 0, 0, Delay::from_numer_denom_ms(280, 1));
            encoder
                .encode_frame(frame)
                .map_err(|e| format!("编码场景 GIF 帧失败：{e}"))?;
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, Serialize)]
pub struct StyleSceneGifCatalog {
    pub moss_hud: BTreeMap<&'static str, String>,
    pub goggle_bot: BTreeMap<&'static str, String>,
}

/// 进程级缓存「硬核太空舱」与「可爱像素小机器人」两套完整的 160×120 5 场景动态 GIF89a。
pub fn cached_style_scene_gifs() -> &'static StyleSceneGifCatalog {
    static CACHE: OnceLock<StyleSceneGifCatalog> = OnceLock::new();
    CACHE.get_or_init(|| {
        let poses = [
            BotPose::IdleBlink,
            BotPose::GogglesEngrave,
            BotPose::ApprovalWave,
            BotPose::MusicGroove,
            BotPose::CelebrateDone,
        ];
        let build_map = |style: ScreenCardStyle| -> BTreeMap<&'static str, String> {
            let mut map = BTreeMap::new();
            for pose in poses {
                if let Ok(gif) = encode_scene_showcase_gif(pose, style) {
                    map.insert(
                        pose.slug(),
                        format!("data:image/gif;base64,{}", base64_encode(&gif)),
                    );
                }
            }
            map
        };
        StyleSceneGifCatalog {
            moss_hud: build_map(ScreenCardStyle::MossHud),
            goggle_bot: build_map(ScreenCardStyle::GoggleBot),
        }
    })
}

impl PreviewStore {
    pub fn stage_thumbnail(&self, source_bytes: &[u8]) -> Result<String, String> {
        self.stage_thumbnail_for_style(
            source_bytes,
            DEFAULT_CANVAS_W,
            DEFAULT_CANVAS_H,
            None,
            ScreenCardStyle::MossHud,
        )
    }

    pub fn stage_thumbnail_for_canvas(
        &self,
        source_bytes: &[u8],
        canvas_w: u32,
        canvas_h: u32,
        ruler_ascii: Option<&str>,
    ) -> Result<String, String> {
        self.stage_thumbnail_for_style(
            source_bytes,
            canvas_w,
            canvas_h,
            ruler_ascii,
            ScreenCardStyle::MossHud,
        )
    }

    pub fn stage_thumbnail_for_style(
        &self,
        source_bytes: &[u8],
        canvas_w: u32,
        canvas_h: u32,
        ruler_ascii: Option<&str>,
        style: ScreenCardStyle,
    ) -> Result<String, String> {
        let png = make_device_thumbnail_png_for_style(
            source_bytes,
            canvas_w,
            canvas_h,
            ruler_ascii,
            style,
        )?;
        self.stage_prebuilt_png(png)
    }
    pub fn stage_prebuilt_png(&self, png: Vec<u8>) -> Result<String, String> {
        if png.is_empty() || png.len() > MAX_THUMBNAIL_PNG_BYTES {
            return Err("屏幕图像体积超出范围".into());
        }
        let token = random_token_hex()?;
        self.insert_with_now(token.clone(), png, unix_now());
        Ok(token)
    }

    fn insert_with_now(&self, token: String, png: Vec<u8>, now: u64) {
        if let Ok(mut guard) = self.items.lock() {
            guard.retain(|item| item.expires_at > now);
            while guard.len() >= MAX_PREVIEW_ENTRIES {
                guard.pop_front();
            }
            guard.push_back(PreviewItem {
                token,
                png_bytes: Arc::new(png),
                expires_at: now.saturating_add(PREVIEW_TTL_SECS),
            });
        }
    }

    pub fn get(&self, token: &str) -> Option<Arc<Vec<u8>>> {
        self.get_at(token, unix_now())
    }

    fn get_at(&self, token: &str, now: u64) -> Option<Arc<Vec<u8>>> {
        if !token_ok(token) {
            return None;
        }
        let mut guard = self.items.lock().ok()?;
        guard.retain(|item| item.expires_at > now);
        guard
            .iter()
            .find(|item| item.token.eq_ignore_ascii_case(token))
            .map(|item| item.png_bytes.clone())
    }
}

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/mcp/preview/{file}", get(serve_preview))
        .route("/mcp/preview/goggle-bot/{file}", get(serve_goggle_bot_gif))
        .with_state(shared)
}

/// 免鉴权的局域网路由：2026-11-05 起取进程级缓存（每个姿势只编码一次），首次编码放到阻塞线程，
/// 反复请求不再在 tokio 工作线程上逐次重编码 GIF。
async fn serve_goggle_bot_gif(Path(file): Path<String>) -> Response {
    let slug = file.strip_suffix(".gif").unwrap_or(&file);
    let Some(pose) = BotPose::from_slug(slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cached = match crate::goggle_bot::goggle_bot_gif_if_encoded(pose) {
        Some(cached) => cached,
        None => tokio::task::spawn_blocking(move || crate::goggle_bot::cached_goggle_bot_gif(pose))
            .await
            .ok()
            .flatten(),
    };
    let Some(bytes) = cached else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let len = bytes.len().to_string();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/gif".to_owned()),
            (header::CONTENT_LENGTH, len),
            (header::CACHE_CONTROL, "public, max-age=300".to_owned()),
        ],
        bytes,
    )
        .into_response()
}
async fn serve_preview(State(shared): State<Arc<Shared>>, Path(file): Path<String>) -> Response {
    let token = file.strip_suffix(".png").unwrap_or(&file);
    let Some(bytes) = shared.preview_store.get(token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let len = bytes.len().to_string();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png".to_owned()),
            (header::CONTENT_LENGTH, len),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        bytes.as_ref().clone(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn lichuang_320x240_canvas_renders_exact_160x120_dilates_fine_lines_and_builds_live_hud() {
        assert_eq!(optimal_canvas_for_screen(Some((320, 240))), (160, 120));
        assert_eq!(optimal_canvas_for_screen(Some((240, 320))), (120, 160));

        // 构造白底 1px 极细黑线原图（模拟激光刀路原图）
        let mut fine = RgbaImage::from_pixel(400, 300, Rgba([255, 255, 255, 255]));
        for x in 40..360 {
            fine.put_pixel(x, 150, Rgba([0, 0, 0, 255]));
        }
        let mut raw_png = Vec::new();
        fine.write_to(&mut Cursor::new(&mut raw_png), ImageFormat::Png)
            .unwrap();

        let card =
            make_device_thumbnail_png_for_canvas(&raw_png, 160, 120, Some("40x30MM S600 F1200"))
                .unwrap();
        assert!(&card[..8] == b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&card).unwrap().to_rgb8();
        assert_eq!(
            decoded.dimensions(),
            (160, 120),
            "精确匹配立创实战派 160×120 1:1 零缩放画布"
        );
        let bright_toolpath_px = decoded
            .pixels()
            .filter(|p| p[1] > 140 && p[2] > 100 && p[0] < 120)
            .count();
        assert!(
            bright_toolpath_px >= 80,
            "1px 细线经形态学加粗后必须清晰可见，实际像素={bright_toolpath_px}"
        );

        // 构造实时加工进度副屏 HUD
        let hud = make_job_hud_png(
            &JobHudSpec {
                percent: 68,
                lines_sent: 680,
                total_lines: 1000,
                power: Some(600),
                feed_rate: Some(1200),
                eta_seconds: Some(42),
            },
            160,
            120,
        )
        .unwrap();
        assert!(
            hud.len() < 16 * 1024,
            "实时副屏 HUD 必须极轻量，实际={}B",
            hud.len()
        );
        let hud_img = image::load_from_memory(&hud).unwrap();
        assert_eq!((hud_img.width(), hud_img.height()), (160, 120));

        // 容量与 TTL 校验
        let store = PreviewStore::default();
        for i in 0..14u8 {
            let tok = format!("{:032x}", i as u128 + 1);
            store.insert_with_now(tok, hud.clone(), 100);
        }
        assert!(store.get_at(&format!("{:032x}", 1u128), 110).is_none());
        assert!(store.get_at(&format!("{:032x}", 14u128), 110).is_some());
        assert!(store.get_at(&format!("{:032x}", 14u128), 500).is_none());
    }

    /// 2026-11-05：免鉴权 GIF 路由取进程级缓存，字节与现场编码完全一致，同一姿势只编码一次。
    #[test]
    fn goggle_bot_gif_route_cache_is_encoded_once_with_identical_bytes() {
        for slug in [
            "idle",
            "engrave",
            "approval",
            "music",
            "celebrate",
            "halted",
        ] {
            let pose = BotPose::from_slug(slug).unwrap();
            let first = crate::goggle_bot::cached_goggle_bot_gif(pose).unwrap();
            let again = crate::goggle_bot::cached_goggle_bot_gif(pose).unwrap();
            assert!(std::ptr::eq(first, again), "{slug} 不应重复编码");
            assert_eq!(
                crate::goggle_bot::goggle_bot_gif_if_encoded(pose),
                Some(Some(first))
            );
            assert_eq!(
                first,
                crate::goggle_bot::encode_goggle_bot_gif(pose)
                    .unwrap()
                    .as_slice(),
                "{slug}"
            );
        }
    }

    #[test]
    fn goggle_bot_gifs_and_all_three_zero_flash_cards_render_cleanly_in_both_styles() {
        let gifs = crate::goggle_bot::cached_goggle_bot_gifs();
        assert_eq!(gifs.len(), 5);
        for item in gifs {
            assert!(
                item.data_url.starts_with("data:image/gif;base64,R0lGODlh"),
                "{}",
                item.id
            );
        }
        let raw_gif = crate::goggle_bot::encode_goggle_bot_gif(BotPose::GogglesEngrave).unwrap();
        assert_eq!(&raw_gif[..6], b"GIF89a");

        for style in [ScreenCardStyle::MossHud, ScreenCardStyle::GoggleBot] {
            let apr = make_approval_card_png(
                &ApprovalCardSpec {
                    kind_ascii: "LASER WORKFLOW".into(),
                    remaining_secs: 95,
                    total_secs: 120,
                },
                style,
                1,
                160,
                120,
            )
            .unwrap();
            let apr_img = image::load_from_memory(&apr).unwrap();
            assert_eq!((apr_img.width(), apr_img.height()), (160, 120));

            let done = make_terminal_badge_png(
                &TerminalBadgeSpec {
                    completed: true,
                    lines: 520,
                },
                style,
                160,
                120,
            )
            .unwrap();
            let done_img = image::load_from_memory(&done).unwrap();
            assert_eq!((done_img.width(), done_img.height()), (160, 120));

            let halted = make_terminal_badge_png(
                &TerminalBadgeSpec {
                    completed: false,
                    lines: 128,
                },
                style,
                160,
                120,
            )
            .unwrap();
            let halted_img = image::load_from_memory(&halted).unwrap();
            assert_eq!((halted_img.width(), halted_img.height()), (160, 120));

            let music = make_music_card_png(
                &MusicCardSpec {
                    title_ascii: "TRACK 01".into(),
                },
                style,
                2,
                160,
                120,
            )
            .unwrap();
            let music_img = image::load_from_memory(&music).unwrap();
            assert_eq!((music_img.width(), music_img.height()), (160, 120));
        }

        let catalog = cached_style_scene_gifs();
        for key in ["idle", "engrave", "approval", "music", "celebrate"] {
            let m_url = catalog.moss_hud.get(key).expect("moss_hud gif exists");
            let b_url = catalog.goggle_bot.get(key).expect("goggle_bot gif exists");
            assert!(m_url.starts_with("data:image/gif;base64,R0lGODlh"));
            assert!(b_url.starts_with("data:image/gif;base64,R0lGODlh"));
            assert_ne!(m_url, b_url, "两套画风的 {key} 动图画面必须不同");
        }
    }
}
