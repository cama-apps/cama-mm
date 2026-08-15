//! Production Rust renderer for Neon big-win terminal animations.
//!
//! This is the native counterpart of Python's
//! utils.neon_drawing.create_bigwin_gif. It deliberately uses a fixed indexed
//! palette and a bundled terminal glyph set: the runtime neither shells out to
//! Python nor depends on font discovery to render a Discord attachment.

use std::borrow::Cow;

use gif::{Encoder, Frame, Repeat};
use thiserror::Error;

use crate::dig_neon::{BigWinFlavor, BigWinSource};
use crate::neon_degen::GifAsset;

pub const BIG_WIN_WIDTH: u16 = 400;
pub const BIG_WIN_HEIGHT: u16 = 300;
pub const BIG_WIN_FRAME_COUNT: usize = 40;
pub const BIG_WIN_FINAL_HOLD_MS: u32 = 60_000;

const PIXEL_COUNT: usize = BIG_WIN_WIDTH as usize * BIG_WIN_HEIGHT as usize;
const CRT_BLACK: u8 = 0;
const NEON_GREEN: u8 = 1;
const NEON_CYAN: u8 = 2;
const NEON_YELLOW: u8 = 5;
const DIM_GREEN: u8 = 6;
const DIM_CYAN: u8 = 7;
const SCANLINE_BLACK: u8 = 8;
const SCANLINE_GREEN: u8 = 9;
const SCANLINE_CYAN: u8 = 10;
const GREEN_GLOW: u8 = 12;
const SCANLINE_YELLOW: u8 = 13;
const SCANLINE_DIM_GREEN: u8 = 14;
const SCANLINE_DIM_CYAN: u8 = 15;

// One animation-wide palette. It contains the Python scene's exact headline
// colors plus dim scanline/glow variants. Every frame references this global
// table and carries no local palette.
const PALETTE: &[u8] = &[
    10, 10, 15, // CRT black
    0, 255, 65, // neon green
    0, 255, 255, // neon cyan
    255, 0, 128, // neon pink
    255, 30, 30, // neon red
    255, 220, 0, // neon yellow
    0, 120, 30, // dim green
    0, 100, 100, // dim cyan
    5, 5, 8, // dark scanline
    0, 200, 50, // scanline green
    0, 200, 200, // scanline cyan
    220, 230, 225, // terminal white
    0, 45, 12, // green phosphor glow
    200, 175, 0, // scanline yellow
    0, 90, 22, // scanline dim green
    0, 75, 75, // scanline dim cyan
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BigWinScene {
    pub source_line: &'static str,
    pub source_progress_line: String,
    pub banner: &'static str,
    pub reconcile_client_line: String,
    pub client_line: String,
    pub payout_line: String,
    pub kind: &'static str,
    accent: u8,
}

impl BigWinScene {
    #[must_use]
    pub fn new(name: &str, payout: i64, source: BigWinSource, flavor: BigWinFlavor) -> Self {
        let source_line = match source {
            BigWinSource::Match => "MATCH SETTLED",
            BigWinSource::Prediction => "MARKET RESOLVED",
            BigWinSource::Gamba => "TABLE PAYS OUT",
        };
        let (banner, accent) = match flavor {
            BigWinFlavor::BigWin => ("PAYOUT CONFIRMED", NEON_GREEN),
            BigWinFlavor::TopDog => ("TOP OF THE BOOK", NEON_CYAN),
            BigWinFlavor::Underdog => ("FADE THE PUBLIC", NEON_YELLOW),
        };
        let kind = match (source, flavor) {
            (BigWinSource::Match, BigWinFlavor::BigWin) => "bigwin_match",
            (BigWinSource::Prediction, BigWinFlavor::TopDog) => "bigwin_prediction",
            (BigWinSource::Gamba, BigWinFlavor::Underdog) => "bigwin_gamba",
            _ => "bigwin",
        };
        Self {
            source_line,
            source_progress_line: format!("> {}...", source_line.to_ascii_lowercase()),
            banner,
            reconcile_client_line: format!("> client: {name}"),
            client_line: format!("client {name}"),
            payout_line: format!("+{} jc", grouped_decimal(payout)),
            kind,
            accent,
        }
    }
}

#[derive(Debug, Error)]
pub enum BigWinRenderError {
    #[error("GIF encoding failed: {0}")]
    Gif(#[from] gif::EncodingError),
}

/// Render the full Python-equivalent Neon big-win animation in memory.
pub fn render_big_win(
    name: &str,
    payout: i64,
    source: BigWinSource,
    flavor: BigWinFlavor,
) -> Result<GifAsset, BigWinRenderError> {
    let scene = BigWinScene::new(name, payout, source, flavor);
    let durations = frame_durations_ms();
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(&mut bytes, BIG_WIN_WIDTH, BIG_WIN_HEIGHT, PALETTE)?;
        // Pillow writes NETSCAPE loop=1 for this generator. Together with the
        // 60-second terminal frame this preserves the production animation.
        encoder.set_repeat(Repeat::Finite(1))?;
        let entropy = scene_entropy(name, payout, source, flavor);
        for (frame_index, duration_ms) in durations.iter().copied().enumerate() {
            let mut pixels = render_frame(&scene, payout, frame_index, entropy);
            apply_scanlines(&mut pixels);
            if frame_index == 26 {
                apply_glitch(&mut pixels, entropy);
            }
            let mut frame = Frame {
                width: BIG_WIN_WIDTH,
                height: BIG_WIN_HEIGHT,
                delay: u16::try_from(duration_ms / 10)
                    .expect("Neon GIF duration fits the GIF centisecond field"),
                buffer: Cow::Owned(pixels),
                ..Frame::default()
            };
            frame.dispose = gif::DisposalMethod::Keep;
            encoder.write_frame(&frame)?;
        }
    }
    Ok(GifAsset {
        kind: scene.kind,
        bytes,
        frame_durations_ms: durations,
        shared_palette: true,
    })
}

#[must_use]
pub fn frame_durations_ms() -> Vec<u32> {
    let mut durations = Vec::with_capacity(BIG_WIN_FRAME_COUNT);
    durations.extend([110; 8]);
    // Pillow stores GIF delays in centiseconds: the source 55ms/95ms values
    // therefore round down to the observed production 50ms/90ms stream.
    durations.extend([50; 16]);
    durations.extend([90; 2]);
    durations.extend([110; 13]);
    durations.push(BIG_WIN_FINAL_HOLD_MS);
    durations
}

fn render_frame(scene: &BigWinScene, payout: i64, frame_index: usize, entropy: u64) -> Vec<u8> {
    let mut canvas = Canvas::new();
    match frame_index {
        0..=7 => render_reconciling(&mut canvas, scene, frame_index),
        8..=25 => render_count_up(&mut canvas, scene, payout, frame_index - 8),
        26..=39 => render_banner(&mut canvas, scene, payout, frame_index - 26, entropy),
        _ => unreachable!("frame index is bounded by BIG_WIN_FRAME_COUNT"),
    }
    canvas.pixels
}

fn render_reconciling(canvas: &mut Canvas, scene: &BigWinScene, frame: usize) {
    canvas.text_centered("JOPA-T/v3.7 LEDGER", 24, NEON_GREEN, 2, true);
    canvas.text_left(&scene.source_progress_line, 24, 70, DIM_GREEN, 2, false);
    canvas.text_left(&scene.reconcile_client_line, 24, 92, DIM_GREEN, 2, false);
    if frame.is_multiple_of(2) {
        canvas.text_left("> reconciling _", 24, 114, NEON_GREEN, 2, false);
    }
}

fn render_count_up(canvas: &mut Canvas, scene: &BigWinScene, payout: i64, frame: usize) {
    canvas.text_centered(scene.source_line, 32, DIM_GREEN, 2, false);
    let shown = payout.saturating_mul(i64::try_from(frame + 1).unwrap_or(18)) / 18;
    canvas.text_centered(
        &format!("+{}", grouped_decimal(shown)),
        128,
        scene.accent,
        4,
        true,
    );
    canvas.text_centered("JOPACOIN", 166, DIM_CYAN, 2, false);
}

fn render_banner(
    canvas: &mut Canvas,
    scene: &BigWinScene,
    payout: i64,
    frame: usize,
    entropy: u64,
) {
    let flash = frame.is_multiple_of(2);
    canvas.text_centered("==============================", 40, DIM_GREEN, 1, false);
    canvas.text_centered(
        scene.banner,
        68,
        if flash { scene.accent } else { NEON_YELLOW },
        3,
        true,
    );
    canvas.text_centered("==============================", 104, DIM_GREEN, 1, false);
    canvas.text_centered(
        &format!("+{} jc", grouped_decimal(payout)),
        135,
        NEON_GREEN,
        2,
        true,
    );
    canvas.text_centered(&scene.client_line, 165, DIM_GREEN, 2, false);
    if flash {
        let mut random = TinyRandom::new(entropy ^ (frame as u64).wrapping_mul(0x9e37_79b9));
        for _ in 0..30 {
            let x = 20 + random.bounded(u32::from(BIG_WIN_WIDTH) - 40) as i32;
            let y = 20 + random.bounded(u32::from(BIG_WIN_HEIGHT) - 40) as i32;
            let color = match random.bounded(3) {
                0 => NEON_GREEN,
                1 => NEON_CYAN,
                _ => NEON_YELLOW,
            };
            canvas.set(x, y, color);
        }
    }
    canvas.text_centered("JOPA-T/v3.7", 276, DIM_GREEN, 2, false);
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
        if x < BIG_WIN_WIDTH as usize && y < BIG_WIN_HEIGHT as usize {
            self.pixels[y * BIG_WIN_WIDTH as usize + x] = color;
        }
    }

    fn text_left(&mut self, text: &str, x: i32, y: i32, color: u8, scale: i32, bold: bool) {
        self.text(text, x, y, color, scale, bold);
    }

    fn text_centered(&mut self, text: &str, y: i32, color: u8, scale: i32, bold: bool) {
        let width = text_width(text, scale);
        let x = (i32::from(BIG_WIN_WIDTH) - width) / 2;
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
        let rows = glyph_rows(character);
        let glow = match color {
            NEON_GREEN | NEON_YELLOW => GREEN_GLOW,
            NEON_CYAN => DIM_CYAN,
            _ => color,
        };
        if matches!(color, NEON_GREEN | NEON_CYAN | NEON_YELLOW) {
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                self.glyph_pixels(rows, x + dx, y + dy, glow, scale, bold);
            }
        }
        self.glyph_pixels(rows, x, y, color, scale, bold);
    }

    fn glyph_pixels(&mut self, rows: [u8; 7], x: i32, y: i32, color: u8, scale: i32, bold: bool) {
        for (row, bits) in rows.into_iter().enumerate() {
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
    let glyphs = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    glyphs.saturating_mul(6 * scale).saturating_sub(scale)
}

fn apply_scanlines(pixels: &mut [u8]) {
    for y in (0..BIG_WIN_HEIGHT as usize).step_by(2) {
        let row = &mut pixels[y * BIG_WIN_WIDTH as usize..(y + 1) * BIG_WIN_WIDTH as usize];
        for pixel in row {
            *pixel = match *pixel {
                CRT_BLACK => SCANLINE_BLACK,
                NEON_GREEN => SCANLINE_GREEN,
                NEON_CYAN => SCANLINE_CYAN,
                NEON_YELLOW => SCANLINE_YELLOW,
                DIM_GREEN => SCANLINE_DIM_GREEN,
                DIM_CYAN => SCANLINE_DIM_CYAN,
                other => other,
            };
        }
    }
}

fn apply_glitch(pixels: &mut [u8], entropy: u64) {
    let original = pixels.to_vec();
    let mut random = TinyRandom::new(entropy ^ 0xa11c_e5ed_5eed);
    for _ in 0..7 {
        let y = random.bounded(u32::from(BIG_WIN_HEIGHT) - 10) as usize;
        let height = 2 + random.bounded(7) as usize;
        let offset = random.bounded(31) as isize - 15;
        for row in y..(y + height).min(BIG_WIN_HEIGHT as usize) {
            for x in 0..BIG_WIN_WIDTH as usize {
                let source_x = (x as isize - offset).rem_euclid(BIG_WIN_WIDTH as isize) as usize;
                pixels[row * BIG_WIN_WIDTH as usize + x] =
                    original[row * BIG_WIN_WIDTH as usize + source_x];
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

fn scene_entropy(name: &str, payout: i64, source: BigWinSource, flavor: BigWinFlavor) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in name
        .bytes()
        .chain(payout.to_le_bytes())
        .chain([source as u8, flavor as u8])
    {
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

// Compact 5x7 terminal glyphs. Lowercase intentionally shares uppercase
// outlines, matching the all-caps CRT aesthetic while retaining punctuation,
// digits, and a visible fallback for arbitrary player names.
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

    fn decoded_frames(asset: &GifAsset) -> (Vec<Vec<u8>>, Vec<u32>) {
        let mut options = DecodeOptions::new();
        options.set_color_output(ColorOutput::Indexed);
        let mut decoder = options
            .read_info(Cursor::new(&asset.bytes))
            .expect("decode rendered big-win GIF");
        assert_eq!((decoder.width(), decoder.height()), (400, 300));
        assert_eq!(decoder.global_palette(), Some(PALETTE));
        let mut frames = Vec::new();
        let mut durations = Vec::new();
        while let Some(frame) = decoder.read_next_frame().expect("read GIF frame") {
            assert!(frame.palette.is_none(), "all frames use the global palette");
            frames.push(frame.buffer.to_vec());
            durations.push(u32::from(frame.delay) * 10);
        }
        (frames, durations)
    }

    #[test]
    fn prediction_top_dog_scene_preserves_exact_terminal_copy() {
        let scene = BigWinScene::new("pf", 3_200, BigWinSource::Prediction, BigWinFlavor::TopDog);
        assert_eq!(scene.source_line, "MARKET RESOLVED");
        assert_eq!(scene.source_progress_line, "> market resolved...");
        assert_eq!(scene.banner, "TOP OF THE BOOK");
        assert_eq!(scene.reconcile_client_line, "> client: pf");
        assert_eq!(scene.client_line, "client pf");
        assert_eq!(scene.payout_line, "+3,200 jc");
        assert_eq!(scene.kind, "bigwin_prediction");
    }

    #[test]
    fn prediction_top_dog_is_a_real_400x300_40_frame_shared_palette_gif() {
        let asset = render_big_win("pf", 3_200, BigWinSource::Prediction, BigWinFlavor::TopDog)
            .expect("render prediction big-win GIF");
        let (frames, durations) = decoded_frames(&asset);
        assert_eq!(frames.len(), BIG_WIN_FRAME_COUNT);
        assert_eq!(durations, frame_durations_ms());
        assert_eq!(durations.iter().sum::<u32>(), 63_290);
        assert_eq!(durations.last(), Some(&BIG_WIN_FINAL_HOLD_MS));
        assert!(asset.bytes.len() < 4 * 1_024 * 1_024);
        assert!(
            asset
                .bytes
                .windows(11)
                .any(|window| window == b"NETSCAPE2.0")
        );
    }

    #[test]
    fn decoded_frames_contain_each_visual_phase_and_top_dog_accent() {
        let asset = render_big_win(
            "Client-1234",
            3_200,
            BigWinSource::Prediction,
            BigWinFlavor::TopDog,
        )
        .expect("render prediction big-win GIF");
        let (frames, _) = decoded_frames(&asset);

        // Reconciling cursor blinks between the first two otherwise-identical
        // terminal frames.
        assert_ne!(frames[0], frames[1]);
        let cursor_region = |frame: &[u8]| {
            frame[114 * 400..129 * 400]
                .iter()
                .filter(|pixel| matches!(**pixel, NEON_GREEN | SCANLINE_GREEN))
                .count()
        };
        assert!(cursor_region(&frames[0]) > cursor_region(&frames[1]));

        // The count-up and banner phases use cyan for prediction/top-dog.
        let cyan_pixels = |frame: &[u8], start_y: usize, end_y: usize| {
            frame[start_y * 400..end_y * 400]
                .iter()
                .filter(|pixel| matches!(**pixel, NEON_CYAN | SCANLINE_CYAN))
                .count()
        };
        assert!(cyan_pixels(&frames[25], 125, 165) > 100);
        assert!(cyan_pixels(&frames[26], 65, 95) > 100);

        // The final frame is a clean, readable hold rather than the phase-3
        // glitch/spray frame. Python alternates the headline and deliberately
        // leaves its last (odd) frame yellow for the 60-second hold.
        assert_ne!(frames[26], frames[39]);
        let final_yellow = frames[39][65 * 400..95 * 400]
            .iter()
            .filter(|pixel| matches!(**pixel, NEON_YELLOW | SCANLINE_YELLOW))
            .count();
        assert!(final_yellow > 100);
        let final_green = frames[39][130 * 400..295 * 400]
            .iter()
            .filter(|pixel| matches!(**pixel, NEON_GREEN | SCANLINE_GREEN))
            .count();
        assert!(final_green > 100);
    }

    #[test]
    fn renderer_is_deterministic_and_input_sensitive() {
        let first = render_big_win("pf", 3_200, BigWinSource::Prediction, BigWinFlavor::TopDog)
            .expect("first render");
        let second = render_big_win("pf", 3_200, BigWinSource::Prediction, BigWinFlavor::TopDog)
            .expect("second render");
        let changed = render_big_win(
            "another-player",
            3_201,
            BigWinSource::Prediction,
            BigWinFlavor::TopDog,
        )
        .expect("changed render");
        assert_eq!(first.bytes, second.bytes);
        assert_ne!(first.bytes, changed.bytes);
    }
}
