use super::*;

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

fn fill(spacing: f64) -> FillOptions {
    FillOptions::new(spacing, FillRule::NonZero)
}

fn individually(spacing: f64) -> FillOptions {
    FillOptions {
        grouping: FillGrouping::Individually,
        ..fill(spacing)
    }
}

fn block(unit: &Unit) -> &ScanBlock {
    match unit {
        Unit::Scan(block) => block,
        other => panic!("{other:?}"),
    }
}

fn point(step: &Step) -> Point {
    match *step {
        Step::Travel(p) | Step::Coast(p) | Step::Burn(p) => p,
    }
}

#[test]
fn holes_are_not_filled_under_the_nonzero_rule() {
    // “口”：外框逆时针、内孔顺时针
    let contours = vec![square(0.0, 0.0, 10.0, true), square(3.0, 3.0, 4.0, false)];
    let units = scan_fill(&contours, &individually(1.0)).unwrap();
    let planned = plan(units.clone(), (0.0, 0.0), &PlanOptions::default()).unwrap();
    for path in &planned.paths {
        let (a, b) = (path[0], path[path.len() - 1]);
        if a.1 > 3.0 && a.1 < 7.0 {
            let (lo, hi) = (a.0.min(b.0), a.0.max(b.0));
            assert!(hi <= 3.0 + 1e-9 || lo >= 7.0 - 1e-9, "{a:?} {b:?}");
        }
    }
    // 孔把中间几行分成左右两块，上下各一块：共 4 块
    assert_eq!(units.len(), 4, "{units:?}");
    assert_eq!(
        units.iter().map(|u| block(u).rows()).sum::<usize>(),
        3 + 3 + 4 + 4
    );
    // 一次填完：一整块，孔所在的行每行两段
    let all = scan_fill(&contours, &fill(1.0)).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(block(&all[0]).rows(), 10);
    assert_eq!(block(&all[0]).spans(), 14);
}

#[test]
fn contours_without_a_repeated_end_point_are_still_filled() {
    // 字体轮廓常不重复首点：漏掉闭合边会交点成单、整片漏填
    let open_square = vec![vec![(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (0.0, 5.0)]];
    let units = scan_fill(&open_square, &fill(1.0)).unwrap();
    assert_eq!(block(&units[0]).spans(), 5);
}

#[test]
fn individual_blocks_are_finished_before_moving_to_the_next() {
    // 两个分开的方块（两个字）：各成一块，两块之间只空走一次
    let contours = vec![square(0.0, 0.0, 5.0, true), square(20.0, 0.0, 5.0, true)];
    let units = scan_fill(&contours, &individually(0.5)).unwrap();
    assert_eq!(units.len(), 2);
    let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
    let long_travels = planned
        .steps
        .windows(2)
        .filter(|w| matches!(w[1], Step::Travel(_)) && distance(point(&w[0]), point(&w[1])) > 1.0)
        .count();
    assert_eq!(long_travels, 1, "{:?}", planned.steps);
}

#[test]
fn fill_all_at_once_sweeps_each_row_across_every_shape_with_overscan() {
    // LightBurn 默认：每行横穿两个方块，中间关光匀速滑过；两头各多走 2 mm 关光
    let contours = vec![square(0.0, 0.0, 5.0, true), square(20.0, 0.0, 5.0, true)];
    let options = FillOptions {
        overscan_mm: 2.0,
        ..fill(1.0)
    };
    let units = scan_fill(&contours, &options).unwrap();
    assert_eq!(units.len(), 1);
    let planned = plan(units, (-2.0, 0.0), &PlanOptions::default()).unwrap();
    // 每行：空走到 -2 → 滑到 0 → 出光到 5 → 滑到 20 → 出光到 25 → 滑到 27
    let first_row: Vec<Step> = planned.steps[..6].to_vec();
    assert_eq!(
        first_row,
        vec![
            Step::Travel((-2.0, 0.5)),
            Step::Coast((0.0, 0.5)),
            Step::Burn((5.0, 0.5)),
            Step::Coast((20.0, 0.5)),
            Step::Burn((25.0, 0.5)),
            Step::Coast((27.0, 0.5)),
        ]
    );
    // 下一行反向（双向蛇形），从右侧过扫描处开始
    assert_eq!(planned.steps[6], Step::Travel((27.0, 1.5)));
    assert_eq!(planned.steps[7], Step::Coast((25.0, 1.5)));
    // 出光长度不受过扫描影响
    assert!((burn_length(&planned.paths) - 5.0 * 10.0).abs() < 1e-9);
    assert_eq!(options.extension_mm(), (2.0, 0.0));
}

#[test]
fn unidirectional_scans_every_row_the_same_way() {
    let options = FillOptions {
        bidirectional: false,
        ..fill(1.0)
    };
    let units = scan_fill(&[square(0.0, 0.0, 4.0, true)], &options).unwrap();
    let planned = plan(units, (10.0, 10.0), &PlanOptions::default()).unwrap();
    for pair in planned.steps.chunks(2) {
        let (Step::Travel(a), Step::Burn(b)) = (pair[0], pair[1]) else {
            panic!("{pair:?}")
        };
        assert!(b.0 > a.0, "每行都从左往右出光：{a:?} → {b:?}");
    }
}

#[test]
fn scan_offset_shifts_forward_and_return_rows_against_their_travel() {
    let options = FillOptions {
        scan_offset_mm: 0.2,
        ..fill(1.0)
    };
    let units = scan_fill(&[square(0.0, 0.0, 4.0, true)], &options).unwrap();
    let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
    // 第一行往右：整体左挪 0.1；第二行往左：整体右挪 0.1
    let expected = [(-0.1, 0.5), (3.9, 0.5), (4.1, 1.5), (0.1, 1.5)];
    for (step, want) in planned.steps.iter().zip(expected) {
        assert!(distance(point(step), want) < 1e-9, "{step:?} vs {want:?}");
    }
    assert!(matches!(planned.steps[2], Step::Travel(_)));
    assert!(matches!(planned.steps[3], Step::Burn(_)));
}

#[test]
fn scan_offset_table_interpolates_at_the_job_speed() {
    let table = [(1000.0, 0.05), (3000.0, 0.15), (6000.0, 0.3)];
    assert!((scan_offset_at(&table, 2000.0).unwrap() - 0.1).abs() < 1e-12);
    assert!((scan_offset_at(&table, 4500.0).unwrap() - 0.225).abs() < 1e-12);
    assert_eq!(scan_offset_at(&table, 3000.0).unwrap(), 0.15);
    // 两端取端点值
    assert_eq!(scan_offset_at(&table, 500.0).unwrap(), 0.05);
    assert_eq!(scan_offset_at(&table, 9000.0).unwrap(), 0.3);
    assert_eq!(scan_offset_at(&[(1500.0, -0.1)], 99.0).unwrap(), -0.1);
    // 无效表
    assert!(scan_offset_at(&[], 1000.0).is_err());
    assert!(scan_offset_at(&[(2000.0, 0.1), (1000.0, 0.2)], 1500.0).is_err());
    assert!(scan_offset_at(&[(1000.0, 0.1), (1000.0, 0.2)], 1500.0).is_err());
    assert!(scan_offset_at(&[(1000.0, 3.0)], 1500.0).is_err());
    assert!(scan_offset_at(&[(0.0, 0.1)], 1500.0).is_err());
    assert!(scan_offset_at(&vec![(1.0, 0.0); 33], 1.0).is_err());
}

#[test]
fn scan_angle_and_cross_hatch_rotate_the_lines() {
    let vertical = FillOptions {
        angle_deg: 90.0,
        ..fill(1.0)
    };
    let units = scan_fill(&[square(0.0, 0.0, 4.0, true)], &vertical).unwrap();
    let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
    for path in &planned.paths {
        assert!((path[0].0 - path[1].0).abs() < 1e-9, "90° 竖扫：{path:?}");
    }
    let cross = FillOptions {
        cross_hatch: true,
        ..fill(1.0)
    };
    let units = scan_fill(&[square(0.0, 0.0, 4.0, true)], &cross).unwrap();
    assert_eq!(units.len(), 2, "交叉排线多一遍转 90° 的扫描");
    let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
    assert!((burn_length(&planned.paths) - 32.0).abs() < 1e-6);
}

#[test]
fn planning_keeps_every_burn_and_shortens_travel() {
    // 一排打乱顺序的短线：规划后出光长度不变、空走明显变短
    let mut units = Vec::new();
    for i in [7, 2, 9, 0, 5, 3, 8, 1, 6, 4] {
        let x = f64::from(i) * 10.0;
        units.push(Unit::Open(vec![(x, 0.0), (x + 5.0, 0.0)]));
    }
    let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
    assert!((burn_length(&planned.paths) - 50.0).abs() < 1e-9);
    assert!(
        planned.stats.travel_distance_after < planned.stats.travel_distance_before * 0.5,
        "{:?}",
        planned.stats
    );
    // 开放线段不被闭合
    assert!(planned.paths.iter().all(|path| path.len() == 2));
}

#[test]
fn closed_contours_start_at_the_nearest_corner() {
    let planned = plan(
        vec![Unit::Closed(square(10.0, 10.0, 5.0, true))],
        (16.0, 16.0),
        &PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(planned.paths[0][0], (15.0, 15.0));
    assert_eq!(planned.paths[0].first(), planned.paths[0].last());

    // 一条边上加了个共线的中间点：它离得最近，但不是尖角，起刀点仍放在角上
    let mut with_mid = square(0.0, 0.0, 10.0, true);
    with_mid.insert(1, (5.0, 0.0));
    let corner = plan(
        vec![Unit::Closed(with_mid.clone())],
        (5.0, -1.0),
        &PlanOptions::default(),
    )
    .unwrap();
    assert!(
        [(0.0, 0.0), (10.0, 0.0)].contains(&corner.paths[0][0]),
        "{:?}",
        corner.paths[0]
    );
    let nearest = plan(
        vec![Unit::Closed(with_mid)],
        (5.0, -1.0),
        &PlanOptions {
            prefer_corners: false,
            ..PlanOptions::default()
        },
    )
    .unwrap();
    assert_eq!(nearest.paths[0][0], (5.0, 0.0));
}

#[test]
fn duplicate_lines_are_burned_once() {
    // 同一个方块画了两遍（一遍换了起点、反了方向），一条线段正反各一遍
    let mut rotated = square(0.0, 0.0, 5.0, false);
    rotated.pop();
    rotated.rotate_left(2);
    rotated.push(rotated[0]);
    let units = vec![
        Unit::Closed(square(0.0, 0.0, 5.0, true)),
        Unit::Closed(rotated),
        Unit::Open(vec![(10.0, 0.0), (20.0, 0.0)]),
        Unit::Open(vec![(20.0, 0.0), (10.0, 0.0)]),
    ];
    let planned = plan(units.clone(), (0.0, 0.0), &PlanOptions::default()).unwrap();
    assert_eq!(planned.removed_duplicates, 2);
    assert!((burn_length(&planned.paths) - 30.0).abs() < 1e-9);
    let kept = plan(
        units,
        (0.0, 0.0),
        &PlanOptions {
            remove_overlaps: false,
            ..PlanOptions::default()
        },
    )
    .unwrap();
    assert_eq!(kept.removed_duplicates, 0);
    assert!((burn_length(&kept.paths) - 60.0).abs() < 1e-9);
}

#[test]
fn cutting_finishes_inner_contours_before_the_outer_one() {
    // 外框离起点最近，但里面两个孔必须先切
    let units = vec![
        Unit::Closed(square(0.0, 0.0, 30.0, true)),
        Unit::Closed(square(5.0, 5.0, 5.0, true)),
        Unit::Closed(square(18.0, 18.0, 5.0, true)),
    ];
    let planned = plan(
        units,
        (0.0, 0.0),
        &PlanOptions {
            inside_first: true,
            ..PlanOptions::default()
        },
    )
    .unwrap();
    let outer_at = planned
        .paths
        .iter()
        .position(|path| path.iter().any(|p| p.0 == 30.0))
        .unwrap();
    assert_eq!(outer_at, 2, "{:?}", planned.paths);
}

#[test]
fn layers_continue_from_where_the_previous_layer_ended() {
    let contours = vec![square(0.0, 0.0, 4.0, true)];
    let fills = scan_fill(&contours, &fill(1.0)).unwrap();
    let outline = vec![Unit::Closed(contours[0].clone())];
    let planned = plan_layers(vec![fills, outline], (0.0, 0.0), &PlanOptions::default()).unwrap();
    // 填充 4 行 + 轮廓 1 圈
    assert_eq!(planned.paths.len(), 5);
    assert!((burn_length(&planned.paths) - (16.0 + 16.0)).abs() < 1e-9);
    assert!(plan_layers(vec![Vec::new()], (0.0, 0.0), &PlanOptions::default()).is_err());
}

#[test]
fn auto_overscan_follows_speed_and_acceleration() {
    // 3000 mm/min = 50 mm/s，a = 1000 → 1.25 mm → 取整 1.3
    assert_eq!(auto_overscan_mm(3000.0, 1000.0), 1.3);
    // 慢速取下限 1 mm，极快取上限 20 mm
    assert_eq!(auto_overscan_mm(600.0, 1000.0), 1.0);
    assert_eq!(auto_overscan_mm(60_000.0, 100.0), 20.0);
    // 加速度无效时用默认 1000
    assert_eq!(auto_overscan_mm(3000.0, f64::NAN), 1.3);
}

#[test]
fn invalid_input_is_rejected() {
    assert!(plan(Vec::new(), (0.0, 0.0), &PlanOptions::default()).is_err());
    assert!(
        plan(
            vec![Unit::Open(vec![(f64::NAN, 0.0), (1.0, 0.0)])],
            (0.0, 0.0),
            &PlanOptions::default()
        )
        .is_err()
    );
    assert!(scan_fill(&[square(0.0, 0.0, 1.0, true)], &fill(0.0)).is_err());
    let too_far = FillOptions {
        overscan_mm: 50.0,
        ..fill(0.1)
    };
    assert!(scan_fill(&[square(0.0, 0.0, 1.0, true)], &too_far).is_err());
}
