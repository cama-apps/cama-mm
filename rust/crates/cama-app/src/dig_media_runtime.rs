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
    let base = decode_image(source, MediaFormat::Png)?
        .resize_lanczos(PINNACLE_SIZE.0 as usize, PINNACLE_SIZE.1 as usize);
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
    let base = prepare_pinnacle_base(base, accent);
    match phase {
        2 => {
            let mut rendered = base;
            draw_pinnacle_atmosphere(&mut rendered, theme.effect, accent, highlight, 0.55, secret);
            Ok(RenderedMedia {
                bytes: encode_png(&rendered, PixelMode::Rgba),
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
            let durations = vec![95; 7]
                .into_iter()
                .chain(std::iter::once(1_500))
                .collect::<Vec<_>>();
            let mut frames = Vec::with_capacity(durations.len());
            for index in 0..durations.len() {
                let progress = index as f64 / 7.0;
                let eased = 0.5 - 0.5 * (progress * std::f64::consts::PI).cos();
                let brightness = 0.96 + eased * 0.06;
                let mut frame = apply_pinnacle_brightness(&base, brightness);
                draw_pinnacle_atmosphere(
                    &mut frame,
                    theme.effect,
                    accent,
                    highlight,
                    progress,
                    secret,
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

fn prepare_pinnacle_base(mut image: RgbaImage, accent: (u8, u8, u8)) -> RgbaImage {
    // Match Pillow's ImageEnhance.Color(base).enhance(0.82), followed by a
    // nine-percent blend toward the boss accent and ImageEnhance.Contrast
    // (1.07).  Keeping this work in linear per-channel arithmetic is also
    // important for source images that are not already 512x288.
    let mut luma_sum = 0_u64;
    let pixel_count = image.width.saturating_mul(image.height).max(1);
    for pixel in image.pixels.chunks_exact_mut(4) {
        let red = f64::from(pixel[0]);
        let green = f64::from(pixel[1]);
        let blue = f64::from(pixel[2]);
        let gray = 0.299 * red + 0.587 * green + 0.114 * blue;
        let colorized = [
            gray * 0.18 + red * 0.82,
            gray * 0.18 + green * 0.82,
            gray * 0.18 + blue * 0.82,
        ];
        let tinted = [
            colorized[0] * 0.91 + f64::from(accent.0) * 0.09,
            colorized[1] * 0.91 + f64::from(accent.1) * 0.09,
            colorized[2] * 0.91 + f64::from(accent.2) * 0.09,
        ];
        pixel[0] = round_channel(tinted[0]);
        pixel[1] = round_channel(tinted[1]);
        pixel[2] = round_channel(tinted[2]);
        luma_sum = luma_sum.saturating_add(
            (0.299 * tinted[0] + 0.587 * tinted[1] + 0.114 * tinted[2]).round() as u64,
        );
    }
    let mean = luma_sum as f64 / pixel_count as f64;
    for pixel in image.pixels.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            let contrasted = mean + 1.07 * (f64::from(*channel) - mean);
            *channel = round_channel(contrasted);
        }
    }
    image
}

fn apply_pinnacle_brightness(image: &RgbaImage, factor: f64) -> RgbaImage {
    let mut rendered = image.clone();
    for pixel in rendered.pixels.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            *channel = round_channel(f64::from(*channel) * factor);
        }
    }
    rendered
}

fn round_channel(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

fn draw_pinnacle_atmosphere(
    image: &mut RgbaImage,
    effect: &str,
    accent: (u8, u8, u8),
    highlight: (u8, u8, u8),
    progress: f64,
    secret: bool,
) {
    let eased = 0.5 - 0.5 * (progress * std::f64::consts::PI).cos();
    let (center, radius, peak_alpha, mote_count) = match effect {
        "crown_fire" => ((0.50, 0.29), 54, 50, 18),
        "crystal_choir" => ((0.50, 0.50), 64, 44, 12),
        "ghost_tunnel" => ((0.12 + 0.04 * eased, 0.62), 58, 30, 9),
        "fortress_siege" => ((0.50, 0.38), 78, 46, 22),
        "map_fold" => ((0.50, 0.50), 0, 0, 0),
        "heat_gear" => ((0.50, 0.46), 72, 58, 20),
        _ => return,
    };
    if radius != 0 {
        draw_soft_pinnacle_glow(
            image,
            (
                (image.width as f64 * center.0) as isize,
                (image.height as f64 * center.1) as isize,
            ),
            radius,
            accent,
            (f64::from(peak_alpha) * (0.55 + 0.45 * eased)) as u8,
        );
    }
    if mote_count != 0 {
        draw_pinnacle_motes(image, effect, accent, highlight, progress, mote_count);
    }
    match effect {
        "crystal_choir" => {
            let sparkle_points = [
                (0.19, 0.34),
                (0.31, 0.19),
                (0.45, 0.43),
                (0.58, 0.24),
                (0.71, 0.39),
                (0.83, 0.21),
            ];
            for (index, (x_ratio, y_ratio)) in sparkle_points.into_iter().enumerate() {
                let pulse =
                    0.5 + 0.5 * (progress * std::f64::consts::TAU + index as f64 * 1.7).sin();
                let alpha = 30 + (60.0 * pulse) as u8;
                let length = 3 + (3.0 * pulse) as isize;
                let x = (image.width as f64 * x_ratio) as isize;
                let y = (image.height as f64 * y_ratio) as isize;
                draw_line(
                    image,
                    (x - length, y),
                    (x + length, y),
                    [highlight.0, highlight.1, highlight.2, alpha],
                );
                draw_line(
                    image,
                    (x, y - length),
                    (x, y + length),
                    [highlight.0, highlight.1, highlight.2, alpha],
                );
            }
        }
        "ghost_tunnel" => {
            draw_soft_pinnacle_glow(
                image,
                (
                    (image.width as f64 * (0.88 - 0.04 * eased)) as isize,
                    (image.height as f64 * 0.46) as isize,
                ),
                48,
                highlight,
                18,
            );
            let drift = (10.0 * eased) as isize;
            for offset in [0_isize, 24, 49] {
                draw_arc(
                    image,
                    (-70 + offset + drift, 70 + offset),
                    (180 + offset + drift, image.height as isize + 75),
                    205.0,
                    326.0,
                    [highlight.0, highlight.1, highlight.2, 42],
                );
            }
        }
        "map_fold" => {
            let sweep_x = (-40.0 + (image.width as f64 + 80.0) * eased) as isize;
            draw_line(
                image,
                (sweep_x - 58, 0),
                (sweep_x + 26, image.height as isize),
                [highlight.0, highlight.1, highlight.2, 58],
            );
            draw_line(
                image,
                (sweep_x - 76, 0),
                (sweep_x + 8, image.height as isize),
                [accent.0, accent.1, accent.2, 30],
            );
            for (node_index, y) in [48_isize, 103, 158, 217].into_iter().enumerate() {
                let x = sweep_x - 42 + node_index as isize * 20;
                draw_ellipse(
                    image,
                    (x, y),
                    (3, 3),
                    [highlight.0, highlight.1, highlight.2, 70],
                    false,
                );
            }
        }
        "heat_gear" => {
            let cx = image.width as isize / 2;
            let cy = (image.height as f64 * 0.46) as isize;
            let spread = 52 + (8.0 * eased) as isize;
            for offset in [0_isize, 16] {
                draw_arc(
                    image,
                    (cx - spread - offset, cy - 28 - offset),
                    (cx + spread + offset, cy + 34 + offset),
                    202.0,
                    338.0,
                    [
                        highlight.0,
                        highlight.1,
                        highlight.2,
                        40_u8.saturating_sub(offset as u8),
                    ],
                );
            }
        }
        _ => {}
    }
    if secret {
        draw_soft_pinnacle_glow(
            image,
            (image.width as isize / 2, image.height as isize / 2),
            68,
            highlight,
            24 + (16.0 * eased) as u8,
        );
        draw_pinnacle_motes(
            image,
            &format!("{effect}:secret"),
            accent,
            highlight,
            progress,
            8,
        );
    }
}

fn draw_pinnacle_motes(
    image: &mut RgbaImage,
    seed_text: &str,
    accent: (u8, u8, u8),
    highlight: (u8, u8, u8),
    progress: f64,
    count: usize,
) {
    let mut rng = DeterministicRng::new(stable_hash(format!("pinnacle:{seed_text}").as_bytes()));
    let width = image.width as isize;
    let height = image.height as isize;
    for mote_index in 0..count {
        let x = rng.range(18, (width - 18).max(18));
        let start_y = rng.range(height / 3, height + 18);
        let travel = rng.range(height / 5, height / 2);
        let y = start_y - (travel as f64 * progress) as isize;
        if !(-5..=height + 5).contains(&y) {
            continue;
        }
        let radius = [1_isize, 1, 2, 2, 3][rng.range(0, 4) as usize];
        let alpha = rng.range(55, 105) as u8;
        let color = if mote_index % 4 == 0 {
            highlight
        } else {
            accent
        };
        draw_ellipse(
            image,
            (x, y),
            (radius, radius),
            [color.0, color.1, color.2, alpha],
            true,
        );
        if mote_index % 6 == 0 {
            draw_line(
                image,
                (x - radius - 2, y),
                (x + radius + 2, y),
                [highlight.0, highlight.1, highlight.2, alpha / 2],
            );
        }
    }
}

#[derive(Clone, Copy)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    const fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0xa076_1d64_78bd_642f,
        }
    }

    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_add(0xe703_7ed1_a0b4_28db)
            .rotate_left(17)
            ^ 0x8ebc_6af0_9c88_c6e3;
        self.state
            .wrapping_mul(0xd6e8_feb8_6659_fd93)
            .rotate_left(29)
    }

    fn range(&mut self, lower: isize, upper: isize) -> isize {
        if upper <= lower {
            return lower;
        }
        lower + (self.next() % u64::try_from(upper - lower + 1).unwrap_or(1)) as isize
    }
}

fn draw_soft_pinnacle_glow(
    image: &mut RgbaImage,
    center: (isize, isize),
    radius: usize,
    color: (u8, u8, u8),
    alpha: u8,
) {
    let mut glow = RgbaImage::new(image.width, image.height, [0, 0, 0, 0]).expect("fixed glow");
    draw_ellipse(
        &mut glow,
        center,
        (radius as isize, radius as isize),
        [color.0, color.1, color.2, alpha],
        true,
    );
    glow.blur(radius.saturating_div(3).max(1));
    image.alpha_composite(&glow);
}

fn draw_ellipse(
    image: &mut RgbaImage,
    center: (isize, isize),
    radii: (isize, isize),
    color: [u8; 4],
    filled: bool,
) {
    let (radius_x, radius_y) = (radii.0.max(1), radii.1.max(1));
    let min_y = center.1 - radius_y;
    let max_y = center.1 + radius_y;
    let min_x = center.0 - radius_x;
    let max_x = center.0 + radius_x;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = (x - center.0) as f64 / radius_x as f64;
            let dy = (y - center.1) as f64 / radius_y as f64;
            let distance = dx * dx + dy * dy;
            if filled {
                if distance <= 1.0 {
                    image.blend_pixel(x, y, color);
                }
            } else if (0.72..=1.0).contains(&distance) {
                image.blend_pixel(x, y, color);
            }
        }
    }
}

fn draw_line(image: &mut RgbaImage, start: (isize, isize), end: (isize, isize), color: [u8; 4]) {
    let (mut x0, mut y0) = start;
    let dx = (end.0 - x0).abs();
    let sx = if x0 < end.0 { 1 } else { -1 };
    let dy = -(end.1 - y0).abs();
    let sy = if y0 < end.1 { 1 } else { -1 };
    let mut error = dx + dy;
    loop {
        image.blend_pixel(x0, y0, color);
        if (x0, y0) == end {
            break;
        }
        let twice = 2 * error;
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

fn draw_arc(
    image: &mut RgbaImage,
    bounds_min: (isize, isize),
    bounds_max: (isize, isize),
    start_degrees: f64,
    end_degrees: f64,
    color: [u8; 4],
) {
    let center_x = (bounds_min.0 + bounds_max.0) as f64 / 2.0;
    let center_y = (bounds_min.1 + bounds_max.1) as f64 / 2.0;
    let radius_x = (bounds_max.0 - bounds_min.0).unsigned_abs() as f64 / 2.0;
    let radius_y = (bounds_max.1 - bounds_min.1).unsigned_abs() as f64 / 2.0;
    let steps = ((end_degrees - start_degrees).abs() as usize).max(1) * 2;
    let mut previous = None;
    for step in 0..=steps {
        let degrees = start_degrees + (end_degrees - start_degrees) * step as f64 / steps as f64;
        let radians = degrees.to_radians();
        let point = (
            (center_x + radius_x * radians.cos()) as isize,
            (center_y + radius_y * radians.sin()) as isize,
        );
        if let Some(previous) = previous {
            draw_line(image, previous, point, color);
        }
        previous = Some(point);
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
                let clamped = source.clamp(0, source_size as isize - 1) as usize;
                weights.push((clamped, weight));
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

    fn blend_pixel(&mut self, x: isize, y: isize, color: [u8; 4]) {
        let Ok(x) = usize::try_from(x) else {
            return;
        };
        let Ok(y) = usize::try_from(y) else {
            return;
        };
        if x >= self.width || y >= self.height {
            return;
        }
        let at = (y * self.width + x) * 4;
        let source_alpha = u32::from(color[3]);
        let destination_alpha = u32::from(self.pixels[at + 3]);
        if source_alpha == 0 {
            return;
        }
        if source_alpha == 255 && destination_alpha == 0 {
            self.pixels[at..at + 4].copy_from_slice(&color);
            return;
        }
        let inverse = 255 - source_alpha;
        let output_alpha = source_alpha + (destination_alpha * inverse + 127) / 255;
        for (channel, source) in color[..3].iter().enumerate() {
            let numerator = u32::from(*source) * source_alpha
                + u32::from(self.pixels[at + channel]) * destination_alpha * inverse / 255;
            self.pixels[at + channel] = ((numerator + output_alpha / 2) / output_alpha) as u8;
        }
        self.pixels[at + 3] = output_alpha as u8;
    }

    fn alpha_composite(&mut self, overlay: &Self) {
        if self.width != overlay.width || self.height != overlay.height {
            return;
        }
        for (index, pixel) in overlay.pixels.chunks_exact(4).enumerate() {
            let x = index % self.width;
            let y = index / self.width;
            self.blend_pixel(
                x as isize,
                y as isize,
                pixel.try_into().expect("RGBA pixel"),
            );
        }
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

    fn resize_lanczos(&self, width: usize, height: usize) -> Self {
        if self.width == width && self.height == height {
            return self.clone();
        }
        if width == 0 || height == 0 || self.width == 0 || self.height == 0 {
            return Self::new(width, height, [0, 0, 0, 0]).expect("bounded resize");
        }
        let x_weights = lanczos_weights(self.width, width);
        let y_weights = lanczos_weights(self.height, height);
        let mut resized = Self::new(width, height, [0, 0, 0, 0]).expect("bounded resize");
        for (y, y_weights_for_pixel) in y_weights.iter().enumerate().take(height) {
            for (x, x_weights_for_pixel) in x_weights.iter().enumerate().take(width) {
                let mut channels = [0.0_f64; 4];
                let mut total_weight = 0.0_f64;
                for &(source_y, y_weight) in y_weights_for_pixel {
                    for &(source_x, x_weight) in x_weights_for_pixel {
                        let weight = x_weight * y_weight;
                        let source = (source_y * self.width + source_x) * 4;
                        for (channel, value) in channels.iter_mut().enumerate() {
                            *value += f64::from(self.pixels[source + channel]) * weight;
                        }
                        total_weight += weight;
                    }
                }
                let target = (y * width + x) * 4;
                if total_weight.abs() > f64::EPSILON {
                    for (channel, value) in channels.iter().enumerate() {
                        resized.pixels[target + channel] = round_channel(*value / total_weight);
                    }
                }
            }
        }
        resized
    }

    fn blur(&mut self, radius: usize) {
        if radius == 0 || self.width == 0 || self.height == 0 {
            return;
        }
        // Three box passes are a close, bounded approximation to Pillow's
        // GaussianBlur while keeping native rendering independent of a large
        // image-processing dependency.
        for _ in 0..3 {
            self.box_blur(radius);
        }
    }

    fn box_blur(&mut self, radius: usize) {
        let mut horizontal = vec![0_u8; self.pixels.len()];
        for y in 0..self.height {
            let mut sums = [0_u32; 4];
            let mut count = 0_u32;
            let initial_end = (radius + 1).min(self.width);
            for x in 0..initial_end {
                let at = (y * self.width + x) * 4;
                for (channel, sum) in sums.iter_mut().enumerate() {
                    *sum += u32::from(self.pixels[at + channel]);
                }
                count += 1;
            }
            for x in 0..self.width {
                if x > radius {
                    let at = (y * self.width + x - radius - 1) * 4;
                    for (channel, sum) in sums.iter_mut().enumerate() {
                        *sum -= u32::from(self.pixels[at + channel]);
                    }
                    count -= 1;
                }
                let add_x = x + radius + 1;
                if add_x < self.width {
                    let at = (y * self.width + add_x) * 4;
                    for (channel, sum) in sums.iter_mut().enumerate() {
                        *sum += u32::from(self.pixels[at + channel]);
                    }
                    count += 1;
                }
                let at = (y * self.width + x) * 4;
                for (channel, sum) in sums.iter().enumerate() {
                    horizontal[at + channel] = ((*sum + count / 2) / count) as u8;
                }
            }
        }
        let original = std::mem::replace(&mut self.pixels, horizontal);
        let mut vertical = vec![0_u8; original.len()];
        for x in 0..self.width {
            let mut sums = [0_u32; 4];
            let mut count = 0_u32;
            let initial_end = (radius + 1).min(self.height);
            for y in 0..initial_end {
                let at = (y * self.width + x) * 4;
                for (channel, sum) in sums.iter_mut().enumerate() {
                    *sum += u32::from(self.pixels[at + channel]);
                }
                count += 1;
            }
            for y in 0..self.height {
                if y > radius {
                    let at = ((y - radius - 1) * self.width + x) * 4;
                    for (channel, sum) in sums.iter_mut().enumerate() {
                        *sum -= u32::from(self.pixels[at + channel]);
                    }
                    count -= 1;
                }
                let add_y = y + radius + 1;
                if add_y < self.height {
                    let at = (add_y * self.width + x) * 4;
                    for (channel, sum) in sums.iter_mut().enumerate() {
                        *sum += u32::from(self.pixels[at + channel]);
                    }
                    count += 1;
                }
                let at = (y * self.width + x) * 4;
                for (channel, sum) in sums.iter().enumerate() {
                    vertical[at + channel] = ((*sum + count / 2) / count) as u8;
                }
            }
        }
        self.pixels = vertical;
        drop(original);
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
