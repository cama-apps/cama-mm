//! Wheel and explosion GIF rendering for `/gamba`.
//!
//! The settlement policy chooses the landed wedge before anything is drawn;
//! this module only paints that already-resolved presentation into GIF bytes,
//! so it cannot influence money or outcome selection.

use std::borrow::Cow;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use fontdue::Font;
use gif::{Encoder, Frame, Repeat};

use crate::font_assets::{DejaVuFace, dejavu_font};
use crate::wheel::{WheelValue, WheelWedge};

/// One rendered wheel animation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedWheelGif {
    pub bytes: Vec<u8>,
    /// Whether the bytes came from the bounded completed-render cache.
    pub cache_hit: bool,
}

const WHEEL_MEDIA_SIZE: u16 = 500;
/// Keep the production GIF contract in one place.  Python's wheel renderer
/// uses 70 frames: four acceleration/deceleration bands, a creep phase, then
/// two terminal hold frames.  GIF stores delays in centiseconds, so the
/// millisecond values below are converted at the encoder boundary.
const WHEEL_MEDIA_FRAME_COUNT: usize = 70;
const EXPLOSION_MEDIA_FRAME_COUNT: usize = 56;
const WHEEL_MEDIA_UPLOAD_LIMIT: usize = 4 * 1024 * 1024;
#[cfg(test)]
const EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET: usize = 500_000;
const MAX_WHEEL_LABEL_SPRITES: usize = 128;
const MAX_CACHED_WHEEL_ATTACHMENTS: usize = 4;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CachedWheelValue {
    Numeric(i64),
    Mechanic(&'static str),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WheelAttachmentCacheKey {
    wedges: Vec<(String, CachedWheelValue, &'static str)>,
    target_index: usize,
    golden: bool,
    display_name: Option<String>,
}

impl WheelAttachmentCacheKey {
    fn new(
        wedges: &[WheelWedge],
        target_index: usize,
        golden: bool,
        display_name: Option<&str>,
    ) -> Self {
        Self {
            wedges: wedges
                .iter()
                .map(|wedge| {
                    let value = match wedge.value {
                        WheelValue::Numeric(value) => CachedWheelValue::Numeric(value),
                        WheelValue::Mechanic(mechanic) => {
                            CachedWheelValue::Mechanic(mechanic.code())
                        }
                    };
                    (wedge.label.clone(), value, wedge.color)
                })
                .collect(),
            target_index,
            golden,
            display_name: display_name.map(str::to_owned),
        }
    }
}

#[derive(Debug, Default)]
struct WheelAttachmentCache {
    entries: BTreeMap<WheelAttachmentCacheKey, Vec<u8>>,
    order: VecDeque<WheelAttachmentCacheKey>,
}

impl WheelAttachmentCache {
    fn get(&mut self, key: &WheelAttachmentCacheKey) -> Option<Vec<u8>> {
        let attachment = self.entries.get(key)?.clone();
        if let Some(position) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(position);
        }
        self.order.push_back(key.clone());
        Some(attachment)
    }

    fn insert(&mut self, key: WheelAttachmentCacheKey, attachment: Vec<u8>) {
        if self.entries.contains_key(&key) {
            if let Some(position) = self.order.iter().position(|candidate| candidate == &key) {
                self.order.remove(position);
            }
        } else if self.entries.len() >= MAX_CACHED_WHEEL_ATTACHMENTS
            && let Some(evicted) = self.order.pop_front()
        {
            self.entries.remove(&evicted);
        }
        self.entries.insert(key.clone(), attachment);
        self.order.push_back(key);
    }
}

fn wheel_frame_delay_ms(frame_index: usize) -> u64 {
    match frame_index {
        0..=13 => 30,
        14..=27 => 45,
        28..=41 => 70,
        42..=57 => 110,
        58 => 180,
        59..=67 => 180 + (frame_index as u64 - 58) * 15,
        _ => 60_000,
    }
}

fn explosion_frame_delay_ms(frame_index: usize) -> u64 {
    match frame_index {
        0..=13 => 50,
        14..=23 => 60 + (frame_index as u64 - 14) * 10,
        24..=27 => 60,
        28..=41 => 80,
        42..=54 => 100,
        _ => 60_000,
    }
}

fn gif_delay_centiseconds(milliseconds: u64) -> u16 {
    // GIF stores centiseconds. Pillow (the Python implementation) truncates
    // sub-centisecond durations when it writes the control extension, so use
    // the same floor conversion rather than rounding 45ms up to 50ms.
    u16::try_from((milliseconds / 10).min(u64::from(u16::MAX))).unwrap_or(u16::MAX)
}

/// Render a compact, deterministic wheel animation for the same attachment
/// lifecycle as Python's `create_wheel_gif`. Completed renders are kept in a
/// small LRU cache because the same wheel is often drawn twice per spin.
pub fn render_wheel_gif(
    wedges: &[WheelWedge],
    target_index: usize,
    golden: bool,
    display_name: Option<&str>,
) -> Result<RenderedWheelGif, String> {
    static CACHE: OnceLock<Mutex<WheelAttachmentCache>> = OnceLock::new();
    let key = WheelAttachmentCacheKey::new(wedges, target_index, golden, display_name);
    let cache = CACHE.get_or_init(|| Mutex::new(WheelAttachmentCache::default()));
    if let Some(bytes) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        return Ok(RenderedWheelGif {
            bytes,
            cache_hit: true,
        });
    }
    let bytes = render_wheel_gif_uncached(wedges, target_index, golden, display_name)?;
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key, bytes.clone());
    Ok(RenderedWheelGif {
        bytes,
        cache_hit: false,
    })
}
fn render_wheel_gif_uncached(
    wedges: &[WheelWedge],
    target_index: usize,
    golden: bool,
    display_name: Option<&str>,
) -> Result<Vec<u8>, String> {
    if wedges.is_empty() {
        return Err("cannot render an empty wheel".to_owned());
    }
    let mut palette = vec![
        [30, 30, 35],                                    // Python wheel background
        [255, 215, 0],                                   // gold outline/glow
        [231, 76, 60],                                   // pointer
        if golden { [58, 40, 0] } else { [44, 62, 80] }, // hub
        [0, 0, 0],                                       // label shadow
        [255, 255, 255],                                 // label/divider text
        [55, 190, 55],                                   // terminal prompt
        [55, 90, 55],                                    // dim matrix/status text
        [255, 255, 0],                                   // selected wedge outline
        [160, 160, 160],                                 // font edge gray
        [220, 220, 220],                                 // font edge white
        [150, 127, 0],                                   // font edge gold
        [30, 110, 30],                                   // font edge green
    ];
    let mut wedge_indices = Vec::with_capacity(wedges.len());
    let mut bright_wedge_indices = Vec::with_capacity(wedges.len());
    for wedge in wedges {
        let rgb = parse_hex_rgb(wedge.color).unwrap_or([80, 80, 80]);
        let index = palette
            .iter()
            .position(|candidate| candidate == &rgb)
            .unwrap_or_else(|| {
                palette.push(rgb);
                palette.len() - 1
            });
        wedge_indices.push(u8::try_from(index).map_err(|_| "wheel palette overflow".to_owned())?);
        let bright = rgb.map(|channel| channel.saturating_add(120));
        let bright_index = palette
            .iter()
            .position(|candidate| candidate == &bright)
            .unwrap_or_else(|| {
                palette.push(bright);
                palette.len() - 1
            });
        bright_wedge_indices
            .push(u8::try_from(bright_index).map_err(|_| "wheel palette overflow".to_owned())?);
    }
    let palette_bytes = palette
        .iter()
        .flat_map(|rgb| rgb.iter().copied())
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(
            &mut bytes,
            WHEEL_MEDIA_SIZE,
            WHEEL_MEDIA_SIZE,
            &palette_bytes,
        )
        .map_err(|error| format!("wheel GIF encoder: {error}"))?;
        encoder
            .set_repeat(Repeat::Finite(1))
            .map_err(|error| format!("wheel GIF repeat: {error}"))?;
        let label_atlas = {
            static SPRITES: OnceLock<Mutex<WheelLabelSpriteCache>> = OnceLock::new();
            let mut sprite_cache = SPRITES
                .get_or_init(|| Mutex::new(WheelLabelSpriteCache::default()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            build_wheel_label_atlas(wedges, &mut sprite_cache)
        };
        for frame_index in 0..WHEEL_MEDIA_FRAME_COUNT {
            let mut pixels =
                vec![0_u8; usize::from(WHEEL_MEDIA_SIZE) * usize::from(WHEEL_MEDIA_SIZE)];
            draw_wheel_frame(
                &mut pixels,
                &wedge_indices,
                &bright_wedge_indices,
                &label_atlas,
                target_index,
                frame_index,
                golden,
                display_name,
            );
            let mut frame = Frame {
                width: WHEEL_MEDIA_SIZE,
                height: WHEEL_MEDIA_SIZE,
                delay: gif_delay_centiseconds(wheel_frame_delay_ms(frame_index)),
                buffer: Cow::Owned(pixels),
                ..Frame::default()
            };
            frame.dispose = gif::DisposalMethod::Keep;
            encoder
                .write_frame(&frame)
                .map_err(|error| format!("wheel GIF frame: {error}"))?;
        }
    }
    if bytes.len() >= WHEEL_MEDIA_UPLOAD_LIMIT {
        return Err("wheel GIF exceeded Discord's 4 MiB upload limit".to_owned());
    }
    Ok(bytes)
}

/// Render the explosion branch as a real GIF attachment.  The short radial
/// animation is intentionally self-contained: Discord upload failures can
/// fall back to the text response without touching the settled wallet state.
pub fn render_explosion_gif() -> Result<Vec<u8>, String> {
    static GIF: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
    GIF.get_or_init(render_explosion_gif_uncached).clone()
}

fn render_explosion_gif_uncached() -> Result<Vec<u8>, String> {
    let palette: &[[u8; 3]] = &[
        [5, 5, 10],
        [70, 8, 8],
        [180, 20, 10],
        [240, 90, 10],
        [255, 210, 25],
        [255, 255, 235],
    ];
    let palette_bytes = palette
        .iter()
        .flat_map(|rgb| rgb.iter().copied())
        .collect::<Vec<_>>();
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(
            &mut bytes,
            WHEEL_MEDIA_SIZE,
            WHEEL_MEDIA_SIZE,
            &palette_bytes,
        )
        .map_err(|error| format!("explosion GIF encoder: {error}"))?;
        encoder
            .set_repeat(Repeat::Finite(1))
            .map_err(|error| format!("explosion GIF repeat: {error}"))?;
        for frame_index in 0..EXPLOSION_MEDIA_FRAME_COUNT {
            let mut pixels =
                vec![0_u8; usize::from(WHEEL_MEDIA_SIZE) * usize::from(WHEEL_MEDIA_SIZE)];
            draw_explosion_frame(&mut pixels, frame_index);
            let mut frame = Frame {
                width: WHEEL_MEDIA_SIZE,
                height: WHEEL_MEDIA_SIZE,
                delay: gif_delay_centiseconds(explosion_frame_delay_ms(frame_index)),
                buffer: Cow::Owned(pixels),
                ..Frame::default()
            };
            frame.dispose = gif::DisposalMethod::Keep;
            encoder
                .write_frame(&frame)
                .map_err(|error| format!("explosion GIF frame: {error}"))?;
        }
    }
    if bytes.len() >= WHEEL_MEDIA_UPLOAD_LIMIT {
        return Err("explosion GIF exceeded Discord's 4 MiB upload limit".to_owned());
    }
    Ok(bytes)
}

/// Return the bounded contact-sheet dimensions used by the Python renderer
/// when it builds one representative palette for all explosion phases.  The
/// Rust renderer's fixed six-color palette is already shared by every frame,
/// but retaining this policy seam makes the memory bound explicit and keeps a
/// future richer palette implementation from accidentally materializing a
/// 500x500x56 RGB sheet.
#[cfg(test)]
fn explosion_palette_sample_size(frame_size: (usize, usize), frame_count: usize) -> (usize, usize) {
    let source_pixels = frame_size
        .0
        .saturating_mul(frame_size.1)
        .saturating_mul(frame_count.max(1));
    let scale =
        ((source_pixels as f64 / EXPLOSION_PALETTE_SAMPLE_PIXEL_BUDGET as f64).sqrt()).max(1.0);
    (
        ((frame_size.0 as f64 / scale) as usize).max(1),
        ((frame_size.1 as f64 / scale) as usize).max(1),
    )
}

fn parse_hex_rgb(color: &str) -> Option<[u8; 3]> {
    let value = color.strip_prefix('#').unwrap_or(color);
    if value.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}

#[derive(Clone, Debug)]
struct WheelLabelSprite {
    width: i32,
    height: i32,
    mask: Vec<u8>,
}

#[derive(Clone, Debug)]
struct WheelLabelAtlas {
    sprites: Vec<WheelLabelSprite>,
    wedge_sprite_indices: Vec<usize>,
}

/// The shared DejaVu faces; a missing face falls back to the bitmap glyphs.
fn wheel_font(bold: bool) -> Option<&'static Font> {
    dejavu_font(if bold {
        DejaVuFace::SansBold
    } else {
        DejaVuFace::SansMono
    })
}

fn render_font_mask(text: &str, pixel_size: f32, bold: bool) -> Option<WheelLabelSprite> {
    let font = wheel_font(bold)?;
    let baseline = pixel_size.ceil() as i32 + 3;
    let mut pen_x = 2.0_f32;
    let mut glyphs = Vec::new();
    for character in text.chars() {
        let (metrics, bitmap) = font.rasterize(character, pixel_size);
        let x = pen_x.round() as i32 + metrics.xmin;
        let y = baseline - metrics.ymin - i32::try_from(metrics.height).ok()?;
        pen_x += metrics.advance_width;
        glyphs.push((x, y, metrics, bitmap));
    }
    let width = (pen_x.ceil() as i32 + 2).max(1);
    let height = (pixel_size * 1.45).ceil() as i32 + 4;
    let mut mask = vec![0_u8; usize::try_from(width.checked_mul(height)?).ok()?];
    for (origin_x, origin_y, metrics, bitmap) in glyphs {
        for glyph_y in 0..metrics.height {
            for glyph_x in 0..metrics.width {
                let alpha = bitmap[glyph_y * metrics.width + glyph_x];
                if alpha == 0 {
                    continue;
                }
                let x = origin_x + i32::try_from(glyph_x).ok()?;
                let y = origin_y + i32::try_from(glyph_y).ok()?;
                if (0..width).contains(&x) && (0..height).contains(&y) {
                    mask[usize::try_from(y * width + x).ok()?] = alpha;
                }
            }
        }
    }
    Some(WheelLabelSprite {
        width,
        height,
        mask,
    })
}

/// Bounded process-local sprite cache used while composing wheel media.
///
/// Pillow's implementation uses an LRU cache keyed by the measured label
/// geometry. Rust keeps the same policy with compact 5x7 masks instead of
/// font-backed RGBA images. The cache is supplied by the caller so one
/// animation can reuse sprites across all frames without global mutable
/// state; callers can also reuse it across renders for warm-cache behavior.
#[derive(Debug, Default)]
struct WheelLabelSpriteCache {
    entries: BTreeMap<(String, i32), WheelLabelSprite>,
    order: VecDeque<(String, i32)>,
    hits: usize,
    misses: usize,
}

impl WheelLabelSpriteCache {
    fn get_or_insert(&mut self, text: &str, scale: i32) -> WheelLabelSprite {
        let key = (text.to_owned(), scale);
        if let Some(sprite) = self.entries.get(&key) {
            self.hits = self.hits.saturating_add(1);
            let sprite = sprite.clone();
            if let Some(position) = self.order.iter().position(|entry| entry == &key) {
                self.order.remove(position);
            }
            self.order.push_back(key);
            return sprite;
        }
        self.misses = self.misses.saturating_add(1);
        let pixel_size = match scale {
            3 => 16.0,
            2 => 14.0,
            _ => 10.0,
        };
        let sprite = render_font_mask(text, pixel_size, true).unwrap_or_else(|| {
            let width = bitmap_text_width(text, scale).max(1);
            let height = 7 * scale;
            WheelLabelSprite {
                width,
                height,
                mask: render_bitmap_mask(text, scale, width, height),
            }
        });
        if self.entries.len() >= MAX_WHEEL_LABEL_SPRITES
            && let Some(evicted) = self.order.pop_front()
        {
            self.entries.remove(&evicted);
        }
        self.entries.insert(key.clone(), sprite.clone());
        self.order.push_back(key);
        sprite
    }
}

fn build_wheel_label_atlas(
    wedges: &[WheelWedge],
    sprite_cache: &mut WheelLabelSpriteCache,
) -> WheelLabelAtlas {
    let wedge_count = wedges.len().max(1);
    let slice = std::f64::consts::TAU / wedge_count as f64;
    let radius = i32::from(WHEEL_MEDIA_SIZE) / 2 - 30;
    let mut atlas_indices = BTreeMap::<(String, i32), usize>::new();
    let mut sprites = Vec::new();
    let mut wedge_sprite_indices = Vec::with_capacity(wedges.len());
    for wedge in wedges {
        let text = match wedge.value {
            WheelValue::Numeric(value) if value > 0 => value.to_string(),
            _ => wedge.label.clone(),
        };
        let scale = label_scale(&text, slice, radius);
        let key = (text.clone(), scale);
        let sprite_index = if let Some(index) = atlas_indices.get(&key).copied() {
            index
        } else {
            let sprite = sprite_cache.get_or_insert(&text, scale);
            let index = sprites.len();
            atlas_indices.insert(key, index);
            sprites.push(sprite);
            index
        };
        wedge_sprite_indices.push(sprite_index);
    }
    WheelLabelAtlas {
        sprites,
        wedge_sprite_indices,
    }
}

#[derive(Clone, Copy, Debug)]
struct WheelPixelGeometry {
    distance: f64,
    angle: f64,
}

fn wheel_pixel_geometry() -> &'static [WheelPixelGeometry] {
    static GEOMETRY: OnceLock<Vec<WheelPixelGeometry>> = OnceLock::new();
    GEOMETRY.get_or_init(|| {
        let size = i32::from(WHEEL_MEDIA_SIZE);
        let center = size / 2;
        let mut geometry = Vec::with_capacity(usize::from(WHEEL_MEDIA_SIZE).pow(2));
        for y in 0..size {
            for x in 0..size {
                let dx = x - center;
                let dy = y - center;
                geometry.push(WheelPixelGeometry {
                    distance: ((dx * dx + dy * dy) as f64).sqrt(),
                    angle: (f64::from(dx).atan2(f64::from(-dy)) + std::f64::consts::TAU)
                        .rem_euclid(std::f64::consts::TAU),
                });
            }
        }
        geometry
    })
}

#[allow(clippy::too_many_arguments)]
fn draw_wheel_frame(
    pixels: &mut [u8],
    wedge_indices: &[u8],
    bright_wedge_indices: &[u8],
    label_atlas: &WheelLabelAtlas,
    target_index: usize,
    frame_index: usize,
    golden: bool,
    display_name: Option<&str>,
) {
    let size = i32::from(WHEEL_MEDIA_SIZE);
    let center = size / 2;
    let radius = center - 30;
    let inner_radius = radius / 3;
    let wedge_count = wedge_indices.len();
    if wedge_count == 0 {
        return;
    }
    let slice = std::f64::consts::TAU / wedge_count as f64;
    let rotation = wheel_frame_rotation(frame_index, target_index, wedge_count);

    if let Some(display_name) = display_name {
        draw_matrix_background(pixels, frame_index);
        draw_terminal_status(pixels, frame_index, display_name);
    }

    let reveal_winner = frame_index == WHEEL_MEDIA_FRAME_COUNT.saturating_sub(1);
    // Pixel distance and angle depend only on the fixed 500px canvas. Cache
    // them once instead of recalculating sqrt/atan2 for every animation frame.
    // The same pass also paints the outer glow rings before skipping pixels
    // outside the wheel face.
    for (index, geometry) in wheel_pixel_geometry().iter().enumerate() {
        let distance = geometry.distance;
        if distance > f64::from(radius) {
            if (f64::from(radius + 3)..=f64::from(radius + 4)).contains(&distance)
                || (f64::from(radius + 8)..=f64::from(radius + 9)).contains(&distance)
                || (f64::from(radius + 13)..=f64::from(radius + 14)).contains(&distance)
            {
                pixels[index] = 11;
            }
            continue;
        }
        if distance <= f64::from(inner_radius) {
            pixels[index] = if distance >= f64::from(inner_radius - 4) {
                1
            } else {
                3
            };
            continue;
        }
        if distance >= f64::from(radius - 2) {
            pixels[index] = 5;
            continue;
        }
        let rotated = (geometry.angle + rotation).rem_euclid(std::f64::consts::TAU);
        let wedge = (rotated / slice).floor() as usize % wedge_count;
        let from_boundary = (rotated % slice).min(slice - (rotated % slice));
        if from_boundary * distance <= 1.25 {
            pixels[index] = if reveal_winner && wedge == target_index {
                8
            } else {
                5
            };
        } else if reveal_winner && wedge == target_index {
            pixels[index] = bright_wedge_indices
                .get(wedge)
                .copied()
                .unwrap_or(wedge_indices[wedge]);
        } else {
            pixels[index] = wedge_indices[wedge];
        }
    }
    draw_wheel_labels(pixels, label_atlas, rotation);

    let center_lines = if golden {
        ["GOLDEN", "WHEEL"]
    } else {
        ["WHEEL OF", "FORTUNE"]
    };
    for (line_index, line) in center_lines.iter().enumerate() {
        let y = center - 18 + i32::try_from(line_index).unwrap_or_default() * 20;
        if let Some(sprite) = render_font_mask(line, 13.0, false) {
            let x = center - sprite.width / 2;
            blit_bitmap_mask(pixels, &sprite, x, y, 1);
        } else {
            let scale = 2;
            let x = center - bitmap_text_width(line, scale) / 2;
            draw_bitmap_text(pixels, line, x, y, scale, 1);
        }
    }

    // Match Python's notched pointer, including the high-contrast white edge.
    let pointer_y = center - radius - 5;
    let pointer = [
        (center, pointer_y + 35),
        (center - 18, pointer_y - 8),
        (center - 6, pointer_y + 2),
        (center, pointer_y + 18),
        (center + 6, pointer_y + 2),
        (center + 18, pointer_y - 8),
    ];
    draw_filled_polygon(pixels, &pointer, 5);
    let pointer_inner = [
        (center, pointer_y + 30),
        (center - 14, pointer_y - 4),
        (center - 4, pointer_y + 4),
        (center, pointer_y + 16),
        (center + 4, pointer_y + 4),
        (center + 14, pointer_y - 4),
    ];
    draw_filled_polygon(pixels, &pointer_inner, 2);
}

fn wheel_frame_rotation(frame_index: usize, target_index: usize, wedge_count: usize) -> f64 {
    if wedge_count == 0 {
        return 0.0;
    }
    let frame_index = frame_index.min(WHEEL_MEDIA_FRAME_COUNT.saturating_sub(1));
    let slice = std::f64::consts::TAU / wedge_count as f64;
    // Python rotates its wheel face by the negative of this animation value.
    // This rasterizer samples the source face directly, so use the inverse
    // convention: six negative turns ending with the target wedge centered at
    // the fixed top pointer.
    let target_center = (target_index as f64 + 0.5) * slice;
    let final_rotation = target_center - std::f64::consts::TAU * 6.0;
    let near_miss_rotation = final_rotation + slice * 1.2;
    const SLOW_END: usize = 58;
    const CREEP_END: usize = 68;
    if frame_index <= SLOW_END {
        let progress = frame_index as f64 / SLOW_END as f64;
        let eased = 1.0 - (1.0 - progress).powi(3);
        near_miss_rotation * eased
    } else {
        let progress =
            ((frame_index - SLOW_END) as f64 / (CREEP_END - SLOW_END) as f64).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - progress).powi(2);
        near_miss_rotation + (final_rotation - near_miss_rotation) * eased
    }
}

#[cfg(test)]
fn wheel_index_at_pointer(rotation: f64, wedge_count: usize) -> Option<usize> {
    if wedge_count == 0 {
        return None;
    }
    let slice = std::f64::consts::TAU / wedge_count as f64;
    Some((rotation.rem_euclid(std::f64::consts::TAU) / slice).floor() as usize % wedge_count)
}

fn draw_matrix_background(pixels: &mut [u8], frame_index: usize) {
    if frame_index >= 32 {
        return;
    }
    let size = i32::from(WHEEL_MEDIA_SIZE);
    let center = size / 2;
    let radius = center - 30;
    const STREAMS: &[(i32, i32, i32, usize, &str)] = &[
        (3, 19, 31, 7, "JOPACOINDEGEN$01234567890"),
        (10, 27, 143, 5, "GAMBARIGGED322JOPA"),
        (18, 34, 287, 8, "ALLINNOBALLSSENDIT"),
        (34, 16, 403, 4, "RNGGODGGEZCOPIUM"),
        (466, 31, 211, 9, "JOPAWASHEREDEGEN"),
        (480, 22, 79, 6, "EZCLAPJOPACOINS"),
        (488, 35, 347, 8, "JUMPMANGAMBA322"),
        (495, 18, 459, 5, "RIGGEDSENDITRNG"),
    ];
    let fade_step = frame_index.saturating_sub(27);
    for (x, speed_tenths, offset, length, glyphs) in STREAMS {
        let trail_span = i32::try_from(*length).unwrap_or_default() * 10;
        let head_y = (offset + i32::try_from(frame_index).unwrap_or_default() * speed_tenths)
            .rem_euclid(size + trail_span);
        let glyphs = glyphs.chars().collect::<Vec<_>>();
        for trail_index in 0..*length {
            if fade_step > 0 && trail_index % 5 < fade_step {
                continue;
            }
            let y = head_y - i32::try_from(trail_index).unwrap_or_default() * 10;
            if !(0..size).contains(&y) {
                continue;
            }
            let dx = *x - center;
            let dy = y - center;
            if dx * dx + dy * dy < (radius + 8) * (radius + 8) {
                continue;
            }
            let glyph_index =
                (usize::try_from(head_y / 10).unwrap_or_default() + trail_index) % glyphs.len();
            let color = if trail_index == 0 { 6 } else { 7 };
            draw_font_text(
                pixels,
                &glyphs[glyph_index].to_string(),
                *x,
                y,
                9.0,
                false,
                color,
            );
        }
    }
}

fn draw_terminal_status(pixels: &mut [u8], frame_index: usize, display_name: &str) {
    let y = i32::from(WHEEL_MEDIA_SIZE) - 15;
    let session = display_name.bytes().fold(0x811c_u32, |hash, byte| {
        hash.wrapping_mul(0x0100_0193) ^ u32::from(byte)
    }) & 0xffff;
    let old_text = format!("JOPA-T/v3.7 │ {display_name} │ #{session:04x}");
    let prompt = format!("{display_name}@jopa-t:~$ ");
    if frame_index < 27 {
        draw_font_text(pixels, &old_text, 4, y, 9.0, false, 7);
        return;
    }
    if frame_index < 31 {
        let progress = frame_index - 27;
        let width = old_text.chars().count().max(prompt.chars().count());
        let mut transition = String::with_capacity(width);
        for index in 0..width {
            let edge_distance = index.min(width.saturating_sub(index + 1));
            let reveal = progress * width / 4;
            let character = if edge_distance <= reveal {
                prompt.chars().nth(index).unwrap_or(' ')
            } else if (index + frame_index).is_multiple_of(5) {
                ['█', '▓', '▒', '░'][(index + frame_index) % 4]
            } else {
                old_text.chars().nth(index).unwrap_or(' ')
            };
            transition.push(character);
        }
        draw_font_text(
            pixels,
            &transition,
            4,
            y,
            9.0,
            false,
            if progress >= 2 { 6 } else { 7 },
        );
        return;
    }
    let status_width = draw_font_text(pixels, &prompt, 4, y, 9.0, false, 6);
    draw_font_text(pixels, "█", 4 + status_width, y, 9.0, false, 6);
}

fn draw_bitmap_text(pixels: &mut [u8], text: &str, left: i32, top: i32, scale: i32, color: u8) {
    let width = bitmap_text_width(text, scale).max(1);
    let height = 7 * scale;
    let sprite = WheelLabelSprite {
        width,
        height,
        mask: render_bitmap_mask(text, scale, width, height),
    };
    blit_bitmap_mask(pixels, &sprite, left, top, color);
}

fn draw_font_text(
    pixels: &mut [u8],
    text: &str,
    left: i32,
    top: i32,
    pixel_size: f32,
    bold: bool,
    color: u8,
) -> i32 {
    if let Some(sprite) = render_font_mask(text, pixel_size, bold) {
        let width = sprite.width;
        blit_bitmap_mask(pixels, &sprite, left, top, color);
        width
    } else {
        let scale = (pixel_size / 7.0).round().max(1.0) as i32;
        draw_bitmap_text(pixels, text, left, top, scale, color);
        bitmap_text_width(text, scale)
    }
}

fn draw_filled_rect(pixels: &mut [u8], left: i32, top: i32, width: i32, height: i32, color: u8) {
    let size = i32::from(WHEEL_MEDIA_SIZE);
    for y in top..top.saturating_add(height) {
        for x in left..left.saturating_add(width) {
            if (0..size).contains(&x) && (0..size).contains(&y) {
                pixels[usize::try_from(y * size + x).unwrap_or_default()] = color;
            }
        }
    }
}

fn draw_filled_polygon(pixels: &mut [u8], points: &[(i32, i32)], color: u8) {
    if points.len() < 3 {
        return;
    }
    let Some(min_y) = points.iter().map(|(_, y)| *y).min() else {
        return;
    };
    let Some(max_y) = points.iter().map(|(_, y)| *y).max() else {
        return;
    };
    for y in min_y..=max_y {
        let mut intersections = Vec::new();
        for index in 0..points.len() {
            let (x1, y1) = points[index];
            let (x2, y2) = points[(index + 1) % points.len()];
            if (y1 <= y && y < y2) || (y2 <= y && y < y1) {
                let x = x1 + (y - y1) * (x2 - x1) / (y2 - y1);
                intersections.push(x);
            }
        }
        intersections.sort_unstable();
        for pair in intersections.chunks_exact(2) {
            draw_filled_rect(pixels, pair[0], y, pair[1] - pair[0] + 1, 1, color);
        }
    }
}

/// Draw compact, font-independent wedge labels.  The Python renderer uses a
/// bounded Pillow sprite cache; this adapter uses a fixed 5x7 glyph atlas so
/// every frame reuses the same measured text geometry and never loads a font
/// or allocates a per-frame layout.  Labels are intentionally horizontal after
/// wheel rotation, matching the Python visual contract.
fn draw_wheel_labels(pixels: &mut [u8], atlas: &WheelLabelAtlas, rotation: f64) {
    if atlas.wedge_sprite_indices.is_empty() {
        return;
    }
    let size = i32::from(WHEEL_MEDIA_SIZE);
    let center = size / 2;
    let radius = center - 30;
    let text_radius = radius * 68 / 100;
    let wedge_count = atlas.wedge_sprite_indices.len();
    let slice = std::f64::consts::TAU / wedge_count as f64;
    for (index, sprite_index) in atlas.wedge_sprite_indices.iter().copied().enumerate() {
        let Some(sprite) = atlas.sprites.get(sprite_index) else {
            continue;
        };
        let width = sprite.width;
        // The face sampler rotates source wedges clockwise by `rotation`.
        // Labels are overlays, so their destination positions use the inverse
        // sign. Keeping these conventions paired is what makes the visible
        // label under the pointer equal the economically settled wedge.
        let angle = index as f64 * slice - rotation + slice / 2.0;
        let x = f64::from(center) + f64::from(text_radius) * angle.sin();
        let y = f64::from(center) - f64::from(text_radius) * angle.cos();
        let left = x.round() as i32 - width / 2;
        let top = y.round() as i32 - sprite.height / 2;
        blit_bitmap_mask(pixels, sprite, left + 1, top + 1, 4);
        blit_bitmap_mask(pixels, sprite, left, top, 5);
    }
}

fn label_scale(text: &str, slice: f64, radius: i32) -> i32 {
    let arc_width =
        2.0 * std::f64::consts::PI * f64::from(radius) * 0.68 * slice / std::f64::consts::TAU;
    let max_width = (arc_width * 0.85).max(5.0);
    (2..=3)
        .rev()
        .find(|scale| f64::from(bitmap_text_width(text, *scale)) <= max_width)
        .unwrap_or(1)
}

fn bitmap_text_width(text: &str, scale: i32) -> i32 {
    text.chars().count() as i32 * 6 * scale - scale
}

fn render_bitmap_mask(text: &str, scale: i32, width: i32, height: i32) -> Vec<u8> {
    let mut mask = vec![0_u8; usize::try_from(width.saturating_mul(height)).unwrap_or_default()];
    for (char_index, character) in text.chars().enumerate() {
        let glyph = bitmap_glyph(character);
        let origin_x = char_index as i32 * 6 * scale;
        for (row, bits) in glyph.iter().copied().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = origin_x + column * scale + dx;
                        let y = row as i32 * scale + dy;
                        if (0..width).contains(&x) && (0..height).contains(&y) {
                            mask[usize::try_from(y * width + x).unwrap_or_default()] = u8::MAX;
                        }
                    }
                }
            }
        }
    }
    mask
}

fn blit_bitmap_mask(pixels: &mut [u8], sprite: &WheelLabelSprite, left: i32, top: i32, color: u8) {
    let size = i32::from(WHEEL_MEDIA_SIZE);
    for y in 0..sprite.height {
        for x in 0..sprite.width {
            let source = sprite.mask[usize::try_from(y * sprite.width + x).unwrap_or_default()];
            if source == 0 {
                continue;
            }
            let target_x = left + x;
            let target_y = top + y;
            if (0..size).contains(&target_x) && (0..size).contains(&target_y) {
                // Preserve fontdue's coverage with fixed edge tones from the
                // shared GIF palette. Ordered dithering made small labels
                // look perforated; tonal edges stay solid like Pillow text.
                let resolved_color = match color {
                    5 if source >= 192 => 5,
                    5 if source >= 112 => 10,
                    5 if source >= 48 => 9,
                    1 if source >= 144 => 1,
                    1 if source >= 48 => 11,
                    6 if source >= 144 => 6,
                    6 if source >= 48 => 12,
                    7 if source >= 144 => 7,
                    7 if source >= 48 => 12,
                    4 if source >= 64 => 4,
                    _ if source >= 128 => color,
                    _ => continue,
                };
                pixels[usize::try_from(target_y * size + target_x).unwrap_or_default()] =
                    resolved_color;
            }
        }
    }
}

fn bitmap_glyph(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0f, 0x10, 0x10, 0x10, 0x10, 0x10, 0x0f],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0f, 0x10, 0x10, 0x17, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x1f],
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
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1b, 0x11],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
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
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1f],
        '@' => [0x0e, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0f],
        ':' => [0x00, 0x04, 0x00, 0x00, 0x04, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '#' => [0x0a, 0x1f, 0x0a, 0x0a, 0x1f, 0x0a, 0x00],
        '$' => [0x04, 0x0f, 0x14, 0x0e, 0x05, 0x1e, 0x04],
        '|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        '~' => [0x00, 0x00, 0x09, 0x16, 0x00, 0x00, 0x00],
        ' ' => [0; 7],
        _ => [0x1f, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x1f],
    }
}

fn draw_explosion_frame(pixels: &mut [u8], frame_index: usize) {
    let size = i32::from(WHEEL_MEDIA_SIZE);
    let center = size / 2;
    let progress = frame_index as f64 / EXPLOSION_MEDIA_FRAME_COUNT as f64;
    let radius = (progress * f64::from(center + 20)) as i32;
    for y in 0..size {
        for x in 0..size {
            let dx = x - center;
            let dy = y - center;
            let distance = ((dx * dx + dy * dy) as f64).sqrt() as i32;
            let index = usize::try_from(y * size + x).unwrap_or_default();
            if distance <= radius {
                pixels[index] = if distance < radius / 3 {
                    5
                } else if distance < radius / 2 {
                    4
                } else if distance < radius * 2 / 3 {
                    3
                } else {
                    2
                };
            } else if distance <= radius.saturating_add(5) {
                pixels[index] = 1;
            }
        }
    }
}

#[cfg(test)]
#[path = "wheel_media/tests.rs"]
mod tests;
