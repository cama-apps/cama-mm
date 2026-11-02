//! Original, bundled battlefield and character illustrations. External avatar
//! data never reaches this trusted-asset path.
use super::*;

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
        WrappedArtwork::Rune => None,
    }
}

pub(super) fn scene(canvas: &mut Canvas, recap: bool) {
    if let Some(image) = battlefield() {
        // Fill the landscape canvas without distorting the illustration.
        canvas.blit_opaque(image, 0, 0);
    }
    for y in 0..600 {
        for x in 0..800 {
            let alpha = if recap {
                (220.0 - x as f32 * 0.16).max(85.0)
            } else {
                (165.0 - (x - 420).max(0) as f32 * 0.8).max(0.0)
            } as u8;
            canvas.blend(x, y, Rgba([25, 45, 43, alpha]));
        }
    }
    canvas.fill_rect(0, 0, 800, 75, Rgba([25, 45, 43, 90]));
    canvas.fill_rect(0, 541, 800, 600, Rgba([25, 45, 43, 170]));
}

pub(super) fn hero(
    canvas: &mut Canvas,
    kind: WrappedArtwork,
    rect: (i32, i32, usize, usize),
    accent: Rgba,
) {
    if let Some(image) = character(kind) {
        blit(canvas, image, rect);
    } else {
        rune(
            canvas,
            (rect.0 + rect.2 as i32 / 2, rect.1 + rect.3 as i32 / 2),
            rect.2.min(rect.3) as i32 / 3,
            accent,
        );
    }
}

/// Premultiplied-alpha bilinear sampling avoids dark fringes around feathers,
/// ink strokes, and cloth on the transparent cutouts.
fn blit(canvas: &mut Canvas, image: &DecodedPng, rect: (i32, i32, usize, usize)) {
    for dy in 0..rect.3 {
        let y = ((dy as f32 + 0.5) * image.height as f32 / rect.3 as f32 - 0.5)
            .clamp(0.0, (image.height - 1) as f32);
        let y0 = y.floor() as usize;
        let y1 = (y0 + 1).min(image.height - 1);
        let fy = y - y0 as f32;
        for dx in 0..rect.2 {
            let x = ((dx as f32 + 0.5) * image.width as f32 / rect.2 as f32 - 0.5)
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
                let a = pixel[3] as f32 * weight;
                alpha += a;
                for channel in 0..3 {
                    rgb[channel] += pixel[channel] as f32 * a;
                }
            }
            if alpha > 0.0 {
                canvas.blend(
                    rect.0 + dx as i32,
                    rect.1 + dy as i32,
                    Rgba([
                        (rgb[0] / alpha).round() as u8,
                        (rgb[1] / alpha).round() as u8,
                        (rgb[2] / alpha).round() as u8,
                        alpha.round() as u8,
                    ]),
                );
            }
        }
    }
}

pub(super) fn rune(canvas: &mut Canvas, center: (i32, i32), radius: i32, color: Rgba) {
    for scale in [1.0, 0.72] {
        let r = (radius as f32 * scale) as i32;
        let points = [
            (center.0, center.1 - r),
            (center.0 + r, center.1),
            (center.0, center.1 + r),
            (center.0 - r, center.1),
            (center.0, center.1 - r),
        ];
        for pair in points.windows(2) {
            canvas.line(pair[0], pair[1], color, 3);
        }
    }
    let r = radius / 3;
    let points = [
        (center.0 - r, center.1 + r),
        (center.0 - r, center.1 - r),
        (center.0 + r, center.1 - r),
        (center.0 + r, center.1),
        (center.0, center.1),
        (center.0, center.1 - r / 2),
    ];
    for pair in points.windows(2) {
        canvas.line(pair[0], pair[1], color, 5);
    }
    for (x, y) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
        canvas.line(
            (center.0 + x * radius, center.1 + y * (radius / 2)),
            (
                center.0 + x * (radius + radius / 5),
                center.1 + y * (radius / 2 + radius / 5),
            ),
            color,
            3,
        );
    }
}

pub(super) fn lanes(canvas: &mut Canvas, origin: (i32, i32), size: i32, color: Rgba) {
    let (x, y) = origin;
    for points in [
        vec![(x, y + size), (x, y), (x + size, y)],
        vec![(x, y + size), (x + size, y)],
        vec![(x, y + size), (x + size, y + size), (x + size, y)],
    ] {
        for pair in points.windows(2) {
            canvas.line(pair[0], pair[1], color, 3);
        }
    }
    canvas.circle((x, y + size), 4, color);
    canvas.circle((x + size, y), 4, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_art_preserves_background_and_adds_detailed_pixels() {
        let base = Rgba::rgb(12, 34, 56);
        let mut canvas = Canvas::new(400, 400, base);
        hero(&mut canvas, WrappedArtwork::Ward, (40, 40, 320, 320), base);
        assert_eq!(&canvas.pixels[..4], base.0);
        assert_eq!(&canvas.pixels[(40 * 400 + 40) * 4..][..4], base.0);
        // A narrow ink silhouette deliberately leaves most of its tile empty.
        assert!(
            canvas
                .pixels
                .chunks_exact(4)
                .filter(|pixel| *pixel != base.0)
                .count()
                > 10_000
        );
        assert!(canvas.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
    }
}
