//! 切割后处理：桥位、引入 / 引出线与过切，并自检出光长度。

use super::{
    CutOptions, CutReport, TabMode, add, cross, distance, dot, scale, signed_area, step_point, sub,
    unit,
};
use crate::gcode::Step;
use crate::safety::SafetyError;
use crate::vector_plan::{burn_length, burn_paths, point_in_polygon};
use laser_contracts::geometry::Point;

// ---------- 后处理：桥位、引线、过切 ----------

/// 规划结果里的闭合轮廓：`Travel(p0)` 后接至少 3 个 `Burn`、最后回到 p0（[`crate::vector_plan::plan`] 的写法）。
fn find_loops(steps: &[Step]) -> Vec<(usize, usize)> {
    let mut loops = Vec::new();
    let mut i = 0;
    while i < steps.len() {
        if let Step::Travel(p0) = steps[i] {
            let mut k = i + 1;
            while k < steps.len() && matches!(steps[k], Step::Burn(_)) {
                k += 1;
            }
            if k - i > 3 && distance(step_point(&steps[k - 1]), p0) <= 1e-9 {
                loops.push((i, k));
                i = k;
                continue;
            }
        }
        i += 1;
    }
    loops
}

/// 线段 a→b 碰到任何轮廓的边（交点离 `ignore` 超过 1 µm）。
fn crosses_any(a: Point, b: Point, rings: &[Vec<Point>], ignore: Point) -> bool {
    let r = sub(b, a);
    rings.iter().any(|ring| {
        ring.windows(2).any(|edge| {
            let (c, d) = (edge[0], edge[1]);
            let s = sub(d, c);
            let denom = cross(r, s);
            let qp = sub(c, a);
            if denom.abs() < 1e-15 {
                // 平行：共线重叠且不止在 ignore 这一点相碰就算碰到
                if cross(qp, r).abs() > 1e-9 * r.0.hypot(r.1).max(1.0) {
                    return false;
                }
                let rr = dot(r, r).max(1e-30);
                let (t0, t1) = (dot(sub(c, a), r) / rr, dot(sub(d, a), r) / rr);
                let (lo, hi) = (t0.min(t1).max(0.0), t0.max(t1).min(1.0));
                return lo <= hi
                    && (distance(add(a, scale(r, lo)), ignore) > 1e-6
                        || distance(add(a, scale(r, hi)), ignore) > 1e-6);
            }
            let t = cross(qp, s) / denom;
            let u = cross(qp, r) / denom;
            (0.0..=1.0).contains(&t)
                && (0.0..=1.0).contains(&u)
                && distance(add(a, scale(r, t)), ignore) > 1e-6
        })
    })
}

struct LoopGeometry<'a> {
    ring: &'a [Point],
    cum: Vec<f64>,
    perimeter: f64,
    /// 外轮廓（废料在外面）；内孔（废料在孔里）为 false
    outer: bool,
    ccw: bool,
}

impl LoopGeometry<'_> {
    /// 周长位置 `pos`（0 ~ 2 圈）处的点。
    fn point_at(&self, pos: f64) -> Point {
        let lap = if pos >= self.perimeter {
            pos - self.perimeter
        } else {
            pos
        };
        let k = self
            .cum
            .partition_point(|&c| c <= lap)
            .clamp(1, self.cum.len() - 1);
        let (a, b) = (self.ring[k - 1], self.ring[k]);
        let len = self.cum[k] - self.cum[k - 1];
        if len <= 0.0 {
            return b;
        }
        add(
            a,
            scale(sub(b, a), ((lap - self.cum[k - 1]) / len).clamp(0.0, 1.0)),
        )
    }

    /// 沿行进方向 `tangent` 时指向废料一侧的单位法线。
    fn scrap_normal(&self, tangent: Point) -> Point {
        let left = (-tangent.1, tangent.0);
        let interior = if self.ccw { left } else { scale(left, -1.0) };
        if self.outer {
            scale(interior, -1.0)
        } else {
            interior
        }
    }

    /// 周长位置 `pos` 处进来与出去的方向（在顶点上两者不同）。
    fn directions_at(&self, pos: f64) -> (Point, Point) {
        let lap = pos.rem_euclid(self.perimeter);
        let n = self.ring.len() - 1;
        let edge = |k: usize| unit(sub(self.ring[k % n + 1], self.ring[k % n]));
        let k = self.cum.partition_point(|&c| c <= lap + 1e-9).clamp(1, n) - 1;
        if (lap - self.cum[k]).abs() <= 1e-9 {
            (edge(k + n - 1), edge(k))
        } else {
            (edge(k), edge(k))
        }
    }

    /// 引线的基准方向：进出两条边废料一侧法线的角平分线（尖角上不贴着前一条边走）。
    fn lead_normal(&self, incoming: Point, outgoing: Point) -> Point {
        let sum = add(self.scrap_normal(incoming), self.scrap_normal(outgoing));
        if sum.0.hypot(sum.1) < 1e-9 {
            self.scrap_normal(outgoing)
        } else {
            unit(sum)
        }
    }

    /// 点在废料一侧（外轮廓的外面 / 内孔的里面）。
    fn on_scrap_side(&self, p: Point) -> bool {
        point_in_polygon(p, self.ring) != self.outer
    }
}

/// 在规划好的步骤上给闭合轮廓加桥位、引线与过切（见模块说明），并单独自检。
pub fn apply_cut_options(
    steps: &[Step],
    options: &CutOptions,
) -> Result<(Vec<Step>, CutReport), SafetyError> {
    options.validate()?;
    let loops = find_loops(steps);
    let rings: Vec<Vec<Point>> = loops
        .iter()
        .map(|&(start, end)| steps[start..end].iter().map(step_point).collect())
        .collect();
    let areas: Vec<f64> = rings
        .iter()
        .map(|ring| signed_area(&ring[..ring.len() - 1]))
        .collect();
    let mut report = CutReport {
        contours: loops.len(),
        ..CutReport::default()
    };
    let mut out: Vec<Step> = Vec::with_capacity(steps.len() + loops.len() * 8);
    let mut added = 0.0;
    let mut cursor = 0;
    let (sin, cos) = options.lead_angle_deg.to_radians().sin_cos();
    for (index, &(start, end)) in loops.iter().enumerate() {
        out.extend_from_slice(&steps[cursor..start]);
        cursor = end;
        let ring = &rings[index];
        // 被几个（面积更大的）轮廓包着：偶数层是零件外框，奇数层是孔
        let probe = scale(add(ring[0], ring[1]), 0.5);
        let depth = rings
            .iter()
            .enumerate()
            .filter(|(j, other)| {
                *j != index
                    && areas[*j].abs() > areas[index].abs()
                    && point_in_polygon(probe, other)
            })
            .count();
        let mut cum = vec![0.0];
        for pair in ring.windows(2) {
            cum.push(cum[cum.len() - 1] + distance(pair[0], pair[1]));
        }
        let perimeter = cum[cum.len() - 1];
        let geometry = LoopGeometry {
            ring,
            perimeter,
            cum,
            outer: depth % 2 == 0,
            ccw: areas[index] > 0.0,
        };

        // 桥位：均匀分布，第一个离起刀点半个间距；总宽不超过周长一半
        let requested = match options.tabs {
            TabMode::Off => 0,
            _ if options.tabs_skip_inner && depth > 0 => 0,
            TabMode::Count(n) => n as usize,
            TabMode::Spacing(spacing) => ((perimeter / spacing).round() as usize).max(1),
        };
        let fit = (0.5 * perimeter / options.tab_width_mm).floor() as usize;
        let count = requested.min(fit);
        if requested > 0 && count == 0 {
            report.contours_without_tabs += 1;
        }
        let half = options.tab_width_mm / 2.0;
        let gaps: Vec<(f64, f64)> = (0..count)
            .flat_map(|j| {
                let center = (j as f64 + 0.5) * perimeter / count as f64;
                [
                    (center - half, center + half),
                    (center + perimeter - half, center + perimeter + half),
                ]
            })
            .collect();
        // 过切：最多一圈；结束点落在桥位里时停在桥位前
        let mut finish = perimeter + options.overcut_mm.min(perimeter);
        if let Some(&(g0, _)) = gaps.iter().find(|&&(g0, g1)| g0 < finish && finish < g1) {
            finish = g0;
        }
        let overcut = finish - perimeter;

        // 引入线：从废料一侧切进起刀点
        let p0 = ring[0];
        let lead_in_from = (options.lead_in_mm > 0.0).then(|| {
            let (incoming, tangent) = geometry.directions_at(0.0);
            let normal = geometry.lead_normal(incoming, tangent);
            add(
                p0,
                scale(
                    unit(sub(scale(normal, sin), scale(tangent, cos))),
                    options.lead_in_mm,
                ),
            )
        });
        match lead_in_from {
            Some(from) if geometry.on_scrap_side(from) && !crosses_any(from, p0, &rings, p0) => {
                out.push(Step::Travel(from));
                out.push(Step::Burn(p0));
                report.lead_ins += 1;
                added += options.lead_in_mm;
            }
            Some(_) => {
                out.push(Step::Travel(p0));
                report.leads_skipped += 1;
            }
            None => out.push(Step::Travel(p0)),
        }

        // 沿轮廓走：顶点、桥位边界与结束点为断点，桥位里关光滑行
        let n = ring.len() - 1;
        let mut marks: Vec<(f64, Point)> = Vec::with_capacity(2 * n + gaps.len() * 2 + 1);
        for lap in 0..2 {
            for (&at, &vertex) in geometry.cum[1..].iter().zip(&ring[1..]) {
                let pos = lap as f64 * perimeter + at;
                if pos < finish - 1e-9 {
                    marks.push((pos, vertex));
                }
            }
        }
        for &(g0, g1) in &gaps {
            for g in [g0, g1] {
                if g > 0.0 && g < finish {
                    marks.push((g, geometry.point_at(g)));
                }
            }
        }
        marks.push((finish, geometry.point_at(finish)));
        marks.sort_by(|a, b| a.0.total_cmp(&b.0));
        marks.dedup_by(|later, earlier| (later.0 - earlier.0).abs() < 1e-9);
        let (mut previous, mut walked, mut here) = (0.0, 0.0, p0);
        for &(pos, point) in &marks {
            let middle = (previous + pos) / 2.0;
            if gaps.iter().any(|&(g0, g1)| g0 <= middle && middle <= g1) {
                out.push(Step::Coast(point));
            } else {
                out.push(Step::Burn(point));
            }
            walked += distance(here, point);
            here = point;
            previous = pos;
        }
        if (walked - finish).abs() > 1e-6 * finish.max(1.0) {
            return Err(SafetyError("切割后处理改变了轮廓路径，拒绝生成 G-code"));
        }
        let gap_total: f64 = gaps
            .iter()
            .map(|&(g0, g1)| (g1.min(finish) - g0.max(0.0)).max(0.0))
            .sum();
        report.tabs += count;
        report.tab_gap_mm += gap_total;
        report.overcut_mm += overcut;
        added += overcut - gap_total;

        // 引出线：从结束点切向废料一侧
        if options.lead_out_mm > 0.0 {
            let (incoming, outgoing) = geometry.directions_at(finish);
            let normal = geometry.lead_normal(incoming, outgoing);
            let to = add(
                here,
                scale(
                    unit(add(scale(normal, sin), scale(incoming, cos))),
                    options.lead_out_mm,
                ),
            );
            if geometry.on_scrap_side(to) && !crosses_any(here, to, &rings, here) {
                out.push(Step::Burn(to));
                report.lead_outs += 1;
                added += options.lead_out_mm;
            } else {
                report.leads_skipped += 1;
            }
        }
    }
    out.extend_from_slice(&steps[cursor..]);

    // 自检：出光总长度 = 原来的 − 桥位 + 引线 + 过切
    let before = burn_length(&burn_paths(steps));
    let after = burn_length(&burn_paths(&out));
    if (after - (before + added)).abs() > 1e-6 * (before + added).max(1.0) {
        return Err(SafetyError(
            "切割后处理的出光长度与预期不符，拒绝生成 G-code",
        ));
    }
    Ok((out, report))
}
