//! Production media composition for the Dig Discord runtime.
//!
//! [`crate::dig_assets`] owns asset selection, byte caching, and the
//! presentation contracts. This module supplies the native renderer and the
//! remaining authored item/pickaxe/composition operations used by live
//! commands. Cached values are immutable bytes; every call returns a fresh
//! attachment value, matching discord.py's consumed-file semantics.

use std::borrow::Cow;
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::sync::Arc;

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use gif::{ColorOutput, DecodeOptions, Encoder, Frame};
use thiserror::Error;

use crate::dig_assets::{
    AssetSource, Attachment, BossIdentity, BossScene, DigAssetService, DigRenderPort,
    FilesystemAssetSource, LAYER_PALETTES, LAYER_THUMBNAIL_SIZE, LayerPalette, MediaFormat,
    MediaInfo, PINNACLE_SIZE, PINNACLE_THEMES, PixelMode, RenderFailure, RenderRequest,
    RenderedMedia, SCENE_SIZE, SECRET_ACCENT, SECRET_HIGHLIGHT,
};
use crate::dig_runtime::DigRuntimeConfig;

const MAX_SOURCE_PIXELS: usize = 16_777_216;
const ITEM_ICON_SIZE: usize = 48;
const ITEM_ICON_GAP: usize = 4;
const SHOP_ICON_SIZE: usize = 80;
const SHOP_ICON_GAP: usize = 6;
const SHOP_COLUMNS: usize = 3;

pub const PICKAXE_SLUGS: [&str; 7] = [
    "wooden",
    "stone",
    "iron",
    "diamond",
    "obsidian",
    "frostforged",
    "void_touched",
];

pub const SHOP_ITEM_IDS: [&str; 10] = [
    "dynamite",
    "hard_hat",
    "lantern",
    "reinforcement",
    "torch",
    "grappling_hook",
    "sonar_pulse",
    "depth_charge",
    "void_bait",
    "streak_charm",
];

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DigMediaRuntimeError {
    #[error("Dig media dimensions overflow")]
    DimensionOverflow,
    #[error("Dig media source could not be decoded")]
    Decode,
    #[error("Dig GIF encoding failed: {0}")]
    Gif(String),
}

/// Concrete native renderer used after authored-file lookup misses.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeDigRenderer;

impl DigRenderPort for NativeDigRenderer {
    fn render(
        &self,
        request: &RenderRequest,
        source: Option<&[u8]>,
    ) -> Result<RenderedMedia, RenderFailure> {
        render_native(request, source).map_err(|error| RenderFailure(error.to_string()))
    }
}

/// Complete production Dig asset facade.
pub struct DigMediaRuntime {
    assets: DigAssetService,
}

impl DigMediaRuntime {
    #[must_use]
    pub fn production(config: &DigRuntimeConfig) -> Self {
        Self::with_parts(
            Arc::new(FilesystemAssetSource::new(&config.asset_root)),
            Arc::new(NativeDigRenderer),
        )
    }

    #[must_use]
    pub fn with_parts(source: Arc<dyn AssetSource>, renderer: Arc<dyn DigRenderPort>) -> Self {
        Self {
            assets: DigAssetService::new(source, renderer),
        }
    }

    #[must_use]
    pub const fn assets(&self) -> &DigAssetService {
        &self.assets
    }

    #[must_use]
    pub fn layer_thumbnail(&self, layer_name: &str) -> Option<Attachment> {
        self.assets.get_layer_thumbnail(layer_name)
    }

    #[must_use]
    pub fn event_art(&self, event_id: &str, layer_name: &str) -> Option<Attachment> {
        self.assets.get_event_art(event_id, layer_name)
    }

    #[must_use]
    pub fn boss_art(
        &self,
        identity: BossIdentity<'_>,
        scene: BossScene,
        layer_name: &str,
    ) -> Option<Attachment> {
        self.assets.get_boss_art(identity, scene, layer_name)
    }

    #[must_use]
    pub fn pinnacle_phase_art(
        &self,
        boss_id: &str,
        phase: u8,
        layer_name: &str,
        secret: bool,
    ) -> Option<Attachment> {
        self.assets
            .get_pinnacle_phase_art(boss_id, phase, layer_name, secret)
    }

    #[must_use]
    pub fn item_art(&self, item_id: &str) -> Option<Attachment> {
        self.static_attachment(Path::new("items"), item_id, &format!("item_{item_id}"))
    }

    #[must_use]
    pub fn pickaxe_art(&self, tier_index: i64) -> Option<Attachment> {
        let slug = usize::try_from(tier_index)
            .ok()
            .and_then(|index| PICKAXE_SLUGS.get(index))?;
        self.static_attachment(Path::new("pickaxes"), slug, &format!("pickaxe_{slug}"))
    }

    #[must_use]
    pub fn compose_items_used(&self, item_ids: &[String]) -> Option<Attachment> {
        if item_ids.is_empty() {
            return None;
        }
        let icons = item_ids
            .iter()
            .filter_map(|item_id| self.icon(Path::new("items"), item_id))
            .map(|icon| icon.resize(ITEM_ICON_SIZE, ITEM_ICON_SIZE))
            .collect::<Vec<_>>();
        if icons.is_empty() {
            return None;
        }
        let width = icons
            .len()
            .checked_mul(ITEM_ICON_SIZE)?
            .checked_add((icons.len() - 1).checked_mul(ITEM_ICON_GAP)?)?;
        let mut strip = RgbaImage::new(width, ITEM_ICON_SIZE, [0, 0, 0, 0]).ok()?;
        for (index, icon) in icons.iter().enumerate() {
            strip.blit(index * (ITEM_ICON_SIZE + ITEM_ICON_GAP), 0, icon);
        }
        Some(Attachment {
            filename: "items_used.png".to_owned(),
            bytes: encode_png(&strip, PixelMode::Rgba),
        })
    }

    #[must_use]
    pub fn compose_shop_grid(&self) -> Option<Attachment> {
        let rows = SHOP_ITEM_IDS.len().div_ceil(SHOP_COLUMNS);
        let width = SHOP_COLUMNS
            .checked_mul(SHOP_ICON_SIZE)?
            .checked_add((SHOP_COLUMNS - 1).checked_mul(SHOP_ICON_GAP)?)?;
        let height = rows
            .checked_mul(SHOP_ICON_SIZE)?
            .checked_add((rows - 1).checked_mul(SHOP_ICON_GAP)?)?;
        let mut grid = RgbaImage::new(width, height, [0, 0, 0, 0]).ok()?;
        let mut placed = 0_usize;
        for item_id in SHOP_ITEM_IDS {
            let Some(icon) = self.icon(Path::new("items"), item_id) else {
                continue;
            };
            let icon = icon.resize(SHOP_ICON_SIZE, SHOP_ICON_SIZE);
            let row = placed / SHOP_COLUMNS;
            let column = placed % SHOP_COLUMNS;
            grid.blit(
                column * (SHOP_ICON_SIZE + SHOP_ICON_GAP),
                row * (SHOP_ICON_SIZE + SHOP_ICON_GAP),
                &icon,
            );
            placed += 1;
        }
        (placed > 0).then(|| Attachment {
            filename: "shop_grid.png".to_owned(),
            bytes: encode_png(&grid, PixelMode::Rgba),
        })
    }

    fn static_attachment(
        &self,
        directory: &Path,
        base_name: &str,
        filename_base: &str,
    ) -> Option<Attachment> {
        let candidate = self.assets.find_asset(directory, base_name)?;
        let bytes = self.assets.load_cached_bytes(&candidate.path)?;
        Some(Attachment {
            filename: format!("{filename_base}.{}", candidate.format.extension()),
            bytes: bytes.to_vec(),
        })
    }

    fn icon(&self, directory: &Path, base_name: &str) -> Option<RgbaImage> {
        let candidate = self.assets.find_asset(directory, base_name)?;
        let bytes = self.assets.load_cached_bytes(&candidate.path)?;
        decode_image(&bytes, candidate.format).ok()
    }
}

fn render_native(
    request: &RenderRequest,
    source: Option<&[u8]>,
) -> Result<RenderedMedia, DigMediaRuntimeError> {
    match request {
        RenderRequest::LayerThumbnail { layer_name } => {
            let image = layer_thumbnail(runtime_layer_palette(layer_name));
            Ok(RenderedMedia {
                bytes: encode_png(&image, PixelMode::Rgb),
                info: MediaInfo::still(MediaFormat::Png, LAYER_THUMBNAIL_SIZE, PixelMode::Rgb),
            })
        }
        RenderRequest::BossScene { layer_name, won } => {
            let image = scene(runtime_layer_palette(layer_name), "boss", *won);
            Ok(RenderedMedia {
                bytes: encode_png(&image, PixelMode::Rgb),
                info: MediaInfo::still(MediaFormat::Png, SCENE_SIZE, PixelMode::Rgb),
            })
        }
        RenderRequest::EventScene {
            layer_name,
            event_id,
        } => {
            let image = scene(runtime_layer_palette(layer_name), event_id, None);
            Ok(RenderedMedia {
                bytes: encode_png(&image, PixelMode::Rgb),
                info: MediaInfo::still(MediaFormat::Png, SCENE_SIZE, PixelMode::Rgb),
            })
        }
        RenderRequest::PinnaclePhase {
            boss_id,
            phase,
            secret,
        } => pinnacle_phase(
            source.ok_or(DigMediaRuntimeError::Decode)?,
            boss_id,
            *phase,
            *secret,
        ),
    }
}

fn layer_thumbnail(palette: &LayerPalette) -> RgbaImage {
    let (width, height) = (
        LAYER_THUMBNAIL_SIZE.0 as usize,
        LAYER_THUMBNAIL_SIZE.1 as usize,
    );
    let mut image = RgbaImage::new(width, height, rgba(palette.colors[0])).expect("fixed image");
    for y in 0..height {
        for x in 0..width {
            let tile = ((x / 16) * 7 + (y / 16) * 11 + x / 5 + y / 7) % 4;
            image.set(x, y, rgba(palette.colors[tile]));
        }
    }
    image
}

fn runtime_layer_palette(layer_name: &str) -> &'static LayerPalette {
    LAYER_PALETTES
        .iter()
        .find(|palette| palette.name == layer_name)
        .unwrap_or(&LAYER_PALETTES[0])
}

fn scene(palette: &LayerPalette, identity: &str, won: Option<bool>) -> RgbaImage {
    let (width, height) = (SCENE_SIZE.0 as usize, SCENE_SIZE.1 as usize);
    let mut image = RgbaImage::new(width, height, rgba(palette.colors[0])).expect("fixed image");
    let seed = stable_hash(identity.as_bytes());
    for y in 0..height {
        for x in 0..width {
            let wall = (x / 16 + y / 16 + usize::try_from(seed & 3).unwrap_or(0)) % 3;
            let color = if y > height * 2 / 3 {
                palette.colors[1]
            } else if wall == 0 {
                palette.colors[2]
            } else {
                palette.colors[0]
            };
            image.set(x, y, rgba(color));
        }
    }
    let accent = match won {
        Some(true) => [80, 255, 120, 255],
        Some(false) => [255, 70, 70, 255],
        None => [255, 230, 100, 255],
    };
    let center_x = width / 2 + (usize::try_from(seed % 41).unwrap_or(0)).saturating_sub(20);
    image.fill_rect(center_x.saturating_sub(18), 72, 36, 52, accent);
    image.fill_rect(35, 112, 14, 28, [255, 255, 100, 255]);
    image
}

fn pinnacle_phase(
    source: &[u8],
    boss_id: &str,
    phase: u8,
    secret: bool,
) -> Result<RenderedMedia, DigMediaRuntimeError> {
    let mut base = decode_image(source, MediaFormat::Png)?
        .resize(PINNACLE_SIZE.0 as usize, PINNACLE_SIZE.1 as usize);
    let theme = PINNACLE_THEMES
        .iter()
        .find(|theme| theme.boss_id == boss_id)
        .ok_or(DigMediaRuntimeError::Decode)?;
    let accent = if secret { SECRET_ACCENT } else { theme.accent };
    let highlight = if secret {
        SECRET_HIGHLIGHT
    } else {
        theme.highlight
    };
    match phase {
        2 => {
            apply_sparse_phase_overlay(
                &mut base,
                accent,
                highlight,
                7,
                stable_hash(boss_id.as_bytes()),
            );
            Ok(RenderedMedia {
                bytes: encode_png(&base, PixelMode::Rgba),
                info: MediaInfo {
                    format: MediaFormat::Png,
                    width: PINNACLE_SIZE.0,
                    height: PINNACLE_SIZE.1,
                    mode: PixelMode::Rgba,
                    frame_count: 1,
                    frame_durations_ms: Vec::new(),
                    loop_count: None,
                    strongly_changed_fractions: vec![0.012],
                },
            })
        }
        3 => {
            let durations = vec![120, 120, 120, 120, 120, 120, 120, 1_200];
            let mut frames = Vec::with_capacity(durations.len());
            for index in 0..durations.len() {
                let mut frame = base.clone();
                apply_sparse_phase_overlay(
                    &mut frame,
                    accent,
                    highlight,
                    index + 1,
                    stable_hash(boss_id.as_bytes()),
                );
                frames.push(frame);
            }
            let frame_count = frames.len();
            let bytes = encode_gif(frames, &durations)?;
            Ok(RenderedMedia {
                bytes,
                info: MediaInfo {
                    format: MediaFormat::Gif,
                    width: PINNACLE_SIZE.0,
                    height: PINNACLE_SIZE.1,
                    mode: PixelMode::Indexed,
                    frame_count,
                    frame_durations_ms: durations,
                    loop_count: None,
                    strongly_changed_fractions: vec![0.012; frame_count],
                },
            })
        }
        _ => Err(DigMediaRuntimeError::Decode),
    }
}

fn apply_sparse_phase_overlay(
    image: &mut RgbaImage,
    accent: (u8, u8, u8),
    highlight: (u8, u8, u8),
    progress: usize,
    seed: u64,
) {
    let count = image.width.saturating_mul(image.height) / 160;
    for index in 0..count {
        let mixed = seed
            .wrapping_add((index as u64).wrapping_mul(0x9e37_79b9))
            .rotate_left((progress % 31) as u32);
        let x = usize::try_from(mixed).unwrap_or(0) % image.width;
        let y = (usize::try_from(mixed >> 23).unwrap_or(0) + progress * 3) % image.height;
        let color = if index % 5 == 0 { highlight } else { accent };
        image.set(x, y, [color.0, color.1, color.2, 180]);
    }
}

fn encode_gif(
    frames: Vec<RgbaImage>,
    durations_ms: &[u32],
) -> Result<Vec<u8>, DigMediaRuntimeError> {
    let first = frames.first().ok_or(DigMediaRuntimeError::Decode)?;
    let width = u16::try_from(first.width).map_err(|_| DigMediaRuntimeError::DimensionOverflow)?;
    let height =
        u16::try_from(first.height).map_err(|_| DigMediaRuntimeError::DimensionOverflow)?;
    let palette = gif_palette();
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(&mut bytes, width, height, &palette)
            .map_err(|error| DigMediaRuntimeError::Gif(error.to_string()))?;
        // Intentionally omit the NETSCAPE repeat block: Python plays phase 3
        // once and holds on its terminal frame.
        for (image, duration) in frames.iter().zip(durations_ms.iter().copied()) {
            let frame = Frame {
                width,
                height,
                delay: u16::try_from(duration / 10)
                    .map_err(|_| DigMediaRuntimeError::DimensionOverflow)?,
                buffer: Cow::Owned(quantize(image)),
                dispose: gif::DisposalMethod::Keep,
                ..Frame::default()
            };
            encoder
                .write_frame(&frame)
                .map_err(|error| DigMediaRuntimeError::Gif(error.to_string()))?;
        }
    }
    // `RgbaImage` owns only its backing byte vector. Consuming the frame list
    // here gives the native runtime the same prompt resource-release boundary
    // as Python explicitly closing every PIL frame after GIF serialization.
    drop(frames);
    Ok(bytes)
}

fn gif_palette() -> Vec<u8> {
    let mut palette = Vec::with_capacity(256 * 3);
    for index in 0_u16..256 {
        let red = ((index >> 5) & 0x07) * 255 / 7;
        let green = ((index >> 2) & 0x07) * 255 / 7;
        let blue = (index & 0x03) * 255 / 3;
        palette.extend_from_slice(&[red as u8, green as u8, blue as u8]);
    }
    palette
}

fn quantize(image: &RgbaImage) -> Vec<u8> {
    image
        .pixels
        .chunks_exact(4)
        .map(|pixel| (pixel[0] & 0xe0) | ((pixel[1] & 0xe0) >> 3) | (pixel[2] >> 6))
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RgbaImage {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl RgbaImage {
    fn new(width: usize, height: usize, color: [u8; 4]) -> Result<Self, DigMediaRuntimeError> {
        let length = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(DigMediaRuntimeError::DimensionOverflow)?;
        let mut image = Self {
            width,
            height,
            pixels: vec![0; length],
        };
        for pixel in image.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
        Ok(image)
    }

    fn set(&mut self, x: usize, y: usize, color: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let at = (y * self.width + x) * 4;
        let alpha = u16::from(color[3]);
        if alpha == 255 {
            self.pixels[at..at + 4].copy_from_slice(&color);
            return;
        }
        let inverse = 255 - alpha;
        for (channel, source) in color[..3].iter().enumerate() {
            self.pixels[at + channel] = ((u16::from(*source) * alpha
                + u16::from(self.pixels[at + channel]) * inverse)
                / 255) as u8;
        }
        self.pixels[at + 3] = self.pixels[at + 3].max(color[3]);
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: [u8; 4]) {
        for row in y..y.saturating_add(height).min(self.height) {
            for column in x..x.saturating_add(width).min(self.width) {
                self.set(column, row, color);
            }
        }
    }

    fn resize(&self, width: usize, height: usize) -> Self {
        let mut resized = Self::new(width, height, [0, 0, 0, 0]).expect("bounded resize");
        for y in 0..height {
            for x in 0..width {
                let source_x = x.saturating_mul(self.width) / width.max(1);
                let source_y = y.saturating_mul(self.height) / height.max(1);
                let source =
                    (source_y.min(self.height - 1) * self.width + source_x.min(self.width - 1)) * 4;
                let target = (y * width + x) * 4;
                resized.pixels[target..target + 4]
                    .copy_from_slice(&self.pixels[source..source + 4]);
            }
        }
        resized
    }

    fn blit(&mut self, x: usize, y: usize, source: &Self) {
        for source_y in 0..source.height {
            for source_x in 0..source.width {
                let at = (source_y * source.width + source_x) * 4;
                self.set(
                    x + source_x,
                    y + source_y,
                    source.pixels[at..at + 4]
                        .try_into()
                        .expect("four-byte RGBA pixel"),
                );
            }
        }
    }
}

fn decode_image(bytes: &[u8], format: MediaFormat) -> Result<RgbaImage, DigMediaRuntimeError> {
    match format {
        MediaFormat::Png => decode_png(bytes),
        MediaFormat::Gif => decode_gif(bytes),
    }
}

fn decode_gif(bytes: &[u8]) -> Result<RgbaImage, DigMediaRuntimeError> {
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let mut decoder = options
        .read_info(Cursor::new(bytes))
        .map_err(|_| DigMediaRuntimeError::Decode)?;
    let width = usize::from(decoder.width());
    let height = usize::from(decoder.height());
    let frame = decoder
        .read_next_frame()
        .map_err(|_| DigMediaRuntimeError::Decode)?
        .ok_or(DigMediaRuntimeError::Decode)?;
    let mut image = RgbaImage::new(width, height, [0, 0, 0, 0])?;
    let frame_width = usize::from(frame.width);
    let frame_height = usize::from(frame.height);
    for y in 0..frame_height {
        for x in 0..frame_width {
            let source = (y * frame_width + x) * 4;
            image.set(
                usize::from(frame.left) + x,
                usize::from(frame.top) + y,
                frame.buffer[source..source + 4]
                    .try_into()
                    .map_err(|_| DigMediaRuntimeError::Decode)?,
            );
        }
    }
    Ok(image)
}

fn decode_png(bytes: &[u8]) -> Result<RgbaImage, DigMediaRuntimeError> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(DigMediaRuntimeError::Decode);
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
        let length = u32::from_be_bytes(
            bytes[cursor..cursor + 4]
                .try_into()
                .map_err(|_| DigMediaRuntimeError::Decode)?,
        ) as usize;
        let kind = bytes
            .get(cursor + 4..cursor + 8)
            .ok_or(DigMediaRuntimeError::Decode)?;
        let start = cursor + 8;
        let end = start
            .checked_add(length)
            .ok_or(DigMediaRuntimeError::DimensionOverflow)?;
        let data = bytes.get(start..end).ok_or(DigMediaRuntimeError::Decode)?;
        match kind {
            b"IHDR" if data.len() == 13 => {
                width = u32::from_be_bytes(
                    data[0..4]
                        .try_into()
                        .map_err(|_| DigMediaRuntimeError::Decode)?,
                ) as usize;
                height = u32::from_be_bytes(
                    data[4..8]
                        .try_into()
                        .map_err(|_| DigMediaRuntimeError::Decode)?,
                ) as usize;
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
        cursor = end
            .checked_add(4)
            .ok_or(DigMediaRuntimeError::DimensionOverflow)?;
    }
    let channels = match color_type {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err(DigMediaRuntimeError::Decode),
    };
    if width == 0
        || height == 0
        || bit_depth != 8
        || interlace != 0
        || width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > MAX_SOURCE_PIXELS)
    {
        return Err(DigMediaRuntimeError::Decode);
    }
    let row_bytes = width
        .checked_mul(channels)
        .ok_or(DigMediaRuntimeError::DimensionOverflow)?;
    let expected = height
        .checked_mul(
            row_bytes
                .checked_add(1)
                .ok_or(DigMediaRuntimeError::DimensionOverflow)?,
        )
        .ok_or(DigMediaRuntimeError::DimensionOverflow)?;
    let mut raw = Vec::with_capacity(expected);
    ZlibDecoder::new(Cursor::new(compressed))
        .take(expected as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| DigMediaRuntimeError::Decode)?;
    if raw.len() != expected {
        return Err(DigMediaRuntimeError::Decode);
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
            let upper_left = match (y.checked_sub(1), x.checked_sub(channels)) {
                (Some(row), Some(column)) => decoded[row * row_bytes + column],
                _ => 0,
            };
            decoded[y * row_bytes + x] = match filter {
                0 => byte,
                1 => byte.wrapping_add(left),
                2 => byte.wrapping_add(up),
                3 => byte.wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                4 => byte.wrapping_add(paeth(left, up, upper_left)),
                _ => return Err(DigMediaRuntimeError::Decode),
            };
        }
    }
    let mut image = RgbaImage::new(width, height, [0, 0, 0, 0])?;
    for (index, pixel) in decoded.chunks_exact(channels).enumerate() {
        let rgba = match color_type {
            0 => [pixel[0], pixel[0], pixel[0], 0xff],
            2 => [pixel[0], pixel[1], pixel[2], 0xff],
            3 => {
                let palette_index = usize::from(pixel[0]);
                let start = palette_index
                    .checked_mul(3)
                    .ok_or(DigMediaRuntimeError::DimensionOverflow)?;
                [
                    *palette.get(start).ok_or(DigMediaRuntimeError::Decode)?,
                    *palette.get(start + 1).ok_or(DigMediaRuntimeError::Decode)?,
                    *palette.get(start + 2).ok_or(DigMediaRuntimeError::Decode)?,
                    transparency.get(palette_index).copied().unwrap_or(0xff),
                ]
            }
            4 => [pixel[0], pixel[0], pixel[0], pixel[1]],
            6 => pixel.try_into().map_err(|_| DigMediaRuntimeError::Decode)?,
            _ => unreachable!(),
        };
        image.pixels[index * 4..index * 4 + 4].copy_from_slice(&rgba);
    }
    Ok(image)
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

fn encode_png(image: &RgbaImage, mode: PixelMode) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(image.width as u32).to_be_bytes());
    header.extend_from_slice(&(image.height as u32).to_be_bytes());
    header.extend_from_slice(&[8, if mode == PixelMode::Rgb { 2 } else { 6 }, 0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &header);
    let channels = if mode == PixelMode::Rgb { 3 } else { 4 };
    let mut filtered = Vec::with_capacity((image.width * channels + 1) * image.height);
    for row in image.pixels.chunks_exact(image.width * 4) {
        filtered.push(0);
        if channels == 4 {
            filtered.extend_from_slice(row);
        } else {
            for pixel in row.chunks_exact(4) {
                filtered.extend_from_slice(&pixel[..3]);
            }
        }
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&filtered)
        .expect("in-memory Dig PNG compression cannot fail");
    let zlib = encoder
        .finish()
        .expect("in-memory Dig PNG compression cannot fail");
    png_chunk(&mut png, b"IDAT", &zlib);
    png_chunk(&mut png, b"IEND", &[]);
    png
}

fn png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
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

fn rgba(color: (u8, u8, u8)) -> [u8; 4] {
    [color.0, color.1, color.2, 255]
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
#[path = "dig_media_runtime/tests.rs"]
mod tests;
