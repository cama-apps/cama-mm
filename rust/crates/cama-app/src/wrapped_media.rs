//! Native 800×600 PNG renderer for the live Cama Wrapped story.
//!
//! Semantic story data becomes fitted TrueType artwork, bounded GIF reveals,
//! and static charts. Discord avatar PNGs are decoded fail-soft.

use std::io::{Read, Write};
use std::sync::OnceLock;

use crate::drawing::{GambaPoint, GambaStats, draw_gamba_chart};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use fontdue::{Font, FontSettings};
use thiserror::Error;

use crate::font_assets::{DejaVuFace, load_dejavu_font};

mod artwork;
mod poster;

pub const WRAPPED_SLIDE_WIDTH: usize = 800;
pub const WRAPPED_SLIDE_HEIGHT: usize = 600;

const PANEL: Rgba = Rgba::rgb(30, 31, 42);
const WHITE: Rgba = Rgba::rgb(245, 245, 248);
const GRID: Rgba = Rgba::rgb(62, 64, 78);
const GREEN: Rgba = Rgba::rgb(46, 204, 113);
const RED: Rgba = Rgba::rgb(237, 66, 69);
const YELLOW: Rgba = Rgba::rgb(241, 196, 15);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WrappedAvatar {
    pub discord_id: i64,
    pub png: Vec<u8>,
}

/// Exact typed payload for the all-time Gamba page in `/wrapped`.
///
/// This is deliberately separate from the live `/profile` chart payload. The
/// wrapped command renders the 700×400 chart inside the 800×600 story canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct WrappedGambaData {
    pub degen_score: i32,
    pub degen_title: String,
    pub points: Vec<GambaPoint>,
    pub stats: GambaStats,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedSlideData {
    pub artwork: WrappedArtwork,
    pub chapter_index: usize,
    pub chapter_count: usize,
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
    pub gamba: Option<WrappedGambaData>,
}

impl WrappedSlideData {
    #[must_use]
    pub fn new(kind: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            artwork: WrappedArtwork::Rune,
            chapter_index: 0,
            chapter_count: 1,
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
            gamba: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WrappedArtwork {
    #[default]
    Rune,
    Ward,
    Courier,
}

#[derive(Debug, Error)]
pub enum WrappedMediaError {
    #[error("Wrapped slide text exceeds the supported image layout")]
    OversizedText,
    #[error("Wrapped animation could not be encoded: {0}")]
    Animation(#[from] gif::EncodingError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedRenderedMedia {
    pub filename: &'static str,
    pub bytes: Vec<u8>,
}

/// Animations reveal a single rendered image, with a complete final frame.
/// Detailed chapters and still mode always use PNG.
pub fn render_wrapped_media(
    slide: &WrappedSlideData,
    animated: bool,
) -> Result<WrappedRenderedMedia, WrappedMediaError> {
    validate_slide(slide)?;
    let canvas = poster::render(slide);
    if !animated
        || !matches!(
            slide.kind.as_str(),
            "story_intro" | "story_games" | "story_identity" | "story_moment"
        )
    {
        return Ok(WrappedRenderedMedia {
            filename: "wrapped_slide.png",
            bytes: encode_png(&canvas),
        });
    }
    let bytes = poster::animate(&canvas, slide)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Ok(WrappedRenderedMedia {
            filename: "wrapped_slide.png",
            bytes: encode_png(&canvas),
        });
    }
    Ok(WrappedRenderedMedia {
        filename: "wrapped_slide.gif",
        bytes,
    })
}

/// Render one story page as a seekable, Discord-ready RGBA PNG.
pub fn render_wrapped_slide(slide: &WrappedSlideData) -> Result<Vec<u8>, WrappedMediaError> {
    validate_slide(slide)?;
    Ok(encode_png(&poster::render(slide)))
}

fn validate_slide(slide: &WrappedSlideData) -> Result<(), WrappedMediaError> {
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
    Ok(())
}

fn draw_chart(canvas: &mut Canvas, slide: &WrappedSlideData, accent: Rgba) {
    poster::text(
        canvas,
        &slide.headline,
        (48, 100, 704, 64),
        34.0,
        accent,
        true,
    );
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
        poster::text(
            canvas,
            &format!("{maximum:.0}"),
            (14, top - 7, 50, 22),
            12.0,
            WHITE,
            false,
        );
        poster::text(
            canvas,
            &format!("{minimum:.0}"),
            (14, bottom - 12, 50, 22),
            12.0,
            WHITE,
            false,
        );
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
    for (index, (label, color)) in [("Glicko", accent), ("OpenSkill", YELLOW)]
        .into_iter()
        .enumerate()
    {
        let left = 78 + index as i32 * 170;
        canvas.line((left, 490), (left + 23, 490), color, 3);
        poster::text(canvas, label, (left + 32, 479, 130, 22), 15.0, WHITE, false);
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
    for line in lines.iter().take(maximum_lines) {
        poster::text(
            canvas,
            line,
            (48, y, 704, 26),
            (scale * 10) as f32,
            color,
            false,
        );
        y += 28;
    }
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
        if color.0[3] == u8::MAX {
            self.pixels[offset..offset + 4].copy_from_slice(&color.0);
            return;
        }
        let alpha = u16::from(color.0[3]);
        let inverse = u16::from(u8::MAX) - alpha;
        for channel in 0..3 {
            self.pixels[offset + channel] = u8::try_from(
                (u16::from(color.0[channel]) * alpha
                    + u16::from(self.pixels[offset + channel]) * inverse
                    + 127)
                    / 255,
            )
            .unwrap_or(u8::MAX);
        }
        self.pixels[offset + 3] = u8::MAX;
    }

    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        for y in top.min(bottom)..top.max(bottom) {
            for x in left.min(right)..left.max(right) {
                self.set(x, y, color);
            }
        }
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

    fn text_case_sensitive(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        for (index, character) in text.chars().enumerate() {
            let x = x + i32::try_from(index).unwrap_or_default() * 6 * scale;
            for (row, bits) in case_sensitive_glyph(character).into_iter().enumerate() {
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

    fn blit_opaque(&mut self, source: &DecodedPng, left: i32, top: i32) {
        for y in 0..source.height {
            for x in 0..source.width {
                let offset = (y * source.width + x) * 4;
                let color = Rgba(
                    source.pixels[offset..offset + 4]
                        .try_into()
                        .unwrap_or([0; 4]),
                );
                self.blend(
                    left + i32::try_from(x).unwrap_or_default(),
                    top + i32::try_from(y).unwrap_or_default(),
                    color,
                );
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

fn wrapped_font(bold: bool) -> Option<&'static Font> {
    static REGULAR: OnceLock<Option<Font>> = OnceLock::new();
    static BOLD: OnceLock<Option<Font>> = OnceLock::new();
    let slot = if bold { &BOLD } else { &REGULAR };
    slot.get_or_init(|| {
        load_dejavu_font(if bold {
            DejaVuFace::SansBold
        } else {
            DejaVuFace::Sans
        })
        .ok()
        .and_then(|bytes| Font::from_bytes(bytes, FontSettings::default()).ok())
    })
    .as_ref()
}

fn truetype_text_width(text: &str, pixel_size: f32, font: &Font) -> i32 {
    let mut width = 0.0_f32;
    let mut previous = None;
    for character in text.chars() {
        if let Some(previous) = previous {
            width += font
                .horizontal_kern(previous, character, pixel_size)
                .unwrap_or_default();
        }
        width += font.metrics(character, pixel_size).advance_width;
        previous = Some(character);
    }
    width.ceil() as i32
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
    if width.checked_mul(height)?.checked_mul(4)? > 8 * 1024 * 1024 {
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
        '·' => [0, 0, 0, 4, 0, 0, 0],
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

    fn wrapped_gamba_fixture_slide() -> WrappedSlideData {
        let mut slide = WrappedSlideData::new("chart_gamba", "Gamba (All-Time)");
        slide.username = "Wrapped Gambler".to_owned();
        slide.lines = vec!["+60 JC · 6 bets · Degen Score: 73".to_owned()];
        slide.gamba = Some(WrappedGambaData {
            degen_score: 73,
            degen_title: "House Favorite".to_owned(),
            points: vec![
                GambaPoint {
                    event_number: 1,
                    cumulative: -20,
                    info: crate::drawing::GambaInfo {
                        source: "bet".to_owned(),
                        outcome: Some("lost".to_owned()),
                        leverage: 1,
                        profit: -20,
                    },
                },
                GambaPoint {
                    event_number: 2,
                    cumulative: 60,
                    info: crate::drawing::GambaInfo {
                        source: "wheel".to_owned(),
                        outcome: Some("won".to_owned()),
                        leverage: 1,
                        profit: 80,
                    },
                },
            ],
            stats: GambaStats {
                total_bets: 1,
                win_rate: 1.0,
                net_pnl: 60,
                roi: 0.5,
            },
        });
        slide
    }

    #[test]
    fn all_production_slide_kinds_are_native_rgba_pngs() {
        for kind in [
            "story_intro",
            "story_identity",
            "story_moment",
            "story_finale",
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
    fn wrapped_gamba_renders_typed_chart_inside_separate_story_canvas() {
        let slide = wrapped_gamba_fixture_slide();
        let png = render_wrapped_slide(&slide).expect("wrapped Gamba PNG");
        let decoded = decode_png(&png).expect("wrapped Gamba native PNG");
        assert_eq!((decoded.width, decoded.height), (800, 600));
        assert_eq!(
            &decoded.pixels[(123 * decoded.width + 50) * 4..][..4],
            crate::drawing::DISCORD_BG.0,
            "the typed 700x400 chart is preserved inside the new story frame"
        );
        let mut untitled = slide;
        untitled.title.clear();
        let untitled =
            decode_png(&render_wrapped_slide(&untitled).expect("untitled Wrapped Gamba PNG"))
                .expect("untitled Wrapped Gamba native PNG");
        let title_start = 4 * decoded.width * 84;
        let title_band = 4 * decoded.width * 122;
        assert_ne!(
            &decoded.pixels[title_start..title_band],
            &untitled.pixels[title_start..title_band],
            "the story title is drawn above the chart"
        );
    }

    #[test]
    fn wrapped_gamba_footer_preserves_authored_case_and_middle_dot() {
        const FOOTER: &str = "+60 JC · 6 bets · Degen Score: 73";
        assert_eq!(glyph('·'), [0, 0, 0, 4, 0, 0, 0]);
        assert_ne!(case_sensitive_glyph('e'), glyph('e'));

        let slide = wrapped_gamba_fixture_slide();
        let rendered = render_wrapped_slide(&slide).expect("authored Wrapped Gamba copy");
        let decoded = decode_png(&rendered).expect("native Wrapped Gamba PNG");
        assert!(
            decoded.pixels[(550 * decoded.width * 4)..(585 * decoded.width * 4)]
                .chunks_exact(4)
                .any(|pixel| pixel[..3] == [255, 249, 239]),
            "the authored footer renders on the story canvas"
        );

        let mut upper_footer = slide.clone();
        upper_footer.lines[0] = FOOTER.to_ascii_uppercase();
        assert_ne!(
            rendered,
            render_wrapped_slide(&upper_footer).expect("uppercase sensitivity")
        );
        let mut fallback_dot = slide.clone();
        fallback_dot.lines[0] = FOOTER.replace('·', "?");
        assert_ne!(
            rendered,
            render_wrapped_slide(&fallback_dot).expect("middle-dot sensitivity")
        );
        let mut uppercase_title = slide;
        uppercase_title.title = uppercase_title.title.to_ascii_uppercase();
        assert_ne!(
            rendered,
            render_wrapped_slide(&uppercase_title).expect("title case sensitivity"),
            "poster typography preserves authored casing"
        );
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
            pixel(40, 201),
            [255, 249, 239, 255],
            "corner remains circularly clipped"
        );
        assert_ne!(pixel(69, 230), [255, 0, 0, 128]);
        assert_eq!(pixel(69, 230)[3], 255, "final Discord PNG stays opaque");
    }
}

#[cfg(test)]
mod remix_tests {
    use super::*;

    #[test]
    fn authored_art_decodes_with_alpha_without_relaxing_avatar_limits() {
        assert!(decode_png(&encode_png(&Canvas::new(1025, 1, WHITE))).is_none());
        for bytes in [
            include_bytes!("../../../../assets/wrapped/ward.png").as_slice(),
            include_bytes!("../../../../assets/wrapped/courier.png").as_slice(),
        ] {
            assert!(bytes.len() <= 450 * 1024);
            let art = decode_png(bytes).expect("bundled artwork");
            assert_eq!((art.width, art.height), (512, 512));
            assert!(art.pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
            assert!(art.pixels.chunks_exact(4).any(|pixel| pixel[3] == 255));
        }
        let bytes = include_bytes!("../../../../assets/wrapped/battlefield.png");
        assert!(bytes.len() <= 900 * 1024);
        let scene = decode_png(bytes).expect("bundled battlefield");
        assert_eq!((scene.width, scene.height), (800, 600));
    }

    #[test]
    fn illustrated_cover_and_character_chapters_have_real_art_detail() {
        let intro = decode_png(
            &render_wrapped_slide(&WrappedSlideData::new("story_intro", "Your year")).unwrap(),
        )
        .unwrap();
        let colors = intro
            .pixels
            .chunks_exact(4)
            .map(|p| [p[0], p[1], p[2]])
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            colors.len() > 5_000,
            "battlefield illustration is composited"
        );
        let mut moment = WrappedSlideData::new("story_moment", "Your vision budget");
        moment.artwork = WrappedArtwork::Ward;
        let ward = render_wrapped_slide(&moment).unwrap();
        moment.artwork = WrappedArtwork::Courier;
        assert_ne!(ward, render_wrapped_slide(&moment).unwrap());
    }

    #[test]
    fn personal_chapters_use_distinct_poster_palettes() {
        let intro =
            render_wrapped_slide(&WrappedSlideData::new("story_intro", "Your year")).unwrap();
        let moment =
            render_wrapped_slide(&WrappedSlideData::new("story_moment", "Your habits")).unwrap();
        let a = decode_png(&intro).unwrap();
        let b = decode_png(&moment).unwrap();
        assert_ne!(
            &a.pixels[(80 * 800 + 20) * 4..][..3],
            &b.pixels[(80 * 800 + 20) * 4..][..3]
        );
    }

    #[test]
    fn hero_and_lane_coverage_is_visible() {
        for kind in ["story_hero", "story_lanes"] {
            let mut slide = WrappedSlideData::new(kind, "Recorded picks");
            slide.stat_label = "Recorded in 7 of 20 games".into();
            let labeled = render_wrapped_slide(&slide).unwrap();
            slide.stat_label.clear();
            assert_ne!(labeled, render_wrapped_slide(&slide).unwrap());
        }
    }

    #[test]
    fn still_and_animation_reject_oversized_input_before_rendering() {
        let mut slide = WrappedSlideData::new("story_intro", "Wrapped");
        slide.username = "a".repeat(501);
        assert!(matches!(
            render_wrapped_slide(&slide),
            Err(WrappedMediaError::OversizedText)
        ));
        assert!(matches!(
            render_wrapped_media(&slide, true),
            Err(WrappedMediaError::OversizedText)
        ));
    }
}
