//! 无摄像头两点对位（2026-11-16，对齐 LightBurn Print and Cut）。
//!
//! 用户把激光头（红光点）点动到材料上两个实物标记点，MOSS 读下两处位置；这里按设计上对应的两个参考点
//! 求出“旋转 + 平移（+ 可选等比缩放）”的相似变换，并把已生成的 G-code 刀路整体变换过去。
//!
//! 坐标约定：设计参考点与刀路都用“相对任务开始时激光头位置”的坐标（与预览的 `frame_mm` 相同）；
//! 实测点由调用方换算成相对新任务起点（第 2 个标记点）的坐标。变换后的刀路仍以开头的 `G92 X0 Y0`
//! 声明激光头当前位置为原点，因此发送门禁、SHA-256 与行程复核照旧适用。

use std::fmt;

/// 两个参考点（设计与实测）至少相距多少毫米：太近时旋转角误差被放大（0.1 mm 误差在 10 mm 上约 0.6°）。
pub const MIN_POINT_DISTANCE_MM: f64 = 10.0;
/// 不允许缩放时，实测距离与设计距离之比允许偏离 1 的比例（超出多半是点错了标记）。
pub const RIGID_SCALE_TOLERANCE: f64 = 0.05;
/// 允许等比缩放时的缩放范围。
pub const MIN_SCALE: f64 = 0.8;
pub const MAX_SCALE: f64 = 1.25;

/// 平面相似变换 `(x, y) -> (a·x − b·y + tx, b·x + a·y + ty)`，`a = s·cosθ`，`b = s·sinθ`。
/// 不含镜像（行列式恒为正），圆弧方向因此保持不变。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    pub a: f64,
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Similarity {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub fn apply(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            self.a * x - self.b * y + self.tx,
            self.b * x + self.a * y + self.ty,
        )
    }

    /// 只作用于向量（圆弧圆心偏移 I/J 等），不加平移。
    pub fn apply_vector(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (self.a * x - self.b * y, self.b * x + self.a * y)
    }

    pub fn scale(&self) -> f64 {
        self.a.hypot(self.b)
    }

    /// 旋转角（度，逆时针为正，范围 (−180, 180]）。
    pub fn rotation_deg(&self) -> f64 {
        self.b.atan2(self.a).to_degrees()
    }
}

/// 两点拟合的结果与质量指标。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegistrationFit {
    pub transform: Similarity,
    pub rotation_deg: f64,
    pub scale: f64,
    /// 是否按实测距离等比缩放（`allow_scale`）
    pub scale_applied: bool,
    pub design_distance_mm: f64,
    pub measured_distance_mm: f64,
    /// 实测距离 / 设计距离
    pub distance_ratio: f64,
    /// 变换后两个设计点与实测点的最大偏差（mm）；允许缩放时为 0（两点正好确定相似变换）。
    pub residual_mm: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RegistrationError {
    /// 坐标缺失或不是有限数
    InvalidPoints,
    /// 两个设计点或两个实测点相距太近
    PointsTooClose { design_mm: f64, measured_mm: f64 },
    /// 不允许缩放时实测距离与设计距离差得太多（多半点错了标记）
    ScaleMismatch { ratio: f64 },
    /// 允许缩放但超出 [`MIN_SCALE`]..=[`MAX_SCALE`]
    ScaleOutOfRange { ratio: f64 },
}

impl RegistrationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPoints => "registration_points_invalid",
            Self::PointsTooClose { .. } => "registration_points_too_close",
            Self::ScaleMismatch { .. } => "registration_scale_mismatch",
            Self::ScaleOutOfRange { .. } => "registration_scale_out_of_range",
        }
    }
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPoints => write!(f, "对位点坐标缺失或不是有效数字"),
            Self::PointsTooClose {
                design_mm,
                measured_mm,
            } => write!(
                f,
                "两个对位点相距太近（设计 {design_mm:.1} mm、实测 {measured_mm:.1} mm），至少要相距 {MIN_POINT_DISTANCE_MM:.0} mm，越远越准"
            ),
            Self::ScaleMismatch { ratio } => write!(
                f,
                "实测两点距离是设计距离的 {:.1}%，相差超过 {:.0}%：多半是对错了标记点；确认是打印缩放造成的可勾选“允许等比缩放”",
                ratio * 100.0,
                RIGID_SCALE_TOLERANCE * 100.0
            ),
            Self::ScaleOutOfRange { ratio } => write!(
                f,
                "实测两点距离是设计距离的 {:.1}%，超出允许的缩放范围 {:.0}% ~ {:.0}%，请检查标记点",
                ratio * 100.0,
                MIN_SCALE * 100.0,
                MAX_SCALE * 100.0
            ),
        }
    }
}

impl std::error::Error for RegistrationError {}

/// 由两对点求相似变换，把 `design[i]` 映射到 `measured[i]`。
///
/// - `allow_scale = false`（默认）：只旋转和平移，按最小二乘让两点的中点重合，残差为两点距离差的一半；
///   实测距离与设计距离相差超过 [`RIGID_SCALE_TOLERANCE`] 时拒绝。
/// - `allow_scale = true`：另按实测距离等比缩放（[`MIN_SCALE`]..=[`MAX_SCALE`]），两点精确重合。
pub fn fit_two_point(
    design: [(f64, f64); 2],
    measured: [(f64, f64); 2],
    allow_scale: bool,
) -> Result<RegistrationFit, RegistrationError> {
    if design
        .iter()
        .chain(measured.iter())
        .any(|p| !p.0.is_finite() || !p.1.is_finite())
    {
        return Err(RegistrationError::InvalidPoints);
    }
    let d = (design[1].0 - design[0].0, design[1].1 - design[0].1);
    let t = (measured[1].0 - measured[0].0, measured[1].1 - measured[0].1);
    let design_distance = d.0.hypot(d.1);
    let measured_distance = t.0.hypot(t.1);
    if design_distance < MIN_POINT_DISTANCE_MM || measured_distance < MIN_POINT_DISTANCE_MM {
        return Err(RegistrationError::PointsTooClose {
            design_mm: design_distance,
            measured_mm: measured_distance,
        });
    }
    let ratio = measured_distance / design_distance;
    let scale = if allow_scale {
        if !(MIN_SCALE..=MAX_SCALE).contains(&ratio) {
            return Err(RegistrationError::ScaleOutOfRange { ratio });
        }
        ratio
    } else {
        if (ratio - 1.0).abs() > RIGID_SCALE_TOLERANCE {
            return Err(RegistrationError::ScaleMismatch { ratio });
        }
        1.0
    };
    let theta = t.1.atan2(t.0) - d.1.atan2(d.0);
    let (a, b) = (scale * theta.cos(), scale * theta.sin());
    let mut transform = Similarity {
        a,
        b,
        tx: 0.0,
        ty: 0.0,
    };
    // 中点对中点：最小二乘意义下的最优平移
    let design_mid = (
        (design[0].0 + design[1].0) / 2.0,
        (design[0].1 + design[1].1) / 2.0,
    );
    let measured_mid = (
        (measured[0].0 + measured[1].0) / 2.0,
        (measured[0].1 + measured[1].1) / 2.0,
    );
    let mapped_mid = transform.apply_vector(design_mid);
    transform.tx = measured_mid.0 - mapped_mid.0;
    transform.ty = measured_mid.1 - mapped_mid.1;
    let residual = design
        .iter()
        .zip(measured.iter())
        .map(|(&p, &m)| {
            let q = transform.apply(p);
            (q.0 - m.0).hypot(q.1 - m.1)
        })
        .fold(0.0_f64, f64::max);
    Ok(RegistrationFit {
        transform,
        rotation_deg: transform.rotation_deg(),
        scale,
        scale_applied: allow_scale,
        design_distance_mm: design_distance,
        measured_distance_mm: measured_distance,
        distance_ratio: ratio,
        residual_mm: residual,
    })
}

/// G-code 无法安全变换的原因（行号从 1 起）。
#[derive(Debug, Clone, PartialEq)]
pub struct GcodeTransformError {
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for GcodeTransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}", self.reason)
        } else {
            write!(f, "第 {} 行{}", self.line, self.reason)
        }
    }
}

impl std::error::Error for GcodeTransformError {}

/// 变换后的刀路与统计。
#[derive(Debug, Clone, PartialEq)]
pub struct TransformedGcode {
    pub gcode: String,
    /// 改写了坐标的移动行数
    pub moves: usize,
    /// 其中圆弧行数
    pub arcs: usize,
}

/// 去掉 `;` 行尾注释与 `( … )` 注释。
fn strip_comments(line: &str) -> String {
    let line = line.split(';').next().unwrap_or("");
    let mut out = String::with_capacity(line.len());
    let mut depth = 0usize;
    for ch in line.chars() {
        match ch {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// 把一行拆成（字母，数值原文，数值）；遇到无法识别的字符（变量 `#`、表达式 `[`、`$` 系统命令等）返回 `Err`。
fn tokenize(code: &str) -> Result<Vec<(char, String, f64)>, String> {
    let chars: Vec<char> = code.chars().filter(|c| !c.is_whitespace()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let letter = chars[i];
        if !letter.is_ascii_alphabetic() {
            return Err(format!("含无法识别的内容“{letter}”"));
        }
        i += 1;
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_digit() || matches!(chars[i], '+' | '-' | '.'))
        {
            i += 1;
        }
        let raw: String = chars[start..i].iter().collect();
        if raw.is_empty() {
            let next = chars.get(i).copied().unwrap_or(letter);
            return Err(format!("含无法识别的内容“{letter}{next}”"));
        }
        let value = raw
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!("“{letter}”后面不是有效数字"))?;
        out.push((letter.to_ascii_uppercase(), raw, value));
    }
    Ok(out)
}

fn fmt_coord(value: f64) -> String {
    let rounded = (value * 10_000.0).round() / 10_000.0;
    // 去掉 -0
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    let text = format!("{rounded:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// 允许出现在可变换刀路里的 G 代码：直线 / 圆弧 / 暂停 / XY 平面 / 毫米 / 绝对与相对 / 改原点 / 每分钟进给。
const ALLOWED_G: [f64; 11] = [0.0, 1.0, 2.0, 3.0, 4.0, 17.0, 21.0, 90.0, 91.0, 92.0, 94.0];

/// 把整份 G-code 按相似变换改写（变换作用于“相对任务开始时激光头位置”的物理坐标）。
///
/// 输出以 `G21` / `G90` / `G92 X0 Y0` 开头（激光头开始位置即原点），之后每条移动都写成绝对坐标并同时带 X、Y；
/// 原文件的 `G92` 只用于跟踪原点、整行不输出（改过 Z 原点后再移动 Z 则拒绝），`G91` 相对写法换算成绝对坐标；
/// 圆弧的 I/J 随之旋转缩放、`R` 乘以缩放比例，方向不变。F、S、M、Z 等其他字保持原样。
///
/// 以下情况拒绝而不是猜测：英制 `G20`、机床坐标 / 回参考点 / 改坐标系（`G53` `G28` `G30` `G10` `G54`~`G59`）、
/// 非 XY 平面（`G18` `G19`）、其他 G 代码、额外轴或 `K`、`$` 系统命令（含 `$J=` 点动）、变量与表达式、
/// 相对模式下的 Z 移动。
pub fn transform_gcode(
    text: &str,
    transform: &Similarity,
) -> Result<TransformedGcode, GcodeTransformError> {
    let fail = |line: usize, reason: String| GcodeTransformError { line, reason };
    if !(transform.a.is_finite()
        && transform.b.is_finite()
        && transform.tx.is_finite()
        && transform.ty.is_finite())
        || transform.scale() <= 0.0
    {
        return Err(fail(0, "对位变换不是有效数字".into()));
    }
    let scale = transform.scale();
    let mut out: Vec<String> = vec![
        format!(
            "; MOSS two-point registration: rotate {:.4} deg, scale {:.5}, offset X{} Y{}",
            transform.rotation_deg(),
            scale,
            fmt_coord(transform.tx),
            fmt_coord(transform.ty)
        ),
        "G21".into(),
        "G90".into(),
        "G92 X0 Y0".into(),
    ];
    let (mut px, mut py) = (0.0_f64, 0.0_f64); // 物理位置（相对原任务开始点）
    let (mut ox, mut oy) = (0.0_f64, 0.0_f64); // 原文件工件坐标原点（相对原任务开始点）
    let mut absolute = true;
    let mut motion = 0_i32;
    // 原文件用 G92 改过 Z 原点：输出不再保留（`G92 Z…` 会让行程复核误判 XY 原点），之后带 Z 的移动一律拒绝
    let mut z_origin_dropped = false;
    let (mut moves, mut arcs) = (0usize, 0usize);
    for (index, raw_line) in text.lines().enumerate() {
        let line_no = index + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed == "%" {
            out.push(raw_line.to_owned());
            continue;
        }
        if trimmed.starts_with('$') {
            return Err(fail(
                line_no,
                format!("是系统命令（{trimmed}），对位刀路里不能带"),
            ));
        }
        let code = strip_comments(trimmed);
        let tokens = tokenize(&code).map_err(|e| fail(line_no, e))?;
        if tokens.is_empty() {
            out.push(raw_line.to_owned());
            continue;
        }
        let mut g_codes = Vec::new();
        for (letter, raw, value) in &tokens {
            match letter {
                'G' => {
                    if !ALLOWED_G.iter().any(|g| (g - value).abs() < 1e-9) {
                        let reason = if (*value - 20.0).abs() < 1e-9 {
                            "使用英制单位 G20，请导出毫米单位（G21）的刀路".to_owned()
                        } else if [53.0, 28.0, 30.0, 10.0, 54.0, 55.0, 56.0, 57.0, 58.0, 59.0]
                            .iter()
                            .any(|g| (g - value.trunc()).abs() < 1e-9)
                        {
                            format!("使用机床坐标、回参考点或切换坐标系（G{raw}），无法对位")
                        } else {
                            format!("含不支持对位的指令 G{raw}")
                        };
                        return Err(fail(line_no, reason));
                    }
                    g_codes.push(*value);
                }
                'M' | 'N' | 'X' | 'Y' | 'Z' | 'F' | 'S' | 'I' | 'J' | 'R' | 'P' => {}
                other => {
                    return Err(fail(
                        line_no,
                        format!("含不支持对位的字“{other}”（只支持 XY 平面刀路）"),
                    ));
                }
            }
        }
        let has = |code: f64| g_codes.iter().any(|v| (v - code).abs() < 1e-9);
        let word = |letter: char| {
            tokens
                .iter()
                .rev()
                .find(|(l, _, _)| *l == letter)
                .map(|(_, _, v)| *v)
        };
        if has(90.0) && has(91.0) {
            return Err(fail(line_no, "同时写了 G90 和 G91".into()));
        }
        if has(90.0) {
            absolute = true;
        }
        if has(91.0) {
            absolute = false;
        }
        let (x, y, z) = (word('X'), word('Y'), word('Z'));
        if has(92.0) {
            // G92 Xa：当前位置在原坐标系里记为 a；整行不输出（XY 原点已由开头的 G92 X0 Y0 定好）
            if let Some(v) = x {
                ox = px - v;
            }
            if let Some(v) = y {
                oy = py - v;
            }
            if x.is_none() && y.is_none() && z.is_none() {
                ox = px;
                oy = py;
            }
            z_origin_dropped |= z.is_some() || (x.is_none() && y.is_none());
            continue;
        }
        if z.is_some() && z_origin_dropped {
            return Err(fail(
                line_no,
                "移动 Z 轴，而前面用 G92 改过 Z 原点，无法安全对位".into(),
            ));
        }
        for code in &g_codes {
            if [0.0, 1.0, 2.0, 3.0].iter().any(|m| (code - m).abs() < 1e-9) {
                motion = *code as i32;
            }
        }
        let has_arc_words = word('I').is_some() || word('J').is_some() || word('R').is_some();
        if !absolute && z.is_some() {
            return Err(fail(
                line_no,
                "在相对模式（G91）下移动 Z 轴，无法安全换算".into(),
            ));
        }
        let changes_mode = has(91.0);
        if x.is_none() && y.is_none() {
            if has_arc_words {
                return Err(fail(line_no, "圆弧缺少终点坐标".into()));
            }
            if changes_mode {
                // 去掉 G91（输出全部是绝对坐标），其余字照抄
                let rebuilt: Vec<String> = tokens
                    .iter()
                    .filter(|(l, _, v)| !(*l == 'G' && (*v - 91.0).abs() < 1e-9) && *l != 'N')
                    .map(|(l, r, _)| format!("{l}{r}"))
                    .collect();
                if !rebuilt.is_empty() {
                    out.push(rebuilt.join(" "));
                }
            } else {
                out.push(raw_line.to_owned());
            }
            continue;
        }
        if has(4.0) {
            return Err(fail(line_no, "暂停 G4 与坐标写在同一行".into()));
        }
        let target = |current: f64, origin: f64, value: Option<f64>| match value {
            None => current,
            Some(v) if absolute => origin + v,
            Some(v) => current + v,
        };
        let (tx, ty) = (target(px, ox, x), target(py, oy, y));
        if !(tx.is_finite() && ty.is_finite()) {
            return Err(fail(line_no, "坐标不是有限数".into()));
        }
        let (nx, ny) = transform.apply((tx, ty));
        let mut parts: Vec<String> = tokens
            .iter()
            .filter(|(l, _, v)| *l == 'G' && (*v - 91.0).abs() >= 1e-9)
            .map(|(l, r, _)| format!("{l}{r}"))
            .collect();
        parts.push(format!("X{}", fmt_coord(nx)));
        parts.push(format!("Y{}", fmt_coord(ny)));
        if motion == 2 || motion == 3 {
            arcs += 1;
            if let Some(r) = word('R') {
                if word('I').is_some() || word('J').is_some() {
                    return Err(fail(line_no, "圆弧同时写了 R 和 I/J".into()));
                }
                parts.push(format!("R{}", fmt_coord(r * scale)));
            } else {
                let (i, j) = (word('I').unwrap_or(0.0), word('J').unwrap_or(0.0));
                if i == 0.0 && j == 0.0 {
                    return Err(fail(line_no, "圆弧缺少圆心 I/J 或半径 R".into()));
                }
                let (ni, nj) = transform.apply_vector((i, j));
                parts.push(format!("I{}", fmt_coord(ni)));
                parts.push(format!("J{}", fmt_coord(nj)));
            }
        } else if has_arc_words {
            return Err(fail(line_no, "直线移动里带了圆弧参数 I/J/R".into()));
        }
        for (letter, raw, _) in &tokens {
            if matches!(letter, 'M' | 'Z' | 'F' | 'S' | 'P') {
                parts.push(format!("{letter}{raw}"));
            }
        }
        out.push(parts.join(" "));
        moves += 1;
        px = tx;
        py = ty;
    }
    if moves == 0 {
        return Err(fail(0, "刀路里没有可对位的 X/Y 移动".into()));
    }
    Ok(TransformedGcode {
        gcode: out.join("\n") + "\n",
        moves,
        arcs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn close_pt(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    #[test]
    fn pure_translation_keeps_rotation_zero() {
        let fit =
            fit_two_point([(0.0, 0.0), (50.0, 0.0)], [(5.0, 3.0), (55.0, 3.0)], false).unwrap();
        assert!(close(fit.rotation_deg, 0.0));
        assert!(close(fit.scale, 1.0));
        assert!(close(fit.residual_mm, 0.0));
        assert!(close_pt(fit.transform.apply((10.0, 10.0)), (15.0, 13.0)));
    }

    #[test]
    fn ninety_degree_rotation_maps_both_points() {
        let fit =
            fit_two_point([(0.0, 0.0), (40.0, 0.0)], [(0.0, 0.0), (0.0, 40.0)], false).unwrap();
        assert!(close(fit.rotation_deg, 90.0));
        assert!(close_pt(fit.transform.apply((40.0, 0.0)), (0.0, 40.0)));
        assert!(close_pt(fit.transform.apply((0.0, 10.0)), (-10.0, 0.0)));
        assert!(fit.residual_mm < 1e-9);
    }

    #[test]
    fn scale_is_applied_only_when_allowed() {
        let design = [(0.0, 0.0), (100.0, 0.0)];
        let measured = [(0.0, 0.0), (103.0, 0.0)];
        let rigid = fit_two_point(design, measured, false).unwrap();
        assert!(close(rigid.scale, 1.0));
        // 中点对中点：两端各差 1.5 mm
        assert!(close(rigid.residual_mm, 1.5));
        assert!(close(rigid.distance_ratio, 1.03));
        let scaled = fit_two_point(design, measured, true).unwrap();
        assert!(close(scaled.scale, 1.03));
        assert!(scaled.residual_mm < 1e-9);
        assert!(close_pt(scaled.transform.apply((100.0, 0.0)), (103.0, 0.0)));
    }

    #[test]
    fn mismatched_or_out_of_range_scale_is_rejected() {
        let design = [(0.0, 0.0), (100.0, 0.0)];
        let err = fit_two_point(design, [(0.0, 0.0), (110.0, 0.0)], false).unwrap_err();
        assert_eq!(err.code(), "registration_scale_mismatch");
        assert!(fit_two_point(design, [(0.0, 0.0), (110.0, 0.0)], true).is_ok());
        let err = fit_two_point(design, [(0.0, 0.0), (130.0, 0.0)], true).unwrap_err();
        assert_eq!(err.code(), "registration_scale_out_of_range");
    }

    #[test]
    fn degenerate_points_are_rejected() {
        let err =
            fit_two_point([(0.0, 0.0), (5.0, 0.0)], [(0.0, 0.0), (5.0, 0.0)], false).unwrap_err();
        assert_eq!(err.code(), "registration_points_too_close");
        let err =
            fit_two_point([(0.0, 0.0), (0.0, 0.0)], [(0.0, 0.0), (50.0, 0.0)], true).unwrap_err();
        assert_eq!(err.code(), "registration_points_too_close");
        let err = fit_two_point(
            [(f64::NAN, 0.0), (50.0, 0.0)],
            [(0.0, 0.0), (50.0, 0.0)],
            false,
        )
        .unwrap_err();
        assert_eq!(err.code(), "registration_points_invalid");
    }

    #[test]
    fn residual_reports_half_of_distance_error_for_rigid_fit() {
        let fit = fit_two_point(
            [(0.0, 0.0), (60.0, 0.0)],
            [(10.0, 10.0), (10.0, 10.0 + 61.0)],
            false,
        )
        .unwrap();
        assert!(close(fit.rotation_deg, 90.0));
        assert!(close(fit.residual_mm, 0.5));
    }

    fn rot90() -> Similarity {
        Similarity {
            a: 0.0,
            b: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    #[test]
    fn identity_transform_preserves_motion_extents() {
        let source =
            "G21\nG90\nG92 X0 Y0 Z0\nM4 S0\nG0 X0 Y0\nG1 X10 Y0 S500 F1200\nG1 Y5\nG1 X0\nM5\nM2\n";
        let out = transform_gcode(source, &Similarity::IDENTITY).unwrap();
        assert_eq!(out.moves, 4);
        let before = crate::gcode::motion_extents(source).unwrap().unwrap();
        let after = crate::gcode::motion_extents(&out.gcode).unwrap().unwrap();
        assert_eq!(before, after);
        // 模态 Y 移动改写后同时带 X、Y
        assert!(out.gcode.contains("G1 X10 Y5"), "{}", out.gcode);
        assert!(!out.gcode.contains("G92 Z"), "{}", out.gcode);
        assert!(out.gcode.contains("S500 F1200") || out.gcode.contains("F1200"));
    }

    #[test]
    fn rotation_tracks_g92_offset_and_incremental_moves() {
        // 文字任务：开头声明激光头在 (5, 5)，即图案中心
        let source = "G92 X5 Y5\nG0 X10 Y5\nG91\nG1 X0 Y10 S300\nG90\nM5\n";
        let out = transform_gcode(source, &rot90()).unwrap();
        // 物理 (5,0) -> (0,5)；(5,10) -> (-10,5)
        assert!(out.gcode.contains("G0 X0 Y5"), "{}", out.gcode);
        assert!(out.gcode.contains("G1 X-10 Y5 S300"), "{}", out.gcode);
        assert!(!out.gcode.contains("G91"), "{}", out.gcode);
        let ext = crate::gcode::motion_extents(&out.gcode).unwrap().unwrap();
        assert!(close(ext.min_x, -10.0) && close(ext.max_y, 5.0), "{ext:?}");
    }

    #[test]
    fn arcs_rotate_center_offsets_and_scale_radius() {
        let scaled = Similarity {
            a: 0.0,
            b: 2.0,
            tx: 1.0,
            ty: 0.0,
        };
        let out = transform_gcode(
            "G0 X10 Y0\nG3 X0 Y10 I-10 J0 S100\nG2 X10 Y0 R10\n",
            &scaled,
        )
        .unwrap();
        assert_eq!(out.arcs, 2);
        // 起点 (10,0) -> (1,20)；终点 (0,10) -> (-19,0)；I/J (-10,0) -> (0,-20)
        assert!(
            out.gcode.contains("G3 X-19 Y0 I0 J-20 S100"),
            "{}",
            out.gcode
        );
        assert!(out.gcode.contains("G2 X1 Y20 R20"), "{}", out.gcode);
        // 变换后的圆弧仍是合法圆弧：起点、终点到圆心距离相等
        let center = (1.0 + 0.0, 20.0 - 20.0);
        let r_start = (1.0_f64 - center.0).hypot(20.0 - center.1);
        let r_end = (-19.0_f64 - center.0).hypot(0.0 - center.1);
        assert!(close(r_start, r_end));
    }

    #[test]
    fn unsupported_gcode_is_refused_with_line_number() {
        for (source, needle) in [
            ("G20\nG1 X1 Y1\n", "G20"),
            ("G53 G0 X0 Y0\n", "机床坐标"),
            ("G28\n", "机床坐标"),
            ("G18\nG1 X1 Z1\n", "G18"),
            ("G1 X1 A5\n", "“A”"),
            ("$J=G91 X10 F1000\n", "系统命令"),
            ("G1 X#1\n", "无法识别"),
            ("G91\nG1 X1 Z1\n", "相对模式"),
            ("G55\nG1 X1\n", "坐标系"),
        ] {
            let err = transform_gcode(source, &rot90()).unwrap_err();
            assert!(err.to_string().contains(needle), "{source}: {err}");
            assert!(err.line >= 1, "{source}: {err:?}");
        }
        assert!(transform_gcode("M5\nM2\n", &rot90()).is_err());
    }

    #[test]
    fn comments_and_non_motion_lines_pass_through() {
        let out = transform_gcode(
            "; header (keep)\nM4 S0 ; laser\nG0 X1 Y0 (move)\nM5\n",
            &Similarity::IDENTITY,
        )
        .unwrap();
        assert!(out.gcode.contains("; header (keep)"));
        assert!(out.gcode.contains("M4 S0 ; laser"));
        assert!(out.gcode.lines().any(|l| l == "G0 X1 Y0"), "{}", out.gcode);
        assert!(out.gcode.starts_with("; MOSS two-point registration"));
    }
}
