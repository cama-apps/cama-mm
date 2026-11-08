//! The card layouts: big type, one reveal at a time.

use super::stage::{Align, Face, HEIGHT, Stage, WIDTH, count_up};
use super::*;

const MARGIN: i32 = 40;
const COLUMN: i32 = WIDTH - MARGIN * 2;
const TOP: i32 = 88;
/// Lowest y body content may reach, leaving clear air above the username footer.
const BOTTOM: i32 = HEIGHT - 72;
const COVER_ART_BAND: i32 = 300;
const COVER_FOOTER_HEIGHT: i32 = 64;

pub(super) fn paint(stage: &mut Stage<'_>) {
    match stage.slide.layout {
        WrappedLayout::Cover => cover(stage),
        WrappedLayout::BigNumber => big_number(stage),
        WrappedLayout::TopList => top_list(stage),
        WrappedLayout::Bars => bars(stage),
        WrappedLayout::People => people(stage),
        WrappedLayout::Curve => curve(stage),
        WrappedLayout::Tiles => tiles(stage),
        WrappedLayout::Spotlight => spotlight(stage),
        WrappedLayout::Persona => persona(stage),
        WrappedLayout::Award => super::awards::paint(stage),
        WrappedLayout::Recap => recap(stage),
        WrappedLayout::Details => details(stage),
    }
    chrome(stage);
}

/// Story progress across the top, like the segments on a phone-sized story.
fn chrome(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let count = slide.chapter_count.clamp(1, 40);
    let current = slide.chapter_index.min(count - 1);
    let gap = 5.0;
    let width = (COLUMN as f32 - gap * (count - 1) as f32) / count as f32;
    for index in 0..count {
        let left = MARGIN as f32 + index as f32 * (width + gap);
        let track = if index < current {
            p.fg
        } else {
            p.fg.scaled_alpha(0.28)
        };
        stage
            .canvas
            .rounded_rect((left, 28.0, left + width, 36.0), 4.0, track);
        if index == current {
            let filled = stage.ease(0.0, 1.0);
            stage
                .canvas
                .rounded_rect((left, 28.0, left + width * filled, 36.0), 4.0, p.fg);
        }
    }
    let label = if slide.year_label.is_empty() {
        "Cama Wrapped"
    } else {
        slide.year_label.as_str()
    };
    stage.block(
        label,
        (MARGIN, 44, COLUMN, 36),
        23.0,
        Face::Bold,
        p.fg,
        Align::Left,
        (0.0, 0.0),
    );
    if slide.layout != WrappedLayout::Cover {
        stage.block(
            &slide.username,
            (MARGIN, HEIGHT - 50, COLUMN, 36),
            23.0,
            Face::Bold,
            p.fg,
            Align::Left,
            (0.0, 0.0),
        );
    }
}

/// The rows a layout should show, derived from legacy `lines`/`series` copy
/// when a card has no structured rows.
fn card_rows(slide: &WrappedSlideData) -> Vec<WrappedRow> {
    if !slide.rows.is_empty() {
        return slide.rows.clone();
    }
    slide
        .lines
        .iter()
        .enumerate()
        .map(|(index, line)| WrappedRow {
            primary: line.clone(),
            secondary: String::new(),
            value: slide.series.get(index).copied().unwrap_or(0.0),
            image_png: None,
        })
        .collect()
}

fn cover(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    if let Some((width, height)) = stage.art.as_ref().map(|art| (art.width, art.height)) {
        // Keep the illustration's foreground at the bottom edge; the reveal
        // clips its upper scenery without cropping away the main subject.
        let art_bottom = HEIGHT - COVER_FOOTER_HEIGHT;
        let top = art_bottom - height as i32;
        let reveal = stage.ease(0.1, 0.7);
        let shown_from =
            art_bottom - ((COVER_ART_BAND - COVER_FOOTER_HEIGHT) as f32 * reveal) as i32;
        super::artwork::draw(
            stage,
            0,
            top,
            (0, shown_from, width as i32, art_bottom),
            0.0,
        );
    }
    stage.canvas.rounded_rect(
        (
            0.0,
            (HEIGHT - COVER_FOOTER_HEIGHT) as f32,
            WIDTH as f32,
            HEIGHT as f32,
        ),
        0.0,
        p.bg,
    );
    let mut y = 92;
    let headline = if slide.headline.is_empty() {
        "Your year.\nYour chaos."
    } else {
        &slide.headline
    };
    y = stage.block(
        headline,
        (MARGIN, y, COLUMN, 330),
        104.0,
        Face::Black,
        p.fg,
        Align::Left,
        (0.0, 0.42),
    );
    y = stage.block(
        &slide.stat_value,
        (MARGIN, y + 8, COLUMN, 88),
        54.0,
        Face::Black,
        p.accent,
        Align::Left,
        (0.42, 0.72),
    );
    // Normally empty; an empty-history disclosure still belongs on the cover.
    if let Some(note) = slide.lines.first() {
        stage.block(
            note,
            (
                MARGIN,
                y + 12,
                COLUMN,
                (HEIGHT - COVER_ART_BAND - 12 - y).max(0),
            ),
            28.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (0.55, 0.85),
        );
    }
}

/// A card gets one heading; its statistics carry the rest of the story.
fn heading(stage: &mut Stage<'_>) -> i32 {
    let slide = stage.slide;
    let value = if slide.title.is_empty() {
        &slide.headline
    } else {
        &slide.title
    };
    stage.block(
        value,
        (MARGIN, TOP, COLUMN, 146),
        66.0,
        Face::Black,
        stage.palette.fg,
        Align::Left,
        (0.0, 0.3),
    ) + 16
}

fn big_number(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    if let Some((width, height)) = stage
        .art
        .as_ref()
        .map(|art| (art.width as i32, art.height as i32))
    {
        let enter = stage.ease(0.35, 0.8);
        let left = WIDTH - width + 8 + ((1.0 - enter) * 340.0) as i32;
        let top = BOTTOM - height + 8;
        super::artwork::draw(stage, left, top, (0, 0, WIDTH, HEIGHT), 0.0);
    }
    let has_art = stage.art.is_some();
    let mut y = heading(stage);
    let spun = count_up(&slide.stat_value, stage.ease(0.3, 0.86));
    y = stage.stat(
        &slide.stat_value,
        &spun,
        (MARGIN, y, COLUMN, 216),
        176.0,
        p.accent,
        Align::Left,
        (0.24, 0.42),
    );
    y = stage.block(
        &slide.stat_label,
        (MARGIN, y, COLUMN, 96),
        38.0,
        Face::Bold,
        p.fg,
        Align::Left,
        (0.46, 0.68),
    ) + 12;
    if !slide.badge.is_empty() {
        y = stage.pill(
            &slide.badge,
            MARGIN,
            y,
            COLUMN,
            28.0,
            p.accent,
            p.on_accent,
            0.72,
        ) + 14;
    }
    let width = if has_art { COLUMN - 270 } else { COLUMN };
    let lines = slide.lines.iter().take(3).collect::<Vec<_>>();
    let room = (BOTTOM - y).max(0);
    let slot = room / lines.len().max(1) as i32;
    for (index, line) in lines.into_iter().enumerate() {
        let start = 0.66 + index as f32 * 0.08;
        y = stage.block(
            line,
            (MARGIN, y, width, slot),
            30.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (start, start + 0.22),
        ) + 6;
    }
}

fn top_list(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let mut y = heading(stage);
    y = stage.block(
        &slide.stat_label,
        (MARGIN, y, COLUMN, 44),
        28.0,
        Face::Medium,
        p.fg,
        Align::Left,
        (0.1, 0.34),
    ) + 12;
    if !slide.badge.is_empty() {
        y = stage.pill(&slide.badge, MARGIN, y, COLUMN, 26.0, p.fg, p.bg, 0.8) + 14;
    }
    let rows = card_rows(slide);
    let rows = &rows[..rows.len().min(5)];
    if rows.is_empty() {
        return;
    }
    let height = ((BOTTOM - y) / rows.len() as i32).min(112);
    let (frame_width, frame_height) = (
        super::artwork::ROW_FRAME.0 as i32,
        super::artwork::ROW_FRAME.1 as i32,
    );
    let portraits = height >= frame_height + 8 && stage.row_art.iter().any(Option::is_some);
    let number_width = if portraits { 54 } else { 76 };
    for (index, row) in rows.iter().enumerate() {
        let top = y + index as i32 * height;
        let start = 0.22 + index as f32 * 0.09;
        let enter = stage.ease(start, start + 0.3);
        if enter <= 0.0 {
            continue;
        }
        let x = ((1.0 - enter) * -300.0) as i32;
        let number = (index + 1).to_string();
        stage.stat(
            &number,
            &number,
            (MARGIN + x, top, number_width, height),
            72.0,
            p.accent,
            Align::Left,
            (0.0, 0.0),
        );
        let mut text = MARGIN + number_width + 6 + x;
        if portraits {
            let (left, art_top) = (text, top + (height - frame_height) / 2);
            // A hero without a cached portrait keeps its slot, so names line up.
            match stage.row_art.get_mut(index).and_then(Option::take) {
                Some(art) => {
                    let (width, art_height) = (art.width as i32, art.height as i32);
                    let frame = (left, art_top, left + width, art_top + art_height);
                    super::artwork::paint(&mut stage.canvas, &art, left, art_top, frame, 12.0);
                    stage.row_art[index] = Some(art);
                }
                None => stage.canvas.rounded_rect(
                    (
                        left as f32,
                        art_top as f32,
                        (left + frame_width) as f32,
                        (art_top + frame_height) as f32,
                    ),
                    12.0,
                    p.soft,
                ),
            }
            text += frame_width + 18;
        }
        let width = WIDTH - MARGIN - text;
        let name = if row.secondary.is_empty() {
            height - 6
        } else {
            (height * 6 / 10).max(36)
        };
        let bottom = stage.block(
            &row.primary,
            (text, top + 2, width, name),
            42.0,
            Face::Black,
            p.fg,
            Align::Left,
            (0.0, 0.0),
        );
        stage.block(
            &row.secondary,
            (text, bottom - 4, width, height - name),
            25.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (0.0, 0.0),
        );
    }
}

fn bars(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let mut y = heading(stage);
    let spun = count_up(&slide.stat_value, stage.ease(0.3, 0.8));
    y = stage.stat(
        &slide.stat_value,
        &spun,
        (MARGIN, y, COLUMN, 96),
        76.0,
        p.accent,
        Align::Left,
        (0.3, 0.44),
    );
    y = stage.block(
        &slide.stat_label,
        (MARGIN, y, COLUMN, 84),
        28.0,
        Face::Medium,
        p.fg,
        Align::Left,
        (0.42, 0.62),
    ) + 12;
    let rows = card_rows(slide);
    if rows.is_empty() {
        return;
    }
    let maximum = rows
        .iter()
        .map(|row| row.value)
        .filter(|value| value.is_finite())
        .fold(0.0_f64, f64::max);
    let peak = rows
        .iter()
        .position(|row| maximum > 0.0 && row.value == maximum);
    let baseline = (BOTTOM - 54) as f32;
    let top = (y as f32 + 8.0).min(baseline - 60.0);
    let gap = if rows.len() > 6 { 8.0 } else { 18.0 };
    let width = (COLUMN as f32 - gap * (rows.len() - 1) as f32) / rows.len() as f32;
    for (index, row) in rows.iter().enumerate() {
        let start = 0.16 + index as f32 * (0.4 / rows.len() as f32);
        let grow = stage.pop(start, start + 0.32);
        let left = MARGIN as f32 + index as f32 * (width + gap);
        let share = if maximum > 0.0 && row.value.is_finite() {
            (row.value / maximum) as f32
        } else {
            0.0
        };
        let bar = (baseline - top) * share;
        let color = if peak == Some(index) {
            p.accent
        } else {
            p.fg.scaled_alpha(0.8)
        };
        if stage.t >= start && bar > 0.0 {
            let height = (bar * grow).max(6.0);
            stage.canvas.rounded_rect(
                (left, baseline - height, left + width, baseline),
                (width / 3.0).min(14.0),
                color,
            );
        } else if share == 0.0 {
            stage.canvas.rounded_rect(
                (left, baseline - 6.0, left + width, baseline),
                3.0,
                p.fg.scaled_alpha(0.3),
            );
        }
        stage.block(
            &row.primary,
            (left as i32 - 2, baseline as i32 + 6, width as i32 + 8, 42),
            if rows.len() > 6 { 24.0 } else { 26.0 },
            Face::Bold,
            p.fg,
            if rows.len() > 6 {
                Align::Center
            } else {
                Align::Left
            },
            (0.0, 0.0),
        );
    }
}

fn people(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let y = heading(stage);
    let rows = card_rows(slide);
    let rows = &rows[..rows.len().min(5)];
    if rows.is_empty() {
        return;
    }
    let height = ((BOTTOM - y) / rows.len() as i32).min(170);
    let size = (height - 22).clamp(60, 118);
    for (index, row) in rows.iter().enumerate() {
        let top = y + index as i32 * height;
        let start = 0.2 + index as f32 * 0.12;
        let enter = stage.pop(start, start + 0.3);
        if stage.t < start {
            continue;
        }
        let center = ((MARGIN + size / 2) as f32, (top + size / 2) as f32);
        let avatar_drawn = match stage.avatars.get(index).and_then(Option::as_ref) {
            Some(avatar) if enter >= 0.98 => {
                let avatar = avatar.clone();
                stage
                    .canvas
                    .blit_scaled(&avatar, MARGIN, top, size as usize, size as usize);
                true
            }
            _ => false,
        };
        if !avatar_drawn {
            stage
                .canvas
                .disc(center, size as f32 / 2.0 * enter.max(0.0), p.accent);
            if enter >= 0.98 {
                let number = (index + 1).to_string();
                stage.stat(
                    &number,
                    &number,
                    (MARGIN, top + size / 6, size, size * 2 / 3),
                    size as f32 * 0.5,
                    p.on_accent,
                    Align::Center,
                    (0.0, 0.0),
                );
            }
        }
        let shift = ((1.0 - stage.ease(start, start + 0.3)) * 60.0) as i32;
        let (left, width) = (MARGIN + size + 20 + shift, COLUMN - size - 20);
        let bottom = stage.block(
            &row.primary,
            (left, top, width, height * 11 / 20),
            40.0,
            Face::Black,
            p.fg,
            Align::Left,
            (0.0, 0.0),
        );
        stage.block(
            &row.secondary,
            (left, bottom - 2, width, top + height - 8 - bottom),
            26.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (0.0, 0.0),
        );
    }
}

pub(super) fn curve_points(slide: &WrappedSlideData) -> Vec<f64> {
    let source = if let Some(gamba) = &slide.gamba {
        gamba
            .points
            .iter()
            .map(|point| point.cumulative as f64)
            .collect()
    } else if slide.series.iter().any(|value| value.is_finite()) {
        slide.series.clone()
    } else {
        slide.secondary_series.clone()
    };
    let finite = source
        .into_iter()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    const MOST: usize = 96;
    if finite.len() <= MOST {
        return finite;
    }
    (0..MOST)
        .map(|index| finite[index * (finite.len() - 1) / (MOST - 1)])
        .collect()
}

fn curve(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let mut y = heading(stage);
    let spun = count_up(&slide.stat_value, stage.ease(0.1, 0.92));
    y = stage.stat(
        &slide.stat_value,
        &spun,
        (MARGIN, y, COLUMN, 120),
        96.0,
        p.accent,
        Align::Left,
        (0.1, 0.26),
    );
    y = stage.block(
        &slide.stat_label,
        (MARGIN, y, COLUMN, 44),
        28.0,
        Face::Bold,
        p.fg,
        Align::Left,
        (0.2, 0.4),
    ) + 10;
    let footer = slide.lines.iter().take(2).collect::<Vec<_>>();
    let footer_height = footer.len() as i32 * 44;
    let chart = (
        MARGIN as f32,
        y as f32 + 8.0,
        (WIDTH - MARGIN) as f32,
        (BOTTOM - footer_height - 8) as f32,
    );
    let points = curve_points(slide);
    if points.len() >= 2 && chart.3 - chart.1 > 40.0 {
        draw_curve(stage, &points, chart);
    } else if slide.gamba.is_none() && points.is_empty() && !slide.lines.is_empty() {
        // Nothing to plot: let the copy carry the card.
    }
    let mut line_top = BOTTOM - footer_height;
    for line in footer {
        stage.block(
            line,
            (MARGIN, line_top, COLUMN, 42),
            26.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (0.7, 0.9),
        );
        line_top += 44;
    }
}

fn draw_curve(stage: &mut Stage<'_>, points: &[f64], chart: (f32, f32, f32, f32)) {
    let p = stage.palette;
    let (left, top, right, bottom) = chart;
    let minimum = points.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = (maximum - minimum).max(1.0);
    let position = |index: usize, value: f64| {
        (
            left + (right - left) * index as f32 / (points.len() - 1) as f32,
            bottom - ((value - minimum) / span) as f32 * (bottom - top),
        )
    };
    if minimum < 0.0 && maximum > 0.0 {
        let zero = position(0, 0.0).1;
        stage.canvas.rounded_rect(
            (left, zero - 1.5, right, zero + 1.5),
            1.5,
            p.fg.scaled_alpha(0.45),
        );
    }
    let draw = stage.ease(0.14, 0.9);
    let last = draw * (points.len() - 1) as f32;
    let whole = last.floor() as usize;
    let mut line = points
        .iter()
        .take(whole + 1)
        .enumerate()
        .map(|(index, value)| position(index, *value))
        .collect::<Vec<_>>();
    if whole + 1 < points.len() {
        let (a, b) = (
            position(whole, points[whole]),
            position(whole + 1, points[whole + 1]),
        );
        let fraction = last - whole as f32;
        line.push((a.0 + (b.0 - a.0) * fraction, a.1 + (b.1 - a.1) * fraction));
    }
    if line.len() >= 2 {
        let mut area = line.clone();
        area.push((line[line.len() - 1].0, bottom));
        area.push((line[0].0, bottom));
        stage.canvas.polygon(&area, p.accent.scaled_alpha(0.22));
        stage.canvas.stroke(&line, 7.0, p.accent);
    }
    if let Some(head) = line.last().copied() {
        stage.canvas.disc(head, 13.0, p.fg);
        stage.canvas.disc(head, 7.0, p.accent);
    }
}

fn tiles(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let y = heading(stage);
    let rows = card_rows(slide);
    let rows = &rows[..rows.len().min(6)];
    let lines = slide.lines.iter().take(2).collect::<Vec<_>>();
    let footer = lines.len() as i32 * 70;
    let grid_rows = rows.len().div_ceil(2).max(1) as i32;
    let gap = 14;
    let tile_width = (COLUMN - gap) / 2;
    let tile_height = (((BOTTOM - footer - y) - gap * (grid_rows - 1)) / grid_rows).min(200);
    for (index, row) in rows.iter().enumerate() {
        let (column, line) = ((index % 2) as i32, (index / 2) as i32);
        let (left, top) = (
            MARGIN + column * (tile_width + gap),
            y + line * (tile_height + gap),
        );
        let start = 0.18 + index as f32 * 0.08;
        let scale = stage.pop(start, start + 0.3);
        if stage.t < start {
            continue;
        }
        let (cx, cy) = (
            left as f32 + tile_width as f32 / 2.0,
            top as f32 + tile_height as f32 / 2.0,
        );
        let (half_width, half_height) = (
            tile_width as f32 / 2.0 * scale,
            tile_height as f32 / 2.0 * scale,
        );
        let inverted = (index + line as usize).is_multiple_of(2);
        let (fill, ink) = if inverted {
            (p.accent, p.on_accent)
        } else {
            (p.fg, p.bg)
        };
        stage.canvas.rounded_rect(
            (
                cx - half_width,
                cy - half_height,
                cx + half_width,
                cy + half_height,
            ),
            24.0,
            fill,
        );
        if scale >= 0.96 {
            let spun = count_up(&row.primary, stage.ease(start, start + 0.45));
            let padding = 18;
            let value_height = tile_height * 11 / 20;
            let bottom = stage.stat(
                &row.primary,
                &spun,
                (
                    left + padding,
                    top + 10,
                    tile_width - padding * 2,
                    value_height,
                ),
                74.0,
                ink,
                Align::Left,
                (0.0, 0.0),
            );
            stage.block(
                &row.secondary,
                (
                    left + padding,
                    bottom - 2,
                    tile_width - padding * 2,
                    top + tile_height - bottom - 8,
                ),
                26.0,
                Face::Medium,
                ink,
                Align::Left,
                (0.0, 0.0),
            );
        }
    }
    let mut line_top = BOTTOM - footer + 8;
    for (index, line) in lines.into_iter().enumerate() {
        let start = 0.66 + index as f32 * 0.1;
        stage.block(
            line,
            (MARGIN, line_top, COLUMN, 66),
            28.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (start, start + 0.2),
        );
        line_top += 70;
    }
}

fn spotlight(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let mut y = TOP;
    if let Some((width, height)) = stage
        .art
        .as_ref()
        .map(|art| (art.width as i32, art.height as i32))
    {
        let left = (WIDTH - width) / 2;
        // Unveiling in place keeps a detailed image out of every frame's diff.
        // Rows unveil top to bottom so each frame only adds new rows.
        let open = stage.ease(0.05, 0.5);
        let reveal = (left, y, left + width, y + (height as f32 * open) as i32);
        super::artwork::draw(stage, left, y, reveal, 26.0);
        y += height + 16;
    }
    y = stage.block(
        if slide.headline.is_empty() {
            &slide.title
        } else {
            &slide.headline
        },
        (MARGIN, y, COLUMN, 100),
        60.0,
        Face::Black,
        p.fg,
        Align::Left,
        (0.25, 0.5),
    ) + 6;
    let spun = count_up(&slide.stat_value, stage.ease(0.35, 0.86));
    y = stage.stat(
        &slide.stat_value,
        &spun,
        (MARGIN, y, COLUMN, 110),
        92.0,
        p.accent,
        Align::Left,
        (0.32, 0.5),
    );
    y = stage.block(
        &slide.stat_label,
        (MARGIN, y, COLUMN, 88),
        32.0,
        Face::Bold,
        p.fg,
        Align::Left,
        (0.5, 0.7),
    ) + 10;
    if !slide.badge.is_empty() {
        y = stage.pill(
            &slide.badge,
            MARGIN,
            y,
            COLUMN,
            26.0,
            p.accent,
            p.on_accent,
            0.72,
        ) + 12;
    }
    let lines = slide.lines.iter().take(3).collect::<Vec<_>>();
    let slot = (BOTTOM - y).max(0) / lines.len().max(1) as i32;
    for (index, line) in lines.into_iter().enumerate() {
        let start = 0.68 + index as f32 * 0.08;
        y = stage.block(
            line,
            (MARGIN, y, COLUMN, slot),
            28.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (start, start + 0.22),
        ) + 4;
    }
}

fn persona(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let mut y = TOP + 48;
    y = stage.block(
        &slide.stat_value,
        (MARGIN, y, COLUMN, 330),
        112.0,
        Face::Black,
        p.accent,
        Align::Center,
        (0.14, 0.5),
    ) + 32;
    // Evidence only: copy in `lines` is not repeated as pills.
    for (index, row) in slide.rows.iter().take(3).enumerate() {
        let start = 0.6 + index as f32 * 0.09;
        let text = if row.secondary.is_empty() {
            row.primary.clone()
        } else {
            format!("{} {}", row.primary, row.secondary)
        };
        let left = MARGIN + (COLUMN - stage.pill_width(&text, COLUMN, 28.0)) / 2;
        y = stage.pill(&text, left, y, COLUMN, 28.0, p.accent, p.on_accent, start) + 10;
    }
    let lines = slide.lines.iter().take(2).collect::<Vec<_>>();
    let slot = (BOTTOM - y).max(0) / lines.len().max(1) as i32;
    for (index, line) in lines.into_iter().enumerate() {
        let start = 0.7 + index as f32 * 0.08;
        y = stage.block(
            line,
            (MARGIN + 10, y, COLUMN - 20, slot),
            28.0,
            Face::Medium,
            p.fg,
            Align::Center,
            (start, start + 0.22),
        );
    }
}

/// The share card: everything worth screenshotting on one image.
fn recap(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let mut y = 128;
    y = stage.block(
        &slide.stat_value,
        (MARGIN, y, COLUMN, 72),
        54.0,
        Face::Black,
        p.fg,
        Align::Left,
        (0.08, 0.3),
    );
    y = stage.pill(
        &slide.headline,
        MARGIN,
        y + 6,
        COLUMN,
        34.0,
        p.accent,
        p.on_accent,
        0.25,
    ) + 22;
    let rows = card_rows(slide);
    let (left_width, gap) = (340, 24);
    let right = MARGIN + left_width + gap;
    let right_width = COLUMN - left_width - gap;
    stage.block(
        "Top heroes",
        (MARGIN, y, left_width, 40),
        26.0,
        Face::Bold,
        p.accent,
        Align::Left,
        (0.3, 0.45),
    );
    stage.block(
        "The numbers",
        (right, y, right_width, 40),
        26.0,
        Face::Bold,
        p.accent,
        Align::Left,
        (0.3, 0.45),
    );
    let content = y + 40;
    let limit = HEIGHT - 150;
    let left_rows = rows.iter().take(5).collect::<Vec<_>>();
    let row_height = ((limit - content) / left_rows.len().max(1) as i32).min(66);
    for (index, row) in left_rows.iter().enumerate() {
        let top = content + index as i32 * row_height;
        let start = 0.34 + index as f32 * 0.06;
        let entry = format!("{} {}", index + 1, row.primary);
        stage.block(
            &entry,
            (MARGIN, top, left_width, row_height),
            32.0,
            Face::Bold,
            p.fg,
            Align::Left,
            (start, start + 0.2),
        );
    }
    let numbers = slide.lines.iter().take(5).collect::<Vec<_>>();
    let number_height = ((limit - content) / numbers.len().max(1) as i32).min(100);
    for (index, line) in numbers.iter().enumerate() {
        let top = content + index as i32 * number_height;
        let start = 0.38 + index as f32 * 0.06;
        let (value, label) = line.split_once('|').unwrap_or((line.as_str(), ""));
        let spun = count_up(value, stage.ease(start, start + 0.4));
        let bottom = stage.stat(
            value,
            &spun,
            (right, top, right_width, number_height * 6 / 10),
            44.0,
            p.fg,
            Align::Left,
            (start, start + 0.2),
        );
        stage.block(
            label,
            (right, bottom - 4, right_width, number_height * 4 / 10),
            21.0,
            Face::Medium,
            p.fg,
            Align::Left,
            (start, start + 0.2),
        );
    }
    if !slide.badge.is_empty() {
        let show = stage.ease(0.78, 0.96);
        stage.canvas.rounded_rect(
            (
                MARGIN as f32 - 8.0,
                794.0,
                (WIDTH - MARGIN + 8) as f32,
                828.0,
            ),
            17.0,
            p.accent.scaled_alpha(show),
        );
        stage.block(
            &slide.badge,
            (MARGIN + 8, 798, COLUMN - 16, 28),
            22.0,
            Face::Bold,
            p.on_accent,
            Align::Left,
            (0.78, 0.96),
        );
    }
}

fn details(stage: &mut Stage<'_>) {
    let (slide, p) = (stage.slide, stage.palette);
    let mut y = TOP;
    y = stage.block(
        &slide.title,
        (MARGIN, y, COLUMN, 130),
        56.0,
        Face::Black,
        p.fg,
        Align::Left,
        (0.0, 0.0),
    ) + 4;
    y = stage.block(
        &slide.headline,
        (MARGIN, y, COLUMN, 90),
        30.0,
        Face::Bold,
        p.accent,
        Align::Left,
        (0.0, 0.0),
    ) + 16;
    let rows = card_rows(slide);
    let rows = &rows[..rows.len().min(8)];
    if rows.is_empty() {
        return;
    }
    let height = ((BOTTOM - y) / rows.len() as i32).min(124);
    for (index, row) in rows.iter().enumerate() {
        let top = y + index as i32 * height;
        stage.canvas.rounded_rect(
            (
                MARGIN as f32,
                top as f32 + 6.0,
                MARGIN as f32 + 8.0,
                (top + height - 12) as f32,
            ),
            4.0,
            p.accent,
        );
        let text = if row.secondary.is_empty() {
            row.primary.clone()
        } else {
            format!("{} {}", row.primary, row.secondary)
        };
        stage.block(
            &text,
            (MARGIN + 24, top, COLUMN - 24, height - 8),
            if rows.len() <= 2 { 40.0 } else { 30.0 },
            if rows.len() <= 2 {
                Face::Bold
            } else {
                Face::Medium
            },
            p.fg,
            Align::Left,
            (0.0, 0.0),
        );
    }
}
