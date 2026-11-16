//! G-code 文本解析：字段、移动、运动范围、运动段与功率标尺。

use super::Move;
use std::f64::consts::TAU;
use std::fmt::Write;

pub(super) fn words(line: &str) -> Vec<(char, f64)> {
    let line = line.split(';').next().unwrap_or("");
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            while i < bytes.len() && bytes[i] != b')' {
                i += 1;
            }
            i += 1;
            continue;
        }
        if bytes[i].is_ascii_alphabetic() {
            let letter = bytes[i].to_ascii_uppercase() as char;
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || matches!(bytes[i], b'+' | b'-' | b'.' | b'e' | b'E'))
            {
                i += 1;
            }
            if i > start + 1
                && let Ok(value) = line[start + 1..i].parse()
            {
                out.push((letter, value));
            }
        } else {
            i += 1;
        }
    }
    out
}

pub fn parse_moves(text: &str) -> Vec<Move> {
    let mut motion = 0;
    let mut feed = None;
    let mut power = None;
    let mut out = Vec::new();
    for line in text.lines() {
        let ws = words(line);
        for &(letter, value) in &ws {
            if letter == 'G'
                && matches!(value as i32, 0 | 1)
                && (value - value.round()).abs() < 1e-9
            {
                motion = value as i32;
            } else if letter == 'F' {
                feed = Some(value);
            } else if letter == 'S' {
                power = Some(value);
            }
        }
        if ws
            .iter()
            .any(|&(l, v)| l == 'G' && (v - v.round()).abs() < 1e-9 && matches!(v as i32, 0 | 1))
            && ws.iter().any(|&(l, _)| l == 'X' || l == 'Y')
        {
            out.push(Move {
                x: ws.iter().rev().find(|(l, _)| *l == 'X').map(|(_, v)| *v),
                y: ws.iter().rev().find(|(l, _)| *l == 'Y').map(|(_, v)| *v),
                feed_rate: feed,
                power,
                rapid: motion == 0,
            });
        }
    }
    out
}

/// 刀路实际走到的范围（mm，相对任务开始时激光头所在位置；起点本身计入）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionExtents {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
}

impl MotionExtents {
    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    fn include(&mut self, x: f64, y: f64) {
        self.min_x = self.min_x.min(x);
        self.max_x = self.max_x.max(x);
        self.min_y = self.min_y.min(y);
        self.max_y = self.max_y.max(y);
    }
}

/// 计算激光头在整份 G-code 中走过的 X/Y 范围，用于发送前与机器行程比对（2026-11-06）。
/// - 跟踪 G90/G91、G20/G21；`G92` 只改坐标原点不移动（生成的任务以 `G92 X0 Y0` 把当前位置设为原点）；
/// - 快速移动也计入（激光头同样会走到）；G2/G3 圆弧计入经过的最左 / 右 / 上 / 下点，`R` 写法按以弦中点为心、
///   半径 |R| 的圆保守计入；
/// - `G53` / `G28` / `G30` 等以机床坐标或回参考点移动的行无法从文件本身得知终点，返回 `Err` 让调用方拒绝。
///
/// 没有任何 X/Y 移动时返回 `Ok(None)`。
pub fn motion_extents(text: &str) -> Result<Option<MotionExtents>, String> {
    let (mut px, mut py) = (0.0_f64, 0.0_f64); // 物理位置（相对开始点）
    let (mut ox, mut oy) = (0.0_f64, 0.0_f64); // 工件坐标原点相对开始点的偏移
    let mut absolute = true;
    let mut unit = 1.0;
    let mut motion = 0;
    let mut extents = MotionExtents {
        min_x: 0.0,
        max_x: 0.0,
        min_y: 0.0,
        max_y: 0.0,
    };
    let mut moved = false;
    for (index, line) in text.lines().enumerate() {
        let ws = words(line);
        if ws.is_empty() {
            continue;
        }
        let g_codes: Vec<f64> = ws
            .iter()
            .filter(|(l, _)| *l == 'G')
            .map(|(_, v)| *v)
            .collect();
        let has = |code: f64| g_codes.iter().any(|v| (v - code).abs() < 1e-9);
        let word = |letter: char| ws.iter().rev().find(|(l, _)| *l == letter).map(|(_, v)| *v);
        if has(20.0) {
            unit = 25.4;
        }
        if has(21.0) {
            unit = 1.0;
        }
        if has(90.0) {
            absolute = true;
        }
        if has(91.0) {
            absolute = false;
        }
        let (x, y) = (word('X'), word('Y'));
        if has(92.0) {
            // G92 Xa：当前位置在新坐标系里记为 a
            if let Some(v) = x {
                ox = px - v * unit;
            }
            if let Some(v) = y {
                oy = py - v * unit;
            }
            if x.is_none() && y.is_none() {
                ox = px;
                oy = py;
            }
            continue;
        }
        if [53.0, 28.0, 30.0, 10.0].iter().any(|code| has(*code))
            && (x.is_some() || y.is_some() || has(28.0) || has(30.0))
        {
            return Err(format!(
                "第 {} 行使用机床坐标、回参考点或改坐标系指令（{}），无法从文件计算加工范围",
                index + 1,
                line.trim()
            ));
        }
        for code in &g_codes {
            if [0.0, 1.0, 2.0, 3.0].iter().any(|m| (code - m).abs() < 1e-9) {
                motion = *code as i32;
            }
        }
        if x.is_none() && y.is_none() {
            continue;
        }
        let (sx, sy) = (px, py);
        let target = |current: f64, origin: f64, value: Option<f64>| match value {
            None => current,
            Some(v) if absolute => origin + v * unit,
            Some(v) => current + v * unit,
        };
        let (tx, ty) = (target(px, ox, x), target(py, oy, y));
        if !(tx.is_finite() && ty.is_finite()) {
            return Err(format!("第 {} 行坐标不是有限数", index + 1));
        }
        moved = true;
        extents.include(tx, ty);
        if motion == 2 || motion == 3 {
            if let Some(r) = word('R') {
                let (cx, cy, radius) = ((sx + tx) / 2.0, (sy + ty) / 2.0, r.abs() * unit);
                extents.include(cx - radius, cy - radius);
                extents.include(cx + radius, cy + radius);
            } else {
                let (cx, cy) = (
                    sx + word('I').unwrap_or(0.0) * unit,
                    sy + word('J').unwrap_or(0.0) * unit,
                );
                let radius = (sx - cx).hypot(sy - cy);
                let start = (sy - cy).atan2(sx - cx);
                let end = (ty - cy).atan2(tx - cx);
                let full = (sx - tx).abs() < 1e-9 && (sy - ty).abs() < 1e-9;
                let tau = std::f64::consts::TAU;
                // 逆时针（G3）从 start 扫到 end；顺时针（G2）反向
                let (from, sweep) = if motion == 3 {
                    (
                        start,
                        if full {
                            tau
                        } else {
                            (end - start).rem_euclid(tau)
                        },
                    )
                } else {
                    (
                        end,
                        if full {
                            tau
                        } else {
                            (start - end).rem_euclid(tau)
                        },
                    )
                };
                for quadrant in 0..4 {
                    let angle = f64::from(quadrant) * std::f64::consts::FRAC_PI_2;
                    if (angle - from).rem_euclid(tau) <= sweep + 1e-12 {
                        extents.include(cx + radius * angle.cos(), cy + radius * angle.sin());
                    }
                }
            }
        }
        px = tx;
        py = ty;
    }
    Ok(moved.then_some(extents))
}

pub fn write_move(m: &Move) -> String {
    let mut line = if m.rapid { "G0" } else { "G1" }.to_owned();
    if let Some(x) = m.x {
        write!(line, " X{x:.3}").unwrap();
    }
    if let Some(y) = m.y {
        write!(line, " Y{y:.3}").unwrap();
    }
    if let Some(feed) = m.feed_rate {
        write!(line, " F{feed:.0}").unwrap();
    }
    if let Some(power) = m.power {
        write!(line, " S{power:.0}").unwrap();
    }
    line
}

/// 一段运动（时间估算用）。
#[derive(Debug, Clone, Copy)]
pub(super) struct MotionSegment {
    /// 起点（以开始时激光头位置为原点，已计入 G92）
    pub(super) start: (f64, f64),
    /// 圆弧（G2/G3）：只记了端点
    pub(super) arc: bool,
    pub(super) dx: f64,
    pub(super) dy: f64,
    pub(super) length: f64,
    /// 指令速度（mm/min，已按快移速度封顶）
    pub(super) rate: f64,
    pub(super) laser: bool,
    /// 这一段开始前机器必须停住（M3/M4/M5 切换、G4 停顿）
    pub(super) stop_before: bool,
    /// G4 停顿秒数（长度为 0 的停顿段）
    pub(super) dwell: f64,
}

/// 逐行解析出运动段（模态 G0/G1/G2/G3、G90/G91、G92、M3/M4/M5、F、S、G4）。
pub(super) fn motion_segments(text: &str, rapid_rate: f64) -> Vec<MotionSegment> {
    let mut x = 0.0;
    let mut y = 0.0;
    let mut feed = 0.0;
    let mut power = 0.0;
    let mut motion = 0;
    let mut spindle = 5;
    let mut absolute = true;
    let mut ox = 0.0;
    let mut oy = 0.0;
    let mut pending_stop = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let ws = words(line);
        if ws.is_empty() {
            continue;
        }
        for &(l, v) in &ws {
            match l {
                'G' if v.round() == 0.0
                    || v.round() == 1.0
                    || v.round() == 2.0
                    || v.round() == 3.0 =>
                {
                    motion = v as i32
                }
                'G' if v.round() == 90.0 => absolute = true,
                'G' if v.round() == 91.0 => absolute = false,
                'M' if (3.0..=5.0).contains(&v) => {
                    // GRBL 在主轴 / 激光状态切换时强制停住
                    if v as i32 != spindle {
                        pending_stop = true;
                    }
                    spindle = v as i32
                }
                'F' => feed = v,
                'S' => power = v,
                _ => {}
            }
        }
        if ws.iter().any(|&(l, v)| l == 'G' && v.round() == 92.0) {
            if let Some(v) = ws.iter().find(|(l, _)| *l == 'X').map(|(_, v)| *v) {
                ox = x - v;
            }
            if let Some(v) = ws.iter().find(|(l, _)| *l == 'Y').map(|(_, v)| *v) {
                oy = y - v;
            }
            continue;
        }
        let px = x;
        let py = y;
        for &(l, v) in &ws {
            if l == 'X' {
                x = if absolute { v + ox } else { x + v };
            }
            if l == 'Y' {
                y = if absolute { v + oy } else { y + v };
            }
        }
        if ws.iter().any(|&(l, v)| l == 'G' && v.round() == 4.0) {
            let pause = ws
                .iter()
                .rev()
                .find(|(l, _)| *l == 'P' || *l == 'S')
                .map_or(0.0, |(_, v)| *v);
            out.push(MotionSegment {
                start: (x, y),
                arc: false,
                dx: 0.0,
                dy: 0.0,
                length: 0.0,
                rate: 0.0,
                laser: false,
                stop_before: true,
                dwell: pause,
            });
            pending_stop = true;
            continue;
        }
        let mut distance = (x - px).hypot(y - py);
        if motion == 2 || motion == 3 {
            if let (Some(i), Some(j)) = (
                ws.iter().rev().find(|(l, _)| *l == 'I').map(|(_, v)| *v),
                ws.iter().rev().find(|(l, _)| *l == 'J').map(|(_, v)| *v),
            ) {
                let radius = (px - (px + i)).hypot(py - (py + j));
                let start = (py - (py + j)).atan2(px - (px + i));
                let end = (y - (py + j)).atan2(x - (px + i));
                let sweep = if motion == 2 {
                    (start - end).rem_euclid(TAU)
                } else {
                    (end - start).rem_euclid(TAU)
                };
                if radius > 0.0 && sweep > 0.0 {
                    distance = radius * sweep;
                }
            } else if let Some(r) = ws.iter().rev().find(|(l, _)| *l == 'R').map(|(_, v)| *v) {
                let radius = r.abs();
                let chord = (x - px).hypot(y - py);
                if radius > 0.0 && chord <= 2.0 * radius {
                    let mut sweep = 2.0 * (chord / (2.0 * radius)).asin();
                    if r < 0.0 {
                        sweep = TAU - sweep;
                    }
                    if sweep > 0.0 {
                        distance = radius * sweep;
                    }
                }
            }
        }
        if distance == 0.0 {
            continue;
        }
        let rate = if motion == 0 {
            rapid_rate
        } else {
            feed.min(rapid_rate)
        };
        if rate <= 0.0 {
            continue;
        }
        out.push(MotionSegment {
            start: (px, py),
            arc: motion == 2 || motion == 3,
            dx: x - px,
            dy: y - py,
            length: distance,
            rate,
            laser: power > 0.0 && (spindle == 3 || (spindle == 4 && motion != 0)),
            stop_before: std::mem::take(&mut pending_stop),
            dwell: 0.0,
        });
    }
    out
}

/// 检测 G-code 中声明的功率标尺（默认为 1000.0）
pub fn detect_gcode_power_scale(text: &str) -> Option<f64> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(pos) = trimmed.find("$30=") {
            let rest = &trimmed[pos + 4..];
            let num_str: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(val) = num_str.parse::<f64>()
                && val > 0.0
            {
                return Some(val);
            }
        }
    }
    None
}

/// 按目标标尺（例如 255 或 1000）等比例重算 G-code 中的 S 功率参数
pub fn rescale_gcode_power(text: &str, from_scale: f64, to_scale: f64) -> String {
    if (from_scale - to_scale).abs() < 1e-6 || from_scale <= 0.0 || to_scale <= 0.0 {
        return text.to_string();
    }
    let ratio = to_scale / from_scale;
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let (content, comment) = match line.find(';') {
            Some(idx) => (&line[..idx], Some(&line[idx..])),
            None => (line, None),
        };

        let mut new_content = String::with_capacity(content.len());
        let mut chars = content.chars().peekable();
        while let Some(c) = chars.next() {
            if (c == 'S' || c == 's')
                && chars
                    .peek()
                    .is_some_and(|next| next.is_ascii_digit() || *next == '+' || *next == '-')
            {
                let mut num_str = String::new();
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_digit() || next == '.' || next == '+' || next == '-' {
                        num_str.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
                if let Ok(s_val) = num_str.parse::<f64>() {
                    let new_s = (s_val * ratio).round().clamp(0.0, to_scale) as i64;
                    new_content.push('S');
                    new_content.push_str(&new_s.to_string());
                } else {
                    new_content.push('S');
                    new_content.push_str(&num_str);
                }
            } else {
                new_content.push(c);
            }
        }

        out.push_str(&new_content);
        if let Some(comm) = comment {
            let from_pattern = format!("$30={}", from_scale as i64);
            let to_pattern = format!("$30={}", to_scale as i64);
            if comm.contains(&from_pattern) {
                out.push_str(&comm.replace(&from_pattern, &to_pattern));
            } else {
                out.push_str(comm);
            }
        }
        out.push('\n');
    }
    out
}
