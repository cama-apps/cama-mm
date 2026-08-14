//! Deterministic, Discord-neutral production chart rendering.
//!
//! This module owns the migrated chart geometry and decisions with a small RGBA
//! rasterizer and real PNG encoder, so the Rust runtime emits seekable image
//! bytes without Python, native graphics dependencies, or process-global
//! plotting state.

use std::collections::BTreeMap;
use std::io::Cursor;

use chrono::{TimeZone, Utc};

pub const DISCORD_BG: Rgba = Rgba::rgb(0x36, 0x39, 0x3f);
pub const DISCORD_DARKER: Rgba = Rgba::rgb(0x2f, 0x31, 0x36);
pub const DISCORD_ACCENT: Rgba = Rgba::rgb(0x58, 0x65, 0xf2);
pub const DISCORD_GREEN: Rgba = Rgba::rgb(0x57, 0xf2, 0x87);
pub const DISCORD_RED: Rgba = Rgba::rgb(0xed, 0x42, 0x45);
pub const DISCORD_YELLOW: Rgba = Rgba::rgb(0xfe, 0xe7, 0x5c);
pub const DISCORD_WHITE: Rgba = Rgba::rgb(0xff, 0xff, 0xff);
pub const DISCORD_GREY: Rgba = Rgba::rgb(0xb9, 0xbb, 0xbe);
pub const DISCORD_GRID: Rgba = Rgba::rgb(0x45, 0x49, 0x50);

const ROLE_ORDER: [&str; 9] = [
    "Carry",
    "Nuker",
    "Initiator",
    "Disabler",
    "Durable",
    "Escape",
    "Support",
    "Pusher",
    "Jungler",
];

const Y_LABEL_MAGNITUDES: [i32; 16] = [
    1, 2, 5, 10, 20, 50, 100, 200, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000,
];
const GAMBA_LEGEND_TYPE_MARKER_RADIUS: i32 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self([red, green, blue, 0xff])
    }

    const fn with_alpha(self, alpha: u8) -> Self {
        Self([self.0[0], self.0[1], self.0[2], alpha])
    }
}

#[derive(Clone, Debug)]
struct Raster {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Raster {
    fn new(width: usize, height: usize, color: Rgba) -> Self {
        let mut pixels = vec![0_u8; width.saturating_mul(height).saturating_mul(4)];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color.0);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    fn set_pixel(&mut self, x: i32, y: i32, color: Rgba) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.width + x) * 4;
        if color.0[3] == 0xff {
            self.pixels[offset..offset + 4].copy_from_slice(&color.0);
            return;
        }
        let alpha = u16::from(color.0[3]);
        let inverse = 255_u16 - alpha;
        for channel in 0..3 {
            let blended = (u16::from(color.0[channel]) * alpha
                + u16::from(self.pixels[offset + channel]) * inverse
                + 127)
                / 255;
            self.pixels[offset + channel] = u8::try_from(blended).unwrap_or(255);
        }
        self.pixels[offset + 3] = 0xff;
    }

    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        for y in top.min(bottom)..top.max(bottom) {
            for x in left.min(right)..left.max(right) {
                self.set_pixel(x, y, color);
            }
        }
    }

    fn line(&mut self, start: (i32, i32), end: (i32, i32), color: Rgba, width: i32) {
        let (mut x0, mut y0) = start;
        let (x1, y1) = end;
        let dx = (x1 - x0).abs();
        let step_x = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let step_y = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        let radius = width.saturating_sub(1) / 2;
        loop {
            for offset_y in -radius..=radius {
                for offset_x in -radius..=radius {
                    self.set_pixel(x0 + offset_x, y0 + offset_y, color);
                }
            }
            if x0 == x1 && y0 == y1 {
                break;
            }
            let twice_error = error * 2;
            if twice_error >= dy {
                error += dy;
                x0 += step_x;
            }
            if twice_error <= dx {
                error += dx;
                y0 += step_y;
            }
        }
    }

    fn circle(&mut self, center: (i32, i32), radius: i32, color: Rgba) {
        let squared = radius * radius;
        for y in -radius..=radius {
            for x in -radius..=radius {
                if x * x + y * y <= squared {
                    self.set_pixel(center.0 + x, center.1 + y, color);
                }
            }
        }
    }

    fn polygon(&mut self, points: &[(i32, i32)], color: Rgba) {
        if points.len() < 3 {
            return;
        }
        let min_y = points.iter().map(|point| point.1).min().unwrap_or(0);
        let max_y = points.iter().map(|point| point.1).max().unwrap_or(0);
        for y in min_y..=max_y {
            let mut intersections = Vec::new();
            for index in 0..points.len() {
                let first = points[index];
                let second = points[(index + 1) % points.len()];
                if (first.1 <= y && second.1 > y) || (second.1 <= y && first.1 > y) {
                    let numerator = i64::from(y - first.1) * i64::from(second.0 - first.0);
                    let denominator = i64::from(second.1 - first.1);
                    let x = i64::from(first.0) + numerator / denominator;
                    intersections.push(i32::try_from(x).unwrap_or(first.0));
                }
            }
            intersections.sort_unstable();
            for pair in intersections.chunks_exact(2) {
                for x in pair[0]..=pair[1] {
                    self.set_pixel(x, y, color);
                }
            }
        }
    }

    fn polygon_outline(&mut self, points: &[(i32, i32)], color: Rgba) {
        for index in 0..points.len() {
            self.line(points[index], points[(index + 1) % points.len()], color, 1);
        }
    }

    fn draw_glyph(&mut self, x: i32, y: i32, character: char, color: Rgba, scale: i32) {
        for (row, bits) in glyph(character).into_iter().enumerate() {
            for column in 0..5_u8 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                let pixel_x = x + i32::from(column) * scale;
                let pixel_y = y + i32::try_from(row).unwrap_or(0) * scale;
                self.fill_rect(pixel_x, pixel_y, pixel_x + scale, pixel_y + scale, color);
            }
        }
    }

    fn draw_glyph_case_sensitive(
        &mut self,
        x: i32,
        y: i32,
        character: char,
        color: Rgba,
        scale: i32,
    ) {
        for (row, bits) in case_sensitive_glyph(character).into_iter().enumerate() {
            for column in 0..5_u8 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                let pixel_x = x + i32::from(column) * scale;
                let pixel_y = y + i32::try_from(row).unwrap_or(0) * scale;
                self.fill_rect(pixel_x, pixel_y, pixel_x + scale, pixel_y + scale, color);
            }
        }
    }

    fn text(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        let mut cursor = x;
        for character in text.chars() {
            self.draw_glyph(cursor, y, character, color, scale);
            cursor += 6 * scale;
        }
    }

    fn text_case_sensitive(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        let mut cursor = x;
        for character in text.chars() {
            self.draw_glyph_case_sensitive(cursor, y, character, color, scale);
            cursor += 6 * scale;
        }
    }

    fn text_width(text: &str, scale: i32) -> i32 {
        text.chars()
            .count()
            .try_into()
            .unwrap_or(i32::MAX)
            .saturating_mul(6)
            .saturating_mul(scale.max(0))
    }

    fn circle_with_outline(&mut self, center: (i32, i32), radius: i32, fill: Rgba, outline: Rgba) {
        let outer_squared = radius.saturating_mul(radius);
        let inner_radius = radius.saturating_sub(1);
        let inner_squared = inner_radius.saturating_mul(inner_radius);
        for y in -radius..=radius {
            for x in -radius..=radius {
                let squared = x.saturating_mul(x).saturating_add(y.saturating_mul(y));
                if squared > outer_squared {
                    continue;
                }
                self.set_pixel(
                    center.0 + x,
                    center.1 + y,
                    if squared > inner_squared {
                        outline
                    } else {
                        fill
                    },
                );
            }
        }
    }

    /// Draw a small filled/outlined rounded rectangle for chart labels.
    ///
    /// The Python chart uses Pillow's ``rounded_rectangle`` for callouts and
    /// the stat strip.  Keeping this primitive in the native rasterizer makes
    /// those surfaces deterministic without introducing a font or graphics
    /// dependency into the runtime.
    fn rounded_rect(
        &mut self,
        bounds: (i32, i32, i32, i32),
        radius: i32,
        fill: Rgba,
        outline: Option<Rgba>,
    ) {
        let (left, top, right, bottom) = bounds;
        let radius = radius
            .max(0)
            .min((right - left).abs() / 2)
            .min((bottom - top).abs() / 2);
        self.fill_rect(left + radius, top, right - radius, bottom, fill);
        self.fill_rect(left, top + radius, right, bottom - radius, fill);
        for center in [
            (left + radius, top + radius),
            (right - radius - 1, top + radius),
            (left + radius, bottom - radius - 1),
            (right - radius - 1, bottom - radius - 1),
        ] {
            self.circle(center, radius, fill);
        }
        if let Some(outline) = outline {
            self.line((left + radius, top), (right - radius - 1, top), outline, 1);
            self.line(
                (left + radius, bottom - 1),
                (right - radius - 1, bottom - 1),
                outline,
                1,
            );
            self.line(
                (left, top + radius),
                (left, bottom - radius - 1),
                outline,
                1,
            );
            self.line(
                (right - 1, top + radius),
                (right - 1, bottom - radius - 1),
                outline,
                1,
            );
            let corner_arcs = [
                (
                    (left + radius, top + radius),
                    std::f64::consts::PI,
                    std::f64::consts::PI * 1.5,
                ),
                (
                    (right - radius - 1, top + radius),
                    std::f64::consts::PI * 1.5,
                    std::f64::consts::TAU,
                ),
                (
                    (right - radius - 1, bottom - radius - 1),
                    0.0,
                    std::f64::consts::FRAC_PI_2,
                ),
                (
                    (left + radius, bottom - radius - 1),
                    std::f64::consts::FRAC_PI_2,
                    std::f64::consts::PI,
                ),
            ];
            for (center, start_angle, end_angle) in corner_arcs {
                let mut previous = (
                    center.0 + (f64::from(radius) * start_angle.cos()).round() as i32,
                    center.1 + (f64::from(radius) * start_angle.sin()).round() as i32,
                );
                for step in 1..=8 {
                    let angle = start_angle + (end_angle - start_angle) * f64::from(step) / 8.0;
                    let point = (
                        center.0 + (f64::from(radius) * angle.cos()).round() as i32,
                        center.1 + (f64::from(radius) * angle.sin()).round() as i32,
                    );
                    self.line(previous, point, outline, 1);
                    previous = point;
                }
            }
        }
    }

    fn pie(&mut self, center: (i32, i32), radius: i32, slices: &[(f64, Rgba)]) {
        let total = slices.iter().map(|(value, _)| value.max(0.0)).sum::<f64>();
        if total <= 0.0 {
            return;
        }
        let squared = radius * radius;
        for y in -radius..=radius {
            for x in -radius..=radius {
                if x * x + y * y > squared {
                    continue;
                }
                let angle = (f64::from(y).atan2(f64::from(x)).to_degrees() + 450.0) % 360.0;
                let mut threshold = 0.0;
                let mut color = slices
                    .iter()
                    .rev()
                    .find(|slice| slice.0 > 0.0)
                    .map_or(DISCORD_GREY, |slice| slice.1);
                for (value, candidate) in slices {
                    if *value <= 0.0 {
                        continue;
                    }
                    threshold += value.max(0.0) / total * 360.0;
                    if angle <= threshold {
                        color = *candidate;
                        break;
                    }
                }
                self.set_pixel(center.0 + x, center.1 + y, color);
            }
        }
    }
}

fn glyph(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [31, 4, 4, 4, 4, 4, 31],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '=' => [0, 31, 0, 31, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 0, 4, 8],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '|' => [4, 4, 4, 4, 4, 4, 4],
        '%' => [25, 25, 2, 4, 8, 19, 19],
        '/' => [1, 2, 2, 4, 8, 8, 16],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '#' => [10, 31, 10, 10, 31, 10, 0],
        '\'' => [4, 4, 2, 0, 0, 0, 0],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        '·' => [0, 0, 0, 4, 0, 0, 0],
        'μ' => [0, 0, 17, 17, 17, 27, 21],
        'σ' => [0, 0, 14, 17, 17, 17, 14],
        '—' => [0, 0, 0, 31, 0, 0, 0],
        ' ' => [0; 7],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

fn case_sensitive_glyph(character: char) -> [u8; 7] {
    match character {
        'a' => [0, 0, 14, 1, 15, 17, 15],
        'b' => [16, 16, 30, 17, 17, 17, 30],
        'c' => [0, 0, 14, 17, 16, 17, 14],
        'd' => [1, 1, 15, 17, 17, 17, 15],
        'e' => [0, 0, 14, 17, 31, 16, 14],
        'f' => [6, 9, 8, 28, 8, 8, 8],
        'g' => [0, 0, 15, 17, 15, 1, 14],
        'h' => [16, 16, 30, 17, 17, 17, 17],
        'i' => [4, 0, 12, 4, 4, 4, 14],
        'j' => [2, 0, 6, 2, 2, 18, 12],
        'k' => [16, 16, 18, 20, 24, 20, 18],
        'l' => [12, 4, 4, 4, 4, 4, 14],
        'm' => [0, 0, 26, 21, 21, 21, 21],
        'n' => [0, 0, 30, 17, 17, 17, 17],
        'o' => [0, 0, 14, 17, 17, 17, 14],
        'p' => [0, 0, 30, 17, 30, 16, 16],
        'q' => [0, 0, 15, 17, 15, 1, 1],
        'r' => [0, 0, 22, 25, 16, 16, 16],
        's' => [0, 0, 15, 16, 14, 1, 30],
        't' => [8, 8, 28, 8, 8, 9, 6],
        'u' => [0, 0, 17, 17, 17, 19, 13],
        'v' => [0, 0, 17, 17, 17, 10, 4],
        'w' => [0, 0, 17, 17, 21, 21, 10],
        'x' => [0, 0, 17, 10, 4, 10, 17],
        'y' => [0, 0, 17, 17, 15, 1, 14],
        'z' => [0, 0, 31, 2, 4, 8, 31],
        _ => glyph(character),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MatchRow {
    pub hero_id: Option<i64>,
    pub hero_name: Option<String>,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub won: Option<bool>,
    pub duration_seconds: Option<u64>,
}

/// Render the compact match table used by profile commands.
#[must_use]
pub fn draw_matches_table(
    matches: &[MatchRow],
    hero_names: &BTreeMap<i64, String>,
) -> Cursor<Vec<u8>> {
    if matches.is_empty() {
        let mut raster = Raster::new(400, 100, DISCORD_BG);
        raster.text(20, 40, "No matches found", DISCORD_GREY, 2);
        return render(raster);
    }

    // Keep these columns in lockstep with ``utils.drawing.tables``.  The
    // profile/recent command feeds both renderers the same ordered rows, so
    // exact column boundaries are part of the attachment contract rather than
    // an approximation for the native rasterizer.
    let columns = [
        ("Hero", 120_i32),
        ("K", 35_i32),
        ("D", 35_i32),
        ("A", 35_i32),
        ("Result", 55_i32),
        ("Duration", 70_i32),
    ];
    let padding = 10_i32;
    let header_height = 32_i32;
    let row_height = 36_i32;
    let width = columns.iter().map(|(_, width)| width).sum::<i32>() + padding * 2;
    let height =
        header_height + i32::try_from(matches.len()).unwrap_or(0) * row_height + padding * 2;
    let mut raster = Raster::new(
        usize::try_from(width).unwrap_or(370),
        usize::try_from(height).unwrap_or(52),
        DISCORD_BG,
    );
    let mut x = padding;
    for &(header, column_width) in &columns {
        raster.text(x + 5, padding + 8, header, DISCORD_WHITE, 1);
        x += column_width;
    }
    raster.fill_rect(
        padding,
        padding + header_height - 2,
        width - padding,
        padding + header_height,
        DISCORD_ACCENT,
    );

    for (index, row) in matches.iter().enumerate() {
        let y = padding + header_height + i32::try_from(index).unwrap_or(0) * row_height;
        if index % 2 == 1 {
            raster.fill_rect(padding, y, width - padding, y + row_height, DISCORD_DARKER);
        }
        let resolved = row
            .hero_id
            .and_then(|hero_id| hero_names.get(&hero_id))
            .cloned()
            .or_else(|| row.hero_name.clone())
            .unwrap_or_else(|| "Unknown".to_owned());
        let hero = truncate(&resolved, 14, 12);
        raster.text(15, y + 10, &hero, DISCORD_WHITE, 1);
        let values = [
            row.kills.to_string(),
            row.deaths.to_string(),
            row.assists.to_string(),
        ];
        let mut column_x = padding + columns[0].1;
        for (value, (_, column_width)) in values.iter().zip(&columns[1..4]) {
            let text_x = column_x + (column_width - Raster::text_width(value, 1)) / 2;
            raster.text(text_x, y + 10, value, DISCORD_WHITE, 1);
            column_x += column_width;
        }
        let (result, color) = match row.won {
            Some(true) => ("Win", DISCORD_GREEN),
            Some(false) => ("Loss", DISCORD_RED),
            None => ("?", DISCORD_GREY),
        };
        let result_x = column_x + (columns[4].1 - Raster::text_width(result, 1)) / 2;
        raster.text(result_x, y + 10, result, color, 1);
        column_x += columns[4].1;
        let duration = row
            .duration_seconds
            .filter(|seconds| *seconds > 0)
            .map_or_else(
                || "-".to_owned(),
                |seconds| format!("{}:{:02}", seconds / 60, seconds % 60),
            );
        let duration_x = column_x + (columns[5].1 - Raster::text_width(&duration, 1)) / 2;
        raster.text(duration_x, y + 10, &duration, DISCORD_GREY, 1);
    }
    render(raster)
}

fn truncate(value: &str, limit: usize, prefix: usize) -> String {
    if value.chars().count() <= limit {
        value.to_owned()
    } else {
        format!("{}..", value.chars().take(prefix).collect::<String>())
    }
}

/// Render a fixed-order radar chart of Dota role percentages.
#[must_use]
pub fn draw_role_graph(values: &BTreeMap<String, f64>, title: &str) -> Cursor<Vec<u8>> {
    let mut raster = Raster::new(400, 400, DISCORD_BG);
    let title_width = Raster::text_width(title, 2);
    raster.text((400 - title_width).max(0) / 2, 8, title, DISCORD_WHITE, 2);
    let mut roles = ROLE_ORDER
        .iter()
        .map(|role| (*role).to_owned())
        .collect::<Vec<_>>();
    for role in values.keys() {
        if !roles.contains(role) {
            roles.push(role.clone());
        }
    }
    if roles.len() < 3 {
        raster.text(100, 200, "Not enough data", DISCORD_GREY, 2);
        return render(raster);
    }
    let maximum = roles
        .iter()
        .filter_map(|role| values.get(role))
        .copied()
        .fold(0.0_f64, f64::max);
    let scale_max = if maximum <= 10.0 {
        10.0
    } else if maximum <= 25.0 {
        ((maximum as i32 / 5) + 1) as f64 * 5.0
    } else {
        ((maximum as i32 / 10) + 1) as f64 * 10.0
    };
    let center = (200, 215);
    for fraction in [0.25, 0.5, 0.75, 1.0] {
        let ring = radial_points(roles.len(), center, 140.0 * fraction, |_| 1.0);
        raster.polygon_outline(&ring, DISCORD_DARKER);
        let label = format!("{}%", (scale_max * fraction) as i32);
        raster.text(
            center.0 + (140.0 * fraction) as i32 + 3,
            center.1 - 5,
            &label,
            DISCORD_GREY,
            1,
        );
    }
    let outer = radial_points(roles.len(), center, 140.0, |_| 1.0);
    for point in &outer {
        raster.line(center, *point, DISCORD_DARKER, 1);
    }
    let data = radial_points(roles.len(), center, 140.0, |index| {
        values.get(&roles[index]).copied().unwrap_or(0.0) / scale_max
    });
    raster.polygon(&data, DISCORD_ACCENT.with_alpha(100));
    raster.polygon_outline(&data, DISCORD_ACCENT);
    for point in data {
        raster.circle(point, 4, DISCORD_ACCENT);
    }

    // Match Python's two-line spoke labels (role, then percentage), including
    // the angular centering rules used at the top/bottom spokes.  The native
    // font is intentionally blocky, but its text and placement remain
    // semantically identical to the live Pillow helper.
    let label_offset = 22.0;
    for (index, role) in roles.iter().enumerate() {
        let angle =
            std::f64::consts::TAU * index as f64 / roles.len() as f64 - std::f64::consts::FRAC_PI_2;
        let mut label_x = center.0 as f64 + (140.0 + label_offset) * angle.cos();
        let mut label_y = center.1 as f64 + (140.0 + label_offset) * angle.sin();
        let text_width = Raster::text_width(role, 1);
        if label_x < f64::from(center.0 - 10) {
            label_x -= f64::from(text_width);
        } else if (label_x - f64::from(center.0)).abs() < 10.0 {
            label_x -= f64::from(text_width / 2);
        }
        if label_y < f64::from(center.1 - 10) {
            label_y -= 7.0;
        } else if (label_y - f64::from(center.1)).abs() < 10.0 {
            label_y -= 3.0;
        }
        let x = label_x as i32;
        let y = label_y as i32;
        raster.text(x, y, role, DISCORD_WHITE, 1);
        raster.text(
            x,
            y + 7,
            &format!("{}%", values.get(role).copied().unwrap_or(0.0) as i32),
            DISCORD_GREY,
            1,
        );
    }
    render(raster)
}

fn radial_points<F>(count: usize, center: (i32, i32), radius: f64, scale: F) -> Vec<(i32, i32)>
where
    F: Fn(usize) -> f64,
{
    (0..count)
        .map(|index| {
            let angle =
                std::f64::consts::TAU * index as f64 / count as f64 - std::f64::consts::FRAC_PI_2;
            let distance = radius * scale(index).clamp(0.0, 1.0);
            (
                center.0 + (distance * angle.cos()) as i32,
                center.1 + (distance * angle.sin()) as i32,
            )
        })
        .collect()
}

/// Render the insertion-ordered horizontal lane bar chart.
#[must_use]
pub fn draw_lane_distribution(lanes: &[(String, f64)]) -> Cursor<Vec<u8>> {
    let height = lanes.len() * 40 + 60;
    let mut raster = Raster::new(350, height, DISCORD_BG);
    raster.text(15, 15, "Lane Distribution", DISCORD_WHITE, 2);
    for (index, (lane, value)) in lanes.iter().enumerate() {
        let y = 55 + i32::try_from(index).unwrap_or(0) * 40;
        raster.text(15, y + 7, &truncate(lane, 11, 9), DISCORD_WHITE, 1);
        raster.fill_rect(95, y + 5, 285, y + 25, DISCORD_DARKER);
        let fill = (190.0 * value.clamp(0.0, 100.0) / 100.0) as i32;
        let color = match lane.as_str() {
            "Safe Lane" => Rgba::rgb(0x4c, 0xaf, 0x50),
            "Mid" => Rgba::rgb(0x21, 0x96, 0xf3),
            "Off Lane" => Rgba::rgb(0xff, 0x98, 0x00),
            "Jungle" => Rgba::rgb(0x9c, 0x27, 0xb0),
            "Roaming" => Rgba::rgb(0xe9, 0x1e, 0x63),
            _ => DISCORD_ACCENT,
        };
        raster.fill_rect(95, y + 5, 95 + fill, y + 25, color);
        raster.text(295, y + 11, &format!("{value:.0}%"), DISCORD_GREY, 1);
    }
    render(raster)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeroPerformanceEntry {
    pub hero_name: String,
    pub games: i64,
    pub wins: i64,
}

/// Render the profile Heroes performance bars, matching Python's top-eight
/// game-count ordering and win-rate color thresholds.
#[must_use]
pub fn draw_hero_performance_chart(
    hero_stats: &[HeroPerformanceEntry],
    username: &str,
) -> Cursor<Vec<u8>> {
    const WIDTH: usize = 450;
    const BAR_HEIGHT: i32 = 32;
    const PADDING: i32 = 15;
    const HEADER_HEIGHT: i32 = 35;
    const LABEL_WIDTH: i32 = 110;
    const VALUE_WIDTH: i32 = 70;
    const BAR_GAP: i32 = 6;
    let stats = hero_stats.iter().take(8).collect::<Vec<_>>();
    if stats.is_empty() {
        let mut raster = Raster::new(WIDTH, 100, DISCORD_BG);
        raster.text(20, 40, "No hero data available", DISCORD_GREY, 2);
        return render(raster);
    }
    let height = usize::try_from(
        HEADER_HEIGHT
            + i32::try_from(stats.len()).unwrap_or(0) * (BAR_HEIGHT + BAR_GAP)
            + PADDING * 2,
    )
    .unwrap_or(100);
    let mut raster = Raster::new(WIDTH, height, DISCORD_BG);
    raster.text(
        PADDING,
        PADDING,
        &format!("Top Heroes: {username}"),
        DISCORD_WHITE,
        1,
    );
    let max_games = stats
        .iter()
        .map(|stat| stat.games)
        .max()
        .unwrap_or(1)
        .max(1);
    let bar_x = PADDING + LABEL_WIDTH;
    let bar_max_width =
        i32::try_from(WIDTH).unwrap_or(450) - PADDING * 2 - LABEL_WIDTH - VALUE_WIDTH - 10;
    for (index, stat) in stats.into_iter().enumerate() {
        let y =
            PADDING + HEADER_HEIGHT + i32::try_from(index).unwrap_or(0) * (BAR_HEIGHT + BAR_GAP);
        let hero_name = truncate(&stat.hero_name, 13, 11);
        raster.text(PADDING, y + 8, &hero_name, DISCORD_WHITE, 1);
        raster.fill_rect(
            bar_x,
            y + 5,
            bar_x + bar_max_width,
            y + BAR_HEIGHT - 5,
            DISCORD_DARKER,
        );
        let fill_width = (bar_max_width as i64)
            .saturating_mul(stat.games.max(0))
            .checked_div(max_games)
            .unwrap_or(0)
            .clamp(4, i64::from(bar_max_width));
        let fill_width = i32::try_from(fill_width).unwrap_or(bar_max_width);
        let win_rate = if stat.games > 0 {
            stat.wins as f64 / stat.games as f64
        } else {
            0.0
        };
        let color = if win_rate >= 0.60 {
            DISCORD_GREEN
        } else if win_rate >= 0.50 {
            Rgba::rgb(0x7c, 0xb3, 0x42)
        } else if win_rate >= 0.40 {
            DISCORD_YELLOW
        } else {
            DISCORD_RED
        };
        raster.fill_rect(bar_x, y + 5, bar_x + fill_width, y + BAR_HEIGHT - 5, color);
        raster.text(
            bar_x + bar_max_width + 8,
            y + 8,
            &format!("{:.0}% ({}g)", win_rate * 100.0, stat.games),
            DISCORD_GREY,
            1,
        );
    }
    render(raster)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AttributeValues {
    pub strength: f64,
    pub agility: f64,
    pub intelligence: f64,
    pub universal: f64,
}

/// Render the four-way hero attribute pie and legend swatches.
#[must_use]
pub fn draw_attribute_distribution(values: AttributeValues) -> Cursor<Vec<u8>> {
    let mut raster = Raster::new(300, 300, DISCORD_BG);
    raster.text(65, 10, "Hero Attributes", DISCORD_WHITE, 2);
    let slices = [
        (values.strength, Rgba::rgb(0xe5, 0x39, 0x35)),
        (values.agility, Rgba::rgb(0x43, 0xa0, 0x47)),
        (values.intelligence, Rgba::rgb(0x1e, 0x88, 0xe5)),
        (values.universal, Rgba::rgb(0x8e, 0x24, 0xaa)),
    ];
    raster.pie((150, 170), 80, &slices);
    for (index, (value, color)) in slices.iter().filter(|slice| slice.0 > 0.0).enumerate() {
        let x = 20 + i32::try_from(index).unwrap_or(0) * 70;
        raster.fill_rect(x, 250, x + 14, 264, *color);
        raster.text(x + 18, 254, &format!("{value:.0}%"), DISCORD_WHITE, 1);
    }
    render(raster)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RatingHistoryEntry {
    pub rating: Option<f64>,
    pub openskill_mu: Option<f64>,
    pub won: Option<bool>,
}

/// Render chronological Glicko/OpenSkill history on the same display scale.
#[must_use]
pub fn draw_rating_history_chart(
    username: &str,
    most_recent_first: &[RatingHistoryEntry],
) -> Cursor<Vec<u8>> {
    let mut raster = Raster::new(700, 400, DISCORD_BG);
    raster.text(
        60,
        12,
        &format!("{username}'s Rating History"),
        DISCORD_WHITE,
        1,
    );
    if most_recent_first.len() < 2 {
        let message = if most_recent_first.is_empty() {
            "No rating history"
        } else {
            "Need 2+ matches for chart"
        };
        raster.text(220, 200, message, DISCORD_GREY, 2);
        return render(raster);
    }

    let data = most_recent_first.iter().rev().copied().collect::<Vec<_>>();
    let glicko = data.iter().map(|entry| entry.rating).collect::<Vec<_>>();
    let openskill = data
        .iter()
        .map(|entry| entry.openskill_mu.map(mu_to_display))
        .collect::<Vec<_>>();
    let combined = glicko
        .iter()
        .chain(&openskill)
        .filter_map(|value| *value)
        .collect::<Vec<_>>();
    let minimum = combined.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = combined.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let raw_range = (maximum - minimum).max(1.0);
    let lower = minimum - raw_range * 0.1;
    let upper = maximum + raw_range * 0.1;
    let chart = ChartRect {
        x: 60,
        y: 70,
        width: 580,
        height: 250,
    };
    for step in 0..=4 {
        let y = chart.y + step * chart.height / 4;
        raster.line((chart.x, y), (chart.x + chart.width, y), DISCORD_GRID, 1);
    }
    for step in 0..=4 {
        let fraction = f64::from(step) / 4.0;
        let value = upper - fraction * (upper - lower);
        let label = format!("{:.0}", (value / 50.0).round() * 50.0);
        raster.text(
            chart.x - Raster::text_width(&label, 1) - 6,
            chart.y + step * chart.height / 4 - 6,
            &label,
            DISCORD_GREY,
            1,
        );
    }
    draw_optional_series(&mut raster, &glicko, lower, upper, chart, DISCORD_ACCENT);
    draw_optional_series(&mut raster, &openskill, lower, upper, chart, DISCORD_YELLOW);

    for (index, entry) in data.iter().enumerate() {
        let Some(value) = glicko[index].or(openskill[index]) else {
            continue;
        };
        let point = chart.point(index, data.len(), value, lower, upper);
        let color = match entry.won {
            Some(true) => DISCORD_GREEN,
            Some(false) => DISCORD_RED,
            None => DISCORD_GREY,
        };
        raster.circle(point, if data.len() > 30 { 3 } else { 4 }, color);
    }

    let mut legend_x = 60;
    if glicko.iter().any(Option::is_some) {
        raster.line((legend_x, 348), (legend_x + 20, 348), DISCORD_ACCENT, 2);
        raster.text(legend_x + 25, 345, "Glicko-2", DISCORD_GREY, 1);
        legend_x += 110;
    }
    if openskill.iter().any(Option::is_some) {
        raster.line((legend_x, 348), (legend_x + 20, 348), DISCORD_YELLOW, 2);
        raster.text(legend_x + 25, 345, "OpenSkill", DISCORD_GREY, 1);
        legend_x += 110;
    }
    raster.circle((legend_x + 6, 348), 6, DISCORD_GREEN);
    raster.text(legend_x + 17, 345, "Win", DISCORD_GREY, 1);
    raster.circle((legend_x + 66, 348), 6, DISCORD_RED);
    raster.text(legend_x + 77, 345, "Loss", DISCORD_GREY, 1);
    render(raster)
}

fn mu_to_display(mu: f64) -> f64 {
    ((mu - 25.0) * 50.0).max(0.0).round()
}

#[derive(Clone, Copy, Debug)]
struct ChartRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl ChartRect {
    fn point(
        self,
        index: usize,
        count: usize,
        value: f64,
        minimum: f64,
        maximum: f64,
    ) -> (i32, i32) {
        let x_fraction = index as f64 / count.saturating_sub(1).max(1) as f64;
        let y_fraction = (maximum - value) / (maximum - minimum).max(f64::EPSILON);
        (
            self.x + (x_fraction * f64::from(self.width)) as i32,
            self.y + (y_fraction * f64::from(self.height)) as i32,
        )
    }
}

fn draw_optional_series(
    raster: &mut Raster,
    values: &[Option<f64>],
    minimum: f64,
    maximum: f64,
    chart: ChartRect,
    color: Rgba,
) {
    for index in 0..values.len().saturating_sub(1) {
        if let (Some(first), Some(second)) = (values[index], values[index + 1]) {
            raster.line(
                chart.point(index, values.len(), first, minimum, maximum),
                chart.point(index + 1, values.len(), second, minimum, maximum),
                color,
                2,
            );
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdvantageData {
    pub radiant_gold: Vec<f64>,
    pub radiant_xp: Vec<f64>,
}

#[derive(Clone, Copy, Debug)]
struct AdvantageProjection {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
    minimum: f64,
    maximum: f64,
}

impl AdvantageProjection {
    fn y(self, value: f64) -> i32 {
        self.top
            + ((self.maximum - value) / (self.maximum - self.minimum)
                * f64::from(self.bottom - self.top)) as i32
    }

    fn point(self, index: usize, count: usize, value: f64) -> (i32, i32) {
        let x = self.left
            + ((index as f64 / count.saturating_sub(1).max(1) as f64)
                * f64::from(self.right - self.left)) as i32;
        (x, self.y(value))
    }
}

/// Render gold and XP advantage, or no attachment when both series are absent.
#[must_use]
pub fn draw_advantage_graph(
    data: &AdvantageData,
    match_id: Option<i64>,
) -> Option<Cursor<Vec<u8>>> {
    if data.radiant_gold.is_empty() && data.radiant_xp.is_empty() {
        return None;
    }
    // Matplotlib's ``tight_layout`` leaves a 790×340 attachment for the
    // production Python renderer. Keep that public media contract while
    // drawing the semantic layers directly in the native rasterizer.
    const WIDTH: i32 = 790;
    const HEIGHT: i32 = 340;
    const LEFT: i32 = 56;
    const RIGHT: i32 = 780;
    const TOP: i32 = 35;
    const BOTTOM: i32 = 288;

    let mut raster = Raster::new(WIDTH as usize, HEIGHT as usize, DISCORD_BG);
    raster.fill_rect(LEFT, TOP, RIGHT, BOTTOM, DISCORD_DARKER);
    let title = match_id.map_or_else(
        || "Team Advantages Per Minute".to_owned(),
        |id| format!("Team Advantages Per Minute — Match #{id}"),
    );
    let title_width = Raster::text_width(&title, 1);
    raster.text((WIDTH - title_width) / 2, 10, &title, DISCORD_WHITE, 1);
    let values = data
        .radiant_gold
        .iter()
        .chain(&data.radiant_xp)
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    let actual_min = values.iter().copied().fold(0.0_f64, f64::min);
    let actual_max = values.iter().copied().fold(0.0_f64, f64::max);
    let data_span = (actual_max - actual_min).max(1.0);
    let minimum = actual_min - data_span * 0.05;
    let maximum = actual_max + data_span * 0.05;
    let projection = AdvantageProjection {
        left: LEFT,
        right: RIGHT,
        top: TOP,
        bottom: BOTTOM,
        minimum,
        maximum,
    };
    let zero = projection.y(0.0);

    // Match the production chart's light grid and signed zero reference.
    for value in advantage_ticks(projection) {
        let y = projection.y(value);
        raster.line((LEFT, y), (RIGHT, y), DISCORD_GREY.with_alpha(38), 1);
    }
    for index in 0..=6_i32 {
        let x = LEFT + (RIGHT - LEFT) * index / 6;
        raster.line((x, TOP), (x, BOTTOM), DISCORD_GREY.with_alpha(38), 1);
    }
    raster.line((LEFT, zero), (RIGHT, zero), DISCORD_GREY.with_alpha(128), 1);

    // Fill each gold segment to zero before drawing its line. Splitting a
    // segment at a zero crossing preserves matplotlib's ``interpolate=True``
    // behavior instead of filling the entire interval with one sign.
    draw_advantage_area(&mut raster, &data.radiant_gold, projection);
    draw_advantage_series(
        &mut raster,
        &data.radiant_gold,
        projection,
        DISCORD_YELLOW,
        2,
        false,
    );
    draw_advantage_series(
        &mut raster,
        &data.radiant_xp,
        projection,
        DISCORD_ACCENT,
        1,
        true,
    );

    raster.text(
        LEFT + 5,
        TOP + 7,
        "Radiant",
        DISCORD_GREEN.with_alpha(180),
        1,
    );
    raster.text(
        LEFT + 5,
        BOTTOM - 16,
        "Dire",
        DISCORD_RED.with_alpha(180),
        1,
    );
    let minutes = "Minutes";
    raster.text(
        (LEFT + RIGHT - Raster::text_width(minutes, 1)) / 2,
        BOTTOM + 24,
        minutes,
        DISCORD_GREY,
        1,
    );
    let last_minute = data
        .radiant_gold
        .len()
        .max(data.radiant_xp.len())
        .saturating_sub(1);
    let x_ticks = advantage_x_tick_values(last_minute);
    let x_intervals = x_ticks.len().saturating_sub(1).max(1) as i32;
    for (position, minute) in x_ticks.into_iter().enumerate() {
        let position = position as i32;
        let x = LEFT + (RIGHT - LEFT) * position / x_intervals;
        let label = minute.to_string();
        let label_width = Raster::text_width(&label, 1);
        let label_x = (x - label_width / 2).clamp(LEFT, RIGHT - label_width);
        raster.text(label_x, BOTTOM + 7, &label, DISCORD_GREY, 1);
    }
    draw_advantage_axis_labels(&mut raster, projection);
    draw_advantage_legend(
        &mut raster,
        RIGHT - 120,
        TOP + 8,
        !data.radiant_gold.is_empty(),
        !data.radiant_xp.is_empty(),
    );
    Some(render(raster))
}

fn draw_advantage_series(
    raster: &mut Raster,
    values: &[f64],
    projection: AdvantageProjection,
    color: Rgba,
    width: i32,
    dashed: bool,
) {
    let points = values
        .iter()
        .enumerate()
        .map(|(index, value)| projection.point(index, values.len(), *value))
        .collect::<Vec<_>>();
    for pair in points.windows(2) {
        if dashed {
            draw_dashed_line(raster, pair[0], pair[1], color, width);
        } else {
            raster.line(pair[0], pair[1], color, width);
        }
    }
}

fn draw_advantage_axis_labels(raster: &mut Raster, projection: AdvantageProjection) {
    // Keep the labels compact and deterministic. The native chart uses the
    // same signed values as the Python formatter, with ``k`` for thousands.
    for value in advantage_ticks(projection) {
        let y = projection.y(value);
        let label = format_advantage_axis_value(value);
        let width = Raster::text_width(&label, 1);
        raster.text(projection.left - width - 7, y - 4, &label, DISCORD_GREY, 1);
    }
}

fn advantage_ticks(projection: AdvantageProjection) -> Vec<f64> {
    let span = (projection.maximum - projection.minimum).max(f64::EPSILON);
    (0..=5)
        .map(|index| projection.minimum + span * f64::from(index) / 5.0)
        .collect()
}

fn advantage_x_tick_values(last_minute: usize) -> Vec<usize> {
    let tick_count = last_minute.saturating_add(1).min(7);
    let intervals = tick_count.saturating_sub(1).max(1);
    (0..tick_count)
        .map(|position| (last_minute as f64 * position as f64 / intervals as f64).round() as usize)
        .collect()
}

fn format_advantage_axis_value(value: f64) -> String {
    if value.abs() >= 1_000.0 {
        format!("{:.0}k", value / 1_000.0)
    } else {
        format!("{value:.0}")
    }
}

fn draw_advantage_area(raster: &mut Raster, values: &[f64], projection: AdvantageProjection) {
    let points = values
        .iter()
        .enumerate()
        .map(|(index, value)| projection.point(index, values.len(), *value))
        .collect::<Vec<_>>();
    let zero = projection.y(0.0);
    for (index, pair) in points.windows(2).enumerate() {
        let first_value = values[index];
        let second_value = values[index + 1];
        if !first_value.is_finite() || !second_value.is_finite() {
            continue;
        }
        if first_value == 0.0
            || second_value == 0.0
            || first_value.signum() == second_value.signum()
        {
            let color = if first_value.max(second_value) >= 0.0 {
                DISCORD_GREEN.with_alpha(38)
            } else {
                DISCORD_RED.with_alpha(38)
            };
            raster.polygon(
                &[pair[0], pair[1], (pair[1].0, zero), (pair[0].0, zero)],
                color,
            );
            continue;
        }
        let fraction = first_value.abs() / (first_value.abs() + second_value.abs());
        let crossing_x = pair[0].0 + ((pair[1].0 - pair[0].0) as f64 * fraction).round() as i32;
        let crossing = (crossing_x, zero);
        let first_color = if first_value >= 0.0 {
            DISCORD_GREEN.with_alpha(38)
        } else {
            DISCORD_RED.with_alpha(38)
        };
        let second_color = if second_value >= 0.0 {
            DISCORD_GREEN.with_alpha(38)
        } else {
            DISCORD_RED.with_alpha(38)
        };
        raster.polygon(
            &[pair[0], crossing, (crossing.0, zero), (pair[0].0, zero)],
            first_color,
        );
        raster.polygon(&[crossing, pair[1], (pair[1].0, zero)], second_color);
    }
}

fn draw_dashed_line(
    raster: &mut Raster,
    start: (i32, i32),
    end: (i32, i32),
    color: Rgba,
    width: i32,
) {
    let dx = f64::from(end.0 - start.0);
    let dy = f64::from(end.1 - start.1);
    let length = dx.hypot(dy).max(1.0);
    let mut offset = 0.0;
    while offset < length {
        let next = (offset + 5.0).min(length);
        let first = (
            start.0 + (dx * offset / length).round() as i32,
            start.1 + (dy * offset / length).round() as i32,
        );
        let second = (
            start.0 + (dx * next / length).round() as i32,
            start.1 + (dy * next / length).round() as i32,
        );
        raster.line(first, second, color, width);
        offset += 8.0;
    }
}

fn draw_advantage_legend(raster: &mut Raster, left: i32, top: i32, has_gold: bool, has_xp: bool) {
    let mut x = left;
    if has_gold {
        raster.line((x, top + 4), (x + 18, top + 4), DISCORD_YELLOW, 2);
        raster.text(x + 23, top, "Gold", DISCORD_WHITE, 1);
        x += 62;
    }
    if has_xp {
        draw_dashed_line(raster, (x, top + 4), (x + 18, top + 4), DISCORD_ACCENT, 1);
        raster.text(x + 23, top, "XP", DISCORD_WHITE, 1);
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GambaInfo {
    pub source: String,
    pub outcome: Option<String>,
    pub leverage: i64,
    pub profit: i64,
}

impl GambaInfo {
    fn normalized_leverage(&self) -> i64 {
        self.leverage.max(1)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GambaPoint {
    pub event_number: i32,
    pub cumulative: i64,
    pub info: GambaInfo,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GambaStats {
    pub total_bets: usize,
    pub win_rate: f64,
    pub net_pnl: i64,
    pub roi: f64,
}

#[must_use]
pub fn marker_radius(info: &GambaInfo) -> i32 {
    if info.source == "double_or_nothing" {
        6
    } else if info.source == "wheel" || info.normalized_leverage() > 1 {
        5
    } else {
        3
    }
}

fn marker_priority(info: &GambaInfo) -> i32 {
    if info.source == "double_or_nothing" {
        4
    } else if info.normalized_leverage() > 1 {
        3
    } else if info.source == "wheel" {
        2
    } else {
        1
    }
}

/// Select non-overlapping semantic event markers while retaining required points.
pub fn select_marker_indices(
    points: &[(i32, i32)],
    infos: &[GambaInfo],
    radii: &[i32],
    required_indices: &[usize],
) -> Result<Vec<usize>, &'static str> {
    if points.len() != infos.len() || points.len() != radii.len() {
        return Err("marker points, infos, and radii must have equal lengths");
    }
    let mut selected = required_indices
        .iter()
        .copied()
        .filter(|index| *index < points.len())
        .collect::<Vec<_>>();
    selected.sort_unstable();
    selected.dedup();
    let mut candidates = (0..points.len())
        .filter(|index| !selected.contains(index))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|index| {
        (
            -marker_priority(&infos[*index]),
            -infos[*index].profit.abs(),
            *index,
        )
    });
    for index in candidates {
        let clear = selected.iter().all(|other| {
            let dx = points[index].0 - points[*other].0;
            let dy = points[index].1 - points[*other].1;
            let required = radii[index] + radii[*other] + 3;
            dx * dx + dy * dy >= required * required
        });
        if clear {
            selected.push(index);
        }
    }
    selected.sort_unstable();
    Ok(selected)
}

fn gamba_outcome_color(info: &GambaInfo) -> Rgba {
    match info.outcome.as_deref() {
        Some("won") => DISCORD_GREEN,
        Some("neutral") => DISCORD_GREY,
        _ => DISCORD_RED,
    }
}

fn gamba_star_points(center: (i32, i32), radius: i32) -> Vec<(i32, i32)> {
    (0..16)
        .map(|index| {
            let angle = f64::from(index) * std::f64::consts::PI / 8.0 - std::f64::consts::FRAC_PI_2;
            let point_radius = if index % 2 == 0 {
                f64::from(radius)
            } else {
                f64::from(radius) * 0.42
            };
            (
                center.0 + (point_radius * angle.cos()) as i32,
                center.1 + (point_radius * angle.sin()) as i32,
            )
        })
        .collect()
}

fn draw_gamba_marker(raster: &mut Raster, point: (i32, i32), info: &GambaInfo, color: Rgba) {
    draw_gamba_marker_with_radius(raster, point, info, color, marker_radius(info));
}

fn draw_gamba_marker_with_radius(
    raster: &mut Raster,
    point: (i32, i32),
    info: &GambaInfo,
    color: Rgba,
    radius: i32,
) {
    match info.source.as_str() {
        "wheel" => {
            raster.circle_with_outline(point, radius, color, DISCORD_DARKER);
            let spoke_radius = radius.saturating_sub(2);
            raster.line(
                (point.0 - spoke_radius, point.1),
                (point.0 + spoke_radius, point.1),
                DISCORD_WHITE,
                1,
            );
            raster.line(
                (point.0, point.1 - spoke_radius),
                (point.0, point.1 + spoke_radius),
                DISCORD_WHITE,
                1,
            );
        }
        "double_or_nothing" => {
            let points = gamba_star_points(point, radius);
            raster.polygon(&points, color);
            raster.polygon_outline(&points, DISCORD_DARKER);
        }
        _ if info.normalized_leverage() > 1 => {
            let points = [
                (point.0, point.1 - radius),
                (point.0 + radius, point.1),
                (point.0, point.1 + radius),
                (point.0 - radius, point.1),
            ];
            raster.polygon(&points, color);
            raster.polygon_outline(&points, DISCORD_DARKER);
        }
        _ => raster.circle(point, radius, color),
    }
}

fn draw_gamba_legend(raster: &mut Raster, left: i32, top: i32, infos: &[GambaInfo]) {
    let mut cursor_x = left;
    let group_font = 1;
    let label_font = 1;
    let outcome_marker_radius = 4;
    let group_label = |raster: &mut Raster, cursor_x: &mut i32, label: &str| {
        raster.text(*cursor_x, top + 1, label, DISCORD_GREY, group_font);
        *cursor_x += Raster::text_width(label, group_font) + 10;
    };
    let text_label = |raster: &mut Raster, cursor_x: &mut i32, label: &str| {
        raster.text(*cursor_x, top, label, DISCORD_GREY, label_font);
        *cursor_x += Raster::text_width(label, label_font) + 14;
    };

    let has_outcome = |outcome: &str| {
        infos
            .iter()
            .any(|info| info.outcome.as_deref() == Some(outcome))
    };
    group_label(raster, &mut cursor_x, "OUTCOME");
    for (outcome, label, color) in [
        ("won", "Win", DISCORD_GREEN),
        ("lost", "Loss", DISCORD_RED),
        ("neutral", "Neutral", DISCORD_GREY),
    ] {
        if !has_outcome(outcome) {
            continue;
        }
        raster.circle(
            (cursor_x + outcome_marker_radius, top + 6),
            outcome_marker_radius,
            color,
        );
        cursor_x += outcome_marker_radius * 2 + 4;
        text_label(raster, &mut cursor_x, label);
    }

    let has_leverage = infos.iter().any(|info| info.normalized_leverage() > 1);
    let has_don = infos.iter().any(|info| info.source == "double_or_nothing");
    let has_wheel = infos.iter().any(|info| info.source == "wheel");
    if has_leverage || has_don || has_wheel {
        cursor_x += 4;
        group_label(raster, &mut cursor_x, "TYPE");
        for (label, info) in [
            (
                "Leverage",
                GambaInfo {
                    leverage: 2,
                    ..GambaInfo::default()
                },
            ),
            (
                "DoN",
                GambaInfo {
                    source: "double_or_nothing".to_owned(),
                    ..GambaInfo::default()
                },
            ),
            (
                "Wheel",
                GambaInfo {
                    source: "wheel".to_owned(),
                    ..GambaInfo::default()
                },
            ),
        ] {
            let include = match label {
                "Leverage" => has_leverage,
                "DoN" => has_don,
                _ => has_wheel,
            };
            if !include {
                continue;
            }
            draw_gamba_marker_with_radius(
                raster,
                (cursor_x + GAMBA_LEGEND_TYPE_MARKER_RADIUS, top + 6),
                &info,
                DISCORD_ACCENT,
                GAMBA_LEGEND_TYPE_MARKER_RADIUS,
            );
            cursor_x += GAMBA_LEGEND_TYPE_MARKER_RADIUS * 2 + 4;
            text_label(raster, &mut cursor_x, label);
        }
    }
}

fn gamba_boxes_overlap(first: (i32, i32, i32, i32), second: (i32, i32, i32, i32)) -> bool {
    !(first.2 + 3 <= second.0
        || second.2 + 3 <= first.0
        || first.3 + 3 <= second.1
        || second.3 + 3 <= first.1)
}

struct GambaCalloutPlacement {
    bounds: (i32, i32, i32, i32),
    prefer_left: bool,
    prefer_above: bool,
}

fn draw_gamba_callout(
    raster: &mut Raster,
    point: (i32, i32),
    text: &str,
    color: Rgba,
    placement: GambaCalloutPlacement,
    occupied: &mut Vec<(i32, i32, i32, i32)>,
) {
    let bounds = placement.bounds;
    let text_width = Raster::text_width(text, 1);
    let box_width = text_width + 12;
    let box_height = 15;
    let horizontal_options = [placement.prefer_left, !placement.prefer_left];
    let vertical_options = [placement.prefer_above, !placement.prefer_above];
    let mut placements = Vec::new();
    for place_above in vertical_options {
        for place_left in horizontal_options {
            let raw_x = if place_left {
                point.0 - box_width - 8
            } else {
                point.0 + 8
            };
            let raw_y = if place_above {
                point.1 - box_height - 8
            } else {
                point.1 + 8
            };
            let box_x = raw_x.max(bounds.0 + 3).min(bounds.2 - box_width - 3);
            let box_y = raw_y.max(bounds.1 + 3).min(bounds.3 - box_height - 3);
            placements.push((box_x, box_y, box_x + box_width, box_y + box_height));
        }
    }
    let selected = placements
        .iter()
        .copied()
        .find(|candidate| {
            !occupied
                .iter()
                .any(|used| gamba_boxes_overlap(*candidate, *used))
        })
        .unwrap_or(placements[0]);
    let anchor = (
        point.0.max(selected.0).min(selected.2),
        point.1.max(selected.1).min(selected.3),
    );
    raster.line(point, anchor, color, 1);
    raster.rounded_rect(selected, 5, DISCORD_DARKER, Some(DISCORD_GRID));
    raster.text(selected.0 + 6, selected.1 + 3, text, color, 1);
    occupied.push(selected);
}

fn group_decimal_digits(digits: &str) -> String {
    let mut grouped = String::with_capacity(digits.len() + digits.len().saturating_sub(1) / 3);
    let leading = digits.len() % 3;
    for (index, character) in digits.chars().enumerate() {
        if index != 0 && index % 3 == leading {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped
}

fn format_gamba_count(value: usize) -> String {
    group_decimal_digits(&value.to_string())
}

fn format_gamba_signed(value: i64) -> String {
    let sign = if value >= 0 { '+' } else { '-' };
    format!(
        "{sign}{}",
        group_decimal_digits(&value.unsigned_abs().to_string())
    )
}

fn draw_gamba_stat_strip(raster: &mut Raster, event_count: usize, stats: GambaStats) {
    const LEFT: i32 = 60;
    const RIGHT: i32 = 640;
    const TOP: i32 = 346;
    const BOTTOM: i32 = 389;
    raster.rounded_rect(
        (LEFT, TOP, RIGHT, BOTTOM),
        7,
        DISCORD_DARKER,
        Some(DISCORD_GRID),
    );
    let metrics = [
        ("EVENTS", format_gamba_count(event_count), DISCORD_WHITE),
        ("BETS", format_gamba_count(stats.total_bets), DISCORD_WHITE),
        (
            "BET WIN RATE",
            format!("{:.0}%", stats.win_rate * 100.0),
            if stats.win_rate >= 0.5 {
                DISCORD_GREEN
            } else {
                DISCORD_RED
            },
        ),
        (
            "BET P&L",
            format_gamba_signed(stats.net_pnl),
            if stats.net_pnl >= 0 {
                DISCORD_GREEN
            } else {
                DISCORD_RED
            },
        ),
        (
            "BET ROI",
            format!("{:+.1}%", stats.roi * 100.0),
            if stats.roi >= 0.0 {
                DISCORD_GREEN
            } else {
                DISCORD_RED
            },
        ),
    ];
    let column_width = f64::from(RIGHT - LEFT) / metrics.len() as f64;
    for (index, (label, value, color)) in metrics.into_iter().enumerate() {
        let center = (f64::from(LEFT) + column_width * (index as f64 + 0.5)).round() as i32;
        if index != 0 {
            let divider = (f64::from(LEFT) + column_width * index as f64).round() as i32;
            raster.line((divider, TOP + 8), (divider, BOTTOM - 8), DISCORD_GRID, 1);
        }
        raster.text(
            center - Raster::text_width(label, 1) / 2,
            TOP + 5,
            label,
            DISCORD_GREY,
            1,
        );
        raster.text(
            center - Raster::text_width(&value, 1) / 2,
            TOP + 20,
            &value,
            color,
            1,
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartProjection {
    pub chart_x: i32,
    pub chart_y: i32,
    pub chart_width: i32,
    pub chart_height: i32,
    pub total_x: usize,
    pub min_log_y: f64,
    pub max_log_y: f64,
}

impl ChartProjection {
    #[must_use]
    pub fn log_y_range(self) -> f64 {
        (self.max_log_y - self.min_log_y).max(1e-9)
    }

    #[must_use]
    pub fn to_pixel(self, x_index: i32, y_value: f64) -> (i32, i32) {
        let x = self.chart_x
            + ((f64::from(x_index - 1) / self.total_x.saturating_sub(1).max(1) as f64)
                * f64::from(self.chart_width)) as i32;
        let y = self.chart_y
            + (((self.max_log_y - signed_log(y_value)) / self.log_y_range())
                * f64::from(self.chart_height)) as i32;
        (x, y)
    }
}

#[must_use]
pub fn make_projection(
    values: &[f64],
    total_x: usize,
    chart: (i32, i32, i32, i32),
) -> ChartProjection {
    let logs = values.iter().copied().map(signed_log).collect::<Vec<_>>();
    let mut minimum = logs.iter().copied().fold(0.0_f64, f64::min);
    let mut maximum = logs.iter().copied().fold(0.0_f64, f64::max);
    let range = (maximum - minimum).abs().max(0.1);
    minimum -= range * 0.1;
    maximum += range * 0.1;
    ChartProjection {
        chart_x: chart.0,
        chart_y: chart.1,
        chart_width: chart.2,
        chart_height: chart.3,
        total_x,
        min_log_y: minimum,
        max_log_y: maximum,
    }
}

fn signed_log(value: f64) -> f64 {
    value.signum() * value.abs().ln_1p()
}

/// Mirror Python's round-to-even selection and always retain both timeline ends.
#[must_use]
pub fn select_x_axis_ticks(indices: &[i32], max_labels: usize) -> Vec<i32> {
    if indices.is_empty() || max_labels == 0 {
        return Vec::new();
    }
    let count = indices.len().min(max_labels);
    if count == 1 {
        return vec![indices[0]];
    }
    (0..count)
        .map(|step| {
            let position = round_ratio_ties_even(
                step.saturating_mul(indices.len().saturating_sub(1)),
                count - 1,
            );
            indices[position]
        })
        .collect()
}

fn round_ratio_ties_even(numerator: usize, denominator: usize) -> usize {
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    match remainder.saturating_mul(2).cmp(&denominator) {
        std::cmp::Ordering::Less => quotient,
        std::cmp::Ordering::Greater => quotient + 1,
        std::cmp::Ordering::Equal if quotient.is_multiple_of(2) => quotient,
        std::cmp::Ordering::Equal => quotient + 1,
    }
}

/// Choose signed-log labels with the same maximin spacing policy as Python.
#[must_use]
pub fn select_y_axis_ticks(
    projection: ChartProjection,
    actual_min: f64,
    actual_max: f64,
) -> Vec<(f64, i32)> {
    let mut candidates = Vec::new();
    for magnitude in Y_LABEL_MAGNITUDES {
        for value in [-f64::from(magnitude), f64::from(magnitude)] {
            if value < actual_min * 1.2 || value > actual_max * 1.2 {
                continue;
            }
            let log = signed_log(value);
            if log < projection.min_log_y || log > projection.max_log_y {
                continue;
            }
            candidates.push((value, projection.to_pixel(1, value).1));
        }
    }
    let zero_y = projection.to_pixel(1, 0.0).1;
    let mut occupied = vec![zero_y];
    let mut selected = Vec::new();
    while !candidates.is_empty() && selected.len() < 7 {
        let key = |candidate: &(f64, i32)| {
            (
                occupied
                    .iter()
                    .map(|y| (candidate.1 - y).abs())
                    .min()
                    .unwrap_or(0),
                candidate.0.abs(),
            )
        };
        let mut best = 0;
        for index in 1..candidates.len() {
            let candidate_key = key(&candidates[index]);
            let best_key = key(&candidates[best]);
            if candidate_key.0 > best_key.0
                || (candidate_key.0 == best_key.0 && candidate_key.1 > best_key.1)
            {
                best = index;
            }
        }
        let candidate = candidates.remove(best);
        let distance = occupied
            .iter()
            .map(|y| (candidate.1 - y).abs())
            .min()
            .unwrap_or(0);
        if distance >= 20 {
            occupied.push(candidate.1);
            selected.push(candidate);
        }
    }
    selected.sort_by_key(|tick| tick.1);
    selected
}

/// Render the fixed-size adaptive gamba journey chart.
#[must_use]
pub fn draw_gamba_chart(
    username: &str,
    degen_score: i32,
    degen_title: &str,
    series: &[GambaPoint],
    stats: GambaStats,
) -> Cursor<Vec<u8>> {
    let mut raster = Raster::new(700, 400, DISCORD_BG);
    raster.text_case_sensitive(
        60,
        12,
        &format!("{username}'s Gamba Journey"),
        DISCORD_WHITE,
        2,
    );
    let subtitle = if degen_title.is_empty() {
        format!("Degen Score {degen_score}")
    } else {
        format!("Degen Score {degen_score}  ·  {degen_title}")
    };
    raster.text_case_sensitive(60, 40, &subtitle, DISCORD_GREY, 2);
    if series.is_empty() {
        let message = "No betting history";
        raster.text(
            350 - Raster::text_width(message, 2) / 2,
            200,
            message,
            DISCORD_GREY,
            2,
        );
        return render(raster);
    }
    const CHART_X: i32 = 60;
    const CHART_Y: i32 = 88;
    const CHART_WIDTH: i32 = 614;
    const CHART_HEIGHT: i32 = 222;
    let values = series
        .iter()
        .map(|point| point.cumulative as f64)
        .collect::<Vec<_>>();
    let projection = make_projection(
        &values,
        series.len(),
        (CHART_X, CHART_Y, CHART_WIDTH, CHART_HEIGHT),
    );
    let zero_y = projection.to_pixel(1, 0.0).1;
    draw_gamba_legend(
        &mut raster,
        CHART_X,
        64,
        &series
            .iter()
            .map(|point| point.info.clone())
            .collect::<Vec<_>>(),
    );
    raster.line(
        (CHART_X, zero_y),
        (CHART_X + CHART_WIDTH, zero_y),
        DISCORD_GREY,
        1,
    );
    raster.text(CHART_X - 25, zero_y - 6, "0", DISCORD_GREY, 1);
    for (value, y_pos) in select_y_axis_ticks(
        projection,
        values.iter().copied().fold(f64::INFINITY, f64::min),
        values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    ) {
        raster.line(
            (CHART_X, y_pos),
            (CHART_X + CHART_WIDTH, y_pos),
            DISCORD_GRID,
            1,
        );
        let label = format_compact_axis_value(value);
        let text_width = Raster::text_width(&label, 1);
        raster.text(CHART_X - text_width - 8, y_pos - 6, &label, DISCORD_GREY, 1);
    }
    let event_numbers = series
        .iter()
        .map(|point| point.event_number)
        .collect::<Vec<_>>();
    let baseline_y = CHART_Y + CHART_HEIGHT;
    raster.line(
        (CHART_X, baseline_y),
        (CHART_X + CHART_WIDTH, baseline_y),
        DISCORD_GRID,
        1,
    );
    for value in select_x_axis_ticks(&event_numbers, 5) {
        let (x_pos, _) = projection.to_pixel(value, 0.0);
        raster.line(
            (x_pos, baseline_y),
            (x_pos, baseline_y + 3),
            DISCORD_GREY,
            1,
        );
        let label = value.to_string();
        let text_width = Raster::text_width(&label, 1);
        raster.text(
            x_pos - text_width / 2,
            baseline_y + 6,
            &label,
            DISCORD_GREY,
            1,
        );
    }
    let pixels = series
        .iter()
        .map(|point| projection.to_pixel(point.event_number, point.cumulative as f64))
        .collect::<Vec<_>>();
    if pixels.len() > 1 {
        let mut positive = vec![(CHART_X, zero_y)];
        let mut negative = vec![(CHART_X, zero_y)];
        for (point, pixel) in series.iter().zip(&pixels) {
            positive.push((
                pixel.0,
                if point.cumulative >= 0 {
                    pixel.1
                } else {
                    zero_y
                },
            ));
            negative.push((
                pixel.0,
                if point.cumulative <= 0 {
                    pixel.1
                } else {
                    zero_y
                },
            ));
        }
        positive.push((CHART_X + CHART_WIDTH, zero_y));
        negative.push((CHART_X + CHART_WIDTH, zero_y));
        raster.polygon(&positive, DISCORD_GREEN.with_alpha(38));
        raster.polygon(&negative, DISCORD_RED.with_alpha(38));
        for pair in pixels.windows(2) {
            raster.line(pair[0], pair[1], DISCORD_DARKER, 4);
        }
    }
    for index in 0..pixels.len().saturating_sub(1) {
        let change = series[index + 1].cumulative - series[index].cumulative;
        let color = if change > 0 {
            DISCORD_GREEN
        } else if change < 0 {
            DISCORD_RED
        } else {
            DISCORD_GREY
        };
        raster.line(pixels[index], pixels[index + 1], color, 2);
    }
    let radii = series
        .iter()
        .map(|point| marker_radius(&point.info))
        .collect::<Vec<_>>();
    let peak = values
        .iter()
        .enumerate()
        .max_by(|first, second| first.1.total_cmp(second.1))
        .map_or(0, |entry| entry.0);
    let trough = values
        .iter()
        .enumerate()
        .min_by(|first, second| first.1.total_cmp(second.1))
        .map_or(0, |entry| entry.0);
    let selected = select_marker_indices(
        &pixels,
        &series
            .iter()
            .map(|point| point.info.clone())
            .collect::<Vec<_>>(),
        &radii,
        &[0, series.len() - 1, peak, trough],
    )
    .unwrap_or_default();
    for index in selected {
        draw_gamba_marker(
            &mut raster,
            pixels[index],
            &series[index].info,
            gamba_outcome_color(&series[index].info),
        );
    }
    raster.text(
        CHART_X + 7,
        CHART_Y + 5,
        "P&L · SIGNED LOG SCALE",
        DISCORD_GREY,
        1,
    );

    let mut occupied = Vec::new();
    let bounds = (
        CHART_X,
        CHART_Y,
        CHART_X + CHART_WIDTH,
        CHART_Y + CHART_HEIGHT,
    );
    if values[peak] > 0.0 && peak != series.len() - 1 {
        draw_gamba_callout(
            &mut raster,
            pixels[peak],
            &format!("Peak {}", format_gamba_signed(series[peak].cumulative)),
            DISCORD_GREEN,
            GambaCalloutPlacement {
                bounds,
                prefer_left: pixels[peak].0 > CHART_X + CHART_WIDTH * 65 / 100,
                prefer_above: true,
            },
            &mut occupied,
        );
    }
    if values[trough] < 0.0 && trough != series.len() - 1 {
        draw_gamba_callout(
            &mut raster,
            pixels[trough],
            &format!("Low {}", format_gamba_signed(series[trough].cumulative)),
            DISCORD_RED,
            GambaCalloutPlacement {
                bounds,
                prefer_left: pixels[trough].0 > CHART_X + CHART_WIDTH * 65 / 100,
                prefer_above: false,
            },
            &mut occupied,
        );
    }
    let current = series.len() - 1;
    let current_label = if current == peak {
        "Now · peak"
    } else if current == trough {
        "Now · low"
    } else {
        "Now"
    };
    draw_gamba_callout(
        &mut raster,
        pixels[current],
        &format!(
            "{current_label} {}",
            format_gamba_signed(series[current].cumulative)
        ),
        if series[current].cumulative >= 0 {
            DISCORD_GREEN
        } else {
            DISCORD_RED
        },
        GambaCalloutPlacement {
            bounds,
            prefer_left: true,
            prefer_above: true,
        },
        &mut occupied,
    );
    draw_gamba_stat_strip(&mut raster, series.len(), stats);
    render(raster)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BalancePoint {
    pub event_number: i32,
    pub cumulative: i64,
    pub source: String,
}

/// Render an all-source balance journey on the shared signed-log projection.
#[must_use]
pub fn draw_balance_chart(
    username: &str,
    series: &[BalancePoint],
    source_totals: &BTreeMap<String, i64>,
) -> Cursor<Vec<u8>> {
    let mut raster = Raster::new(700, 400, DISCORD_BG);
    const CHART_X: i32 = 60;
    const CHART_Y: i32 = 70;
    const CHART_WIDTH: i32 = 580;
    const CHART_HEIGHT: i32 = 250;
    const PADDING: i32 = 60;
    raster.text(
        PADDING,
        12,
        &format!("{username}'s Balance Journey"),
        DISCORD_WHITE,
        2,
    );
    let net = source_totals.values().sum::<i64>();
    raster.text(
        PADDING,
        38,
        &format!("Net: {net:+} jopacoin across {} events", series.len()),
        if net >= 0 { DISCORD_GREEN } else { DISCORD_RED },
        1,
    );
    if series.is_empty() {
        raster.text(220, 200, "No balance history yet", DISCORD_GREY, 2);
        return render(raster);
    }
    let values = series
        .iter()
        .map(|point| point.cumulative as f64)
        .collect::<Vec<_>>();
    let projection = make_projection(
        &values,
        series.len(),
        (CHART_X, CHART_Y, CHART_WIDTH, CHART_HEIGHT),
    );
    let zero_y = projection.to_pixel(1, 0.0).1;
    raster.line(
        (CHART_X, zero_y),
        (CHART_X + CHART_WIDTH, zero_y),
        DISCORD_GREY,
        1,
    );
    raster.text(CHART_X - 25, zero_y - 6, "0", DISCORD_GREY, 1);
    for (value, y_pos) in select_y_axis_ticks(
        projection,
        values.iter().copied().fold(f64::INFINITY, f64::min),
        values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    ) {
        raster.line(
            (CHART_X, y_pos),
            (CHART_X + CHART_WIDTH, y_pos),
            DISCORD_GRID,
            1,
        );
        let label = format_compact_axis_value(value);
        let text_width = Raster::text_width(&label, 1);
        raster.text(CHART_X - text_width - 8, y_pos - 6, &label, DISCORD_GREY, 1);
    }
    let event_numbers = series
        .iter()
        .map(|point| point.event_number)
        .collect::<Vec<_>>();
    let baseline_y = CHART_Y + CHART_HEIGHT;
    raster.line(
        (CHART_X, baseline_y),
        (CHART_X + CHART_WIDTH, baseline_y),
        DISCORD_GRID,
        1,
    );
    for value in select_x_axis_ticks(&event_numbers, 5) {
        let (x_pos, _) = projection.to_pixel(value, 0.0);
        raster.line(
            (x_pos, baseline_y),
            (x_pos, baseline_y + 3),
            DISCORD_GREY,
            1,
        );
        let label = value.to_string();
        let text_width = Raster::text_width(&label, 1);
        raster.text(
            x_pos - text_width / 2,
            baseline_y + 6,
            &label,
            DISCORD_GREY,
            1,
        );
    }
    let points = series
        .iter()
        .map(|point| projection.to_pixel(point.event_number, point.cumulative as f64))
        .collect::<Vec<_>>();
    if points.len() > 1 {
        let mut positive = vec![(CHART_X, zero_y)];
        let mut negative = vec![(CHART_X, zero_y)];
        for (point, pixel) in series.iter().zip(&points) {
            positive.push((
                pixel.0,
                if point.cumulative >= 0 {
                    pixel.1
                } else {
                    zero_y
                },
            ));
            negative.push((
                pixel.0,
                if point.cumulative <= 0 {
                    pixel.1
                } else {
                    zero_y
                },
            ));
        }
        positive.push((CHART_X + CHART_WIDTH, zero_y));
        negative.push((CHART_X + CHART_WIDTH, zero_y));
        raster.polygon(&positive, Rgba::rgb(0x57, 0xf2, 0x87).with_alpha(60));
        raster.polygon(&negative, Rgba::rgb(0xed, 0x42, 0x45).with_alpha(60));
    }
    for index in 0..points.len().saturating_sub(1) {
        let color = if series[index + 1].cumulative >= series[index].cumulative {
            DISCORD_GREEN
        } else {
            DISCORD_RED
        };
        raster.line(points[index], points[index + 1], color, 2);
    }
    for (point, pixel) in series.iter().zip(points) {
        raster.circle_with_outline(pixel, 4, source_color(&point.source), DISCORD_WHITE);
    }

    let peak_value = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let peak_index = values
        .iter()
        .position(|value| *value == peak_value)
        .unwrap_or(0);
    let trough_value = values.iter().copied().fold(f64::INFINITY, f64::min);
    let trough_index = values
        .iter()
        .position(|value| *value == trough_value)
        .unwrap_or(0);
    let (peak_x, peak_y) = projection.to_pixel(
        series[peak_index].event_number,
        series[peak_index].cumulative as f64,
    );
    if series[peak_index].cumulative > 0 {
        raster.text(
            peak_x + 5,
            peak_y - 15,
            &format!("Peak: +{}", series[peak_index].cumulative),
            DISCORD_GREEN,
            1,
        );
    }
    let (trough_x, trough_y) = projection.to_pixel(
        series[trough_index].event_number,
        series[trough_index].cumulative as f64,
    );
    if series[trough_index].cumulative < 0 {
        raster.text(
            trough_x + 5,
            trough_y + 5,
            &format!("Trough: {}", series[trough_index].cumulative),
            DISCORD_RED,
            1,
        );
    }
    let current = series.last().expect("non-empty balance series");
    let (current_x, current_y) =
        projection.to_pixel(current.event_number, current.cumulative as f64);
    raster.text(
        current_x - 40,
        current_y - 20,
        &format!("Now: {:+}", current.cumulative),
        if current.cumulative >= 0 {
            DISCORD_GREEN
        } else {
            DISCORD_RED
        },
        1,
    );

    let mut sources_present = Vec::new();
    for point in series {
        if !sources_present.iter().any(|source| source == &point.source)
            && source_label(&point.source).is_some()
        {
            sources_present.push(point.source.as_str());
        }
    }
    let legend_y = CHART_Y + CHART_HEIGHT + 22;
    let mut cursor_x = PADDING;
    let max_x = 700 - PADDING;
    for source in sources_present {
        let label = source_label(source).unwrap_or(source);
        let label_width = Raster::text_width(label, 1);
        let entry_width = 10 + 4 + label_width + 14;
        if cursor_x + entry_width > max_x && cursor_x != PADDING {
            break;
        }
        raster.circle_with_outline(
            (cursor_x + 5, legend_y + 6),
            5,
            source_color(source),
            DISCORD_WHITE,
        );
        raster.text(cursor_x + 14, legend_y, label, DISCORD_GREY, 1);
        cursor_x += entry_width;
    }

    let mut totals = source_totals.iter().collect::<Vec<_>>();
    totals.sort_by_key(|(_, value)| std::cmp::Reverse(value.abs()));
    let mut footer = if totals.is_empty() {
        "No activity".to_owned()
    } else {
        totals
            .iter()
            .map(|(source, value)| {
                format!("{} {:+}", source_label(source).unwrap_or(source), value)
            })
            .collect::<Vec<_>>()
            .join("  |  ")
    };
    if Raster::text_width(&footer, 1) > 700 - PADDING * 2 {
        // The deterministic bitmap font is wider than Python's pinned TTF.
        // Preserve the same per-source information with tighter separators
        // before falling back to the terse overflow summary.
        footer = totals
            .iter()
            .map(|(source, value)| {
                format!("{} {:+}", source_label(source).unwrap_or(source), value)
            })
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if Raster::text_width(&footer, 1) > 700 - PADDING * 2 {
        footer = format!("{} events | net {net:+}", series.len());
    }
    let footer_width = Raster::text_width(&footer, 1);
    raster.text((700 - footer_width) / 2, 365, &footer, DISCORD_WHITE, 1);
    render(raster)
}

/// Render the production prediction-market fair-history chart as a real PNG.
///
/// The geometry and branch decisions mirror ``draw_market_fair_history`` in
/// ``utils/drawing/predictions.py``. The native renderer deliberately uses a
/// tiny deterministic bitmap font (rather than a process-/host-dependent
/// system font); title wrapping therefore measures that fallback font, while
/// retaining Python's greedy word wrapping and fixed 700×280 canvas.
///
/// `now` is supplied by the worker clock so scheduling and image output share
/// one time snapshot.
#[must_use]
pub fn draw_prediction_market_chart(
    market_id: i64,
    title: Option<&str>,
    snapshots: &[(i64, i64)],
    created_at: i64,
    now: i64,
) -> Cursor<Vec<u8>> {
    const WIDTH: i32 = 700;
    const HEIGHT: i32 = 280;
    const PADDING_LEFT: i32 = 50;
    const PADDING_RIGHT: i32 = 30;
    const PADDING_BOTTOM: i32 = 40;
    const TITLE_Y: i32 = 12;
    const TITLE_LINE_HEIGHT: i32 = 22;
    const TITLE_SCALE: i32 = 2;
    const LABEL_SCALE: i32 = 1;
    const LATEST_SCALE: i32 = 2;
    const CHART_WIDTH: i32 = WIDTH - PADDING_LEFT - PADDING_RIGHT;
    const CHART_BOTTOM: i32 = HEIGHT - PADDING_BOTTOM;

    let mut raster = Raster::new(WIDTH as usize, HEIGHT as usize, DISCORD_BG);
    let raw_title = match title {
        Some(value) if !value.is_empty() => value.to_owned(),
        _ => format!("Market #{market_id} — fair history"),
    };
    let title_lines = wrap_prediction_title(&raw_title, CHART_WIDTH, TITLE_SCALE);
    for (index, line) in title_lines.iter().enumerate() {
        let y = TITLE_Y.saturating_add(
            TITLE_LINE_HEIGHT.saturating_mul(i32::try_from(index).unwrap_or(i32::MAX)),
        );
        raster.text(PADDING_LEFT, y, line, DISCORD_WHITE, TITLE_SCALE);
    }
    let chart_x = PADDING_LEFT;
    let chart_y = TITLE_Y
        .saturating_add(
            TITLE_LINE_HEIGHT.saturating_mul(i32::try_from(title_lines.len()).unwrap_or(i32::MAX)),
        )
        .saturating_add(6);
    let chart_height = CHART_BOTTOM.saturating_sub(chart_y).max(40);

    // Y range: auto-zoom to data with ±10pp padding snapped to nearest 10
    // when there are at least two snapshots; otherwise use the full axis.
    let (low, high) = if snapshots.len() >= 2 {
        let minimum = snapshots.iter().map(|(_, fair)| *fair).min().unwrap_or(0);
        let maximum = snapshots.iter().map(|(_, fair)| *fair).max().unwrap_or(100);
        let low = ((minimum.saturating_sub(10)).div_euclid(10) * 10).max(0);
        let high = (maximum.saturating_add(10).saturating_add(9).div_euclid(10) * 10).min(100);
        if high > low { (low, high) } else { (0, 100) }
    } else {
        (0, 100)
    };
    let vertical_span = (high - low).max(1);
    let last_timestamp = snapshots
        .last()
        .map_or(now.max(created_at.saturating_add(1)), |(timestamp, _)| {
            now.max(*timestamp)
        });
    let horizontal_span = last_timestamp.saturating_sub(created_at).max(1);
    let x_for = |timestamp: i64| {
        prediction_x_for(timestamp, created_at, horizontal_span, chart_x, CHART_WIDTH)
    };
    let y_for = |fair: i64| {
        let clamped = fair.clamp(low, high);
        chart_y
            + i32::try_from(i64::from(chart_height).saturating_mul(high - clamped) / vertical_span)
                .unwrap_or_default()
    };

    // Python uses round() for the five evenly-spaced gridline labels. This
    // helper preserves its ties-to-even behavior for integer percentage axes.
    for index in 0..=4_i64 {
        let fair = low + round_div_4_bankers(vertical_span * index);
        let y = y_for(fair);
        raster.line((chart_x, y), (chart_x + CHART_WIDTH, y), DISCORD_GREY, 1);
        let label = format!("{fair}%");
        let text_width = Raster::text_width(&label, LABEL_SCALE);
        raster.text(
            chart_x.saturating_sub(text_width).saturating_sub(6),
            y - 7,
            &label,
            DISCORD_GREY,
            LABEL_SCALE,
        );
    }

    // UTC labels: start/end plus two or four interior ticks, matching Python's
    // short-span HH:MM versus long-span MM-DD choice.
    let tick_count = if horizontal_span >= 4 * 60 * 60 {
        4_i64
    } else {
        2_i64
    };
    for index in 0..=tick_count {
        let timestamp =
            created_at.saturating_add(horizontal_span.saturating_mul(index) / tick_count.max(1));
        let label = prediction_utc_label(timestamp, horizontal_span);
        let text_width = Raster::text_width(&label, LABEL_SCALE);
        raster.text(
            x_for(timestamp).saturating_sub(text_width / 2),
            chart_y + chart_height + 6,
            &label,
            DISCORD_GREY,
            LABEL_SCALE,
        );
    }

    let points = snapshots
        .iter()
        .map(|(timestamp, fair)| (x_for(*timestamp), y_for(*fair)))
        .collect::<Vec<_>>();
    if points.len() == 1 {
        let point = points[0];
        raster.line(
            (chart_x, point.1),
            (chart_x + CHART_WIDTH, point.1),
            DISCORD_ACCENT,
            2,
        );
        raster.circle_with_outline(point, 3, DISCORD_ACCENT, DISCORD_WHITE);
    } else {
        for pair in points.windows(2) {
            raster.line(pair[0], pair[1], DISCORD_ACCENT, 2);
        }
        for point in &points {
            raster.circle_with_outline(*point, 2, DISCORD_ACCENT, DISCORD_WHITE);
        }
    }
    if let (Some(point), Some((_, fair))) = (points.last(), snapshots.last()) {
        let label = format!("{fair}%");
        let text_width = Raster::text_width(&label, LATEST_SCALE);
        raster.text(
            (point.0 + 6).min(chart_x + CHART_WIDTH - text_width),
            point.1 - 8,
            &label,
            DISCORD_WHITE,
            LATEST_SCALE,
        );
    }
    render(raster)
}

/// Greedy word wrapping equivalent to Python's ``_wrap_text``. A word wider
/// than the available width is kept intact and allowed to overflow.
fn wrap_prediction_title(text: &str, max_width: i32, scale: i32) -> Vec<String> {
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return vec![String::new()];
    };
    let mut lines = Vec::new();
    let mut current = first.to_owned();
    for word in words {
        let candidate = format!("{current} {word}");
        if Raster::text_width(&candidate, scale) <= max_width {
            current = candidate;
        } else {
            lines.push(current);
            current = word.to_owned();
        }
    }
    lines.push(current);
    lines
}

fn round_div_4_bankers(value: i64) -> i64 {
    let quotient = value.div_euclid(4);
    match value.rem_euclid(4) {
        0 | 1 => quotient,
        3 => quotient + 1,
        2 if quotient % 2 == 0 => quotient,
        2 => quotient + 1,
        _ => unreachable!("remainder modulo four is bounded"),
    }
}

fn prediction_x_for(timestamp: i64, created_at: i64, span: i64, left: i32, width: i32) -> i32 {
    left.saturating_add(
        i32::try_from(
            i64::from(width).saturating_mul(timestamp.saturating_sub(created_at)) / span.max(1),
        )
        .unwrap_or_default(),
    )
}

fn prediction_utc_label(timestamp: i64, span: i64) -> String {
    let format = if span < 24 * 60 * 60 {
        "%H:%M"
    } else {
        "%m-%d"
    };
    Utc.timestamp_opt(timestamp, 0)
        .single()
        .map_or_else(|| "--".to_owned(), |date| date.format(format).to_string())
}

fn source_color(source: &str) -> Rgba {
    match source {
        "bets" => Rgba::rgb(0x3b, 0x82, 0xf6),
        "predictions" => Rgba::rgb(0xa8, 0x55, 0xf7),
        "wheel" => Rgba::rgb(0xfa, 0xcc, 0x15),
        "double_or_nothing" => Rgba::rgb(0xf9, 0x73, 0x16),
        "tips" => Rgba::rgb(0xec, 0x48, 0x99),
        "disburse" => Rgba::rgb(0x22, 0xc5, 0x5e),
        "bonus" => Rgba::rgb(0x9c, 0xa3, 0xaf),
        "dig" => Rgba::rgb(0xa1, 0x62, 0x07),
        _ => Rgba::rgb(0x3b, 0x82, 0xf6),
    }
}

fn source_label(source: &str) -> Option<&'static str> {
    match source {
        "bets" => Some("Bets"),
        "predictions" => Some("Predictions"),
        "wheel" => Some("Wheel"),
        "double_or_nothing" => Some("DoN"),
        "tips" => Some("Tips"),
        "disburse" => Some("Disburse"),
        "bonus" => Some("Bonuses"),
        "dig" => Some("Dig"),
        _ => None,
    }
}

fn format_compact_axis_value(value: f64) -> String {
    let magnitude = value.abs();
    let compact = if magnitude >= 1_000_000.0 {
        format!("{}m", magnitude / 1_000_000.0)
    } else if magnitude >= 1_000.0 {
        format!("{}k", magnitude / 1_000.0)
    } else {
        format!("{magnitude}")
    };
    if value > 0.0 {
        format!("+{compact}")
    } else {
        format!("-{compact}")
    }
}

/// Render the rating-distribution diagnostic used by `/calibration`.
///
/// The chart retains the Python surface: 100-point density bins, normal and
/// KDE overlays when the sample has spread, mean/median markers, axes, legend,
/// and descriptive statistics. Rendering is native and deterministic, so the
/// production Rust process does not require matplotlib/scipy.
#[must_use]
pub fn draw_rating_distribution(ratings: &[f64]) -> Cursor<Vec<u8>> {
    let median_rating = if ratings.is_empty() {
        None
    } else {
        let mut sorted = ratings.to_vec();
        sorted.sort_by(f64::total_cmp);
        Some(if sorted.len().is_multiple_of(2) {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
        } else {
            sorted[sorted.len() / 2]
        })
    };
    draw_rating_distribution_with_median(ratings, median_rating)
}

/// Render the rating-distribution diagnostic with the service's explicit
/// median value.
///
/// The live Python `/calibration` boundary passes its already-computed median
/// into the drawing helper. `None` intentionally suppresses the median marker
/// and legend entry instead of silently recomputing it.
#[must_use]
pub fn draw_rating_distribution_with_median(
    ratings: &[f64],
    median_rating: Option<f64>,
) -> Cursor<Vec<u8>> {
    const LEFT: i32 = 84;
    const RIGHT: i32 = 630;
    const TOP: i32 = 39;
    const BOTTOM: i32 = 338;
    const PLOT_HEIGHT: f64 = 285.0;
    const BIN_WIDTH: f64 = 100.0;
    const SPINE: Rgba = Rgba::rgb(0x4f, 0x54, 0x5c);
    const MEDIAN: Rgba = Rgba::rgb(0xf4, 0x7b, 0x67);

    if ratings.is_empty() {
        let mut raster = Raster::new(640, 390, DISCORD_BG);
        raster.fill_rect(84, TOP, RIGHT, BOTTOM, DISCORD_DARKER);
        raster.text(240, 190, "No rating data", DISCORD_WHITE, 2);
        return render(raster);
    }
    let mean = ratings.iter().sum::<f64>() / ratings.len() as f64;
    let variance = ratings
        .iter()
        .map(|rating| (rating - mean).powi(2))
        .sum::<f64>()
        / ratings.len() as f64;
    let deviation = variance.sqrt();
    let has_spread = deviation > f64::EPSILON * mean.abs().max(1.0);
    let mut sorted = ratings.to_vec();
    sorted.sort_by(f64::total_cmp);
    let minimum = ratings.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = ratings.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let histogram_start = (minimum / BIN_WIDTH).floor().max(0.0) * BIN_WIDTH;
    let histogram_end = (maximum / BIN_WIDTH).ceil() * BIN_WIDTH + BIN_WIDTH;
    let histogram_span = histogram_end - histogram_start;
    let axis_padding = histogram_span * 0.05;
    let axis_start = (histogram_start - axis_padding).max(0.0);
    let axis_end = histogram_end + axis_padding;
    let bin_count = ((histogram_span / BIN_WIDTH) as usize).max(1);
    let mut bins = vec![0_usize; bin_count];
    for rating in ratings {
        let index = (((rating - histogram_start) / BIN_WIDTH) as usize).min(bin_count - 1);
        bins[index] += 1;
    }

    let normal_density = |x: f64| {
        if has_spread {
            (-(x - mean).powi(2) / (2.0 * variance)).exp()
                / (deviation * (2.0 * std::f64::consts::PI).sqrt())
        } else {
            0.0
        }
    };
    let kde_bandwidth = if has_spread && ratings.len() >= 5 {
        let sample_deviation = (ratings
            .iter()
            .map(|rating| (rating - mean).powi(2))
            .sum::<f64>()
            / (ratings.len() - 1) as f64)
            .sqrt();
        Some(sample_deviation * (ratings.len() as f64).powf(-0.2))
            .filter(|bandwidth| *bandwidth > f64::EPSILON)
    } else {
        None
    };
    let kde_density = |x: f64| {
        kde_bandwidth.map_or(0.0, |bandwidth| {
            ratings
                .iter()
                .map(|rating| {
                    let z = (x - rating) / bandwidth;
                    (-0.5 * z * z).exp() / (bandwidth * (2.0 * std::f64::consts::PI).sqrt())
                })
                .sum::<f64>()
                / ratings.len() as f64
        })
    };
    let histogram_peak = bins
        .iter()
        .map(|count| *count as f64 / (ratings.len() as f64 * BIN_WIDTH))
        .fold(0.0_f64, f64::max);
    let curve_peak = (0..200)
        .map(|index| histogram_start + f64::from(index) / 199.0 * histogram_span)
        .map(|x| normal_density(x).max(kde_density(x)))
        .fold(0.0_f64, f64::max);
    let density_peak = histogram_peak.max(curve_peak).max(f64::EPSILON);
    let mut raster = Raster::new(640, 390, DISCORD_BG);
    raster.fill_rect(LEFT, TOP, RIGHT, BOTTOM, DISCORD_DARKER);
    let rating_x = |rating: f64| {
        LEFT + ((rating - axis_start) / (axis_end - axis_start) * f64::from(RIGHT - LEFT)) as i32
    };
    let curve_left = rating_x(histogram_start);
    let curve_right = rating_x(histogram_end);

    for index in 1..=8 {
        let y = BOTTOM - index * (BOTTOM - TOP) / 8;
        raster.line((LEFT, y), (RIGHT, y), DISCORD_GRID, 1);
    }
    for (index, count) in bins.iter().enumerate() {
        let left = rating_x(histogram_start + index as f64 * BIN_WIDTH);
        let right = rating_x(histogram_start + (index + 1) as f64 * BIN_WIDTH);
        let density = *count as f64 / (ratings.len() as f64 * BIN_WIDTH);
        let top = BOTTOM - (density / density_peak * PLOT_HEIGHT) as i32;
        raster.fill_rect(left, top, right - 1, BOTTOM, DISCORD_ACCENT.with_alpha(180));
    }

    if has_spread {
        let mut previous = None;
        for pixel_x in curve_left..=curve_right {
            let x = histogram_start
                + f64::from(pixel_x - curve_left) / f64::from(curve_right - curve_left)
                    * histogram_span;
            let curve_y = BOTTOM - (normal_density(x) / density_peak * PLOT_HEIGHT) as i32;
            let point = (pixel_x, curve_y.min(BOTTOM - 1));
            if let Some(before) = previous {
                raster.line(before, point, DISCORD_GREEN, 2);
            }
            previous = Some(point);
        }
    }

    if kde_bandwidth.is_some() {
        let mut previous = None;
        for pixel_x in curve_left..=curve_right {
            let x = histogram_start
                + f64::from(pixel_x - curve_left) / f64::from(curve_right - curve_left)
                    * histogram_span;
            let curve_y = BOTTOM - (kde_density(x) / density_peak * PLOT_HEIGHT) as i32;
            let point = (pixel_x, curve_y.min(BOTTOM - 1));
            if pixel_x % 8 < 5
                && let Some(before) = previous
            {
                raster.line(before, point, DISCORD_YELLOW, 2);
            }
            previous = Some(point);
        }
    }

    let mean_x = rating_x(mean);
    raster.line(
        (mean_x, TOP),
        (mean_x, BOTTOM),
        DISCORD_RED.with_alpha(205),
        1,
    );
    if let Some(median) = median_rating {
        let median_x = rating_x(median);
        let mut y = TOP;
        while y < BOTTOM {
            raster.line((median_x, y), (median_x, (y + 5).min(BOTTOM)), MEDIAN, 1);
            y += 9;
        }
    }

    raster.line((LEFT, TOP), (LEFT, BOTTOM), SPINE, 1);
    raster.line((LEFT, BOTTOM), (RIGHT, BOTTOM), SPINE, 1);
    let tick_count = ((histogram_span / 50.0) as usize).max(1);
    for index in 0..=tick_count {
        let value = histogram_start + index as f64 / tick_count as f64 * histogram_span;
        let x = rating_x(value);
        raster.line((x, BOTTOM), (x, BOTTOM + 4), SPINE, 1);
        raster.text(x - 12, BOTTOM + 8, &format!("{value:.0}"), DISCORD_GREY, 1);
    }
    raster.text(300, 366, "Rating", DISCORD_GREY, 1);
    raster.text(8, 188, "Density", DISCORD_GREY, 1);

    let (skewness, kurtosis) = if has_spread {
        let skewness = ratings
            .iter()
            .map(|rating| ((rating - mean) / deviation).powi(3))
            .sum::<f64>()
            / ratings.len() as f64;
        let kurtosis = ratings
            .iter()
            .map(|rating| ((rating - mean) / deviation).powi(4))
            .sum::<f64>()
            / ratings.len() as f64
            - 3.0;
        (format!("{skewness:.2}"), format!("{kurtosis:.2}"))
    } else {
        ("N/A".to_owned(), "N/A".to_owned())
    };
    let normality = distribution_normality_p_value(&sorted).map(|probability| {
        format!(
            "{} (p={probability:.3})",
            if probability > 0.05 {
                "Normal"
            } else {
                "Non-normal"
            }
        )
    });
    let stats_left = LEFT + 6;
    let stats_bottom = if normality.is_some() { 100 } else { 88 };
    raster.fill_rect(
        stats_left,
        42,
        stats_left + 182,
        stats_bottom,
        DISCORD_DARKER.with_alpha(230),
    );
    raster.text(
        stats_left + 6,
        48,
        &format!("μ={mean:.0}, σ={deviation:.0}"),
        DISCORD_GREY,
        1,
    );
    raster.text(
        stats_left + 6,
        60,
        &format!("Skew={skewness}, Kurt={kurtosis}"),
        DISCORD_GREY,
        1,
    );
    if let Some(normality) = &normality {
        raster.text(stats_left + 6, 72, normality, DISCORD_GREY, 1);
    }

    let legend_x = 510;
    let mut legend = vec![(format!("Data (n={})", ratings.len()), DISCORD_ACCENT)];
    if has_spread {
        legend.push(("Normal fit".to_owned(), DISCORD_GREEN));
    }
    if kde_bandwidth.is_some() {
        legend.push(("KDE".to_owned(), DISCORD_YELLOW));
    }
    legend.push((format!("Mean: {mean:.0}"), DISCORD_RED));
    if let Some(median) = median_rating {
        legend.push((format!("Median: {median:.0}"), MEDIAN));
    }
    raster.fill_rect(
        legend_x - 8,
        40,
        RIGHT - 5,
        54 + i32::try_from(legend.len()).unwrap_or(5) * 15,
        DISCORD_DARKER.with_alpha(230),
    );
    for (index, (label, color)) in legend.into_iter().enumerate() {
        let y = 47 + index as i32 * 15;
        raster.line((legend_x, y + 3), (legend_x + 16, y + 3), color, 2);
        raster.text(legend_x + 22, y, &label, DISCORD_WHITE, 1);
    }
    raster.text(
        145,
        15,
        &format!("Rating Distribution (n={})", ratings.len()),
        DISCORD_WHITE,
        2,
    );
    render(raster)
}

/// Shapiro-Wilk/Royston normality probability used by scipy for samples up to
/// 5,000. Larger samples use the same D'Agostino-Pearson chi-square test as
/// scipy's `normaltest` branch.
fn distribution_normality_p_value(sorted: &[f64]) -> Option<f64> {
    let count = sorted.len();
    if count < 3 || sorted[count - 1] <= sorted[0] {
        return None;
    }
    if count > 5_000 {
        let mean = sorted.iter().sum::<f64>() / count as f64;
        let second = sorted
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / count as f64;
        if second <= f64::EPSILON {
            return None;
        }
        let skew = sorted
            .iter()
            .map(|value| (value - mean).powi(3))
            .sum::<f64>()
            / count as f64
            / second.powf(1.5);
        let kurtosis = sorted
            .iter()
            .map(|value| (value - mean).powi(4))
            .sum::<f64>()
            / count as f64
            / second.powi(2)
            - 3.0;
        return Some(dagostino_pearson_p_value(count, skew, kurtosis + 3.0));
    }

    const C1: [f64; 6] = [0.0, 0.221_157, -0.147_981, -2.071_19, 4.434_685, -2.706_056];
    const C2: [f64; 6] = [
        0.0, 0.042_981, -0.293_762, -1.752_461, 5.682_633, -3.582_633,
    ];
    const C3: [f64; 4] = [0.544, -0.399_78, 0.025_054, -0.000_671_4];
    const C4: [f64; 4] = [1.382_2, -0.778_57, 0.062_767, -0.002_032_2];
    const C5: [f64; 3] = [-1.586_1, -0.310_82, -0.083_751];
    const C6: [f64; 3] = [-0.480_3, -0.082_676, 0.003_030_2];

    let half = count / 2;
    let adjusted_count = count as f64 + 0.25;
    let mut weights = (1..=half)
        .map(|index| inverse_normal_cdf((index as f64 - 0.375) / adjusted_count))
        .collect::<Vec<_>>();
    let squared_sum = 2.0 * weights.iter().map(|weight| weight.powi(2)).sum::<f64>();
    let root_sum = squared_sum.sqrt();
    let reciprocal_root_count = 1.0 / (count as f64).sqrt();
    let first = polynomial(&C1, reciprocal_root_count) - weights[0] / root_sum;
    let start = if count > 5 {
        let second = -weights[1] / root_sum + polynomial(&C2, reciprocal_root_count);
        let factor = ((squared_sum - 2.0 * weights[0].powi(2) - 2.0 * weights[1].powi(2))
            / (1.0 - 2.0 * first.powi(2) - 2.0 * second.powi(2)))
        .sqrt();
        weights[1] = second;
        for weight in &mut weights[2..] {
            *weight /= -factor;
        }
        2
    } else {
        let factor =
            ((squared_sum - 2.0 * weights[0].powi(2)) / (1.0 - 2.0 * first.powi(2))).sqrt();
        for weight in &mut weights[1..] {
            *weight /= -factor;
        }
        1
    };
    weights[0] = first;
    debug_assert!(start <= weights.len());

    let mean = sorted.iter().sum::<f64>() / count as f64;
    let denominator = sorted
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>();
    if denominator <= f64::EPSILON {
        return None;
    }
    let numerator = weights
        .iter()
        .enumerate()
        .map(|(index, weight)| weight * (sorted[count - 1 - index] - sorted[index]))
        .sum::<f64>()
        .powi(2);
    let statistic = (numerator / denominator).clamp(0.0, 1.0);
    if count == 3 {
        return Some(
            (6.0 / std::f64::consts::PI * (statistic.sqrt().asin() - (0.75_f64).sqrt().asin()))
                .clamp(0.0, 1.0),
        );
    }

    let mut transformed = (1.0 - statistic).max(f64::MIN_POSITIVE).ln();
    let (location, scale) = if count <= 11 {
        let gamma = -2.273 + 0.459 * count as f64;
        if transformed >= gamma {
            return Some(0.0);
        }
        transformed = -(gamma - transformed).ln();
        (
            polynomial(&C3, count as f64),
            polynomial(&C4, count as f64).exp(),
        )
    } else {
        let log_count = (count as f64).ln();
        (polynomial(&C5, log_count), polynomial(&C6, log_count).exp())
    };
    Some(normal_survival((transformed - location) / scale).clamp(0.0, 1.0))
}

fn dagostino_pearson_p_value(count: usize, skew: f64, pearson_kurtosis: f64) -> f64 {
    let count = count as f64;
    let mut adjusted_skew = skew * (((count + 1.0) * (count + 3.0)) / (6.0 * (count - 2.0))).sqrt();
    let beta_two = 3.0 * (count.powi(2) + 27.0 * count - 70.0) * (count + 1.0) * (count + 3.0)
        / ((count - 2.0) * (count + 5.0) * (count + 7.0) * (count + 9.0));
    let w_squared = -1.0 + (2.0 * (beta_two - 1.0)).sqrt();
    let delta = 1.0 / (0.5 * w_squared.ln()).sqrt();
    let alpha = (2.0 / (w_squared - 1.0)).sqrt();
    // Preserve scipy.stats.skewtest's exact zero-skew branch.
    if adjusted_skew == 0.0 {
        adjusted_skew = 1.0;
    }
    let skew_z =
        delta * (adjusted_skew / alpha + ((adjusted_skew / alpha).powi(2) + 1.0).sqrt()).ln();

    let expected = 3.0 * (count - 1.0) / (count + 1.0);
    let variance = 24.0 * count * (count - 2.0) * (count - 3.0)
        / ((count + 1.0).powi(2) * (count + 3.0) * (count + 5.0));
    let standardized = (pearson_kurtosis - expected) / variance.sqrt();
    let root_beta_one = 6.0 * (count.powi(2) - 5.0 * count + 2.0) / ((count + 7.0) * (count + 9.0))
        * (6.0 * (count + 3.0) * (count + 5.0) / (count * (count - 2.0) * (count - 3.0))).sqrt();
    let a = 6.0
        + 8.0 / root_beta_one * (2.0 / root_beta_one + (1.0 + 4.0 / root_beta_one.powi(2)).sqrt());
    let first = 1.0 - 2.0 / (9.0 * a);
    let denominator = 1.0 + standardized * (2.0 / (a - 4.0)).sqrt();
    let second = denominator.signum() * ((1.0 - 2.0 / a) / denominator.abs()).powf(1.0 / 3.0);
    let kurtosis_z = (first - second) / (2.0 / (9.0 * a)).sqrt();
    let statistic = skew_z.powi(2) + kurtosis_z.powi(2);
    (-statistic / 2.0).exp().clamp(0.0, 1.0)
}

fn polynomial(coefficients: &[f64], value: f64) -> f64 {
    coefficients
        .iter()
        .rev()
        .fold(0.0, |result, coefficient| result * value + coefficient)
}

fn normal_survival(value: f64) -> f64 {
    0.5 * (1.0 - approximate_erf(value / 2.0_f64.sqrt()))
}

fn approximate_erf(value: f64) -> f64 {
    let sign = value.signum();
    let x = value.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let polynomial =
        (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t;
    sign * (1.0 - polynomial * (-x * x).exp())
}

fn inverse_normal_cdf(probability: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    const LOW: f64 = 0.024_25;
    const HIGH: f64 = 1.0 - LOW;

    if probability < LOW {
        let q = (-2.0 * probability.ln()).sqrt();
        return (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    }
    if probability > HIGH {
        let q = (-2.0 * (1.0 - probability).ln()).sqrt();
        return -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    }
    let q = probability - 0.5;
    let r = q * q;
    (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
        / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
}

fn render(raster: Raster) -> Cursor<Vec<u8>> {
    Cursor::new(encode_png(&raster))
}

fn encode_png(raster: &Raster) -> Vec<u8> {
    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(raster.width as u32).to_be_bytes());
    header.extend_from_slice(&(raster.height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_png_chunk(&mut png, b"IHDR", &header);

    let row_bytes = raster.width * 4;
    let mut filtered = Vec::with_capacity((row_bytes + 1) * raster.height);
    for row in raster.pixels.chunks_exact(row_bytes) {
        filtered.push(0);
        filtered.extend_from_slice(row);
    }
    let mut zlib = Vec::with_capacity(filtered.len() + filtered.len() / 65_535 * 5 + 16);
    zlib.extend_from_slice(&[0x78, 0x01]);
    let blocks = filtered.chunks(65_535).collect::<Vec<_>>();
    for (index, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(index + 1 == blocks.len()));
        let length = u16::try_from(block.len()).expect("stored DEFLATE block length");
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&filtered).to_be_bytes());
    append_png_chunk(&mut png, b"IDAT", &zlib);
    append_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn append_png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut checksum = Vec::with_capacity(4 + data.len());
    checksum.extend_from_slice(kind);
    checksum.extend_from_slice(data);
    output.extend_from_slice(&crc32(&checksum).to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    const MODULUS: u32 = 65_521;
    let mut a = 1_u32;
    let mut b = 0_u32;
    for byte in bytes {
        a = (a + u32::from(*byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests;
