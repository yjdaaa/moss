//! 「小 MOSS · 防激光护目镜小机器人」像素精灵、双画风卡片元素与 GIF89a 动图生成器。
//!
//! 完全在服务端内存中按帧生成（零设备 Flash 分区擦写风险，不触碰设备原厂 `srmodels.bin` 唤醒词分区）：
//! - 在立创·实战派 ESP32-S3（`320×240` → `160×120` 1:1 预览框）上，随 4 秒节拍切换动作帧；
//! - 在桌面端语音中心，直接输出标准无限循环 GIF89a 动图供实时预览与展示。

use std::io::Cursor;
use std::sync::OnceLock;

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, Rgb, RgbImage, Rgba, RgbaImage};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenCardStyle {
    #[default]
    MossHud,
    GoggleBot,
}

impl ScreenCardStyle {
    pub fn from_setting(raw: &str) -> Self {
        match raw.trim() {
            "goggle_bot" => Self::GoggleBot,
            _ => Self::MossHud,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MossHud => "moss_hud",
            Self::GoggleBot => "goggle_bot",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotPose {
    /// 待机眨眼：护目镜架在额头，青色大眼睛眨动 + 呼吸微浮
    IdleBlink,
    /// 激光作业：拉下红色防激光护目镜遮住双眼，手持迷你激光笔溅出火花
    GogglesEngrave,
    /// 待电脑审批：护目镜架在额头，小手高举琥珀色确认牌挥手
    ApprovalWave,
    /// 听歌摇摆：戴上荧光青耳机左右摇摆，飘起像素音符
    MusicGroove,
    /// 完工欢呼：月牙笑眼 + 粉红脸颊 + 竖大拇指 + 闪烁星星
    CelebrateDone,
    /// 停机护盾：戴好护目镜举起停机小盾牌
    HaltedShield,
    /// n70 聆听：手拢在耳边，耳罩亮起
    Listening,
    /// n70 思考：眼睛往右上看，手托下巴
    Thinking,
    /// n70 说话：嘴巴一张一合
    Talking,
    /// n70 开心：月牙笑眼，双手举起
    Happy,
    /// n70 难过：耷拉眼，掉小眼泪
    Sad,
    /// n70 惊讶：圆眼 + O 形嘴
    Surprised,
}

impl BotPose {
    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug.trim().to_ascii_lowercase().as_str() {
            "idle" => Some(Self::IdleBlink),
            "engrave" => Some(Self::GogglesEngrave),
            "approval" => Some(Self::ApprovalWave),
            "music" => Some(Self::MusicGroove),
            "celebrate" => Some(Self::CelebrateDone),
            "halted" => Some(Self::HaltedShield),
            "listen" => Some(Self::Listening),
            "think" => Some(Self::Thinking),
            "talk" => Some(Self::Talking),
            "happy" => Some(Self::Happy),
            "sad" => Some(Self::Sad),
            "surprised" => Some(Self::Surprised),
            _ => None,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::IdleBlink => "idle",
            Self::GogglesEngrave => "engrave",
            Self::ApprovalWave => "approval",
            Self::MusicGroove => "music",
            Self::CelebrateDone => "celebrate",
            Self::HaltedShield => "halted",
            Self::Listening => "listen",
            Self::Thinking => "think",
            Self::Talking => "talk",
            Self::Happy => "happy",
            Self::Sad => "sad",
            Self::Surprised => "surprised",
        }
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            Self::IdleBlink => "待机眨眼",
            Self::GogglesEngrave => "戴护目镜雕刻",
            Self::ApprovalWave => "待电脑审批",
            Self::MusicGroove => "听歌摇摆",
            Self::CelebrateDone => "完工欢呼",
            Self::HaltedShield => "急停护盾",
            Self::Listening => "竖耳聆听",
            Self::Thinking => "托腮思考",
            Self::Talking => "张嘴说话",
            Self::Happy => "开心举手",
            Self::Sad => "难过掉泪",
            Self::Surprised => "惊讶圆眼",
        }
    }

    pub fn moss_hud_label_zh(self) -> &'static str {
        match self {
            Self::IdleBlink => "1:1 刀路预览与红眼待机",
            Self::GogglesEngrave => "太空舱实时雕刻仪表盘",
            Self::ApprovalWave => "太空舱待电脑审批门禁",
            Self::MusicGroove => "太空舱黑胶像素唱机",
            Self::CelebrateDone => "太空舱 5 秒完工徽章",
            Self::HaltedShield => "太空舱安全急停徽章",
            Self::Listening => "太空舱声波聆听",
            Self::Thinking => "太空舱扫描思考",
            Self::Talking => "太空舱波形说话",
            Self::Happy => "太空舱青色愉悦",
            Self::Sad => "太空舱蓝色低落",
            Self::Surprised => "太空舱白光惊讶",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct GoggleBotGifItem {
    pub id: &'static str,
    pub label: &'static str,
    pub data_url: String,
}

fn fill_rect(
    img: &mut RgbImage,
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    scale: i32,
    ox: i32,
    oy: i32,
    color: Rgb<u8>,
) {
    let sx0 = ox + x0 * scale;
    let sy0 = oy + y0 * scale;
    let sw = w * scale;
    let sh = h * scale;
    let iw = img.width() as i32;
    let ih = img.height() as i32;
    for yy in sy0.max(0)..(sy0 + sh).min(ih) {
        for xx in sx0.max(0)..(sx0 + sw).min(iw) {
            img.put_pixel(xx as u32, yy as u32, color);
        }
    }
}

fn dot(img: &mut RgbImage, x: i32, y: i32, scale: i32, ox: i32, oy: i32, color: Rgb<u8>) {
    fill_rect(img, x, y, 1, 1, scale, ox, oy, color);
}

/// 在 `(ox, oy)` 左上角按 `26×26 * scale` 绘制一帧「小 MOSS · 护目镜小机器人」。
pub fn draw_goggle_bot(img: &mut RgbImage, ox: i32, oy: i32, scale: i32, pose: BotPose, frame: u8) {
    let s = scale.max(1);
    let f = frame % 4;
    let dx = match pose {
        BotPose::MusicGroove => match f {
            0 => -1,
            2 => 1,
            _ => 0,
        },
        _ => 0,
    };
    let dy = match pose {
        BotPose::IdleBlink | BotPose::ApprovalWave | BotPose::Listening => {
            i32::from(f == 1 || f == 2)
        }
        BotPose::CelebrateDone | BotPose::Happy => -i32::from(f % 2 == 1),
        BotPose::Surprised => -i32::from(f == 0),
        _ => 0,
    };
    let bx = ox + dx * s;
    let by = oy + dy * s;

    let shell = Rgb([34, 46, 68]);
    let shell_hi = Rgb([74, 98, 136]);
    let shell_dark = Rgb([20, 28, 42]);
    let visor = Rgb([10, 15, 24]);
    let moss_red = Rgb([255, 64, 54]);
    let moss_red_hi = Rgb([255, 148, 136]);
    let goggle_rim = Rgb([255, 84, 64]);
    let goggle_lens = Rgb([255, 146, 56]);
    let goggle_glint = Rgb([255, 246, 230]);
    let cyan_eye = Rgb([72, 250, 210]);
    let cheek_pink = Rgb([255, 108, 148]);
    let gold = Rgb([255, 208, 68]);
    let neon_cyan = Rgb([0, 226, 255]);

    // 1. 头顶小天线 + MOSS 呼吸信号球
    fill_rect(img, 12, 2, 2, 3, s, bx, by, Rgb([120, 142, 174]));
    let ant_color = if f.is_multiple_of(2) {
        moss_red
    } else {
        moss_red_hi
    };
    fill_rect(img, 11, 0, 4, 2, s, bx, by, ant_color);
    dot(img, 12, 0, s, bx, by, Rgb([255, 240, 240]));

    // 2. 圆角头部外壳 (x: 4..22, y: 5..19)
    fill_rect(img, 6, 5, 14, 14, s, bx, by, shell);
    fill_rect(img, 5, 6, 16, 12, s, bx, by, shell);
    fill_rect(img, 4, 7, 18, 10, s, bx, by, shell);
    // 顶部高光倒角与底部暗边
    fill_rect(img, 6, 5, 14, 1, s, bx, by, shell_hi);
    fill_rect(img, 5, 6, 2, 1, s, bx, by, shell_hi);
    fill_rect(img, 6, 18, 14, 1, s, bx, by, shell_dark);

    // 两侧小耳罩（听歌模式变为荧光青大耳机）
    if pose == BotPose::MusicGroove {
        fill_rect(img, 2, 9, 2, 7, s, bx, by, neon_cyan);
        fill_rect(img, 22, 9, 2, 7, s, bx, by, neon_cyan);
        dot(img, 2, 10, s, bx, by, Rgb([220, 255, 255]));
        dot(img, 22, 10, s, bx, by, Rgb([220, 255, 255]));
    } else if pose == BotPose::Listening {
        // 聆听：两侧耳罩亮起青色
        let ear = if f.is_multiple_of(2) {
            neon_cyan
        } else {
            cyan_eye
        };
        fill_rect(img, 2, 10, 2, 5, s, bx, by, ear);
        fill_rect(img, 22, 10, 2, 5, s, bx, by, ear);
    } else {
        fill_rect(img, 3, 10, 1, 5, s, bx, by, shell_hi);
        fill_rect(img, 22, 10, 1, 5, s, bx, by, shell_hi);
    }

    // 3. 面部深色显示屏面罩 (x: 6..20, y: 9..17)
    fill_rect(img, 6, 9, 14, 8, s, bx, by, visor);

    let goggles_down = matches!(pose, BotPose::GogglesEngrave | BotPose::HaltedShield);
    if goggles_down {
        // 防激光护目镜拉下遮住双眼 (y: 10..15)
        fill_rect(img, 4, 11, 2, 2, s, bx, by, Rgb([62, 76, 102]));
        fill_rect(img, 20, 11, 2, 2, s, bx, by, Rgb([62, 76, 102]));
        // 左右镜框 + 鼻梁
        fill_rect(img, 6, 10, 6, 5, s, bx, by, goggle_rim);
        fill_rect(img, 14, 10, 6, 5, s, bx, by, goggle_rim);
        fill_rect(img, 12, 11, 2, 2, s, bx, by, goggle_rim);
        // 橙红防激光滤光镜片 + 动态扫光高光
        fill_rect(img, 7, 11, 4, 3, s, bx, by, goggle_lens);
        fill_rect(img, 15, 11, 4, 3, s, bx, by, goggle_lens);
        let glint_x = (f as i32) % 3;
        dot(img, 7 + glint_x, 11, s, bx, by, goggle_glint);
        dot(img, 8 + glint_x, 12, s, bx, by, goggle_glint);
        dot(img, 15 + glint_x, 11, s, bx, by, goggle_glint);
        dot(img, 16 + glint_x, 12, s, bx, by, goggle_glint);
        // 专注小嘴
        fill_rect(img, 12, 16, 2, 1, s, bx, by, cyan_eye);
    } else {
        // 防激光护目镜帅气架在额头上 (y: 5..9)
        fill_rect(img, 5, 7, 16, 1, s, bx, by, Rgb([52, 66, 92]));
        fill_rect(img, 6, 5, 6, 4, s, bx, by, goggle_rim);
        fill_rect(img, 14, 5, 6, 4, s, bx, by, goggle_rim);
        fill_rect(img, 12, 6, 2, 2, s, bx, by, goggle_rim);
        fill_rect(img, 7, 6, 4, 2, s, bx, by, goggle_lens);
        fill_rect(img, 15, 6, 4, 2, s, bx, by, goggle_lens);
        dot(img, 7, 6, s, bx, by, goggle_glint);
        dot(img, 15, 6, s, bx, by, goggle_glint);

        // 面罩上的可爱发光像素眼与粉红腮红
        match pose {
            BotPose::CelebrateDone | BotPose::MusicGroove
                if f % 2 == 1 || pose == BotPose::CelebrateDone =>
            {
                // 弯弯笑眼 ^ ^
                dot(img, 8, 13, s, bx, by, cyan_eye);
                dot(img, 9, 12, s, bx, by, cyan_eye);
                dot(img, 10, 13, s, bx, by, cyan_eye);
                dot(img, 15, 13, s, bx, by, cyan_eye);
                dot(img, 16, 12, s, bx, by, cyan_eye);
                dot(img, 17, 13, s, bx, by, cyan_eye);
                // 开心微笑
                fill_rect(img, 12, 15, 2, 1, s, bx, by, cyan_eye);
            }
            BotPose::IdleBlink if f == 2 => {
                // 眨眼帧 - -
                fill_rect(img, 8, 13, 3, 1, s, bx, by, cyan_eye);
                fill_rect(img, 15, 13, 3, 1, s, bx, by, cyan_eye);
            }
            BotPose::Happy => {
                // 月牙笑眼 + 大笑嘴
                dot(img, 8, 13, s, bx, by, cyan_eye);
                dot(img, 9, 12, s, bx, by, cyan_eye);
                dot(img, 10, 13, s, bx, by, cyan_eye);
                dot(img, 15, 13, s, bx, by, cyan_eye);
                dot(img, 16, 12, s, bx, by, cyan_eye);
                dot(img, 17, 13, s, bx, by, cyan_eye);
                fill_rect(img, 11, 15, 4, 1, s, bx, by, cyan_eye);
                dot(img, 10, 14, s, bx, by, cyan_eye);
                dot(img, 15, 14, s, bx, by, cyan_eye);
            }
            BotPose::Sad => {
                // 耷拉眼（外低内高）+ 下撇嘴 + 一滴往下掉的眼泪
                fill_rect(img, 8, 13, 3, 1, s, bx, by, cyan_eye);
                dot(img, 10, 12, s, bx, by, cyan_eye);
                fill_rect(img, 15, 13, 3, 1, s, bx, by, cyan_eye);
                dot(img, 15, 12, s, bx, by, cyan_eye);
                fill_rect(img, 12, 15, 2, 1, s, bx, by, cyan_eye);
                dot(img, 11, 16, s, bx, by, cyan_eye);
                dot(img, 14, 16, s, bx, by, cyan_eye);
                let tear_y = 14 + (f as i32 % 3);
                dot(img, 8, tear_y, s, bx, by, Rgb([110, 170, 255]));
            }
            BotPose::Surprised => {
                // 圆圈眼 + O 形嘴
                fill_rect(img, 8, 11, 3, 3, s, bx, by, cyan_eye);
                fill_rect(img, 15, 11, 3, 3, s, bx, by, cyan_eye);
                dot(img, 9, 12, s, bx, by, visor);
                dot(img, 16, 12, s, bx, by, visor);
                fill_rect(img, 12, 15, 2, 2, s, bx, by, cyan_eye);
            }
            BotPose::Thinking => {
                // 眼睛往右上看 + 歪嘴
                fill_rect(img, 9, 11, 2, 2, s, bx, by, cyan_eye);
                fill_rect(img, 16, 11, 2, 2, s, bx, by, cyan_eye);
                dot(img, 10, 11, s, bx, by, Rgb([255, 255, 255]));
                dot(img, 17, 11, s, bx, by, Rgb([255, 255, 255]));
                fill_rect(img, 13, 15, 2, 1, s, bx, by, cyan_eye);
            }
            BotPose::Talking => {
                // 大眼睛 + 一张一合的嘴
                fill_rect(img, 8, 11, 3, 3, s, bx, by, cyan_eye);
                fill_rect(img, 15, 11, 3, 3, s, bx, by, cyan_eye);
                dot(img, 8, 11, s, bx, by, Rgb([255, 255, 255]));
                dot(img, 15, 11, s, bx, by, Rgb([255, 255, 255]));
                if f.is_multiple_of(2) {
                    fill_rect(img, 11, 15, 4, 2, s, bx, by, cyan_eye);
                    fill_rect(img, 12, 15, 2, 1, s, bx, by, visor);
                } else {
                    fill_rect(img, 12, 15, 2, 1, s, bx, by, cyan_eye);
                }
            }
            _ => {
                // 水灵大眼睛（带白色高光点）
                fill_rect(img, 8, 11, 3, 3, s, bx, by, cyan_eye);
                fill_rect(img, 15, 11, 3, 3, s, bx, by, cyan_eye);
                dot(img, 8, 11, s, bx, by, Rgb([255, 255, 255]));
                dot(img, 15, 11, s, bx, by, Rgb([255, 255, 255]));
                fill_rect(img, 12, 15, 2, 1, s, bx, by, cyan_eye);
            }
        }
        // 粉红像素腮红
        fill_rect(img, 6, 15, 2, 1, s, bx, by, cheek_pink);
        fill_rect(img, 18, 15, 2, 1, s, bx, by, cheek_pink);
    }

    // 4. 圆润小身体 + 胸口标志性 MOSS 红核 (x: 7..19, y: 19..24)
    fill_rect(img, 7, 19, 12, 5, s, bx, by, Rgb([42, 56, 82]));
    fill_rect(img, 8, 19, 10, 1, s, bx, by, shell_hi);
    // 胸口 MOSS 光学小红眼核心
    fill_rect(img, 12, 20, 2, 2, s, bx, by, moss_red);
    dot(img, 12, 20, s, bx, by, moss_red_hi);
    // 小短腿
    fill_rect(img, 8, 24, 3, 2, s, bx, by, shell_dark);
    fill_rect(img, 15, 24, 3, 2, s, bx, by, shell_dark);

    // 5. 按姿态绘制小手臂与道具动效
    match pose {
        BotPose::IdleBlink => {
            fill_rect(img, 5, 20, 2, 3, s, bx, by, shell_hi);
            fill_rect(img, 19, 20, 2, 3, s, bx, by, shell_hi);
        }
        BotPose::GogglesEngrave => {
            fill_rect(img, 5, 20, 2, 3, s, bx, by, shell_hi);
            // 右手握持激光雕刻头并发射青绿光束与跳动火花
            fill_rect(img, 19, 19, 3, 2, s, bx, by, shell_hi);
            fill_rect(img, 22, 19, 2, 2, s, bx, by, neon_cyan);
            for step in 0..3 {
                dot(img, 23 + step, 21 + step, s, bx, by, cyan_eye);
            }
            if f.is_multiple_of(2) {
                dot(img, 24, 20, s, bx, by, gold);
                dot(img, 22, 23, s, bx, by, gold);
            } else {
                dot(img, 25, 21, s, bx, by, Rgb([255, 255, 255]));
                dot(img, 23, 24, s, bx, by, gold);
            }
        }
        BotPose::ApprovalWave => {
            fill_rect(img, 5, 20, 2, 3, s, bx, by, shell_hi);
            // 右手高举琥珀色小警示牌轻轻挥动
            let wave_y = if f.is_multiple_of(2) { 14 } else { 13 };
            fill_rect(img, 19, wave_y + 3, 2, 3, s, bx, by, shell_hi);
            fill_rect(img, 20, wave_y - 2, 6, 5, s, bx, by, gold);
            fill_rect(img, 21, wave_y - 1, 4, 3, s, bx, by, Rgb([30, 22, 12]));
            dot(img, 22, wave_y, s, bx, by, gold);
            dot(img, 23, wave_y, s, bx, by, moss_red);
        }
        BotPose::MusicGroove => {
            let arm_y = if f.is_multiple_of(2) { 18 } else { 20 };
            fill_rect(img, 5, arm_y, 2, 3, s, bx, by, shell_hi);
            fill_rect(img, 19, 38 - arm_y, 2, 3, s, bx, by, shell_hi);
            // 飘动的像素八分音符
            let ny = 4 - (f as i32 % 3);
            fill_rect(img, 0, ny + 2, 2, 2, s, bx, by, neon_cyan);
            fill_rect(img, 1, ny - 1, 1, 3, s, bx, by, neon_cyan);
            dot(img, 2, ny - 1, s, bx, by, neon_cyan);
            let ny2 = 3 - ((f as i32 + 1) % 3);
            fill_rect(img, 24, ny2 + 2, 2, 2, s, bx, by, cheek_pink);
            fill_rect(img, 25, ny2 - 1, 1, 3, s, bx, by, cheek_pink);
        }
        BotPose::CelebrateDone => {
            fill_rect(img, 5, 18, 2, 3, s, bx, by, shell_hi);
            // 右手竖起大拇指 + 四周闪烁小星星
            fill_rect(img, 19, 17, 3, 3, s, bx, by, shell_hi);
            fill_rect(img, 20, 15, 2, 2, s, bx, by, gold);
            let star_c = if f.is_multiple_of(2) { gold } else { cyan_eye };
            dot(img, 2, 4, s, bx, by, star_c);
            dot(img, 1, 5, s, bx, by, star_c);
            dot(img, 3, 5, s, bx, by, star_c);
            dot(img, 2, 6, s, bx, by, star_c);
            dot(img, 23, 3, s, bx, by, cyan_eye);
            dot(img, 24, 4, s, bx, by, gold);
        }
        BotPose::HaltedShield => {
            fill_rect(img, 5, 20, 2, 3, s, bx, by, shell_hi);
            // 举起红琥珀停机盾牌
            fill_rect(img, 19, 16, 6, 6, s, bx, by, moss_red);
            fill_rect(img, 20, 17, 4, 4, s, bx, by, gold);
            fill_rect(img, 21, 18, 2, 2, s, bx, by, moss_red);
        }
        BotPose::Listening => {
            fill_rect(img, 5, 20, 2, 3, s, bx, by, shell_hi);
            // 右手拢在耳边
            fill_rect(img, 19, 17, 2, 3, s, bx, by, shell_hi);
            fill_rect(img, 21, 14, 2, 4, s, bx, by, shell_hi);
        }
        BotPose::Thinking => {
            fill_rect(img, 5, 20, 2, 3, s, bx, by, shell_hi);
            // 右手托下巴
            fill_rect(img, 18, 19, 2, 2, s, bx, by, shell_hi);
            fill_rect(img, 15, 17, 4, 2, s, bx, by, shell_hi);
        }
        BotPose::Talking => {
            // 说话时小手跟着比划
            let left_y = if f.is_multiple_of(2) { 19 } else { 20 };
            fill_rect(img, 5, left_y, 2, 3, s, bx, by, shell_hi);
            fill_rect(img, 19, 39 - left_y, 2, 3, s, bx, by, shell_hi);
        }
        BotPose::Happy => {
            // 双手高举
            let arm_y = if f.is_multiple_of(2) { 15 } else { 16 };
            fill_rect(img, 4, arm_y, 2, 4, s, bx, by, shell_hi);
            fill_rect(img, 20, arm_y, 2, 4, s, bx, by, shell_hi);
        }
        BotPose::Sad => {
            // 小手垂下
            fill_rect(img, 5, 21, 2, 3, s, bx, by, shell_dark);
            fill_rect(img, 19, 21, 2, 3, s, bx, by, shell_dark);
        }
        BotPose::Surprised => {
            // 双手张开
            fill_rect(img, 3, 18, 3, 2, s, bx, by, shell_hi);
            fill_rect(img, 20, 18, 3, 2, s, bx, by, shell_hi);
        }
    }
}

/// 绘制萌系圆角点阵边框（用于 `goggle_bot` 卡片风格）。
pub fn draw_cute_frame(img: &mut RgbImage, accent: Rgb<u8>, secondary: Rgb<u8>) {
    let w = img.width() as i32;
    let h = img.height() as i32;
    let soft_border = Rgb([42, 54, 78]);
    for x in 2..(w - 2) {
        img.put_pixel(x as u32, 0, soft_border);
        img.put_pixel(x as u32, (h - 1) as u32, soft_border);
    }
    for y in 2..(h - 2) {
        img.put_pixel(0, y as u32, soft_border);
        img.put_pixel((w - 1) as u32, y as u32, soft_border);
    }
    for &(cx, cy, dx, dy) in &[
        (1, 1, 1, 1),
        (w - 2, 1, -1, 1),
        (1, h - 2, 1, -1),
        (w - 2, h - 2, -1, -1),
    ] {
        for i in 0..7 {
            let x = (cx + i * dx).clamp(0, w - 1) as u32;
            let y = (cy + i * dy).clamp(0, h - 1) as u32;
            img.put_pixel(x, cy as u32, accent);
            img.put_pixel(cx as u32, y, accent);
        }
        img.put_pixel((cx + 2 * dx) as u32, (cy + 2 * dy) as u32, secondary);
    }
}

/// 生成单姿态 4 帧无限循环 GIF89a 动图（96×96 像素，零外部文件依赖）。
pub fn encode_goggle_bot_gif(pose: BotPose) -> Result<Vec<u8>, String> {
    let size = 96u32;
    let mut out = Vec::with_capacity(4096);
    {
        let mut encoder = GifEncoder::new_with_speed(&mut out, 10);
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|e| format!("设置 GIF 循环失败：{e}"))?;
        for f in 0..4u8 {
            let mut rgb = RgbImage::from_pixel(size, size, Rgb([13, 17, 26]));
            draw_cute_frame(&mut rgb, Rgb([72, 250, 210]), Rgb([255, 118, 98]));
            // 脚下柔和投影椭圆
            for sx in 28..68 {
                rgb.put_pixel(sx, 86, Rgb([22, 30, 46]));
                rgb.put_pixel(sx, 87, Rgb([18, 24, 38]));
            }
            draw_goggle_bot(&mut rgb, 9, 8, 3, pose, f);
            let mut rgba = RgbaImage::new(size, size);
            for (x, y, p) in rgb.enumerate_pixels() {
                rgba.put_pixel(x, y, Rgba([p[0], p[1], p[2], 255]));
            }
            let frame = Frame::from_parts(rgba, 0, 0, Delay::from_numer_denom_ms(240, 1));
            encoder
                .encode_frame(frame)
                .map_err(|e| format!("编码 GIF 帧失败：{e}"))?;
        }
    }
    Ok(out)
}

pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// 每个姿势一格的 GIF 原始字节缓存槽（编码结果确定，失败也只试一次）。
fn goggle_bot_gif_slot(pose: BotPose) -> &'static OnceLock<Option<Vec<u8>>> {
    static SLOTS: [OnceLock<Option<Vec<u8>>>; 12] = [const { OnceLock::new() }; 12];
    let index = match pose {
        BotPose::IdleBlink => 0,
        BotPose::GogglesEngrave => 1,
        BotPose::ApprovalWave => 2,
        BotPose::MusicGroove => 3,
        BotPose::CelebrateDone => 4,
        BotPose::HaltedShield => 5,
        BotPose::Listening => 6,
        BotPose::Thinking => 7,
        BotPose::Talking => 8,
        BotPose::Happy => 9,
        BotPose::Sad => 10,
        BotPose::Surprised => 11,
    };
    &SLOTS[index]
}

/// 进程级缓存的单个姿势 GIF 原始字节（2026-11-05：HTTP 路由与展台共用，每个姿势只编码一次）。
/// 首次调用会同步编码 4 帧 GIF，异步上下文里应放到 `spawn_blocking`，或先用 [`goggle_bot_gif_if_encoded`]。
pub fn cached_goggle_bot_gif(pose: BotPose) -> Option<&'static [u8]> {
    goggle_bot_gif_slot(pose)
        .get_or_init(|| encode_goggle_bot_gif(pose).ok())
        .as_deref()
}

/// 已编码过时直接取缓存（不阻塞）；尚未编码返回 `None`。
pub(crate) fn goggle_bot_gif_if_encoded(pose: BotPose) -> Option<Option<&'static [u8]>> {
    goggle_bot_gif_slot(pose).get().map(Option::as_deref)
}

/// 进程级缓存全套 5 个「小 MOSS · 护目镜小机器人」动态 GIF（供语音中心实时展台直接渲染）。
pub fn cached_goggle_bot_gifs() -> &'static [GoggleBotGifItem] {
    static CACHE: OnceLock<Vec<GoggleBotGifItem>> = OnceLock::new();
    CACHE.get_or_init(|| {
        [
            BotPose::IdleBlink,
            BotPose::GogglesEngrave,
            BotPose::ApprovalWave,
            BotPose::MusicGroove,
            BotPose::CelebrateDone,
        ]
        .into_iter()
        .filter_map(|pose| {
            let gif = cached_goggle_bot_gif(pose)?;
            Some(GoggleBotGifItem {
                id: pose.slug(),
                label: pose.label_zh(),
                data_url: format!("data:image/gif;base64,{}", base64_encode(gif)),
            })
        })
        .collect()
    })
}

/// 渲染一张可直接投送到小智 `320×240` 彩屏的「护目镜小机器人桌宠名片」PNG（用于用户在界面点击试看）。
pub fn make_bot_showcase_card_png(
    pose: BotPose,
    frame: u8,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    let cw = canvas_w.clamp(96, 240);
    let ch = canvas_h.clamp(80, 240);
    let mut canvas = RgbImage::from_pixel(cw, ch, Rgb([12, 16, 26]));
    draw_cute_frame(&mut canvas, Rgb([72, 250, 210]), Rgb([255, 118, 98]));
    draw_goggle_bot(&mut canvas, 12, (ch as i32 - 56) / 2, 2, pose, frame);
    let mut out = Vec::with_capacity(4096);
    canvas
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|_| "编码小机器人展示卡失败".to_owned())?;
    Ok(out)
}
