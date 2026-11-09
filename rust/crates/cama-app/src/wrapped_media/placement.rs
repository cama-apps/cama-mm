//! Where a two-beat card's hero stands beside a Plain punchline: the
//! figure's silhouette, the largest placement that keeps it clear of the
//! type, and the cutout resampled once at that place.

use super::artwork::resample_region;
use super::stage::{HEIGHT, PlacedArt, PreparedArt, WIDTH};
use super::*;

/// Space kept between a punchline hero's silhouette and any type.
const BESIDE_GAP: i32 = 22;
/// The largest a punchline hero is drawn, then the steps down tried in turn.
const BESIDE_HEIGHT: f32 = 660.0;
const BESIDE_WIDTH: f32 = 560.0;
const BESIDE_STEPS: [f32; 7] = [1.0, 0.9, 0.8, 0.7, 0.6, 0.5, 0.4];
/// A cutout may sink below the card's bottom edge by up to this share of
/// its height, so the head and torso still show.
const BESIDE_SINK: f32 = 0.45;
/// The hero never rises into the progress bar and year label.
const BESIDE_TOP: i32 = 96;

/// Each source row's opaque span, `(first, last)` relative to the figure's
/// left edge, across the figure's bounds within the image.
struct Silhouette {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    rows: Vec<Option<(usize, usize)>>,
    cutout: bool,
}

/// Whether the image is a transparent cutout: all four corners see-through.
pub(super) fn is_cutout(image: &DecodedPng) -> bool {
    let (last_x, last_y) = (image.width - 1, image.height - 1);
    [(0, 0), (last_x, 0), (0, last_y), (last_x, last_y)]
        .into_iter()
        .all(|(x, y)| image.pixels[(y * image.width + x) * 4 + 3] < 128)
}

/// The figure in a transparent render, or the whole picture for opaque art.
fn silhouette(image: &DecodedPng) -> Option<Silhouette> {
    if !is_cutout(image) {
        return Some(Silhouette {
            left: 0,
            top: 0,
            width: image.width,
            height: image.height,
            rows: vec![Some((0, image.width - 1)); image.height],
            cutout: false,
        });
    }
    let spans = (0..image.height)
        .map(|y| {
            let solid = |x: &usize| image.pixels[(y * image.width + x) * 4 + 3] >= 32;
            let first = (0..image.width).find(solid)?;
            let last = (0..image.width).rev().find(solid)?;
            Some((first, last))
        })
        .collect::<Vec<_>>();
    let top = spans.iter().position(Option::is_some)?;
    let bottom = spans.iter().rposition(Option::is_some)?;
    let left = spans[top..=bottom]
        .iter()
        .flatten()
        .map(|span| span.0)
        .min()?;
    let right = spans[top..=bottom]
        .iter()
        .flatten()
        .map(|span| span.1)
        .max()?;
    Some(Silhouette {
        left,
        top,
        width: right - left + 1,
        height: bottom - top + 1,
        rows: spans[top..=bottom]
            .iter()
            .map(|span| span.map(|(first, last)| (first - left, last - left)))
            .collect(),
        cutout: true,
    })
}

/// Whether the silhouette scaled by `scale` with its corner at `(left, top)`
/// stays `BESIDE_GAP` clear of every box in `taken`.
fn clear_of(
    figure: &Silhouette,
    scale: f32,
    (left, top): (i32, i32),
    taken: &[(i32, i32, i32, i32)],
) -> bool {
    let height = (figure.height as f32 * scale).round() as i32;
    for y in top.max(0)..(top + height).min(HEIGHT) {
        // Every source row this card row covers, so thin details count.
        let from = ((y - top) as f32 / scale) as usize;
        let to = (((y - top + 1) as f32 / scale).ceil() as usize).clamp(from + 1, figure.height);
        let Some((first, last)) = figure.rows[from.min(figure.height - 1)..to]
            .iter()
            .flatten()
            .fold(None, |span: Option<(usize, usize)>, row| {
                Some(span.map_or(*row, |(a, b)| (a.min(row.0), b.max(row.1))))
            })
        else {
            continue;
        };
        let x0 = left + (first as f32 * scale).floor() as i32;
        let x1 = left + ((last + 1) as f32 * scale).ceil() as i32;
        let hits = taken
            .iter()
            .any(|(box_left, box_top, box_right, box_bottom)| {
                y >= box_top - BESIDE_GAP
                    && y < box_bottom + BESIDE_GAP
                    && x1 > box_left - BESIDE_GAP
                    && x0 < box_right + BESIDE_GAP
            });
        if hits {
            return false;
        }
    }
    true
}

/// Where a figure goes beside a punchline: its scale and its top-left
/// corner on the card.
struct Placement {
    scale: f32,
    left: i32,
    top: i32,
}

/// The largest placement of the figure against the card's right edge that
/// clears `taken`. A cutout stands on the bottom edge and may sink below it;
/// opaque art sits as a rounded picture above the footer. `None` when even
/// the smallest leaves no clear room.
fn find_place(figure: &Silhouette, taken: &[(i32, i32, i32, i32)]) -> Option<Placement> {
    let (bottom, right, sink) = if figure.cutout {
        (HEIGHT, WIDTH - 18, BESIDE_SINK)
    } else {
        (HEIGHT - 72, WIDTH - 40, 0.0)
    };
    let largest = (BESIDE_HEIGHT / figure.height as f32).min(BESIDE_WIDTH / figure.width as f32);
    BESIDE_STEPS.into_iter().find_map(|step| {
        let scale = largest * step;
        let width = (figure.width as f32 * scale).round() as i32;
        let height = (figure.height as f32 * scale).round() as i32;
        let deepest = (height as f32 * sink) as i32;
        (0..=deepest).step_by(6).find_map(|drop| {
            let (left, top) = (right - width, bottom - height + drop);
            (top >= BESIDE_TOP && clear_of(figure, scale, (left, top), taken))
                .then_some(Placement { scale, left, top })
        })
    })
}

/// The hero resampled once at its place beside the punchline.
pub(super) fn place_beside(
    image: &DecodedPng,
    taken: &[(i32, i32, i32, i32)],
) -> Option<PlacedArt> {
    let figure = silhouette(image)?;
    let Placement { scale, left, top } = find_place(&figure, taken)?;
    let region = (
        figure.left as f32,
        figure.top as f32,
        figure.width as f32,
        figure.height as f32,
    );
    let frame = (
        ((figure.width as f32 * scale).round() as usize).max(1),
        ((figure.height as f32 * scale).round() as usize).max(1),
    );
    let mut art = resample_region(image, region, frame);
    if figure.cutout {
        fade_haze(&mut art);
    }
    Some(PlacedArt {
        art,
        left,
        top,
        radius: if figure.cutout { 0.0 } else { 20.0 },
    })
}

/// Renders carry faint glows that run past the figure's bounds; on an open
/// card the crop would show as a hard rectangle. Alpha below the
/// silhouette's threshold fades to nothing and the rest rescales, so the
/// figure keeps its soft edge and the haze leaves no line.
fn fade_haze(art: &mut PreparedArt) {
    const HAZE: f32 = 32.0;
    for pixel in art.pixels.chunks_exact_mut(4) {
        let alpha = f32::from(pixel[3]);
        pixel[3] = ((alpha - HAZE).max(0.0) * 255.0 / (255.0 - HAZE)).round() as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pool_punchline_leaves_room_for_a_hero() {
        // A broad full-body cutout, the hardest kind of figure to fit.
        let (width, height) = (600, 1200);
        let mut render = DecodedPng {
            width,
            height,
            pixels: vec![0; width * height * 4],
        };
        for y in 40..1160 {
            for x in 0..width {
                let (dx, dy) = ((x as f32 - 300.0) / 270.0, (y as f32 - 600.0) / 560.0);
                if dx * dx + dy * dy <= 1.0 {
                    render.pixels[(y * width + x) * 4 + 3] = 255;
                }
            }
        }
        let figure = silhouette(&render).expect("a figure");
        let worst = [
            ("{name}", "Maximilianus"),
            ("{n}", "23,400"),
            ("{hero}", "Keeper of the Light"),
            ("{rank}", "187"),
            ("{of}", "187"),
            ("{time}", "12:59 AM"),
            ("{item}", "Black King Bar"),
        ];
        let punches = crate::wrapped_story::roast_pools::all_beats()
            .into_iter()
            .map(|beat| {
                worst
                    .iter()
                    .fold(beat.punch.to_owned(), |text, (slot, value)| {
                        text.replace(slot, value)
                    })
            })
            .collect::<std::collections::BTreeSet<_>>();
        let mut slide = WrappedSlideData::new("story_record", "Personal records");
        slide.layout = WrappedLayout::TwoBeat;
        slide.username = "Alexandria Maximilianus".to_owned();
        let mut cramped = Vec::new();
        for punch in punches {
            slide.beat = Some(WrappedBeat {
                punch: punch.clone(),
                credit: "27 kills · Keeper of the Light · Mar 14 · Lost".to_owned(),
                treatment: WrappedPunch::Plain,
                ..WrappedBeat::default()
            });
            let taken = super::super::beats::punch_art_clearance(&slide).expect("a plain card");
            if find_place(&figure, &taken).is_none() {
                cramped.push(punch);
            }
        }
        assert!(cramped.is_empty(), "no room for a hero: {cramped:#?}");
    }
}
