//! Personal gameplay updates: carved change marks, measured facts and an earned seal.

use std::f32::consts::{PI, TAU};

use super::stage::{Align, Face, Stage, WIDTH, count_up, text_width, tracked_width};
use super::*;

const TEAL: Rgba = Rgba::rgb(112, 213, 174);
const CORAL: Rgba = Rgba::rgb(239, 141, 122);
const MUTED: Rgba = Rgba::rgb(179, 194, 196);
const LEFT: i32 = 48;
const COLUMN: i32 = WIDTH - LEFT * 2;

/// A balance change's title and its value under it, each as `(width, height,
/// size)`, and the distance from one change to the next.
pub(super) const CHANGE_TITLE: (i32, i32, f32) = (554, 56, 44.0);
pub(super) const CHANGE_VALUE: (i32, i32, f32) = (554, 50, 36.0);
const CHANGE_PITCH: i32 = 148;

/// Three generous reading bands, with the verdict encoded in both color and shape.
pub(super) fn notes(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    // The top line says what is being compared; without one it names the card.
    stage.block(
        slide.lines.first().unwrap_or(&slide.title),
        (LEFT, 96, COLUMN, 40),
        28.0,
        Face::Medium,
        p.accent,
        Align::Left,
        (0.0, 0.14),
    );
    stage.block(
        if slide.headline.is_empty() {
            &slide.title
        } else {
            &slide.headline
        },
        (LEFT, 143, COLUMN, 135),
        62.0,
        Face::Serif,
        p.fg,
        Align::Left,
        (0.04, 0.23),
    );
    rule(stage, 282.0);
    for (index, row) in slide.rows.iter().take(3).enumerate() {
        // Three changes sit evenly below the rule.
        let top = 328 + index as i32 * CHANGE_PITCH;
        let start = 0.19 + index as f32 * 0.17;
        let color = if row.value > 0.0 {
            TEAL
        } else if row.value < 0.0 {
            CORAL
        } else {
            p.accent
        };
        change_mark(stage, (74.0, top as f32 + 28.0), row.value, color, start);
        let (width, height, size) = CHANGE_TITLE;
        stage.block(
            &row.primary,
            (118, top, width, height),
            size,
            Face::Bold,
            color,
            Align::Left,
            (start, start + 0.16),
        );
        let (width, value_height, size) = CHANGE_VALUE;
        stage.block(
            &row.secondary,
            (118, top + height, width, value_height),
            size,
            Face::Medium,
            p.fg,
            Align::Left,
            (start + 0.04, start + 0.22),
        );
    }
}

/// A stamped diamond with an up-cut, down-cut or crossed rework rune.
fn change_mark(stage: &mut Stage<'_>, center: (f32, f32), value: f64, color: Rgba, start: f32) {
    let shown = stage.ease(start, start + 0.18);
    if shown <= 0.0 {
        return;
    }
    let point = |x: f32, y: f32| (center.0 + x * shown, center.1 + y * shown);
    let diamond = [
        point(0.0, -27.0),
        point(27.0, 0.0),
        point(0.0, 27.0),
        point(-27.0, 0.0),
    ];
    stage.canvas.polygon(&diamond, stage.palette.soft);
    stage.canvas.stroke(
        &[diamond[0], diamond[1], diamond[2], diamond[3], diamond[0]],
        1.5,
        color.scaled_alpha(0.65),
    );
    if value != 0.0 && value.is_finite() {
        let direction = if value > 0.0 { -1.0 } else { 1.0 };
        stage.canvas.stroke(
            &[
                point(-10.0, 4.0 * direction),
                point(0.0, 13.0 * direction),
                point(10.0, 4.0 * direction),
            ],
            4.0,
            color,
        );
        stage.canvas.stroke(
            &[point(0.0, 11.0 * direction), point(0.0, -11.0 * direction)],
            4.0,
            color,
        );
    } else {
        stage
            .canvas
            .stroke(&[point(-10.0, -10.0), point(10.0, 10.0)], 4.0, color);
        stage
            .canvas
            .stroke(&[point(-10.0, 10.0), point(10.0, -10.0)], 4.0, color);
    }
}

fn rule(stage: &mut Stage<'_>, y: f32) {
    let width = COLUMN as f32 * stage.ease(0.08, 0.3);
    stage.canvas.stroke(
        &[(LEFT as f32, y), (LEFT as f32 + width, y)],
        1.0,
        stage.palette.accent.scaled_alpha(0.5),
    );
}

/// When the cover's update downloads, when the progress bar leaves, and when
/// the title starts to arrive, on the card's timeline.
const DOWNLOAD: (f32, f32) = (0.04, 0.44);
const CLEAR: (f32, f32) = (0.5, 0.57);
const TITLE: f32 = 0.56;

/// The share of the update downloaded: a fast start, a stall, then the rest.
fn downloaded(progress: f32) -> f32 {
    let smooth = |x: f32| x * x * (3.0 - 2.0 * x);
    if progress < 0.45 {
        0.64 * (1.0 - (1.0 - progress / 0.45).powi(2))
    } else if progress < 0.6 {
        0.64 + 0.03 * (progress - 0.45) / 0.15
    } else {
        0.67 + 0.33 * smooth((progress - 0.6) / 0.4)
    }
}

/// A small tracked label centered on the card.
fn centered_label(stage: &mut Stage<'_>, value: &str, baseline: f32, size: f32, color: Rgba) {
    let tracking = size * 0.2;
    let width = tracked_width(value, size, Face::Bold, tracking);
    stage.tracked(
        value,
        (WIDTH as f32 - width) / 2.0,
        baseline,
        size,
        Face::Bold,
        color,
        tracking,
    );
}

/// The update downloading: a percentage counting up over a filling bar. It
/// sinks away once the download completes.
fn download(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let leave = stage.ease(CLEAR.0, CLEAR.1);
    if leave >= 1.0 {
        return;
    }
    let fill = downloaded(stage.progress(DOWNLOAD.0, DOWNLOAD.1));
    if leave < 0.3 {
        let label = if fill >= 1.0 {
            "READY TO PLAY"
        } else {
            "DOWNLOADING UPDATE"
        };
        centered_label(stage, label, 318.0, 24.0, p.accent);
        let version = slide.stat_value.trim();
        if !version.is_empty() {
            let version = format!("PATCH {}", version.to_uppercase());
            centered_label(stage, &version, 668.0, 20.0, MUTED);
        }
    }
    // The percentage sinks behind the bar as the card clears.
    let percent = format!("{}%", (fill * 100.0).floor() as i32);
    let size = 190.0;
    let (top, bottom) = (352, 352 + (size * 1.17) as i32);
    let width = text_width("100%", size, Face::Display);
    stage.line(
        &percent,
        (WIDTH as f32 + width) / 2.0 - text_width(&percent, size, Face::Display),
        top as f32 + size * 0.9 + leave * size * 1.2,
        size,
        Face::Display,
        p.fg,
        (top, bottom),
    );
    let (left, right) = ((LEFT + 12) as f32, (WIDTH - LEFT - 12) as f32);
    let (middle, half) = ((left + right) / 2.0, (right - left) / 2.0 * (1.0 - leave));
    let track = (middle - half, 596.0, middle + half, 624.0);
    stage.canvas.rounded_rect(track, 14.0, p.soft);
    let filled = (track.0, track.1, track.0 + half * 2.0 * fill, track.3);
    if filled.2 - filled.0 >= 1.0 {
        stage.canvas.rounded_rect(filled, 14.0, p.accent);
    }
}

/// The patch cover: an update downloads, then resolves into the version, the
/// patch's name and the player it shipped for.
pub(super) fn cover(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    if let Some(height) = stage.art.as_ref().map(|art| art.height as i32) {
        let top = 900 - height;
        let shown = stage.ease(TITLE + 0.02, 0.94);
        let reveal = 900 - (height as f32 * shown) as i32;
        super::artwork::draw(stage, 0, top, (0, reveal, WIDTH, 900), 0.0);
        // Merge the illustration into the slate without putting type on its focal point.
        for y in top.max(reveal)..(top + 110) {
            let opacity = 1.0 - (y - top) as f32 / 110.0;
            stage.canvas.rounded_rect(
                (0.0, y as f32, WIDTH as f32, (y + 1) as f32),
                0.0,
                p.bg.scaled_alpha(opacity),
            );
        }
    }
    download(stage);
    let version = slide.stat_value.trim();
    let titled = !version.is_empty();
    let mut y = 102;
    if titled {
        if stage.t >= TITLE {
            stage.tracked(
                "PATCH",
                LEFT as f32 + 3.0,
                124.0,
                26.0,
                Face::Bold,
                p.accent,
                26.0 * 0.3,
            );
        }
        let spun = count_up(version, stage.ease(TITLE, TITLE + 0.26));
        y = stage.stat_in(
            Face::Display,
            version,
            &spun,
            (LEFT, 136, COLUMN, 240),
            204.0,
            p.fg,
            Align::Left,
            (TITLE, TITLE + 0.12),
        ) - 22;
    }
    y = stage.block(
        if slide.headline.is_empty() {
            "Your personal\npatch notes"
        } else {
            &slide.headline
        },
        (LEFT, y, COLUMN, if titled { 124 } else { 284 }),
        if titled { 56.0 } else { 80.0 },
        Face::Serif,
        if titled { p.accent } else { p.fg },
        Align::Left,
        (TITLE + 0.1, TITLE + 0.26),
    );
    stage.block(
        // The cover has no footer, so the player is named here.
        if slide.username.is_empty() {
            &slide.stat_label
        } else {
            &slide.username
        },
        (LEFT, y + 6, COLUMN, if titled { 50 } else { 70 }),
        if titled { 34.0 } else { 43.0 },
        Face::Bold,
        if titled { p.fg } else { p.accent },
        Align::Left,
        (TITLE + 0.2, TITLE + 0.34),
    );
    if let Some(note) = slide.lines.first() {
        let shown = stage.ease(TITLE + 0.24, TITLE + 0.36);
        stage.canvas.rounded_rect(
            (0.0, 774.0, WIDTH as f32, 900.0),
            0.0,
            p.bg.scaled_alpha(0.88 * shown),
        );
        stage.block(
            note,
            (LEFT, 796, COLUMN, 76),
            27.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (TITLE + 0.24, TITLE + 0.38),
        );
    }
}

/// Whether an award card is a patch superlative, which wears the medal.
pub(super) fn is_patch_award(slide: &WrappedSlideData) -> bool {
    slide.theme == WrappedTheme::Patch
        || matches!(
            slide.kind.as_str(),
            "story_patch_award" | "more_story_patch_award"
        )
}

/// The size an item icon is shown at inside the medal, from Steam's 88x64.
pub(super) const MEDAL_ICON: (usize, usize) = (198, 144);
const MEDAL: (f32, f32) = (360.0, 240.0);

pub(super) fn award(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    medal(stage);
    stage.block(
        &slide.title,
        (LEFT, 360, COLUMN, 132),
        56.0,
        Face::Serif,
        p.fg,
        Align::Center,
        (0.26, 0.48),
    );
    let spun = count_up(&slide.stat_value, stage.ease(0.43, 0.77));
    stage.stat_in(
        Face::Display,
        &slide.stat_value,
        &spun,
        (LEFT, 508, COLUMN, 115),
        100.0,
        p.accent,
        Align::Center,
        (0.4, 0.58),
    );
    stage.block(
        &slide.stat_label,
        (LEFT + 12, 646, COLUMN - 24, 108),
        27.0,
        Face::Medium,
        p.fg,
        Align::Center,
        (0.56, 0.75),
    );
    if let Some(coverage) = slide.lines.get(1) {
        stage.block(
            coverage,
            (LEFT, 779, COLUMN, 48),
            22.0,
            Face::Medium,
            MUTED,
            Align::Center,
            (0.7, 0.85),
        );
    }
}

/// The medal: the item the award is named for in a brass bezel, or a faceted
/// crest when there is no icon, under a fan of rays.
fn medal(stage: &mut Stage<'_>) {
    let p = stage.palette;
    let scale = stage.pop(0.08, 0.37).max(0.0);
    if scale <= 0.0 {
        return;
    }
    // Rays fan out over the upper half, clear of the title below.
    let reach = stage.ease(0.2, 0.52);
    let turn = (1.0 - stage.ease(0.2, 0.8)) * 0.22;
    const RAYS: usize = 13;
    for ray in 0..RAYS {
        let angle = PI + ray as f32 / (RAYS - 1) as f32 * PI + turn;
        let (cos, sin) = (angle.cos(), angle.sin());
        let long = ray % 2 == 0;
        let (from, to) = (150.0, 150.0 + if long { 26.0 } else { 14.0 } * reach);
        if reach > 0.0 && sin <= 0.05 {
            stage.canvas.stroke(
                &[
                    (MEDAL.0 + cos * from, MEDAL.1 + sin * from),
                    (MEDAL.0 + cos * to, MEDAL.1 + sin * to),
                ],
                if long { 7.0 } else { 5.0 },
                p.accent,
            );
        }
    }
    let Some((width, height)) = stage
        .art
        .as_ref()
        .map(|art| (art.width as f32, art.height as f32))
    else {
        crest(stage, scale);
        return;
    };
    let frame = |inset: f32| {
        let (half_width, half_height) = (
            (width / 2.0 + 14.0 - inset) * scale,
            (height / 2.0 + 14.0 - inset) * scale,
        );
        (
            MEDAL.0 - half_width,
            MEDAL.1 - half_height,
            MEDAL.0 + half_width,
            MEDAL.1 + half_height,
        )
    };
    stage
        .canvas
        .rounded_rect(frame(0.0), 30.0 * scale, p.accent);
    stage.canvas.rounded_rect(frame(9.0), 21.0 * scale, p.bg);
    let (left, top) = (
        (MEDAL.0 - width / 2.0).round() as i32,
        (MEDAL.1 - height / 2.0).round() as i32,
    );
    let (right, bottom) = (left + width as i32, top + height as i32);
    // The icon opens from its center once the bezel has landed.
    let open = stage.ease(0.26, 0.46);
    let inset = (
        ((1.0 - open) * width / 2.0) as i32,
        ((1.0 - open) * height / 2.0) as i32,
    );
    let reveal = (
        left + inset.0,
        top + inset.1,
        right - inset.0,
        bottom - inset.1,
    );
    super::artwork::draw(stage, left, top, reveal, 16.0);
    // One glint crosses the icon after the title lands.
    let sweep = stage.progress(0.5, 0.84);
    if sweep > 0.0 && sweep < 1.0 {
        let (lean, band) = (height * 0.45, 46.0);
        let x = left as f32 - band - lean + (width + band * 2.0 + lean) * sweep;
        let (top, bottom) = (top as f32, bottom as f32);
        let within = |x: f32| x.clamp(left as f32, right as f32);
        stage.canvas.polygon(
            &[
                (within(x + lean), top),
                (within(x + lean + band), top),
                (within(x + band), bottom),
                (within(x), bottom),
            ],
            Rgba([255, 255, 255, 96]),
        );
    }
}

/// A faceted crest with a four-point star: the medal without an item.
fn crest(stage: &mut Stage<'_>, scale: f32) {
    let p = stage.palette;
    let point = |x: f32, y: f32| (MEDAL.0 + x * scale, MEDAL.1 + y * scale);
    let crest = [
        point(0.0, -104.0),
        point(88.0, -52.0),
        point(88.0, 52.0),
        point(0.0, 104.0),
        point(-88.0, 52.0),
        point(-88.0, -52.0),
    ];
    stage.canvas.polygon(&crest, p.accent);
    stage.canvas.polygon(
        &[crest[2], crest[3], crest[4], point(0.0, 0.0)],
        Rgba::rgb(158, 117, 63),
    );
    stage.canvas.ring(MEDAL, 70.0 * scale, 2.0 * scale, p.bg);
    let star = (0..8)
        .map(|index| {
            let angle = index as f32 / 8.0 * TAU - PI / 2.0;
            let radius = if index % 2 == 0 { 50.0 } else { 15.0 };
            point(angle.cos() * radius, angle.sin() * radius)
        })
        .collect::<Vec<_>>();
    stage.canvas.polygon(&star, p.bg);
}
