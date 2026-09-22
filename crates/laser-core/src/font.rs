use std::path::Path;

use laser_contracts::geometry::{Paths, Point};
use ttf_parser::{Face, OutlineBuilder};

#[derive(Debug, Clone, PartialEq)]
pub struct GlyphOutline {
    pub glyph_id: u16,
    pub advance_units: u16,
    pub paths: Paths,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    pub paths: Paths,
    pub width: f64,
    pub height: f64,
    pub glyphs: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlignment {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextLayoutOptions {
    pub height_mm: f64,
    pub spacing_mm: f64,
    pub line_spacing_ratio: f64,
    pub max_width_mm: Option<f64>,
    pub alignment: TextAlignment,
}

impl Default for TextLayoutOptions {
    fn default() -> Self {
        Self {
            height_mm: 10.0,
            spacing_mm: 0.0,
            line_spacing_ratio: 0.18,
            max_width_mm: None,
            alignment: TextAlignment::Left,
        }
    }
}

pub const WINDOWS_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\msyhbd.ttc",
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\simsun.ttc",
];
pub const MACOS_FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Medium.ttc",
];
pub const LINUX_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttf",
];

pub fn find_system_font() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    let candidates = WINDOWS_FONT_CANDIDATES;
    #[cfg(target_os = "macos")]
    let candidates = MACOS_FONT_CANDIDATES;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let candidates = LINUX_FONT_CANDIDATES;

    for candidate in candidates {
        let path = std::path::Path::new(candidate);
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }
    None
}

struct Builder {
    paths: Paths,
    current: Vec<Point>,
    scale: f64,
    tolerance: f64,
}

impl Builder {
    fn new(scale: f64, tolerance: f64) -> Self {
        Self {
            paths: Vec::new(),
            current: Vec::new(),
            scale,
            tolerance,
        }
    }

    fn finish(mut self) -> Paths {
        for path in &mut self.paths {
            dedupe_path(path);
        }
        if self.current.len() > 1 {
            dedupe_path(&mut self.current);
            self.paths.push(std::mem::take(&mut self.current));
        }
        self.paths
    }
}

fn subdivide_quad(
    p0: Point,
    p1: Point,
    p2: Point,
    tol: f64,
    depth: usize,
    out: &mut Vec<Point>,
) {
    let chord = (p2.0 - p0.0).hypot(p2.1 - p0.1);
    let dist = if chord < 1e-6 {
        (p1.0 - p0.0).hypot(p1.1 - p0.1)
    } else {
        ((p2.1 - p0.1) * p1.0 - (p2.0 - p0.0) * p1.1 + p2.0 * p0.1 - p2.1 * p0.0).abs() / chord
    };
    if dist <= tol || depth >= 10 {
        out.push(p2);
    } else {
        let p01 = ((p0.0 + p1.0) * 0.5, (p0.1 + p1.1) * 0.5);
        let p12 = ((p1.0 + p2.0) * 0.5, (p1.1 + p2.1) * 0.5);
        let p012 = ((p01.0 + p12.0) * 0.5, (p01.1 + p12.1) * 0.5);
        subdivide_quad(p0, p01, p012, tol, depth + 1, out);
        subdivide_quad(p012, p12, p2, tol, depth + 1, out);
    }
}

fn subdivide_cubic(
    p0: Point,
    p1: Point,
    p2: Point,
    p3: Point,
    tol: f64,
    depth: usize,
    out: &mut Vec<Point>,
) {
    let chord = (p3.0 - p0.0).hypot(p3.1 - p0.1);
    let (d1, d2) = if chord < 1e-6 {
        ((p1.0 - p0.0).hypot(p1.1 - p0.1), (p2.0 - p0.0).hypot(p2.1 - p0.1))
    } else {
        let a = p3.1 - p0.1;
        let b = -(p3.0 - p0.0);
        let c = p3.0 * p0.1 - p3.1 * p0.0;
        (
            (a * p1.0 + b * p1.1 + c).abs() / chord,
            (a * p2.0 + b * p2.1 + c).abs() / chord,
        )
    };
    if (d1 <= tol && d2 <= tol) || depth >= 10 {
        out.push(p3);
    } else {
        let p01 = ((p0.0 + p1.0) * 0.5, (p0.1 + p1.1) * 0.5);
        let p12 = ((p1.0 + p2.0) * 0.5, (p1.1 + p2.1) * 0.5);
        let p23 = ((p2.0 + p3.0) * 0.5, (p2.1 + p3.1) * 0.5);
        let p012 = ((p01.0 + p12.0) * 0.5, (p01.1 + p12.1) * 0.5);
        let p123 = ((p12.0 + p23.0) * 0.5, (p12.1 + p23.1) * 0.5);
        let p0123 = ((p012.0 + p123.0) * 0.5, (p012.1 + p123.1) * 0.5);
        subdivide_cubic(p0, p01, p012, p0123, tol, depth + 1, out);
        subdivide_cubic(p0123, p123, p23, p3, tol, depth + 1, out);
    }
}

impl OutlineBuilder for Builder {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.current.len() > 1 {
            self.paths.push(std::mem::take(&mut self.current));
        }
        self.current
            .push((x as f64 * self.scale, -y as f64 * self.scale));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.current
            .push((x as f64 * self.scale, -y as f64 * self.scale));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let p0 = *self.current.last().unwrap_or(&(0.0, 0.0));
        let p1 = (x1 as f64 * self.scale, -y1 as f64 * self.scale);
        let p2 = (x as f64 * self.scale, -y as f64 * self.scale);
        subdivide_quad(p0, p1, p2, self.tolerance, 0, &mut self.current);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p0 = *self.current.last().unwrap_or(&(0.0, 0.0));
        let p1 = (x1 as f64 * self.scale, -y1 as f64 * self.scale);
        let p2 = (x2 as f64 * self.scale, -y2 as f64 * self.scale);
        let p3 = (x as f64 * self.scale, -y as f64 * self.scale);
        subdivide_cubic(p0, p1, p2, p3, self.tolerance, 0, &mut self.current);
    }
    fn close(&mut self) {
        if let Some(first) = self.current.first().copied() {
            if self.current.last().copied() != Some(first) {
                self.current.push(first);
            }
        }
    }
}

fn dedupe_path(path: &mut Vec<Point>) {
    path.dedup_by(|a, b| (a.0 - b.0).hypot(a.1 - b.1) <= 0.01);
    if path.len() > 1 {
        let first = path[0];
        if path
            .last()
            .is_some_and(|last| (last.0 - first.0).hypot(last.1 - first.1) <= 0.01)
        {
            path.pop();
        }
    }
}

pub fn glyph_outline(
    data: &[u8],
    face_index: u32,
    ch: char,
    height_mm: f64,
) -> Result<GlyphOutline, &'static str> {
    if !height_mm.is_finite() || height_mm <= 0.0 {
        return Err("font height must be positive");
    }
    let face = Face::parse(data, face_index).map_err(|_| "invalid font")?;
    let glyph = face.glyph_index(ch).ok_or("glyph is missing")?;
    let scale = height_mm / f64::from(face.units_per_em());
    let mut builder = Builder::new(scale, 0.02);
    face.outline_glyph(glyph, &mut builder)
        .ok_or("glyph has no outline")?;
    Ok(GlyphOutline {
        glyph_id: glyph.0,
        advance_units: face.glyph_hor_advance(glyph).unwrap_or(0),
        paths: builder.finish(),
    })
}

pub fn layout_text_file(
    path: impl AsRef<Path>,
    text: &str,
    height_mm: f64,
    spacing_mm: f64,
) -> Result<TextLayout, &'static str> {
    let data = std::fs::read(path).map_err(|_| "font file cannot be read")?;
    layout_text(&data, 0, text, height_mm, spacing_mm)
}

pub fn wrap_text_lines<'a>(
    face: &Face<'_>,
    text: &'a str,
    scale: f64,
    spacing_mm: f64,
    max_width_mm: Option<f64>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        let trimmed = raw_line.trim_end();
        if trimmed.is_empty() {
            lines.push(String::new());
            continue;
        }
        if let Some(max_w) = max_width_mm {
            if max_w > 0.0 {
                let mut current_line = String::new();
                let mut current_w = 0.0;
                for ch in trimmed.chars() {
                    let adv = face
                        .glyph_index(ch)
                        .and_then(|g| face.glyph_hor_advance(g))
                        .unwrap_or(0) as f64
                        * scale
                        + spacing_mm;
                    if !current_line.is_empty() && current_w + adv > max_w {
                        lines.push(current_line);
                        current_line = ch.to_string();
                        current_w = adv;
                    } else {
                        current_line.push(ch);
                        current_w += adv;
                    }
                }
                if !current_line.is_empty() {
                    lines.push(current_line);
                }
                continue;
            }
        }
        lines.push(trimmed.to_string());
    }
    lines
}

pub fn layout_text_advanced(
    data: &[u8],
    face_index: u32,
    text: &str,
    options: &TextLayoutOptions,
) -> Result<TextLayout, &'static str> {
    if !options.height_mm.is_finite() || options.height_mm <= 0.0 {
        return Err("font height must be positive");
    }
    if !options.spacing_mm.is_finite() || options.spacing_mm < 0.0 {
        return Err("font spacing must be finite and nonnegative");
    }
    let face = Face::parse(data, face_index).map_err(|_| "invalid font")?;
    let scale = options.height_mm / f64::from(face.units_per_em());
    let em_line_height = (f64::from(face.ascender()) - f64::from(face.descender())
        + f64::from(face.line_gap()))
    .max(f64::from(face.units_per_em()))
        * scale;
    let line_height = em_line_height + options.height_mm * options.line_spacing_ratio;

    let lines = wrap_text_lines(&face, text, scale, options.spacing_mm, options.max_width_mm);
    let mut line_widths = Vec::new();
    for line in &lines {
        let mut lw = 0.0;
        for ch in line.chars() {
            if let Some(glyph) = face.glyph_index(ch) {
                lw += f64::from(face.glyph_hor_advance(glyph).unwrap_or(0)) * scale
                    + options.spacing_mm;
            }
        }
        line_widths.push(lw);
    }
    let max_line_width = line_widths.iter().copied().fold(0.0_f64, f64::max);

    let mut baseline = f64::from(face.ascender()) * scale;
    let mut paths = Vec::new();
    let mut glyphs = 0;

    for (line_idx, line) in lines.iter().enumerate() {
        let lw = line_widths[line_idx];
        let mut x = match options.alignment {
            TextAlignment::Left => 0.0,
            TextAlignment::Center => (max_line_width - lw) * 0.5,
            TextAlignment::Right => max_line_width - lw,
        };

        for ch in line.chars() {
            let glyph = face.glyph_index(ch).ok_or("glyph is missing")?;
            let mut builder = Builder::new(scale, 0.02);
            face.outline_glyph(glyph, &mut builder);
            for mut path in builder.finish() {
                for point in &mut path {
                    point.0 += x;
                    point.1 += baseline;
                }
                paths.push(path);
            }
            x += f64::from(face.glyph_hor_advance(glyph).unwrap_or(0)) * scale
                + options.spacing_mm;
            glyphs += 1;
        }
        baseline += line_height;
    }

    let min_x = paths.iter().flatten().map(|p| p.0).fold(0.0, f64::min);
    let min_y = paths.iter().flatten().map(|p| p.1).fold(0.0, f64::min);
    let max_x = paths
        .iter()
        .flatten()
        .map(|p| p.0)
        .fold(max_line_width, f64::max);
    let max_y = paths
        .iter()
        .flatten()
        .map(|p| p.1)
        .fold(line_height * lines.len().max(1) as f64, f64::max);

    for point in paths.iter_mut().flatten() {
        point.0 -= min_x;
        point.1 -= min_y;
    }

    Ok(TextLayout {
        paths,
        width: max_x - min_x,
        height: max_y - min_y,
        glyphs,
    })
}

pub fn layout_text(
    data: &[u8],
    face_index: u32,
    text: &str,
    height_mm: f64,
    spacing_mm: f64,
) -> Result<TextLayout, &'static str> {
    layout_text_advanced(
        data,
        face_index,
        text,
        &TextLayoutOptions {
            height_mm,
            spacing_mm,
            line_spacing_ratio: 0.0,
            max_width_mm: None,
            alignment: TextAlignment::Left,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_font_and_dimensions_are_rejected() {
        assert!(glyph_outline(&[], 0, 'A', 10.0).is_err());
        assert!(layout_text(&[], 0, "A", 0.0, 1.0).is_err());
    }

    #[test]
    fn fixture_layout_moves_baselines_and_contains_all_glyphs() {
        let data = include_bytes!("../../../tests/fixtures/fonts/geometry.ttf");
        let single = layout_text(data, 0, "A", 10.0, 0.0).unwrap();
        let multiple = layout_text(data, 0, "A\ng", 10.0, 0.0).unwrap();
        assert_eq!(multiple.glyphs, 2);
        assert_eq!(single.height, 12.0);
        assert_eq!(multiple.height, 24.0);
        assert!(multiple.paths.last().unwrap().iter().all(|p| p.1 > 12.0));
        for p in multiple.paths.iter().flatten() {
            assert!((0.0..=multiple.width).contains(&p.0));
            assert!((0.0..=multiple.height).contains(&p.1));
        }
        for height in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(layout_text(data, 0, "A", height, 0.0).is_err());
        }
        for spacing in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(layout_text(data, 0, "A", 10.0, spacing).is_err());
        }

        let centered = layout_text_advanced(
            data,
            0,
            "A\nAA",
            &TextLayoutOptions {
                height_mm: 10.0,
                spacing_mm: 0.0,
                line_spacing_ratio: 0.2,
                max_width_mm: None,
                alignment: TextAlignment::Center,
            },
        )
        .unwrap();
        assert_eq!(centered.glyphs, 3);

        let wrapped = layout_text_advanced(
            data,
            0,
            "AAAA",
            &TextLayoutOptions {
                height_mm: 10.0,
                spacing_mm: 0.0,
                line_spacing_ratio: 0.0,
                max_width_mm: Some(15.0),
                alignment: TextAlignment::Left,
            },
        )
        .unwrap();
        assert_eq!(wrapped.glyphs, 4);
    }
}
