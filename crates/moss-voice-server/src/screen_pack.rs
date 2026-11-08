//! n70 小智全屏画风资源包（2026-11-08 用户决定：“显示整个屏幕，字体压在背景上，一直显示，不要几秒又换回官方”）。
//!
//! 核实自 `78/xiaozhi-esp32` 官方固件（`main/assets.cc`、`main/display/lcd_display.cc`）：
//! - 资源包 `index.json` 的 `emoji_collection` 每项 `{name, file}`，文件可为 GIF（`LvglRawImage::IsGif`，固件循环播放）；
//! - 表情控件 `emoji_box_` 在最底层、尺寸随图片、居中、背景透明，顶栏、状态字与对话字幕都画在它上面，
//!   所以一张屏幕大小的 GIF 会铺满全屏，固件自己的文字叠在上面；表情没有计时器，换下一个表情前一直显示；
//! - 固件在待命 / 连接 / 聆听时固定 `SetEmotion("neutral")`，说话时显示服务端 `llm` 消息里的表情名；
//!   找不到的表情名不改画面（`SetEmotion` 返回 false），所以没刷画风包的设备收到 `hud_listen` 之类的名字也不会花屏；
//! - `skin.{light,dark}.background_image` 是整屏背景（RGB565 + 28 字节 `lv_image_dsc_t` 头，与官方生成器
//!   `AssetsBuilder.convertImageToRgb565` 一致），只在表情被预览图挡住时露出来——本模块把它画成雕刻时
//!   刀路预览卡（屏幕中央半屏）四周的全屏框。
//!
//! 每个场景先按半分辨率（`320×240` 屏 → `160×120`）画像素画，再 2 倍最近邻放大成全屏，和副屏预览卡同一套笔触。
//! 两套画风都打进包里（`hud_*` / `bot_*`），在线切换画风不用重刷；只有固件固定显示的 `neutral`（待命画面）
//! 跟随刷包时选的画风。

use std::collections::BTreeMap;
use std::sync::OnceLock;

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, Rgb, RgbImage, Rgba, RgbaImage};
use serde_json::json;

use crate::goggle_bot::{BotPose, ScreenCardStyle, base64_encode, draw_goggle_bot};
use crate::preview_push::{draw_moss_optical_eye, draw_text_5x7, put_px, text_width_px};
use crate::wake_assets::{AssetEntry, ScreenExtras};

/// 画风包版本，记在设备偏好 `screen_pack` 里（`moss-screen-1:moss_hud`）。
pub const PACK_VERSION: &str = "moss-screen-1";
/// 每个场景的帧数与帧间隔（毫秒）。
pub const FRAMES: u8 = 4;
const FRAME_MS: u32 = 300;
const BACKDROP_FILE: &str = "moss_backdrop.raw";

/// 画风包里的 12 个场景（复用 [`BotPose`] 作场景名）。
pub const SCENES: [BotPose; 12] = [
    BotPose::IdleBlink,
    BotPose::Listening,
    BotPose::Thinking,
    BotPose::Talking,
    BotPose::Happy,
    BotPose::Sad,
    BotPose::Surprised,
    BotPose::GogglesEngrave,
    BotPose::ApprovalWave,
    BotPose::MusicGroove,
    BotPose::CelebrateDone,
    BotPose::HaltedShield,
];

/// 小智固件认识的 21 个标准表情名；包里把它们指向刷包画风的对应场景。
const STANDARD_EMOTIONS: [&str; 21] = [
    "neutral",
    "happy",
    "laughing",
    "funny",
    "sad",
    "angry",
    "crying",
    "loving",
    "embarrassed",
    "surprised",
    "shocked",
    "thinking",
    "winking",
    "cool",
    "relaxed",
    "delicious",
    "kissy",
    "confident",
    "sleepy",
    "silly",
    "confused",
];

pub fn style_prefix(style: ScreenCardStyle) -> &'static str {
    match style {
        ScreenCardStyle::MossHud => "hud",
        ScreenCardStyle::GoggleBot => "bot",
    }
}

/// 设备上的表情名，例如 `hud_listen`、`bot_engrave`。
pub fn emotion_name(style: ScreenCardStyle, scene: BotPose) -> String {
    format!("{}_{}", style_prefix(style), scene.slug())
}

/// 设备偏好里记录的画风包标记。
pub fn pack_marker(style: ScreenCardStyle) -> String {
    format!("{PACK_VERSION}:{}", style.as_str())
}

/// 设备偏好里的标记是否表示已装好本版画风包。
pub fn marker_installed(marker: &str) -> bool {
    marker
        .split(':')
        .next()
        .is_some_and(|version| version == PACK_VERSION)
}

/// 标准表情名在包里指向哪个场景（固件待命固定 `neutral` → 待命画面）。
fn alias_scene(name: &str) -> BotPose {
    match name {
        "neutral" | "sleepy" | "relaxed" => BotPose::IdleBlink,
        "thinking" => BotPose::Thinking,
        "sad" | "crying" | "embarrassed" | "angry" => BotPose::Sad,
        "surprised" | "shocked" | "confused" => BotPose::Surprised,
        _ => BotPose::Happy,
    }
}

/// 说话时大模型给出的表情名换成哪个场景；没有明显情绪时显示“说话”。
pub fn speaking_scene(emotion: &str) -> BotPose {
    match emotion {
        "thinking" => BotPose::Thinking,
        "sad" | "crying" | "embarrassed" | "angry" => BotPose::Sad,
        "surprised" | "shocked" | "confused" => BotPose::Surprised,
        "happy" | "laughing" | "funny" | "loving" | "kissy" | "delicious" | "winking" | "cool"
        | "confident" | "silly" => BotPose::Happy,
        _ => BotPose::Talking,
    }
}

/// 半分辨率画布：屏幕宽高各一半（`320×240` → `160×120`）。
pub fn logical_size(screen: (u32, u32)) -> (u32, u32) {
    ((screen.0 / 2).clamp(48, 240), (screen.1 / 2).clamp(40, 240))
}

const RED: Rgb<u8> = Rgb([255, 64, 54]);
const CYAN: Rgb<u8> = Rgb([0, 224, 255]);
const MINT: Rgb<u8> = Rgb([72, 250, 210]);
const AMBER: Rgb<u8> = Rgb([255, 190, 60]);
const WHITE: Rgb<u8> = Rgb([240, 246, 255]);
const DIM: Rgb<u8> = Rgb([40, 54, 76]);
const PINK: Rgb<u8> = Rgb([255, 108, 148]);

fn scale_rgb(c: Rgb<u8>, k: u32) -> Rgb<u8> {
    Rgb([
        (u32::from(c[0]) * k / 255) as u8,
        (u32::from(c[1]) * k / 255) as u8,
        (u32::from(c[2]) * k / 255) as u8,
    ])
}

fn fill(img: &mut RgbImage, x: i32, y: i32, w: i32, h: i32, c: Rgb<u8>) {
    for yy in y..y + h {
        for xx in x..x + w {
            put_px(img, xx, yy, c);
        }
    }
}

/// 圆环（`r` 为外半径，`t` 为粗细）；`step` > 1 时画成虚线。
fn ring(img: &mut RgbImage, cx: i32, cy: i32, r: i32, t: i32, step: i32, c: Rgb<u8>) {
    let inner = (r - t).max(0);
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 <= r * r && d2 > inner * inner && (step <= 1 || (dx + dy).rem_euclid(step) == 0) {
                put_px(img, cx + dx, cy + dy, c);
            }
        }
    }
}

fn disc(img: &mut RgbImage, cx: i32, cy: i32, r: i32, c: Rgb<u8>) {
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                put_px(img, cx + dx, cy + dy, c);
            }
        }
    }
}

fn line(img: &mut RgbImage, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgb<u8>) {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y, mut err) = (x0, y0, dx + dy);
    loop {
        put_px(img, x, y, c);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

fn centered_text(img: &mut RgbImage, cx: i32, y: i32, scale: i32, text: &str, c: Rgb<u8>) {
    draw_text_5x7(img, cx - text_width_px(text, scale) / 2, y, scale, text, c);
}

/// 8 个方向的单位向量（×16），用于旋转的唱片指针、光芒。
const DIRS: [(i32, i32); 8] = [
    (16, 0),
    (11, 11),
    (0, 16),
    (-11, 11),
    (-16, 0),
    (-11, -11),
    (0, -16),
    (11, -11),
];

/// 文字安全区：顶部留给固件状态栏，底部留给对话字幕。
struct Layout {
    w: i32,
    h: i32,
    top: i32,
    bottom: i32,
}

impl Layout {
    fn new(img: &RgbImage) -> Self {
        let (w, h) = (img.width() as i32, img.height() as i32);
        Self {
            w,
            h,
            top: h * 12 / 100,
            bottom: h * 18 / 100,
        }
    }
    fn avail(&self) -> i32 {
        self.h - self.top - self.bottom
    }
    fn cx(&self) -> i32 {
        self.w / 2
    }
    fn cy(&self) -> i32 {
        self.top + self.avail() / 2
    }
}

fn corner_brackets(img: &mut RgbImage, len: i32, c: Rgb<u8>) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    for i in 0..len {
        for (x, y) in [
            (2 + i, 2),
            (2, 2 + i),
            (w - 3 - i, 2),
            (w - 3, 2 + i),
            (2 + i, h - 3),
            (2, h - 3 - i),
            (w - 3 - i, h - 3),
            (w - 3, h - 3 - i),
        ] {
            put_px(img, x, y, c);
        }
    }
}

fn border(img: &mut RgbImage, t: i32, c: Rgb<u8>) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    fill(img, 0, 0, w, t, c);
    fill(img, 0, h - t, w, t, c);
    fill(img, 0, 0, t, h, c);
    fill(img, w - t, 0, t, h, c);
}

fn hazard_corners(img: &mut RgbImage, size: i32, frame: u8) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let shift = i32::from(frame % 2) * 2;
    for y in 0..size {
        for x in 0..(size - y) {
            let c = if (x + y + shift) / 3 % 2 == 0 {
                AMBER
            } else {
                Rgb([30, 22, 10])
            };
            put_px(img, x, y, c);
            put_px(img, w - 1 - x, y, c);
            put_px(img, x, h - 1 - y, c);
            put_px(img, w - 1 - x, h - 1 - y, c);
        }
    }
}

fn hud_background(img: &mut RgbImage, accent: Rgb<u8>) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    for p in img.pixels_mut() {
        *p = Rgb([7, 10, 16]);
    }
    for y in (4..h).step_by(8) {
        for x in (4..w).step_by(8) {
            put_px(img, x, y, Rgb([18, 26, 40]));
        }
    }
    corner_brackets(img, 10, accent);
}

/// 伪随机但固定的星点位置。
fn star_at(i: i32, w: i32, h: i32) -> (i32, i32) {
    let n = i as u32;
    let a = n.wrapping_mul(2_654_435_761).wrapping_add(17) >> 8;
    let b = n.wrapping_mul(1_597_334_677).wrapping_add(29) >> 8;
    ((a % w.max(1) as u32) as i32, (b % h.max(1) as u32) as i32)
}

fn bot_background(img: &mut RgbImage, frame: u8) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    for p in img.pixels_mut() {
        *p = Rgb([11, 15, 25]);
    }
    for i in 0..22 {
        let (x, y) = star_at(i, w, h);
        let c = if (i + i32::from(frame)) % 4 == 0 {
            Rgb([150, 190, 230])
        } else {
            Rgb([36, 48, 72])
        };
        put_px(img, x, y, c);
    }
}

/// 一个像素八分音符。
fn note(img: &mut RgbImage, x: i32, y: i32, c: Rgb<u8>) {
    fill(img, x, y + 3, 2, 2, c);
    fill(img, x + 1, y, 1, 3, c);
    put_px(img, x + 2, y, c);
    put_px(img, x + 3, y + 1, c);
}

/// 四角小星星。
fn sparkle(img: &mut RgbImage, x: i32, y: i32, big: bool, c: Rgb<u8>) {
    put_px(img, x, y, WHITE);
    for d in 1..=(if big { 2 } else { 1 }) {
        put_px(img, x + d, y, c);
        put_px(img, x - d, y, c);
        put_px(img, x, y + d, c);
        put_px(img, x, y - d, c);
    }
}

fn render_hud(img: &mut RgbImage, scene: BotPose, frame: u8) {
    let l = Layout::new(img);
    let f = i32::from(frame % FRAMES);
    let (cx, cy) = (l.cx(), l.cy());
    let r = (l.avail() * 3 / 10).max(8);
    let accent = match scene {
        BotPose::ApprovalWave => AMBER,
        BotPose::CelebrateDone | BotPose::Happy => MINT,
        BotPose::Sad => Rgb([80, 120, 220]),
        BotPose::Listening | BotPose::Surprised | BotPose::Talking => CYAN,
        _ => RED,
    };
    hud_background(img, accent);
    match scene {
        BotPose::IdleBlink => {
            let k = [170, 230, 255, 210][f as usize];
            draw_moss_optical_eye(img, cx, cy, r, scale_rgb(RED, k), RED);
            let span = (cx - r - 14).max(4);
            for x in 8..8 + span {
                put_px(img, x, cy, DIM);
                put_px(img, l.w - 1 - x, cy, DIM);
            }
            let dash = 8 + (f * span / 4);
            fill(img, dash, cy - 1, 4, 3, CYAN);
            fill(img, l.w - 4 - dash, cy - 1, 4, 3, CYAN);
        }
        BotPose::Listening => {
            draw_moss_optical_eye(img, cx, cy, r, RED, MINT);
            for k in 0..3 {
                let step = (f + k * 2) % 6;
                let rr = r + 4 + step * 4;
                let fade = 255 - step as u32 * 36;
                ring(img, cx, cy, rr, 1, 2, scale_rgb(MINT, fade));
            }
        }
        BotPose::Thinking => {
            draw_moss_optical_eye(img, cx, cy, r, scale_rgb(AMBER, 200), AMBER);
            let rr = r + 8;
            for seg in 0..12 {
                let active = (seg - f * 3).rem_euclid(12) < 3;
                let ang = seg as f32 * std::f32::consts::TAU / 12.0;
                let c = if active { AMBER } else { DIM };
                for t in 0..3 {
                    let rad = (rr + t) as f32;
                    for w in -1..=1 {
                        let a2 = ang + w as f32 * 0.07;
                        put_px(
                            img,
                            cx + (rad * a2.cos()) as i32,
                            cy + (rad * a2.sin()) as i32,
                            c,
                        );
                    }
                }
            }
        }
        BotPose::Talking => {
            let k = if f % 2 == 0 { 255 } else { 190 };
            draw_moss_optical_eye(img, cx, cy, r, scale_rgb(RED, k), CYAN);
            const HEIGHTS: [i32; 8] = [3, 7, 11, 6, 9, 4, 10, 5];
            for i in 0..5 {
                let hgt = HEIGHTS[((i + f) % 8) as usize] * l.avail() / 60;
                let x = cx + r + 6 + i * 5;
                fill(img, x, cy - hgt, 3, hgt * 2 + 1, CYAN);
                fill(img, l.w - 1 - x - 2, cy - hgt, 3, hgt * 2 + 1, CYAN);
            }
        }
        BotPose::Happy => {
            draw_moss_optical_eye(img, cx, cy, r, MINT, MINT);
            for k in 0..3 {
                let y = cy + r - ((f * 5 + k * 10) % (r * 2).max(10));
                for side in [cx - r - 14, cx + r + 10] {
                    line(img, side, y + 3, side + 2, y, MINT);
                    line(img, side + 2, y, side + 4, y + 3, MINT);
                }
            }
        }
        BotPose::Sad => {
            let blue = Rgb([70, 110, 210]);
            draw_moss_optical_eye(img, cx, cy, r, scale_rgb(blue, 180), blue);
            for col in 0..8 {
                let x = 10 + col * (l.w - 20) / 7;
                if (x - cx).abs() <= r + 2 {
                    continue;
                }
                let y = l.top + (f * 6 + col * 13) % l.avail().max(1);
                fill(img, x, y, 1, 4, scale_rgb(blue, 200));
            }
        }
        BotPose::Surprised => {
            draw_moss_optical_eye(img, cx, cy, r, WHITE, CYAN);
            ring(
                img,
                cx,
                cy,
                r + 4 + f * 3,
                1,
                1,
                scale_rgb(WHITE, 255 - f as u32 * 50),
            );
            if f % 2 == 0 {
                draw_text_5x7(img, cx - r - 18, cy - 10, 3, "!", WHITE);
                draw_text_5x7(img, cx + r + 6, cy - 10, 3, "!", WHITE);
            }
        }
        BotPose::GogglesEngrave => {
            for x in (6..l.w - 6).step_by(2) {
                put_px(img, x, cy, Rgb([90, 30, 30]));
            }
            for y in (l.top..l.h - l.bottom).step_by(2) {
                put_px(img, cx, y, Rgb([90, 30, 30]));
            }
            draw_moss_optical_eye(img, cx, cy, r, RED, RED);
            for i in 0..6 {
                let (dx, dy) = DIRS[((i + f) % 8) as usize];
                let rad = r + 4 + (i * 3 + f * 2) % 8;
                put_px(img, cx + dx * rad / 16, cy + dy * rad / 16, AMBER);
            }
            draw_text_5x7(img, 8, l.top + 2, 1, "LASER ON", RED);
        }
        BotPose::ApprovalWave => {
            border(
                img,
                2,
                if f % 2 == 0 {
                    AMBER
                } else {
                    Rgb([110, 80, 20])
                },
            );
            draw_moss_optical_eye(img, cx, cy, r, AMBER, AMBER);
            centered_text(img, cx, cy + r + 4, 1, "WAIT PC AUTH", AMBER);
        }
        BotPose::MusicGroove => {
            disc(img, cx, cy, r, Rgb([18, 18, 26]));
            for g in [r - 3, r - 7, r - 11] {
                if g > r / 3 {
                    ring(img, cx, cy, g, 1, 1, Rgb([42, 42, 58]));
                }
            }
            let (dx, dy) = DIRS[(f * 2 % 8) as usize];
            line(
                img,
                cx,
                cy,
                cx + dx * r / 16,
                cy + dy * r / 16,
                Rgb([130, 130, 150]),
            );
            disc(img, cx, cy, r / 3, RED);
            disc(img, cx, cy, 1, Rgb([10, 10, 14]));
            for k in 0..2 {
                let y = cy + 10 - ((f * 6 + k * 14) % 28);
                note(img, cx - r - 14, y, CYAN);
                note(img, cx + r + 8, y + 7, PINK);
            }
        }
        BotPose::CelebrateDone => {
            draw_moss_optical_eye(img, cx, cy, r, MINT, MINT);
            for (i, (dx, dy)) in DIRS.iter().enumerate() {
                if (i as i32 + f) % 2 == 0 {
                    line(
                        img,
                        cx + dx * (r + 3) / 16,
                        cy + dy * (r + 3) / 16,
                        cx + dx * (r + 11) / 16,
                        cy + dy * (r + 11) / 16,
                        if i % 4 == 0 { AMBER } else { MINT },
                    );
                }
            }
            centered_text(img, cx, cy + r + 13, 1, "DONE", MINT);
        }
        BotPose::HaltedShield => {
            hazard_corners(img, 14, frame);
            border(img, 2, if f % 2 == 0 { RED } else { Rgb([90, 20, 18]) });
            draw_moss_optical_eye(img, cx, cy, r, RED, RED);
            centered_text(img, cx, cy + r + 4, 1, "HALT", RED);
        }
    }
}

fn render_bot(img: &mut RgbImage, scene: BotPose, frame: u8) {
    let l = Layout::new(img);
    let f = i32::from(frame % FRAMES);
    bot_background(img, frame);
    let s = (l.avail() / 26).clamp(1, 4);
    let size = 26 * s;
    let ox = (l.w - size) / 2;
    let oy = l.top + (l.avail() - size) / 2;
    // 脚下投影
    let floor = oy + size;
    for x in (ox + 4 * s)..(ox + 22 * s) {
        put_px(img, x, floor, Rgb([24, 32, 50]));
        put_px(img, x, floor + 1, Rgb([18, 24, 38]));
    }
    let right = ox + size + 3;
    match scene {
        BotPose::Listening => {
            let (ex, ey) = (ox + 24 * s, oy + 12 * s);
            for k in 0..=(f % 4) {
                let rad = 5 + k * 5;
                for dy in -rad..=rad {
                    let dx = ((rad * rad - dy * dy) as f32).sqrt() as i32;
                    if dy.abs() < rad * 3 / 4 {
                        put_px(img, ex + dx, ey + dy, scale_rgb(MINT, 255 - k as u32 * 50));
                    }
                }
            }
        }
        BotPose::Thinking => {
            let bx = right.min(l.w - 30);
            let by = oy + 2 * s;
            disc(img, bx - 4, by + 18, 1, DIM);
            disc(img, bx, by + 13, 2, DIM);
            fill(img, bx + 2, by, 24, 10, Rgb([28, 38, 58]));
            for d in 0..(f % 4) {
                fill(img, bx + 6 + d * 6, by + 4, 2, 2, AMBER);
            }
        }
        BotPose::Talking => {
            let lens = [[10, 6, 8], [6, 10, 4], [8, 4, 10], [4, 8, 6]][f as usize];
            for (i, len) in lens.iter().enumerate() {
                let y = oy + 8 * s + i as i32 * 5;
                fill(img, right, y, *len, 2, CYAN);
            }
        }
        BotPose::Happy => {
            for i in 0..6 {
                let (x, y) = star_at(i + 40, l.w, l.avail());
                sparkle(
                    img,
                    x,
                    l.top + y,
                    (i + f) % 3 == 0,
                    if i % 2 == 0 { AMBER } else { PINK },
                );
            }
        }
        BotPose::Sad => {
            let cx = (ox - 18).max(10);
            let cy = oy + 6;
            disc(img, cx, cy, 5, Rgb([60, 72, 96]));
            disc(img, cx + 6, cy + 1, 4, Rgb([60, 72, 96]));
            disc(img, cx - 6, cy + 2, 3, Rgb([60, 72, 96]));
            for k in 0..3 {
                let y = cy + 6 + (f * 4 + k * 7) % 20;
                fill(img, cx - 4 + k * 4, y, 1, 3, Rgb([110, 170, 255]));
            }
        }
        BotPose::Surprised => {
            if f % 2 == 0 {
                draw_text_5x7(img, right, oy + 2 * s, 3, "!", AMBER);
            }
        }
        BotPose::GogglesEngrave => {
            let px = right.min(l.w - 26);
            let py = floor - 8;
            fill(img, px, py, 24, 6, Rgb([150, 108, 60]));
            fill(img, px, py, 24, 1, Rgb([190, 140, 84]));
            let burn = 4 + f * 5;
            fill(img, px + 2, py + 3, burn, 1, Rgb([60, 30, 14]));
            put_px(img, px + 2 + burn, py + 3, AMBER);
        }
        BotPose::ApprovalWave => {
            border(img, 2, if f % 2 == 0 { AMBER } else { Rgb([90, 68, 20]) });
        }
        BotPose::MusicGroove => {
            for k in 0..2 {
                let y = oy + size / 2 - ((f * 6 + k * 12) % 24);
                note(img, (ox - 14).max(2), y, CYAN);
                note(img, right, y + 6, PINK);
            }
        }
        BotPose::CelebrateDone => {
            for i in 0..16 {
                let (x, y0) = star_at(i + 80, l.w, l.h);
                let y = (y0 + f * 5).rem_euclid(l.h);
                let c = [AMBER, MINT, PINK, CYAN][(i % 4) as usize];
                fill(img, x, y, 2, 1, c);
            }
        }
        BotPose::HaltedShield => {
            hazard_corners(img, 14, frame);
            border(img, 2, if f % 2 == 0 { RED } else { Rgb([90, 20, 18]) });
        }
        BotPose::IdleBlink => {}
    }
    draw_goggle_bot(img, ox, oy, s, scene, frame);
}

/// 画一帧半分辨率场景。
pub fn render_scene(style: ScreenCardStyle, scene: BotPose, frame: u8, w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w.max(16), h.max(16));
    match style {
        ScreenCardStyle::MossHud => render_hud(&mut img, scene, frame),
        ScreenCardStyle::GoggleBot => render_bot(&mut img, scene, frame),
    }
    img
}

/// 2 倍最近邻放大（像素画放大后边缘仍清楚）。
pub fn upscale2(img: &RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    RgbImage::from_fn(w * 2, h * 2, |x, y| *img.get_pixel(x / 2, y / 2))
}

fn encode_gif(frames: &[RgbImage]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(16 * 1024);
    {
        let mut encoder = GifEncoder::new_with_speed(&mut out, 10);
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|e| format!("设置画风 GIF 循环失败：{e}"))?;
        for rgb in frames {
            let rgba = RgbaImage::from_fn(rgb.width(), rgb.height(), |x, y| {
                let p = rgb.get_pixel(x, y);
                Rgba([p[0], p[1], p[2], 255])
            });
            encoder
                .encode_frame(Frame::from_parts(
                    rgba,
                    0,
                    0,
                    Delay::from_numer_denom_ms(FRAME_MS, 1),
                ))
                .map_err(|e| format!("编码画风 GIF 帧失败：{e}"))?;
        }
    }
    Ok(out)
}

/// 一个场景的循环 GIF：`full` 为真时放大到屏幕大小（写进资源包），否则是半分辨率预览（语音中心展示）。
pub fn scene_gif(
    style: ScreenCardStyle,
    scene: BotPose,
    logical: (u32, u32),
    full: bool,
) -> Result<Vec<u8>, String> {
    let frames: Vec<RgbImage> = (0..FRAMES)
        .map(|f| {
            let img = render_scene(style, scene, f, logical.0, logical.1);
            if full { upscale2(&img) } else { img }
        })
        .collect();
    encode_gif(&frames)
}

/// 雕刻时刀路预览卡（屏幕中央半屏）四周的全屏框，半分辨率画。
pub fn render_backdrop(style: ScreenCardStyle, w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w.max(16), h.max(16));
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let (wx, wy, ww, wh) = (iw / 4, ih / 4, iw / 2, ih / 2);
    match style {
        ScreenCardStyle::MossHud => {
            hud_background(&mut img, RED);
            for x in wx - 2..wx + ww + 2 {
                put_px(&mut img, x, wy - 2, Rgb([120, 36, 32]));
                put_px(&mut img, x, wy + wh + 1, Rgb([120, 36, 32]));
            }
            for y in wy - 2..wy + wh + 2 {
                put_px(&mut img, wx - 2, y, Rgb([120, 36, 32]));
                put_px(&mut img, wx + ww + 1, y, Rgb([120, 36, 32]));
            }
            for i in 0..6 {
                let y = wy + i * wh / 6 + 2;
                let c = if i < 4 { RED } else { DIM };
                fill(&mut img, wx - 10, y, 5, (wh / 6 - 3).max(1), c);
                fill(&mut img, wx + ww + 5, y, 5, (wh / 6 - 3).max(1), c);
            }
            centered_text(&mut img, iw / 2, wy - 11, 1, "MOSS LASER", RED);
        }
        ScreenCardStyle::GoggleBot => {
            bot_background(&mut img, 0);
            for x in wx - 2..wx + ww + 2 {
                put_px(&mut img, x, wy - 2, MINT);
                put_px(&mut img, x, wy + wh + 1, MINT);
            }
            for y in wy - 2..wy + wh + 2 {
                put_px(&mut img, wx - 2, y, MINT);
                put_px(&mut img, wx + ww + 1, y, MINT);
            }
            let s = ((wx - 6) / 26).clamp(1, 2);
            draw_goggle_bot(
                &mut img,
                (wx - 26 * s) / 2,
                ih / 2 - 13 * s,
                s,
                BotPose::GogglesEngrave,
                0,
            );
            centered_text(&mut img, iw / 2, wy - 11, 1, "MOSS BOT", MINT);
        }
    }
    img
}

/// 转成官方生成器同款 RGB565 背景图（28 字节 `lv_image_dsc_t` 头 + 小端像素）。
pub fn rgb565_image(img: &RgbImage) -> Vec<u8> {
    const LV_IMAGE_HEADER_MAGIC: u32 = 0x19;
    const LV_COLOR_FORMAT_RGB565: u32 = 0x12;
    let (w, h) = img.dimensions();
    let stride = w * 2;
    let data_len = (stride * h) as usize;
    let mut out = Vec::with_capacity(28 + data_len);
    out.extend_from_slice(&((LV_COLOR_FORMAT_RGB565 << 8) | LV_IMAGE_HEADER_MAGIC).to_le_bytes());
    out.extend_from_slice(&((h << 16) | w).to_le_bytes());
    out.extend_from_slice(&stride.to_le_bytes());
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    out.extend_from_slice(&28u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for p in img.pixels() {
        let v = (u16::from(p[0] >> 3) << 11) | (u16::from(p[1] >> 2) << 5) | u16::from(p[2] >> 3);
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// 生成写进资源包的全屏画风：两套画风 × 12 个场景的全屏 GIF、标准表情名别名、整屏背景与文字颜色。
/// `default_style` 决定标准表情名（含固件待命固定的 `neutral`）与背景用哪套画风。
pub fn build_screen_extras(
    default_style: ScreenCardStyle,
    screen: (u32, u32),
) -> Result<ScreenExtras, String> {
    let logical = logical_size(screen);
    let (fw, fh) = (logical.0 * 2, logical.1 * 2);
    let mut extras = ScreenExtras::default();
    for style in [ScreenCardStyle::MossHud, ScreenCardStyle::GoggleBot] {
        for scene in SCENES {
            let name = emotion_name(style, scene);
            let file = format!("{name}.gif");
            extras.entries.push(AssetEntry {
                name: file.clone(),
                data: scene_gif(style, scene, logical, true)?,
                width: fw as u16,
                height: fh as u16,
            });
            extras
                .emoji_collection
                .push(json!({ "name": name, "file": file }));
        }
    }
    for name in STANDARD_EMOTIONS {
        let file = format!("{}.gif", emotion_name(default_style, alias_scene(name)));
        extras
            .emoji_collection
            .push(json!({ "name": name, "file": file }));
    }
    let backdrop = upscale2(&render_backdrop(default_style, logical.0, logical.1));
    extras.entries.push(AssetEntry {
        name: BACKDROP_FILE.into(),
        data: rgb565_image(&backdrop),
        width: fw as u16,
        height: fh as u16,
    });
    let (text, bg) = match default_style {
        ScreenCardStyle::MossHud => ("#D8E6F5", "#070A10"),
        ScreenCardStyle::GoggleBot => ("#E6F2FF", "#0B0F19"),
    };
    let theme =
        json!({ "text_color": text, "background_color": bg, "background_image": BACKDROP_FILE });
    extras.skin = Some(json!({ "light": theme.clone(), "dark": theme }));
    Ok(extras)
}

/// 语音中心展示用：两套画风 × 12 个场景的半分辨率（160×120）GIF data URL，进程内只编码一次。
pub fn cached_scene_gif_catalog() -> &'static BTreeMap<&'static str, BTreeMap<&'static str, String>>
{
    static CACHE: OnceLock<BTreeMap<&'static str, BTreeMap<&'static str, String>>> =
        OnceLock::new();
    CACHE.get_or_init(|| {
        let logical = logical_size((320, 240));
        [ScreenCardStyle::MossHud, ScreenCardStyle::GoggleBot]
            .into_iter()
            .map(|style| {
                let scenes = SCENES
                    .into_iter()
                    .filter_map(|scene| {
                        let gif = scene_gif(style, scene, logical, false).ok()?;
                        Some((
                            scene.slug(),
                            format!("data:image/gif;base64,{}", base64_encode(&gif)),
                        ))
                    })
                    .collect();
                (style.as_str(), scenes)
            })
            .collect()
    })
}

/// 没刷画风包的彩屏试看：把场景第 `frame` 帧画成预览卡（`self.screen.preview_image`，半屏）。
pub fn scene_card_png(
    style: ScreenCardStyle,
    scene: BotPose,
    frame: u8,
    w: u32,
    h: u32,
) -> Result<Vec<u8>, String> {
    let img = render_scene(style, scene, frame, w, h);
    let mut out = Vec::with_capacity(4096);
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|_| "编码场景卡失败".to_owned())?;
    Ok(out)
}

/// 资源包里 `index.json` 的表情条目（测试用）。
pub fn emoji_names(extras: &ScreenExtras) -> Vec<String> {
    extras
        .emoji_collection
        .iter()
        .filter_map(|e| e["name"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests;
