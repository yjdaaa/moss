use super::*;

#[test]
fn motion_extents_follow_modes_offsets_units_and_arcs() {
    // 生成任务的典型开头：G92 把当前位置设为原点，再走 0..80 × 0..20
    let job = "G21\nG90\nG92 X0 Y0 Z0\nM4 S0\nG0 X0 Y0\nG1 X80 Y0 S420\nG1 X80 Y20\nG0 X0 Y0\nM5";
    let e = motion_extents(job).unwrap().unwrap();
    assert_eq!((e.width(), e.height()), (80.0, 20.0));
    // 相对坐标与英寸
    let e = motion_extents("G91\nG1 X10 Y5\nG1 X10 Y5")
        .unwrap()
        .unwrap();
    assert_eq!((e.max_x, e.max_y), (20.0, 10.0));
    let e = motion_extents("G20\nG90\nG1 X2 Y1").unwrap().unwrap();
    assert!((e.max_x - 50.8).abs() < 1e-9 && (e.max_y - 25.4).abs() < 1e-9);
    // 中途 G92 改原点：之后的 X0 实际在开始点右侧 30 mm
    let e = motion_extents("G90\nG0 X30\nG92 X0\nG1 X100")
        .unwrap()
        .unwrap();
    assert_eq!(e.max_x, 130.0);
    // 负方向同样计入
    let e = motion_extents("G90\nG1 X-15 Y-5\nG1 X10 Y10")
        .unwrap()
        .unwrap();
    assert_eq!((e.width(), e.height()), (25.0, 15.0));
    // 逆时针半圆从 (10,0) 绕 (0,0) 到 (-10,0)，经过最高点 (0,10)
    let e = motion_extents("G90\nG0 X10 Y0\nG3 X-10 Y0 I-10 J0")
        .unwrap()
        .unwrap();
    assert!((e.max_y - 10.0).abs() < 1e-9 && e.min_y.abs() < 1e-9);
    // 顺时针同一半圆经过最低点 (0,-10)
    let e = motion_extents("G90\nG0 X10 Y0\nG2 X-10 Y0 I-10 J0")
        .unwrap()
        .unwrap();
    assert!((e.min_y + 10.0).abs() < 1e-9 && e.max_y.abs() < 1e-9);
    // 整圆
    let e = motion_extents("G90\nG0 X10 Y0\nG2 X10 Y0 I-10 J0")
        .unwrap()
        .unwrap();
    assert!((e.width() - 20.0).abs() < 1e-9 && (e.height() - 20.0).abs() < 1e-9);
    // 模态 G1 下只写坐标的行也算移动；注释里的坐标不算
    let e = motion_extents("G1 X5\nX40 ; X999\n(Y999) Y7")
        .unwrap()
        .unwrap();
    assert_eq!((e.max_x, e.max_y), (40.0, 7.0));
    assert_eq!(motion_extents("G21\nM5\n").unwrap(), None);
    assert!(motion_extents("G53 G0 X0 Y0").is_err());
    assert!(motion_extents("G28").is_err());
}
#[test]
fn modal_parser_and_estimator_cover_compact_words_arcs_and_pauses() {
    let text = "G21 G90 G94 M4 S100 F600\nG0X0Y0\nG1X10Y0\nG2 X0 Y10 I-5 J5 G4 P2\nM5";
    assert_eq!(parse_moves(text).len(), 2);
    let estimate = estimate_time(text, 1200.0);
    assert!(estimate.estimated_seconds > 2.0);
    assert!(estimate.laser_distance_mm > 0.0);

    // Test arc with R
    let text_r = "G21 G90 G94 M4 S100 F600\nG0 X0 Y0\nG2 X10 Y0 R5\nM5";
    let estimate_r = estimate_time(text_r, 1200.0);
    assert!(estimate_r.laser_distance_mm > 0.0);
}

#[test]
fn raster_writer_supports_scanline_and_segment() {
    let plan = crate::raster::RasterPlan {
        horizontal: vec![
            crate::raster::RasterSegment {
                axis: "horizontal",
                line: 0,
                start: 2,
                end: 8,
            },
            crate::raster::RasterSegment {
                axis: "horizontal",
                line: 1,
                start: 3,
                end: 7,
            },
        ],
        vertical: vec![],
        selected_axis: "horizontal",
        output_strategy: "scanline",
    };
    let options = RasterWriterOptions {
        pixel_size_mm: 0.1,
        overscan_mm: 1.0,
        work_area_width_mm: 100.0,
        work_area_height_mm: 100.0,
        power: 200,
        feed_rate: 1000,
        laser_mode: "M4",
        snake_scan: true,
        material: Some("basswood".into()),
        safety_note: Some("test".into()),
        power_scale: None,
        dot_width_mm: 0.0,
    };
    let gcode = write_raster(&plan, 10, 2, &options).unwrap();
    assert!(gcode.contains("; Raster mode: black/white dithered scan"));
    assert!(gcode.contains("G1 X"));
    assert!(gcode.contains("M4 S0"));
    assert!(gcode.contains("M2"));

    let mut plan_seg = plan.clone();
    plan_seg.output_strategy = "segment";
    let gcode_seg = write_raster(&plan_seg, 10, 2, &options).unwrap();
    assert!(gcode_seg.contains("G1 X"));
}
#[test]
fn writer_is_stable() {
    assert_eq!(
        write_move(&Move {
            x: Some(1.0),
            y: Some(2.0),
            feed_rate: Some(600.0),
            power: Some(0.0),
            rapid: false
        }),
        "G1 X1.000 Y2.000 F600 S0"
    );
}

#[test]
fn outline_writer_flips_y_and_preview_is_nonblank() {
    let paths = vec![vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]];
    let options = WriterOptions {
        width_mm: 20.0,
        height_mm: 20.0,
        power: 120,
        feed_rate: 600,
        auto_close: true,
        laser_mode: "M4",
        power_scale: None,
    };
    let output = write_outline(&paths, &options).unwrap();
    assert!(output.contains("G1 X10.000 Y20.000 S120 F600"));
    assert!(output.contains("G1 X0.000 Y20.000 S120\n"));
    let preview = preview_outline(&paths, 100, 100, 20.0, 20.0).unwrap();
    assert!(preview.pixels().any(|pixel| pixel.0[0] == 0));
}

#[test]
fn preview_with_huge_coordinates_finishes_quickly_and_stays_bounded() {
    // X1e12 让旧实现的单段步数饱和到 u32::MAX（约 43 亿次循环），预览接口会被拖死
    let moves = parse_moves("G0 X0 Y2\nG1 X1e12 Y2 S500\nG1 X1e12 Y-1e12\n");
    let path: Vec<_> = moves
        .iter()
        .map(|m| (m.x.unwrap_or(0.0), m.y.unwrap_or(0.0)))
        .collect();
    assert_eq!(path[1], (1e12, 2.0));
    let paths = vec![path];
    let started = std::time::Instant::now();
    let gray = preview_outline(&paths, 101, 101, 10.0, 10.0).unwrap();
    // 可见部分仍按原位置绘制：y=2mm 那条线穿过整幅画布
    assert_eq!(gray.get_pixel(50, 20).0[0], 0);
    assert_eq!(gray.get_pixel(100, 20).0[0], 0);
    assert_eq!(gray.get_pixel(50, 80).0[0], 255);
    let rgba = preview_outline_rgba_with_options(&paths, 800, 600, 10.0, 10.0, false).unwrap();
    assert_eq!(rgba.dimensions(), (800, 600));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "预览耗时 {:?}",
        started.elapsed()
    );
    // 完全落在画布外的超大线段直接跳过
    let outside = vec![vec![(-1e15, -1e15), (-1e12, -2e12)]];
    let blank = preview_outline(&outside, 50, 50, 10.0, 10.0).unwrap();
    assert!(blank.pixels().all(|p| p.0[0] == 255));
}

fn all_on_circle(points: &[(f64, f64)], center: (f64, f64), radius: f64) -> bool {
    points
        .iter()
        .all(|p| ((p.0 - center.0).hypot(p.1 - center.1) - radius).abs() < 1e-6)
}

#[test]
fn preview_toolpath_tessellates_quarter_arc_with_ij() {
    let tp = preview_toolpath("G21 G90\nG0 X10 Y0\nM4 S500\nG3 X0 Y10 I-10 J0 F600\nM5");
    assert_eq!(tp.arcs_simplified, 0);
    assert_eq!(tp.paths.len(), 1);
    let path = &tp.paths[0];
    assert_eq!(path[0], (10.0, 0.0));
    assert_eq!(*path.last().unwrap(), (0.0, 10.0));
    assert!(
        path.len() > 9,
        "四分之一圆至少按 10° 一段拆：{}",
        path.len()
    );
    assert!(all_on_circle(path, (0.0, 0.0), 10.0));
    // 逆时针：所有点都在第一象限
    assert!(path.iter().all(|p| p.0 >= -1e-9 && p.1 >= -1e-9));
    let (min_x, min_y, max_x, max_y) = tp.bounds.unwrap();
    assert!(min_x.abs() < 1e-9 && min_y.abs() < 1e-9);
    assert!((max_x - 10.0).abs() < 1e-9 && (max_y - 10.0).abs() < 1e-9);
    // 顺时针同一对端点走另外 270°
    let cw = preview_toolpath("G0 X10 Y0\nG2 X0 Y10 I-10 J0");
    let (min_x, min_y, ..) = cw.bounds.unwrap();
    assert!((min_x + 10.0).abs() < 0.05 && (min_y + 10.0).abs() < 0.05);
}

#[test]
fn preview_toolpath_draws_full_circle_and_respects_units_and_relative_moves() {
    let tp = preview_toolpath("G0 X10 Y0\nG2 X10 Y0 I-10 J0");
    let path = &tp.paths[0];
    assert!(all_on_circle(path, (0.0, 0.0), 10.0));
    assert_eq!(*path.last().unwrap(), (10.0, 0.0));
    let (min_x, min_y, max_x, max_y) = tp.bounds.unwrap();
    for (value, expected) in [(min_x, -10.0), (min_y, -10.0), (max_x, 10.0), (max_y, 10.0)] {
        assert!((value - expected).abs() < 0.05, "{value} vs {expected}");
    }
    // 英制 + 相对坐标：G91 下终点相对当前点，I/J 也按英寸换算
    let inch = preview_toolpath("G20 G91\nG0 X1\nG3 X-1 Y1 I-1 J0");
    let path = &inch.paths[0];
    assert!(all_on_circle(path, (0.0, 0.0), 25.4));
    let last = *path.last().unwrap();
    assert!(last.0.abs() < 1e-9 && (last.1 - 25.4).abs() < 1e-9);
    // 不写 G 的模态行也照常连线
    let modal = preview_toolpath("G1 X5 Y0\nX5 Y5\nX0 Y5");
    assert_eq!(
        modal.paths[0],
        vec![(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (0.0, 5.0)]
    );
}

#[test]
fn preview_toolpath_r_form_picks_short_or_long_arc() {
    let r = 50f64.sqrt();
    // G2 从 (0,0) 到 (10,0)：R>0 圆心在弦右侧 (5,-5)，走上方 90°；R<0 圆心 (5,5)，绕上方走 270°
    let short = preview_toolpath(&format!("G1 X0 Y0\nG2 X10 Y0 R{r}"));
    assert!(all_on_circle(&short.paths[0], (5.0, -5.0), r));
    let (_, min_y, _, max_y) = short.bounds.unwrap();
    assert!(min_y.abs() < 1e-9 && (max_y - (r - 5.0)).abs() < 0.05);
    let long = preview_toolpath(&format!("G1 X0 Y0\nG2 X10 Y0 R-{r}"));
    assert!(all_on_circle(&long.paths[0], (5.0, 5.0), r));
    let (_, min_y, _, max_y) = long.bounds.unwrap();
    assert!(min_y.abs() < 1e-9 && (max_y - (5.0 + r)).abs() < 0.05);
    assert!(
        long.paths[0].iter().all(|p| p.1 >= -1e-9),
        "长弧不经过圆心下方"
    );
    assert!(long.paths[0].len() > short.paths[0].len());
    // G3 + R>0：圆心在弦左侧 (5,5)，走下方 90°
    let ccw = preview_toolpath(&format!("G1 X0 Y0\nG3 X10 Y0 R{r}"));
    assert!(all_on_circle(&ccw.paths[0], (5.0, 5.0), r));
    assert!(ccw.paths[0].iter().all(|p| p.1 <= 1e-9));
    // 半径不够跨过弦、起终点重合的 R 圆弧无法定圆：只画首尾连线
    let invalid = preview_toolpath("G0 X0 Y0\nG2 X10 Y0 R2\nG2 X10 Y0 R3");
    assert_eq!(invalid.arcs_simplified, 2);
    assert_eq!(invalid.paths[0], vec![(0.0, 0.0), (10.0, 0.0), (10.0, 0.0)]);
}

#[test]
fn preview_toolpath_bounds_arc_work() {
    // 每个大圆都按单段上限拆分，超出整份文件的点数预算后只画首尾连线
    let mut text = String::from("G0 X1000000 Y0\n");
    for _ in 0..2000 {
        text.push_str("G2 X1000000 Y0 I-1000000 J0\n");
    }
    // 超大半径与非 XY 平面的圆弧同样退回连线
    text.push_str("G2 X0 Y0 I-1e9 J0\nG18\nG2 X5 Y0 I1 J0\n");
    let started = std::time::Instant::now();
    let tp = preview_toolpath(&text);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    let points: usize = tp.paths.iter().map(Vec::len).sum();
    assert!(points <= MAX_PREVIEW_ARC_POINTS + 2010, "点数 {points}");
    let tessellated = MAX_PREVIEW_ARC_POINTS / MAX_PREVIEW_ARC_SEGMENTS;
    assert_eq!(tp.arcs_simplified, 2000 - tessellated + 2);
    // 拆分结果仍可按画布裁剪出图
    let rgba = preview_outline_rgba_with_options(&tp.paths, 200, 200, 10.0, 10.0, false).unwrap();
    assert_eq!(rgba.dimensions(), (200, 200));
}

#[test]
fn preview_preserves_top_left_source_coordinates() {
    let paths = vec![vec![(0.0, 2.0), (10.0, 2.0)]];
    let preview = preview_outline(&paths, 101, 101, 10.0, 10.0).unwrap();
    assert_eq!(preview.get_pixel(50, 20).0[0], 0);
    assert_eq!(preview.get_pixel(50, 80).0[0], 255);
    let output = write_outline(
        &paths,
        &WriterOptions {
            width_mm: 10.0,
            height_mm: 10.0,
            power: 120,
            feed_rate: 600,
            auto_close: false,
            laser_mode: "M4",
            power_scale: None,
        },
    )
    .unwrap();
    assert!(output.contains("G1 X10.000 Y8.000"));
}

#[test]
fn gcode_power_scale_detection_and_rescaling_work() {
    let original = "\
; Generated by laser-core
; GRBL laser mode expected: $30=1000, $32=1
G21
G90
M4 S0
G1 X10.000 Y20.000 S1000 F600
G1 X15.000 Y25.000 S120 F600
M5
";
    assert_eq!(detect_gcode_power_scale(original), Some(1000.0));
    let rescaled = rescale_gcode_power(original, 1000.0, 255.0);
    assert!(rescaled.contains("; GRBL laser mode expected: $30=255, $32=1"));
    assert!(rescaled.contains("M4 S0"));
    assert!(rescaled.contains("G1 X10.000 Y20.000 S255 F600"));
    assert!(rescaled.contains("G1 X15.000 Y25.000 S31 F600"));
    assert_eq!(detect_gcode_power_scale(&rescaled), Some(255.0));
}

#[test]
fn raster_writer_omits_redundant_m5_and_inherits_modal_feed_rate() {
    let bitmap = vec![vec![0, 0, 255, 0], vec![255, 0, 0, 255]];
    let plan = crate::raster::raster_plan(&bitmap, "scanline").unwrap();
    let options = RasterWriterOptions {
        pixel_size_mm: 0.1,
        overscan_mm: 1.0,
        work_area_width_mm: 100.0,
        work_area_height_mm: 100.0,
        power: 800,
        feed_rate: 3000,
        laser_mode: "M4",
        snake_scan: true,
        material: None,
        safety_note: None,
        power_scale: Some(1000),
        dot_width_mm: 0.0,
    };
    let gcode = write_raster(&plan, 4, 2, &options).unwrap();
    // 验证：中间各行换向处不含 M5，整个 G-code 仅在结束时出现一次 M5
    let m5_count = gcode.lines().filter(|l| l.trim() == "M5").count();
    assert_eq!(
        m5_count, 1,
        "M5 should only appear once at the very end of the job"
    );
    // 验证：F3000 仅在首条 G1 出现，后续 G1 模态继承不再输出 F3000
    let f_count = gcode.lines().filter(|l| l.contains("F3000")).count();
    // 开头有 G0 F3000，以及首个 G1 带 F3000，共 2 次
    assert_eq!(
        f_count, 2,
        "F3000 should only be output on initial G0 and first modal G1"
    );
}

#[test]
fn framing_gcode_generates_correct_box() {
    let gcode = generate_framing_gcode(10.0, 20.0, 50.0, 60.0, 1500, 1).unwrap();
    assert!(gcode.contains("G0 X10.000 Y20.000 F3000"));
    assert!(gcode.contains("M4 S1"));
    assert!(gcode.contains("G1 X50.000 Y20.000 F1500"));
    assert!(gcode.contains("G1 X50.000 Y60.000"));
    assert!(gcode.contains("G1 X10.000 Y60.000"));
    assert!(gcode.contains("G1 X10.000 Y20.000"));
    assert!(gcode.contains("M5 S0"));
    assert!(gcode.contains("M2"));

    let paths = vec![vec![(10.0, 10.0), (30.0, 40.0)]];
    let paths_gcode = generate_paths_framing_gcode(&paths, 100.0, 1200, 0).unwrap();
    assert!(paths_gcode.contains("G1 X30.000"));
    assert!(!paths_gcode.contains("M4 S"));
}

fn raster_options_for_tests() -> RasterWriterOptions {
    RasterWriterOptions {
        pixel_size_mm: 0.1,
        overscan_mm: 1.0,
        work_area_width_mm: 100.0,
        work_area_height_mm: 100.0,
        power: 800,
        feed_rate: 1500,
        laser_mode: "M4",
        snake_scan: true,
        material: None,
        safety_note: None,
        power_scale: Some(1000),
        dot_width_mm: 0.0,
    }
}

#[test]
fn dot_width_correction_shortens_each_run_and_rejects_width_not_below_interval() {
    let bitmap = vec![vec![255, 0, 0, 0, 255, 0, 255]];
    let plan = crate::raster::raster_plan(&bitmap, "scanline").unwrap();
    let plain = write_raster(&plan, 7, 1, &raster_options_for_tests()).unwrap();
    // 不补偿：第一段 X1.1→1.4，第二段 X1.5→1.6
    assert!(plain.contains("G1 X1.100 S0") && plain.contains("G1 X1.400 S800"));
    let corrected = write_raster(
        &plan,
        7,
        1,
        &RasterWriterOptions {
            dot_width_mm: 0.04,
            ..raster_options_for_tests()
        },
    )
    .unwrap();
    // 两端各缩 0.02 mm：1.12→1.38、1.52→1.58；过扫描范围不变
    for expected in [
        "G1 X1.120 S0",
        "G1 X1.380 S800",
        "G1 X1.520 S0",
        "G1 X1.580 S800",
        "G0 X0.100 Y0.000",
        "G1 X2.600 S0",
    ] {
        assert!(corrected.contains(expected), "缺 {expected}:\n{corrected}");
    }
    let mut segment = plan.clone();
    segment.output_strategy = "segment";
    let seg = write_raster(
        &segment,
        7,
        1,
        &RasterWriterOptions {
            dot_width_mm: 0.04,
            ..raster_options_for_tests()
        },
    )
    .unwrap();
    assert!(seg.contains("G1 X1.120 S0") && seg.contains("G1 X1.380 S800"));
    for bad in [0.1, 0.2, -0.01, f64::NAN] {
        let options = RasterWriterOptions {
            dot_width_mm: bad,
            ..raster_options_for_tests()
        };
        assert!(write_raster(&plan, 7, 1, &options).is_err(), "{bad}");
        assert!(write_grayscale_raster(&[vec![0, 1]], &[0, 500], &options).is_err());
    }
}

#[test]
fn grayscale_writer_varies_power_per_run_and_stays_in_bounds() {
    // 两行：下面一行先刻（机器 Y 自下而上），第二条扫描线反向
    let levels = vec![vec![0, 3, 3, 1, 0, 2], vec![2, 2, 0, 0, 3, 0]];
    let table = crate::raster::grayscale_power_table(4, 200, 800);
    assert_eq!(table, vec![0, 400, 600, 800]);
    let gcode = write_grayscale_raster(&levels, &table, &raster_options_for_tests()).unwrap();
    assert!(gcode.contains("; Raster mode: grayscale variable power (4 levels)"));
    assert!(gcode.contains("; GRBL laser mode expected: $30=1000, $32=1"));
    let body: Vec<&str> = gcode
        .lines()
        .skip_while(|l| !l.starts_with("G0 X"))
        .collect();
    assert_eq!(
        body,
        vec![
            // 图像第 2 行（y = 0）正向：X1.0~1.2 S600，空白，X1.4~1.5 S800
            "G0 X0.000 Y0.000",
            "G1 X1.000 S0 F1500",
            "G1 X1.200 S600",
            "G1 X1.400 S0",
            "G1 X1.500 S800",
            "G1 X2.500 S0",
            // 图像第 1 行（y = 0.1）反向；相邻不同功率直接换 S，不插空走
            "G0 X2.600 Y0.100",
            "G1 X1.600 S0",
            "G1 X1.500 S600",
            "G1 X1.400 S0",
            "G1 X1.300 S400",
            "G1 X1.100 S800",
            "G1 X0.100 S0",
            "M5",
            "M2",
        ]
    );
    let powers: Vec<i32> = gcode
        .lines()
        .filter_map(|l| l.split_whitespace().find_map(|t| t.strip_prefix('S')))
        .filter_map(|s| s.parse().ok())
        .collect();
    assert!(powers.iter().all(|p| (0..=800).contains(p)));

    // 点宽补偿只缩连续出光区的外端
    let trimmed = write_grayscale_raster(
        &[vec![0, 3, 1, 0]],
        &table,
        &RasterWriterOptions {
            dot_width_mm: 0.02,
            ..raster_options_for_tests()
        },
    )
    .unwrap();
    for expected in ["G1 X1.110 S0", "G1 X1.200 S800", "G1 X1.290 S400"] {
        assert!(trimmed.contains(expected), "缺 {expected}:\n{trimmed}");
    }
    // 功率表超出本次最大功率、级数越界、全白
    assert!(
        write_grayscale_raster(&levels, &[0, 400, 600, 900], &raster_options_for_tests()).is_err()
    );
    assert!(write_grayscale_raster(&levels, &[0, 400], &raster_options_for_tests()).is_err());
    let blank = write_grayscale_raster(&[vec![0, 0]], &table, &raster_options_for_tests()).unwrap();
    assert!(!blank.lines().any(|l| l.starts_with("G1 ")), "{blank}");
}

#[test]
fn write_raster_vertical_and_nan_options_never_panic() {
    let bitmap = vec![vec![0, 255], vec![0, 0], vec![0, 255], vec![255, 0]];
    let mut plan = crate::raster::raster_plan(&bitmap, "scanline").unwrap();
    plan.selected_axis = "vertical";
    let mut options = RasterWriterOptions {
        pixel_size_mm: 0.5,
        overscan_mm: 1.0,
        work_area_width_mm: 100.0,
        work_area_height_mm: 100.0,
        power: 600,
        feed_rate: 1800,
        laser_mode: "M4",
        snake_scan: true,
        material: None,
        safety_note: None,
        power_scale: Some(1000),
        dot_width_mm: 0.0,
    };
    let gcode = write_raster(&plan, 2, 4, &options).unwrap();
    assert!(gcode.contains("G0 X0.000 Y"), "{gcode}");
    assert!(gcode.contains("G1 Y1.500 S0 F1800"), "{gcode}");
    assert!(gcode.contains("G0 X0.500 Y"), "{gcode}");

    options.overscan_mm = f64::NAN;
    assert!(write_raster(&plan, 2, 4, &options).is_err());
}

#[test]
fn acceleration_aware_estimate_is_slower_for_short_back_and_forth_rows() {
    // 一行 100 mm 长直线：加减速只占两头，与匀速估算接近
    let long = "G21 G90 M4 S0\nG1 X100 Y0 S500 F6000\nM5";
    // 快移 6000 mm/min（GRBL 里加工速度也不能超过它）
    let flat = estimate_time(long, 6000.0);
    let profile = MotionProfile {
        rapid_mm_per_min: 6000.0,
        ..MotionProfile::default()
    };
    let planned = estimate_time_with(long, &profile);
    assert!((flat.estimated_seconds - 1.0).abs() < 0.05, "{flat:?}");
    // 100 mm/s、a=1000：加速与减速各 0.1 s、各 5 mm，巡航 90 mm 0.9 s，共 1.1 s
    assert!(
        (planned.estimated_seconds - 1.1).abs() < 0.05,
        "{planned:?}"
    );

    // 100 行 2 mm 的来回扫描：每行都要掉头停住，匀速估算严重偏短
    let mut rows = String::from("G21 G90 M4 S0\n");
    for i in 0..100 {
        let x = if i % 2 == 0 { 2 } else { 0 };
        rows.push_str(&format!(
            "G1 X{x} Y{} S500 F6000\nG0 Y{}\n",
            i as f64 * 0.1,
            (i + 1) as f64 * 0.1
        ));
    }
    rows.push_str("M5\n");
    let flat = estimate_time(&rows, 6000.0);
    let planned = estimate_time_with(&rows, &profile);
    assert!(
        planned.estimated_seconds > flat.estimated_seconds * 2.0,
        "{flat:?} vs {planned:?}"
    );
    // 出光距离两者相同
    assert_eq!(flat.laser_distance_mm, planned.laser_distance_mm);

    // 加速度越大越快；轴最大速度封顶会变慢
    let fast = estimate_time_with(
        &rows,
        &MotionProfile {
            accel_x_mm_s2: 5000.0,
            accel_y_mm_s2: 5000.0,
            ..profile
        },
    );
    assert!(fast.estimated_seconds < planned.estimated_seconds);
    let capped = estimate_time_with(
        long,
        &MotionProfile {
            max_rate_x_mm_per_min: Some(3000.0),
            ..profile
        },
    );
    assert!(
        capped.estimated_seconds > estimate_time_with(long, &profile).estimated_seconds * 1.5,
        "{capped:?}"
    );

    // 直线上连续的共线小段不减速；90° 转角要减速
    let straight = "G21 G90 M4 S0\nG1 X50 Y0 S500 F6000\nG1 X100 Y0\nM5";
    assert!((estimate_time_with(straight, &profile).estimated_seconds - 1.1).abs() < 0.05);
    let corner = "G21 G90 M4 S0\nG1 X50 Y0 S500 F6000\nG1 X50 Y50\nM5";
    assert!(estimate_time_with(corner, &profile).estimated_seconds > 1.15);

    // 停顿照算
    let dwell = "G21 G90 M4 S0\nG4 P2\nM5";
    assert_eq!(estimate_time_with(dwell, &profile).estimated_seconds, 2.0);
}

#[test]
fn burn_hull_follows_lit_moves_only_and_outline_framing_closes() {
    // 一个三角形出光，两头还有关光的过扫描滑行：凸包只取出光点，以激光头起点为原点（G92）
    let text = "G21\nG90\nG92 X10 Y10 Z0\nM4 S0\nG0 X-5 Y0\nG1 X0 Y0 S0 F1000\nG1 X20 Y0 S500\nG1 X10 Y15 S500\nG1 X0 Y0 S500\nG1 X-5 Y0 S0\nM5\nM2\n";
    let hull = burn_hull(text).unwrap();
    assert_eq!(hull.len(), 3, "{hull:?}");
    for corner in [(-10.0, -10.0), (10.0, -10.0), (0.0, 5.0)] {
        assert!(
            hull.iter()
                .any(|p| (p.0 - corner.0).abs() < 1e-9 && (p.1 - corner.1).abs() < 1e-9),
            "{hull:?} 缺 {corner:?}"
        );
    }
    // 圆弧只知道端点：不给轮廓，退回矩形
    assert!(burn_hull("G21\nM4 S0\nG1 X10 Y0 S500 F600\nG2 X0 Y0 I-5 J0\nM5").is_none());
    assert!(
        burn_hull("G21\nM4 S0\nG1 X10 Y0 S500 F600\nM5").is_none(),
        "一条线没有面积"
    );

    let framing = generate_outline_framing_gcode(&hull, 2000, 0).unwrap();
    let moves: Vec<&str> = framing.lines().filter(|l| l.starts_with("G1 ")).collect();
    assert_eq!(moves.len(), 3, "{framing}");
    assert!(moves[0].contains("F2000"));
    assert!(!framing.contains("M4"), "功率 0 不出光");
    assert!(framing.trim_end().ends_with("G0 X0 Y0 F3000\nM2"));
    let lit = generate_outline_framing_gcode(&hull, 2000, 10).unwrap();
    assert!(lit.contains("M4 S10"));
    assert!(generate_outline_framing_gcode(&hull[..2], 2000, 0).is_err());
    assert!(
        generate_outline_framing_gcode(&[(0.0, 0.0), (f64::NAN, 1.0), (1.0, 1.0)], 2000, 0)
            .is_err()
    );
}

#[test]
fn steps_write_travel_coast_and_burn_with_flipped_y() {
    let options = WriterOptions {
        width_mm: 10.0,
        height_mm: 10.0,
        power: 500,
        feed_rate: 1200,
        auto_close: false,
        laser_mode: "M4",
        power_scale: None,
    };
    let gcode = write_steps(
        &[
            Step::Travel((-1.0, 2.0)),
            Step::Coast((0.0, 2.0)),
            Step::Burn((5.0, 2.0)),
            Step::Coast((6.0, 2.0)),
            Step::Travel((6.0, 3.0)),
            Step::Burn((1.0, 3.0)),
        ],
        &options,
    )
    .unwrap();
    let body: Vec<&str> = gcode
        .lines()
        .skip_while(|l| !l.starts_with("G0 F3000"))
        .skip(1)
        .collect();
    assert_eq!(
        body,
        vec![
            "G0 X-1.000 Y8.000",
            "G1 X0.000 Y8.000 S0 F1200",
            "G1 X5.000 Y8.000 S500",
            "G1 X6.000 Y8.000 S0",
            "G0 X6.000 Y7.000",
            "G1 X1.000 Y7.000 S500",
            "S0",
            "M5",
            "M2",
        ]
    );
    assert!(write_steps(&[Step::Burn((f64::NAN, 0.0))], &options).is_err());
}
