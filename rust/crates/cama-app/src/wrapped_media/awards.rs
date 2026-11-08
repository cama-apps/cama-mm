//! A personal prize ticket: setup, trophy stamp, then the earned reveal.

use std::f32::consts::TAU;

use super::stage::{Align, Face, Stage, count_up};

pub(super) fn paint(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    stage.block(
        &slide.headline,
        (64, 100, 592, 104),
        35.0,
        Face::Bold,
        p.fg,
        Align::Center,
        (0.0, 0.2),
    );

    // The ticket snaps open only after the setup. Its fixed bands keep even
    // unusually long award names away from the evidence and player footer.
    let open = stage.ease(0.2, 0.46);
    let half = 320.0 * open;
    stage.canvas.polygon(
        &[
            (360.0 - half, 334.0),
            (360.0 + half, 318.0),
            (360.0 + half, 668.0),
            (360.0 - half, 684.0),
        ],
        p.accent,
    );
    if open > 0.0 {
        // Punched ticket edges, rather than a generic rounded card.
        stage.canvas.disc((360.0 - half, 500.0), 15.0, p.bg);
        stage.canvas.disc((360.0 + half, 500.0), 15.0, p.bg);
    }
    trophy(stage);
    stage.block(
        &slide.title,
        (72, 352, 576, 126),
        66.0,
        Face::Black,
        p.on_accent,
        Align::Center,
        (0.3, 0.52),
    );
    let spun = count_up(&slide.stat_value, stage.ease(0.48, 0.84));
    stage.stat(
        &slide.stat_value,
        &spun,
        (80, 480, 560, 136),
        118.0,
        p.on_accent,
        Align::Center,
        (0.46, 0.64),
    );
    stage.block(
        &slide.stat_label,
        (80, 610, 560, 52),
        29.0,
        Face::Bold,
        p.on_accent,
        Align::Center,
        (0.56, 0.76),
    );
    stage.block(
        &slide.badge,
        (72, 688, 576, 32),
        22.0,
        Face::Bold,
        p.accent,
        Align::Center,
        (0.64, 0.82),
    );
    for (index, line) in slide.lines.iter().take(2).enumerate() {
        stage.block(
            line,
            (60, 728 + index as i32 * 48, 600, 44),
            23.0,
            Face::Medium,
            p.fg,
            Align::Center,
            (0.7 + index as f32 * 0.06, 0.9 + index as f32 * 0.06),
        );
    }
}

/// A scalloped seal and a purpose-drawn cup, animated as a single stamp.
fn trophy(stage: &mut Stage<'_>) {
    let p = stage.palette;
    let scale = stage.pop(0.12, 0.42).max(0.0);
    if scale < 0.01 {
        return;
    }
    let center = (360.0, 264.0);
    let point = |x: f32, y: f32| (center.0 + x * scale, center.1 + y * scale);
    let seal = (0..40)
        .map(|index| {
            let angle = index as f32 / 40.0 * TAU;
            let radius = if index % 2 == 0 { 70.0 } else { 61.0 };
            point(angle.cos() * radius, angle.sin() * radius)
        })
        .collect::<Vec<_>>();
    for side in [-1.0, 1.0] {
        stage.canvas.polygon(
            &[
                point(side * 16.0, 35.0),
                point(side * 46.0, 35.0),
                point(side * 64.0, 88.0),
                point(side * 38.0, 73.0),
                point(side * 24.0, 92.0),
            ],
            p.fg,
        );
    }
    stage.canvas.polygon(&seal, p.accent);
    stage.canvas.ring(center, 53.0 * scale, 2.0 * scale, p.bg);
    // Open cup handles, bowl, stem and base.
    for side in [-1.0, 1.0] {
        stage.canvas.stroke(
            &[
                point(side * 24.0, -24.0),
                point(side * 39.0, -24.0),
                point(side * 34.0, -2.0),
                point(side * 20.0, 5.0),
            ],
            6.0 * scale,
            p.bg,
        );
    }
    stage.canvas.polygon(
        &[
            point(-26.0, -31.0),
            point(26.0, -31.0),
            point(18.0, 6.0),
            point(0.0, 18.0),
            point(-18.0, 6.0),
        ],
        p.bg,
    );
    stage
        .canvas
        .stroke(&[point(0.0, 12.0), point(0.0, 29.0)], 8.0 * scale, p.bg);
    stage
        .canvas
        .stroke(&[point(-18.0, 32.0), point(18.0, 32.0)], 7.0 * scale, p.bg);

    // Four sparks arrive with the stamp, outside the reading area.
    for (x, y) in [
        (232.0, 248.0),
        (488.0, 248.0),
        (264.0, 298.0),
        (456.0, 298.0),
    ] {
        let size = 11.0 * stage.ease(0.22, 0.5);
        stage.canvas.polygon(
            &[
                (x, y - size),
                (x + size * 0.4, y),
                (x, y + size),
                (x - size * 0.4, y),
            ],
            p.accent,
        );
    }
}
