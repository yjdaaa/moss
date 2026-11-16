use super::*;
use crate::vector_plan::{PlanOptions, Unit, burn_length, burn_paths, plan, point_in_polygon};

fn square(x: f64, y: f64, size: f64, ccw: bool) -> Vec<Point> {
    let mut points = vec![
        (x, y),
        (x + size, y),
        (x + size, y + size),
        (x, y + size),
        (x, y),
    ];
    if !ccw {
        points.reverse();
    }
    points
}

fn bbox(path: &[Point]) -> (f64, f64, f64, f64) {
    path.iter().fold(
        (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ),
        |(a, b, c, d), p| (a.min(p.0), b.min(p.1), c.max(p.0), d.max(p.1)),
    )
}

fn area(path: &[Point]) -> f64 {
    signed_area(&path[..path.len() - 1]).abs()
}

#[test]
fn kerf_grows_and_shrinks_a_square() {
    let grown = offset_contours(&[square(0.0, 0.0, 10.0, true)], 0.5).unwrap();
    assert_eq!((grown.before, grown.after), (1, 1));
    let (x0, y0, x1, y1) = bbox(&grown.contours[0]);
    assert!(
        (x0 + 0.5).abs() < 1e-9 && (y0 + 0.5).abs() < 1e-9,
        "{x0} {y0}"
    );
    assert!((x1 - 10.5).abs() < 1e-9 && (y1 - 10.5).abs() < 1e-9);
    // 凸角补圆弧：面积 = 11² − 4 个角的 (1 − π/4)·0.25
    let expected = 11.0 * 11.0 - (1.0 - std::f64::consts::PI / 4.0);
    assert!((area(&grown.contours[0]) - expected).abs() < 0.01);
    // 每个点到原正方形的距离都正好是 0.5
    for p in &grown.contours[0] {
        let dx = (p.0 - 5.0).abs() - 5.0;
        let dy = (p.1 - 5.0).abs() - 5.0;
        let d = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0);
        assert!((d - 0.5).abs() < 0.006, "{p:?} → {d}");
    }

    // 方向反了（顺时针）也一样往外
    let cw = offset_contours(&[square(0.0, 0.0, 10.0, false)], 0.5).unwrap();
    assert!((area(&cw.contours[0]) - expected).abs() < 0.01);

    let shrunk = offset_contours(&[square(0.0, 0.0, 10.0, true)], -0.5).unwrap();
    let (x0, y0, x1, y1) = bbox(&shrunk.contours[0]);
    assert!((x0 - 0.5).abs() < 1e-9 && (y0 - 0.5).abs() < 1e-9);
    assert!((x1 - 9.5).abs() < 1e-9 && (y1 - 9.5).abs() < 1e-9);
    assert!(
        (area(&shrunk.contours[0]) - 81.0).abs() < 1e-6,
        "内缩保持尖角"
    );
    assert_eq!(shrunk.contours[0].first(), shrunk.contours[0].last());
}

#[test]
fn holes_shrink_when_the_part_grows() {
    // “口”：外框 10 mm，孔 4 mm（字体里孔与外框方向相反）
    let part = vec![square(0.0, 0.0, 10.0, true), square(3.0, 3.0, 4.0, false)];
    let grown = offset_contours(&part, 0.25).unwrap();
    assert_eq!((grown.before, grown.after), (2, 2));
    let mut sizes: Vec<f64> = grown
        .contours
        .iter()
        .map(|c| {
            let (x0, _, x1, _) = bbox(c);
            x1 - x0
        })
        .collect();
    sizes.sort_by(f64::total_cmp);
    assert!((sizes[0] - 3.5).abs() < 1e-9, "孔变小：{sizes:?}");
    assert!((sizes[1] - 10.5).abs() < 1e-9, "外框变大：{sizes:?}");
    // 内缩：孔变大
    let shrunk = offset_contours(&part, -0.25).unwrap();
    let hole = shrunk
        .contours
        .iter()
        .map(|c| bbox(c))
        .map(|(x0, _, x1, _)| x1 - x0)
        .fold(f64::INFINITY, f64::min);
    assert!((hole - 4.5).abs() < 0.01, "{hole}");
}

#[test]
fn vanishing_and_overlapping_shapes_are_handled() {
    // 1 mm 的小方块内缩 0.6 mm 会消失：丢掉，不输出翻折的路径
    let shapes = vec![square(0.0, 0.0, 10.0, true), square(20.0, 0.0, 1.0, true)];
    let result = offset_contours(&shapes, -0.6).unwrap();
    assert_eq!((result.before, result.after), (2, 1));
    assert!(bbox(&result.contours[0]).2 < 10.0);
    // 全部消失：明确报错
    assert!(offset_contours(&[square(0.0, 0.0, 1.0, true)], -0.6).is_err());
    // 孔被外扩吃掉：只剩外框
    let thin_hole = vec![square(0.0, 0.0, 10.0, true), square(4.0, 4.0, 0.4, false)];
    assert_eq!(offset_contours(&thin_hole, 0.3).unwrap().after, 1);
    // 两个重叠的笔画先合成一个外轮廓再补偿
    let overlap = vec![square(0.0, 0.0, 4.0, true), square(2.0, 0.0, 4.0, true)];
    let merged = offset_contours(&overlap, 0.1).unwrap();
    assert_eq!((merged.before, merged.after), (1, 1));
    let (x0, _, x1, _) = bbox(&merged.contours[0]);
    assert!((x1 - x0 - 6.2).abs() < 1e-9);
    // 两个靠得很近的方块外扩后相连成一个（轮廓数变少会回显给用户）
    let close = vec![square(0.0, 0.0, 4.0, true), square(4.3, 0.0, 4.0, true)];
    assert_eq!(offset_contours(&close, 0.2).unwrap().after, 1);
    // 自交的“8”字（领结）按非零环绕补偿成两块
    let bowtie = vec![vec![(0.0, 0.0), (4.0, 4.0), (4.0, 0.0), (0.0, 4.0)]];
    let tie = offset_contours(&bowtie, -0.1).unwrap();
    assert_eq!(tie.after, 2);
    // 无效输入
    assert!(offset_contours(&[square(0.0, 0.0, 4.0, true)], 3.0).is_err());
    assert!(offset_contours(&[vec![(0.0, 0.0), (1.0, 0.0)]], 0.1).is_err());
    assert!(offset_contours(&[vec![(f64::NAN, 0.0), (1.0, 0.0), (1.0, 1.0)]], 0.1).is_err());
}

fn planned(units: Vec<Unit>) -> Vec<Step> {
    plan(
        units,
        (0.0, 0.0),
        &PlanOptions {
            inside_first: true,
            prefer_corners: true,
            remove_overlaps: true,
        },
    )
    .unwrap()
    .steps
}

#[test]
fn tabs_leave_laser_off_gaps_of_the_right_width() {
    let steps = planned(vec![Unit::Closed(square(0.0, 0.0, 10.0, true))]);
    let options = CutOptions {
        tabs: TabMode::Count(4),
        tab_width_mm: 1.0,
        ..CutOptions::default()
    };
    let (out, report) = apply_cut_options(&steps, &options).unwrap();
    assert_eq!(report.tabs, 4);
    assert!((report.tab_gap_mm - 4.0).abs() < 1e-9);
    // 4 段关光滑行，每段 1 mm，都在轮廓上
    let mut gaps = Vec::new();
    for pair in out.windows(2) {
        if let Step::Coast(b) = pair[1] {
            gaps.push(distance(step_point(&pair[0]), b));
        }
    }
    assert_eq!(gaps.len(), 4, "{out:?}");
    assert!(gaps.iter().all(|g| (g - 1.0).abs() < 1e-9), "{gaps:?}");
    // 桥位居中在每条边中点（周长 40，起点在角上，间距 10）
    let paths = burn_paths(&out);
    assert!((burn_length(&paths) - 36.0).abs() < 1e-9);
    // G-code 里桥位是 G1 … S0
    let gcode = crate::gcode::write_steps(
        &out,
        &crate::gcode::WriterOptions {
            width_mm: 10.0,
            height_mm: 10.0,
            power: 500,
            feed_rate: 600,
            auto_close: false,
            laser_mode: "M4",
            power_scale: None,
        },
    )
    .unwrap();
    assert_eq!(
        gcode
            .lines()
            .filter(|l| l.starts_with("G1 ") && l.contains(" S0"))
            .count(),
        4
    );

    // 按间距：周长 40、间距 15 → 3 个
    let spaced = CutOptions {
        tabs: TabMode::Spacing(15.0),
        ..options
    };
    assert_eq!(apply_cut_options(&steps, &spaced).unwrap().1.tabs, 3);
    // 桥位太宽放不下：这个轮廓不加
    let wide = CutOptions {
        tabs: TabMode::Count(30),
        tab_width_mm: 10.0,
        ..options
    };
    let (_, r) = apply_cut_options(&steps, &wide).unwrap();
    assert_eq!(r.tabs, 2, "总宽不超过周长一半：{r:?}");
}

#[test]
fn tabs_skip_inner_shapes_when_asked() {
    let steps = planned(vec![
        Unit::Closed(square(0.0, 0.0, 20.0, true)),
        Unit::Closed(square(5.0, 5.0, 5.0, false)),
    ]);
    let options = CutOptions {
        tabs: TabMode::Count(2),
        tab_width_mm: 0.5,
        ..CutOptions::default()
    };
    assert_eq!(apply_cut_options(&steps, &options).unwrap().1.tabs, 2);
    let all = CutOptions {
        tabs_skip_inner: false,
        ..options
    };
    assert_eq!(apply_cut_options(&steps, &all).unwrap().1.tabs, 4);
}

#[test]
fn lead_in_lead_out_and_overcut_geometry() {
    let steps = planned(vec![
        Unit::Closed(square(0.0, 0.0, 20.0, true)),
        Unit::Closed(square(5.0, 5.0, 6.0, true)),
    ]);
    let options = CutOptions {
        lead_in_mm: 2.0,
        lead_out_mm: 1.0,
        overcut_mm: 3.0,
        ..CutOptions::default()
    };
    let (out, report) = apply_cut_options(&steps, &options).unwrap();
    assert_eq!(
        (report.lead_ins, report.lead_outs, report.leads_skipped),
        (2, 2, 0)
    );
    assert!((report.overcut_mm - 6.0).abs() < 1e-9);
    let loops: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, Step::Travel(_)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(loops.len(), 2);
    // 先切的是内孔（先内后外）：引入线从孔里垂直切进起刀点
    let (from, start) = (step_point(&out[loops[0]]), step_point(&out[loops[0] + 1]));
    assert!((distance(from, start) - 2.0).abs() < 1e-9);
    assert!(
        point_in_polygon(from, &square(5.0, 5.0, 6.0, true)),
        "{from:?}"
    );
    // 外框的引入线在外面
    let (from, start) = (step_point(&out[loops[1]]), step_point(&out[loops[1] + 1]));
    assert!(
        !point_in_polygon(from, &square(0.0, 0.0, 20.0, true)),
        "{from:?}"
    );
    assert!((distance(from, start) - 2.0).abs() < 1e-9);
    // 过切：回到起点后又沿第一条边切了 3 mm，再切出 1 mm 引出线
    let last = step_point(&out[out.len() - 1]);
    let before_last = step_point(&out[out.len() - 2]);
    assert!(
        (distance(before_last, start) - 3.0).abs() < 1e-9,
        "{before_last:?} {start:?}"
    );
    assert!((distance(last, before_last) - 1.0).abs() < 1e-9);
    assert!(!point_in_polygon(last, &square(0.0, 0.0, 20.0, true)));
    let length = burn_length(&burn_paths(&out));
    assert!((length - (80.0 + 24.0 + 2.0 * (2.0 + 1.0 + 3.0))).abs() < 1e-9);

    // 斜着引入（30°）：长度不变，从起刀边后方斜着切进来；孔的角上斜线会落到零件一侧，这一条不加
    let angled = CutOptions {
        lead_angle_deg: 30.0,
        ..options
    };
    let (out, report) = apply_cut_options(&steps, &angled).unwrap();
    assert_eq!(
        (report.lead_ins, report.leads_skipped),
        (1, 1),
        "{report:?}"
    );
    let t = out
        .iter()
        .rposition(|s| matches!(s, Step::Travel(_)))
        .unwrap();
    let (from, start, next) = (
        step_point(&out[t]),
        step_point(&out[t + 1]),
        step_point(&out[t + 2]),
    );
    let dir = unit(sub(next, start));
    assert!(dot(unit(sub(start, from)), dir) > 0.9, "{from:?}");
    assert!((distance(from, start) - 2.0).abs() < 1e-9);
    assert!(!point_in_polygon(from, &square(0.0, 0.0, 20.0, true)));
}

#[test]
fn leads_that_would_hit_another_contour_are_skipped() {
    // 孔只有 1 mm 宽，3 mm 的引入线会切到对面的孔壁：不加
    let steps = planned(vec![
        Unit::Closed(square(0.0, 0.0, 20.0, true)),
        Unit::Closed(square(5.0, 5.0, 1.0, true)),
    ]);
    let options = CutOptions {
        lead_in_mm: 3.0,
        ..CutOptions::default()
    };
    let (_, report) = apply_cut_options(&steps, &options).unwrap();
    assert_eq!((report.lead_ins, report.leads_skipped), (1, 1));
}

#[test]
fn overcut_does_not_burn_through_a_tab_and_other_steps_are_untouched() {
    let steps = planned(vec![
        Unit::Closed(square(0.0, 0.0, 4.0, true)),
        Unit::Open(vec![(10.0, 0.0), (15.0, 0.0)]),
    ]);
    // 周长 16、1 个桥位在 7.75 ~ 8.25：过切 8 mm 会落进第二圈的桥位（23.75 起），停在桥位前
    let options = CutOptions {
        tabs: TabMode::Count(1),
        tab_width_mm: 0.5,
        overcut_mm: 8.0,
        ..CutOptions::default()
    };
    let (out, report) = apply_cut_options(&steps, &options).unwrap();
    assert!((report.overcut_mm - 7.75).abs() < 1e-9, "{report:?}");
    // 开放线段照原样保留
    assert!(out.contains(&Step::Burn((15.0, 0.0))) || out.contains(&Step::Burn((10.0, 0.0))));
    assert!(CutOptions::default().validate().is_ok());
    assert!(!CutOptions::default().is_active());
    assert!(
        CutOptions {
            lead_angle_deg: 0.0,
            lead_in_mm: 1.0,
            ..CutOptions::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        CutOptions {
            tabs: TabMode::Count(0),
            ..CutOptions::default()
        }
        .validate()
        .is_err()
    );
}
