use laser_contracts::geometry::Paths;
use laser_contracts::{ParamCell, builtin_material_library};

#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    pub material: String,
    pub requested_thickness_mm: f64,
    pub matched_thickness_mm: f64,
    pub laser_mode: String,
    pub engraving_mode: Option<String>,
    pub params: ParamCell,
    /// 命中的是“表面通用”条目（不分厚度，对齐 LightBurn 的 No thickness 条目）
    pub surface: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationCell {
    pub id: String,
    pub row: usize,
    pub column: usize,
    pub x_mm: f64,
    pub y_mm: f64,
    pub size_mm: f64,
    pub power: i32,
    pub speed: i32,
    pub passes: i32,
    pub pixel_size_mm: Option<f64>,
    pub line_interval_mm: Option<f64>,
}

pub fn recommend(
    material: &str,
    thickness_mm: f64,
    laser_mode: &str,
    engraving_mode: Option<&str>,
) -> Option<Recommendation> {
    recommend_with_scale(material, thickness_mm, laser_mode, engraving_mode, 1000.0)
}

pub fn recommend_with_scale(
    material: &str,
    thickness_mm: f64,
    laser_mode: &str,
    engraving_mode: Option<&str>,
    power_scale: f64,
) -> Option<Recommendation> {
    recommend_in_library(
        &builtin_material_library(),
        material,
        thickness_mm,
        laser_mode,
        engraving_mode,
        power_scale,
    )
}

pub fn recommend_in_library(
    library: &laser_contracts::MaterialLibrary,
    material: &str,
    thickness_mm: f64,
    laser_mode: &str,
    engraving_mode: Option<&str>,
    power_scale: f64,
) -> Option<Recommendation> {
    if !thickness_mm.is_finite() || thickness_mm <= 0.0 {
        return None;
    }
    let (name, entry) = library.materials.iter().find(|(name, entry)| {
        name.eq_ignore_ascii_case(material)
            || entry
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(material))
    })?;
    let numeric = |key: &str| key.parse::<f64>().ok().filter(|v| v.is_finite());
    let normalized_mode = laser_mode.to_ascii_lowercase();
    let engrave_mode = engraving_mode.unwrap_or("raster");
    match normalized_mode.as_str() {
        "cut" => {
            if !entry.thicknesses.keys().any(|value| {
                numeric(value).is_some_and(|candidate| (candidate - thickness_mm).abs() <= 1e-9)
            }) {
                return None;
            }
        }
        "engrave" if matches!(engrave_mode, "raster" | "outline") => {}
        _ => return None,
    }
    let pick = |modes: &laser_contracts::ThicknessEntry| -> Option<ParamCell> {
        if normalized_mode == "cut" {
            return modes.cut.clone();
        }
        let engrave = modes.engrave.as_ref()?;
        if engrave_mode == "outline" {
            engrave.outline.clone()
        } else {
            engrave.raster.clone()
        }
    };
    // 雕刻：同厚度有该模式参数 > “表面通用”条目 > 最近厚度（2026-11-16 对齐 LightBurn No thickness 条目）
    let exact_has_mode = entry.thicknesses.iter().any(|(key, modes)| {
        numeric(key).is_some_and(|v| (v - thickness_mm).abs() <= 1e-9) && pick(modes).is_some()
    });
    let surface_params = if normalized_mode == "engrave" && !exact_has_mode {
        entry
            .thicknesses
            .get(laser_contracts::SURFACE_THICKNESS_KEY)
            .and_then(pick)
    } else {
        None
    };
    let (matched_thickness_mm, params, surface) = if let Some(params) = surface_params {
        (thickness_mm, params, true)
    } else {
        let (thickness, modes) = entry
            .thicknesses
            .iter()
            .filter_map(|(key, modes)| numeric(key).map(|v| (v, modes)))
            .min_by(|(a, _), (b, _)| {
                (a - thickness_mm)
                    .abs()
                    .total_cmp(&(b - thickness_mm).abs())
            })?;
        (thickness, pick(modes)?, false)
    };
    Some(Recommendation {
        material: name.to_owned(),
        requested_thickness_mm: thickness_mm,
        matched_thickness_mm,
        engraving_mode: (normalized_mode == "engrave").then(|| engrave_mode.to_owned()),
        laser_mode: normalized_mode,
        params: if (power_scale - 1000.0).abs() > 1e-6 && power_scale > 0.0 {
            params.rescaled_to(power_scale)
        } else {
            params
        },
        surface,
    })
}

/// 标定填充未指定线距时的默认扫描线距（mm，与 2026-11-16 之前的标定刀路一致）。
pub const DEFAULT_CALIBRATION_INTERVAL_MM: f64 = 0.2;
/// 材料测试线距轴允许范围（mm）。
pub const TEST_INTERVAL_RANGE_MM: (f64, f64) = (0.01, 2.0);
/// 材料测试遍数轴允许范围。
pub const TEST_PASSES_RANGE: (i32, i32) = (1, 10);

/// 材料测试网格的一个轴可以选的参数（对齐 LightBurn Material Test 的 X / Y 轴参数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestAxis {
    Power,
    Speed,
    Interval,
    Passes,
}

impl TestAxis {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "power" => Some(Self::Power),
            "speed" => Some(Self::Speed),
            "interval" | "line_interval" => Some(Self::Interval),
            "passes" => Some(Self::Passes),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Power => "power",
            Self::Speed => "speed",
            Self::Interval => "interval",
            Self::Passes => "passes",
        }
    }

    /// 刻在样板上的轴标题（内置单线字体只含大写字母）。
    pub fn title(self) -> &'static str {
        match self {
            Self::Power => "POWER %",
            Self::Speed => "SPEED MM/MIN",
            Self::Interval => "INTERVAL MM",
            Self::Passes => "PASSES",
        }
    }

    /// 单元格在该轴上的取值文字（功率按 0~1000 标尺换算成百分比）。
    pub fn value_label(self, cell: &CalibrationCell) -> String {
        match self {
            Self::Power => {
                let percent = cell.power as f64 / 10.0;
                if (percent - percent.round()).abs() < 1e-9 {
                    format!("{}", percent.round() as i64)
                } else {
                    format!("{percent:.1}")
                }
            }
            Self::Speed => cell.speed.to_string(),
            Self::Interval => {
                // 至少两位小数，0.125 这类第三位非零时保留三位
                let value = cell
                    .line_interval_mm
                    .unwrap_or(DEFAULT_CALIBRATION_INTERVAL_MM);
                let mut text = format!("{value:.3}");
                if text.ends_with('0') {
                    text.pop();
                }
                text
            }
            Self::Passes => cell.passes.to_string(),
        }
    }
}

/// 材料测试网格规格：列走 `x_axis`，行走 `y_axis`；不在轴上的参数用 `fixed_*`。
/// 某个轴只有 1 格时取该轴区间下限（与 2026-11-16 之前的功率 × 速度矩阵一致）。
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialTestSpec {
    pub rows: usize,
    pub columns: usize,
    pub x_axis: TestAxis,
    pub y_axis: TestAxis,
    pub power: (i32, i32),
    pub speed: (i32, i32),
    pub interval_mm: (f64, f64),
    pub passes: (i32, i32),
    pub fixed_power: i32,
    pub fixed_speed: i32,
    /// `None` = 不指定线距（填充按 [`DEFAULT_CALIBRATION_INTERVAL_MM`]，选格时不写回线距）
    pub fixed_interval_mm: Option<f64>,
    pub fixed_passes: i32,
    pub cell_size_mm: f64,
    pub gap_mm: f64,
}

impl MaterialTestSpec {
    /// 2026-11-16 之前的功率（列）× 速度（行）矩阵：12 mm 方格、16 mm 间距。
    pub fn power_speed(
        rows: usize,
        columns: usize,
        power: (i32, i32),
        speed: (i32, i32),
        passes: i32,
    ) -> Self {
        Self {
            rows,
            columns,
            x_axis: TestAxis::Power,
            y_axis: TestAxis::Speed,
            power,
            speed,
            interval_mm: (
                DEFAULT_CALIBRATION_INTERVAL_MM,
                DEFAULT_CALIBRATION_INTERVAL_MM,
            ),
            passes: (passes, passes),
            fixed_power: power.0,
            fixed_speed: speed.0,
            fixed_interval_mm: None,
            fixed_passes: passes,
            cell_size_mm: 12.0,
            gap_mm: 4.0,
        }
    }

    /// 线距测试（对齐 LightBurn Interval Test）：一排 `count` 个小填充方块，线距从
    /// `interval_mm.0` 到 `.1` 逐格变化，功率、速度、遍数固定。默认 8 mm 方块、4 mm 间距。
    pub fn interval_test(
        count: usize,
        interval_mm: (f64, f64),
        power: i32,
        speed: i32,
        passes: i32,
    ) -> Self {
        Self {
            rows: 1,
            columns: count,
            x_axis: TestAxis::Interval,
            y_axis: TestAxis::Speed,
            power: (power, power),
            speed: (speed, speed),
            interval_mm,
            passes: (passes, passes),
            fixed_power: power,
            fixed_speed: speed,
            fixed_interval_mm: None,
            fixed_passes: passes,
            cell_size_mm: 8.0,
            gap_mm: 4.0,
        }
    }

    fn uses(&self, axis: TestAxis) -> bool {
        self.x_axis == axis || self.y_axis == axis
    }
}

fn interpolate_i32(min: i32, max: i32, index: usize, count: usize) -> i32 {
    if count == 1 {
        min
    } else {
        min + ((max - min) as f64 * index as f64 / (count - 1) as f64).round() as i32
    }
}

fn interpolate_mm(min: f64, max: f64, index: usize, count: usize) -> f64 {
    let value = if count == 1 {
        min
    } else {
        min + (max - min) * index as f64 / (count - 1) as f64
    };
    (value * 1000.0).round() / 1000.0
}

/// 按规格生成材料测试单元格（行优先，`cell_number` = 下标 + 1）。坐标 y 向上，A1 在左下。
pub fn material_test_cells(spec: &MaterialTestSpec) -> Result<Vec<CalibrationCell>, &'static str> {
    if spec.rows == 0 || spec.columns == 0 || spec.rows > 25 || spec.columns > 25 {
        return Err("invalid calibration matrix dimensions");
    }
    if spec.x_axis == spec.y_axis && spec.rows > 1 && spec.columns > 1 {
        return Err("x_axis and y_axis must be different parameters");
    }
    if spec.fixed_passes < 1 || spec.passes.0 < 1 {
        return Err("invalid calibration matrix dimensions");
    }
    if spec.speed.0 < 1 || spec.fixed_speed < 1 || spec.power.0 < 0 || spec.fixed_power < 0 {
        return Err("speed must be positive and power non-negative");
    }
    if !spec.cell_size_mm.is_finite()
        || !(1.0..=50.0).contains(&spec.cell_size_mm)
        || !spec.gap_mm.is_finite()
        || !(0.0..=50.0).contains(&spec.gap_mm)
    {
        return Err("cell size must be 1..50 mm and gap 0..50 mm");
    }
    let in_interval = |v: f64| {
        v.is_finite() && (TEST_INTERVAL_RANGE_MM.0..=TEST_INTERVAL_RANGE_MM.1).contains(&v)
    };
    if spec.uses(TestAxis::Interval)
        && (!in_interval(spec.interval_mm.0)
            || !in_interval(spec.interval_mm.1)
            || spec.interval_mm.0 > spec.interval_mm.1)
    {
        return Err("line interval range must be 0.01..2 mm with min <= max");
    }
    if spec.fixed_interval_mm.is_some_and(|v| !in_interval(v)) {
        return Err("line interval range must be 0.01..2 mm with min <= max");
    }
    let passes_ok = |v: i32| (TEST_PASSES_RANGE.0..=TEST_PASSES_RANGE.1).contains(&v);
    if spec.uses(TestAxis::Passes)
        && (!passes_ok(spec.passes.0) || !passes_ok(spec.passes.1) || spec.passes.0 > spec.passes.1)
    {
        return Err("passes range must be 1..10 with min <= max");
    }
    let pitch = spec.cell_size_mm + spec.gap_mm;
    let mut cells = Vec::with_capacity(spec.rows * spec.columns);
    for row in 0..spec.rows {
        for column in 0..spec.columns {
            // 该参数在哪个轴上：列轴按列号插值，行轴按行号插值，否则取固定值
            // （两轴同一参数只允许在单行或单列时出现，此时按有多格的那个方向插值）
            let position = |axis: TestAxis| {
                if spec.x_axis == axis && (spec.columns > 1 || spec.y_axis != axis) {
                    Some((column, spec.columns))
                } else if spec.y_axis == axis {
                    Some((row, spec.rows))
                } else {
                    None
                }
            };
            let power = position(TestAxis::Power).map_or(spec.fixed_power, |(i, n)| {
                interpolate_i32(spec.power.0, spec.power.1, i, n)
            });
            let speed = position(TestAxis::Speed).map_or(spec.fixed_speed, |(i, n)| {
                interpolate_i32(spec.speed.0, spec.speed.1, i, n)
            });
            let passes = position(TestAxis::Passes).map_or(spec.fixed_passes, |(i, n)| {
                interpolate_i32(spec.passes.0, spec.passes.1, i, n)
            });
            let interval = position(TestAxis::Interval).map_or(spec.fixed_interval_mm, |(i, n)| {
                Some(interpolate_mm(spec.interval_mm.0, spec.interval_mm.1, i, n))
            });
            cells.push(CalibrationCell {
                id: format!("{}{}", (b'A' + row as u8) as char, column + 1),
                row: row + 1,
                column: column + 1,
                x_mm: column as f64 * pitch,
                y_mm: row as f64 * pitch,
                size_mm: spec.cell_size_mm,
                power,
                speed,
                passes,
                pixel_size_mm: interval,
                line_interval_mm: interval,
            });
        }
    }
    Ok(cells)
}

pub fn calibration_matrix(
    rows: usize,
    columns: usize,
    power_min: i32,
    power_max: i32,
    speed_min: i32,
    speed_max: i32,
    passes: i32,
) -> Result<Vec<CalibrationCell>, &'static str> {
    material_test_cells(&MaterialTestSpec::power_speed(
        rows,
        columns,
        (power_min, power_max),
        (speed_min, speed_max),
        passes,
    ))
}

/// 单元格的相对烧蚀风险：单位面积能量 ∝ 功率 × 遍数 ÷（速度 × 线距）。
/// 只用于排序（高速、低功率、宽线距、少遍数的格子风险最低）。
pub fn burn_risk(cell: &CalibrationCell) -> f64 {
    let interval = cell
        .line_interval_mm
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(DEFAULT_CALIBRATION_INTERVAL_MM);
    f64::from(cell.power.max(0)) * f64::from(cell.passes.max(1))
        / (f64::from(cell.speed.max(1)) * interval)
}

/// 加工顺序：从烧蚀风险最低到最高（对齐 LightBurn Material Test），同风险按网格顺序。
/// 先刻的格子即使起火冒烟也最轻，后面高能量格子的烟尘不会熏到还没刻的低能量格子。
pub fn risk_order(cells: &[CalibrationCell]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..cells.len()).collect();
    order.sort_by(|&a, &b| {
        burn_risk(&cells[a])
            .total_cmp(&burn_risk(&cells[b]))
            .then(a.cmp(&b))
    });
    order
}

/// 样板标注：轴标题 + 每列 / 每行参数值，用内置单线字体刻出。
#[derive(Debug, Clone, PartialEq)]
pub struct TestLabels {
    pub height_mm: f64,
    pub power: i32,
    pub speed: i32,
}

/// 排版结果：单元格已平移到给标注留出的位置，`labels` 为标注笔画（y 向上，mm）。
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialTestLayout {
    pub cells: Vec<CalibrationCell>,
    pub labels: Paths,
    pub width_mm: f64,
    pub height_mm: f64,
}

const LABEL_GAP_MM: f64 = 1.5;

/// 给网格加行列标注并算出整体外框：列值在网格上方、列轴标题再往上；行值在网格左侧，
/// 行轴标题竖排在最左。只有多于 1 格的方向才标注。整体以左下角为原点（全部坐标 ≥ 0）。
pub fn layout_material_test(
    cells: Vec<CalibrationCell>,
    x_axis: TestAxis,
    y_axis: TestAxis,
    labels: Option<&TestLabels>,
) -> Result<MaterialTestLayout, &'static str> {
    let bounds = |cells: &[CalibrationCell]| {
        cells.iter().fold((0.0_f64, 0.0_f64), |(w, h), c| {
            (w.max(c.x_mm + c.size_mm), h.max(c.y_mm + c.size_mm))
        })
    };
    let Some(spec) = labels else {
        let (width_mm, height_mm) = bounds(&cells);
        return Ok(MaterialTestLayout {
            cells,
            labels: Vec::new(),
            width_mm,
            height_mm,
        });
    };
    if !spec.height_mm.is_finite() || !(1.0..=10.0).contains(&spec.height_mm) {
        return Err("label height must be 1..10 mm");
    }
    let h = spec.height_mm;
    let rows = cells.iter().map(|c| c.row).max().unwrap_or(0);
    let columns = cells.iter().map(|c| c.column).max().unwrap_or(0);
    // 行标注：各行的值（右对齐，垂直居中于该行）+ 竖排轴标题
    let mut row_texts: Vec<(f64, Paths, f64)> = Vec::new();
    if rows > 1 {
        for row in 1..=rows {
            let cell = cells
                .iter()
                .find(|c| c.row == row && c.column == 1)
                .ok_or("calibration cells are incomplete")?;
            let (strokes, w) = crate::stroke_font::stroke_text(&y_axis.value_label(cell), h)?;
            row_texts.push((cell.y_mm + (cell.size_mm - h) / 2.0, strokes, w));
        }
    }
    let row_value_width = row_texts.iter().map(|(_, _, w)| *w).fold(0.0_f64, f64::max);
    let offset_x = if rows > 1 {
        h + LABEL_GAP_MM + row_value_width + LABEL_GAP_MM
    } else {
        0.0
    };
    let cells: Vec<CalibrationCell> = cells
        .into_iter()
        .map(|mut c| {
            c.x_mm += offset_x;
            c
        })
        .collect();
    let (grid_right, grid_top) = bounds(&cells);
    let mut paths: Paths = Vec::new();
    let mut place = |strokes: Paths, dx: f64, dy: f64, rotate: bool| {
        for stroke in strokes {
            paths.push(
                stroke
                    .into_iter()
                    .map(|(x, y)| {
                        if rotate {
                            (dx - y, dy + x)
                        } else {
                            (dx + x, dy + y)
                        }
                    })
                    .collect(),
            );
        }
    };
    let mut top = grid_top;
    if columns > 1 {
        let baseline = grid_top + LABEL_GAP_MM;
        for column in 1..=columns {
            let cell = cells
                .iter()
                .find(|c| c.row == 1 && c.column == column)
                .ok_or("calibration cells are incomplete")?;
            let (strokes, w) = crate::stroke_font::stroke_text(&x_axis.value_label(cell), h)?;
            place(
                strokes,
                cell.x_mm + (cell.size_mm - w) / 2.0,
                baseline,
                false,
            );
        }
        let title_baseline = baseline + h + LABEL_GAP_MM;
        let (strokes, w) = crate::stroke_font::stroke_text(x_axis.title(), h)?;
        let grid_left = offset_x;
        place(
            strokes,
            (grid_left + (grid_right - grid_left - w) / 2.0).max(0.0),
            title_baseline,
            false,
        );
        top = title_baseline + h;
    }
    if rows > 1 {
        for (baseline, strokes, w) in row_texts {
            place(strokes, offset_x - LABEL_GAP_MM - w, baseline, false);
        }
        // 竖排标题：逆时针转 90°，字头朝左，占 x ∈ [0, h]
        let (strokes, w) = crate::stroke_font::stroke_text(y_axis.title(), h)?;
        place(strokes, h, ((grid_top - w) / 2.0).max(0.0), true);
    }
    let width_mm = paths
        .iter()
        .flatten()
        .map(|p| p.0)
        .fold(grid_right, f64::max);
    let height_mm = paths
        .iter()
        .flatten()
        .map(|p| p.1)
        .fold(top.max(grid_top), f64::max);
    Ok(MaterialTestLayout {
        cells,
        labels: paths,
        width_mm,
        height_mm,
    })
}

pub fn build_calibration_cells(
    task_type: &str,
    power: i32,
    speed: i32,
    passes: i32,
) -> Result<Vec<CalibrationCell>, &'static str> {
    if !matches!(task_type, "cut_contour" | "engrave_photo" | "engrave_logo") {
        return Err("unsupported calibration task type");
    }
    let mut cells = Vec::with_capacity(9);
    for row in 0..3 {
        for column in 0..3 {
            let pixel = if task_type == "cut_contour" {
                None
            } else {
                Some([0.16, 0.20, 0.24][column])
            };
            cells.push(CalibrationCell {
                id: format!("{}{}", (b'A' + row as u8) as char, column + 1),
                row: row + 1,
                column: column + 1,
                x_mm: column as f64 * 16.0,
                y_mm: row as f64 * 16.0,
                size_mm: 12.0,
                power: (power + [-40, 0, 40][column]).clamp(1, 500),
                speed: (speed + [200, 0, -200][row]).clamp(300, 3000),
                passes: if task_type == "cut_contour" {
                    (passes + column as i32 - 1).clamp(1, 5)
                } else {
                    passes
                },
                pixel_size_mm: pixel,
                line_interval_mm: pixel,
            });
        }
    }
    Ok(cells)
}

pub fn build_calibration_gcode(
    material: &str,
    task_type: &str,
    cells: &[CalibrationCell],
) -> Result<String, &'static str> {
    let order: Vec<usize> = (0..cells.len()).collect();
    build_material_test_gcode(material, task_type, cells, &order, None)
}

/// 标注笔画与出光参数（y 向上，mm）。
#[derive(Debug, Clone, Copy)]
pub struct LabelBurn<'a> {
    pub paths: &'a Paths,
    pub power: i32,
    pub speed: i32,
}

/// 写材料测试 G-code：先刻标注（单线、低风险参数），再按 `order` 逐格加工。
/// `order` 必须是 `cells` 下标的一个排列；单元格的刀路写法与旧版标定矩阵相同。
pub fn build_material_test_gcode(
    material: &str,
    task_type: &str,
    cells: &[CalibrationCell],
    order: &[usize],
    labels: Option<LabelBurn<'_>>,
) -> Result<String, &'static str> {
    if material.is_empty() || cells.is_empty() {
        return Err("calibration requires material and cells");
    }
    let mut seen = vec![false; cells.len()];
    if order.len() != cells.len()
        || order
            .iter()
            .any(|&i| i >= cells.len() || std::mem::replace(&mut seen[i], true))
    {
        return Err("calibration run order must list every cell once");
    }
    let mut lines = vec![
        "; Generated by ai-laser-gcode-nc".to_owned(),
        "; Calibration matrix only; observe sample results before saving verified parameters"
            .to_owned(),
        format!("; Material: {material}"),
        format!("; Task type: {task_type}"),
        "G21".into(),
        "G90".into(),
        "G94".into(),
        "G17".into(),
        "G92 X0 Y0 Z0".into(),
        "M4 S0".into(),
        "G0 F3000".into(),
    ];
    if let Some(label) = labels.filter(|l| !l.paths.is_empty()) {
        if label.power < 0 || label.speed < 1 {
            return Err("label power/speed are invalid");
        }
        lines.push(format!(
            "; Labels: S{} F{} (single-line text)",
            label.power, label.speed
        ));
        for stroke in label.paths {
            let Some(&(x0, y0)) = stroke.first() else {
                continue;
            };
            if stroke.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
                return Err("label path contains non-finite coordinates");
            }
            lines.push(format!("G0 X{x0:.3} Y{y0:.3}"));
            for &(x, y) in &stroke[1..] {
                lines.push(format!(
                    "G1 X{x:.3} Y{y:.3} S{} F{}",
                    label.power, label.speed
                ));
            }
            lines.push("S0".into());
        }
    }
    if order.iter().enumerate().any(|(k, &i)| k != i) {
        lines.push("; Run order: lowest burn risk first".into());
    }
    for cell in order.iter().map(|&i| &cells[i]) {
        let interval_note = cell
            .line_interval_mm
            .map(|v| format!(" interval={v:.3}"))
            .unwrap_or_default();
        lines.push(format!(
            "; Cell {}: S{} F{} passes={}{interval_note}",
            cell.id, cell.power, cell.speed, cell.passes
        ));
        if lines.len() > MAX_CALIBRATION_LINES {
            return Err(
                "calibration G-code is too long; use fewer cells, passes or a wider interval",
            );
        }
        for _ in 0..cell.passes {
            let (x0, y0) = (cell.x_mm, cell.y_mm);
            let (x1, y1) = (x0 + cell.size_mm, y0 + cell.size_mm);
            lines.extend(["M5".into(), format!("G0 X{x0:.3} Y{y0:.3}"), "M4 S0".into()]);
            if matches!(task_type, "cut_contour" | "engrave_outline") {
                lines.extend([
                    format!("G1 X{x1:.3} Y{y0:.3} S{} F{}", cell.power, cell.speed),
                    format!("G1 X{x1:.3} Y{y1:.3} S{} F{}", cell.power, cell.speed),
                    format!("G1 X{x0:.3} Y{y1:.3} S{} F{}", cell.power, cell.speed),
                    format!("G1 X{x0:.3} Y{y0:.3} S{} F{}", cell.power, cell.speed),
                ]);
            } else {
                let step = cell
                    .line_interval_mm
                    .unwrap_or(DEFAULT_CALIBRATION_INTERVAL_MM);
                let mut y = y0;
                while y <= y1 + 1e-9 {
                    lines.push(format!("G0 X{x0:.3} Y{y:.3}"));
                    lines.push(format!(
                        "G1 X{x1:.3} Y{y:.3} S{} F{}",
                        cell.power, cell.speed
                    ));
                    y += step;
                }
            }
        }
        lines.push("S0".into());
    }
    if lines.len() > MAX_CALIBRATION_LINES {
        return Err("calibration G-code is too long; use fewer cells, passes or a wider interval");
    }
    lines.extend(["M5".into(), "M2".into()]);
    Ok(lines.join("\n") + "\n")
}

/// 标定刀路行数上限（约 50 MB 文本）：再多说明格子、遍数太多或线距太细。
pub const MAX_CALIBRATION_LINES: usize = 2_000_000;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recommends_builtin_materials_and_aliases() {
        let result = recommend("basswood", 3.0, "engrave", Some("outline")).unwrap();
        assert_eq!(result.material, "椴木");
        assert_eq!(result.params.laser_max_power, 420);
    }
    #[test]
    fn builds_deterministic_calibration_grid() {
        let grid = calibration_matrix(3, 3, 80, 160, 1000, 1400, 1).unwrap();
        assert_eq!(grid.len(), 9);
        assert_eq!(grid[0].power, 80);
        assert_eq!(grid[8].speed, 1400);
    }

    fn spec(x: TestAxis, y: TestAxis, rows: usize, columns: usize) -> MaterialTestSpec {
        MaterialTestSpec {
            rows,
            columns,
            x_axis: x,
            y_axis: y,
            power: (100, 500),
            speed: (600, 2400),
            interval_mm: (0.08, 0.16),
            passes: (1, 3),
            fixed_power: 300,
            fixed_speed: 1500,
            fixed_interval_mm: Some(0.1),
            fixed_passes: 1,
            cell_size_mm: 12.0,
            gap_mm: 4.0,
        }
    }

    #[test]
    fn power_speed_spec_matches_the_legacy_matrix_exactly() {
        let legacy = calibration_matrix(4, 4, 100, 500, 600, 2400, 2).unwrap();
        assert_eq!(
            legacy.iter().map(|c| c.power).collect::<Vec<_>>()[..4],
            [100, 233, 367, 500]
        );
        assert_eq!(
            legacy
                .iter()
                .step_by(4)
                .map(|c| c.speed)
                .collect::<Vec<_>>(),
            [600, 1200, 1800, 2400]
        );
        assert!(
            legacy
                .iter()
                .all(|c| c.passes == 2 && c.line_interval_mm.is_none())
        );
        assert_eq!(
            (legacy[5].x_mm, legacy[5].y_mm, legacy[5].size_mm),
            (16.0, 16.0, 12.0)
        );
        assert!(calibration_matrix(0, 3, 1, 2, 3, 4, 1).is_err());
        assert!(calibration_matrix(3, 3, 1, 2, 3, 4, 0).is_err());
    }

    #[test]
    fn each_axis_maps_to_its_own_parameter_and_others_stay_fixed() {
        let cells = material_test_cells(&spec(TestAxis::Interval, TestAxis::Passes, 3, 3)).unwrap();
        // 列 → 线距，行 → 遍数，功率 / 速度固定
        assert_eq!(cells[0].line_interval_mm, Some(0.08));
        assert_eq!(cells[1].line_interval_mm, Some(0.12));
        assert_eq!(cells[2].line_interval_mm, Some(0.16));
        assert_eq!(cells[2].pixel_size_mm, Some(0.16));
        assert_eq!(
            [cells[0].passes, cells[3].passes, cells[6].passes],
            [1, 2, 3]
        );
        assert!(cells.iter().all(|c| c.power == 300 && c.speed == 1500));

        let swapped = material_test_cells(&spec(TestAxis::Speed, TestAxis::Power, 2, 2)).unwrap();
        assert_eq!((swapped[1].speed, swapped[2].power), (2400, 500));
        assert!(swapped.iter().all(|c| c.line_interval_mm == Some(0.1)));

        assert!(material_test_cells(&spec(TestAxis::Power, TestAxis::Power, 2, 2)).is_err());
        let mut bad = spec(TestAxis::Interval, TestAxis::Power, 2, 2);
        bad.interval_mm = (0.001, 0.1);
        assert!(material_test_cells(&bad).is_err());
        let mut bad = spec(TestAxis::Passes, TestAxis::Power, 2, 2);
        bad.passes = (1, 11);
        assert!(material_test_cells(&bad).is_err());
        assert_eq!(TestAxis::parse(" Interval "), Some(TestAxis::Interval));
        assert_eq!(TestAxis::parse("focus"), None);
    }

    #[test]
    fn run_order_goes_from_lowest_to_highest_burn_risk() {
        // 默认功率（列，左低右高）× 速度（行，下慢上快）：先刻最快、功率最低的格子
        let cells = calibration_matrix(3, 3, 100, 300, 600, 1800, 1).unwrap();
        let order = risk_order(&cells);
        assert_eq!(cells[order[0]].id, "C1", "fastest row + lowest power first");
        assert_eq!(
            cells[*order.last().unwrap()].id,
            "A3",
            "slowest + strongest last"
        );
        for pair in order.windows(2) {
            assert!(burn_risk(&cells[pair[0]]) <= burn_risk(&cells[pair[1]]));
        }
        // 宽线距、少遍数风险更低
        let cells = material_test_cells(&spec(TestAxis::Interval, TestAxis::Passes, 2, 2)).unwrap();
        let order = risk_order(&cells);
        assert_eq!(cells[order[0]].id, "A2", "widest interval + 1 pass first");
        assert_eq!(
            cells[order[3]].id, "B1",
            "narrowest interval + most passes last"
        );

        let gcode =
            build_material_test_gcode("椴木", "engrave_logo", &cells, &order, None).unwrap();
        let positions: Vec<usize> = ["A2", "A1", "B2", "B1"]
            .iter()
            .map(|id| gcode.find(&format!("; Cell {id}:")).unwrap())
            .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{positions:?}");
        assert!(
            build_material_test_gcode("椴木", "engrave_logo", &cells, &[0, 0, 1, 2], None).is_err()
        );
    }

    #[test]
    fn labels_sit_in_positive_space_around_the_grid() {
        let cells = calibration_matrix(3, 4, 100, 400, 600, 1800, 1).unwrap();
        let label = TestLabels {
            height_mm: 3.0,
            power: 200,
            speed: 3000,
        };
        let layout = layout_material_test(
            cells.clone(),
            TestAxis::Power,
            TestAxis::Speed,
            Some(&label),
        )
        .unwrap();
        assert!(!layout.labels.is_empty());
        let grid_left = layout.cells[0].x_mm;
        assert!(grid_left > 3.0, "row labels need room on the left");
        assert!(
            layout
                .cells
                .iter()
                .all(|c| c.y_mm == cells[(c.row - 1) * 4 + c.column - 1].y_mm)
        );
        for point in layout.labels.iter().flatten() {
            assert!(
                point.0 >= -1e-9 && point.0 <= layout.width_mm + 1e-9,
                "{point:?}"
            );
            assert!(
                point.1 >= -1e-9 && point.1 <= layout.height_mm + 1e-9,
                "{point:?}"
            );
            // 标注不压在任何格子上
            assert!(!layout.cells.iter().any(|c| point.0 > c.x_mm + 1e-6
                && point.0 < c.x_mm + c.size_mm - 1e-6
                && point.1 > c.y_mm + 1e-6
                && point.1 < c.y_mm + c.size_mm - 1e-6));
        }
        assert!(layout.height_mm > 3.0 * 16.0);
        let order = risk_order(&layout.cells);
        let gcode = build_material_test_gcode(
            "椴木",
            "engrave_logo",
            &layout.cells,
            &order,
            Some(LabelBurn {
                paths: &layout.labels,
                power: 200,
                speed: 3000,
            }),
        )
        .unwrap();
        assert!(gcode.contains("; Labels: S200 F3000"));
        assert!(gcode.find("; Labels").unwrap() < gcode.find("; Cell").unwrap());
        assert!(gcode.ends_with("M5\nM2\n"));
        let unlabeled =
            layout_material_test(cells, TestAxis::Power, TestAxis::Speed, None).unwrap();
        assert_eq!((unlabeled.width_mm, unlabeled.height_mm), (60.0, 44.0));
        assert_eq!(TestAxis::Power.value_label(&unlabeled.cells[1]), "20");
    }

    #[test]
    fn interval_test_is_one_row_of_labelled_fill_squares() {
        let spec = MaterialTestSpec::interval_test(9, (0.08, 0.16), 300, 1500, 1);
        let cells = material_test_cells(&spec).unwrap();
        assert_eq!(cells.len(), 9);
        let intervals: Vec<f64> = cells.iter().map(|c| c.line_interval_mm.unwrap()).collect();
        assert_eq!(
            intervals,
            [0.08, 0.09, 0.10, 0.11, 0.12, 0.13, 0.14, 0.15, 0.16]
        );
        assert!(
            cells
                .iter()
                .all(|c| c.size_mm == 8.0 && c.power == 300 && c.speed == 1500)
        );
        assert_eq!(cells[1].x_mm - cells[0].x_mm, 12.0);
        let labels: Vec<String> = cells
            .iter()
            .map(|c| TestAxis::Interval.value_label(c))
            .collect();
        assert_eq!(labels[0], "0.08");
        assert_eq!(labels[2], "0.10");
        let layout = layout_material_test(
            cells,
            TestAxis::Interval,
            TestAxis::Speed,
            Some(&TestLabels {
                height_mm: 2.5,
                power: 200,
                speed: 3000,
            }),
        )
        .unwrap();
        // 单行：只有列标注（数值 + 标题），格子不右移
        assert_eq!(layout.cells[0].x_mm, 0.0);
        assert!(layout.height_mm > 8.0 + 2.0 * 2.5);
        let order = risk_order(&layout.cells);
        assert_eq!(layout.cells[order[0]].id, "A9", "widest interval first");
        let gcode =
            build_material_test_gcode("椴木", "engrave_logo", &layout.cells, &order, None).unwrap();
        // 每格 8 mm 按线距扫：0.08 mm 那格的行数约是 0.16 mm 那格的两倍
        let rows_of = |id: &str| {
            let start = gcode.find(&format!("; Cell {id}:")).unwrap();
            let rest = &gcode[start + 1..];
            let end = rest.find("; Cell").unwrap_or(rest.len());
            rest[..end].matches("G1 ").count()
        };
        assert_eq!(rows_of("A9"), 51);
        assert_eq!(rows_of("A1"), 101);
        assert!(
            material_test_cells(&MaterialTestSpec::interval_test(
                9,
                (0.2, 0.1),
                300,
                1500,
                1
            ))
            .is_err()
        );
    }

    #[test]
    fn surface_entries_back_up_engraving_for_any_thickness() {
        let mut lib = laser_contracts::MaterialLibrary {
            version: 1,
            power_scale: 1000,
            materials: Default::default(),
        };
        let mut entry = laser_contracts::MaterialEntry::default();
        let mut surface = ParamCell::new(222, 2222, -1);
        surface.set_title(Some("深色雕刻"));
        entry.thicknesses.insert(
            laser_contracts::SURFACE_THICKNESS_KEY.into(),
            laser_contracts::ThicknessEntry {
                engrave: Some(laser_contracts::EngraveModes {
                    raster: Some(surface),
                    outline: None,
                }),
                cut: None,
            },
        );
        entry.thicknesses.insert(
            "3".into(),
            laser_contracts::ThicknessEntry {
                engrave: Some(laser_contracts::EngraveModes {
                    raster: None,
                    outline: Some(ParamCell::new(333, 1000, -1)),
                }),
                cut: Some(ParamCell::new(800, 300, -1)),
            },
        );
        lib.materials.insert("测试板".into(), entry);
        // 3 mm 没有光栅参数 → 用表面条目；轮廓参数同厚度有 → 用同厚度
        let raster =
            recommend_in_library(&lib, "测试板", 3.0, "engrave", Some("raster"), 1000.0).unwrap();
        assert!(raster.surface);
        assert_eq!(raster.params.laser_max_power, 222);
        assert_eq!(raster.params.title(), Some("深色雕刻"));
        assert_eq!(raster.matched_thickness_mm, 3.0);
        let outline =
            recommend_in_library(&lib, "测试板", 3.0, "engrave", Some("outline"), 1000.0).unwrap();
        assert!(!outline.surface);
        assert_eq!(outline.params.laser_max_power, 333);
        // 其他厚度的光栅也走表面条目；切割仍要求真实厚度
        assert!(
            recommend_in_library(&lib, "测试板", 8.0, "engrave", None, 1000.0)
                .unwrap()
                .surface
        );
        assert!(recommend_in_library(&lib, "测试板", 8.0, "cut", None, 1000.0).is_none());
        assert_eq!(
            recommend_in_library(&lib, "测试板", 3.0, "cut", None, 1000.0)
                .unwrap()
                .params
                .laser_max_power,
            800
        );
    }

    #[test]
    fn cut_does_not_silently_select_a_nearest_thickness() {
        assert!(recommend("椴木", 2.0, "cut", None).is_none());
        assert!(recommend("椴木", f64::NAN, "engrave", None).is_none());
    }

    #[test]
    fn calibration_cells_match_python_offsets_and_safe_clamps() {
        let cells = build_calibration_cells("cut_contour", 120, 1200, 1).unwrap();
        assert_eq!(cells[0].id, "A1");
        assert_eq!((cells[0].power, cells[0].speed), (80, 1400));
        assert_eq!(cells[2].passes, 2);
        assert_eq!(cells[8].y_mm, 32.0);
        assert!(
            build_calibration_gcode("椴木", "cut_contour", &cells)
                .unwrap()
                .ends_with("M5\nM2\n")
        );
    }

    #[test]
    fn recommends_custom_materials_from_supplied_library() {
        let mut lib = builtin_material_library();
        let mut entry = laser_contracts::MaterialEntry::default();
        entry.aliases.push("custom-bamboo".into());
        let th = laser_contracts::ThicknessEntry {
            cut: Some(ParamCell {
                laser_min_power: 0,
                laser_max_power: 750,
                feed_rate: 320,
                travel_rate: 3000,
                threshold: -1,
                passes: 2,
                pixel_size_mm: 0.1,
                power_scale: Some(1000),
                source: None,
                notes: None,
                updated_at: None,
                extra: Default::default(),
            }),
            ..Default::default()
        };
        entry.thicknesses.insert("5".into(), th);
        lib.materials.insert("特制楠竹".into(), entry);

        let res = recommend_in_library(&lib, "custom-bamboo", 5.0, "cut", None, 255.0).unwrap();
        assert_eq!(res.material, "特制楠竹");
        assert_eq!(res.laser_mode, "cut");
        assert_eq!(res.params.feed_rate, 320);
        assert_eq!(res.params.passes, 2);
        assert!(res.params.laser_max_power < 255);
    }
}
