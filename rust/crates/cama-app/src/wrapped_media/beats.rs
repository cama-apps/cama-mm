//! Two-beat cards: a setup with its number, a full-card color wipe, then a
//! meme punchline. Treatments use only type, hero art, and clean graphics:
//! chart lines, bars, tiles, letterbox bars, a keycap, and a rank marker.
//!
//! Timeline: the setup reveals over 0.00-0.30 and holds to 0.48, the wipe
//! runs 0.48-0.56, the punchline animates over 0.56-0.92, and the card holds
//! on the finished punchline, which is the still. The GIF plays those marks
//! at 1.0 s, 5.6 s, 6.08 s, and 7.2 s (`motion::TWO_BEAT_PACE`).

use super::stage::{
    Align, DESCENT, FIRST_BASELINE, FOOTER_NAME, Face, HEIGHT, PILL_HEIGHT, Stage, WIDTH,
    block_height, count_up, fit_text, fit_text_within, pill_extent, pill_size, text_width,
};
use super::*;

mod memes;

const MARGIN: i32 = 40;
const COLUMN: i32 = WIDTH - MARGIN * 2;
/// Content stays between the year label and the player's name.
const TOP: i32 = 100;
const BOTTOM: i32 = HEIGHT - 72;
const WIPE: (f32, f32) = (0.48, 0.56);
/// How far the wipe's leading edge leans across the card.
const SLANT: f32 = 90.0;
const WIPE_STRIPE: f32 = 44.0;
/// The setup's hero frame: border, offset shadow, and corner radius.
const FRAME_BORDER: i32 = 8;
const FRAME_SHADOW: i32 = 11;
const FRAME_RADIUS: f32 = 26.0;
/// A Plain punchline's credit chip: its gap below the type and its size.
const CHIP_GAP: i32 = 30;
const CHIP_SIZE: f32 = 30.0;
/// A Plain punchline starts here when the hero stands beside it, leaving
/// the lower card to the hero.
const BESIDE_TYPE_TOP: i32 = TOP + 12;

const WHITE: Rgba = Rgba::rgb(255, 255, 255);
const INK: Rgba = Rgba::rgb(20, 10, 46);
const LIME: Rgba = Rgba::rgb(200, 255, 60);
const GREEN: Rgba = Rgba::rgb(61, 220, 132);
const RED: Rgba = Rgba::rgb(255, 77, 94);
const PINK: Rgba = Rgba::rgb(255, 157, 230);

/// A punchline's colors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Look {
    pub(super) bg: Rgba,
    /// The punchline and other loud type.
    pub(super) ink: Rgba,
    /// Chrome and the credit line.
    pub(super) quiet: Rgba,
    /// The treatment's graphic: a line, bar, tile, or marker.
    pub(super) signal: Rgba,
}

pub(super) fn look(theme: WrappedTheme, treatment: WrappedPunch) -> Look {
    let rgb = Rgba::rgb;
    let (bg, ink, quiet, signal) = match treatment {
        // The setup's contrast colors: ink and coral for a roast, yellow and
        // ink for a popoff.
        WrappedPunch::Plain => {
            let p = stage::palette(theme);
            (p.accent, p.on_accent, p.on_accent, p.on_accent)
        }
        WrappedPunch::Cinema => (rgb(0, 0, 0), WHITE, rgb(150, 150, 150), WHITE),
        WrappedPunch::GoldGraph => (rgb(15, 20, 32), WHITE, rgb(138, 147, 168), GREEN),
        WrappedPunch::Stonks => (rgb(11, 43, 29), GREEN, rgb(150, 196, 170), GREEN),
        WrappedPunch::NotStonks => (rgb(11, 43, 29), RED, rgb(150, 196, 170), RED),
        WrappedPunch::Calculated => (rgb(27, 17, 64), WHITE, rgb(169, 159, 201), LIME),
        WrappedPunch::TaskFailed => (rgb(24, 9, 14), WHITE, rgb(184, 156, 166), RED),
        WrappedPunch::LossTiles => (
            rgb(255, 245, 225),
            INK,
            rgb(107, 100, 117),
            rgb(226, 35, 62),
        ),
        WrappedPunch::PressF => (
            rgb(22, 24, 29),
            WHITE,
            rgb(154, 160, 171),
            rgb(236, 238, 242),
        ),
        WrappedPunch::SkillIssue => (rgb(92, 33, 240), WHITE, rgb(214, 204, 255), LIME),
        WrappedPunch::Clock => (rgb(8, 8, 14), WHITE, rgb(138, 138, 153), rgb(255, 59, 48)),
    };
    Look {
        bg,
        ink,
        quiet,
        signal,
    }
}

#[derive(Clone, Copy)]
enum Case {
    Keep,
    Upper,
    Lower,
}

/// Designed type for one text slot: its face and size, the smallest size the
/// layout accepts, the box height that holds it, and its letter case.
#[derive(Clone, Copy)]
struct Type {
    face: Face,
    size: f32,
    minimum: f32,
    height: i32,
    case: Case,
}

impl Type {
    const fn new(face: Face, size: f32, minimum: f32, height: i32, case: Case) -> Self {
        Self {
            face,
            size,
            minimum,
            height,
            case,
        }
    }

    fn apply(self, value: &str) -> String {
        match self.case {
            Case::Keep => value.to_owned(),
            Case::Upper => value.to_uppercase(),
            Case::Lower => value.to_lowercase(),
        }
    }

    /// The design size, lowered just enough for the widest word of `text` to
    /// fit the column, so a long word shrinks instead of breaking mid-word,
    /// but never below the minimum: a word too wide even there breaks.
    fn size_for(self, text: &str) -> f32 {
        let widest = text
            .split_whitespace()
            .map(|word| text_width(word, self.size, self.face))
            .fold(0.0_f32, f32::max);
        if widest <= COLUMN as f32 {
            self.size
        } else {
            (self.size * COLUMN as f32 / widest)
                .floor()
                .max(self.minimum)
        }
    }
}

/// The setup line above the number: two lines at most.
const SETUP_TYPE: Type = Type::new(Face::Black, 50.0, 44.0, 112, Case::Keep);
const LABEL_TYPE: Type = Type::new(Face::Bold, 38.0, 26.0, 92, Case::Keep);
const CREDIT_TYPE: Type = Type::new(Face::Bold, 30.0, 18.0, 40, Case::Keep);
const VALUE_SIZE: f32 = 200.0;

fn punch_type(treatment: WrappedPunch) -> Type {
    match treatment {
        WrappedPunch::Plain => Type::new(Face::Black, 112.0, 72.0, 440, Case::Keep),
        WrappedPunch::Cinema => Type::new(Face::Serif, 84.0, 56.0, 214, Case::Upper),
        WrappedPunch::Stonks | WrappedPunch::NotStonks => {
            Type::new(Face::Display, 150.0, 96.0, 300, Case::Upper)
        }
        WrappedPunch::GoldGraph => Type::new(Face::Black, 84.0, 60.0, 190, Case::Lower),
        _ => Type::new(Face::Black, 84.0, 60.0, 190, Case::Keep),
    }
}

/// `value` fitted to `style`: the size it is set at, never below the
/// minimum, and its rows.
fn fitted(value: &str, style: Type) -> (f32, Vec<String>) {
    let text = style.apply(value);
    let start = style.size_for(&text);
    fit_text_within(
        &text,
        style.face,
        start,
        style.minimum,
        COLUMN,
        style.height,
    )
}

/// Whether `value` fits `style` at no less than its minimum size, whole words
/// only.
fn fits(value: &str, style: Type) -> bool {
    let (size, rows) = fitted(value, style);
    let words = style.apply(value);
    size >= style.minimum
        && rows.join(" ") == words.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn copy_fits(setup: &str, punch: &str) -> bool {
    fits(setup, SETUP_TYPE)
        && WrappedPunch::ALL
            .iter()
            .all(|treatment| fits(punch, punch_type(*treatment)))
}

/// Whether the unit under the number and the fact line fit where a
/// `treatment` card sets them: the label, then the credit as a cinema
/// caption or under the punchline. A treatment missing its data falls back to
/// Plain, so every credit must also fit a Plain card's chip.
pub(super) fn facts_fit(label: &str, credit: &str, treatment: WrappedPunch) -> bool {
    let set = match treatment {
        WrappedPunch::Plain => true,
        WrappedPunch::Cinema => memes::caption_fit(&credit.to_uppercase()).0 >= CREDIT_TYPE.minimum,
        _ => fits(credit, CREDIT_TYPE),
    };
    fits(label, LABEL_TYPE) && pill_size(credit, COLUMN, CHIP_SIZE) >= CREDIT_TYPE.minimum && set
}

/// The height `value` occupies once fitted to `style`.
fn text_height(value: &str, style: Type) -> i32 {
    if value.trim().is_empty() {
        return 0;
    }
    let (size, rows) = fitted(value, style);
    block_height(rows.len(), size, style.face).ceil() as i32
}

/// `value` set in `style` across the column from `y`, rising into place
/// during `rise`. Returns its bottom.
fn text(
    stage: &mut Stage<'_>,
    value: &str,
    y: i32,
    style: Type,
    color: Rgba,
    align: Align,
    rise: (f32, f32),
) -> i32 {
    let (size, rows) = fitted(value, style);
    let rect = (MARGIN, y, COLUMN, style.height);
    stage.block(&rows.join("\n"), rect, size, style.face, color, align, rise)
}

/// The top of a block `height` tall, centered between the chrome bands.
fn centered(height: i32) -> i32 {
    TOP + ((BOTTOM - TOP - height) / 2).max(0)
}

/// Spin a number up from zero. A clock reading would pass through impossible
/// times, so it stays put.
fn spun(value: &str, progress: f32) -> String {
    if value.contains(':') {
        value.to_owned()
    } else {
        count_up(value, progress)
    }
}

pub(super) fn paint(stage: &mut Stage<'_>) {
    let slide = stage.slide;
    let Some(beat) = slide.beat.as_ref() else {
        return;
    };
    let look = look(slide.theme, beat.treatment);
    let wipe = stage.ease(WIPE.0, WIPE.1);
    if wipe < 1.0 {
        setup(stage, beat);
    }
    if wipe >= 1.0 {
        stage.canvas.clear(look.bg);
    } else if wipe > 0.0 {
        // A stripe of the setup's shape color leads the punchline's color in.
        let edge = wipe_edge(wipe);
        for (lead, color) in [(WIPE_STRIPE, setup_shape(stage).2), (0.0, look.bg)] {
            stage.canvas.polygon(
                &[
                    (0.0, edge - SLANT / 2.0 - lead),
                    (WIDTH as f32, edge + SLANT / 2.0 - lead),
                    (WIDTH as f32, HEIGHT as f32),
                    (0.0, HEIGHT as f32),
                ],
                color,
            );
        }
    }
    if wipe > 0.0 {
        hero_beside(stage, wipe);
    }
    if stage.t < WIPE.1 {
        return;
    }
    match beat.treatment {
        WrappedPunch::Plain => plain(stage, beat, look),
        WrappedPunch::Cinema => memes::cinema(stage, beat, look),
        WrappedPunch::GoldGraph => memes::gold_graph(stage, beat, look),
        WrappedPunch::Stonks | WrappedPunch::NotStonks => memes::stonks(stage, beat, look),
        WrappedPunch::Calculated => memes::calculated(stage, beat, look),
        WrappedPunch::TaskFailed => memes::task_failed(stage, beat, look),
        WrappedPunch::LossTiles => memes::loss_tiles(stage, beat, look),
        WrappedPunch::PressF => memes::press_f(stage, beat, look),
        WrappedPunch::SkillIssue => memes::skill_issue(stage, beat, look),
        WrappedPunch::Clock => memes::clock(stage, beat, look),
    }
}

/// The wipe edge's height at the middle of the card.
fn wipe_edge(wipe: f32) -> f32 {
    let travel = HEIGHT as f32 + SLANT + WIPE_STRIPE;
    HEIGHT as f32 + SLANT / 2.0 - travel * wipe
}

/// The punchline's hero, uncovered by the wipe as its color sweeps up and
/// then held still, so the finished card keeps it.
fn hero_beside(stage: &mut Stage<'_>, wipe: f32) {
    let Some(placed) = stage.punch_art.take() else {
        return;
    };
    // Below the slanted edge's lowest point, only the new color shows.
    let uncovered = if wipe >= 1.0 {
        0
    } else {
        (wipe_edge(wipe) + SLANT / 2.0).ceil() as i32
    };
    let (width, height) = (placed.art.width as i32, placed.art.height as i32);
    let reveal = (
        placed.left,
        placed.top.max(uncovered),
        placed.left + width,
        placed.top + height,
    );
    let corner = placed.radius;
    super::artwork::paint(
        &mut stage.canvas,
        &placed.art,
        placed.left,
        placed.top,
        reveal,
        corner,
    );
    stage.punch_art = Some(placed);
}

/// The boxes, `(left, top, right, bottom)`, that a Plain punchline's type,
/// its credit chip and the footer name cover when a hero stands beside it.
/// `None` for the meme treatments, whose graphics need the room.
pub(super) fn punch_art_clearance(slide: &WrappedSlideData) -> Option<Vec<(i32, i32, i32, i32)>> {
    let beat = slide.beat.as_ref()?;
    if beat.treatment != WrappedPunch::Plain {
        return None;
    }
    let style = punch_type(WrappedPunch::Plain);
    let text = style.apply(&beat.punch);
    let (size, rows) = fit_text(
        &text,
        style.face,
        style.size_for(&text),
        COLUMN,
        style.height,
    );
    let advance = size * style.face.leading();
    let mut boxes = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let top = BESIDE_TYPE_TOP as f32 + index as f32 * advance;
            let width = text_width(row, size, style.face).ceil() as i32;
            let bottom = (top + size * (FIRST_BASELINE + DESCENT)).ceil() as i32;
            (MARGIN, top as i32, MARGIN + width, bottom)
        })
        .collect::<Vec<_>>();
    if !beat.credit.trim().is_empty() {
        let top = BESIDE_TYPE_TOP + text_height(&beat.punch, style) + CHIP_GAP;
        let (width, height) = pill_extent(&beat.credit, COLUMN, CHIP_SIZE);
        let (right, bottom) = (MARGIN + width.ceil() as i32, top + height.ceil() as i32);
        boxes.push((MARGIN, top, right, bottom));
    }
    if !slide.username.trim().is_empty() {
        let (top, height, size) = FOOTER_NAME;
        let width = text_width(&slide.username, size, Face::Bold).min(COLUMN as f32);
        boxes.push((MARGIN, top, MARGIN + width.ceil() as i32, top + height));
    }
    Some(boxes)
}

/// Chrome ink for whichever beat is behind height `y`.
pub(super) fn chrome_ink(stage: &Stage<'_>, y: i32) -> Rgba {
    let Some(beat) = stage.slide.beat.as_ref() else {
        return stage.palette.fg;
    };
    let wipe = stage.ease(WIPE.0, WIPE.1);
    if wipe >= 1.0 || (wipe > 0.0 && y as f32 >= wipe_edge(wipe)) {
        look(stage.slide.theme, beat.treatment).quiet
    } else {
        stage.palette.fg
    }
}

/// The setup's one large soft shape: its center, radius, and color. It sits in
/// a corner clear of the type.
fn setup_shape(stage: &Stage<'_>) -> ((f32, f32), f32, Rgba) {
    match stage.slide.theme {
        WrappedTheme::BeatPopoff => ((700.0, 10.0), 250.0, PINK),
        theme if theme.is_roast_beat() => ((640.0, 800.0), 260.0, stage.palette.soft),
        _ => ((700.0, 10.0), 250.0, stage.palette.soft),
    }
}

/// Beat one: hero art, the setup line, the big number, and its label.
fn setup(stage: &mut Stage<'_>, beat: &WrappedBeat) {
    let p = stage.palette;
    let grow = stage.pop(0.0, 0.22).max(0.0);
    if grow > 0.0 {
        let (center, radius, color) = setup_shape(stage);
        stage.canvas.disc(center, radius * grow, color);
    }
    let art = stage
        .art
        .as_ref()
        .map(|art| (art.width as i32, art.height as i32));
    let drain = matches!(
        beat.treatment,
        WrappedPunch::GoldGraph | WrappedPunch::NotStonks
    );
    let value_size = if beat.value.trim().is_empty() {
        0.0
    } else {
        let width = text_width(&beat.value, VALUE_SIZE, Face::Display);
        VALUE_SIZE.min(VALUE_SIZE * COLUMN as f32 / width)
    };
    let value_height = block_height(1, value_size, Face::Display).ceil() as i32;
    // Anton's figures barely descend, so the label tucks up under the number.
    let value_tuck = (value_size * 0.15) as i32;
    let art_height = art.map_or(0, |(_, height)| {
        height + FRAME_BORDER * 2 + FRAME_SHADOW + 30
    });
    let mut y = centered(
        art_height + text_height(&beat.setup, SETUP_TYPE) + value_height - value_tuck
            + text_height(&beat.setup_label, LABEL_TYPE)
            + if drain { 56 } else { 0 },
    );
    if let Some((width, height)) = art {
        // The framed hero slides in from the left edge.
        let enter = stage.ease(0.02, 0.16);
        let left = MARGIN - ((1.0 - enter) * (width + 120) as f32) as i32;
        let (outer_width, outer_height) = (width + FRAME_BORDER * 2, height + FRAME_BORDER * 2);
        for offset in [FRAME_SHADOW, 0] {
            let (x, top) = ((left + offset) as f32, (y + offset) as f32);
            let frame = (x, top, x + outer_width as f32, top + outer_height as f32);
            stage.canvas.rounded_rect(frame, FRAME_RADIUS, INK);
        }
        let (art_left, art_top) = (left + FRAME_BORDER, y + FRAME_BORDER);
        // Hero renders are cutouts; the frame's ink shows through behind them.
        super::artwork::draw(
            stage,
            art_left,
            art_top,
            (art_left, art_top, art_left + width, art_top + height),
            FRAME_RADIUS - FRAME_BORDER as f32,
        );
        y += art_height;
    }
    y = text(
        stage,
        &beat.setup,
        y,
        SETUP_TYPE,
        p.fg,
        Align::Left,
        (0.08, 0.2),
    );
    let shown = spun(&beat.value, stage.ease(0.1, 0.3));
    y = stage.stat_in(
        Face::Display,
        &beat.value,
        &shown,
        (MARGIN, y, COLUMN, value_height),
        VALUE_SIZE,
        p.accent,
        Align::Left,
        (0.1, 0.18),
    ) - value_tuck;
    y = text(
        stage,
        &beat.setup_label,
        y,
        LABEL_TYPE,
        p.fg,
        Align::Left,
        (0.18, 0.3),
    );
    if drain && stage.t >= 0.16 {
        // The lead fills up with the label, then drains away during the hold.
        let fill = stage.ease(0.16, 0.28) * (1.0 - stage.progress(0.3, 0.48).powi(2));
        let (left, top) = (MARGIN as f32, (y + 22) as f32);
        let track = (left, top, left + COLUMN as f32, top + 32.0);
        stage.canvas.rounded_rect(track, 16.0, Rgba([0, 0, 0, 51]));
        if fill > 0.0 {
            let filled = (left, top, left + COLUMN as f32 * fill, top + 32.0);
            stage.canvas.rounded_rect(filled, 16.0, LIME);
        }
    }
}

fn plain(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    let style = punch_type(WrappedPunch::Plain);
    let chip = if beat.credit.trim().is_empty() {
        0
    } else {
        CHIP_GAP + (CHIP_SIZE * PILL_HEIGHT) as i32
    };
    // With the hero standing below and beside it, the type keeps to the top.
    let y = if stage.punch_art.is_some() {
        BESIDE_TYPE_TOP
    } else {
        centered(text_height(&beat.punch, style) + chip)
    };
    let bottom = text(
        stage,
        &beat.punch,
        y,
        style,
        look.ink,
        Align::Left,
        (0.58, 0.72),
    );
    // The fact rides along in a chip, so the still stands on its own.
    let credit = &beat.credit;
    stage.pill(
        credit,
        MARGIN,
        bottom + CHIP_GAP,
        COLUMN,
        CHIP_SIZE,
        look.ink,
        look.bg,
        0.7,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `value` set in `style` stays at or above its minimum and inside its
    /// box: every row within the column, the block within its height.
    fn assert_readable(value: &str, style: Type) {
        let (size, rows) = fitted(value, style);
        assert!(size >= style.minimum, "{value:?} set at {size}");
        for row in &rows {
            assert!(
                text_width(row, size, style.face) <= COLUMN as f32,
                "{row:?} runs off the card"
            );
        }
        assert!(
            block_height(rows.len(), size, style.face) <= style.height as f32,
            "{value:?} spills out of its box"
        );
    }

    #[test]
    fn long_names_and_hero_credits_stay_readable_on_the_card() {
        // A 24-character name with nowhere to break, and the longest hero.
        let name = "Bartholomew_Featherstone";
        let credit = format!("27 / 22 / 34 · Keeper of the Light · Won with {name}");
        for treatment in WrappedPunch::ALL {
            let beat = WrappedBeat {
                setup: format!("{name} again."),
                value: "27".to_owned(),
                setup_label: format!("kills with {name}"),
                punch: format!("{name}."),
                credit: credit.clone(),
                treatment,
            };
            assert_readable(&beat.setup, SETUP_TYPE);
            assert_readable(&beat.setup_label, LABEL_TYPE);
            assert_readable(&beat.punch, punch_type(treatment));
            assert_readable(&beat.credit, CREDIT_TYPE);
            let mut slide = WrappedSlideData::new("story_distinct", "Combat");
            slide.layout = WrappedLayout::TwoBeat;
            slide.theme = WrappedTheme::BeatPopoff;
            slide.username = name.to_owned();
            slide.beat = Some(beat);
            render_wrapped_slide(&slide).expect("the card renders");
        }
    }
}
