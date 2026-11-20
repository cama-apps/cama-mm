//! Personal gameplay updates: carved change marks, measured facts and an earned seal.

use super::stage::{Align, Face, Stage, WIDTH, count_up};
use super::*;

const TEAL: Rgba = Rgba::rgb(112, 213, 174);
const CORAL: Rgba = Rgba::rgb(239, 141, 122);
const MUTED: Rgba = Rgba::rgb(179, 194, 196);
const LEFT: i32 = 48;
const COLUMN: i32 = WIDTH - LEFT * 2;

/// Three generous reading bands, with the verdict encoded in both color and shape.
pub(super) fn notes(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    stage.block(
        &slide.title,
        (LEFT, 96, COLUMN, 40),
        25.0,
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
        let top = 309 + index as i32 * 148;
        let start = 0.19 + index as f32 * 0.17;
        let color = if row.value > 0.0 {
            TEAL
        } else if row.value < 0.0 {
            CORAL
        } else {
            p.accent
        };
        change_mark(stage, (74.0, top as f32 + 28.0), row.value, color, start);
        stage.block(
            &row.primary,
            (118, top, 554, 46),
            32.0,
            Face::Bold,
            color,
            Align::Left,
            (start, start + 0.16),
        );
        stage.block(
            &row.secondary,
            (118, top + 47, 554, 92),
            27.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (start + 0.04, start + 0.22),
        );
    }
    if let Some(note) = slide.lines.first() {
        stage.block(
            note,
            (LEFT, 777, COLUMN, 54),
            23.0,
            Face::Medium,
            MUTED,
            Align::Left,
            (0.66, 0.82),
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

pub(super) fn cover(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    if let Some(height) = stage.art.as_ref().map(|art| art.height as i32) {
        let top = 900 - height;
        let shown = stage.ease(0.08, 0.72);
        let reveal = 900 - (height as f32 * shown) as i32;
        super::artwork::draw(stage, 0, top, (0, reveal, WIDTH, 900), 0.0);
        // Merge the illustration into the slate without putting type on its focal point.
        for y in top..(top + 110) {
            let opacity = 1.0 - (y - top) as f32 / 110.0;
            stage.canvas.rounded_rect(
                (0.0, y as f32, WIDTH as f32, (y + 1) as f32),
                0.0,
                p.bg.scaled_alpha(opacity),
            );
        }
    }
    stage.block(
        if slide.headline.is_empty() {
            "Your personal\npatch notes"
        } else {
            &slide.headline
        },
        (LEFT, 102, COLUMN, 284),
        80.0,
        Face::Serif,
        p.fg,
        Align::Left,
        (0.0, 0.32),
    );
    stage.block(
        if slide.stat_value.is_empty() {
            &slide.username
        } else {
            &slide.stat_value
        },
        (LEFT, 393, COLUMN, 70),
        43.0,
        Face::Bold,
        p.accent,
        Align::Left,
        (0.3, 0.52),
    );
    if let Some(note) = slide.lines.first() {
        stage.canvas.rounded_rect(
            (0.0, 774.0, WIDTH as f32, 900.0),
            0.0,
            p.bg.scaled_alpha(0.88),
        );
        stage.block(
            note,
            (LEFT, 796, COLUMN, 76),
            27.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (0.54, 0.8),
        );
    }
}

pub(super) fn award(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let scale = stage.pop(0.08, 0.37).max(0.0);
    let point = |x: f32, y: f32| (360.0 + x * scale, 235.0 + y * scale);
    // A faceted medal, split into lit and shadowed brass faces.
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
    stage
        .canvas
        .ring((360.0, 235.0), 70.0 * scale, 2.0 * scale, p.bg);
    // An ascending blade engraved into the medal.
    stage.canvas.polygon(
        &[
            point(0.0, -48.0),
            point(14.0, -22.0),
            point(7.0, 20.0),
            point(-7.0, 20.0),
            point(-14.0, -22.0),
        ],
        p.bg,
    );
    stage
        .canvas
        .stroke(&[point(-25.0, 24.0), point(25.0, 24.0)], 6.0 * scale, p.bg);
    stage
        .canvas
        .stroke(&[point(0.0, 22.0), point(0.0, 47.0)], 7.0 * scale, p.bg);
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
