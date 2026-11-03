//! Bundled illustrations and per-player game art. External avatar data never
//! reaches this trusted-asset path.
use super::stage::{HEIGHT, PreparedArt, Stage, WIDTH};
use super::*;

/// The largest box an illustration or game scene is drawn into.
const SPOTLIGHT_FRAME: (usize, usize) = (640, 320);
const CHARACTER_FRAME: (usize, usize) = (300, 300);
const COVER_ART_HEIGHT: usize = 480;

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
    let Some(art) = stage.art.as_ref() else {
        return;
    };
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
                stage.canvas.set(x, y, color.scaled_alpha(alpha));
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
