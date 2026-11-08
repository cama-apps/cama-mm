//! Bundled illustrations and per-player game art. External avatar data never
//! reaches this trusted-asset path.
use super::stage::{HEIGHT, PreparedArt, Stage, WIDTH};
use super::*;

/// The largest box an illustration or game scene is drawn into.
const SPOTLIGHT_FRAME: (usize, usize) = (640, 320);
const CHARACTER_FRAME: (usize, usize) = (300, 300);
const COVER_ART_HEIGHT: usize = 480;
/// Hero portraits beside ranked rows: half of Steam's 256x144 portrait.
pub(super) const ROW_FRAME: (usize, usize) = (128, 72);
pub(super) const RECAP_ROW_FRAME: (usize, usize) = (80, 45);
/// The hero inside a two-beat setup's frame, 16:9 like Steam's portraits.
pub(super) const BEAT_FRAME: (usize, usize) = (384, 216);
/// The open letterbox of a Cinema punchline: full width, cut from a 16:9 still.
pub(super) const CINEMA_FRAME: (usize, usize) = (720, 332);

fn battlefield() -> Option<&'static DecodedPng> {
    static IMAGE: OnceLock<Option<DecodedPng>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            decode_png(include_bytes!(
                "../../../../../assets/wrapped/battlefield.png"
            ))
        })
        .as_ref()
}

fn character(kind: WrappedArtwork) -> Option<&'static DecodedPng> {
    static WARD: OnceLock<Option<DecodedPng>> = OnceLock::new();
    static COURIER: OnceLock<Option<DecodedPng>> = OnceLock::new();
    match kind {
        WrappedArtwork::Ward => WARD
            .get_or_init(|| decode_png(include_bytes!("../../../../../assets/wrapped/ward.png")))
            .as_ref(),
        WrappedArtwork::Courier => COURIER
            .get_or_init(|| decode_png(include_bytes!("../../../../../assets/wrapped/courier.png")))
            .as_ref(),
        WrappedArtwork::None => None,
    }
}

/// Resample whichever art this card shows, once, before its frames are painted.
pub(super) fn prepare(slide: &WrappedSlideData) -> Option<PreparedArt> {
    match slide.layout {
        WrappedLayout::Cover => battlefield().map(|image| {
            let scale = WIDTH as f32 / image.width as f32;
            let height = ((image.height as f32 * scale) as usize).max(COVER_ART_HEIGHT);
            duotone(resample(image, WIDTH as usize, height), slide)
        }),
        WrappedLayout::Spotlight => slide.art_png.as_deref().and_then(decode_png).map(|image| {
            let (width, height) = fit_within(&image, SPOTLIGHT_FRAME);
            resample(&image, width, height)
        }),
        WrappedLayout::BigNumber => {
            let kind = character(slide.artwork)?;
            Some(resample(kind, CHARACTER_FRAME.0, CHARACTER_FRAME.1))
        }
        _ => None,
    }
}

/// Resample each ranked row's picture once. Rows without one stay `None`.
pub(super) fn prepare_rows(slide: &WrappedSlideData) -> Vec<Option<PreparedArt>> {
    let frame = match slide.layout {
        WrappedLayout::TopList => ROW_FRAME,
        WrappedLayout::Recap => RECAP_ROW_FRAME,
        _ => return Vec::new(),
    };
    slide
        .rows
        .iter()
        .map(|row| {
            let image = decode_png(row.image_png.as_deref()?)?;
            let (width, height) = fit_within(&image, frame);
            Some(resample(&image, width, height))
        })
        .collect()
}

/// A two-beat card's hero, framed beside the setup and, on a Cinema punchline,
/// letterboxed full width. One decode serves both.
pub(super) fn prepare_beat(slide: &WrappedSlideData) -> (Option<PreparedArt>, Option<PreparedArt>) {
    let Some(image) = slide.art_png.as_deref().and_then(decode_art_png) else {
        return (None, None);
    };
    let cinema = slide
        .beat
        .as_ref()
        .is_some_and(|beat| beat.treatment == WrappedPunch::Cinema);
    let framed = resample_region(&image, hero_region(&image, BEAT_FRAME, 0.32), BEAT_FRAME);
    let still = cinema.then(|| {
        vignette(resample_region(
            &image,
            hero_region(&image, CINEMA_FRAME, 0.3),
            CINEMA_FRAME,
        ))
    });
    (Some(framed), still)
}

/// The source rectangle `(left, top, width, height)` to show in `frame`. A
/// transparent full-body render is cut to its head and shoulders, `reach` of
/// the figure's height. Art already cut off at its bottom edge is only fitted:
/// from the top for a cutout, about the center for an opaque portrait.
fn hero_region(image: &DecodedPng, frame: (usize, usize), reach: f32) -> (f32, f32, f32, f32) {
    let aspect = frame.0 as f32 / frame.1 as f32;
    let (image_width, image_height) = (image.width as f32, image.height as f32);
    let alpha = |x: usize, y: usize| image.pixels[(y * image.width + x) * 4 + 3];
    let (last_x, last_y) = (image.width - 1, image.height - 1);
    let cutout = [(0, 0), (last_x, 0), (0, last_y), (last_x, last_y)]
        .into_iter()
        .all(|(x, y)| alpha(x, y) < 128);
    let solid = |y: usize| (0..image.width).any(|x| alpha(x, y) >= 32);
    let figure = cutout
        .then(|| {
            let top = (0..image.height).find(|y| solid(*y))?;
            let bottom = (top..image.height).rev().find(|y| solid(*y))?;
            Some((top, bottom))
        })
        .flatten();
    let Some((top, bottom)) = figure.filter(|(_, bottom)| *bottom < last_y) else {
        let width = image_width.min(image_height * aspect);
        let height = width / aspect;
        let top = if cutout {
            0.0
        } else {
            (image_height - height) / 2.0
        };
        return ((image_width - width) / 2.0, top, width, height);
    };
    // Center on the head: the weighted middle of the figure's top rows.
    let figure_height = (bottom - top + 1) as f32;
    // Millions of weighted positions: accumulate wide so the sum stays exact.
    let (mut weight, mut sum) = (0_u64, 0_u64);
    for y in top..top + (figure_height * 0.22) as usize {
        for x in 0..image.width {
            let value = u64::from(alpha(x, y));
            weight += value;
            sum += value * x as u64;
        }
    }
    let center = if weight > 0 {
        (sum as f64 / weight as f64) as f32
    } else {
        image_width / 2.0
    };
    let mut height = figure_height * reach;
    let mut width = height * aspect;
    if width > image_width {
        width = image_width;
        height = width / aspect;
    }
    let left = (center - width / 2.0).clamp(0.0, image_width - width);
    let top = (top as f32 - height * 0.06).clamp(0.0, (image_height - height).max(0.0));
    (left, top, width, height.min(image_height))
}

/// Resample `region` of `image` into `frame`, averaging several premultiplied
/// bilinear taps per pixel so large renders shrink without shimmering edges.
fn resample_region(
    image: &DecodedPng,
    region: (f32, f32, f32, f32),
    frame: (usize, usize),
) -> PreparedArt {
    let (left, top, region_width, region_height) = region;
    let (width, height) = frame;
    let step = (region_width / width as f32, region_height / height as f32);
    let taps = (step.0.max(step.1).ceil() as usize).clamp(1, 4);
    let tap = |x: f32, y: f32| {
        let x = x.clamp(0.0, (image.width - 1) as f32);
        let y = y.clamp(0.0, (image.height - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = (
            (x0 + 1).min(image.width - 1),
            (y0 + 1).min(image.height - 1),
        );
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let mut sample = [0.0_f32; 4];
        for (x, y, weight) in [
            (x0, y0, (1.0 - fx) * (1.0 - fy)),
            (x1, y0, fx * (1.0 - fy)),
            (x0, y1, (1.0 - fx) * fy),
            (x1, y1, fx * fy),
        ] {
            let pixel = &image.pixels[(y * image.width + x) * 4..][..4];
            let alpha = f32::from(pixel[3]) * weight;
            for channel in 0..3 {
                sample[channel] += f32::from(pixel[channel]) * alpha;
            }
            sample[3] += alpha;
        }
        sample
    };
    let mut pixels = vec![0_u8; width * height * 4];
    for dy in 0..height {
        for dx in 0..width {
            let mut total = [0.0_f32; 4];
            for j in 0..taps {
                for i in 0..taps {
                    let x = left + (dx as f32 + (i as f32 + 0.5) / taps as f32) * step.0 - 0.5;
                    let y = top + (dy as f32 + (j as f32 + 0.5) / taps as f32) * step.1 - 0.5;
                    for (sum, value) in total.iter_mut().zip(tap(x, y)) {
                        *sum += value;
                    }
                }
            }
            if total[3] > 0.0 {
                let output = &mut pixels[(dy * width + dx) * 4..][..4];
                for channel in 0..3 {
                    output[channel] = (total[channel] / total[3]).round() as u8;
                }
                output[3] = (total[3] / (taps * taps) as f32).round() as u8;
            }
        }
    }
    PreparedArt {
        width,
        height,
        pixels,
    }
}

/// Settle the still onto the black screen and darken its edges, as a film
/// frame falls off toward its corners.
fn vignette(mut art: PreparedArt) -> PreparedArt {
    let (half_width, half_height) = (art.width as f32 / 2.0, art.width as f32 * 9.0 / 32.0);
    for y in 0..art.height {
        for x in 0..art.width {
            let dx = (x as f32 + 0.5 - half_width) / half_width;
            let dy = (y as f32 + 0.5 - art.height as f32 / 2.0) / half_height;
            let reach = (dx.hypot(dy) / std::f32::consts::SQRT_2 - 0.55) / 0.45;
            let shade = 1.0 - 0.6 * reach.clamp(0.0, 1.0);
            let pixel = &mut art.pixels[(y * art.width + x) * 4..][..4];
            let cover = f32::from(pixel[3]) / 255.0 * shade;
            for channel in &mut pixel[..3] {
                *channel = (f32::from(*channel) * cover).round() as u8;
            }
            pixel[3] = 255;
        }
    }
    art
}

fn fit_within(image: &DecodedPng, frame: (usize, usize)) -> (usize, usize) {
    let scale = (frame.0 as f32 / image.width.max(1) as f32)
        .min(frame.1 as f32 / image.height.max(1) as f32);
    (
        ((image.width as f32 * scale) as usize).max(1),
        ((image.height as f32 * scale) as usize).max(1),
    )
}

/// Premultiplied-alpha bilinear sampling avoids dark fringes around feathers,
/// ink strokes, and cloth on the transparent cutouts.
fn resample(image: &DecodedPng, width: usize, height: usize) -> PreparedArt {
    let mut pixels = vec![0_u8; width * height * 4];
    for dy in 0..height {
        let y = ((dy as f32 + 0.5) * image.height as f32 / height as f32 - 0.5)
            .clamp(0.0, (image.height - 1) as f32);
        let y0 = y.floor() as usize;
        let y1 = (y0 + 1).min(image.height - 1);
        let fy = y - y0 as f32;
        for dx in 0..width {
            let x = ((dx as f32 + 0.5) * image.width as f32 / width as f32 - 0.5)
                .clamp(0.0, (image.width - 1) as f32);
            let x0 = x.floor() as usize;
            let x1 = (x0 + 1).min(image.width - 1);
            let fx = x - x0 as f32;
            let mut alpha = 0.0;
            let mut rgb = [0.0; 3];
            for (x, y, weight) in [
                (x0, y0, (1.0 - fx) * (1.0 - fy)),
                (x1, y0, fx * (1.0 - fy)),
                (x0, y1, (1.0 - fx) * fy),
                (x1, y1, fx * fy),
            ] {
                let pixel = &image.pixels[(y * image.width + x) * 4..][..4];
                let a = f32::from(pixel[3]) * weight;
                alpha += a;
                for channel in 0..3 {
                    rgb[channel] += f32::from(pixel[channel]) * a;
                }
            }
            if alpha > 0.0 {
                let output = &mut pixels[(dy * width + dx) * 4..][..4];
                for channel in 0..3 {
                    output[channel] = (rgb[channel] / alpha).round() as u8;
                }
                output[3] = alpha.round() as u8;
            }
        }
    }
    PreparedArt {
        width,
        height,
        pixels,
    }
}

/// Recolor an illustration from its luminance so it belongs to the card's
/// palette, as print posters do with two inks.
fn duotone(mut art: PreparedArt, slide: &WrappedSlideData) -> PreparedArt {
    let palette = stage::palette(slide.theme);
    let (dark, light) = (palette.bg.0, palette.accent.0);
    for pixel in art.pixels.chunks_exact_mut(4) {
        let value =
            (0.3 * f32::from(pixel[0]) + 0.59 * f32::from(pixel[1]) + 0.11 * f32::from(pixel[2]))
                / 255.0;
        // Quantized steps keep the woodcut look and a small GIF palette.
        let level = ((value * 4.0).round() / 4.0).clamp(0.0, 1.0);
        for channel in 0..3 {
            pixel[channel] = (f32::from(dark[channel])
                + (f32::from(light[channel]) - f32::from(dark[channel])) * level * 0.85)
                as u8;
        }
    }
    art
}

/// Copy prepared art to `(left, top)`, keeping only `reveal` rows and columns and
/// rounding the visible corners to `radius`.
pub(super) fn draw(
    stage: &mut Stage<'_>,
    left: i32,
    top: i32,
    reveal: (i32, i32, i32, i32),
    radius: f32,
) {
    let Some(art) = stage.art.take() else {
        return;
    };
    paint(&mut stage.canvas, &art, left, top, reveal, radius);
    stage.art = Some(art);
}

/// Copy prepared art onto the canvas inside `reveal`, rounding its corners.
pub(super) fn paint(
    canvas: &mut Canvas,
    art: &PreparedArt,
    left: i32,
    top: i32,
    reveal: (i32, i32, i32, i32),
    radius: f32,
) {
    let (reveal_left, reveal_top, reveal_right, reveal_bottom) = reveal;
    let (art_width, art_height) = (art.width as i32, art.height as i32);
    for y in reveal_top.max(top).max(0)..reveal_bottom.min(top + art_height).min(HEIGHT) {
        let row = (y - top) as usize;
        for x in reveal_left.max(left).max(0)..reveal_right.min(left + art_width).min(WIDTH) {
            let column = (x - left) as usize;
            let offset = (row * art.width + column) * 4;
            let mut alpha = f32::from(art.pixels[offset + 3]) / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            if radius > 0.0 {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let dx = (reveal_left as f32 + radius - fx)
                    .max(fx - (reveal_right as f32 - radius))
                    .max(0.0);
                let dy = (reveal_top as f32 + radius - fy)
                    .max(fy - (reveal_bottom as f32 - radius))
                    .max(0.0);
                alpha *= (radius - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
            }
            if alpha > 0.0 {
                let color = Rgba([
                    art.pixels[offset],
                    art.pixels[offset + 1],
                    art.pixels[offset + 2],
                    255,
                ]);
                canvas.set(x, y, color.scaled_alpha(alpha));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_art_preserves_background_and_adds_detailed_pixels() {
        let mut slide = WrappedSlideData::new("story_moment", "Wards");
        slide.artwork = WrappedArtwork::Ward;
        let mut stage = Stage::new(&slide);
        let base = Rgba::rgb(12, 34, 56);
        stage.canvas.clear(base);
        draw(&mut stage, 200, 200, (0, 0, WIDTH, HEIGHT), 0.0);
        assert_eq!(&stage.canvas.pixels[..4], base.0);
        // A narrow ink silhouette deliberately leaves most of its tile empty.
        assert!(
            stage
                .canvas
                .pixels
                .chunks_exact(4)
                .filter(|pixel| *pixel != base.0)
                .count()
                > 10_000
        );
        assert!(
            stage
                .canvas
                .pixels
                .chunks_exact(4)
                .all(|pixel| pixel[3] == 255)
        );
    }

    #[test]
    fn missing_or_malformed_player_art_is_fail_soft() {
        let mut slide = WrappedSlideData::new("story_pet", "Your pet");
        slide.layout = WrappedLayout::Spotlight;
        assert!(prepare(&slide).is_none());
        slide.art_png = Some(b"not png".to_vec());
        assert!(prepare(&slide).is_none());
        assert!(render_wrapped_slide(&slide).is_ok());
    }

    #[test]
    fn hero_art_frames_a_renders_head_and_only_fits_other_art() {
        // A 200x800 figure standing clear of every edge of a 1000 px render.
        let mut render = DecodedPng {
            width: 1000,
            height: 1000,
            pixels: vec![0; 1000 * 1000 * 4],
        };
        fn fill(render: &mut DecodedPng, rows: std::ops::Range<usize>) {
            for y in rows {
                for x in 400..600 {
                    render.pixels[(y * 1000 + x) * 4 + 3] = 255;
                }
            }
        }
        fill(&mut render, 100..900);
        let (left, top, width, height) = hero_region(&render, (16, 9), 0.25);
        assert_eq!(height, 200.0, "a quarter of the figure");
        assert!((top - 88.0).abs() < 0.01, "just above the head: {top}");
        assert!((left + width / 2.0 - 499.5).abs() < 0.01, "centered on it");
        // Art already cut off at the bottom is fitted from its top.
        fill(&mut render, 900..1000);
        assert_eq!(
            hero_region(&render, (16, 9), 0.25),
            (0.0, 0.0, 1000.0, 562.5)
        );
        // An opaque portrait is cropped about its center.
        let portrait = DecodedPng {
            width: 256,
            height: 144,
            pixels: vec![255; 256 * 144 * 4],
        };
        let (left, top, width, height) = hero_region(&portrait, CINEMA_FRAME, 0.3);
        assert_eq!((left, width), (0.0, 256.0));
        assert!((top * 2.0 + height - 144.0).abs() < 0.01);
    }

    #[test]
    fn wide_game_scenes_keep_their_shape_inside_the_frame() {
        let image = DecodedPng {
            width: 512,
            height: 288,
            pixels: vec![255; 512 * 288 * 4],
        };
        assert_eq!(fit_within(&image, SPOTLIGHT_FRAME), (568, 320));
        let tall = DecodedPng {
            width: 100,
            height: 400,
            pixels: vec![255; 100 * 400 * 4],
        };
        assert_eq!(fit_within(&tall, SPOTLIGHT_FRAME), (80, 320));
    }
}
