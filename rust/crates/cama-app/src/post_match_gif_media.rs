//! Native renderer for JOPA-T post-match settlement animations.
//!
//! This is the Rust counterpart of `utils.neon_drawing.create_post_match_gif`.
//! It uses a bundled 5x7 terminal font and one fixed indexed palette, so the
//! production runtime neither shells out to Python nor depends on host fonts.

use std::borrow::Cow;

use gif::{Encoder, Frame, Repeat};
use thiserror::Error;

use crate::neon_degen::GifAsset;

pub const POST_MATCH_GIF_WIDTH: u16 = 400;
pub const POST_MATCH_GIF_HEIGHT: u16 = 300;
pub const POST_MATCH_GIF_FRAME_COUNT: usize = 18;
pub const POST_MATCH_GIF_FINAL_HOLD_MS: u32 = 60_000;
pub const POST_MATCH_GIF_THEMES: [&str; 5] = [
    "divine_rapier_position",
    "buyback_denied",
    "ancient_liquidated",
    "beyond_godlike",
    "odds_anomaly",
];

const PIXEL_COUNT: usize = POST_MATCH_GIF_WIDTH as usize * POST_MATCH_GIF_HEIGHT as usize;
const CRT_BLACK: u8 = 0;
const NEON_GREEN: u8 = 1;
const NEON_CYAN: u8 = 2;
const NEON_PINK: u8 = 3;
const NEON_RED: u8 = 4;
const NEON_YELLOW: u8 = 5;
const NEON_PURPLE: u8 = 6;
const DIM_GREEN: u8 = 7;
const DIM_CYAN: u8 = 8;
const SCANLINE_BLACK: u8 = 9;
const SCANLINE_GREEN: u8 = 10;
const SCANLINE_CYAN: u8 = 11;
const SCANLINE_PINK: u8 = 12;
const SCANLINE_RED: u8 = 13;
const SCANLINE_YELLOW: u8 = 14;
const SCANLINE_PURPLE: u8 = 15;

const PALETTE: &[u8] = &[
    10, 10, 15, // CRT black
    0, 255, 65, // neon green
    0, 255, 255, // neon cyan
    255, 0, 128, // neon pink
    255, 30, 30, // neon red
    255, 220, 0, // neon yellow
    180, 80, 255, // neon purple
    0, 120, 30, // dim green
    0, 100, 100, // dim cyan
    5, 5, 8, // dark scanline
    0, 195, 50, // green scanline
    0, 195, 195, // cyan scanline
    195, 0, 98, // pink scanline
    195, 23, 23, // red scanline
    195, 169, 0, // yellow scanline
    138, 61, 195, // purple scanline
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Theme {
    DivineRapierPosition,
    BuybackDenied,
    AncientLiquidated,
    BeyondGodlike,
    OddsAnomaly,
}

impl Theme {
    fn parse(value: &str) -> Result<Self, PostMatchGifRenderError> {
        match value {
            "divine_rapier_position" => Ok(Self::DivineRapierPosition),
            "buyback_denied" => Ok(Self::BuybackDenied),
            "ancient_liquidated" => Ok(Self::AncientLiquidated),
            "beyond_godlike" => Ok(Self::BeyondGodlike),
            "odds_anomaly" => Ok(Self::OddsAnomaly),
            _ => Err(PostMatchGifRenderError::UnsupportedTheme(value.to_owned())),
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::DivineRapierPosition => "divine_rapier_position",
            Self::BuybackDenied => "buyback_denied",
            Self::AncientLiquidated => "ancient_liquidated",
            Self::BeyondGodlike => "beyond_godlike",
            Self::OddsAnomaly => "odds_anomaly",
        }
    }

    const fn banner(self) -> &'static str {
        match self {
            Self::DivineRapierPosition => "RAPIER POSITION",
            Self::BuybackDenied => "BUYBACK DENIED",
            Self::AncientLiquidated => "ANCIENT LIQUIDATED",
            Self::BeyondGodlike => "BEYOND GODLIKE",
            Self::OddsAnomaly => "ODDS ANOMALY",
        }
    }

    const fn colors(self) -> (u8, u8) {
        match self {
            Self::DivineRapierPosition => (NEON_YELLOW, NEON_GREEN),
            Self::BuybackDenied => (NEON_RED, NEON_PINK),
            Self::AncientLiquidated => (NEON_CYAN, NEON_GREEN),
            Self::BeyondGodlike => (NEON_PURPLE, NEON_YELLOW),
            Self::OddsAnomaly => (NEON_CYAN, NEON_PINK),
        }
    }

    fn value_line(self, value: i64) -> String {
        let value = grouped_decimal(value);
        match self {
            Self::DivineRapierPosition => format!("+{value} RATING"),
            Self::BuybackDenied => format!("{value} JC LOST"),
            Self::AncientLiquidated => format!("{value} JC PAID"),
            Self::BeyondGodlike => format!("{value} MATCH STREAK"),
            Self::OddsAnomaly => format!("{value}% IMPLIED ODDS"),
        }
    }
}

#[derive(Debug, Error)]
pub enum PostMatchGifRenderError {
    #[error("Unsupported post-match GIF theme: {0:?}")]
    UnsupportedTheme(String),
    #[error("GIF encoding failed: {0}")]
    Gif(#[from] gif::EncodingError),
}

/// Render one of the five production post-match themes.
pub fn render_post_match_gif(
    name: &str,
    value: i64,
    theme: &str,
) -> Result<GifAsset, PostMatchGifRenderError> {
    let theme = Theme::parse(theme)?;
    let display_name = normalized_display_name(name);
    let durations = frame_durations_ms();
    let entropy = scene_entropy(&display_name, value, theme);
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(
            &mut bytes,
            POST_MATCH_GIF_WIDTH,
            POST_MATCH_GIF_HEIGHT,
            PALETTE,
        )?;
        encoder.set_repeat(Repeat::Finite(1))?;
        for (frame_index, duration_ms) in durations.iter().copied().enumerate() {
            let mut canvas = render_frame(theme, &display_name, value, frame_index);
            apply_scanlines(&mut canvas.pixels);
            if matches!(frame_index, 0 | 9) {
                apply_glitch(&mut canvas.pixels, entropy ^ frame_index as u64);
            }
            let mut frame = Frame {
                width: POST_MATCH_GIF_WIDTH,
                height: POST_MATCH_GIF_HEIGHT,
                delay: u16::try_from(duration_ms / 10)
                    .expect("post-match GIF duration fits the centisecond field"),
                buffer: Cow::Owned(canvas.pixels),
                ..Frame::default()
            };
            frame.dispose = gif::DisposalMethod::Keep;
            encoder.write_frame(&frame)?;
        }
    }
    Ok(GifAsset {
        kind: theme.key(),
        bytes,
        frame_durations_ms: durations,
        shared_palette: true,
    })
}

#[must_use]
pub fn frame_durations_ms() -> Vec<u32> {
    let mut durations = vec![80; POST_MATCH_GIF_FRAME_COUNT - 1];
    durations.push(POST_MATCH_GIF_FINAL_HOLD_MS);
    durations
}

fn normalized_display_name(name: &str) -> String {
    let normalized = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = if normalized.is_empty() {
        "UNKNOWN CLIENT"
    } else {
        &normalized
    };
    normalized.chars().take(22).collect()
}

fn render_frame(theme: Theme, name: &str, value: i64, frame_index: usize) -> Canvas {
    let mut canvas = Canvas::new();
    let (primary, secondary) = theme.colors();
    let progress = i32::try_from(frame_index + 1).unwrap_or(POST_MATCH_GIF_FRAME_COUNT as i32);
    canvas.outline_rect(12, 12, 387, 287, primary, 2);
    canvas.text_centered("JOPA-T/v3.7 MATCH LEDGER", 24, secondary, 1, false);
    canvas.text_centered(theme.banner(), 50, primary, 2, true);
    canvas.text_centered(name, 88, secondary, 2, true);
    canvas.text_centered(&theme.value_line(value), 112, primary, 2, true);
    canvas.line(24, 145, 376, 145, DIM_GREEN, 1);
    canvas.line(
        24,
        149,
        24 + progress * 352 / POST_MATCH_GIF_FRAME_COUNT as i32,
        149,
        DIM_CYAN,
        1,
    );

    match theme {
        Theme::DivineRapierPosition => {
            let size = 20 + progress * 48 / POST_MATCH_GIF_FRAME_COUNT as i32;
            let (x, y) = (200, 210);
            canvas.line(x, y - size, x + size, y, primary, 3);
            canvas.line(x + size, y, x, y + size, primary, 3);
            canvas.line(x, y + size, x - size, y, primary, 3);
            canvas.line(x - size, y, x, y - size, primary, 3);
            for offset in 0..3 {
                let y = 250 - offset * 16;
                canvas.line(
                    80 + frame_index as i32 * 8,
                    y,
                    320 - frame_index as i32 * 8,
                    y,
                    secondary,
                    1,
                );
            }
        }
        Theme::BuybackDenied => {
            for row in 0..6 {
                let y = 160 + row * 18;
                let width =
                    (50 + progress * 250 / POST_MATCH_GIF_FRAME_COUNT as i32 - row * 18).max(12);
                canvas.fill_rect(200 - width / 2, y, 200 + width / 2, y + 8, primary);
            }
            canvas.text_centered("LIQUIDATION LOCKED", 268, secondary, 1, false);
        }
        Theme::AncientLiquidated => {
            for column in 0..8 {
                let x = 54 + column * 38;
                let height = (column + 2) * 8 * progress / POST_MATCH_GIF_FRAME_COUNT as i32;
                canvas.outline_rect(x, 255 - height, x + 22, 255, primary, 2);
                if frame_index >= usize::try_from(column * 2).unwrap_or_default() {
                    canvas.fill_rect(x + 4, 251 - height, x + 18, 251, secondary);
                }
            }
            canvas.text_centered("SETTLEMENT COMPLETE", 268, primary, 1, false);
        }
        Theme::BeyondGodlike => {
            for streak in 0..7 {
                let x = (frame_index as i32 * 24 + streak * 59).rem_euclid(480) - 40;
                let y = 170 + (streak * 17) % 90;
                canvas.line(
                    x,
                    y,
                    x + 64,
                    y - 36,
                    if streak % 2 == 0 { secondary } else { primary },
                    3,
                );
            }
            canvas.text_centered("STREAK ASCENDING", 268, secondary, 1, false);
        }
        Theme::OddsAnomaly => {
            let mut previous = None;
            for x in (36..364).step_by(8) {
                let phase = x + frame_index as i32 * 18;
                let y = 210 + triangle_wave(phase, 80, 28);
                if let Some((previous_x, previous_y)) = previous {
                    canvas.line(previous_x, previous_y, x, y, primary, 3);
                }
                previous = Some((x, y));
            }
            for x in (36..364).step_by(16) {
                let phase = x + frame_index as i32 * 18 + 20;
                let y = 210 + triangle_wave(phase, 88, 20);
                canvas.fill_rect(x - 2, y - 2, x + 2, y + 2, secondary);
            }
            canvas.text_centered("MARKET SIGNAL UNSTABLE", 268, secondary, 1, false);
        }
    }
    canvas
}

fn triangle_wave(value: i32, period: i32, amplitude: i32) -> i32 {
    let phase = value.rem_euclid(period);
    let half = period / 2;
    let centered = if phase < half { phase } else { period - phase };
    centered * amplitude * 2 / half - amplitude
}

fn apply_scanlines(pixels: &mut [u8]) {
    for y in (0..POST_MATCH_GIF_HEIGHT as usize).step_by(2) {
        let row =
            &mut pixels[y * POST_MATCH_GIF_WIDTH as usize..(y + 1) * POST_MATCH_GIF_WIDTH as usize];
        for pixel in row {
            *pixel = match *pixel {
                CRT_BLACK => SCANLINE_BLACK,
                NEON_GREEN => SCANLINE_GREEN,
                NEON_CYAN => SCANLINE_CYAN,
                NEON_PINK => SCANLINE_PINK,
                NEON_RED => SCANLINE_RED,
                NEON_YELLOW => SCANLINE_YELLOW,
                NEON_PURPLE => SCANLINE_PURPLE,
                other => other,
            };
        }
    }
}

fn apply_glitch(pixels: &mut [u8], entropy: u64) {
    let original = pixels.to_vec();
    let mut random = TinyRandom::new(entropy);
    for _ in 0..6 {
        let y = random.bounded(u32::from(POST_MATCH_GIF_HEIGHT) - 10) as usize;
        let height = 2 + random.bounded(7) as usize;
        let offset = random.bounded(31) as isize - 15;
        for row in y..(y + height).min(POST_MATCH_GIF_HEIGHT as usize) {
            for x in 0..POST_MATCH_GIF_WIDTH as usize {
                let source_x =
                    (x as isize - offset).rem_euclid(POST_MATCH_GIF_WIDTH as isize) as usize;
                pixels[row * POST_MATCH_GIF_WIDTH as usize + x] =
                    original[row * POST_MATCH_GIF_WIDTH as usize + source_x];
            }
        }
    }
}

fn grouped_decimal(value: i64) -> String {
    let negative = value.is_negative();
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(character);
    }
    if negative {
        format!("-{grouped}")
    } else {
        grouped
    }
}

fn scene_entropy(name: &str, value: i64, theme: Theme) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in name.bytes().chain(value.to_le_bytes()).chain([theme as u8]) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

struct TinyRandom(u64);

impl TinyRandom {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as u32
    }

    fn bounded(&mut self, bound: u32) -> u32 {
        if bound == 0 { 0 } else { self.next() % bound }
    }
}

struct Canvas {
    pixels: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        Self {
            pixels: vec![CRT_BLACK; PIXEL_COUNT],
        }
    }

    fn set(&mut self, x: i32, y: i32, color: u8) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x < POST_MATCH_GIF_WIDTH as usize && y < POST_MATCH_GIF_HEIGHT as usize {
            self.pixels[y * POST_MATCH_GIF_WIDTH as usize + x] = color;
        }
    }

    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: u8) {
        for y in top.min(bottom)..=top.max(bottom) {
            for x in left.min(right)..=left.max(right) {
                self.set(x, y, color);
            }
        }
    }

    fn outline_rect(
        &mut self,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        color: u8,
        width: i32,
    ) {
        for offset in 0..width {
            self.line(
                left + offset,
                top + offset,
                right - offset,
                top + offset,
                color,
                1,
            );
            self.line(
                left + offset,
                bottom - offset,
                right - offset,
                bottom - offset,
                color,
                1,
            );
            self.line(
                left + offset,
                top + offset,
                left + offset,
                bottom - offset,
                color,
                1,
            );
            self.line(
                right - offset,
                top + offset,
                right - offset,
                bottom - offset,
                color,
                1,
            );
        }
    }

    fn line(&mut self, mut x0: i32, mut y0: i32, x1: i32, y1: i32, color: u8, width: i32) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        loop {
            let radius = width.saturating_sub(1) / 2;
            self.fill_rect(x0 - radius, y0 - radius, x0 + radius, y0 + radius, color);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let twice_error = error * 2;
            if twice_error >= dy {
                error += dy;
                x0 += sx;
            }
            if twice_error <= dx {
                error += dx;
                y0 += sy;
            }
        }
    }

    fn text_centered(&mut self, text: &str, y: i32, color: u8, scale: i32, bold: bool) {
        let x = (i32::from(POST_MATCH_GIF_WIDTH) - text_width(text, scale)) / 2;
        self.text(text, x, y, color, scale, bold);
    }

    fn text(&mut self, text: &str, x: i32, y: i32, color: u8, scale: i32, bold: bool) {
        let advance = 6 * scale;
        for (index, character) in text.chars().enumerate() {
            let offset = i32::try_from(index)
                .unwrap_or(i32::MAX)
                .saturating_mul(advance);
            self.glyph(character, x.saturating_add(offset), y, color, scale, bold);
        }
    }

    fn glyph(&mut self, character: char, x: i32, y: i32, color: u8, scale: i32, bold: bool) {
        for (row, bits) in glyph_rows(character).into_iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                let base_x = x + column * scale;
                let base_y = y + i32::try_from(row).unwrap_or_default() * scale;
                for dy in 0..scale {
                    for dx in 0..scale {
                        self.set(base_x + dx, base_y + dy, color);
                    }
                    if bold {
                        self.set(base_x + scale, base_y + dy, color);
                    }
                }
            }
        }
    }
}

fn text_width(text: &str, scale: i32) -> i32 {
    i32::try_from(text.chars().count())
        .unwrap_or(i32::MAX)
        .saturating_mul(6 * scale)
        .saturating_sub(scale)
}

#[allow(clippy::too_many_lines)]
fn glyph_rows(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0e, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x0e, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x12, 0x12, 0x0c],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x14, 0x04, 0x04, 0x04, 0x1f],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x10, 0x1e, 0x01, 0x01, 0x1e],
        '6' => [0x0e, 0x10, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x01, 0x0e],
        '+' => [0x00, 0x04, 0x04, 0x1f, 0x04, 0x04, 0x00],
        '-' => [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00],
        '=' => [0x00, 0x1f, 0x00, 0x1f, 0x00, 0x00, 0x00],
        '>' => [0x10, 0x08, 0x04, 0x02, 0x04, 0x08, 0x10],
        '<' => [0x01, 0x02, 0x04, 0x08, 0x04, 0x02, 0x01],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '\\' => [0x10, 0x08, 0x08, 0x04, 0x02, 0x02, 0x01],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c, 0x08],
        '%' => [0x11, 0x02, 0x04, 0x08, 0x11, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f],
        ' ' => [0; 7],
        _ => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use gif::{ColorOutput, DecodeOptions};

    use super::*;

    fn assert_theme(theme: &str) {
        let asset = render_post_match_gif(
            "Very Long Display Name ".repeat(3).as_str(),
            1_234_567,
            theme,
        )
        .expect("render post-match theme");
        assert_eq!(asset.kind, theme);
        assert!(asset.shared_palette);
        assert_eq!(asset.frame_durations_ms, frame_durations_ms());
        assert!(asset.bytes.len() < 4 * 1_024 * 1_024);

        let mut options = DecodeOptions::new();
        options.set_color_output(ColorOutput::Indexed);
        let mut decoder = options
            .read_info(Cursor::new(&asset.bytes))
            .expect("decode post-match GIF");
        assert_eq!(
            (decoder.width(), decoder.height()),
            (POST_MATCH_GIF_WIDTH, POST_MATCH_GIF_HEIGHT)
        );
        assert_eq!(decoder.global_palette(), Some(PALETTE));
        let mut frames = Vec::new();
        let mut delays = Vec::new();
        while let Some(frame) = decoder.read_next_frame().expect("read GIF frame") {
            assert!(frame.palette.is_none(), "frames share the global palette");
            frames.push(frame.buffer.to_vec());
            delays.push(u32::from(frame.delay) * 10);
        }
        assert_eq!(frames.len(), POST_MATCH_GIF_FRAME_COUNT);
        assert_eq!(delays, frame_durations_ms());
        assert!(frames.windows(2).any(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn test_post_match_gif_theme_is_seekable_animated_gif_divine_rapier_position() {
        assert_theme("divine_rapier_position");
    }

    #[test]
    fn test_post_match_gif_theme_is_seekable_animated_gif_buyback_denied() {
        assert_theme("buyback_denied");
    }

    #[test]
    fn test_post_match_gif_theme_is_seekable_animated_gif_ancient_liquidated() {
        assert_theme("ancient_liquidated");
    }

    #[test]
    fn test_post_match_gif_theme_is_seekable_animated_gif_beyond_godlike() {
        assert_theme("beyond_godlike");
    }

    #[test]
    fn test_post_match_gif_theme_is_seekable_animated_gif_odds_anomaly() {
        assert_theme("odds_anomaly");
    }

    #[test]
    fn test_post_match_gif_exports_all_supported_themes() {
        assert_eq!(
            POST_MATCH_GIF_THEMES,
            [
                "divine_rapier_position",
                "buyback_denied",
                "ancient_liquidated",
                "beyond_godlike",
                "odds_anomaly",
            ]
        );
    }

    #[test]
    fn test_post_match_gif_rejects_unknown_theme() {
        let error = render_post_match_gif("TestUser", 100, "not_a_theme")
            .expect_err("unknown themes must be rejected");
        assert!(
            error
                .to_string()
                .contains("Unsupported post-match GIF theme")
        );
    }

    #[test]
    fn native_font_repeated_renders_are_byte_identical() {
        let first = render_post_match_gif("Cached Client", 1_337, "odds_anomaly")
            .expect("first native-font render");
        let second = render_post_match_gif("Cached Client", 1_337, "odds_anomaly")
            .expect("second native-font render");
        assert_eq!(first.bytes, second.bytes);
    }

    #[test]
    fn native_font_style_and_scale_parameters_render_distinct_pixels() {
        let mut regular = Canvas::new();
        regular.text("CAMA", 4, 4, NEON_GREEN, 1, false);
        let mut bold = Canvas::new();
        bold.text("CAMA", 4, 4, NEON_GREEN, 1, true);
        let mut scaled = Canvas::new();
        scaled.text("CAMA", 4, 4, NEON_GREEN, 2, false);

        assert_ne!(regular.pixels, bold.pixels);
        assert_ne!(regular.pixels, scaled.pixels);
        assert!(
            bold.pixels
                .iter()
                .filter(|pixel| **pixel == NEON_GREEN)
                .count()
                > regular
                    .pixels
                    .iter()
                    .filter(|pixel| **pixel == NEON_GREEN)
                    .count()
        );
    }

    #[test]
    fn native_font_atlas_covers_required_ascii_with_exact_scale_geometry() {
        for character in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-=<>/\\:.,%_".chars() {
            assert_ne!(glyph_rows(character), [0; 7], "missing glyph {character:?}");
        }
        assert_eq!(text_width("CAMA", 1), 23);
        assert_eq!(text_width("CAMA", 2), 46);
    }

    #[test]
    fn native_font_atlas_is_fixed_width_and_unknown_glyphs_are_visible() {
        assert_eq!(text_width("IIII", 2), text_width("WWWW", 2));
        assert_eq!(glyph_rows('λ'), glyph_rows('?'));
        assert_ne!(glyph_rows('λ'), [0; 7]);
    }

    #[test]
    fn native_font_renderer_has_no_host_truetype_failure_mode() {
        let source = include_str!("post_match_gif_media.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes tests");
        for forbidden in ["DejaVu", "ImageFont", "fontconfig", "std::fs::"] {
            assert!(
                !production.contains(forbidden),
                "native media unexpectedly depends on {forbidden}"
            );
        }
        let asset = render_post_match_gif("No Host Fonts", 42, "buyback_denied")
            .expect("embedded glyph renderer remains available");
        assert!(asset.bytes.starts_with(b"GIF89a"));
    }
}
