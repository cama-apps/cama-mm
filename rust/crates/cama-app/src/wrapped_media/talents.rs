//! The talent tree: a trunk of level medallions with an option on each side.
//! It lights from the lowest level up, and the picked side of each level
//! stays lit.

use super::stage::{Align, Face, HEIGHT, Stage, WIDTH, ink_height, text_width};
use super::*;

const MARGIN: i32 = 40;
const COLUMN: i32 = WIDTH - MARGIN * 2;
const TOP: i32 = 88;
const BOTTOM: i32 = HEIGHT - 72;
const TITLE_HEIGHT: i32 = 78;
const TRUNK: f32 = WIDTH as f32 / 2.0;
const TRUNK_WIDTH: f32 = 14.0;
const MEDALLION: f32 = 46.0;
const PLATE: (i32, i32) = (250, 92);
/// The widest box an option's text is set in, and its design size.
const OPTION_BOX: (i32, i32) = (PLATE.0 - 28, PLATE.1 - 14);
const OPTION_TYPE: f32 = 27.0;
const INK: Rgba = Rgba::rgb(17, 16, 28);

/// The unlit tree: the card's color, darkened, so the canopy behind it and
/// the lit branches both stand apart from it.
fn unlit(background: Rgba) -> Rgba {
    let mix = |index: usize| {
        let (from, to) = (f32::from(background.0[index]), f32::from(INK.0[index]));
        (from + (to - from) * 0.42) as u8
    };
    Rgba::rgb(mix(0), mix(1), mix(2))
}

/// The y of the root, of the top level's medallion, and the distance from
/// one level to the next.
fn levels(slide: &WrappedSlideData) -> (i32, i32, i32) {
    let count = slide.rows.len().clamp(1, 4) as i32;
    let root = if slide.lines.is_empty() {
        BOTTOM - 8
    } else {
        BOTTOM - 46
    };
    let highest = TOP + TITLE_HEIGHT + 18 + PLATE.1 / 2;
    let lowest = root - PLATE.1 / 2 - 24;
    let pitch = if count > 1 {
        ((lowest - highest) / (count - 1)).min(176)
    } else {
        0
    };
    let spare = lowest - highest - pitch * (count - 1);
    // A lone level sits where the eye lands, not at the foot of the card.
    let first = highest + if count > 1 { spare / 2 } else { spare / 3 };
    (root, first, pitch)
}

/// The time between one level lighting and the next, on the card's timeline.
fn beat(count: usize) -> f32 {
    (0.62 / count.max(1) as f32).min(0.17)
}

/// When level `index` of `count` lights: the bottom row first, then upward.
fn lights_at(index: usize, count: usize) -> f32 {
    0.26 + (count - 1 - index) as f32 * beat(count)
}

pub(super) fn paint(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    stage.block(
        &slide.title,
        (MARGIN, TOP, COLUMN, TITLE_HEIGHT),
        62.0,
        Face::Black,
        p.fg,
        Align::Left,
        (0.0, 0.1),
    );
    let rows = &slide.rows[..slide.rows.len().min(4)];
    if let Some(note) = slide.lines.first() {
        stage.block(
            note,
            (MARGIN, BOTTOM - 34, COLUMN, 34),
            23.0,
            Face::Medium,
            p.fg,
            Align::Center,
            (0.86, 0.98),
        );
    }
    if rows.is_empty() {
        return;
    }
    let (root, first, pitch) = levels(slide);
    let center = |index: usize| (first + index as i32 * pitch) as f32;
    let count = rows.len();
    let beat = beat(count);
    let lights_at = |index: usize| lights_at(index, count);
    // The tree is lit from its root to the highest level with a pick.
    let highest = rows.iter().position(|row| row.value != 0.0);

    // The unlit tree stands first: trunk, then each level from the root up.
    let grown = stage.ease(0.02, 0.18);
    let root = root as f32;
    let crown = center(0);
    trunk(stage, root, root + (crown - root) * grown, unlit(p.bg));
    if let Some(highest) = highest {
        let mut lit = root;
        for index in (highest..count).rev() {
            let reached = stage.ease(lights_at(index) - beat * 0.8, lights_at(index));
            lit += (center(index) - lit) * reached;
            if reached < 1.0 {
                break;
            }
        }
        if stage.t > 0.14 {
            trunk(stage, root, lit, p.accent);
        }
    }
    for (index, row) in rows.iter().enumerate().rev() {
        let order = (count - 1 - index) as f32;
        let appears = 0.06 + order * 0.035;
        let lit = highest.is_some_and(|highest| index >= highest)
            && (stage.t >= 1.0 || stage.t >= lights_at(index));
        level(
            stage,
            row,
            center(index),
            appears,
            lit.then(|| lights_at(index)),
        );
    }
}

/// The trunk from `root` up to `top`.
fn trunk(stage: &mut Stage<'_>, root: f32, top: f32, color: Rgba) {
    if top >= root {
        return;
    }
    let half = TRUNK_WIDTH / 2.0;
    stage
        .canvas
        .rounded_rect((TRUNK - half, top, TRUNK + half, root), half, color);
}

/// One level: its medallion on the trunk and an option plate on each side.
/// `lit_at` is when the level lit, if it has.
fn level(stage: &mut Stage<'_>, row: &WrappedRow, y: f32, appears: f32, lit_at: Option<f32>) {
    let p = stage.palette;
    let sides = [
        (&row.primary, MARGIN, row.value < 0.0),
        (&row.secondary, WIDTH - MARGIN - PLATE.0, row.value > 0.0),
    ];
    let (dark, quiet) = (unlit(p.bg), p.fg.scaled_alpha(0.7));
    for (text, left, picked) in sides {
        // Plates open outward from the trunk.
        let open = stage.ease(appears + 0.02, appears + 0.16);
        if open <= 0.0 {
            continue;
        }
        let from_trunk = left < WIDTH / 2;
        let plate = |amount: f32| {
            let width = PLATE.0 as f32 * amount;
            let (near, far) = if from_trunk {
                ((left + PLATE.0) as f32 - width, (left + PLATE.0) as f32)
            } else {
                (left as f32, left as f32 + width)
            };
            (
                near,
                y - PLATE.1 as f32 / 2.0,
                far,
                y + PLATE.1 as f32 / 2.0,
            )
        };
        let (inner, outer) = if from_trunk {
            ((left + PLATE.0) as f32, TRUNK - MEDALLION + 4.0)
        } else {
            (TRUNK + MEDALLION - 4.0, left as f32)
        };
        let branch = (inner, y - 5.0, outer, y + 5.0);
        stage.canvas.rounded_rect(branch, 0.0, dark);
        stage.canvas.rounded_rect(plate(open), 22.0, dark);
        let filled = match lit_at {
            Some(at) if picked => stage.ease(at + 0.02, at + 0.14),
            _ => 0.0,
        };
        if filled > 0.0 {
            stage.canvas.rounded_rect(branch, 0.0, p.accent);
            stage.canvas.rounded_rect(plate(filled), 22.0, p.accent);
        }
        let ink = if filled > 0.5 { p.on_accent } else { quiet };
        stage.centered_block(
            text,
            (
                left + (PLATE.0 - OPTION_BOX.0) / 2,
                y as i32 - OPTION_BOX.1 / 2,
                OPTION_BOX.0,
                OPTION_BOX.1,
            ),
            OPTION_TYPE,
            Face::Bold,
            ink,
            Align::Center,
            (appears + 0.08, appears + 0.2),
        );
    }
    let grow = stage.pop(appears, appears + 0.14).max(0.0);
    if stage.t < appears || grow <= 0.0 {
        return;
    }
    // A level swells as it lights, then settles.
    let pulse = lit_at.map_or(0.0, |at| 1.0 - stage.ease(at, at + 0.12));
    let radius = MEDALLION * grow * (1.0 + pulse * 0.2);
    let (ring, figure) = if lit_at.is_some() {
        for (reach, strength) in [(20.0, 0.14), (11.0, 0.2)] {
            let halo = p.accent.scaled_alpha(strength);
            stage.canvas.disc((TRUNK, y), radius + reach, halo);
        }
        (p.accent, p.accent)
    } else {
        (dark, quiet)
    };
    stage.canvas.disc((TRUNK, y), radius, ring);
    stage.canvas.disc((TRUNK, y), radius - 7.0, INK);
    let tag = row.tag.trim();
    if grow > 0.8 && !tag.is_empty() {
        let widest = (MEDALLION - 12.0) * 2.0;
        let natural = text_width(tag, 46.0, Face::Display);
        let size = 46.0 * (widest / natural).min(1.0);
        stage.line(
            tag,
            TRUNK - text_width(tag, size, Face::Display) / 2.0,
            y + ink_height(tag, size, Face::Display) / 2.0,
            size,
            Face::Display,
            figure,
            (0, HEIGHT),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapped_media::stage::fit_text;

    #[test]
    fn every_level_stays_between_the_title_and_the_footer() {
        for count in 1..=4 {
            for note in [false, true] {
                let mut slide = WrappedSlideData::new("story_talents", "Talents");
                slide.rows = vec![WrappedRow::default(); count];
                if note {
                    slide.lines = vec!["Level 25 not reached.".to_owned()];
                }
                let (root, first, pitch) = levels(&slide);
                assert!(first - PLATE.1 / 2 >= TOP + TITLE_HEIGHT);
                let last = first + (count as i32 - 1) * pitch;
                assert!(last + PLATE.1 / 2 < root, "{count} levels");
                assert!(root <= BOTTOM);
                assert!(count == 1 || pitch >= PLATE.1 + 20, "plates {pitch} apart");
            }
        }
    }

    #[test]
    fn the_tree_lights_from_the_lowest_level_up_and_keeps_each_pick_lit() {
        let mut slide = WrappedSlideData::new("story_talents", "Talents");
        slide.rows = [("25", -1.0), ("20", 1.0), ("15", 1.0), ("10", -1.0)]
            .into_iter()
            .map(|(level, pick)| {
                let mut row = WrappedRow::new("+8 Strength", "+20 Damage");
                row.tag = level.to_owned();
                row.value = pick;
                row
            })
            .collect();
        let palette = stage::palette(slide.theme);
        let (lit, dark) = (palette.accent.0, unlit(palette.bg).0);
        let (_, first, pitch) = levels(&slide);
        // Just inside each plate's outer edge, clear of its text.
        let probe = |canvas: &Canvas, index: usize, right: bool| {
            let x = if right {
                WIDTH - MARGIN - 8
            } else {
                MARGIN + 8
            } as usize;
            let y = (first + index as i32 * pitch) as usize;
            canvas.pixels[(y * canvas.width + x) * 4..][..4].to_vec()
        };
        let mut stage = Stage::new(&slide);
        // Before level 20 lights, the two levels under it have, and only on
        // the side that was picked.
        let part = stage.paint(lights_at(1, 4) - 0.005).clone();
        assert_eq!(probe(&part, 3, false), lit, "level 10, left");
        assert_eq!(probe(&part, 3, true), dark);
        assert_eq!(probe(&part, 2, true), lit, "level 15, right");
        assert_eq!(probe(&part, 2, false), dark);
        for side in [false, true] {
            assert_eq!(probe(&part, 1, side), dark, "level 20 waits");
            assert_eq!(probe(&part, 0, side), dark, "level 25 waits");
        }
        let done = stage.paint(1.0).clone();
        for (index, right) in [(0, false), (1, true), (2, true), (3, false)] {
            assert_eq!(probe(&done, index, right), lit, "level {index}");
            assert_eq!(probe(&done, index, !right), dark, "level {index}");
        }
        // A level nobody reached stays dark, and so does the trunk up to it.
        slide.rows[0].value = 0.0;
        let short = stage::render_still(&slide);
        assert_eq!(probe(&short, 0, false), dark);
        assert_eq!(probe(&short, 0, true), dark);
        let trunk = |canvas: &Canvas, above: usize| {
            let y = (first + above as i32 * pitch + pitch / 2) as usize;
            canvas.pixels[(y * canvas.width + WIDTH as usize / 2) * 4..][..4].to_vec()
        };
        assert_eq!(trunk(&short, 0), dark, "between levels 25 and 20");
        assert_eq!(trunk(&short, 1), lit, "between levels 20 and 15");
        assert_eq!(trunk(&done, 0), lit);
    }

    #[test]
    fn eighteen_character_options_fit_their_plates() {
        for option in [
            "+25 Attack Speed",
            "+1.5s Hex Duration",
            "-2s Blink Cooldown",
            "+30 Movement Speed",
            "+40 Sonic Wave Dmg",
            "+400 Night Vision",
        ] {
            assert!(option.chars().count() <= 18);
            let (size, rows) =
                fit_text(option, Face::Bold, OPTION_TYPE, OPTION_BOX.0, OPTION_BOX.1);
            assert!(size >= 24.0, "{option:?} set at {size}");
            assert_eq!(rows.join(" "), option, "{option:?} breaks inside a word");
        }
    }
}
