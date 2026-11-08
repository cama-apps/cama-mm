//! Moving backgrounds. Each chapter has one idea that settles into a composition
//! the type can sit on: rings for the queue, the Radiant treeline for your
//! squad and Dire thorns for your rivals, falling coins for gamba, strata for
//! the dig, a passing eclipse for mafia.

use std::f32::consts::{PI, TAU};

use super::stage::{HEIGHT, Stage, WIDTH};
use super::*;

const W: f32 = WIDTH as f32;
const H: f32 = HEIGHT as f32;

pub(super) fn paint(stage: &mut Stage<'_>) {
    match stage.slide.theme {
        WrappedTheme::Plain => {}
        WrappedTheme::Cover => record_sleeve(stage),
        WrappedTheme::Months => waves(stage),
        WrappedTheme::Queue | WrappedTheme::QueueAlt => rings(stage),
        WrappedTheme::Squad => treeline(stage),
        WrappedTheme::Heroes | WrappedTheme::Persona => burst(stage),
        WrappedTheme::Rivals => thorns(stage),
        WrappedTheme::Rating => grid(stage),
        WrappedTheme::GambaUp | WrappedTheme::GambaDown => coins(stage),
        WrappedTheme::Dig | WrappedTheme::DigDeep => strata(stage),
        WrappedTheme::Pet => bubbles(stage),
        WrappedTheme::PetGrave => graveyard(stage),
        WrappedTheme::Mafia | WrappedTheme::MafiaRed => eclipse(stage),
        WrappedTheme::Recap => confetti(stage),
        // Two-beat cards paint their own shapes for each beat.
        WrappedTheme::Awards | WrappedTheme::BeatRoast | WrappedTheme::BeatPopoff => {}
    }
}

/// A cropped record sleeve: the opening spins into place around the story.
fn record_sleeve(stage: &mut Stage<'_>) {
    let p = stage.palette;
    let enter = stage.ease(0.0, 0.7);
    let center = (W + 80.0 - 80.0 * enter, 440.0);
    for index in 0..5 {
        let radius = 180.0 + index as f32 * 56.0;
        stage.canvas.ring(center, radius * enter, 22.0, p.soft);
    }
    let sweep = stage.ease(0.15, 0.65);
    stage.canvas.polygon(
        &[
            (W, 610.0),
            (W, 690.0),
            (W - 400.0 * sweep, 900.0),
            (W - 520.0 * sweep, 900.0),
        ],
        p.accent,
    );
}

/// Small deterministic sequence: a card always moves the same way.
struct Scatter(u32);

impl Scatter {
    fn new(seed: usize) -> Self {
        Self(0x9E37_79B9 ^ (seed as u32).wrapping_mul(0x85EB_CA6B))
    }

    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 % 10_000) as f32 / 10_000.0
    }
}

fn bounce(progress: f32) -> f32 {
    const N: f32 = 7.5625;
    const D: f32 = 2.75;
    let p = progress.clamp(0.0, 1.0);
    if p < 1.0 / D {
        N * p * p
    } else if p < 2.0 / D {
        let q = p - 1.5 / D;
        N * q * q + 0.75
    } else if p < 2.5 / D {
        let q = p - 2.25 / D;
        N * q * q + 0.9375
    } else {
        let q = p - 2.625 / D;
        N * q * q + 0.984375
    }
}

fn rings(stage: &mut Stage<'_>) {
    let (bg, soft) = (stage.palette.bg, stage.palette.soft);
    let flip = stage.slide.chapter_index.is_multiple_of(2);
    let center = if flip {
        (W + 20.0, H - 60.0)
    } else {
        (-20.0, H - 120.0)
    };
    for index in (0..6).rev() {
        let grow = stage.ease(index as f32 * 0.07, 0.55 + index as f32 * 0.07);
        let radius = (150.0 + index as f32 * 118.0) * grow;
        let color = if index % 2 == 0 { soft } else { bg };
        stage.canvas.disc(center, radius, color);
    }
}

/// The Radiant treeline: pines rise along the bottom edge, back row first.
fn treeline(stage: &mut Stage<'_>) {
    let soft = stage.palette.soft;
    let mut scatter = Scatter::new(stage.slide.chapter_index + 11);
    for (row, (count, ground, scale)) in [(9, H - 64.0, 0.78), (7, H + 18.0, 1.0)]
        .into_iter()
        .enumerate()
    {
        for index in 0..count {
            let x = (index as f32 + 0.15 + scatter.next() * 0.7) / count as f32 * W;
            let tall = (150.0 + scatter.next() * 90.0) * scale;
            let start = 0.04 + row as f32 * 0.16 + index as f32 * 0.035;
            let rise = stage.pop(start, start + 0.34);
            if rise <= 0.0 {
                continue;
            }
            let base = ground + (1.0 - rise) * (tall + 120.0);
            pine(stage, (x, base), tall, soft);
        }
    }
}

/// The Dire's thorns: jagged spikes climb from the bottom edge, back row first.
fn thorns(stage: &mut Stage<'_>) {
    let soft = stage.palette.soft;
    let mut scatter = Scatter::new(stage.slide.chapter_index + 23);
    for (row, (count, ground, reach)) in [(11, H - 40.0, 150.0), (8, H + 10.0, 230.0)]
        .into_iter()
        .enumerate()
    {
        for index in 0..count {
            let x = (index as f32 + 0.1 + scatter.next() * 0.8) / count as f32 * W;
            let tall = reach * (0.55 + scatter.next() * 0.45);
            let lean = (scatter.next() - 0.5) * tall * 0.35;
            let half = 16.0 + scatter.next() * 14.0;
            let start = 0.04 + row as f32 * 0.14 + index as f32 * 0.03;
            let rise = stage.pop(start, start + 0.3);
            if rise <= 0.0 {
                continue;
            }
            let base = ground + (1.0 - rise) * (tall + 60.0);
            stage.canvas.polygon(
                &[(x - half, base), (x + half, base), (x + lean, base - tall)],
                soft,
            );
        }
    }
}

/// Three stacked tiers on a short trunk, `tall` pixels from ground to tip.
fn pine(stage: &mut Stage<'_>, (x, ground): (f32, f32), tall: f32, color: Rgba) {
    let trunk = tall * 0.14;
    stage.canvas.polygon(
        &[
            (x - tall * 0.05, ground),
            (x + tall * 0.05, ground),
            (x + tall * 0.05, ground - trunk - 4.0),
            (x - tall * 0.05, ground - trunk - 4.0),
        ],
        color,
    );
    for tier in 0..3 {
        let tier = tier as f32;
        let bottom = ground - trunk - tier * tall * 0.24;
        let half = tall * (0.34 - tier * 0.07);
        stage.canvas.polygon(
            &[
                (x - half, bottom),
                (x + half, bottom),
                (x, bottom - tall * 0.42),
            ],
            color,
        );
    }
}

fn waves(stage: &mut Stage<'_>) {
    let (bg, soft) = (stage.palette.bg, stage.palette.soft);
    let phase = stage.ease(0.0, 1.0) * PI * 1.6;
    for layer in 0..4 {
        let rise = stage.ease(layer as f32 * 0.08, 0.6 + layer as f32 * 0.08);
        let base = H + 40.0 - (560.0 - layer as f32 * 135.0) * rise;
        let color = if layer % 2 == 0 { soft } else { bg };
        let mut points = Vec::with_capacity(44);
        for step in 0..=40 {
            let x = step as f32 / 40.0 * W;
            let wave = (x / 150.0 + phase + layer as f32 * 1.3).sin() * 34.0
                + (x / 71.0 + phase * 1.7 + layer as f32).sin() * 12.0;
            points.push((x, base + wave));
        }
        points.push((W, H));
        points.push((0.0, H));
        stage.canvas.polygon(&points, color);
    }
}

fn burst(stage: &mut Stage<'_>) {
    let soft = stage.palette.soft;
    let spin = stage.ease(0.0, 1.0) * 0.55;
    let open = stage.ease(0.0, 0.5);
    let center = (W * 0.5, H * 0.56);
    const RAYS: usize = 14;
    for ray in 0..RAYS {
        let angle = ray as f32 / RAYS as f32 * TAU + spin;
        let width = TAU / RAYS as f32 * 0.5 * open;
        stage.canvas.polygon(
            &[
                center,
                (
                    center.0 + (angle - width / 2.0).cos() * 1100.0,
                    center.1 + (angle - width / 2.0).sin() * 1100.0,
                ),
                (
                    center.0 + (angle + width / 2.0).cos() * 1100.0,
                    center.1 + (angle + width / 2.0).sin() * 1100.0,
                ),
            ],
            soft,
        );
    }
}

fn grid(stage: &mut Stage<'_>) {
    let soft = stage.palette.soft;
    let shown = stage.ease(0.0, 0.3);
    for index in 0..6 {
        let y = 100.0 + index as f32 * 140.0;
        stage
            .canvas
            .rounded_rect((0.0, y, W * shown, y + 3.0), 0.0, soft);
    }
}

fn coin(stage: &mut Stage<'_>, center: (f32, f32), radius: f32, squash: f32) {
    let accent = stage.palette.accent;
    let rim = Rgba::rgb(
        (f32::from(accent.0[0]) * 0.72) as u8,
        (f32::from(accent.0[1]) * 0.72) as u8,
        (f32::from(accent.0[2]) * 0.72) as u8,
    );
    let wide = radius * squash.max(0.12);
    let oval = |r: f32, offset: f32| {
        (0..28)
            .map(|step| {
                let angle = step as f32 / 28.0 * TAU;
                (
                    center.0 + angle.cos() * wide * r,
                    center.1 + offset + angle.sin() * radius * r,
                )
            })
            .collect::<Vec<_>>()
    };
    stage.canvas.polygon(&oval(1.0, 4.0), rim);
    stage.canvas.polygon(&oval(1.0, 0.0), accent);
    stage.canvas.polygon(&oval(0.68, 0.0), rim);
    stage.canvas.polygon(&oval(0.56, 0.0), accent);
}

fn coins(stage: &mut Stage<'_>) {
    let mut scatter = Scatter::new(stage.slide.chapter_index);
    // Coins pile up in the bottom corner, clear of the copy above.
    for index in 0..9 {
        let start = index as f32 * 0.05;
        let fall = bounce(stage.progress_of(start, start + 0.5));
        let x = W * 0.58 + scatter.next() * (W * 0.46);
        let rest = H - 4.0 - scatter.next() * 30.0;
        let radius = 24.0 + scatter.next() * 18.0;
        let spin = (stage.t * 7.0 + index as f32).cos().abs();
        let settled = stage.t >= start + 0.5;
        let y = -60.0 + (rest + 60.0) * fall;
        coin(stage, (x, y), radius, if settled { 1.0 } else { spin });
    }
}

fn strata(stage: &mut Stage<'_>) {
    let deep = stage.slide.theme == WrappedTheme::DigDeep;
    let bands: &[[u8; 3]] = if deep {
        &[
            [58, 20, 66],
            [86, 24, 62],
            [122, 30, 48],
            [176, 46, 30],
            [232, 88, 20],
        ]
    } else {
        &[
            [86, 48, 26],
            [110, 66, 34],
            [136, 88, 46],
            [92, 88, 96],
            [66, 62, 74],
        ]
    };
    // The whole cross-section scrolls up, as if the card were still digging.
    let scroll = (1.0 - stage.ease(0.0, 0.85)) * 420.0;
    let mut scatter = Scatter::new(7);
    let band_top = 250.0;
    let thickness = 150.0;
    for (index, color) in bands.iter().enumerate() {
        let base = band_top + index as f32 * thickness + scroll;
        let mut points = vec![(0.0, H + 500.0), (W, H + 500.0)];
        let mut x = W;
        while x > -40.0 {
            points.push((x, base + scatter.next() * 38.0 - 19.0));
            x -= 48.0;
        }
        stage
            .canvas
            .polygon(&points, Rgba::rgb(color[0], color[1], color[2]));
    }
    // Gems and fossils that scroll past with the rock.
    let accent = stage.palette.accent;
    for index in 0..18 {
        let x = scatter.next() * W;
        let y = band_top + 40.0 + scatter.next() * (H - band_top) + scroll;
        let size = 4.0 + scatter.next() * 6.0;
        let color = if index % 3 == 0 {
            accent
        } else {
            accent.scaled_alpha(0.3)
        };
        stage.canvas.polygon(
            &[
                (x, y - size),
                (x + size * 0.8, y),
                (x, y + size),
                (x - size * 0.8, y),
            ],
            color,
        );
    }
}

fn heart(stage: &mut Stage<'_>, center: (f32, f32), size: f32, color: Rgba) {
    let lobe = size * 0.52;
    stage.canvas.disc(
        (center.0 - lobe * 0.92, center.1 - lobe * 0.35),
        lobe,
        color,
    );
    stage.canvas.disc(
        (center.0 + lobe * 0.92, center.1 - lobe * 0.35),
        lobe,
        color,
    );
    stage.canvas.polygon(
        &[
            (center.0 - size * 0.98, center.1 - lobe * 0.05),
            (center.0 + size * 0.98, center.1 - lobe * 0.05),
            (center.0, center.1 + size * 1.08),
        ],
        color,
    );
}

fn bubbles(stage: &mut Stage<'_>) {
    let soft = stage.palette.soft;
    let mut scatter = Scatter::new(11);
    for index in 0..7 {
        let grow = stage.pop(index as f32 * 0.06, 0.32 + index as f32 * 0.06);
        let (x, y) = (scatter.next() * W, 120.0 + scatter.next() * (H - 160.0));
        let radius = 70.0 + scatter.next() * 130.0;
        if stage.t >= index as f32 * 0.06 {
            stage.canvas.disc((x, y), radius * grow.max(0.0), soft);
        }
    }
    let accent = stage.palette.accent.scaled_alpha(0.55);
    for index in 0..7 {
        let x = 30.0 + scatter.next() * (W - 60.0);
        let size = 13.0 + scatter.next() * 16.0;
        let start = 0.05 + index as f32 * 0.07;
        // Hearts float up and off the card; the finished card keeps the type clear.
        let float = stage.progress_of(start, start + 0.45);
        if stage.t >= start && float < 1.0 {
            heart(stage, (x, H + 40.0 - float * (H + 120.0)), size, accent);
        }
    }
}

fn graveyard(stage: &mut Stage<'_>) {
    let soft = stage.palette.soft;
    let mut scatter = Scatter::new(3);
    for _ in 0..34 {
        let (x, y) = (scatter.next() * W, 60.0 + scatter.next() * (H * 0.62));
        let radius = 1.2 + scatter.next() * 2.2;
        stage.canvas.disc((x, y), radius, soft);
    }
    let rise = stage.ease(0.0, 0.55);
    let ground = H - 62.0 + (1.0 - rise) * 170.0;
    stage
        .canvas
        .rounded_rect((0.0, ground, W, H + 200.0), 0.0, soft);
    for index in 0..5 {
        let x = 26.0 + index as f32 * 150.0 + scatter.next() * 40.0;
        let height = 74.0 + scatter.next() * 56.0;
        let width = 76.0;
        let pop = stage.ease(0.08 * index as f32, 0.45 + 0.08 * index as f32);
        let top = ground - height * pop;
        stage
            .canvas
            .rounded_rect((x, top, x + width, ground + 2.0), 0.0, soft);
        stage.canvas.disc((x + width / 2.0, top), width / 2.0, soft);
    }
}

fn eclipse(stage: &mut Stage<'_>) {
    let (bg, fg) = (stage.palette.bg, stage.palette.fg);
    let mut scatter = Scatter::new(5);
    if stage.slide.theme == WrappedTheme::Mafia {
        for _ in 0..30 {
            let (x, y) = (scatter.next() * W, 60.0 + scatter.next() * (H - 120.0));
            let radius = 0.9 + scatter.next() * 1.7;
            stage.canvas.disc((x, y), radius, stage.palette.soft);
        }
    }
    let moon = (W - 90.0, 280.0);
    let radius = 240.0;
    // A dim moon: the type passes over it and must stay readable.
    let color = if stage.slide.theme == WrappedTheme::Mafia {
        fg.scaled_alpha(0.16)
    } else {
        stage.palette.soft
    };
    let appear = stage.ease(0.0, 0.4);
    stage.canvas.disc(moon, radius * appear, color);
    // The shadow crosses most of the moon and stops on a thin crescent.
    let slide = stage.ease(0.08, 0.82);
    let shadow = (moon.0 + 480.0 - slide * 560.0, moon.1 - slide * 30.0);
    stage.canvas.disc(shadow, radius * 1.02, bg);
}

fn confetti(stage: &mut Stage<'_>) {
    const COLORS: [[u8; 3]; 6] = [
        [198, 244, 50],
        [255, 77, 157],
        [255, 210, 30],
        [18, 216, 196],
        [255, 122, 26],
        [150, 100, 255],
    ];
    let mut scatter = Scatter::new(19);
    for index in 0..64 {
        let x = scatter.next() * W;
        // Pieces come to rest along the card's edges and leave the middle to
        // the type, which is what the finished share card shows.
        let settled_y = if index % 3 == 0 {
            scatter.next() * 18.0 + 2.0
        } else {
            H - scatter.next() * 46.0 - 66.0
        };
        let delay = scatter.next() * 0.3;
        let fall = stage.ease(delay, delay + 0.65);
        let y = settled_y - (1.0 - fall) * (H + 200.0) * (0.6 + scatter.next() * 0.4);
        let angle = scatter.next() * TAU + (1.0 - fall) * 9.0;
        let (long, short) = (7.0 + scatter.next() * 8.0, 3.0 + scatter.next() * 3.0);
        let (cos, sin) = (angle.cos(), angle.sin());
        let color = COLORS[index % COLORS.len()];
        stage.canvas.polygon(
            &[
                (x + cos * long - sin * short, y + sin * long + cos * short),
                (x - cos * long - sin * short, y - sin * long + cos * short),
                (x - cos * long + sin * short, y - sin * long - cos * short),
                (x + cos * long + sin * short, y + sin * long - cos * short),
            ],
            Rgba::rgb(color[0], color[1], color[2]),
        );
    }
}

impl Stage<'_> {
    fn progress_of(&self, start: f32, end: f32) -> f32 {
        if self.t >= 1.0 {
            return 1.0;
        }
        ((self.t - start) / (end - start)).clamp(0.0, 1.0)
    }
}
