//! The patch chapters that read as release notes: hero updates, item updates
//! and bug fixes. Each row is real game art beside a dry official line, and
//! its verdict lands as a stamp.

use super::stage::{
    Align, Face, HEIGHT, Stage, WIDTH, block_height, count_up, ink_height, text_width,
    tracked_width,
};
use super::*;

const MARGIN: i32 = 40;
const COLUMN: i32 = WIDTH - MARGIN * 2;
const TOP: i32 = 88;
/// Lowest y a row may reach, leaving clear air above the username footer.
const BOTTOM: i32 = HEIGHT - 72;
const TITLE_HEIGHT: i32 = 78;
const SUBTITLE_HEIGHT: i32 = 36;
const FOOTNOTE_HEIGHT: i32 = 40;
const GAP: i32 = 12;

/// The ink panel behind a hero block, and the lighter tile set into it.
const PANEL: Rgba = Rgba::rgb(17, 16, 28);
const INSET: Rgba = Rgba::rgb(46, 44, 70);
const PAPER: Rgba = Rgba::rgb(255, 245, 225);
const BUFF: Rgba = Rgba::rgb(86, 224, 138);
const NERF: Rgba = Rgba::rgb(255, 98, 98);
const FIXED: Rgba = Rgba::rgb(22, 150, 92);
const WARNING: Rgba = Rgba::rgb(255, 206, 48);

/// Letter spacing of a stamp, in multiples of its type size.
const STAMP_TRACKING: f32 = 0.08;

/// Steam's hero portraits are 16:9 and its item icons 11:8.
const ITEM_ICON: (i32, i32) = (88, 64);
/// A fixed issue's small hero portrait.
pub(super) const FIX_PORTRAIT: (usize, usize) = (96, 54);

fn verdict(value: f64, neutral: Rgba) -> Rgba {
    if value > 0.0 {
        BUFF
    } else if value < 0.0 {
        NERF
    } else {
        neutral
    }
}

/// The top of the rows under the title and, when a card has one, its subtitle.
fn content_top(slide: &WrappedSlideData, subtitle: bool) -> i32 {
    let subtitle = subtitle && !slide.headline.trim().is_empty();
    TOP + TITLE_HEIGHT + if subtitle { SUBTITLE_HEIGHT } else { 0 } + 12
}

fn heading(stage: &mut Stage<'_>, subtitle: bool) {
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
    if subtitle {
        stage.block(
            &slide.headline,
            (MARGIN, TOP + TITLE_HEIGHT, COLUMN, SUBTITLE_HEIGHT),
            26.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (0.03, 0.13),
        );
    }
}

fn footnote(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    if let Some(note) = slide.lines.first() {
        stage.block(
            note,
            (
                MARGIN,
                BOTTOM - FOOTNOTE_HEIGHT + 8,
                COLUMN,
                FOOTNOTE_HEIGHT - 8,
            ),
            23.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (0.86, 0.98),
        );
    }
}

/// When row `index` of `count` starts on the timeline and how long its
/// reveal runs. The heading has the first tenth; rows overlap slightly.
fn row_window(index: usize, count: usize) -> (f32, f32) {
    let step = (0.8 / count.max(1) as f32).min(0.3);
    (0.1 + index as f32 * step, step * 1.25)
}

/// A span of a row's reveal as a span of the card's timeline.
fn within(window: (f32, f32), from: f32, to: f32) -> (f32, f32) {
    (window.0 + window.1 * from, window.0 + window.1 * to)
}

/// A stamp's finished width and height.
fn stamp_extent(text: &str, size: f32) -> (f32, f32) {
    (
        tracked_width(text, size, Face::Bold, size * STAMP_TRACKING) + size * 1.4,
        size * 1.85,
    )
}

/// A verdict that slams onto the card: it lands oversized, then settles with
/// its top-left corner at `(left, top)`. An ink edge keeps it off any art.
fn stamp(
    stage: &mut Stage<'_>,
    text: &str,
    (left, top): (f32, f32),
    size: f32,
    (fill, ink): (Rgba, Rgba),
    span: (f32, f32),
) {
    let landed = stage.progress(span.0, span.1);
    if text.is_empty() || landed <= 0.0 {
        return;
    }
    let scale = 1.0 + (1.0 - landed).powi(2) * 0.9;
    let (width, height) = stamp_extent(text, size);
    let (cx, cy) = (left + width / 2.0, top + height / 2.0);
    let (half_width, half_height) = (width / 2.0 * scale, height / 2.0 * scale);
    let edge = 3.0 * scale;
    stage.canvas.rounded_rect(
        (
            cx - half_width - edge,
            cy - half_height - edge,
            cx + half_width + edge,
            cy + half_height + edge,
        ),
        9.0 * scale + edge,
        ink,
    );
    stage.canvas.rounded_rect(
        (
            cx - half_width,
            cy - half_height,
            cx + half_width,
            cy + half_height,
        ),
        9.0 * scale,
        fill,
    );
    let type_size = size * scale;
    let tracking = type_size * STAMP_TRACKING;
    stage.tracked(
        text,
        cx - tracked_width(text, type_size, Face::Bold, tracking) / 2.0,
        cy + type_size * 0.36,
        type_size,
        Face::Bold,
        ink,
        tracking,
    );
}

/// A row's picture wiped in from the left inside `slot` `(left, top, width,
/// height)`. A row without one gets its initial on a tile, so the slot never
/// stands empty.
fn picture(
    stage: &mut Stage<'_>,
    index: usize,
    name: &str,
    slot: (i32, i32, i32, i32),
    radius: f32,
    (tile, letter): (Rgba, Rgba),
    span: (f32, f32),
) {
    let wipe = stage.ease(span.0, span.1);
    if wipe <= 0.0 {
        return;
    }
    let (left, top, width, height) = slot;
    if let Some(art) = stage.row_art.get(index).and_then(Option::as_ref) {
        let x = left + (width - art.width as i32) / 2;
        let y = top + (height - art.height as i32) / 2;
        let shown = x + (art.width as f32 * wipe).round() as i32;
        let reveal = (x, y, shown, y + art.height as i32);
        super::artwork::paint(&mut stage.canvas, art, x, y, reveal, radius);
        return;
    }
    stage.canvas.rounded_rect(
        (
            left as f32,
            top as f32,
            left as f32 + width as f32 * wipe,
            (top + height) as f32,
        ),
        radius,
        tile,
    );
    let initial = name
        .chars()
        .find(|character| character.is_alphanumeric())
        .map(|character| character.to_uppercase().collect::<String>())
        .unwrap_or_default();
    if wipe > 0.6 {
        let size = height as f32 * 0.7;
        let ink = ink_height(&initial, size, Face::Display);
        stage.line(
            &initial,
            left as f32 + (width as f32 - text_width(&initial, size, Face::Display)) / 2.0,
            top as f32 + (height as f32 + ink) / 2.0,
            size,
            Face::Display,
            letter,
            (top, top + height),
        );
    }
}

/// Change lines with a small diamond before each one. A line too long for
/// its row wraps under itself, and the diamond stays with its first row.
fn bulleted(
    stage: &mut Stage<'_>,
    text: &str,
    rect: (i32, i32, i32, i32),
    size: f32,
    (ink, bullet): (Rgba, Rgba),
    rise: (f32, f32),
) {
    const INDENT: i32 = 26;
    let (left, top, width, height) = rect;
    let column = width - INDENT;
    let (fitted, _) = stage.fit(text, Face::Medium, size, column, height);
    stage.block(
        text,
        (left + INDENT, top, column, height),
        size,
        Face::Medium,
        ink,
        Align::Left,
        rise,
    );
    let advance = fitted * Face::Medium.leading();
    let mut row = 0;
    for line in text.split('\n') {
        let (_, rows) = stage.fit(line, Face::Medium, fitted, column, HEIGHT);
        if rows.is_empty() {
            continue;
        }
        let stagger = row as f32 * 0.05;
        if stage.ease(rise.0 + stagger, rise.1 + stagger) > 0.5 {
            let center = (
                left as f32 + 9.0,
                top as f32 + fitted * 0.57 + row as f32 * advance,
            );
            let half = fitted * 0.2;
            stage.canvas.polygon(
                &[
                    (center.0, center.1 - half),
                    (center.0 + half, center.1),
                    (center.0, center.1 + half),
                    (center.0 - half, center.1),
                ],
                bullet,
            );
        }
        row += rows.len();
    }
}

/// Where one hero block puts its pieces. Offsets are from the block's top;
/// boxes are `(left, top, width, height)`.
#[derive(Clone, Copy)]
struct HeroBlock {
    height: i32,
    portrait: (i32, i32, i32, i32),
    name: (i32, i32, i32, i32),
    name_size: f32,
    /// The ability icons' left edge, top, and size.
    strip: (i32, i32, i32),
    changes: (i32, i32, i32, i32),
    change_size: f32,
    stamp_size: f32,
    /// Centered for a lone hero, whose portrait stands above its name.
    align: Align,
}

const INNER: i32 = MARGIN + 16;
const INNER_WIDTH: i32 = COLUMN - 32;
const HERO_PAD: i32 = 14;

fn change_lines_height(size: f32) -> i32 {
    block_height(2, size, Face::Medium).ceil() as i32 + 2
}

/// A hero block with its portrait on the left and the name and ability
/// icons beside it, the change lines running full width underneath.
fn side_block(portrait_height: i32, icon: i32, name_size: f32, change_size: f32) -> HeroBlock {
    let portrait_width = portrait_height * 16 / 9;
    let text_left = INNER + portrait_width + 18;
    let changes_top = HERO_PAD + portrait_height + 10;
    let changes_height = change_lines_height(change_size);
    HeroBlock {
        height: changes_top + changes_height + HERO_PAD,
        portrait: (INNER, HERO_PAD, portrait_width, portrait_height),
        name: (
            text_left,
            HERO_PAD - 6,
            INNER + INNER_WIDTH - text_left,
            portrait_height - icon,
        ),
        name_size,
        strip: (text_left, HERO_PAD + portrait_height - icon, icon),
        changes: (INNER, changes_top, INNER_WIDTH, changes_height),
        change_size,
        stamp_size: name_size * 0.5,
        align: Align::Left,
    }
}

/// The top of the first hero block and the shape every block takes: three
/// share the card evenly, two grow, and one stands its portrait above its
/// name.
fn hero_blocks(slide: &WrappedSlideData) -> (i32, HeroBlock) {
    let top = content_top(slide, true);
    let room = BOTTOM - top;
    let count = slide.rows.len().clamp(1, 3) as i32;
    let block = match count {
        1 => {
            let (portrait, name, icon) = ((384, 216), 66, 64);
            let name_top = 22 + portrait.1 + 12;
            let strip_top = name_top + name + 8;
            let changes_top = strip_top + icon + 16;
            let changes_height = change_lines_height(30.0);
            HeroBlock {
                height: changes_top + changes_height + 22,
                portrait: ((WIDTH - portrait.0) / 2, 22, portrait.0, portrait.1),
                name: (INNER, name_top, INNER_WIDTH, name),
                name_size: 52.0,
                strip: (INNER, strip_top, icon),
                changes: (INNER + 44, changes_top, INNER_WIDTH - 88, changes_height),
                change_size: 30.0,
                stamp_size: 26.0,
                align: Align::Center,
            }
        }
        2 => side_block(180, 58, 44.0, 30.0),
        _ => {
            let height = (room - GAP * 2) / 3;
            let fixed = HERO_PAD * 2 + 10 + change_lines_height(26.0);
            side_block((height - fixed).clamp(72, 126), 52, 38.0, 26.0)
        }
    };
    let used = block.height * count + GAP * (count - 1);
    (top + (room - used).max(0) / 2, block)
}

/// The size hero portraits are drawn at on this card.
pub(super) fn hero_portrait(slide: &WrappedSlideData) -> (usize, usize) {
    let (_, _, width, height) = hero_blocks(slide).1.portrait;
    (width as usize, height as usize)
}

/// The side of this card's square ability icons.
pub(super) fn hero_icon(slide: &WrappedSlideData) -> usize {
    hero_blocks(slide).1.strip.2 as usize
}

/// Hero updates: each hero's portrait, stamped verdict, ability icons and
/// change lines on an ink panel, like a block of official patch notes.
pub(super) fn heroes(stage: &mut Stage<'_>) {
    let slide = stage.slide;
    heading(stage, true);
    let (first, block) = hero_blocks(slide);
    let count = slide.rows.len().min(3);
    for (index, row) in slide.rows.iter().take(3).enumerate() {
        let top = first + index as i32 * (block.height + GAP);
        hero(stage, index, row, top, block, row_window(index, count));
    }
}

fn hero(
    stage: &mut Stage<'_>,
    index: usize,
    row: &WrappedRow,
    top: i32,
    block: HeroBlock,
    window: (f32, f32),
) {
    let span = |from: f32, to: f32| within(window, from, to);
    let color = verdict(row.value, stage.palette.accent);
    let (open_from, open_to) = span(0.0, 0.26);
    let open = stage.ease(open_from, open_to);
    if open <= 0.0 {
        return;
    }
    let icons = stage.row_icons.get(index).map_or(0, Vec::len) as i32;
    let (strip_left, strip_top, icon) = block.strip;
    // A lone hero without ability icons closes the gap they would have left:
    // its lines move up and its panel is that much shorter.
    let lift = if icons == 0 && block.align == Align::Center {
        icon + 8
    } else {
        0
    };
    let top = top + lift / 2;
    stage.canvas.rounded_rect(
        (
            MARGIN as f32,
            top as f32,
            MARGIN as f32 + COLUMN as f32 * open,
            (top + block.height - lift) as f32,
        ),
        24.0,
        PANEL,
    );
    let (left, offset, width, height) = block.portrait;
    let portrait = (left, top + offset, width, height);
    picture(
        stage,
        index,
        &row.primary,
        portrait,
        14.0,
        (INSET, color),
        span(0.1, 0.42),
    );
    let (left, offset, width, height) = block.name;
    if icons == 0 && block.align == Align::Left {
        // With no ability icons under it, the name centers on the portrait.
        stage.centered_block(
            &row.primary,
            (left, portrait.1, width, portrait.3),
            block.name_size,
            Face::Black,
            PAPER,
            block.align,
            span(0.22, 0.48),
        );
    } else {
        stage.block(
            &row.primary,
            (left, top + offset, width, height),
            block.name_size,
            Face::Black,
            PAPER,
            block.align,
            span(0.22, 0.48),
        );
    }
    let strip_width = icons * icon + (icons - 1).max(0) * 8;
    let strip_left = match block.align {
        Align::Left => strip_left,
        Align::Center => (WIDTH - strip_width) / 2,
    };
    for position in 0..icons {
        let delay = 0.44 + position as f32 * 0.07;
        let (from, to) = span(delay, delay + 0.2);
        let grow = stage.pop(from, to).clamp(0.0, 1.1);
        if grow <= 0.0 {
            continue;
        }
        let center = (
            (strip_left + position * (icon + 8)) as f32 + icon as f32 / 2.0,
            (top + strip_top) as f32 + icon as f32 / 2.0,
        );
        let half = icon as f32 / 2.0 * grow;
        stage.canvas.rounded_rect(
            (
                center.0 - half,
                center.1 - half,
                center.0 + half,
                center.1 + half,
            ),
            9.0,
            INSET,
        );
        let art = &stage.row_icons[index][position as usize];
        let (x, y) = (
            (center.0 - art.width as f32 / 2.0).round() as i32,
            (center.1 - art.height as f32 / 2.0).round() as i32,
        );
        let inset = ((1.0 - grow.min(1.0)) * icon as f32 / 2.0).round() as i32;
        let reveal = (
            x + inset,
            y + inset,
            x + art.width as i32 - inset,
            y + art.height as i32 - inset,
        );
        super::artwork::paint(&mut stage.canvas, art, x, y, reveal, 9.0);
    }
    let (left, offset, width, height) = block.changes;
    bulleted(
        stage,
        &row.secondary,
        (left, top + offset - lift, width, height),
        block.change_size,
        (PAPER, color),
        span(0.55, 0.8),
    );
    // The verdict hangs off the portrait's corner, clear of the hero's face.
    let tag = row.tag.trim().to_uppercase();
    let (_, stamp_height) = stamp_extent(&tag, block.stamp_size);
    stamp(
        stage,
        &tag,
        (
            (portrait.0 - 12) as f32,
            portrait.1 as f32 - stamp_height * 0.5,
        ),
        block.stamp_size,
        (color, PANEL),
        span(0.5, 0.62),
    );
}

/// Where one item row puts its pieces. Offsets are from the row's top.
#[derive(Clone, Copy)]
struct ItemRow {
    /// The distance from one row's top to the next.
    pitch: i32,
    /// The icon's size inside its frame.
    icon: (i32, i32),
    /// The frame's left edge and its offset from the row's top.
    frame: (i32, i32),
    name: (i32, i32, i32, i32),
    name_size: f32,
    /// The line under the name: a leading figure is set large in it.
    line: (i32, i32, i32, i32),
    figure_size: f32,
    line_size: f32,
    stamp_size: f32,
    /// Centered for a lone item, whose icon stands above its name.
    align: Align,
}

const ITEM_BORDER: i32 = 6;
const ITEM_SHADOW: i32 = 8;

fn item_rows(slide: &WrappedSlideData) -> (i32, ItemRow) {
    let top = content_top(slide, false) + 8;
    let room = BOTTOM - top;
    let count = slide.rows.len().clamp(1, 4) as i32;
    let scaled = |scale: i32| (ITEM_ICON.0 * scale / 4, ITEM_ICON.1 * scale / 4);
    let row = if count == 1 {
        let icon = scaled(12);
        let frame_height = icon.1 + ITEM_BORDER * 2;
        let name_top = frame_height + ITEM_SHADOW + 20;
        ItemRow {
            pitch: name_top + 72 + 230,
            icon,
            frame: ((WIDTH - icon.0) / 2 - ITEM_BORDER, 0),
            name: (MARGIN, name_top, COLUMN, 72),
            name_size: 56.0,
            line: (MARGIN, name_top + 72, COLUMN, 230),
            figure_size: 150.0,
            line_size: 32.0,
            stamp_size: 24.0,
            align: Align::Center,
        }
    } else {
        let (icon, name_size, name_height, figure_size, line_size) = if count == 4 {
            (scaled(8), 36.0, 46, 78.0, 27.0)
        } else {
            (scaled(10), 42.0, 54, 96.0, 29.0)
        };
        let frame_height = icon.1 + ITEM_BORDER * 2;
        let pitch = (room / count).min(frame_height + ITEM_SHADOW + 60);
        let text_left = MARGIN + icon.0 + ITEM_BORDER * 2 + ITEM_SHADOW + 22;
        let text_width = WIDTH - MARGIN - text_left;
        ItemRow {
            pitch,
            icon,
            frame: (MARGIN, 0),
            name: (text_left, -2, text_width, name_height),
            name_size,
            line: (
                text_left,
                name_height,
                text_width,
                frame_height - name_height,
            ),
            figure_size,
            line_size,
            stamp_size: 18.0,
            align: Align::Left,
        }
    };
    let content = row.icon.1 + ITEM_BORDER * 2 + ITEM_SHADOW;
    let used = row.pitch * (count - 1) + content.max(row.line.1 + row.line.3);
    (top + (room - used).max(0) / 2, row)
}

/// The size item icons are drawn at on this card.
pub(super) fn item_icon(slide: &WrappedSlideData) -> (usize, usize) {
    let (width, height) = item_rows(slide).1.icon;
    (width as usize, height as usize)
}

/// A line that opens with a figure, such as `41 of 48 games`, split into
/// the figure and the words after it.
fn leading_figure(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let (first, rest) = line.split_once(' ').unwrap_or((line, ""));
    let digits = first.trim_start_matches(['+', '-', '\u{2212}', '#', '$']);
    (first.chars().count() <= 7 && digits.starts_with(|character: char| character.is_ascii_digit()))
        .then_some((first, rest.trim()))
}

/// Item updates: the icons are the art, framed large, each with its name and
/// one line whose leading figure counts up.
pub(super) fn items(stage: &mut Stage<'_>) {
    let slide = stage.slide;
    heading(stage, false);
    let (first, geometry) = item_rows(slide);
    let count = slide.rows.len().min(4);
    for (index, row) in slide.rows.iter().take(4).enumerate() {
        let top = first + index as i32 * geometry.pitch;
        item(stage, index, row, top, geometry, row_window(index, count));
    }
}

fn item(
    stage: &mut Stage<'_>,
    index: usize,
    row: &WrappedRow,
    top: i32,
    geometry: ItemRow,
    window: (f32, f32),
) {
    let p = stage.palette;
    let span = |from: f32, to: f32| within(window, from, to);
    let (pop_from, pop_to) = span(0.0, 0.3);
    let grow = stage.pop(pop_from, pop_to).max(0.0);
    if stage.t < pop_from || grow <= 0.0 {
        return;
    }
    let (icon_width, icon_height) = geometry.icon;
    let frame = (
        geometry.frame.0,
        top + geometry.frame.1,
        icon_width + ITEM_BORDER * 2,
        icon_height + ITEM_BORDER * 2,
    );
    let center = (
        frame.0 as f32 + frame.2 as f32 / 2.0,
        frame.1 as f32 + frame.3 as f32 / 2.0,
    );
    let (half_width, half_height) = (frame.2 as f32 / 2.0 * grow, frame.3 as f32 / 2.0 * grow);
    for (offset, color) in [(ITEM_SHADOW as f32, p.accent), (0.0, PANEL)] {
        stage.canvas.rounded_rect(
            (
                center.0 - half_width + offset,
                center.1 - half_height + offset,
                center.0 + half_width + offset,
                center.1 + half_height + offset,
            ),
            20.0,
            color,
        );
    }
    let slot = (
        frame.0 + ITEM_BORDER,
        frame.1 + ITEM_BORDER,
        icon_width,
        icon_height,
    );
    picture(
        stage,
        index,
        &row.primary,
        slot,
        14.0,
        (INSET, p.accent),
        span(0.22, 0.5),
    );
    let (left, offset, width, height) = geometry.name;
    stage.block(
        &row.primary,
        (left, top + offset, width, height),
        geometry.name_size,
        Face::Black,
        p.fg,
        geometry.align,
        span(0.25, 0.5),
    );
    let (left, offset, width, height) = geometry.line;
    let line = (left, top + offset, width, height);
    match leading_figure(&row.secondary) {
        Some((figure, words)) => item_figure(stage, figure, words, line, geometry, window),
        None => {
            stage.block(
                &row.secondary,
                (line.0, line.1 + 4, line.2, line.3 - 4),
                geometry.line_size + 2.0,
                Face::Medium,
                p.fg,
                geometry.align,
                span(0.4, 0.66),
            );
        }
    }
    let tag = row.tag.trim().to_uppercase();
    stamp(
        stage,
        &tag,
        (
            (frame.0 - 8) as f32,
            frame.1 as f32 - geometry.stamp_size * 0.8,
        ),
        geometry.stamp_size,
        (verdict(row.value, p.accent), PANEL),
        span(0.62, 0.74),
    );
}

/// The figure a line opens with, large and counting up, and the words after
/// it: beside the figure in a row, under it for a lone item.
fn item_figure(
    stage: &mut Stage<'_>,
    figure: &str,
    words: &str,
    line: (i32, i32, i32, i32),
    geometry: ItemRow,
    window: (f32, f32),
) {
    let p = stage.palette;
    let span = |from: f32, to: f32| within(window, from, to);
    let (left, top, width, height) = line;
    let (count_from, count_to) = span(0.35, 0.9);
    let spun = count_up(figure, stage.ease(count_from, count_to));
    let rise = span(0.35, 0.55);
    if geometry.align == Align::Center {
        let bottom = stage.stat_in(
            Face::Display,
            figure,
            &spun,
            (left, top, width, height * 3 / 4),
            geometry.figure_size,
            p.accent,
            Align::Center,
            rise,
        );
        stage.block(
            words,
            (left, bottom - 6, width, top + height - bottom + 6),
            geometry.line_size,
            Face::Medium,
            p.fg,
            Align::Center,
            span(0.5, 0.76),
        );
        return;
    }
    // The figure keeps to just over half the column, so its words have room.
    let figure_room = width * 11 / 20;
    let size = geometry.figure_size.min(height as f32 / 1.17);
    let natural = text_width(figure, size, Face::Display);
    let figure_width = natural.min(figure_room as f32).ceil() as i32;
    stage.stat_in(
        Face::Display,
        figure,
        &spun,
        (left, top, figure_room, height),
        geometry.figure_size,
        p.accent,
        Align::Left,
        rise,
    );
    let words_left = left + figure_width + 14;
    stage.centered_block(
        words,
        (
            words_left,
            top + 2,
            left + width - words_left,
            (size * 0.96) as i32,
        ),
        geometry.line_size,
        Face::Medium,
        p.fg,
        Align::Left,
        span(0.5, 0.76),
    );
}

/// Where a fixed issue puts its pieces: the first row's top, the distance
/// between rows, and each row's panel height.
fn fix_rows(slide: &WrappedSlideData) -> (i32, i32, i32) {
    let top = content_top(slide, true);
    let bottom = if slide.lines.is_empty() {
        BOTTOM
    } else {
        BOTTOM - FOOTNOTE_HEIGHT
    };
    let count = slide.rows.len().clamp(1, 4) as i32;
    let pitch = ((bottom - top + GAP) / count).min(172);
    let used = pitch * count - GAP;
    (top + (bottom - top - used).max(0) / 2, pitch, pitch - GAP)
}

const FIX_PAD: i32 = 18;
const FIX_MARK: i32 = 44;

/// The box a fix's line is set in: narrower beside a hero portrait.
fn fix_text_box(panel_height: i32, portrait: bool) -> (i32, i32, i32, i32) {
    let left = MARGIN + FIX_PAD + FIX_MARK + 14;
    let right = WIDTH
        - MARGIN
        - FIX_PAD
        - if portrait {
            FIX_PORTRAIT.0 as i32 + 14
        } else {
            0
        };
    (left, 10, right - left, panel_height - 20)
}

/// Bug fixes: each line on a paper ticket, ticked off as it lands. A known
/// issue keeps its warning color and mark instead.
pub(super) fn fixes(stage: &mut Stage<'_>) {
    let slide = stage.slide;
    heading(stage, true);
    let (first, pitch, height) = fix_rows(slide);
    let count = slide.rows.len().min(4);
    // One or two tickets are taller, and their lines are set larger.
    let size = if count <= 2 { 32.0 } else { FIX_TYPE };
    for (index, row) in slide.rows.iter().take(4).enumerate() {
        let top = first + index as i32 * pitch;
        let window = row_window(index, count);
        fix(stage, index, row, (top, height), size, window);
    }
    footnote(stage);
}

fn fix(
    stage: &mut Stage<'_>,
    index: usize,
    row: &WrappedRow,
    (top, height): (i32, i32),
    size: f32,
    window: (f32, f32),
) {
    let span = |from: f32, to: f32| within(window, from, to);
    let tag = row.tag.trim().to_uppercase();
    let open_issue = !tag.is_empty();
    let (open_from, open_to) = span(0.0, 0.3);
    let open = stage.ease(open_from, open_to);
    if open <= 0.0 {
        return;
    }
    stage.canvas.rounded_rect(
        (
            MARGIN as f32,
            top as f32,
            MARGIN as f32 + COLUMN as f32 * open,
            (top + height) as f32,
        ),
        22.0,
        if open_issue { WARNING } else { PAPER },
    );
    let portrait = stage.row_art.get(index).is_some_and(Option::is_some);
    let (left, offset, width, text_height) = fix_text_box(height, portrait);
    stage.centered_block(
        &row.primary,
        (left, top + offset, width, text_height),
        size,
        Face::Medium,
        PANEL,
        Align::Left,
        span(0.2, 0.5),
    );
    if portrait {
        let (art_width, art_height) = (FIX_PORTRAIT.0 as i32, FIX_PORTRAIT.1 as i32);
        let slot = (
            WIDTH - MARGIN - FIX_PAD - art_width,
            top + (height - art_height) / 2,
            art_width,
            art_height,
        );
        picture(
            stage,
            index,
            &row.primary,
            slot,
            10.0,
            (INSET, PAPER),
            span(0.3, 0.6),
        );
    }
    let center = (
        (MARGIN + FIX_PAD + FIX_MARK / 2) as f32,
        top as f32 + height as f32 / 2.0,
    );
    let (mark_from, mark_to) = span(0.55, 0.78);
    let grow = stage.pop(mark_from, mark_to).max(0.0);
    if stage.t >= mark_from && grow > 0.0 {
        if open_issue {
            warning_mark(stage, center, grow);
        } else {
            let (draw_from, draw_to) = span(0.66, 0.92);
            let drawn = stage.ease(draw_from, draw_to);
            check_mark(stage, center, grow, drawn);
        }
    }
    let (stamp_width, stamp_height) = stamp_extent(&tag, 17.0);
    stamp(
        stage,
        &tag,
        (
            (WIDTH - MARGIN - 18) as f32 - stamp_width,
            top as f32 - stamp_height / 2.0,
        ),
        17.0,
        (PANEL, WARNING),
        span(0.62, 0.74),
    );
}

/// The design size of a fixed issue's line.
const FIX_TYPE: f32 = 27.0;

/// A green disc whose tick is drawn stroke by stroke.
fn check_mark(stage: &mut Stage<'_>, center: (f32, f32), grow: f32, drawn: f32) {
    let radius = FIX_MARK as f32 / 2.0;
    stage.canvas.disc(center, radius * grow, FIXED);
    if drawn <= 0.0 {
        return;
    }
    let point = |x: f32, y: f32| (center.0 + x, center.1 + y);
    let (start, elbow, end) = (point(-10.0, 1.0), point(-3.0, 8.0), point(10.0, -7.0));
    // The short stroke is a third of the tick's length.
    let lerp = |a: (f32, f32), b: (f32, f32), amount: f32| {
        (a.0 + (b.0 - a.0) * amount, a.1 + (b.1 - a.1) * amount)
    };
    let mut path = vec![start, lerp(start, elbow, (drawn * 3.0).min(1.0))];
    if drawn > 1.0 / 3.0 {
        path.push(lerp(elbow, end, (drawn - 1.0 / 3.0) * 1.5));
    }
    stage.canvas.stroke(&path, 5.5, PAPER);
}

/// An ink triangle with the exclamation mark cut out of it.
fn warning_mark(stage: &mut Stage<'_>, center: (f32, f32), grow: f32) {
    let point = |x: f32, y: f32| (center.0 + x * grow, center.1 + y * grow);
    stage.canvas.polygon(
        &[point(0.0, -21.0), point(23.0, 18.0), point(-23.0, 18.0)],
        PANEL,
    );
    stage
        .canvas
        .stroke(&[point(0.0, -6.0), point(0.0, 4.0)], 4.5 * grow, WARNING);
    stage.canvas.disc(point(0.0, 11.5), 2.6 * grow, WARNING);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapped_media::stage::fit_text;

    /// `value` wraps whole into a box at no less than `smallest`.
    fn assert_fits(value: &str, face: Face, size: f32, smallest: f32, width: i32, height: i32) {
        let (fitted, rows) = fit_text(value, face, size, width, height);
        assert!(fitted >= smallest, "{value:?} set at {fitted}");
        assert!(
            rows.iter().all(|row| !row.contains('…')),
            "{value:?} is cut short"
        );
        assert_eq!(
            rows.join(" "),
            value.split_whitespace().collect::<Vec<_>>().join(" "),
            "{value:?} breaks inside a word"
        );
    }

    fn slide(kind: &str, rows: usize, subtitle: bool, note: bool) -> WrappedSlideData {
        let mut slide = WrappedSlideData::new(kind, "Updates");
        if subtitle {
            slide.headline = "Across 186 recorded picks.".to_owned();
        }
        if note {
            slide.lines = vec!["Known issues remain known.".to_owned()];
        }
        slide.rows = vec![WrappedRow::default(); rows];
        slide
    }

    #[test]
    fn hero_blocks_stay_on_the_card_and_keep_their_copy_readable() {
        const CHANGES: &str = "Win rate increased from 40% to 67%.\nRemoved from Captain's Mode.";
        for rows in 1..=3 {
            for subtitle in [false, true] {
                let slide = slide("story_hero_patch", rows, subtitle, false);
                let (first, block) = hero_blocks(&slide);
                assert!(first >= content_top(&slide, true));
                let last = first + (rows as i32 - 1) * (block.height + GAP) + block.height;
                assert!(last <= BOTTOM, "{rows} rows end at {last}");
                assert!(block.portrait.3 >= 90, "portrait {:?}", block.portrait);
                let (_, _, width, height) = block.name;
                assert_fits(
                    "Keeper of the Light",
                    Face::Black,
                    block.name_size,
                    32.0,
                    width,
                    height,
                );
                let (_, _, width, height) = block.changes;
                assert_fits(
                    CHANGES,
                    Face::Medium,
                    block.change_size,
                    25.0,
                    width - 26,
                    height,
                );
                // Four ability icons fit beside or under the name.
                let (left, _, icon) = block.strip;
                assert!(left + icon * 4 + 24 <= WIDTH - MARGIN - 16);
            }
        }
    }

    #[test]
    fn item_rows_stay_on_the_card_and_keep_their_copy_readable() {
        for rows in 1..=4 {
            let slide = slide("story_items", rows, false, false);
            let (first, row) = item_rows(&slide);
            assert!(first >= content_top(&slide, false));
            let frame = row.icon.1 + ITEM_BORDER * 2 + ITEM_SHADOW;
            let last = first + (rows as i32 - 1) * row.pitch + frame.max(row.line.1 + row.line.3);
            assert!(last <= BOTTOM, "{rows} rows end at {last}");
            assert!(row.pitch >= frame || rows == 1, "rows overlap");
            assert!(row.icon.0 >= ITEM_ICON.0 * 2, "icons are shown large");
            let (_, _, width, height) = row.name;
            assert_fits(
                "Black King Bar",
                Face::Black,
                row.name_size,
                34.0,
                width,
                height,
            );
            assert_fits(
                "Aghanim's Scepter",
                Face::Black,
                row.name_size,
                30.0,
                width,
                height,
            );
            let (_, _, width, height) = row.line;
            assert_fits(
                "Purchased in every game you lost.",
                Face::Medium,
                row.line_size + 2.0,
                26.0,
                width,
                height - 4,
            );
        }
        assert_eq!(
            leading_figure("41 of 48 games"),
            Some(("41", "of 48 games"))
        );
        assert_eq!(
            leading_figure("+12% win rate with it"),
            Some(("+12%", "win rate with it"))
        );
        assert_eq!(leading_figure("Bought 12 times"), None);
        assert_eq!(leading_figure("1,204,880,112 gold"), None);
    }

    #[test]
    fn eighty_character_fixes_fit_their_tickets_beside_a_portrait() {
        let lines = [
            "Fixed an issue where the player would buy back and immediately die again anyway.",
            "Fixed Black King Bar being purchased only after the team fight had already ended",
            "Fixed an issue where wards were placed exclusively on the high ground cliff. OK.",
            "Wwwwwwww mmmmmmmm wwwwwwww mmmmmmmm wwwwwwww mmmmmmmm wwwwwwww mmmmmmmm wwwwwww.",
        ];
        for subtitle in [false, true] {
            for note in [false, true] {
                let slide = slide("story_bugfixes", 4, subtitle, note);
                let (first, pitch, height) = fix_rows(&slide);
                assert!(first >= content_top(&slide, true));
                assert!(first + pitch * 3 + height <= BOTTOM);
                let (_, _, width, box_height) = fix_text_box(height, true);
                for (index, line) in lines.into_iter().enumerate() {
                    assert_eq!(line.chars().count(), 80);
                    // Prose stays at reading size on the fullest card. Even
                    // eighty of the widest letters stay whole and legible.
                    let smallest = if index < 3 { 25.0 } else { 20.0 };
                    assert_fits(line, Face::Medium, FIX_TYPE, smallest, width, box_height);
                }
            }
        }
    }

    const GREEN: [u8; 3] = [0, 200, 0];
    const BLUE: [u8; 3] = [0, 0, 220];

    fn flat(width: usize, height: usize, color: [u8; 3]) -> Vec<u8> {
        let [red, green, blue] = color;
        encode_png(&Canvas::new(width, height, Rgba::rgb(red, green, blue)))
    }

    fn count(slide: &WrappedSlideData, color: [u8; 3]) -> usize {
        stage::render_still(slide)
            .pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3] == color)
            .count()
    }

    fn rgb(color: Rgba) -> [u8; 3] {
        [color.0[0], color.0[1], color.0[2]]
    }

    /// Every change to one row reaches the finished card.
    fn assert_each_field_shows(slide: &WrappedSlideData, rows: usize) {
        let full = stage::render_still(slide).pixels;
        let changes: [fn(&mut WrappedRow); 4] = [
            |row| row.primary = "Another name".to_owned(),
            |row| row.secondary = "9 other words entirely".to_owned(),
            |row| row.tag = "OTHER".to_owned(),
            |row| row.value = if row.value > 0.0 { -1.0 } else { 1.0 },
        ];
        for index in 0..rows {
            for (field, change) in changes.iter().enumerate() {
                let mut changed = slide.clone();
                change(&mut changed.rows[index]);
                assert!(
                    full != stage::render_still(&changed).pixels,
                    "{:?} row {index} field {field} is not shown",
                    slide.layout
                );
            }
        }
    }

    /// One row more than a card holds is left off rather than squeezed in.
    fn assert_full(slide: &WrappedSlideData) {
        let mut crowded = slide.clone();
        crowded
            .rows
            .push(WrappedRow::new("Off the card", "Not shown"));
        assert!(
            stage::render_still(slide).pixels == stage::render_still(&crowded).pixels,
            "{:?} squeezes in another row",
            slide.layout
        );
    }

    #[test]
    fn hero_updates_show_each_portrait_its_icons_and_its_verdict() {
        let mut slide = WrappedSlideData::new("story_hero_patch", "Hero updates");
        slide.rows = ["Queen of Pain", "Pudge", "Keeper of the Light"]
            .into_iter()
            .map(|name| {
                let mut row = WrappedRow::new(name, "Win rate increased from 40% to 67%.");
                row.tag = "BUFFED".to_owned();
                row.value = 1.0;
                row.image_png = Some(flat(256, 144, GREEN));
                row.icons = vec![flat(128, 128, BLUE); 4];
                row
            })
            .collect();
        // Three portraits, each less the corner its stamp hangs over.
        let (width, height) = hero_portrait(&slide);
        let portraits = count(&slide, GREEN);
        assert!(
            portraits > width * height * 3 * 8 / 10 && portraits <= width * height * 3,
            "{portraits}"
        );
        let side = hero_icon(&slide);
        let icons = count(&slide, BLUE);
        assert!(
            icons > side * side * 12 * 9 / 10 && icons <= side * side * 12,
            "{icons}"
        );
        assert_each_field_shows(&slide, 3);
        assert_full(&slide);
        // A hero block shows four icons; one that does not decode is skipped.
        slide.rows[0].icons.push(flat(128, 128, BLUE));
        assert_eq!(count(&slide, BLUE), icons);
        slide.rows[0].icons[1] = b"not png".to_vec();
        assert_eq!(count(&slide, BLUE), icons);
        // A hero with nothing cached keeps its place with an initial.
        slide.rows[1].image_png = None;
        slide.rows[1].icons.clear();
        let left = count(&slide, GREEN);
        assert!(left < portraits && left > width * height * 2 * 8 / 10);
        assert_eq!(count(&slide, BLUE), icons * 2 / 3);
    }

    #[test]
    fn item_updates_frame_each_icon_at_twice_its_size_or_more() {
        let mut slide = WrappedSlideData::new("story_items", "Item updates");
        slide.rows = (0..4)
            .map(|_| {
                let mut row = WrappedRow::new("Blink Dagger", "41 of 48 games");
                row.tag = "SIGNATURE".to_owned();
                row.image_png = Some(flat(88, 64, GREEN));
                row
            })
            .collect();
        assert_full(&slide);
        for rows in (1..=4).rev() {
            slide.rows.truncate(rows);
            let (width, height) = item_icon(&slide);
            assert!(
                width >= 176 && height >= 128,
                "{rows} rows: {width}x{height}"
            );
            // Each icon whole, less its rounded corners and its stamp's overlap.
            let shown = count(&slide, GREEN);
            assert!(
                shown > width * height * rows * 8 / 10 && shown <= width * height * rows,
                "{rows} rows: {shown}"
            );
            assert_each_field_shows(&slide, rows);
        }
    }

    #[test]
    fn fixed_issues_are_ticked_and_known_issues_flagged() {
        let mut slide = WrappedSlideData::new("story_bugfixes", "Bug fixes");
        slide.rows = vec![WrappedRow::new("Fixed a rare case where the courier was used.", ""); 3];
        let ticks = count(&slide, rgb(FIXED));
        assert!(ticks > 3 * 1_000, "{ticks}");
        assert_eq!(count(&slide, rgb(WARNING)), 0);
        slide.rows.push(slide.rows[0].clone());
        assert_full(&slide);
        slide.rows.pop();
        slide.rows[2].tag = "KNOWN ISSUE".to_owned();
        // Two of the three ticks remain, and the third ticket turns to warning.
        assert_eq!(count(&slide, rgb(FIXED)), ticks * 2 / 3);
        assert!(count(&slide, rgb(WARNING)) > 50_000);
        // A ticket's tick is drawn after its line has landed.
        let mut stage = Stage::new(&slide);
        let (start, span) = row_window(0, 3);
        let early = stage.paint(start + span * 0.5);
        let ticked = early
            .pixels
            .chunks_exact(4)
            .any(|pixel| pixel[..3] == rgb(FIXED));
        assert!(!ticked, "the tick waits for its line");
        // A fix beside a hero keeps the portrait whole.
        slide.rows[0].image_png = Some(flat(256, 144, GREEN));
        let shown = count(&slide, GREEN);
        let (width, height) = FIX_PORTRAIT;
        assert!(shown > width * height * 9 / 10 && shown <= width * height);
    }
}
