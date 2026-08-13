//! Native 800×600 PNG renderer for the live Cama Wrapped story.
//!
//! The production command supplies semantic slide data and this module draws
//! the complete server, awards, personal, records, pairwise, package, lane,
//! rating, and gambling pages without Python, Pillow, matplotlib, fonts, or
//! temporary files. Discord avatar PNGs are decoded fail-soft and composited
//! when available.

use std::io::{Read, Write};

use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use thiserror::Error;

pub const WRAPPED_SLIDE_WIDTH: usize = 800;
pub const WRAPPED_SLIDE_HEIGHT: usize = 600;

const BACKGROUND: Rgba = Rgba::rgb(18, 18, 24);
const PANEL: Rgba = Rgba::rgb(30, 31, 42);
const WHITE: Rgba = Rgba::rgb(245, 245, 248);
const GREY: Rgba = Rgba::rgb(170, 174, 190);
const GRID: Rgba = Rgba::rgb(62, 64, 78);
const GREEN: Rgba = Rgba::rgb(46, 204, 113);
const RED: Rgba = Rgba::rgb(237, 66, 69);
const YELLOW: Rgba = Rgba::rgb(241, 196, 15);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WrappedAvatar {
    pub discord_id: i64,
    pub png: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedSlideData {
    pub kind: String,
    pub title: String,
    pub username: String,
    pub year_label: String,
    pub headline: String,
    pub stat_value: String,
    pub stat_label: String,
    pub lines: Vec<String>,
    pub accent: [u8; 3],
    pub series: Vec<f64>,
    pub secondary_series: Vec<f64>,
    pub outcomes: Vec<Option<bool>>,
    pub avatars: Vec<WrappedAvatar>,
}

impl WrappedSlideData {
    #[must_use]
    pub fn new(kind: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            title: title.into(),
            username: String::new(),
            year_label: String::new(),
            headline: String::new(),
            stat_value: String::new(),
            stat_label: String::new(),
            lines: Vec::new(),
            accent: [88, 101, 242],
            series: Vec::new(),
            secondary_series: Vec::new(),
            outcomes: Vec::new(),
            avatars: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum WrappedMediaError {
    #[error("Wrapped slide text exceeds the supported image layout")]
    OversizedText,
}

/// Render one story page as a seekable, Discord-ready RGBA PNG.
pub fn render_wrapped_slide(slide: &WrappedSlideData) -> Result<Vec<u8>, WrappedMediaError> {
    if slide
        .lines
        .iter()
        .chain([
            &slide.title,
            &slide.username,
            &slide.year_label,
            &slide.headline,
            &slide.stat_value,
            &slide.stat_label,
        ])
        .any(|line| line.chars().count() > 500)
    {
        return Err(WrappedMediaError::OversizedText);
    }
    let accent = Rgba::rgb(slide.accent[0], slide.accent[1], slide.accent[2]);
    let mut canvas = Canvas::new(WRAPPED_SLIDE_WIDTH, WRAPPED_SLIDE_HEIGHT, BACKGROUND);
    canvas.fill_rect(0, 0, WRAPPED_SLIDE_WIDTH as i32, 10, accent);
    canvas.text_centered(&slide.title, 35, WHITE, 4);
    canvas.text_centered(&slide.year_label, 78, GREY, 2);
    match slide.kind.as_str() {
        "story_games" => draw_reveal(&mut canvas, slide, accent),
        "server_summary" => draw_server_summary(&mut canvas, slide, accent),
        "awards" => draw_cards(&mut canvas, slide, accent, true),
        kind if kind.starts_with("records_") => draw_cards(&mut canvas, slide, accent, false),
        "story_hero" => draw_hero(&mut canvas, slide, accent),
        "story_lanes" => draw_lane_bars(&mut canvas, slide, accent),
        "story_teammates" | "story_rivals" => draw_pairwise(&mut canvas, slide, accent),
        "story_packages" => draw_package(&mut canvas, slide, accent),
        "chart_rating" | "chart_gamba" => draw_chart(&mut canvas, slide, accent),
        _ => draw_stats_grid(&mut canvas, slide, accent),
    }
    if !slide.username.is_empty() {
        canvas.text(28, 568, &slide.username, GREY, 2);
    }
    canvas.text_right("CAMA WRAPPED", 772, 568, GREY, 2);
    Ok(encode_png(&canvas))
}

fn draw_reveal(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba) {
    canvas.text_centered(&slide.headline, 135, accent, 3);
    canvas.text_centered(&slide.stat_value, 215, WHITE, 7);
    canvas.text_centered(&slide.stat_label, 285, accent, 3);
    draw_wrapped_lines(canvas, &slide.lines, 350, 4, 2, WHITE);
}

fn draw_server_summary(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba) {
    canvas.fill_rect(45, 125, 755, 530, PANEL);
    canvas.text_centered(&slide.headline, 150, accent, 3);
    draw_metric_grid(canvas, &slide.lines, 205, accent);
}

fn draw_stats_grid(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba) {
    canvas.text_centered(&slide.headline, 125, accent, 3);
    draw_metric_grid(canvas, &slide.lines, 175, accent);
}

fn draw_metric_grid(canvas: &mut Canvas, lines: &[String], top: i32, accent: Rgba) {
    for (index, line) in lines.iter().take(8).enumerate() {
        let row = i32::try_from(index / 2).unwrap_or_default();
        let column = i32::try_from(index % 2).unwrap_or_default();
        let left = 60 + column * 350;
        let y = top + row * 82;
        canvas.fill_rect(left, y, left + 325, y + 65, PANEL);
        canvas.fill_rect(left, y, left + 6, y + 65, accent);
        canvas.text(left + 20, y + 22, &truncate(line, 43), WHITE, 2);
    }
}

fn draw_cards(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba, awards: bool) {
    canvas.text_centered(&slide.headline, 120, accent, 3);
    for (index, line) in slide.lines.iter().take(6).enumerate() {
        let row = i32::try_from(index / 2).unwrap_or_default();
        let column = i32::try_from(index % 2).unwrap_or_default();
        let left = 48 + column * 365;
        let top = 165 + row * 120;
        canvas.fill_rect(left, top, left + 340, top + 98, PANEL);
        canvas.outline_rect(left, top, left + 340, top + 98, accent, 2);
        let color = if awards && index % 2 == 1 {
            WHITE
        } else {
            accent
        };
        for (line_index, wrapped) in wrap_text(line, 26, 3).iter().enumerate() {
            canvas.text(
                left + 16,
                top + 16 + i32::try_from(line_index).unwrap_or_default() * 24,
                wrapped,
                color,
                2,
            );
        }
    }
}

fn draw_hero(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba) {
    canvas.fill_rect(55, 125, 745, 300, PANEL);
    canvas.text_centered(&slide.headline, 155, accent, 4);
    canvas.text_centered(&slide.stat_value, 220, WHITE, 5);
    draw_wrapped_lines(canvas, &slide.lines, 335, 7, 2, WHITE);
}

fn draw_lane_bars(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba) {
    canvas.text_centered(&slide.headline, 120, accent, 3);
    let values = if slide.series.is_empty() {
        vec![0.0; slide.lines.len()]
    } else {
        slide.series.clone()
    };
    let max = values.iter().copied().fold(1.0_f64, f64::max);
    for (index, (label, value)) in slide.lines.iter().zip(values).take(5).enumerate() {
        let y = 180 + i32::try_from(index).unwrap_or_default() * 72;
        canvas.text(75, y, &truncate(label, 24), WHITE, 2);
        canvas.fill_rect(285, y - 4, 710, y + 25, PANEL);
        let width = (420.0 * (value / max).clamp(0.0, 1.0)).round() as i32;
        canvas.fill_rect(285, y - 4, 285 + width, y + 25, accent);
    }
}

fn draw_pairwise(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba) {
    canvas.text_centered(&slide.headline, 118, accent, 3);
    for (index, line) in slide.lines.iter().take(6).enumerate() {
        let top = 165 + i32::try_from(index).unwrap_or_default() * 61;
        if let Some(avatar) = slide
            .avatars
            .get(index)
            .and_then(|avatar| decode_png(&avatar.png))
        {
            canvas.blit_scaled(&avatar, 60, top - 8, 50, 50);
        } else {
            canvas.circle((85, top + 17), 24, accent);
        }
        canvas.fill_rect(125, top - 8, 735, top + 43, PANEL);
        for (line_index, wrapped) in wrap_text(line, 72, 3).iter().enumerate() {
            canvas.text(
                145,
                top + i32::try_from(line_index).unwrap_or_default() * 13,
                wrapped,
                WHITE,
                1,
            );
        }
    }
}

fn draw_package(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba) {
    canvas.text_centered(&slide.headline, 130, accent, 4);
    canvas.text_centered(&slide.stat_value, 215, WHITE, 6);
    canvas.text_centered(&slide.stat_label, 275, accent, 2);
    draw_wrapped_lines(canvas, &slide.lines, 340, 6, 2, WHITE);
}

fn draw_chart(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba) {
    canvas.text_centered(&slide.headline, 115, accent, 3);
    let (left, top, right, bottom) = (70, 175, 745, 475);
    canvas.fill_rect(left, top, right, bottom, PANEL);
    for line in 0..=5 {
        let y = top + (bottom - top) * line / 5;
        canvas.line((left, y), (right, y), GRID, 1);
    }
    let combined = slide
        .series
        .iter()
        .chain(&slide.secondary_series)
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if slide.series.len().max(slide.secondary_series.len()) >= 2 && !combined.is_empty() {
        let minimum = combined.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = combined.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let span = (maximum - minimum).max(1.0);
        let point = |index: usize, value: f64, count: usize| {
            let x = left
                + (right - left) * i32::try_from(index).unwrap_or_default()
                    / i32::try_from(count.saturating_sub(1)).unwrap_or(1).max(1);
            let y = bottom - ((value - minimum) / span * f64::from(bottom - top)) as i32;
            (x, y)
        };
        for (series, color) in [
            (slide.series.as_slice(), accent),
            (slide.secondary_series.as_slice(), YELLOW),
        ] {
            for index in 0..series.len().saturating_sub(1) {
                if series[index].is_finite() && series[index + 1].is_finite() {
                    canvas.line(
                        point(index, series[index], series.len()),
                        point(index + 1, series[index + 1], series.len()),
                        color,
                        3,
                    );
                }
            }
        }
        let count = slide.series.len().max(slide.secondary_series.len());
        for index in 0..count {
            let value = slide
                .series
                .get(index)
                .copied()
                .filter(|value| value.is_finite())
                .or_else(|| {
                    slide
                        .secondary_series
                        .get(index)
                        .copied()
                        .filter(|value| value.is_finite())
                });
            if let Some(value) = value {
                let color = match slide.outcomes.get(index).copied().flatten() {
                    Some(true) => GREEN,
                    Some(false) => RED,
                    None => WHITE,
                };
                canvas.circle(point(index, value, count), 4, color);
            }
        }
    }
    draw_wrapped_lines(canvas, &slide.lines, 500, 2, 2, WHITE);
}

fn draw_wrapped_lines(
    canvas: &mut Canvas,
    lines: &[String],
    top: i32,
    maximum_lines: usize,
    scale: i32,
    color: Rgba,
) {
    let mut y = top;
    for line in lines
        .iter()
        .flat_map(|line| wrap_text(line, 75, 2))
        .take(maximum_lines)
    {
        canvas.text_centered(&line, y, color, scale);
        y += 28;
    }
}

fn truncate(text: &str, maximum: usize) -> String {
    if text.chars().count() <= maximum {
        return text.to_owned();
    }
    text.chars()
        .take(maximum.saturating_sub(2))
        .collect::<String>()
        + ".."
}

fn wrap_text(text: &str, maximum: usize, lines: usize) -> Vec<String> {
    let mut output = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.len() + 1 + word.len() > maximum {
            output.push(std::mem::take(&mut current));
            if output.len() == lines {
                break;
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() && output.len() < lines {
        output.push(current);
    }
    output
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rgba([u8; 4]);

impl Rgba {
    const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self([red, green, blue, 255])
    }
}

#[derive(Clone, Debug)]
struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: usize, height: usize, color: Rgba) -> Self {
        let mut pixels = vec![0; width * height * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color.0);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    fn set(&mut self, x: i32, y: i32, color: Rgba) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.width + x) * 4;
        self.pixels[offset..offset + 4].copy_from_slice(&color.0);
    }

    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        for y in top.min(bottom)..top.max(bottom) {
            for x in left.min(right)..left.max(right) {
                self.set(x, y, color);
            }
        }
    }

    fn outline_rect(&mut self, l: i32, t: i32, r: i32, b: i32, color: Rgba, width: i32) {
        self.fill_rect(l, t, r, t + width, color);
        self.fill_rect(l, b - width, r, b, color);
        self.fill_rect(l, t, l + width, b, color);
        self.fill_rect(r - width, t, r, b, color);
    }

    fn line(&mut self, start: (i32, i32), end: (i32, i32), color: Rgba, width: i32) {
        let (mut x0, mut y0) = start;
        let (x1, y1) = end;
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        loop {
            let radius = width.saturating_sub(1) / 2;
            for y in -radius..=radius {
                for x in -radius..=radius {
                    self.set(x0 + x, y0 + y, color);
                }
            }
            if x0 == x1 && y0 == y1 {
                break;
            }
            let twice = error * 2;
            if twice >= dy {
                error += dy;
                x0 += sx;
            }
            if twice <= dx {
                error += dx;
                y0 += sy;
            }
        }
    }

    fn circle(&mut self, center: (i32, i32), radius: i32, color: Rgba) {
        for y in -radius..=radius {
            for x in -radius..=radius {
                if x * x + y * y <= radius * radius {
                    self.set(center.0 + x, center.1 + y, color);
                }
            }
        }
    }

    fn text(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        for (index, character) in text.chars().enumerate() {
            let x = x + i32::try_from(index).unwrap_or_default() * 6 * scale;
            for (row, bits) in glyph(character).into_iter().enumerate() {
                for column in 0..5_u8 {
                    if bits & (1 << (4 - column)) != 0 {
                        let left = x + i32::from(column) * scale;
                        let top = y + i32::try_from(row).unwrap_or_default() * scale;
                        self.fill_rect(left, top, left + scale, top + scale, color);
                    }
                }
            }
        }
    }

    fn text_centered(&mut self, text: &str, y: i32, color: Rgba, scale: i32) {
        let width = i32::try_from(text.chars().count()).unwrap_or_default() * 6 * scale;
        self.text((self.width as i32 - width) / 2, y, text, color, scale);
    }

    fn text_right(&mut self, text: &str, right: i32, y: i32, color: Rgba, scale: i32) {
        let width = i32::try_from(text.chars().count()).unwrap_or_default() * 6 * scale;
        self.text(right - width, y, text, color, scale);
    }

    fn blit_scaled(
        &mut self,
        source: &DecodedPng,
        left: i32,
        top: i32,
        width: usize,
        height: usize,
    ) {
        for y in 0..height {
            for x in 0..width {
                let centered_x = x as f64 + 0.5 - width as f64 / 2.0;
                let centered_y = y as f64 + 0.5 - height as f64 / 2.0;
                let radius = width.min(height) as f64 / 2.0;
                if centered_x.mul_add(centered_x, centered_y * centered_y) > radius * radius {
                    continue;
                }
                let source_x = x * source.width / width;
                let source_y = y * source.height / height;
                let offset = (source_y * source.width + source_x) * 4;
                let source_pixel: [u8; 4] = source.pixels[offset..offset + 4]
                    .try_into()
                    .unwrap_or([0, 0, 0, 0]);
                let destination_x = left + i32::try_from(x).unwrap_or_default();
                let destination_y = top + i32::try_from(y).unwrap_or_default();
                self.blend(destination_x, destination_y, Rgba(source_pixel));
            }
        }
    }

    fn blend(&mut self, x: i32, y: i32, source: Rgba) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.width + x) * 4;
        let alpha = u16::from(source.0[3]);
        let inverse = 255_u16 - alpha;
        for (destination, source) in self.pixels[offset..offset + 3]
            .iter_mut()
            .zip(source.0[..3].iter())
        {
            *destination =
                ((u16::from(*source) * alpha + u16::from(*destination) * inverse) / 255) as u8;
        }
        self.pixels[offset + 3] = 255;
    }
}

#[derive(Clone, Debug)]
struct DecodedPng {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

fn decode_png(bytes: &[u8]) -> Option<DecodedPng> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut offset = 8_usize;
    let (mut width, mut height, mut color_type) = (0_usize, 0_usize, 0_u8);
    let mut idat = Vec::new();
    while offset.checked_add(12)? <= bytes.len() {
        let length = usize::try_from(u32::from_be_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
        .ok()?;
        let kind = bytes.get(offset + 4..offset + 8)?;
        let data = bytes.get(offset + 8..offset.checked_add(8 + length)?)?;
        offset = offset.checked_add(12 + length)?;
        match kind {
            b"IHDR" => {
                width =
                    usize::try_from(u32::from_be_bytes(data.get(0..4)?.try_into().ok()?)).ok()?;
                height =
                    usize::try_from(u32::from_be_bytes(data.get(4..8)?.try_into().ok()?)).ok()?;
                if data.get(8) != Some(&8) || data.get(10..13) != Some(&[0, 0, 0]) {
                    return None;
                }
                color_type = *data.get(9)?;
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
    }
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return None;
    }
    let channels = match color_type {
        2 => 3,
        6 => 4,
        _ => return None,
    };
    let row_bytes = width.checked_mul(channels)?;
    let expected = height.checked_mul(row_bytes.checked_add(1)?)?;
    if expected > 8 * 1024 * 1024 {
        return None;
    }
    let mut filtered = Vec::new();
    ZlibDecoder::new(idat.as_slice())
        .take(expected as u64 + 1)
        .read_to_end(&mut filtered)
        .ok()?;
    if filtered.len() != expected {
        return None;
    }
    let mut raw = vec![0_u8; width * height * channels];
    for row in 0..height {
        let source = &filtered[row * (row_bytes + 1)..(row + 1) * (row_bytes + 1)];
        let filter = source[0];
        for index in 0..row_bytes {
            let x = source[index + 1];
            let left = if index >= channels {
                raw[row * row_bytes + index - channels]
            } else {
                0
            };
            let up = if row > 0 {
                raw[(row - 1) * row_bytes + index]
            } else {
                0
            };
            let upper_left = if row > 0 && index >= channels {
                raw[(row - 1) * row_bytes + index - channels]
            } else {
                0
            };
            raw[row * row_bytes + index] = match filter {
                0 => x,
                1 => x.wrapping_add(left),
                2 => x.wrapping_add(up),
                3 => x.wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                4 => x.wrapping_add(paeth(left, up, upper_left)),
                _ => return None,
            };
        }
    }
    let mut pixels = Vec::with_capacity(width * height * 4);
    for pixel in raw.chunks_exact(channels) {
        pixels.extend_from_slice(&[
            pixel[0],
            pixel[1],
            pixel[2],
            if channels == 4 { pixel[3] } else { 255 },
        ]);
    }
    Some(DecodedPng {
        width,
        height,
        pixels,
    })
}

fn paeth(left: u8, up: u8, upper_left: u8) -> u8 {
    let prediction = i32::from(left) + i32::from(up) - i32::from(upper_left);
    let left_distance = (prediction - i32::from(left)).abs();
    let up_distance = (prediction - i32::from(up)).abs();
    let diagonal_distance = (prediction - i32::from(upper_left)).abs();
    if left_distance <= up_distance && left_distance <= diagonal_distance {
        left
    } else if up_distance <= diagonal_distance {
        up
    } else {
        upper_left
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
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '%' => [25, 25, 2, 4, 8, 19, 19],
        '/' => [1, 2, 2, 4, 8, 8, 16],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        ' ' => [0; 7],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

fn encode_png(canvas: &Canvas) -> Vec<u8> {
    let mut filtered = Vec::with_capacity((canvas.width * 4 + 1) * canvas.height);
    for row in canvas.pixels.chunks_exact(canvas.width * 4) {
        filtered.push(0);
        filtered.extend_from_slice(row);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&filtered)
        .expect("writing an in-memory Wrapped PNG cannot fail");
    let zlib = encoder
        .finish()
        .expect("finishing an in-memory Wrapped PNG cannot fail");
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&(canvas.width as u32).to_be_bytes());
    header.extend_from_slice(&(canvas.height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_chunk(&mut png, b"IHDR", &header);
    append_chunk(&mut png, b"IDAT", &zlib);
    append_chunk(&mut png, b"IEND", &[]);
    png
}

fn append_chunk(png: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    png.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(payload);
    let mut data = kind.to_vec();
    data.extend_from_slice(payload);
    png.extend_from_slice(&crc32(&data).to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320_u32 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_production_slide_kinds_are_native_rgba_pngs() {
        for kind in [
            "server_summary",
            "awards",
            "story_games",
            "story_summary",
            "records_combat",
            "story_hero",
            "story_lanes",
            "story_teammates",
            "story_rivals",
            "story_packages",
            "chart_rating",
            "chart_gamba",
        ] {
            let mut slide = WrappedSlideData::new(kind, "Wrapped");
            slide.year_label = "Cama Wrapped 2026".to_owned();
            slide.headline = "Production semantic page".to_owned();
            slide.stat_value = "42".to_owned();
            slide.stat_label = "Games".to_owned();
            slide.lines = vec![
                "Alice - 10 games - 60%".to_owned(),
                "Bob - 8 games".to_owned(),
            ];
            slide.series = vec![1.0, 4.0, 2.0, 8.0];
            let png = render_wrapped_slide(&slide).unwrap();
            let decoded = decode_png(&png).expect("native PNG");
            assert_eq!((decoded.width, decoded.height), (800, 600));
            assert!(png.len() > 1_000);
        }
    }

    #[test]
    fn malformed_avatar_is_fail_soft() {
        let mut slide = WrappedSlideData::new("story_teammates", "Teammates");
        slide.lines.push("Alice - 3 games".to_owned());
        slide.avatars.push(WrappedAvatar {
            discord_id: 1,
            png: b"not png".to_vec(),
        });
        assert!(render_wrapped_slide(&slide).is_ok());
    }

    #[test]
    fn transparent_avatar_is_alpha_blended_and_clipped_to_a_circle() {
        let mut avatar_canvas = Canvas::new(2, 2, Rgba([255, 0, 0, 128]));
        avatar_canvas.set(1, 1, Rgba([0, 255, 0, 255]));
        let avatar = encode_png(&avatar_canvas);
        let mut slide = WrappedSlideData::new("story_teammates", "Teammates");
        slide.lines.push("Alice - 3 games".to_owned());
        slide.avatars.push(WrappedAvatar {
            discord_id: 1,
            png: avatar,
        });
        let png = render_wrapped_slide(&slide).unwrap();
        let decoded = decode_png(&png).unwrap();
        let pixel = |x: usize, y: usize| -> [u8; 4] {
            decoded.pixels[(y * decoded.width + x) * 4..][..4]
                .try_into()
                .unwrap()
        };
        assert_eq!(
            pixel(60, 157),
            BACKGROUND.0,
            "corner remains circularly clipped"
        );
        assert_ne!(pixel(85, 182), [255, 0, 0, 128]);
        assert_eq!(pixel(85, 182)[3], 255, "final Discord PNG stays opaque");
    }
}
