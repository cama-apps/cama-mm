//! The visual grammar of Wrapped: large type, ink illustrations, and one reveal
//! at a time. Coordinates stay in the Discord-friendly 800 × 600 canvas.

use super::*;
use std::borrow::Cow;

const INK: Rgba = Rgba::rgb(25, 45, 43);
const PAPER: Rgba = Rgba::rgb(255, 249, 239);
const OXIDE: Rgba = Rgba::rgb(132, 43, 33);
const CORAL: Rgba = Rgba::rgb(219, 103, 77);
const OCHRE: Rgba = Rgba::rgb(229, 186, 86);
const SAND: Rgba = Rgba::rgb(233, 216, 184);
const TEAL: Rgba = Rgba::rgb(25, 45, 43);
const JADE: Rgba = Rgba::rgb(152, 190, 169);

pub(super) fn render(slide: &WrappedSlideData) -> Canvas {
    let (background, foreground, accent) = match slide.kind.as_str() {
        "story_intro" => (TEAL, PAPER, JADE),
        "story_moment" | "story_identity" if slide.artwork == WrappedArtwork::Ward => {
            (TEAL, PAPER, JADE)
        }
        "story_games" => (OCHRE, INK, OXIDE),
        "story_identity" => (SAND, INK, OXIDE),
        "story_moment" if slide.chapter_index.is_multiple_of(2) => (CORAL, INK, PAPER),
        "story_moment" => (SAND, INK, OXIDE),
        "story_hero" => (OXIDE, PAPER, OCHRE),
        "story_finale" => (INK, PAPER, OCHRE),
        "story_teammates" => (PAPER, INK, OXIDE),
        "story_rivals" => (SAND, INK, OXIDE),
        "chart_rating" | "chart_gamba" => (INK, PAPER, OCHRE),
        "story_lanes" => (OCHRE, INK, OXIDE),
        "story_packages" => (CORAL, INK, PAPER),
        "awards" => (OXIDE, PAPER, OCHRE),
        _ => (PAPER, INK, OXIDE),
    };
    let mut canvas = Canvas::new(800, 600, background);
    if matches!(slide.kind.as_str(), "story_intro" | "story_finale") {
        artwork::scene(&mut canvas, slide.kind == "story_finale");
    } else {
        artwork::lanes(
            &mut canvas,
            (598, 16),
            36,
            Rgba([accent.0[0], accent.0[1], accent.0[2], 150]),
        );
    }
    text(
        &mut canvas,
        &slide.year_label,
        (40, 24, 470, 30),
        19.0,
        foreground,
        false,
    );
    let count = slide.chapter_count.max(1);
    let chapter = slide.chapter_index.min(count - 1) + 1;

    text(
        &mut canvas,
        &format!("{chapter:02} / {count:02}"),
        (660, 24, 100, 26),
        18.0,
        foreground,
        true,
    );
    canvas.fill_rect(40, 66, 760, 69, foreground);
    match slide.kind.as_str() {
        "story_intro" => intro(&mut canvas, slide, foreground, accent),
        "story_games" | "story_moment" => reveal(&mut canvas, slide, foreground, accent),
        "story_packages" => package(&mut canvas, slide, foreground),
        "story_identity" => identity(&mut canvas, slide, foreground, accent),
        "story_hero" => hero(&mut canvas, slide, foreground, accent),
        "story_finale" => finale(&mut canvas, slide, foreground, accent),
        "story_lanes" => lanes(&mut canvas, slide, foreground, accent),
        "story_teammates" | "story_rivals" => people(&mut canvas, slide, foreground, accent),
        "chart_rating" => super::draw_chart(&mut canvas, slide, accent),
        "chart_gamba" => gamba(&mut canvas, slide),
        _ => details(&mut canvas, slide, foreground, accent),
    }
    // Footer is deliberately separate from the last content baseline.
    text(
        &mut canvas,
        &slide.username,
        (40, 559, 520, 25),
        16.0,
        foreground,
        false,
    );
    text(
        &mut canvas,
        "Cama Wrapped",
        (616, 559, 144, 25),
        16.0,
        foreground,
        true,
    );
    canvas.fill_rect(40, 546, 760, 548, foreground);
    canvas.fill_rect(40, 546, 40 + (720 * chapter / count) as i32, 550, accent);
    canvas
}

fn package(canvas: &mut Canvas, slide: &WrappedSlideData, fg: Rgba) {
    text(canvas, &slide.title, (40, 88, 700, 42), 28.0, fg, false);
    text(canvas, &slide.headline, (40, 142, 700, 52), 35.0, fg, true);
    text(
        canvas,
        &slide.stat_value,
        (40, 209, 700, 70),
        56.0,
        fg,
        true,
    );
    text(
        canvas,
        &slide.stat_label,
        (40, 284, 700, 31),
        22.0,
        fg,
        true,
    );
    lines(canvas, &slide.lines, (40, 331, 700, 204), 22.0, fg, false);
}

fn gamba(canvas: &mut Canvas, slide: &WrappedSlideData) {
    text(canvas, &slide.title, (40, 85, 700, 32), 24.0, OCHRE, true);
    if let Some(data) = &slide.gamba {
        let png = draw_gamba_chart(
            &slide.username,
            data.degen_score,
            &data.degen_title,
            &data.points,
            data.stats,
        )
        .into_inner();
        if let Some(chart) = decode_png(&png) {
            canvas.blit_opaque(&chart, 50, 123);
        }
    } else {
        text(
            canvas,
            "No recorded wagers yet.",
            (50, 240, 700, 60),
            32.0,
            PAPER,
            true,
        );
    }
    // The graph already contains betting totals. Its supporting footer is
    // retained in the readable message rather than colliding with the plot.
    if let Some(line) = slide.lines.first() {
        text(canvas, line, (50, 525, 700, 20), 14.0, PAPER, false);
    }
}

fn intro(canvas: &mut Canvas, slide: &WrappedSlideData, fg: Rgba, accent: Rgba) {
    text(canvas, &slide.title, (40, 95, 450, 42), 27.0, fg, false);
    let headline = if slide.headline.is_empty() {
        "Your year.\nYour chaos."
    } else {
        &slide.headline
    };
    text(canvas, headline, (40, 155, 430, 185), 66.0, fg, true);
    text(
        canvas,
        &slide.username,
        (40, 365, 430, 62),
        34.0,
        accent,
        true,
    );
    lines(canvas, &slide.lines, (40, 451, 430, 78), 21.0, fg, false);
}

fn reveal(canvas: &mut Canvas, slide: &WrappedSlideData, fg: Rgba, accent: Rgba) {
    let art = if slide.kind == "story_games" {
        WrappedArtwork::Courier
    } else {
        slide.artwork
    };
    artwork::hero(canvas, art, (470, 218, 310, 310), accent);
    text(canvas, &slide.title, (40, 90, 700, 42), 28.0, fg, false);
    text(canvas, &slide.headline, (40, 144, 700, 72), 34.0, fg, true);
    text(
        canvas,
        &slide.stat_value,
        (40, 235, 425, 115),
        100.0,
        fg,
        true,
    );
    text(
        canvas,
        &slide.stat_label,
        (44, 359, 415, 62),
        25.0,
        fg,
        true,
    );
    lines(canvas, &slide.lines, (44, 437, 415, 96), 20.0, fg, false);
}

fn identity(canvas: &mut Canvas, slide: &WrappedSlideData, fg: Rgba, accent: Rgba) {
    artwork::hero(canvas, slide.artwork, (480, 218, 300, 300), accent);
    text(canvas, &slide.title, (40, 90, 700, 44), 28.0, fg, false);
    text(canvas, &slide.headline, (40, 156, 430, 78), 29.0, fg, true);
    text(
        canvas,
        &slide.stat_value,
        (40, 250, 430, 115),
        57.0,
        accent,
        true,
    );
    text(
        canvas,
        &slide.stat_label,
        (40, 378, 430, 48),
        22.0,
        fg,
        true,
    );
    lines(canvas, &slide.lines, (40, 440, 430, 90), 20.0, fg, false);
}

fn hero(canvas: &mut Canvas, slide: &WrappedSlideData, fg: Rgba, accent: Rgba) {
    text(canvas, &slide.title, (40, 88, 700, 40), 27.0, fg, false);
    text(canvas, &slide.headline, (40, 136, 700, 80), 55.0, fg, true);
    text(
        canvas,
        &slide.stat_value,
        (40, 220, 680, 43),
        34.0,
        accent,
        true,
    );
    text(
        canvas,
        &slide.stat_label,
        (40, 270, 700, 30),
        20.0,
        fg,
        false,
    );
    for (index, line) in slide.lines.iter().take(3).enumerate() {
        let y = 310 + index as i32 * 73;
        canvas.fill_rect(40, y, 91, y + 51, accent);
        text(
            canvas,
            &(index + 1).to_string(),
            (51, y + 2, 35, 44),
            32.0,
            OXIDE,
            true,
        );
        text(canvas, line, (110, y, 630, 58), 24.0, fg, index == 0);
    }
}

fn details(canvas: &mut Canvas, slide: &WrappedSlideData, fg: Rgba, accent: Rgba) {
    text(canvas, &slide.title, (40, 88, 700, 54), 35.0, fg, true);
    text(
        canvas,
        &slide.headline,
        (40, 148, 700, 46),
        23.0,
        accent,
        true,
    );
    let count = slide.lines.len().clamp(1, 7);
    let row_height = 330 / count as i32;
    for (index, line) in slide.lines.iter().take(7).enumerate() {
        let y = 204 + index as i32 * row_height;
        canvas.fill_rect(40, y + 3, 47, y + row_height - 11, accent);
        text(
            canvas,
            line,
            (65, y, 673, row_height - 13),
            if count <= 2 { 29.0 } else { 23.0 },
            fg,
            count <= 2,
        );
    }
}

fn lanes(canvas: &mut Canvas, slide: &WrappedSlideData, fg: Rgba, accent: Rgba) {
    text(canvas, &slide.title, (40, 88, 700, 54), 36.0, fg, true);
    text(canvas, &slide.headline, (40, 145, 700, 38), 25.0, fg, false);
    text(
        canvas,
        &slide.stat_label,
        (40, 185, 700, 30),
        18.0,
        fg,
        false,
    );
    let maximum = slide
        .series
        .iter()
        .copied()
        .filter(|n| n.is_finite())
        .fold(1.0_f64, f64::max);
    for (index, line) in slide.lines.iter().take(3).enumerate() {
        let y = 224 + index as i32 * 98;
        text(canvas, line, (40, y, 700, 35), 24.0, fg, true);
        let value = slide
            .series
            .get(index)
            .copied()
            .filter(|n| n.is_finite())
            .unwrap_or(0.0);
        canvas.fill_rect(40, y + 44, 760, y + 69, PAPER);
        canvas.fill_rect(
            40,
            y + 44,
            40 + (720.0 * (value / maximum).clamp(0.0, 1.0)) as i32,
            y + 69,
            accent,
        );
    }
}

fn people(canvas: &mut Canvas, slide: &WrappedSlideData, fg: Rgba, accent: Rgba) {
    text(canvas, &slide.title, (40, 87, 700, 51), 36.0, fg, true);
    text(canvas, &slide.headline, (40, 144, 700, 47), 25.0, fg, false);
    let count = slide.lines.len().clamp(1, 6);
    let height = 337 / count as i32;
    for (index, line) in slide.lines.iter().take(6).enumerate() {
        let top = 201 + index as i32 * height;
        let size = (height - 8).min(58) as usize;
        if let Some(avatar) = slide.avatars.get(index).and_then(|a| decode_png(&a.png)) {
            canvas.blit_scaled(&avatar, 40, top, size, size);
        } else {
            canvas.circle(
                (40 + size as i32 / 2, top + size as i32 / 2),
                size as i32 / 2,
                accent,
            );
            text(
                canvas,
                &(index + 1).to_string(),
                (53, top + 6, 32, 38),
                25.0,
                PAPER,
                true,
            );
        }
        text(canvas, line, (116, top, 624, height - 7), 24.0, fg, false);
    }
}

fn finale(canvas: &mut Canvas, slide: &WrappedSlideData, fg: Rgba, accent: Rgba) {
    text(canvas, &slide.title, (40, 90, 700, 45), 28.0, fg, false);
    text(
        canvas,
        &slide.headline,
        (40, 145, 700, 96),
        52.0,
        accent,
        true,
    );
    let start = if slide.stat_value.is_empty() {
        258
    } else {
        text(
            canvas,
            &slide.stat_value,
            (40, 251, 700, 44),
            29.0,
            fg,
            true,
        );
        304
    };
    lines(
        canvas,
        &slide.lines,
        (40, start, 700, 500 - start),
        24.0,
        fg,
        false,
    );
    text(
        canvas,
        &slide.stat_label,
        (40, 511, 700, 25),
        18.0,
        accent,
        false,
    );
}

fn lines(
    canvas: &mut Canvas,
    values: &[String],
    rect: (i32, i32, i32, i32),
    size: f32,
    color: Rgba,
    bold: bool,
) {
    if values.is_empty() {
        return;
    }
    let count = values.len().min(7);
    let height = rect.3 / count as i32;
    for (index, value) in values.iter().take(count).enumerate() {
        text(
            canvas,
            value,
            (rect.0, rect.1 + index as i32 * height, rect.2, height - 3),
            size,
            color,
            bold,
        );
    }
}

/// Fits complete text by measuring actual glyph advances, including long words
/// and explicit line breaks. Only extreme input gets a visible ellipsis.
pub(super) fn text(
    canvas: &mut Canvas,
    value: &str,
    rect: (i32, i32, i32, i32),
    size: f32,
    color: Rgba,
    bold: bool,
) {
    if value.is_empty() || rect.3 <= 0 {
        return;
    }
    let Some(font) = wrapped_font(bold) else {
        // The runtime image bundles the pinned fonts. Keep the existing fail-soft
        // path useful for diagnostics when an asset is unavailable.
        let scale = ((size / 11.0) as i32).clamp(1, 3);
        let maximum = (rect.2 / (6 * scale)).max(1) as usize;
        for (index, line) in value
            .chars()
            .collect::<Vec<_>>()
            .chunks(maximum)
            .take((rect.3 / (9 * scale)).max(1) as usize)
            .enumerate()
        {
            canvas.text_case_sensitive(
                rect.0,
                rect.1 + index as i32 * 9 * scale,
                &line.iter().collect::<String>(),
                color,
                scale,
            );
        }
        return;
    };
    let (size, lines) = fit_lines(value, font, size, rect.2, rect.3);
    let advance = size * 1.16;
    for (index, line) in lines.iter().enumerate() {
        draw_type(
            canvas,
            line,
            font,
            (rect.0, rect.1 + (index as f32 * advance) as i32),
            size,
            color,
        );
    }
}

fn fit_lines(
    value: &str,
    font: &Font,
    maximum: f32,
    width: i32,
    height: i32,
) -> (f32, Vec<String>) {
    let mut size = maximum;
    loop {
        let mut rows = Vec::new();
        for paragraph in value.split('\n') {
            let mut line = String::new();
            for word in paragraph.split_whitespace() {
                let joined = if line.is_empty() {
                    word.to_owned()
                } else {
                    format!("{line} {word}")
                };
                if truetype_text_width(&joined, size, font) <= width {
                    line = joined;
                    continue;
                }
                if !line.is_empty() {
                    rows.push(std::mem::take(&mut line));
                }
                for character in word.chars() {
                    let mut next = line.clone();
                    next.push(character);
                    if !line.is_empty() && truetype_text_width(&next, size, font) > width {
                        rows.push(std::mem::take(&mut line));
                    }
                    line.push(character);
                }
            }
            if !line.is_empty() {
                rows.push(line);
            }
        }
        if rows.len() as f32 * size * 1.16 <= height as f32 || size <= 13.0 {
            let maximum_rows = (height as f32 / (size * 1.16)).floor().max(1.0) as usize;
            if rows.len() > maximum_rows {
                rows.truncate(maximum_rows);
                if let Some(last) = rows.last_mut() {
                    while !last.is_empty()
                        && truetype_text_width(&format!("{last}…"), size, font) > width
                    {
                        last.pop();
                    }
                    last.push('…');
                }
            }
            return (size, rows);
        }
        size = (size - 1.0).max(13.0);
    }
}

fn draw_type(
    canvas: &mut Canvas,
    value: &str,
    font: &Font,
    origin: (i32, i32),
    size: f32,
    color: Rgba,
) {
    let baseline = origin.1 as f32
        + font
            .horizontal_line_metrics(size)
            .map_or(size, |m| m.ascent);
    let mut x = origin.0 as f32;
    let mut previous = None;
    for character in value.chars() {
        if let Some(previous) = previous {
            x += font
                .horizontal_kern(previous, character, size)
                .unwrap_or_default();
        }
        let (metrics, bitmap) = font.rasterize(character, size);
        let left = x.round() as i32 + metrics.xmin;
        let top = baseline.round() as i32 - metrics.ymin - metrics.height as i32;
        for y in 0..metrics.height {
            for x in 0..metrics.width {
                let alpha = bitmap[y * metrics.width + x];
                if alpha > 0 {
                    canvas.set(
                        left + x as i32,
                        top + y as i32,
                        Rgba([color.0[0], color.0[1], color.0[2], alpha]),
                    );
                }
            }
        }
        x += metrics.advance_width;
        previous = Some(character);
    }
}

pub(super) fn animate(
    canvas: &Canvas,
    slide: &WrappedSlideData,
) -> Result<Vec<u8>, gif::EncodingError> {
    // Quantize the complete poster once, retaining the authored palette and
    // antialiased type. Every reveal frame shares these colors and indices.
    let mut rgba = canvas.pixels.clone();
    let final_frame = gif::Frame::from_rgba_speed(800, 600, &mut rgba, 10);
    let palette = final_frame.palette.as_deref().unwrap_or_default();
    let indexed = final_frame.buffer.as_ref();
    let nearest = |rgb: [u8; 3]| -> u8 {
        palette
            .chunks_exact(3)
            .enumerate()
            .min_by_key(|(_, p)| {
                (0..3)
                    .map(|c| (i32::from(p[c]) - i32::from(rgb[c])).pow(2))
                    .sum::<i32>()
            })
            .map_or(0, |(index, _)| index as u8)
    };
    let glow = nearest([152, 190, 169]);
    let background = indexed[240 * 800 + 20];
    let mut bytes = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut bytes, 800, 600, palette)?;
        // No loop extension: one reveal, then a readable final poster. Still
        // mode remains explicit because client playback preferences vary.
        for frame_index in 0..10 {
            let mut buffer = indexed.to_vec();
            if frame_index < 9 {
                let progress = frame_index as f32 / 9.0;
                if slide.kind == "story_intro" {
                    // A rune travels up the battlefield lanes. The entire
                    // title remains readable while the world comes alive.
                    let x = 515 + (progress * 230.0) as i32;
                    let y = 510 - (progress * 345.0) as i32;
                    for dy in -9_i32..=9 {
                        for dx in -9_i32..=9 {
                            if dx.abs() + dy.abs() == 9 {
                                buffer[((y + dy) * 800 + x + dx) as usize] = glow;
                            }
                        }
                    }
                    for tail in 1..24 {
                        let tx = x - tail;
                        let ty = y + tail * 3 / 2;
                        if ty < 540 {
                            buffer[(ty * 800 + tx) as usize] = glow;
                        }
                    }
                } else {
                    let art = if slide.kind == "story_games" {
                        WrappedArtwork::Courier
                    } else {
                        slide.artwork
                    };
                    let easing = 1.0 - (1.0 - progress).powi(3);
                    let shift = (125.0 * (1.0 - easing)) as usize;
                    // Move only the illustration region; semantic text never
                    // disappears or gets swept into an animation mask.
                    for y in 218..530 {
                        buffer[y * 800 + 470..y * 800 + 790].fill(background);
                    }
                    for y in 218..530 {
                        for x in 470..790 {
                            if art == WrappedArtwork::Rune {
                                let angle = (1.0 - easing) * 0.45;
                                let dx = x as f32 - 630.0;
                                let dy = y as f32 - 374.0;
                                let sx =
                                    (630.0 + dx * angle.cos() + dy * angle.sin()).round() as i32;
                                let sy =
                                    (374.0 - dx * angle.sin() + dy * angle.cos()).round() as i32;
                                if (470..790).contains(&sx) && (218..530).contains(&sy) {
                                    buffer[y * 800 + x] = indexed[(sy * 800 + sx) as usize];
                                }
                            } else if x + shift < 800 {
                                buffer[y * 800 + x + shift] = indexed[y * 800 + x];
                            }
                        }
                    }
                }
            }
            encoder.write_frame(&gif::Frame {
                width: 800,
                height: 600,
                delay: if frame_index == 9 { 250 } else { 8 },
                dispose: gif::DisposalMethod::Keep,
                buffer: Cow::Owned(buffer),
                ..gif::Frame::default()
            })?;
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_is_bounded_and_ends_on_the_complete_poster() {
        let mut slide = WrappedSlideData::new("story_games", "You kept coming back");
        slide.stat_value = "247".into();
        let final_canvas = render(&slide);
        let media = render_wrapped_media(&slide, true).unwrap();
        assert_eq!(media.filename, "wrapped_slide.gif");
        assert!(media.bytes.len() < 4 * 1024 * 1024);
        let mut decoder = gif::DecodeOptions::new()
            .read_info(media.bytes.as_slice())
            .unwrap();
        let mut frames = Vec::new();
        while let Some(frame) = decoder.read_next_frame().unwrap() {
            frames.push(frame.buffer.to_vec());
        }
        assert_eq!(frames.len(), 10);
        assert_ne!(frames[0], frames[9]);
        let mut rgba = final_canvas.pixels;
        let expected = gif::Frame::from_rgba_speed(800, 600, &mut rgba, 10);
        assert_eq!(frames[9], expected.buffer.as_ref());
        assert_eq!(
            render_wrapped_media(&slide, false).unwrap().filename,
            "wrapped_slide.png"
        );
        slide.kind = "story_finale".into();
        assert_eq!(
            render_wrapped_media(&slide, true).unwrap().filename,
            "wrapped_slide.png"
        );
    }

    #[test]
    fn long_words_and_names_fit_actual_glyph_bounds() {
        for value in [
            "Alexandria the Unreasonably Determined",
            "éééééééééééééééééééééééééééééééééééééééé",
            "A very long award with actual statistics and a very detailed explanation of how it was earned.",
        ] {
            let mut canvas = Canvas::new(400, 150, PAPER);
            text(&mut canvas, value, (10, 10, 320, 100), 44.0, INK, true);
            assert!(canvas.pixels.chunks_exact(4).any(|pixel| pixel != PAPER.0));
            for y in 0..150 {
                for x in 0..400 {
                    if !(10..330).contains(&x) || !(10..110).contains(&y) {
                        assert_eq!(&canvas.pixels[(y * 400 + x) * 4..][..4], PAPER.0);
                    }
                }
            }
            if let Some(font) = wrapped_font(true) {
                let (size, lines) = fit_lines(value, font, 44.0, 320, 100);
                assert!(
                    lines
                        .iter()
                        .all(|line| truetype_text_width(line, size, font) <= 320)
                );
                assert!(lines.len() as f32 * size * 1.16 <= 100.0);
            }
        }
    }
}
