//! Deterministic pet-card media policy for the side-by-side Rust migration.
//!
//! This module is the live native renderer. It emits standards-valid RGBA PNG
//! files, preserves full-card overrides, composes the shipped hybrid component
//! pack with semantic anchor sidecars, and falls back per slot to deterministic
//! procedural art. Filesystem discovery and Discord delivery remain separate
//! runtime boundaries.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use cama_domain::pet::{PetMood, PetStage};
use cama_domain::pet_evolution::{PetCalling, PetInstinct};
use flate2::read::ZlibDecoder;
use gif::{ColorOutput, DecodeOptions};

pub const CARD_WIDTH: usize = 512;
pub const CARD_HEIGHT: usize = 288;
pub const VERSUS_WIDTH: usize = CARD_WIDTH * 2;

const FLOOR_Y: i32 = 244;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_AUTHORED_ASSET_BYTES: u64 = 8 * 1024 * 1024;
const MAX_AUTHORED_PIXELS: usize = 4_096 * 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    const TRANSPARENT: Self = Self(0, 0, 0, 0);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RasterImage {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl RasterImage {
    #[must_use]
    pub fn new(width: usize, height: usize, color: Rgba) -> Self {
        let mut pixels = vec![0; width.saturating_mul(height).saturating_mul(4)];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[color.0, color.1, color.2, color.3]);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    #[must_use]
    pub fn transparent_card() -> Self {
        Self::new(CARD_WIDTH, CARD_HEIGHT, Rgba::TRANSPARENT)
    }

    #[must_use]
    pub fn solid_card(color: Rgba) -> Self {
        Self::new(CARD_WIDTH, CARD_HEIGHT, color)
    }

    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> Option<Rgba> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (y * self.width + x) * 4;
        Some(Rgba(
            self.pixels[offset],
            self.pixels[offset + 1],
            self.pixels[offset + 2],
            self.pixels[offset + 3],
        ))
    }

    #[must_use]
    pub fn alpha_bounds(&self) -> Option<(usize, usize, usize, usize)> {
        let mut left = self.width;
        let mut top = self.height;
        let mut right = 0;
        let mut bottom = 0;
        let mut found = false;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.pixels[(y * self.width + x) * 4 + 3] == 0 {
                    continue;
                }
                found = true;
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
        found.then_some((left, top, right, bottom))
    }

    #[must_use]
    pub fn encode_png(&self) -> Vec<u8> {
        encode_png(self)
    }

    fn set_pixel(&mut self, x: i32, y: i32, source: Rgba) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x >= self.width || y >= self.height || source.3 == 0 {
            return;
        }
        let offset = (y * self.width + x) * 4;
        if source.3 == u8::MAX || self.pixels[offset + 3] == 0 {
            self.pixels[offset..offset + 4]
                .copy_from_slice(&[source.0, source.1, source.2, source.3]);
            return;
        }
        let source_alpha = u32::from(source.3);
        let target_alpha = u32::from(self.pixels[offset + 3]);
        let inverse = 255 - source_alpha;
        let output_alpha = source_alpha + (target_alpha * inverse + 127) / 255;
        for (channel, source_channel) in [source.0, source.1, source.2].into_iter().enumerate() {
            let premultiplied = u32::from(source_channel) * source_alpha
                + (u32::from(self.pixels[offset + channel]) * target_alpha * inverse + 127) / 255;
            self.pixels[offset + channel] =
                u8::try_from((premultiplied + output_alpha / 2) / output_alpha).unwrap_or(u8::MAX);
        }
        self.pixels[offset + 3] = u8::try_from(output_alpha).unwrap_or(u8::MAX);
    }

    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        for y in top.min(bottom)..top.max(bottom) {
            for x in left.min(right)..left.max(right) {
                self.set_pixel(x, y, color);
            }
        }
    }

    fn ellipse(&mut self, bounds: (i32, i32, i32, i32), color: Rgba) {
        let (left, top, right, bottom) = bounds;
        let width = (right - left).max(1);
        let height = (bottom - top).max(1);
        let center_x_twice = left + right;
        let center_y_twice = top + bottom;
        let width_squared = i64::from(width) * i64::from(width);
        let height_squared = i64::from(height) * i64::from(height);
        let limit = width_squared * height_squared;
        for y in top..=bottom {
            for x in left..=right {
                let dx = i64::from(2 * x - center_x_twice);
                let dy = i64::from(2 * y - center_y_twice);
                if dx * dx * height_squared + dy * dy * width_squared <= limit {
                    self.set_pixel(x, y, color);
                }
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
                x0 += sx;
            }
            if twice_error <= dx {
                error += dx;
                y0 += sy;
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

    fn overlay(&mut self, source: &Self, offset_x: i32, offset_y: i32, mirror_x: bool) {
        for source_y in 0..source.height {
            for target_x in 0..source.width {
                let source_x = if mirror_x {
                    source.width - target_x - 1
                } else {
                    target_x
                };
                let offset = (source_y * source.width + source_x) * 4;
                self.set_pixel(
                    offset_x + i32::try_from(target_x).unwrap_or(i32::MAX),
                    offset_y + i32::try_from(source_y).unwrap_or(i32::MAX),
                    Rgba(
                        source.pixels[offset],
                        source.pixels[offset + 1],
                        source.pixels[offset + 2],
                        source.pixels[offset + 3],
                    ),
                );
            }
        }
    }

    fn resized_lanczos(&self, width: usize, height: usize) -> Self {
        if self.width == width && self.height == height {
            return self.clone();
        }
        if width == 0 || height == 0 || self.width == 0 || self.height == 0 {
            return Self::new(width, height, Rgba::TRANSPARENT);
        }
        let x_weights = lanczos_weights(self.width, width);
        let y_weights = lanczos_weights(self.height, height);
        let mut output = Self::new(width, height, Rgba::TRANSPARENT);
        for (y, y_weights_for_pixel) in y_weights.iter().enumerate() {
            for (x, x_weights_for_pixel) in x_weights.iter().enumerate() {
                let mut channels = [0.0_f64; 4];
                let mut total_weight = 0.0_f64;
                for &(source_y, y_weight) in y_weights_for_pixel {
                    for &(source_x, x_weight) in x_weights_for_pixel {
                        let weight = x_weight * y_weight;
                        let source = (source_y * self.width + source_x) * 4;
                        let alpha = f64::from(self.pixels[source + 3]) / 255.0;
                        for (channel, value) in channels[..3].iter_mut().enumerate() {
                            *value += f64::from(self.pixels[source + channel]) * alpha * weight;
                        }
                        channels[3] += f64::from(self.pixels[source + 3]) * weight;
                        total_weight += weight;
                    }
                }
                let target = (y * width + x) * 4;
                if total_weight.abs() > f64::EPSILON {
                    let alpha = channels[3] / total_weight;
                    let alpha_byte = round_channel(alpha);
                    output.pixels[target + 3] = alpha_byte;
                    if alpha_byte != 0 {
                        for (channel, value) in channels[..3].iter().enumerate() {
                            output.pixels[target + channel] =
                                round_channel(*value / total_weight * 255.0 / alpha);
                        }
                    } else {
                        output.pixels[target..target + 3].fill(0);
                    }
                }
            }
        }
        output
    }

    fn draw_glyph(&mut self, x: i32, y: i32, character: char, color: Rgba, scale: i32) {
        for (row, bits) in glyph(character).into_iter().enumerate() {
            for column in 0..5_u8 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                let left = x + i32::from(column) * scale;
                let top = y + i32::try_from(row).unwrap_or_default() * scale;
                self.fill_rect(left, top, left + scale, top + scale, color);
            }
        }
    }

    fn text(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        for (index, character) in text.chars().enumerate() {
            self.draw_glyph(
                x + i32::try_from(index).unwrap_or(i32::MAX) * 6 * scale,
                y,
                character,
                color,
                scale,
            );
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayerSlot {
    Backdrop,
    Ground,
    Back,
    Creature,
    Detail,
    Face,
    Front,
}

impl LayerSlot {
    const ORDER: [Self; 7] = [
        Self::Backdrop,
        Self::Ground,
        Self::Back,
        Self::Creature,
        Self::Detail,
        Self::Face,
        Self::Front,
    ];

    const fn key(self) -> &'static str {
        match self {
            Self::Backdrop => "backdrop",
            Self::Ground => "ground",
            Self::Back => "back",
            Self::Creature => "creature",
            Self::Detail => "detail",
            Self::Face => "face",
            Self::Front => "front",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvolutionVisual {
    pub calling: PetCalling,
    pub primary: PetInstinct,
    pub secondary: Option<PetInstinct>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PetRenderRequest<'a> {
    pub species_id: &'a str,
    pub stage: PetStage,
    pub mood: PetMood,
    pub seed: i64,
    pub accessory: Option<&'a str>,
    pub evolution: Option<EvolutionVisual>,
}

#[derive(Clone, Copy)]
struct Geometry {
    center_x: i32,
    body_y: i32,
    body_rx: i32,
    body_ry: i32,
    head_y: i32,
    head_rx: i32,
    head_ry: i32,
}

fn geometry(stage: PetStage) -> Geometry {
    match stage {
        PetStage::Adult => Geometry {
            center_x: 256,
            body_y: 184,
            body_rx: 92,
            body_ry: 55,
            head_y: 110,
            head_rx: 51,
            head_ry: 46,
        },
        PetStage::Egg | PetStage::Baby => Geometry {
            center_x: 256,
            body_y: 194,
            body_rx: 73,
            body_ry: 43,
            head_y: 130,
            head_rx: 58,
            head_ry: 51,
        },
    }
}

#[derive(Clone, Copy)]
struct Palette {
    body: Rgba,
    light: Rgba,
    dark: Rgba,
    accent: Rgba,
}

fn palette(species_id: &str) -> Palette {
    let signature = stable_signature(species_id);
    let body = Rgba(
        105 + u8::try_from(signature & 79).unwrap_or_default(),
        92 + u8::try_from((signature >> 8) & 71).unwrap_or_default(),
        88 + u8::try_from((signature >> 16) & 75).unwrap_or_default(),
        255,
    );
    let light = Rgba(
        body.0.saturating_add(48),
        body.1.saturating_add(48),
        body.2.saturating_add(48),
        255,
    );
    let dark = Rgba(body.0 / 2, body.1 / 2, body.2 / 2, 255);
    let accent = match species_id {
        "riverglow_cama" => Rgba(62, 234, 226, 255),
        "prismwool_cama" => Rgba(238, 126, 255, 255),
        "moondrift_cama" => Rgba(180, 196, 255, 255),
        "sunspun_cama" => Rgba(255, 198, 55, 255),
        "embergear_cama" => Rgba(255, 104, 40, 255),
        _ => Rgba(
            100 + u8::try_from((signature >> 24) & 127).unwrap_or_default(),
            90 + u8::try_from((signature >> 32) & 127).unwrap_or_default(),
            80 + u8::try_from((signature >> 40) & 127).unwrap_or_default(),
            255,
        ),
    };
    Palette {
        body,
        light,
        dark,
        accent,
    }
}

#[must_use]
pub fn render_layer(
    slot: LayerSlot,
    species_id: &str,
    stage: PetStage,
    mood: PetMood,
    seed: i64,
) -> RasterImage {
    let mut image = RasterImage::transparent_card();
    let mut random = DeterministicRandom::new(stable_signature(&format!("{seed}:{}", slot.key())));
    let geometry = geometry(stage);
    let palette = palette(species_id);
    match slot {
        LayerSlot::Backdrop => draw_backdrop(&mut image, &mut random),
        LayerSlot::Ground => draw_ground(&mut image, species_id, geometry, palette),
        LayerSlot::Back => draw_back(&mut image, species_id, geometry, palette),
        LayerSlot::Creature => draw_creature(&mut image, species_id, geometry, palette),
        LayerSlot::Detail => draw_detail(&mut image, species_id, geometry, palette, &mut random),
        LayerSlot::Face => draw_face(&mut image, geometry, palette, mood),
        LayerSlot::Front => draw_front(&mut image, species_id, geometry, palette),
    }
    image
}

fn draw_backdrop(image: &mut RasterImage, random: &mut DeterministicRandom) {
    image.fill_rect(
        0,
        0,
        CARD_WIDTH as i32,
        CARD_HEIGHT as i32,
        Rgba(37, 31, 54, 255),
    );
    image.fill_rect(0, 150, CARD_WIDTH as i32, FLOOR_Y, Rgba(47, 38, 62, 255));
    image.fill_rect(
        0,
        FLOOR_Y,
        CARD_WIDTH as i32,
        CARD_HEIGHT as i32,
        Rgba(29, 27, 39, 255),
    );
    for _ in 0..64 {
        let x = random.range(5, CARD_WIDTH as i32 - 5);
        let y = random.range(6, 146);
        let glow = 130 + u8::try_from(random.range(0, 110)).unwrap_or_default();
        image.set_pixel(x, y, Rgba(glow, glow, glow.saturating_add(8), 180));
    }
}

fn draw_ground(image: &mut RasterImage, species_id: &str, geometry: Geometry, palette: Palette) {
    image.ellipse(
        (
            geometry.center_x - geometry.body_rx - 35,
            FLOOR_Y - 13,
            geometry.center_x + geometry.body_rx + 35,
            FLOOR_Y + 14,
        ),
        Rgba(23, 20, 33, 205),
    );
    match species_id {
        "riverglow_cama" => {
            for radius in [118, 90, 62] {
                image.ellipse(
                    (
                        geometry.center_x - radius,
                        FLOOR_Y - radius / 7,
                        geometry.center_x + radius,
                        FLOOR_Y + radius / 7,
                    ),
                    Rgba(35, 205, 216, 42),
                );
            }
            image.line((146, FLOOR_Y), (366, FLOOR_Y), palette.accent, 2);
        }
        "prismwool_cama" => {
            let colors = [
                Rgba(255, 74, 134, 150),
                Rgba(255, 211, 67, 145),
                Rgba(69, 224, 201, 140),
                Rgba(111, 145, 255, 145),
            ];
            for (index, color) in colors.into_iter().enumerate() {
                let x = 118 + i32::try_from(index).unwrap_or_default() * 92;
                image.polygon(
                    &[
                        (x, FLOOR_Y),
                        (x + 42, FLOOR_Y),
                        (x + 72, 278),
                        (x - 24, 278),
                    ],
                    color,
                );
            }
        }
        "moondrift_cama" => {
            for x in [102, 156, 356, 410] {
                image.ellipse(
                    (x - 12, FLOOR_Y - 7, x + 12, FLOOR_Y + 7),
                    Rgba(173, 190, 255, 130),
                );
                image.ellipse(
                    (x - 5, FLOOR_Y - 8, x + 15, FLOOR_Y + 5),
                    Rgba(0, 0, 0, 120),
                );
            }
        }
        _ => {}
    }
}

fn draw_back(image: &mut RasterImage, species_id: &str, geometry: Geometry, palette: Palette) {
    let center = geometry.center_x;
    match species_id {
        "aegis_cama" => image.ellipse(
            (
                center - geometry.body_rx,
                geometry.body_y - geometry.body_ry - 42,
                center + geometry.body_rx,
                geometry.body_y + 18,
            ),
            Rgba(252, 206, 74, 255),
        ),
        "dromedary_cross" => image.ellipse(
            (
                center - geometry.body_rx + 18,
                geometry.body_y - geometry.body_ry - 50,
                center + geometry.body_rx - 18,
                geometry.body_y + 12,
            ),
            palette.light,
        ),
        "embergear_cama" => {
            for (x, y, radius, color) in [
                (
                    center + geometry.body_rx + 12,
                    geometry.body_y,
                    18,
                    Rgba(96, 82, 82, 190),
                ),
                (
                    center + geometry.body_rx + 34,
                    geometry.body_y - 18,
                    13,
                    Rgba(82, 74, 82, 155),
                ),
                (
                    center + geometry.body_rx + 51,
                    geometry.body_y - 31,
                    9,
                    Rgba(70, 66, 78, 115),
                ),
            ] {
                image.ellipse((x - radius, y - radius, x + radius, y + radius), color);
            }
        }
        "sunspun_cama" => {
            image.ellipse(
                (
                    center - geometry.body_rx - 24,
                    geometry.body_y - geometry.body_ry - 30,
                    center + geometry.body_rx + 24,
                    geometry.body_y + geometry.body_ry + 24,
                ),
                Rgba(255, 184, 72, 45),
            );
            for index in 0..16 {
                let angle = f64::from(index) * std::f64::consts::TAU / 16.0;
                let start = (
                    center + (95.0 * angle.cos()) as i32,
                    geometry.body_y + (72.0 * angle.sin()) as i32,
                );
                let end = (
                    center + (120.0 * angle.cos()) as i32,
                    geometry.body_y + (94.0 * angle.sin()) as i32,
                );
                image.line(
                    start,
                    end,
                    Rgba(palette.accent.0, palette.accent.1, palette.accent.2, 155),
                    3,
                );
            }
        }
        _ => {}
    }
}

fn draw_creature(image: &mut RasterImage, species_id: &str, geometry: Geometry, palette: Palette) {
    let center = geometry.center_x;
    for leg_x in [center - 50, center + 38] {
        image.fill_rect(
            leg_x,
            geometry.body_y + 24,
            leg_x + 18,
            FLOOR_Y,
            palette.dark,
        );
        image.fill_rect(
            leg_x - 3,
            FLOOR_Y - 7,
            leg_x + 23,
            FLOOR_Y + 2,
            palette.dark,
        );
    }
    image.ellipse(
        (
            center - geometry.body_rx,
            geometry.body_y - geometry.body_ry,
            center + geometry.body_rx,
            geometry.body_y + geometry.body_ry,
        ),
        palette.body,
    );
    if matches!(species_id, "dromedary_cross" | "aegis_cama") {
        image.ellipse(
            (
                center - 35,
                geometry.body_y - geometry.body_ry - 34,
                center + 38,
                geometry.body_y,
            ),
            palette.body,
        );
    }
    image.fill_rect(
        center - 27,
        geometry.head_y + 22,
        center + 27,
        geometry.body_y,
        palette.body,
    );
    image.ellipse(
        (
            center - geometry.head_rx,
            geometry.head_y - geometry.head_ry,
            center + geometry.head_rx,
            geometry.head_y + geometry.head_ry,
        ),
        palette.body,
    );
    image.ellipse(
        (
            center - 25,
            geometry.head_y + 15,
            center + 25,
            geometry.head_y + 44,
        ),
        palette.light,
    );
}

fn draw_detail(
    image: &mut RasterImage,
    species_id: &str,
    geometry: Geometry,
    palette: Palette,
    random: &mut DeterministicRandom,
) {
    let center = geometry.center_x;
    if species_id == "riverglow_cama" {
        image.polygon(
            &[
                (center, geometry.body_y - 18),
                (center - 8, geometry.body_y + 2),
                (center, geometry.body_y + 11),
                (center + 8, geometry.body_y + 2),
            ],
            palette.accent,
        );
        return;
    }
    let count = 3 + usize::try_from(stable_signature(species_id) % 5).unwrap_or_default();
    for _ in 0..count {
        let x = random.range(
            center - geometry.body_rx + 18,
            center + geometry.body_rx - 18,
        );
        let y = random.range(
            geometry.body_y - geometry.body_ry + 15,
            geometry.body_y + geometry.body_ry - 14,
        );
        image.ellipse((x - 5, y - 5, x + 5, y + 5), palette.accent);
    }
    if species_id == "prismwool_cama" {
        for (offset, color) in [
            (-46, Rgba(255, 91, 140, 255)),
            (-16, Rgba(255, 222, 94, 255)),
            (16, Rgba(91, 224, 199, 255)),
            (46, Rgba(109, 145, 255, 255)),
        ] {
            image.ellipse(
                (
                    center + offset - 13,
                    geometry.body_y - 13,
                    center + offset + 13,
                    geometry.body_y + 13,
                ),
                color,
            );
        }
    }
}

fn draw_face(image: &mut RasterImage, geometry: Geometry, palette: Palette, mood: PetMood) {
    let center = geometry.center_x;
    let eye_y = geometry.head_y - 3;
    let eye_color = Rgba(25, 22, 29, 255);
    match mood.art_key() {
        "happy" => {
            image.line((center - 28, eye_y), (center - 17, eye_y + 5), eye_color, 4);
            image.line((center - 17, eye_y + 5), (center - 8, eye_y), eye_color, 4);
            image.line((center + 8, eye_y), (center + 17, eye_y + 5), eye_color, 4);
            image.line((center + 17, eye_y + 5), (center + 28, eye_y), eye_color, 4);
            image.line(
                (center - 10, eye_y + 26),
                (center, eye_y + 32),
                palette.dark,
                3,
            );
            image.line(
                (center, eye_y + 32),
                (center + 10, eye_y + 26),
                palette.dark,
                3,
            );
        }
        "hungry" => {
            image.ellipse((center - 29, eye_y - 7, center - 12, eye_y + 10), eye_color);
            image.ellipse((center + 12, eye_y - 7, center + 29, eye_y + 10), eye_color);
            image.ellipse(
                (center - 7, eye_y + 25, center + 7, eye_y + 39),
                palette.dark,
            );
        }
        _ => {
            image.ellipse((center - 27, eye_y - 6, center - 13, eye_y + 8), eye_color);
            image.ellipse((center + 13, eye_y - 6, center + 27, eye_y + 8), eye_color);
            image.line(
                (center - 9, eye_y + 30),
                (center + 9, eye_y + 30),
                palette.dark,
                3,
            );
        }
    }
}

fn draw_front(image: &mut RasterImage, species_id: &str, geometry: Geometry, palette: Palette) {
    if species_id == "riverglow_cama" {
        return;
    }
    if species_id == "invoker_cama" {
        for (offset_x, offset_y, color) in [
            (-75, -35, Rgba(248, 102, 74, 230)),
            (0, -86, Rgba(108, 204, 255, 230)),
            (75, -35, Rgba(158, 109, 255, 230)),
        ] {
            image.ellipse(
                (
                    geometry.center_x + offset_x - 11,
                    geometry.head_y + offset_y - 11,
                    geometry.center_x + offset_x + 11,
                    geometry.head_y + offset_y + 11,
                ),
                color,
            );
        }
    } else if species_id == "pudge_cama" {
        image.line(
            (geometry.center_x - 58, geometry.body_y + 30),
            (geometry.center_x + 58, geometry.body_y + 30),
            palette.accent,
            5,
        );
    }
}

fn draw_accessory(image: &mut RasterImage, accessory: &str, stage: PetStage) {
    let geometry = geometry(stage);
    let center = geometry.center_x;
    match accessory {
        "red_bow" => {
            image.polygon(
                &[
                    (center, geometry.head_y - 54),
                    (center - 36, geometry.head_y - 69),
                    (center - 34, geometry.head_y - 39),
                ],
                Rgba(210, 38, 57, 255),
            );
            image.polygon(
                &[
                    (center, geometry.head_y - 54),
                    (center + 36, geometry.head_y - 69),
                    (center + 34, geometry.head_y - 39),
                ],
                Rgba(210, 38, 57, 255),
            );
            image.ellipse(
                (
                    center - 10,
                    geometry.head_y - 64,
                    center + 10,
                    geometry.head_y - 44,
                ),
                Rgba(255, 93, 110, 255),
            );
        }
        "top_hat" => {
            let headwear_y = geometry.head_y - geometry.head_rx;
            image.fill_rect(
                center - 42,
                headwear_y - 4,
                center + 42,
                headwear_y + 7,
                Rgba(25, 24, 32, 255),
            );
            image.fill_rect(
                center - 28,
                headwear_y - 42,
                center + 28,
                headwear_y,
                Rgba(35, 34, 43, 255),
            );
            image.fill_rect(
                center - 28,
                headwear_y - 12,
                center + 28,
                headwear_y - 6,
                Rgba(150, 30, 40, 255),
            );
        }
        _ => {
            image.ellipse(
                (
                    center - 12,
                    geometry.head_y + 39,
                    center + 12,
                    geometry.head_y + 63,
                ),
                Rgba(247, 206, 77, 255),
            );
        }
    }
}

fn draw_evolution(image: &mut RasterImage, visual: EvolutionVisual, stage: PetStage, seed: i64) {
    if stage != PetStage::Adult {
        return;
    }
    let instincts = [Some(visual.primary), visual.secondary];
    let mut random = DeterministicRandom::new(stable_signature(&format!(
        "{seed}:{}",
        visual.calling.as_str()
    )));
    for index in 0..12 {
        let instinct = instincts[index % instincts.len()].unwrap_or(visual.primary);
        let color = instinct_color(instinct);
        let left = index % 2 == 0;
        let x = if left {
            random.range(32, 150)
        } else {
            random.range(362, 480)
        };
        let y = random.range(40, 220);
        image.polygon(&[(x, y - 6), (x + 6, y), (x, y + 6), (x - 6, y)], color);
    }
}

const fn instinct_color(instinct: PetInstinct) -> Rgba {
    match instinct {
        PetInstinct::Delving => Rgba(181, 132, 77, 210),
        PetInstinct::Competition => Rgba(237, 77, 84, 210),
        PetInstinct::Fortune => Rgba(250, 205, 67, 210),
        PetInstinct::Fellowship => Rgba(94, 211, 144, 210),
        PetInstinct::Wisdom => Rgba(139, 125, 240, 210),
    }
}

#[must_use]
pub fn render_pet_card(request: &PetRenderRequest<'_>) -> RasterImage {
    let mut card = RasterImage::solid_card(Rgba(28, 24, 39, 255));
    for slot in LayerSlot::ORDER {
        let layer = render_layer(
            slot,
            request.species_id,
            request.stage,
            request.mood,
            request.seed,
        );
        if slot == LayerSlot::Front {
            if let Some(accessory) = request.accessory {
                let mut accessory_layer = RasterImage::transparent_card();
                draw_accessory(&mut accessory_layer, accessory, request.stage);
                card.overlay(&accessory_layer, 0, 0, false);
            }
            if let Some(evolution) = request.evolution {
                let mut evolution_layer = RasterImage::transparent_card();
                draw_evolution(&mut evolution_layer, evolution, request.stage, request.seed);
                card.overlay(&evolution_layer, 0, 0, false);
            }
        }
        card.overlay(&layer, 0, 0, false);
    }
    card
}

#[must_use]
pub fn render_egg_card(seed: i64) -> RasterImage {
    let mut random = DeterministicRandom::new(seed as u64);
    let mut image = RasterImage::solid_card(Rgba(39, 31, 52, 255));
    image.fill_rect(
        0,
        FLOOR_Y,
        CARD_WIDTH as i32,
        CARD_HEIGHT as i32,
        Rgba(34, 29, 40, 255),
    );
    for radius in (30..=120).rev().step_by(10) {
        image.ellipse(
            (256 - radius, 150 - radius, 256 + radius, 150 + radius),
            Rgba(255, 188, 82, 8),
        );
    }
    image.ellipse((126, 208, 386, 266), Rgba(103, 72, 36, 255));
    for _ in 0..42 {
        let x = random.range(140, 372);
        let y = random.range(213, 256);
        image.line(
            (x, y),
            (x + random.range(-18, 19), y + random.range(-4, 5)),
            Rgba(188, 147, 72, 255),
            2,
        );
    }
    image.ellipse((198, 66, 314, 230), Rgba(239, 230, 206, 255));
    for _ in 0..16 {
        let x = random.range(216, 296);
        let y = random.range(92, 206);
        image.ellipse((x - 3, y - 3, x + 3, y + 3), Rgba(174, 148, 115, 255));
    }
    image.ellipse((214, 82, 242, 113), Rgba(255, 250, 238, 225));
    image
}

#[must_use]
pub fn render_tombstone_card(name: &str) -> RasterImage {
    let mut image = RasterImage::solid_card(Rgba(18, 16, 40, 255));
    let mut random = DeterministicRandom::new(0);
    for _ in 0..40 {
        image.set_pixel(
            random.range(4, 508),
            random.range(4, 150),
            Rgba(210, 214, 240, 255),
        );
    }
    image.ellipse((394, 42, 422, 70), Rgba(238, 238, 252, 255));
    image.fill_rect(
        0,
        FLOOR_Y,
        CARD_WIDTH as i32,
        CARD_HEIGHT as i32,
        Rgba(30, 40, 34, 255),
    );
    image.ellipse((146, 54, 366, 250), Rgba(118, 118, 126, 255));
    image.fill_rect(146, 118, 366, FLOOR_Y + 1, Rgba(118, 118, 126, 255));
    image.line((146, 118), (146, FLOOR_Y), Rgba(58, 58, 66, 255), 4);
    image.line((366, 118), (366, FLOOR_Y), Rgba(58, 58, 66, 255), 4);
    engrave(&mut image, name);
    image
}

fn engrave(image: &mut RasterImage, name: &str) {
    let shown = memorial_name(name);
    let rip = "R.I.P.";
    let rip_x = (CARD_WIDTH as i32 - i32::try_from(rip.len()).unwrap_or_default() * 18) / 2;
    image.text(rip_x + 2, 132, rip, Rgba(190, 188, 210, 180), 3);
    image.text(rip_x, 130, rip, Rgba(64, 61, 83, 235), 3);
    let name_width = i32::try_from(shown.chars().count()).unwrap_or_default() * 12;
    let name_x = (CARD_WIDTH as i32 - name_width) / 2;
    image.text(name_x + 1, 175, &shown, Rgba(190, 188, 210, 180), 2);
    image.text(name_x, 174, &shown, Rgba(64, 61, 83, 235), 2);
}

fn memorial_name(name: &str) -> String {
    let trimmed = name.trim();
    let source = if trimmed.is_empty() {
        "Unnamed"
    } else {
        trimmed
    };
    if source.chars().count() <= 11 {
        return source.to_owned();
    }
    format!("{}...", source.chars().take(8).collect::<String>())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredAsset {
    pub bytes: Vec<u8>,
    pub decoded: Option<RasterImage>,
    pub extension: String,
}

impl AuthoredAsset {
    #[must_use]
    pub fn png(image: RasterImage) -> Self {
        Self {
            bytes: image.encode_png(),
            decoded: Some(image),
            extension: "png".to_owned(),
        }
    }
}

pub trait AuthoredPetAssets {
    fn load(&self, base_name: &str) -> Option<AuthoredAsset>;
}

/// Read Python's finite `assets/pets` override set without changing its
/// precedence (`.gif` before `.png`) or Discord's 8 MiB upload boundary.
#[derive(Clone, Debug)]
pub struct FilesystemPetAssets {
    directory: PathBuf,
}

impl FilesystemPetAssets {
    #[must_use]
    pub fn new(directory: impl AsRef<Path>) -> Self {
        Self {
            directory: directory.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub fn production() -> Self {
        Self::new("/app/assets/pets")
    }

    #[must_use]
    pub fn components_directory(&self) -> PathBuf {
        self.directory.join("components")
    }
}

impl AuthoredPetAssets for FilesystemPetAssets {
    fn load(&self, base_name: &str) -> Option<AuthoredAsset> {
        for extension in ["gif", "png"] {
            let path = self.directory.join(format!("{base_name}.{extension}"));
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > MAX_AUTHORED_ASSET_BYTES {
                continue;
            }
            let Ok(bytes) = fs::read(path) else {
                continue;
            };
            let Ok(byte_len) = u64::try_from(bytes.len()) else {
                continue;
            };
            if byte_len > MAX_AUTHORED_ASSET_BYTES {
                continue;
            }
            let decoded = match extension {
                "gif" => decode_gif_raster(&bytes),
                "png" => decode_png_raster(&bytes),
                _ => None,
            };
            return Some(AuthoredAsset {
                bytes,
                decoded,
                extension: extension.to_owned(),
            });
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoAuthoredPetAssets;

impl AuthoredPetAssets for NoAuthoredPetAssets {
    fn load(&self, _base_name: &str) -> Option<AuthoredAsset> {
        None
    }
}

pub trait PetCardRenderer {
    fn render(&mut self, request: &PetRenderRequest<'_>) -> RasterImage;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProceduralPetRenderer;

impl PetCardRenderer for ProceduralPetRenderer {
    fn render(&mut self, request: &PetRenderRequest<'_>) -> RasterImage {
        render_pet_card(request)
    }
}

/// Production renderer matching Python's hybrid component-pack fallback:
/// each slot independently prefers a species component, then a shared
/// component, and finally the procedural layer. Creature sidecars move the
/// face and accessories onto the authored body's semantic mounts.
#[derive(Clone, Debug)]
pub struct HybridPetRenderer {
    components_directory: PathBuf,
    pools: BTreeMap<(String, String), Vec<PathBuf>>,
    images: BTreeMap<PathBuf, Option<RasterImage>>,
    anchors: BTreeMap<PathBuf, ComponentFrame>,
}

impl HybridPetRenderer {
    #[must_use]
    pub fn new(components_directory: impl AsRef<Path>) -> Self {
        Self {
            components_directory: components_directory.as_ref().to_path_buf(),
            pools: BTreeMap::new(),
            images: BTreeMap::new(),
            anchors: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn production() -> Self {
        Self::new("/app/assets/pets/components")
    }

    pub fn clear_caches(&mut self) {
        self.pools.clear();
        self.images.clear();
        self.anchors.clear();
    }

    fn stage_key(stage: PetStage) -> &'static str {
        match stage {
            PetStage::Adult => "adult",
            PetStage::Egg | PetStage::Baby => "baby",
        }
    }

    fn pool(&mut self, stage: &str, slot: LayerSlot) -> Vec<PathBuf> {
        let key = (stage.to_owned(), slot.key().to_owned());
        if let Some(pool) = self.pools.get(&key) {
            return pool.clone();
        }
        let directory = self.components_directory.join(stage).join(slot.key());
        let mut pool = fs::read_dir(directory)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
            })
            .collect::<Vec<_>>();
        pool.sort();
        if pool.is_empty() && slot == LayerSlot::Backdrop && stage != "adult" {
            pool = self.pool("adult", slot);
        }
        self.pools.insert(key, pool.clone());
        pool
    }

    fn pick_component(
        &mut self,
        slot: LayerSlot,
        stage: &str,
        species_id: &str,
        mood: PetMood,
        seed: i64,
    ) -> Option<(PathBuf, bool)> {
        let pool = self.pool(stage, slot);
        let (species_prefix, shared_prefix) = if slot == LayerSlot::Face {
            (
                format!("{species_id}_{}_", mood.art_key()),
                format!("any_{}_", mood.art_key()),
            )
        } else {
            (format!("{species_id}_"), "any_".to_owned())
        };
        let candidates = |prefix: &str| {
            pool.iter()
                .filter(|path| {
                    path.file_name()
                        .and_then(std::ffi::OsStr::to_str)
                        .is_some_and(|name| name.starts_with(prefix))
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let scoped = candidates(&species_prefix);
        let (choices, shared) = if scoped.is_empty() {
            (candidates(&shared_prefix), true)
        } else {
            (scoped, false)
        };
        if choices.is_empty() {
            return None;
        }
        let signature = stable_signature(&format!("{seed}:pick:{}", slot.key()));
        let index = usize::try_from(signature % choices.len() as u64).unwrap_or_default();
        Some((choices[index].clone(), shared && tintable_slot(slot)))
    }

    fn image(&mut self, path: &Path) -> Option<RasterImage> {
        if let Some(image) = self.images.get(path) {
            return image.clone();
        }
        let image = fs::metadata(path)
            .ok()
            .filter(|metadata| metadata.is_file() && metadata.len() <= MAX_AUTHORED_ASSET_BYTES)
            .and_then(|_| fs::read(path).ok())
            .and_then(|bytes| decode_png_raster(&bytes))
            .filter(|image| image.width == CARD_WIDTH && image.height == CARD_HEIGHT);
        self.images.insert(path.to_path_buf(), image.clone());
        image
    }

    fn frame(&mut self, creature: &Path, stage: PetStage) -> ComponentFrame {
        if let Some(frame) = self.anchors.get(creature) {
            return frame.clone();
        }
        let authoring = authoring_frame(stage);
        let sidecar = creature.with_extension("json");
        let parsed = fs::read_to_string(sidecar)
            .ok()
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok());
        let frame = parsed.map_or(authoring.clone(), |value| {
            frame_from_sidecar(&authoring, &value)
        });
        self.anchors.insert(creature.to_path_buf(), frame.clone());
        frame
    }

    fn resolved_layer(
        &mut self,
        slot: LayerSlot,
        request: &PetRenderRequest<'_>,
        target: &ComponentFrame,
        creature_mask: Option<&RasterImage>,
    ) -> RasterImage {
        let stage = Self::stage_key(request.stage);
        let picked =
            self.pick_component(slot, stage, request.species_id, request.mood, request.seed);
        let (mut layer, source) = if let Some((path, tint)) = picked {
            if let Some(mut image) = self.image(&path) {
                if tint {
                    tint_component(&mut image, request.species_id);
                }
                (image, authoring_frame(request.stage))
            } else {
                (
                    render_layer(
                        slot,
                        request.species_id,
                        request.stage,
                        request.mood,
                        request.seed,
                    ),
                    geometry_frame(request.stage),
                )
            }
        } else {
            (
                render_layer(
                    slot,
                    request.species_id,
                    request.stage,
                    request.mood,
                    request.seed,
                ),
                geometry_frame(request.stage),
            )
        };
        layer = match slot {
            LayerSlot::Ground => fit_ground(&layer, &source, target),
            LayerSlot::Face => {
                let mood_mount = format!("face_{}", request.mood.art_key());
                let mount = target
                    .mounts
                    .get(&mood_mount)
                    .map_or("face", |_| mood_mount.as_str());
                fit_to_mount(&layer, &source, target, mount)
            }
            LayerSlot::Front => fit_to_mount(&layer, &source, target, "head"),
            LayerSlot::Back => fit_to_mount(&layer, &source, target, "body"),
            LayerSlot::Detail => {
                let fitted = fit_to_mount(&layer, &source, target, "body");
                if request.stage == PetStage::Adult
                    && matches!(request.species_id, "courier_cama" | "rama")
                {
                    translate_layer(
                        &fitted,
                        target.mount("chest").center.0 - target.mount("body").center.0,
                        0.0,
                    )
                } else {
                    fitted
                }
            }
            LayerSlot::Backdrop | LayerSlot::Creature => layer,
        };
        if slot == LayerSlot::Face
            && let Some(mask) = creature_mask
        {
            for (pixel, mask) in layer
                .pixels
                .chunks_exact_mut(4)
                .zip(mask.pixels.chunks_exact(4))
            {
                if mask[3] <= 20 {
                    pixel.fill(0);
                }
            }
        }
        layer
    }

    #[must_use]
    pub fn manifest_token(&mut self) -> u64 {
        let mut paths = Vec::new();
        for stage in ["baby", "adult"] {
            for slot in LayerSlot::ORDER {
                paths.extend(self.pool(stage, slot));
            }
        }
        paths.sort();
        paths.dedup();
        paths.iter().fold(0xcbf2_9ce4_8422_2325, |hash, path| {
            path.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or_default()
                .bytes()
                .fold(hash, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
                })
        })
    }
}

impl PetCardRenderer for HybridPetRenderer {
    fn render(&mut self, request: &PetRenderRequest<'_>) -> RasterImage {
        let stage = Self::stage_key(request.stage);
        let creature = self.pick_component(
            LayerSlot::Creature,
            stage,
            request.species_id,
            request.mood,
            request.seed,
        );
        let creature_image = creature.as_ref().and_then(|(path, _)| self.image(path));
        let target = creature
            .as_ref()
            .and_then(|(path, _)| {
                creature_image
                    .as_ref()
                    .map(|_| self.frame(path, request.stage))
            })
            .unwrap_or_else(|| geometry_frame(request.stage));
        let mut card = RasterImage::solid_card(Rgba(28, 24, 39, 255));
        for slot in LayerSlot::ORDER {
            let layer = self.resolved_layer(slot, request, &target, creature_image.as_ref());
            if slot == LayerSlot::Front {
                if let Some(accessory) = request.accessory {
                    let mut accessory_layer = RasterImage::transparent_card();
                    draw_accessory(&mut accessory_layer, accessory, request.stage);
                    let mount = accessory_mount(accessory);
                    let fitted = fit_to_mount(
                        &accessory_layer,
                        &geometry_frame(request.stage),
                        &target,
                        mount,
                    );
                    card.overlay(&fitted, 0, 0, false);
                }
                if let Some(evolution) = request.evolution {
                    let mut evolution_layer = RasterImage::transparent_card();
                    draw_evolution(&mut evolution_layer, evolution, request.stage, request.seed);
                    card.overlay(&evolution_layer, 0, 0, false);
                }
            }
            card.overlay(&layer, 0, 0, false);
        }
        card
    }
}

#[derive(Clone, Copy, Debug)]
struct ComponentMount {
    center: (f64, f64),
    width: f64,
}

#[derive(Clone, Debug)]
struct ComponentFrame {
    mounts: BTreeMap<String, ComponentMount>,
}

impl ComponentFrame {
    fn mount(&self, name: &str) -> ComponentMount {
        self.mounts
            .get(name)
            .copied()
            .or_else(|| self.mounts.get("body").copied())
            .expect("component frame always contains body")
    }
}

fn authoring_frame(stage: PetStage) -> ComponentFrame {
    let values = match stage {
        PetStage::Adult => [
            ("head", 256.0, 64.0, 64.0),
            ("face", 256.0, 68.0, 64.0),
            ("headwear", 256.0, 32.0, 64.0),
            ("neck", 256.0, 100.0, 64.0),
            ("chest", 256.0, 115.0, 64.0),
            ("body", 256.0, 184.0, 160.0),
        ],
        PetStage::Egg | PetStage::Baby => [
            ("head", 256.0, 95.0, 80.0),
            ("face", 256.0, 100.0, 80.0),
            ("headwear", 256.0, 55.0, 80.0),
            ("neck", 256.0, 138.0, 80.0),
            ("chest", 256.0, 149.0, 80.0),
            ("body", 256.0, 190.0, 140.0),
        ],
    };
    ComponentFrame {
        mounts: values
            .into_iter()
            .map(|(name, x, y, width)| {
                (
                    name.to_owned(),
                    ComponentMount {
                        center: (x, y),
                        width,
                    },
                )
            })
            .collect(),
    }
}

fn geometry_frame(stage: PetStage) -> ComponentFrame {
    let geometry = geometry(stage);
    let head_width = f64::from(geometry.head_rx * 2);
    let body_width = f64::from(geometry.body_rx * 2);
    let head_x = f64::from(geometry.center_x);
    let head_y = f64::from(geometry.head_y);
    let body_y = f64::from(geometry.body_y);
    ComponentFrame {
        mounts: [
            ("head", head_x, head_y, head_width),
            ("face", head_x, head_y, head_width),
            ("headwear", head_x, head_y - head_width / 2.0, head_width),
            ("neck", head_x, head_y + head_width * 0.55, head_width),
            (
                "chest",
                head_x,
                head_y + (body_y - head_y) * 0.48,
                head_width,
            ),
            ("body", head_x, body_y, body_width),
        ]
        .into_iter()
        .map(|(name, x, y, width)| {
            (
                name.to_owned(),
                ComponentMount {
                    center: (x, y),
                    width,
                },
            )
        })
        .collect(),
    }
}

fn frame_from_sidecar(authoring: &ComponentFrame, value: &serde_json::Value) -> ComponentFrame {
    let mut frame = authoring.clone();
    let Some(object) = value.as_object() else {
        return frame;
    };
    for (key, center) in object {
        let Some(name) = key.strip_suffix("_center") else {
            continue;
        };
        let Some(center) = center.as_array().filter(|center| center.len() == 2) else {
            continue;
        };
        let (Some(x), Some(y)) = (center[0].as_f64(), center[1].as_f64()) else {
            continue;
        };
        let width = object
            .get(&format!("{name}_width"))
            .and_then(serde_json::Value::as_f64)
            .or_else(|| frame.mounts.get(name).map(|mount| mount.width))
            .unwrap_or(1.0);
        frame.mounts.insert(
            name.to_owned(),
            ComponentMount {
                center: (x, y),
                width,
            },
        );
    }
    let head = frame.mount("head");
    let body = frame.mount("body");
    for (name, mount) in [
        (
            "face",
            ComponentMount {
                center: head.center,
                width: head.width,
            },
        ),
        (
            "headwear",
            ComponentMount {
                center: (head.center.0, head.center.1 - head.width / 2.0),
                width: head.width,
            },
        ),
        (
            "neck",
            ComponentMount {
                center: (head.center.0, head.center.1 + head.width * 0.55),
                width: head.width,
            },
        ),
        (
            "chest",
            ComponentMount {
                center: (
                    (head.center.0 + body.center.0) / 2.0,
                    head.center.1 + (body.center.1 - head.center.1) * 0.48,
                ),
                width: head.width,
            },
        ),
    ] {
        if !object.contains_key(&format!("{name}_center")) {
            frame.mounts.insert(name.to_owned(), mount);
        }
    }
    frame
}

fn fit_to_mount(
    layer: &RasterImage,
    source: &ComponentFrame,
    target: &ComponentFrame,
    mount: &str,
) -> RasterImage {
    let mut source_mount = source.mount(if mount.starts_with("face_") {
        "face"
    } else {
        mount
    });
    if mount.starts_with("face")
        && matches!(source_mount.width.round() as i64, 64 | 80)
        && let Some((left, top, right, bottom)) = alpha_bounds_above(layer, 20)
    {
        let (reference_width, reference_height) = if source_mount.width < 70.0 {
            (42.0, 25.0)
        } else {
            (64.0, 33.0)
        };
        let content_width = (right - left) as f64;
        let content_height = (bottom - top) as f64;
        let fitted_width = if content_width > reference_width {
            content_width + 2.0
        } else {
            reference_width
        };
        let fitted_height = if content_height > reference_height {
            content_height + 2.0
        } else {
            reference_height
        };
        source_mount.center = ((left + right) as f64 / 2.0, (top + bottom) as f64 / 2.0);
        source_mount.width *=
            (fitted_width / reference_width).max(fitted_height / reference_height);
    }
    let target_mount = target.mount(mount);
    let scale = if source_mount.width.abs() < f64::EPSILON {
        1.0
    } else {
        target_mount.width / source_mount.width
    };
    transform_layer(
        layer,
        source_mount.center,
        target_mount.center,
        scale,
        scale,
    )
}

fn alpha_bounds_above(image: &RasterImage, threshold: u8) -> Option<(usize, usize, usize, usize)> {
    let mut bounds = (image.width, image.height, 0_usize, 0_usize);
    let mut found = false;
    for y in 0..image.height {
        for x in 0..image.width {
            if image.pixels[(y * image.width + x) * 4 + 3] <= threshold {
                continue;
            }
            found = true;
            bounds.0 = bounds.0.min(x);
            bounds.1 = bounds.1.min(y);
            bounds.2 = bounds.2.max(x + 1);
            bounds.3 = bounds.3.max(y + 1);
        }
    }
    found.then_some(bounds)
}

fn fit_ground(
    layer: &RasterImage,
    source: &ComponentFrame,
    target: &ComponentFrame,
) -> RasterImage {
    let source_mount = source.mount("body");
    let target_mount = target.mount("body");
    let scale = target_mount.width / source_mount.width.max(1.0);
    transform_layer(
        layer,
        (source_mount.center.0, 0.0),
        (target_mount.center.0, 0.0),
        scale,
        1.0,
    )
}

fn translate_layer(layer: &RasterImage, x: f64, y: f64) -> RasterImage {
    transform_layer(layer, (0.0, 0.0), (x, y), 1.0, 1.0)
}

fn transform_layer(
    layer: &RasterImage,
    source_center: (f64, f64),
    target_center: (f64, f64),
    scale_x: f64,
    scale_y: f64,
) -> RasterImage {
    let mut output = RasterImage::transparent_card();
    for y in 0..CARD_HEIGHT {
        for x in 0..CARD_WIDTH {
            let source_x = source_center.0 + (x as f64 - target_center.0) / scale_x;
            let source_y = source_center.1 + (y as f64 - target_center.1) / scale_y;
            let (source_x, source_y) = (source_x.round() as i32, source_y.round() as i32);
            if source_x < 0 || source_y < 0 {
                continue;
            }
            let (source_x, source_y) = (source_x as usize, source_y as usize);
            if let Some(pixel) = layer.pixel(source_x, source_y) {
                output.set_pixel(x as i32, y as i32, pixel);
            }
        }
    }
    output
}

fn tintable_slot(slot: LayerSlot) -> bool {
    matches!(
        slot,
        LayerSlot::Ground | LayerSlot::Back | LayerSlot::Creature | LayerSlot::Detail
    )
}

fn component_palette(species_id: &str) -> (Rgba, Rgba, Rgba) {
    let (dark, mid, light) = match species_id {
        "dromedary_cross" => ((146, 106, 48), (206, 166, 98), (234, 206, 152)),
        "banana_ears" => ((86, 112, 60), (142, 172, 98), (198, 216, 152)),
        "embergear_cama" => ((68, 58, 54), (138, 90, 58), (214, 164, 94)),
        "jopacama" => ((152, 108, 20), (218, 172, 42), (250, 216, 112)),
        "pudge_cama" => ((108, 48, 38), (162, 82, 62), (204, 130, 104)),
        "courier_cama" => ((94, 56, 26), (139, 90, 43), (188, 138, 88)),
        "riverglow_cama" => ((42, 72, 98), (58, 132, 142), (144, 214, 202)),
        "aegis_cama" => ((186, 172, 96), (234, 224, 150), (255, 250, 206)),
        "invoker_cama" => ((94, 50, 128), (138, 84, 182), (186, 140, 222)),
        "crystal_cama" => ((72, 118, 158), (128, 182, 220), (190, 226, 248)),
        "prismwool_cama" => ((78, 46, 102), (132, 82, 154), (214, 176, 226)),
        "rama" => ((150, 100, 96), (222, 214, 205), (248, 244, 238)),
        "moondrift_cama" => ((32, 36, 78), (72, 82, 138), (188, 202, 236)),
        "sunspun_cama" => ((126, 54, 34), (220, 126, 52), (255, 222, 136)),
        _ => ((122, 89, 53), (181, 141, 96), (215, 185, 141)),
    };
    (
        Rgba(dark.0, dark.1, dark.2, 255),
        Rgba(mid.0, mid.1, mid.2, 255),
        Rgba(light.0, light.1, light.2, 255),
    )
}

fn tint_component(image: &mut RasterImage, species_id: &str) {
    let (dark, mid, light) = component_palette(species_id);
    for pixel in image.pixels.chunks_exact_mut(4) {
        let luminance = (299 * u32::from(pixel[0])
            + 587 * u32::from(pixel[1])
            + 114 * u32::from(pixel[2])
            + 500)
            / 1000;
        let (low, high, numerator, denominator) = if luminance < 128 {
            (dark, mid, luminance, 127)
        } else {
            (mid, light, luminance - 128, 127)
        };
        for (channel, (low, high)) in [(low.0, high.0), (low.1, high.1), (low.2, high.2)]
            .into_iter()
            .enumerate()
        {
            let value = u32::from(low)
                + (u32::from(high).saturating_sub(u32::from(low)) * numerator + denominator / 2)
                    / denominator;
            pixel[channel] = u8::try_from(value).unwrap_or(u8::MAX);
        }
    }
}

fn accessory_mount(accessory: &str) -> &'static str {
    match accessory {
        "bell_collar" | "wool_scarf" => "neck",
        "aegis_charm" | "divine_rapier_pin" => "chest",
        _ => "headwear",
    }
}

#[derive(Debug)]
pub struct PreparedPetFile {
    pub filename: String,
    pub fp: Cursor<Vec<u8>>,
}

impl PreparedPetFile {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.fp.get_ref()
    }
}

pub struct PetAssetLoader<A, R> {
    authored: A,
    renderer: R,
    byte_cache: BTreeMap<String, Vec<u8>>,
}

impl<A: AuthoredPetAssets, R: PetCardRenderer> PetAssetLoader<A, R> {
    #[must_use]
    pub fn new(authored: A, renderer: R) -> Self {
        Self {
            authored,
            renderer,
            byte_cache: BTreeMap::new(),
        }
    }

    pub fn get_pet_card(&mut self, request: &PetRenderRequest<'_>) -> PreparedPetFile {
        let base_name = format!(
            "{}_{}_{}",
            request.species_id,
            request.stage.as_str(),
            request.mood.art_key()
        );
        if request.evolution.is_none()
            && let Some(asset) = self.authored.load(&base_name)
        {
            return prepared(asset.bytes, format!("pet_{base_name}.{}", asset.extension));
        }
        let key = render_cache_key(request);
        let bytes = if let Some(bytes) = self.byte_cache.get(&key) {
            bytes.clone()
        } else {
            let bytes = self.renderer.render(request).encode_png();
            self.byte_cache.insert(key, bytes.clone());
            bytes
        };
        prepared(bytes, format!("pet_{base_name}.png"))
    }

    pub fn get_egg_card(&mut self, seed: i64) -> PreparedPetFile {
        if let Some(asset) = self.authored.load("egg") {
            let extension = asset.extension.clone();
            return prepared(asset.bytes, format!("pet_egg.{extension}"));
        }
        let key = format!("render_egg_{seed}");
        let bytes = self
            .byte_cache
            .entry(key)
            .or_insert_with(|| render_egg_card(seed).encode_png())
            .clone();
        prepared(bytes, "pet_egg.png".to_owned())
    }

    pub fn get_tombstone_card(&mut self, name: &str, _seed: i64) -> PreparedPetFile {
        if let Some(asset) = self.authored.load("tombstone")
            && let Some(mut decoded) = asset.decoded
        {
            let key = format!(
                "engraved_tombstone_{}_{name}",
                stable_signature(&asset.extension)
            );
            let bytes = self
                .byte_cache
                .entry(key)
                .or_insert_with(|| {
                    engrave(&mut decoded, name);
                    decoded.encode_png()
                })
                .clone();
            return prepared(bytes, format!("pet_tombstone.{}", asset.extension));
        }
        let key = format!("render_tombstone_{name}");
        let bytes = self
            .byte_cache
            .entry(key)
            .or_insert_with(|| render_tombstone_card(name).encode_png())
            .clone();
        prepared(bytes, "pet_tombstone.png".to_owned())
    }

    pub fn get_altar_card(&mut self, name: &str, seed: i64) -> PreparedPetFile {
        if let Some(asset) = self.authored.load("altar") {
            let extension = asset.extension.clone();
            return prepared(asset.bytes, format!("pet_altar.{extension}"));
        }
        self.get_tombstone_card(name, seed)
    }

    pub fn get_versus_card(
        &mut self,
        left: &PetRenderRequest<'_>,
        right: &PetRenderRequest<'_>,
    ) -> PreparedPetFile {
        // The native brawl attachment contract is a fixed 1024x288 canvas.
        // Python's _compose_versus instead keeps authored card dimensions
        // intrinsic, so non-512x288 authored cards remain a known cross-
        // runtime difference; the filtered resize below only removes the
        // native renderer's nearest-neighbor artifact within this contract.
        let overlay = self.authored.load("versus_overlay");
        let overlay_token = overlay
            .as_ref()
            .map_or(0, |asset| stable_bytes(&asset.bytes));
        let key = format!(
            "versus_{}_{}_{}_{}_{}_{}_{}",
            render_cache_key(left),
            left.accessory.unwrap_or_default(),
            render_cache_key(right),
            right.accessory.unwrap_or_default(),
            left.seed,
            right.seed,
            overlay_token
        );
        let bytes = if let Some(bytes) = self.byte_cache.get(&key) {
            bytes.clone()
        } else {
            let left_image = self.render_image(left);
            let right_image = self.render_image(right);
            let mut canvas = RasterImage::new(VERSUS_WIDTH, CARD_HEIGHT, Rgba(24, 20, 34, 255));
            canvas.overlay(&left_image, 0, 0, false);
            canvas.overlay(&right_image, CARD_WIDTH as i32, 0, true);
            if let Some(mut decoded) = overlay.and_then(|asset| asset.decoded) {
                if decoded.width != VERSUS_WIDTH || decoded.height != CARD_HEIGHT {
                    decoded = decoded.resized_lanczos(VERSUS_WIDTH, CARD_HEIGHT);
                }
                canvas.overlay(&decoded, 0, 0, false);
            } else {
                canvas.polygon(
                    &[
                        (500, 88),
                        (537, 88),
                        (514, 142),
                        (544, 142),
                        (488, 224),
                        (505, 160),
                        (478, 160),
                    ],
                    Rgba(255, 214, 68, 255),
                );
                canvas.text(478, 116, "VS", Rgba(255, 242, 154, 255), 4);
            }
            let bytes = canvas.encode_png();
            self.byte_cache.insert(key, bytes.clone());
            bytes
        };
        prepared(bytes, "pet_versus.png".to_owned())
    }

    fn render_image(&mut self, request: &PetRenderRequest<'_>) -> RasterImage {
        let base_name = format!(
            "{}_{}_{}",
            request.species_id,
            request.stage.as_str(),
            request.mood.art_key()
        );
        if request.evolution.is_none()
            && let Some(decoded) = self
                .authored
                .load(&base_name)
                .and_then(|asset| asset.decoded)
        {
            // Python passes this authored image through unchanged before
            // _compose_versus. Rust's fixed-card brawl contract normalizes it
            // here, using Pillow-compatible filtering rather than nearest
            // neighbor. Current shipped authored cards are all 512x288.
            return if decoded.width == CARD_WIDTH && decoded.height == CARD_HEIGHT {
                decoded
            } else {
                decoded.resized_lanczos(CARD_WIDTH, CARD_HEIGHT)
            };
        }
        self.renderer.render(request)
    }
}

fn prepared(bytes: Vec<u8>, filename: String) -> PreparedPetFile {
    PreparedPetFile {
        filename,
        fp: Cursor::new(bytes),
    }
}

fn render_cache_key(request: &PetRenderRequest<'_>) -> String {
    let evolution = request.evolution.map_or_else(
        || "none".to_owned(),
        |visual| {
            format!(
                "{}:{}:{}",
                visual.calling.as_str(),
                visual.primary.as_str(),
                visual.secondary.map_or("", PetInstinct::as_str)
            )
        },
    );
    format!(
        "render_{}_{}_{}_{}_{}_{}",
        request.species_id,
        request.stage.as_str(),
        request.mood.art_key(),
        request.seed,
        request.accessory.unwrap_or_default(),
        evolution
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PngInfo {
    pub width: usize,
    pub height: usize,
    pub color_type: u8,
}

/// Decode a bounded, non-interlaced 8-bit PNG into the renderer's RGBA model.
/// Authored pet overrides currently use RGB/RGBA, while palette/greyscale
/// support keeps the adapter faithful to Pillow's accepted input surface.
#[must_use]
pub fn decode_png_raster(bytes: &[u8]) -> Option<RasterImage> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut cursor = PNG_SIGNATURE.len();
    let mut width = 0_usize;
    let mut height = 0_usize;
    let mut bit_depth = 0_u8;
    let mut color_type = 0_u8;
    let mut interlace = 0_u8;
    let mut compressed = Vec::new();
    let mut palette = Vec::new();
    let mut transparency = Vec::new();
    while cursor.checked_add(12).is_some_and(|end| end <= bytes.len()) {
        let length = usize::try_from(read_u32(bytes, cursor)?).ok()?;
        let kind = bytes.get(cursor + 4..cursor + 8)?;
        let start = cursor + 8;
        let end = start.checked_add(length)?;
        let data = bytes.get(start..end)?;
        match kind {
            b"IHDR" if data.len() == 13 => {
                width = usize::try_from(read_u32(data, 0)?).ok()?;
                height = usize::try_from(read_u32(data, 4)?).ok()?;
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
        cursor = end.checked_add(4)?;
    }
    let channels = match color_type {
        0 | 3 => 1_usize,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    if width == 0
        || height == 0
        || bit_depth != 8
        || interlace != 0
        || width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > MAX_AUTHORED_PIXELS)
    {
        return None;
    }
    let row_bytes = width.checked_mul(channels)?;
    let expected = height.checked_mul(row_bytes.checked_add(1)?)?;
    let mut raw = Vec::with_capacity(expected);
    ZlibDecoder::new(Cursor::new(compressed))
        .take(u64::try_from(expected).ok()?.saturating_add(1))
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() != expected {
        return None;
    }
    let mut decoded = vec![0_u8; row_bytes.checked_mul(height)?];
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
            let upper_left = match (y.checked_sub(1), x.checked_sub(channels)) {
                (Some(row), Some(column)) => decoded[row * row_bytes + column],
                _ => 0,
            };
            decoded[y * row_bytes + x] = match filter {
                0 => byte,
                1 => byte.wrapping_add(left),
                2 => byte.wrapping_add(up),
                3 => byte.wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                4 => byte.wrapping_add(paeth_predictor(left, up, upper_left)),
                _ => return None,
            };
        }
    }
    let mut pixels = Vec::with_capacity(width.checked_mul(height)?.checked_mul(4)?);
    for pixel in decoded.chunks_exact(channels) {
        match color_type {
            0 => pixels.extend_from_slice(&[pixel[0], pixel[0], pixel[0], u8::MAX]),
            2 => pixels.extend_from_slice(&[pixel[0], pixel[1], pixel[2], u8::MAX]),
            3 => {
                let index = usize::from(pixel[0]);
                let start = index.checked_mul(3)?;
                pixels.extend_from_slice(&[
                    *palette.get(start)?,
                    *palette.get(start + 1)?,
                    *palette.get(start + 2)?,
                    transparency.get(index).copied().unwrap_or(u8::MAX),
                ]);
            }
            4 => pixels.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]),
            6 => pixels.extend_from_slice(pixel),
            _ => return None,
        }
    }
    Some(RasterImage {
        width,
        height,
        pixels,
    })
}

/// Decode the first frame of a GIF into the logical canvas used by Pillow's
/// `Image.open(...).convert("RGBA")`. The authored bytes remain untouched for
/// direct egg/altar delivery; this raster is only used when a caller needs to
/// engrave or compose the asset.
#[must_use]
pub fn decode_gif_raster(bytes: &[u8]) -> Option<RasterImage> {
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    // Reject an out-of-canvas frame from its descriptor before gif allocates
    // the decoded pixel buffer. The caller still gets a fail-soft `None` for
    // malformed authored input.
    options.check_frame_consistency(true);
    let mut decoder = options.read_info(Cursor::new(bytes)).ok()?;
    let width = usize::from(decoder.width());
    let height = usize::from(decoder.height());
    if width == 0
        || height == 0
        || width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > MAX_AUTHORED_PIXELS)
    {
        return None;
    }
    let background_index = decoder.bg_color();
    let background_rgb = background_index.and_then(|index| {
        let palette = decoder.global_palette()?;
        let start = index.checked_mul(3)?;
        Some((
            *palette.get(start)?,
            *palette.get(start + 1)?,
            *palette.get(start + 2)?,
        ))
    });
    let frame = decoder.read_next_frame().ok()??;
    let frame_width = usize::from(frame.width);
    let frame_height = usize::from(frame.height);
    let left = usize::from(frame.left);
    let top = usize::from(frame.top);
    if left
        .checked_add(frame_width)
        .is_none_or(|right| right > width)
        || top
            .checked_add(frame_height)
            .is_none_or(|bottom| bottom > height)
        || frame_width
            .checked_mul(frame_height)
            .and_then(|pixels| pixels.checked_mul(4))
            .is_none_or(|length| length != frame.buffer.len())
    {
        return None;
    }

    let background = background_rgb.map_or(Rgba::TRANSPARENT, |(red, green, blue)| {
        // A transparency index belongs to the frame's local palette when one
        // is present; it must not be compared numerically with the logical
        // screen's global background index.
        let transparent = frame.palette.is_none()
            && background_index
                .and_then(|index| u8::try_from(index).ok())
                .is_some_and(|index| frame.transparent == Some(index));
        Rgba(red, green, blue, if transparent { 0 } else { u8::MAX })
    });
    let mut image = RasterImage::new(width, height, background);
    for y in 0..frame_height {
        for x in 0..frame_width {
            let source = (y * frame_width + x) * 4;
            if frame.buffer[source + 3] == 0 {
                continue;
            }
            let target = ((top + y) * width + left + x) * 4;
            image.pixels[target..target + 4].copy_from_slice(&frame.buffer[source..source + 4]);
        }
    }
    Some(image)
}

fn round_channel(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn lanczos_weights(source_size: usize, target_size: usize) -> Vec<Vec<(usize, f64)>> {
    let scale = source_size as f64 / target_size as f64;
    let filter_scale = scale.max(1.0);
    let support = 3.0 * filter_scale;
    (0..target_size)
        .map(|target| {
            let center = (target as f64 + 0.5) * scale - 0.5;
            let first = (center - support).floor() as isize;
            let last = (center + support).ceil() as isize;
            let mut weights = Vec::new();
            for source in first..=last {
                let distance = (center - source as f64) / filter_scale;
                let weight = lanczos_kernel(distance);
                if weight == 0.0 {
                    continue;
                }
                if source < 0 || source >= source_size as isize {
                    continue;
                }
                weights.push((source as usize, weight));
            }
            weights
        })
        .collect()
}

fn lanczos_kernel(value: f64) -> f64 {
    if value.abs() < f64::EPSILON {
        return 1.0;
    }
    if value.abs() >= 3.0 {
        return 0.0;
    }
    let pi_value = std::f64::consts::PI * value;
    (pi_value.sin() / pi_value) * ((pi_value / 3.0).sin() / (pi_value / 3.0))
}

fn paeth_predictor(left: u8, up: u8, upper_left: u8) -> u8 {
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

/// Validate the PNG signature, chunk framing/CRCs, RGBA IHDR, and IEND.
pub fn inspect_png(bytes: &[u8]) -> Result<PngInfo, &'static str> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err("missing PNG signature");
    }
    let mut cursor = PNG_SIGNATURE.len();
    let mut info = None;
    let mut saw_idat = false;
    let mut saw_end = false;
    while cursor < bytes.len() {
        let length = read_u32(bytes, cursor).ok_or("truncated PNG chunk length")? as usize;
        cursor += 4;
        let kind = bytes
            .get(cursor..cursor + 4)
            .ok_or("truncated PNG chunk kind")?;
        cursor += 4;
        let data = bytes
            .get(cursor..cursor + length)
            .ok_or("truncated PNG chunk data")?;
        cursor += length;
        let expected = read_u32(bytes, cursor).ok_or("truncated PNG chunk CRC")?;
        cursor += 4;
        let mut checksum = Vec::with_capacity(4 + length);
        checksum.extend_from_slice(kind);
        checksum.extend_from_slice(data);
        if crc32(&checksum) != expected {
            return Err("invalid PNG chunk CRC");
        }
        match kind {
            b"IHDR" if data.len() == 13 => {
                let width = read_u32(data, 0).ok_or("invalid PNG width")? as usize;
                let height = read_u32(data, 4).ok_or("invalid PNG height")? as usize;
                if data[8..] != [8, 6, 0, 0, 0] {
                    return Err("unsupported PNG format");
                }
                info = Some(PngInfo {
                    width,
                    height,
                    color_type: data[9],
                });
            }
            b"IDAT" => saw_idat = true,
            b"IEND" if data.is_empty() => {
                saw_end = true;
                break;
            }
            _ => {}
        }
    }
    if !saw_idat || !saw_end {
        return Err("PNG is missing IDAT or IEND");
    }
    info.ok_or("PNG is missing IHDR")
}

fn encode_png(image: &RasterImage) -> Vec<u8> {
    let mut png = Vec::new();
    png.extend_from_slice(PNG_SIGNATURE);
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(image.width as u32).to_be_bytes());
    header.extend_from_slice(&(image.height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_png_chunk(&mut png, b"IHDR", &header);

    let row_bytes = image.width * 4;
    let mut filtered = Vec::with_capacity((row_bytes + 1) * image.height);
    for row in image.pixels.chunks_exact(row_bytes) {
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

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
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

fn stable_signature(value: &str) -> u64 {
    stable_bytes(value.as_bytes())
}

fn stable_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

struct DeterministicRandom(u64);

impl DeterministicRandom {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0 = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d);
        self.0
    }

    fn range(&mut self, start: i32, end: i32) -> i32 {
        let width = u64::try_from(end.saturating_sub(start).max(1)).unwrap_or(1);
        start + i32::try_from(self.next() % width).unwrap_or_default()
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
        '.' => [0, 0, 0, 0, 0, 12, 12],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        ' ' => [0; 7],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

#[cfg(test)]
#[path = "pet_assets/tests.rs"]
mod tests;
