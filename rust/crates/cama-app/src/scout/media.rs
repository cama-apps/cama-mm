//! Native Scout report rendering, including cached Steam hero portraits.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use flate2::read::ZlibDecoder;
use thiserror::Error;

use super::{ScoutImageRenderer, ScoutReportInput};

const WIDTH: usize = 360;
const PADDING: i32 = 12;
const HEADER_HEIGHT: i32 = 50;
const ROW_HEIGHT: i32 = 32;
const PORTRAIT_WIDTH: usize = 48;
const PORTRAIT_HEIGHT: usize = 27;
const MAX_SOURCE_PIXELS: usize = 16_777_216;

const BG: Rgba = Rgba([0x36, 0x39, 0x3f, 0xff]);
const DARKER: Rgba = Rgba([0x2f, 0x31, 0x36, 0xff]);
const WHITE: Rgba = Rgba([0xff, 0xff, 0xff, 0xff]);
const GREY: Rgba = Rgba([0x99, 0xa0, 0xaa, 0xff]);
const ACCENT: Rgba = Rgba([0x58, 0x65, 0xf2, 0xff]);
const GREEN: Rgba = Rgba([0x57, 0xf2, 0x87, 0xff]);
const LIGHT_GREEN: Rgba = Rgba([0x9c, 0xe6, 0x82, 0xff]);
const YELLOW: Rgba = Rgba([0xfe, 0xd1, 0x4b, 0xff]);
const RED: Rgba = Rgba([0xed, 0x42, 0x45, 0xff]);

#[derive(Clone, Debug, Default)]
pub struct NativeScoutImageRenderer {
    portraits: BTreeMap<i64, Vec<u8>>,
}

impl NativeScoutImageRenderer {
    #[must_use]
    pub fn new(portraits: BTreeMap<i64, Vec<u8>>) -> Self {
        Self { portraits }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ScoutMediaError {
    #[error("Scout raster dimensions overflow")]
    DimensionOverflow,
}

impl ScoutImageRenderer for NativeScoutImageRenderer {
    type Error = ScoutMediaError;

    fn render(&self, report: &ScoutReportInput) -> Result<Vec<u8>, Self::Error> {
        render_report(report, &self.portraits)
    }
}

/// Whether bytes can be decoded by the native production portrait path.
/// Scout's disk cache uses this to discard partial/corrupt downloads before
/// falling back to a fresh Steam CDN copy, matching the Python cache policy.
#[must_use]
pub fn portrait_png_is_supported(bytes: &[u8]) -> bool {
    decode_png(bytes).is_ok()
}

fn render_report(
    report: &ScoutReportInput,
    portraits: &BTreeMap<i64, Vec<u8>>,
) -> Result<Vec<u8>, ScoutMediaError> {
    if report.scout_data.heroes.is_empty() {
        let mut raster = Raster::new(WIDTH, 100, BG)?;
        raster.text(20, 40, "No hero data available", GREY, 1);
        return Ok(encode_png(&raster));
    }
    let height = usize::try_from(PADDING * 2 + HEADER_HEIGHT)
        .ok()
        .and_then(|fixed| {
            report
                .scout_data
                .heroes
                .len()
                .checked_mul(ROW_HEIGHT as usize)
                .and_then(|rows| fixed.checked_add(rows))
        })
        .ok_or(ScoutMediaError::DimensionOverflow)?;
    let mut raster = Raster::new(WIDTH, height, BG)?;
    let title = truncate(&report.title, 38);
    raster.center_text(PADDING, &title, WHITE, 1);
    if !report.player_names.is_empty() {
        let shown = report
            .player_names
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>();
        let mut names = shown.join(", ");
        if report.player_names.len() > 5 {
            names.push_str(&format!(" +{}", report.player_names.len() - 5));
        }
        raster.center_text(PADDING + 22, &truncate(&names, 38), GREY, 1);
    }

    const HERO_X: i32 = 16;
    const TOTAL_X: i32 = 72;
    const CR_X: i32 = 106;
    const W_X: i32 = 146;
    const L_X: i32 = 174;
    const B_X: i32 = 202;
    const WR_X: i32 = 230;
    let header_y = PADDING + HEADER_HEIGHT - 18;
    for (x, label) in [
        (TOTAL_X, "Tot"),
        (CR_X, "CR"),
        (W_X, "W"),
        (L_X, "L"),
        (B_X, "B"),
        (WR_X, "WR"),
    ] {
        raster.text(x, header_y, label, GREY, 1);
    }
    raster.horizontal_line(
        PADDING,
        WIDTH as i32 - PADDING,
        PADDING + HEADER_HEIGHT - 5,
        ACCENT,
    );

    let total_matches = report.scout_data.total_matches.unwrap_or_default();
    let mut y = PADDING + HEADER_HEIGHT;
    for (index, hero) in report.scout_data.heroes.iter().enumerate() {
        if index % 2 == 1 {
            raster.fill_rect(PADDING, y, WIDTH as i32 - PADDING, y + ROW_HEIGHT, DARKER);
        }
        let image_y = y + (ROW_HEIGHT - PORTRAIT_HEIGHT as i32) / 2;
        let pasted = portraits
            .get(&hero.hero_id)
            .and_then(|bytes| decode_png(bytes).ok())
            .is_some_and(|portrait| {
                raster.paste_scaled(HERO_X, image_y, PORTRAIT_WIDTH, PORTRAIT_HEIGHT, &portrait);
                true
            });
        if !pasted {
            raster.fill_rect(
                HERO_X,
                image_y,
                HERO_X + PORTRAIT_WIDTH as i32 + 1,
                image_y + PORTRAIT_HEIGHT as i32 + 1,
                DARKER,
            );
            raster.rect_outline(
                HERO_X,
                image_y,
                HERO_X + PORTRAIT_WIDTH as i32,
                image_y + PORTRAIT_HEIGHT as i32,
                GREY,
            );
        }
        let games = hero.wins.saturating_add(hero.losses);
        let total = games.saturating_add(hero.bans);
        let contest_rate = if total_matches > 0 {
            total as f64 / total_matches as f64
        } else {
            0.0
        };
        let win_rate = if games > 0 {
            hero.wins as f64 / games as f64
        } else {
            0.0
        };
        let stat_y = y + (ROW_HEIGHT - 7) / 2;
        raster.right_text(TOTAL_X, 25, stat_y, &total.to_string(), WHITE);
        raster.right_text(
            CR_X,
            32,
            stat_y,
            &format!("{:.0}%", contest_rate * 100.0),
            contest_color(contest_rate),
        );
        raster.right_text(W_X, 20, stat_y, &hero.wins.to_string(), GREEN);
        raster.right_text(L_X, 20, stat_y, &hero.losses.to_string(), RED);
        raster.right_text(
            B_X,
            20,
            stat_y,
            &hero.bans.to_string(),
            if hero.bans > 0 { RED } else { GREY },
        );
        let (wr, color) = if games > 0 {
            (format!("{:.0}%", win_rate * 100.0), winrate_color(win_rate))
        } else {
            ("-".to_owned(), GREY)
        };
        raster.right_text(WR_X, 32, stat_y, &wr, color);
        y += ROW_HEIGHT;
    }
    Ok(encode_png(&raster))
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    format!(
        "{}..",
        value
            .chars()
            .take(limit.saturating_sub(2))
            .collect::<String>()
    )
}

fn contest_color(rate: f64) -> Rgba {
    if rate >= 0.75 {
        RED
    } else if rate >= 0.5 {
        YELLOW
    } else if rate >= 0.25 {
        LIGHT_GREEN
    } else {
        GREY
    }
}

fn winrate_color(rate: f64) -> Rgba {
    if rate >= 0.6 {
        GREEN
    } else if rate >= 0.5 {
        LIGHT_GREEN
    } else if rate >= 0.4 {
        YELLOW
    } else {
        RED
    }
}

#[derive(Clone, Copy)]
struct Rgba([u8; 4]);

struct Raster {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Raster {
    fn new(width: usize, height: usize, color: Rgba) -> Result<Self, ScoutMediaError> {
        let bytes = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(ScoutMediaError::DimensionOverflow)?;
        let mut data = vec![0; bytes];
        for pixel in data.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color.0);
        }
        Ok(Self {
            width,
            height,
            pixels: data,
        })
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
        for y in top.max(0)..bottom.min(self.height as i32) {
            for x in left.max(0)..right.min(self.width as i32) {
                self.set(x, y, color);
            }
        }
    }

    fn horizontal_line(&mut self, left: i32, right: i32, y: i32, color: Rgba) {
        for x in left..right {
            self.set(x, y, color);
        }
    }

    fn rect_outline(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        for x in left..=right {
            self.set(x, top, color);
            self.set(x, bottom, color);
        }
        for y in top..=bottom {
            self.set(left, y, color);
            self.set(right, y, color);
        }
    }

    fn text(&mut self, x: i32, y: i32, value: &str, color: Rgba, scale: i32) {
        let mut cursor = x;
        for character in value.chars() {
            for (row, bits) in glyph(character).into_iter().enumerate() {
                for column in 0..5_u8 {
                    if bits & (1 << (4 - column)) != 0 {
                        let left = cursor + i32::from(column) * scale;
                        let top = y + i32::try_from(row).unwrap_or_default() * scale;
                        self.fill_rect(left, top, left + scale, top + scale, color);
                    }
                }
            }
            cursor += 6 * scale;
        }
    }

    fn center_text(&mut self, y: i32, value: &str, color: Rgba, scale: i32) {
        let width = i32::try_from(value.chars().count()).unwrap_or_default() * 6 * scale;
        self.text((self.width as i32 - width) / 2, y, value, color, scale);
    }

    fn right_text(&mut self, left: i32, width: i32, y: i32, value: &str, color: Rgba) {
        let text_width = i32::try_from(value.chars().count()).unwrap_or_default() * 6;
        self.text((left + width - text_width).max(left), y, value, color, 1);
    }

    fn paste_scaled(
        &mut self,
        left: i32,
        top: i32,
        width: usize,
        height: usize,
        source: &DecodedImage,
    ) {
        for y in 0..height {
            let source_y = y * source.height / height;
            for x in 0..width {
                let source_x = x * source.width / width;
                let offset = (source_y * source.width + source_x) * 4;
                let alpha = source.rgba[offset + 3];
                if alpha != 0 {
                    self.set(
                        left + x as i32,
                        top + y as i32,
                        Rgba([
                            source.rgba[offset],
                            source.rgba[offset + 1],
                            source.rgba[offset + 2],
                            alpha,
                        ]),
                    );
                }
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
        '9' => [14, 17, 17, 15, 1, 1, 30],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '%' => [25, 25, 2, 4, 8, 19, 19],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        ',' => [0, 0, 0, 0, 4, 4, 8],
        ' ' => [0; 7],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

struct DecodedImage {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

fn decode_png(bytes: &[u8]) -> Result<DecodedImage, ()> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(());
    }
    let mut cursor = 8_usize;
    let mut width = 0_usize;
    let mut height = 0_usize;
    let mut bit_depth = 0_u8;
    let mut color_type = 0_u8;
    let mut interlace = 0_u8;
    let mut compressed = Vec::new();
    let mut palette = Vec::new();
    let mut transparency = Vec::new();
    while cursor.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        let length =
            u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().map_err(|_| ())?) as usize;
        let kind = bytes.get(cursor + 4..cursor + 8).ok_or(())?;
        let start = cursor + 8;
        let end = start.checked_add(length).ok_or(())?;
        let data = bytes.get(start..end).ok_or(())?;
        match kind {
            b"IHDR" if data.len() == 13 => {
                width = u32::from_be_bytes(data[0..4].try_into().map_err(|_| ())?) as usize;
                height = u32::from_be_bytes(data[4..8].try_into().map_err(|_| ())?) as usize;
                bit_depth = data[8];
                color_type = data[9];
                interlace = data[12];
            }
            b"PLTE" => palette.extend_from_slice(data),
            b"tRNS" => transparency.extend_from_slice(data),
            b"IDAT" => compressed.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        cursor = end.checked_add(4).ok_or(())?;
    }
    let channels = match color_type {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err(()),
    };
    if width == 0
        || height == 0
        || bit_depth != 8
        || interlace != 0
        || width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > MAX_SOURCE_PIXELS)
    {
        return Err(());
    }
    let row_bytes = width.checked_mul(channels).ok_or(())?;
    let expected = height
        .checked_mul(row_bytes.checked_add(1).ok_or(())?)
        .ok_or(())?;
    let mut raw = Vec::with_capacity(expected);
    ZlibDecoder::new(Cursor::new(compressed))
        .take(expected as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| ())?;
    if raw.len() != expected {
        return Err(());
    }
    let mut decoded = vec![0_u8; row_bytes * height];
    for y in 0..height {
        let source = y * (row_bytes + 1);
        let filter = raw[source];
        for x in 0..row_bytes {
            let byte = raw[source + 1 + x];
            let left = x
                .checked_sub(channels)
                .map_or(0, |at| decoded[y * row_bytes + at]);
            let up = y
                .checked_sub(1)
                .map_or(0, |row| decoded[row * row_bytes + x]);
            let up_left = match (y.checked_sub(1), x.checked_sub(channels)) {
                (Some(row), Some(column)) => decoded[row * row_bytes + column],
                _ => 0,
            };
            decoded[y * row_bytes + x] = match filter {
                0 => byte,
                1 => byte.wrapping_add(left),
                2 => byte.wrapping_add(up),
                3 => byte.wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                4 => byte.wrapping_add(paeth(left, up, up_left)),
                _ => return Err(()),
            };
        }
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for pixel in decoded.chunks_exact(channels) {
        match color_type {
            0 => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 0xff]),
            2 => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]),
            3 => {
                let index = usize::from(pixel[0]);
                let start = index.checked_mul(3).ok_or(())?;
                rgba.extend_from_slice(&[
                    *palette.get(start).ok_or(())?,
                    *palette.get(start + 1).ok_or(())?,
                    *palette.get(start + 2).ok_or(())?,
                    transparency.get(index).copied().unwrap_or(0xff),
                ]);
            }
            4 => rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]),
            6 => rgba.extend_from_slice(pixel),
            _ => unreachable!(),
        }
    }
    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn paeth(left: u8, up: u8, upper_left: u8) -> u8 {
    let left = i32::from(left);
    let up = i32::from(up);
    let upper_left = i32::from(upper_left);
    let prediction = left + up - upper_left;
    let left_distance = (prediction - left).abs();
    let up_distance = (prediction - up).abs();
    let diagonal_distance = (prediction - upper_left).abs();
    if left_distance <= up_distance && left_distance <= diagonal_distance {
        left as u8
    } else if up_distance <= diagonal_distance {
        up as u8
    } else {
        upper_left as u8
    }
}

fn encode_png(raster: &Raster) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(raster.width as u32).to_be_bytes());
    header.extend_from_slice(&(raster.height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &header);
    let row_bytes = raster.width * 4;
    let mut filtered = Vec::with_capacity((row_bytes + 1) * raster.height);
    for row in raster.pixels.chunks_exact(row_bytes) {
        filtered.push(0);
        filtered.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks = filtered.chunks(65_535).collect::<Vec<_>>();
    for (index, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(index + 1 == blocks.len()));
        let length = u16::try_from(block.len()).expect("stored deflate block");
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&filtered).to_be_bytes());
    chunk(&mut png, b"IDAT", &zlib);
    chunk(&mut png, b"IEND", &[]);
    png
}

fn chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut checksum = kind.to_vec();
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
    let (mut a, mut b) = (1_u32, 0_u32);
    for byte in bytes {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scout::{ScoutData, ScoutHero};

    #[test]
    fn native_report_is_a_real_fixed_geometry_png_with_portrait_fallback() {
        let report = ScoutReportInput {
            scout_data: ScoutData {
                player_count: 2,
                total_matches: Some(20),
                heroes: vec![ScoutHero {
                    hero_id: 14,
                    games: 10,
                    wins: 6,
                    losses: 4,
                    bans: 2,
                    primary_role: 1,
                }],
            },
            player_names: vec!["Ada".to_owned(), "Linus".to_owned()],
            title: "SCOUT: Radiant".to_owned(),
        };
        let png = NativeScoutImageRenderer::default()
            .render(&report)
            .expect("render native scout report");
        let decoded = decode_png(&png).expect("decode generated png");
        assert_eq!((decoded.width, decoded.height), (360, 106));
        assert!(decoded.rgba.chunks_exact(4).any(|pixel| pixel == GREEN.0));
        assert!(decoded.rgba.chunks_exact(4).any(|pixel| pixel == RED.0));
    }

    #[test]
    fn png_decoder_accepts_the_native_output_and_rejects_non_png() {
        let report = ScoutReportInput {
            scout_data: ScoutData {
                player_count: 0,
                total_matches: Some(0),
                heroes: Vec::new(),
            },
            player_names: Vec::new(),
            title: "SCOUT".to_owned(),
        };
        let png = NativeScoutImageRenderer::default().render(&report).unwrap();
        assert_eq!(
            decode_png(&png).map(|image| (image.width, image.height)),
            Ok((360, 100))
        );
        assert_eq!(decode_png(b"not a png").map(|image| image.width), Err(()));
    }

    #[test]
    fn cached_rgba_portrait_is_decoded_scaled_and_pasted() {
        let mut source = Raster::new(2, 2, Rgba([0x12, 0x34, 0x56, 0xff])).unwrap();
        source.pixels[3] = 0xff;
        let report = ScoutReportInput {
            scout_data: ScoutData {
                player_count: 1,
                total_matches: Some(1),
                heroes: vec![ScoutHero {
                    hero_id: 1,
                    games: 1,
                    wins: 1,
                    losses: 0,
                    bans: 0,
                    primary_role: 1,
                }],
            },
            player_names: vec!["Ada".to_owned()],
            title: "SCOUT".to_owned(),
        };
        let png = NativeScoutImageRenderer::new(BTreeMap::from([(1, encode_png(&source))]))
            .render(&report)
            .unwrap();
        let decoded = decode_png(&png).unwrap();
        let portrait_y = usize::try_from(PADDING + HEADER_HEIGHT + 2).unwrap();
        let portrait_x = 16_usize;
        let offset = (portrait_y * decoded.width + portrait_x) * 4;
        assert_eq!(&decoded.rgba[offset..offset + 4], &[0x12, 0x34, 0x56, 0xff]);
    }
}
