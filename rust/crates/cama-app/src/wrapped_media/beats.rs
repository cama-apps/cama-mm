//! Two-beat cards: a setup with its number, a full-card color wipe, then a
//! meme punchline. Treatments use only type, hero art, and clean graphics:
//! chart lines, bars, tiles, letterbox bars, a keycap, and a rank marker.
//!
//! Timeline: the setup reveals over 0.00-0.30 and holds to 0.48, the wipe
//! runs 0.48-0.56, the punchline animates over 0.56-0.92, and the card holds
//! on the finished punchline, which is the still.

use super::stage::{
    Align, FIRST_BASELINE, Face, HEIGHT, Stage, WIDTH, block_height, count_up, fit_text,
    ink_height, text_width,
};
use super::*;

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

/// Whether `value` fits `style` at no less than its minimum size, whole words
/// only.
fn fits(value: &str, style: Type) -> bool {
    let text = style.apply(value);
    let (size, rows) = fit_text(&text, style.face, style.size, COLUMN, style.height);
    size >= style.minimum && rows.join(" ") == text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn copy_fits(setup: &str, punch: &str) -> bool {
    fits(setup, SETUP_TYPE)
        && WrappedPunch::ALL
            .iter()
            .all(|treatment| fits(punch, punch_type(*treatment)))
}

/// The height `value` occupies once fitted to `style`.
fn text_height(value: &str, style: Type) -> i32 {
    if value.trim().is_empty() {
        return 0;
    }
    let (size, rows) = fit_text(
        &style.apply(value),
        style.face,
        style.size,
        COLUMN,
        style.height,
    );
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
    let value = style.apply(value);
    let rect = (MARGIN, y, COLUMN, style.height);
    stage.block(&value, rect, style.size, style.face, color, align, rise)
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

/// Cubic ease-in-out, for the film-like letterbox and the chart draw.
fn in_out(progress: f32) -> f32 {
    if progress < 0.5 {
        4.0 * progress.powi(3)
    } else {
        1.0 - (2.0 - 2.0 * progress).powi(3) / 2.0
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
    if stage.t < WIPE.1 {
        return;
    }
    match beat.treatment {
        WrappedPunch::Plain => plain(stage, beat, look),
        WrappedPunch::Cinema => cinema(stage, beat, look),
        WrappedPunch::GoldGraph => gold_graph(stage, beat, look),
        WrappedPunch::Stonks | WrappedPunch::NotStonks => stonks(stage, beat, look),
        WrappedPunch::Calculated => calculated(stage, beat, look),
        WrappedPunch::TaskFailed => task_failed(stage, beat, look),
        WrappedPunch::LossTiles => loss_tiles(stage, beat, look),
        WrappedPunch::PressF => press_f(stage, beat, look),
        WrappedPunch::SkillIssue => skill_issue(stage, beat, look),
        WrappedPunch::Clock => clock(stage, beat, look),
    }
}

/// The wipe edge's height at the middle of the card.
fn wipe_edge(wipe: f32) -> f32 {
    let travel = HEIGHT as f32 + SLANT + WIPE_STRIPE;
    HEIGHT as f32 + SLANT / 2.0 - travel * wipe
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
        WrappedTheme::BeatRoast => ((640.0, 800.0), 260.0, stage.palette.soft),
        WrappedTheme::BeatPopoff => ((700.0, 10.0), 250.0, PINK),
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

/// Centers a treatment's graphic over its punchline and credit. Returns the
/// tops of the graphic, the punchline, and the credit.
fn stack(beat: &WrappedBeat, graphic: i32) -> (i32, i32, i32) {
    let gap = if graphic > 0 { 44 } else { 0 };
    let punch = text_height(&beat.punch, punch_type(beat.treatment));
    let credit = if beat.credit.trim().is_empty() {
        0
    } else {
        18 + CREDIT_TYPE.height
    };
    let top = centered(graphic + gap + punch + credit);
    (top, top + graphic + gap, top + graphic + gap + punch + 18)
}

/// The punchline rising into place, then its credit, from `start`. Starting
/// by 0.76 lets a two-line punchline and its credit settle by the 0.92 hold.
fn punchline(
    stage: &mut Stage<'_>,
    beat: &WrappedBeat,
    look: Look,
    (punch_top, credit_top): (i32, i32),
    align: Align,
    start: f32,
) {
    let style = punch_type(beat.treatment);
    text(
        stage,
        &beat.punch,
        punch_top,
        style,
        look.ink,
        align,
        (start, start + 0.1),
    );
    let rise = (start + 0.04, start + 0.14);
    text(
        stage,
        &beat.credit,
        credit_top,
        CREDIT_TYPE,
        look.quiet,
        align,
        rise,
    );
}

/// One line of spaced caps, shrunk to fit `rect` and placed `anchor` of the
/// way across it (0 left, 0.5 centered, 1 right), rising into place.
#[allow(clippy::too_many_arguments)]
fn tracked(
    stage: &mut Stage<'_>,
    value: &str,
    rect: (i32, i32, i32, i32),
    size: f32,
    face: Face,
    color: Rgba,
    spacing: f32,
    anchor: f32,
    rise: (f32, f32),
) {
    if value.trim().is_empty() {
        return;
    }
    let glyphs = value.chars().map(String::from).collect::<Vec<_>>();
    let measure = |size: f32| {
        let letters = glyphs.iter().map(|glyph| text_width(glyph, size, face));
        letters.sum::<f32>() + spacing * size * (glyphs.len() - 1) as f32
    };
    let size = size.min(size * rect.2 as f32 / measure(size));
    let shown = stage.ease(rise.0, rise.1);
    if shown <= 0.0 {
        return;
    }
    let mut pen = rect.0 as f32 + (rect.2 as f32 - measure(size)).max(0.0) * anchor;
    let baseline = rect.1 as f32 + size * FIRST_BASELINE + (1.0 - shown) * size;
    let clip = (rect.1, rect.1 + rect.3);
    for glyph in &glyphs {
        stage.line(glyph, pen, baseline, size, face, color, clip);
        pen += text_width(glyph, size, face) + spacing * size;
    }
}

fn plain(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    let style = punch_type(WrappedPunch::Plain);
    let chip = if beat.credit.trim().is_empty() { 0 } else { 87 };
    let y = centered(text_height(&beat.punch, style) + chip);
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
        bottom + 30,
        COLUMN,
        30.0,
        look.ink,
        look.bg,
        0.7,
    );
}

/// A movie title card: the hero letterboxed full width, the punchline in
/// spaced serif caps, and the credit as a tracked caption.
fn cinema(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    let style = punch_type(WrappedPunch::Cinema);
    let still = stage
        .still_art
        .as_ref()
        .map(|art| (art.width as i32, art.height as i32));
    let still_height = still.map_or(0, |(_, height)| height + 44);
    let credit_height = if beat.credit.trim().is_empty() { 0 } else { 64 };
    let mut y = centered(still_height + text_height(&beat.punch, style) + credit_height);
    if let (Some((width, height)), Some(art)) = (still, stage.still_art.take()) {
        // The black bars part from the middle of the frame.
        let half = (height as f32 / 2.0 * in_out(stage.progress(0.56, 0.7))).round() as i32;
        let (left, middle) = ((WIDTH - width) / 2, y + height / 2);
        let reveal = (left, middle - half, left + width, middle + half);
        super::artwork::paint(&mut stage.canvas, &art, left, y, reveal, 0.0);
        stage.still_art = Some(art);
        y += still_height;
    }
    let bottom = text(
        stage,
        &beat.punch,
        y,
        style,
        look.ink,
        Align::Center,
        (0.66, 0.78),
    );
    tracked(
        stage,
        &beat.credit.to_uppercase(),
        (MARGIN, bottom + 30, COLUMN, 34),
        26.0,
        Face::Medium,
        look.quiet,
        0.32,
        0.5,
        (0.74, 0.86),
    );
}

/// The series plotted for a chart treatment: finite values, at most 96.
fn chart_points(slide: &WrappedSlideData) -> Vec<f64> {
    if slide.series.iter().any(|value| value.is_finite()) {
        super::cards::curve_points(slide)
    } else {
        Vec::new()
    }
}

/// Chart treatments caption their chart with the credit, like a post-game
/// graph's title, and set the punchline beneath. Draws the caption and the
/// punchline, rising from `start`, and returns the chart's top.
fn chart_card(
    stage: &mut Stage<'_>,
    beat: &WrappedBeat,
    look: Look,
    chart: i32,
    start: f32,
) -> i32 {
    let caption = if beat.credit.trim().is_empty() {
        0
    } else {
        CREDIT_TYPE.height + 20
    };
    let gap = if chart > 0 { 52 } else { 0 };
    let style = punch_type(beat.treatment);
    let top = centered(caption + chart + gap + text_height(&beat.punch, style));
    text(
        stage,
        &beat.credit,
        top,
        CREDIT_TYPE,
        look.quiet,
        Align::Left,
        (0.56, 0.66),
    );
    let punch_top = top + caption + chart + gap;
    let rise = (start, start + 0.1);
    text(
        stage,
        &beat.punch,
        punch_top,
        style,
        look.ink,
        Align::Left,
        rise,
    );
    top + caption
}

/// A post-game gold graph: the lead climbs above zero in green, crashes below
/// it in red, and ends on GG.
fn gold_graph(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    let points = chart_points(stage.slide);
    let chart = if points.len() >= 2 { 400 } else { 0 };
    let chart_top = chart_card(stage, beat, look, chart, 0.76);
    if chart == 0 {
        return;
    }
    let (left, top) = (MARGIN as f32, (chart_top + 84) as f32);
    let (right, bottom) = ((WIDTH - MARGIN) as f32, (chart_top + chart - 44) as f32);
    let lowest = points.iter().copied().fold(0.0_f64, f64::min);
    let highest = points.iter().copied().fold(0.0_f64, f64::max);
    let span = (highest - lowest).max(1.0);
    let last = points.len() - 1;
    let at = |index: f32, value: f64| {
        let y = bottom - ((value - lowest) / span) as f32 * (bottom - top);
        (left + (right - left) * index / last as f32, y)
    };
    let zero = at(0.0, 0.0).1;
    let axis = left + (right - left) * stage.ease(0.56, 0.62);
    let axis_color = Rgba::rgb(42, 51, 70);
    stage
        .canvas
        .rounded_rect((left, zero - 2.0, axis, zero + 2.0), 2.0, axis_color);
    let label = (left as i32, zero as i32 + 8, 60, 30);
    stage.block(
        "0",
        label,
        22.0,
        Face::Bold,
        look.quiet,
        Align::Left,
        (0.56, 0.62),
    );
    let drawn = in_out(stage.progress(0.58, 0.8)) * last as f32;
    // Split the drawn line where it crosses zero: green ahead, red behind.
    let mut runs: Vec<(bool, Vec<(f32, f32)>)> = Vec::new();
    for index in 0..=last.min(drawn.ceil() as usize) {
        let (position, value) = if index as f32 > drawn {
            let fraction = f64::from(drawn - (index - 1) as f32);
            let value = points[index - 1] + (points[index] - points[index - 1]) * fraction;
            (drawn, value)
        } else {
            (index as f32, points[index])
        };
        let ahead = value >= 0.0;
        match runs.last_mut() {
            Some((side, run)) if *side != ahead => {
                let (from, previous) = ((index - 1) as f32, points[index - 1]);
                let crossing = from + (previous / (previous - value)) as f32 * (position - from);
                run.push(at(crossing, 0.0));
                runs.push((ahead, vec![at(crossing, 0.0)]));
            }
            Some(_) => {}
            None => runs.push((ahead, Vec::new())),
        }
        if let Some((_, run)) = runs.last_mut() {
            run.push(at(position, value));
        }
    }
    for (ahead, run) in &runs {
        let color = if *ahead { GREEN } else { RED };
        stage.canvas.stroke(run, 13.0, color);
    }
    let peak = (0..=last)
        .max_by(|a, b| points[*a].total_cmp(&points[*b]))
        .unwrap_or(0);
    if drawn >= peak as f32 && points[peak] > 0.0 {
        let (x, y) = at(peak as f32, points[peak]);
        stage.canvas.disc((x, y), 13.0, WHITE);
        let grouped = cama_domain::formatting::format_grouped(points[peak].round() as i64);
        let label = format!("+{grouped}");
        let width = text_width(&label, 44.0, Face::Display);
        let x = (x - width / 2.0).clamp(left, right - width);
        let rect = (x as i32, (y - 76.0) as i32, width.ceil() as i32 + 4, 54);
        stage.block(
            &label,
            rect,
            44.0,
            Face::Display,
            WHITE,
            Align::Left,
            (0.0, 0.0),
        );
    }
    if drawn >= last as f32 {
        let end = at(last as f32, points[last]);
        stage
            .canvas
            .disc(end, 13.0 * stage.pop(0.8, 0.86).max(0.0), RED);
        let width = text_width("GG", 40.0, Face::Display);
        let x = (end.0 - 24.0 - width) as i32;
        let rect = (x, (end.1 - 8.0) as i32, width.ceil() as i32 + 4, 50);
        stage.block(
            "GG",
            rect,
            40.0,
            Face::Display,
            RED,
            Align::Left,
            (0.8, 0.86),
        );
    }
}

/// Stonks and not stonks: a chart line rising or falling over condensed caps.
fn stonks(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    let points = chart_points(stage.slide);
    let chart = if points.len() >= 2 { 300 } else { 0 };
    let chart_top = chart_card(stage, beat, look, chart, 0.68) as f32;
    if chart == 0 {
        return;
    }
    let (left, right) = (MARGIN as f32, (WIDTH - MARGIN) as f32);
    let chart_bottom = chart_top + chart as f32;
    for step in 1..=3 {
        let y = chart_top + chart as f32 * step as f32 / 4.0;
        let start = 0.56 + step as f32 * 0.02;
        let reach = left + (right - left) * stage.ease(start, start + 0.1);
        let grid = Rgba::rgb(31, 90, 61);
        stage
            .canvas
            .rounded_rect((left, y - 1.5, reach, y + 1.5), 1.5, grid);
    }
    let lowest = points.iter().copied().fold(f64::INFINITY, f64::min);
    let highest = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = (highest - lowest).max(1.0);
    let last = points.len() - 1;
    let at = |index: f32, value: f64| {
        let rise = ((value - lowest) / span) as f32 * (chart as f32 - 24.0);
        (
            left + (right - left) * index / last as f32,
            chart_bottom - 12.0 - rise,
        )
    };
    let drawn = stage.ease(0.58, 0.74) * last as f32;
    let whole = drawn.floor() as usize;
    let mut line = (0..=whole.min(last))
        .map(|index| at(index as f32, points[index]))
        .collect::<Vec<_>>();
    if whole < last {
        let fraction = f64::from(drawn - whole as f32);
        line.push(at(
            drawn,
            points[whole] + (points[whole + 1] - points[whole]) * fraction,
        ));
    }
    if stage.t >= 0.58 {
        stage.canvas.stroke(&line, 16.0, look.signal);
    }
}

/// Calculated: the win-chance bar fills to a small percent, then the brag.
fn calculated(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    const NUMBER: i32 = 234;
    const BAR: f32 = 52.0;
    let digits = stage.slide.stat_value.trim().trim_end_matches('%').trim();
    let percent = digits
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 100.0));
    let graphic = if percent.is_some() {
        42 + NUMBER - 22 + BAR as i32
    } else {
        0
    };
    let (top, punch_top, credit_top) = stack(beat, graphic);
    if let Some(percent) = percent {
        let label_rect = (MARGIN, top, COLUMN, 30);
        let quiet = look.quiet;
        tracked(
            stage,
            "WIN CHANCE",
            label_rect,
            26.0,
            Face::Bold,
            quiet,
            0.2,
            0.0,
            (0.56, 0.64),
        );
        let fill = stage.ease(0.6, 0.76);
        let label = format!("{digits}%");
        stage.stat_in(
            Face::Display,
            &label,
            &count_up(&label, fill),
            (MARGIN, top + 42, COLUMN, NUMBER),
            200.0,
            look.signal,
            Align::Left,
            (0.58, 0.66),
        );
        let (left, bar_top) = (MARGIN as f32, (top + 42 + NUMBER - 22) as f32);
        let track = left + COLUMN as f32 * stage.ease(0.56, 0.62);
        let track_color = WHITE.scaled_alpha(0.14);
        stage.canvas.rounded_rect(
            (left, bar_top, track, bar_top + BAR),
            BAR / 2.0,
            track_color,
        );
        if fill > 0.0 {
            let filled = (left + COLUMN as f32 * percent / 100.0 * fill).max(left + BAR);
            stage.canvas.rounded_rect(
                (left, bar_top, filled, bar_top + BAR),
                BAR / 2.0,
                look.signal,
            );
        }
    }
    punchline(
        stage,
        beat,
        look,
        (punch_top, credit_top),
        Align::Left,
        0.76,
    );
}

/// A defeat banner between two rules, over the K/D/A that should have won.
fn task_failed(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    const BANNER: f32 = 180.0;
    let banner = block_height(1, BANNER, Face::Display).ceil() as i32;
    let has_value = !beat.value.trim().is_empty();
    let value_height = if has_value { 150 } else { 0 };
    let (top, punch_top, credit_top) = stack(beat, 24 + banner + value_height);
    let reach = stage.ease(0.56, 0.66) * COLUMN as f32 / 2.0;
    let middle = (WIDTH / 2) as f32;
    for y in [top as f32, (top + 18 + banner) as f32] {
        let rule = (middle - reach, y, middle + reach, y + 6.0);
        stage.canvas.rounded_rect(rule, 3.0, look.signal);
    }
    stage.stat_in(
        Face::Display,
        "DEFEAT",
        "DEFEAT",
        (MARGIN, top + 12, COLUMN, banner),
        BANNER,
        look.signal,
        Align::Center,
        (0.6, 0.7),
    );
    if has_value {
        stage.stat_in(
            Face::Display,
            &beat.value,
            &spun(&beat.value, stage.ease(0.66, 0.8)),
            (MARGIN, top + 30 + banner, COLUMN, value_height - 12),
            116.0,
            look.ink,
            Align::Center,
            (0.66, 0.74),
        );
    }
    punchline(
        stage,
        beat,
        look,
        (punch_top, credit_top),
        Align::Center,
        0.76,
    );
}

/// A run of losses as a row of L tiles, popping in one after another.
fn loss_tiles(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    const MOST: usize = 18;
    const GAP: i32 = 16;
    let losses = stage.slide.series.len();
    let shown = losses.min(MOST);
    let columns = match shown {
        0..=5 => shown.max(1),
        6..=10 => 5,
        _ => 6,
    } as i32;
    let rows = shown.div_ceil(columns as usize) as i32;
    let tile = ((COLUMN - GAP * (columns - 1)) / columns).min(120);
    let graphic = if shown > 0 {
        rows * tile + (rows - 1) * GAP
    } else {
        0
    };
    let (top, punch_top, credit_top) = stack(beat, graphic);
    for index in 0..shown {
        let start = 0.56 + index as f32 * 0.2 / shown as f32;
        let scale = stage.pop(start, start + 0.1);
        if stage.t < start || scale <= 0.01 {
            continue;
        }
        let left = MARGIN + index as i32 % columns * (tile + GAP);
        let tile_top = top + index as i32 / columns * (tile + GAP);
        let (cx, cy) = ((left + tile / 2) as f32, (tile_top + tile / 2) as f32);
        let half = tile as f32 / 2.0 * scale;
        let square = (cx - half, cy - half, cx + half, cy + half);
        stage
            .canvas
            .rounded_rect(square, tile as f32 * 0.2 * scale, look.signal);
        if scale >= 0.9 {
            // A longer run than fits ends on a count of the rest.
            let mark = if losses > MOST && index == MOST - 1 {
                format!("+{}", losses - (MOST - 1))
            } else {
                "L".to_owned()
            };
            let size = tile as f32 * 0.72;
            let height = block_height(1, size, Face::Display).ceil() as i32;
            let rect = (left + 8, cy as i32 - height / 2 + 4, tile - 16, height);
            let (bg, center) = (look.bg, Align::Center);
            stage.stat_in(
                Face::Display,
                &mark,
                &mark,
                rect,
                size,
                bg,
                center,
                (0.0, 0.0),
            );
        }
    }
    punchline(
        stage,
        beat,
        look,
        (punch_top, credit_top),
        Align::Left,
        0.76,
    );
}

/// One keycap, pressed once.
fn press_f(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    const KEY: f32 = 240.0;
    let (top, punch_top, credit_top) = stack(beat, KEY as i32);
    let scale = stage.pop(0.56, 0.66).max(0.0);
    if scale > 0.01 {
        let press = 14.0 * (stage.ease(0.7, 0.73) - stage.ease(0.75, 0.82));
        let (cx, top) = ((WIDTH / 2) as f32, top as f32);
        let middle = top + KEY / 2.0;
        let span = |y: f32| middle + (y - middle) * scale;
        // The key's base, then its lighter top, which travels when pressed.
        let half = KEY / 2.0 * scale;
        let base = (cx - half, span(top + press), cx + half, span(top + KEY));
        let side = Rgba::rgb(148, 154, 166);
        stage.canvas.rounded_rect(base, 34.0 * scale, side);
        let face_half = (KEY / 2.0 - 16.0) * scale;
        let (face_top, face_bottom) = (span(top + 10.0 + press), span(top + KEY - 34.0 + press));
        let face = (cx - face_half, face_top, cx + face_half, face_bottom);
        stage.canvas.rounded_rect(face, 26.0 * scale, look.signal);
        if scale >= 0.9 {
            let height = block_height(1, 120.0, Face::Bold).ceil() as i32;
            let rect = (
                MARGIN,
                ((face_top + face_bottom) / 2.0) as i32 - height / 2 + 6,
                COLUMN,
                height,
            );
            let ink = Rgba::rgb(40, 44, 52);
            stage.block("F", rect, 120.0, Face::Bold, ink, Align::Center, (0.0, 0.0));
        }
    }
    punchline(
        stage,
        beat,
        look,
        (punch_top, credit_top),
        Align::Center,
        0.76,
    );
}

/// The server's rank as a bar from top to bottom, with the player's marker
/// sliding all the way to the bottom end.
fn skill_issue(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    const SEGMENTS: usize = 10;
    const BAR: f32 = 56.0;
    let (top, punch_top, credit_top) = stack(beat, 126 + BAR as i32);
    let bar_top = (top + 126) as f32;
    let (left, right) = (MARGIN as f32, (WIDTH - MARGIN) as f32);
    for (label, anchor) in [("TOP", 0.0), ("BOTTOM", 1.0)] {
        let (rect, quiet) = ((MARGIN, top, COLUMN, 30), look.quiet);
        tracked(
            stage,
            label,
            rect,
            24.0,
            Face::Bold,
            quiet,
            0.2,
            anchor,
            (0.56, 0.62),
        );
    }
    let gap = 8.0;
    let segment = (right - left - gap * (SEGMENTS - 1) as f32) / SEGMENTS as f32;
    for index in 0..SEGMENTS {
        let start = 0.56 + index as f32 * 0.012;
        let grow = stage.ease(start, start + 0.06);
        if grow <= 0.0 {
            continue;
        }
        // Lime at the top of the ladder warms to red at the bottom.
        let mix = index as f32 / (SEGMENTS - 1) as f32;
        let channel = |c: usize| {
            let (from, to) = (f32::from(LIME.0[c]), f32::from(RED.0[c]));
            (from + (to - from) * mix) as u8
        };
        let x = left + index as f32 * (segment + gap);
        let (middle, half) = (bar_top + BAR / 2.0, BAR / 2.0 * grow);
        let color = Rgba::rgb(channel(0), channel(1), channel(2));
        stage
            .canvas
            .rounded_rect((x, middle - half, x + segment, middle + half), 10.0, color);
    }
    if stage.t >= 0.62 {
        let (start, end) = (left + segment / 2.0, right - segment / 2.0);
        let x = start + (end - start) * stage.ease(0.62, 0.8);
        stage.canvas.disc((x, bar_top + BAR / 2.0), 20.0, WHITE);
        stage.canvas.disc((x, bar_top + BAR / 2.0), 11.0, INK);
        let pointer = [
            (x - 12.0, bar_top - 18.0),
            (x + 12.0, bar_top - 18.0),
            (x, bar_top - 4.0),
        ];
        stage.canvas.polygon(&pointer, WHITE);
        let width = stage.pill_width("YOU", 200, 28.0) as f32;
        let pill_left = (x - width / 2.0).min(right - width) as i32;
        stage.pill(
            "YOU",
            pill_left,
            (bar_top - 70.0) as i32,
            200,
            28.0,
            WHITE,
            INK,
            0.0,
        );
    }
    punchline(
        stage,
        beat,
        look,
        (punch_top, credit_top),
        Align::Left,
        0.76,
    );
}

/// A bedside clock: the late-night time in red, its colon blinking.
fn clock(stage: &mut Stage<'_>, beat: &WrappedBeat, look: Look) {
    const SUFFIX: f32 = 0.32;
    let value = beat.value.trim();
    let (time, suffix) = value.split_once(' ').unwrap_or((value, ""));
    let (hours, minutes) = time.split_once(':').unwrap_or((time, ""));
    let pieces = [hours, if minutes.is_empty() { "" } else { ":" }, minutes];
    let width = |size: f32| {
        let figures = pieces
            .iter()
            .map(|piece| text_width(piece, size, Face::Display));
        let suffix = if suffix.is_empty() {
            0.0
        } else {
            18.0 + text_width(suffix, size * SUFFIX, Face::Display)
        };
        figures.sum::<f32>() + suffix
    };
    let size = 250.0_f32.min(250.0 * COLUMN as f32 / width(250.0).max(1.0));
    let height = if value.is_empty() {
        0
    } else {
        block_height(1, size, Face::Display).ceil() as i32
    };
    let (top, punch_top, credit_top) = stack(beat, height);
    let shown = stage.ease(0.56, 0.66);
    if height > 0 && shown > 0.0 {
        let baseline = top as f32 + size * FIRST_BASELINE + (1.0 - shown) * size;
        let clip = (top, top + height);
        let (color, display) = (look.signal, Face::Display);
        let mut pen = (WIDTH as f32 - width(size)) / 2.0;
        // The colon blinks while the punchline lands, then stays lit.
        let lit = stage.t < 0.62 || stage.t >= 0.92 || ((stage.t - 0.62) / 0.06) as i32 % 2 == 1;
        for (index, piece) in pieces.into_iter().enumerate() {
            if index != 1 || lit {
                stage.line(piece, pen, baseline, size, display, color, clip);
            }
            pen += text_width(piece, size, display);
        }
        if !suffix.is_empty() {
            // AM or PM, its caps level with the tops of the figures.
            let small = size * SUFFIX;
            let lift = ink_height(time, size, display) - ink_height(suffix, small, display);
            stage.line(
                suffix,
                pen + 18.0,
                baseline - lift,
                small,
                display,
                color,
                clip,
            );
        }
    }
    punchline(
        stage,
        beat,
        look,
        (punch_top, credit_top),
        Align::Center,
        0.72,
    );
}
