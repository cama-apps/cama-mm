//! Native semantic PNG charts for `/ratinganalysis`.

use std::sync::OnceLock;

use fontdue::{Font, FontSettings};

use crate::drawing::{
    DISCORD_ACCENT, DISCORD_BG, DISCORD_DARKER, DISCORD_GREEN, DISCORD_GREY, DISCORD_GRID,
    DISCORD_WHITE, DISCORD_YELLOW, Rgba,
};
use crate::font_assets::{DejaVuFace, load_dejavu_font};
use crate::rating_analysis_command::{CalibrationCurveData, RatingAnalysisDrawingPort};
use crate::rating_comparison_service::RatingComparisonResult;

// Matplotlib's production `draw_rating_comparison_chart` uses a tight
// bounding box around a 10x4-inch figure.  The resulting attachment geometry
// for the fixed comparison fixture is 989x413; keep the native renderer on
// that same Discord attachment contract.
pub const COMPARISON_WIDTH: usize = 989;
pub const COMPARISON_HEIGHT: usize = 413;
// Matplotlib's tight bounding box trims the live attachments to these
// dimensions for the fixed visual-equivalence inputs.
pub const CALIBRATION_WIDTH: usize = 640;
pub const CALIBRATION_HEIGHT: usize = 490;
pub const TREND_WIDTH: usize = 789;
pub const TREND_HEIGHT: usize = 390;

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeRatingAnalysisDrawing;

impl RatingAnalysisDrawingPort for NativeRatingAnalysisDrawing {
    fn rating_comparison_chart(
        &mut self,
        comparison: &RatingComparisonResult,
    ) -> Result<Vec<u8>, String> {
        Ok(draw_rating_comparison_chart(comparison))
    }

    fn calibration_curve_chart(
        &mut self,
        curves: &CalibrationCurveData,
    ) -> Result<Vec<u8>, String> {
        Ok(draw_calibration_curve(curves))
    }

    fn prediction_over_time_chart(
        &mut self,
        comparison: &RatingComparisonResult,
        window: usize,
    ) -> Result<Vec<u8>, String> {
        draw_prediction_over_time(comparison, window)
    }
}

#[must_use]
pub fn draw_rating_comparison_chart(comparison: &RatingComparisonResult) -> Vec<u8> {
    let mut canvas = Canvas::new(COMPARISON_WIDTH, COMPARISON_HEIGHT, DISCORD_BG);
    canvas.text_centered(
        &format!(
            "RATING SYSTEM COMPARISON ({} MATCHES)",
            comparison.matches_analyzed
        ),
        18,
        DISCORD_WHITE,
        2,
    );
    let metrics = [
        (
            "BRIER SCORE",
            comparison.glicko.brier_score,
            comparison.openskill.brier_score,
            true,
        ),
        (
            "ACCURACY",
            comparison.glicko.accuracy,
            comparison.openskill.accuracy,
            false,
        ),
        (
            "LOG LOSS",
            comparison.glicko.log_loss,
            comparison.openskill.log_loss,
            true,
        ),
    ];
    for (index, (label, glicko, openskill, lower_is_better)) in metrics.into_iter().enumerate() {
        // These are the three axes produced by the live Python chart after
        // its tight layout: 275x275 panels with 48px gaps.
        let left = 58 + i32::try_from(index).unwrap_or_default() * 323;
        let right = left + 275;
        let top = 104;
        let baseline = 379;
        canvas.fill_rect(left, top, right, baseline, DISCORD_DARKER);
        canvas.text_centered_in(label, left, right, 67, DISCORD_WHITE, 2);
        let subtitle = if lower_is_better {
            "(LOWER = BETTER)"
        } else {
            "(HIGHER = BETTER)"
        };
        canvas.text_centered_in(subtitle, left, right, 82, DISCORD_WHITE, 1);
        canvas.line((left, baseline), (right, baseline), DISCORD_GRID, 1);
        // Matplotlib's y-axis leaves only a small automatic headroom above
        // the tallest bar.  The fixed native plot uses the same 264px data
        // span and lets the maximum value reach that span.
        let maximum = glicko.max(openskill).max(0.001);
        let g_height = (264.0 * (glicko / maximum).clamp(0.0, 1.0)).round() as i32;
        let o_height = (264.0 * (openskill / maximum).clamp(0.0, 1.0)).round() as i32;
        canvas.fill_rect(
            left + 11,
            baseline - g_height,
            left + 124,
            baseline,
            DISCORD_ACCENT,
        );
        canvas.fill_rect(
            left + 151,
            baseline - o_height,
            left + 264,
            baseline,
            DISCORD_GREEN,
        );
        let glicko_wins = if lower_is_better {
            glicko < openskill
        } else {
            glicko > openskill
        };
        let winner = if glicko_wins {
            (left + 9, baseline - g_height - 2, left + 126, baseline + 2)
        } else {
            (
                left + 149,
                baseline - o_height - 2,
                left + 266,
                baseline + 2,
            )
        };
        canvas.outline_rect(winner.0, winner.1, winner.2, winner.3, DISCORD_YELLOW, 2);
        canvas.text_centered_in("GLICKO-2", left + 11, left + 124, 389, DISCORD_GREY, 1);
        canvas.text_centered_in("OPENSKILL", left + 151, left + 264, 389, DISCORD_GREY, 1);
        canvas.text_centered_in(
            &format!("{glicko:.3}"),
            left + 11,
            left + 124,
            baseline - g_height - 17,
            DISCORD_WHITE,
            1,
        );
        canvas.text_centered_in(
            &format!("{openskill:.3}"),
            left + 151,
            left + 264,
            baseline - o_height - 17,
            DISCORD_WHITE,
            1,
        );
    }
    encode_png(&canvas)
}

#[must_use]
pub fn draw_calibration_curve(curves: &CalibrationCurveData) -> Vec<u8> {
    let mut canvas = Canvas::new(CALIBRATION_WIDTH, CALIBRATION_HEIGHT, DISCORD_BG);
    canvas.text_centered(
        "CALIBRATION CURVE: PREDICTED VS ACTUAL",
        16,
        DISCORD_WHITE,
        2,
    );
    let plot = Plot::new((65, 55, 620, 435), PlotBounds::unit());
    canvas.fill_rect(plot.left, plot.top, plot.right, plot.bottom, DISCORD_DARKER);
    draw_grid(&mut canvas, &plot, 5);
    for step in 0..20 {
        let first = f64::from(step) / 20.0;
        let second = f64::from(step + 1) / 20.0;
        if step % 2 == 0 {
            canvas.line(
                plot.point(first, first),
                plot.point(second, second),
                DISCORD_GREY,
                1,
            );
        }
    }
    draw_curve(&mut canvas, &plot, &curves.glicko, DISCORD_ACCENT, false);
    draw_curve(&mut canvas, &plot, &curves.openskill, DISCORD_GREEN, true);
    canvas.text(70, 455, "PREDICTED WIN PROBABILITY", DISCORD_GREY, 1);
    canvas.text(405, 455, "GLICKO", DISCORD_ACCENT, 1);
    canvas.text(505, 455, "OPENSKILL", DISCORD_GREEN, 1);
    encode_png(&canvas)
}

pub fn draw_prediction_over_time(
    comparison: &RatingComparisonResult,
    window: usize,
) -> Result<Vec<u8>, String> {
    if window == 0 || comparison.match_data.len() < window {
        return Err(format!("Need at least {window} matches for trend analysis"));
    }
    let mut glicko = Vec::new();
    let mut openskill = Vec::new();
    for end in window..=comparison.match_data.len() {
        let rows = &comparison.match_data[end - window..end];
        glicko.push(rows.iter().filter(|row| row.glicko_correct).count() as f64 / window as f64);
        openskill
            .push(rows.iter().filter(|row| row.openskill_correct).count() as f64 / window as f64);
    }
    let mut canvas = Canvas::new(TREND_WIDTH, TREND_HEIGHT, DISCORD_BG);
    canvas.text_centered("PREDICTION ACCURACY OVER TIME", 16, DISCORD_WHITE, 2);
    let plot = Plot::new(
        (65, 55, 770, 335),
        PlotBounds {
            x_min: 0.0,
            x_max: (glicko.len() - 1).max(1) as f64,
            y_min: 0.3,
            y_max: 0.9,
        },
    );
    canvas.fill_rect(plot.left, plot.top, plot.right, plot.bottom, DISCORD_DARKER);
    draw_grid(&mut canvas, &plot, 6);
    for index in 0..10 {
        if index % 2 == 0 {
            let x1 = plot.left + (plot.right - plot.left) * index / 10;
            let x2 = plot.left + (plot.right - plot.left) * (index + 1) / 10;
            canvas.line((x1, plot.y(0.5)), (x2, plot.y(0.5)), DISCORD_GREY, 1);
        }
    }
    draw_series(&mut canvas, &plot, &glicko, DISCORD_ACCENT);
    draw_series(&mut canvas, &plot, &openskill, DISCORD_GREEN);
    canvas.text(70, 355, "MATCH NUMBER", DISCORD_GREY, 1);
    canvas.text(490, 355, "GLICKO", DISCORD_ACCENT, 1);
    canvas.text(590, 355, "OPENSKILL", DISCORD_GREEN, 1);
    Ok(encode_png(&canvas))
}

fn draw_curve(
    canvas: &mut Canvas,
    plot: &Plot,
    points: &[crate::rating_analysis_command::CalibrationPoint],
    color: Rgba,
    squares: bool,
) {
    for pair in points.windows(2) {
        canvas.line(
            plot.point(pair[0].predicted, pair[0].actual_rate),
            plot.point(pair[1].predicted, pair[1].actual_rate),
            color,
            2,
        );
    }
    for point in points {
        let center = plot.point(point.predicted, point.actual_rate);
        let radius = (3.0 + (point.count as f64).sqrt()).min(10.0).round() as i32;
        if squares {
            canvas.fill_rect(
                center.0 - radius,
                center.1 - radius,
                center.0 + radius + 1,
                center.1 + radius + 1,
                color,
            );
        } else {
            canvas.circle(center, radius, color);
        }
    }
}

fn draw_series(canvas: &mut Canvas, plot: &Plot, values: &[f64], color: Rgba) {
    for (index, pair) in values.windows(2).enumerate() {
        canvas.line(
            plot.point(index as f64, pair[0]),
            plot.point((index + 1) as f64, pair[1]),
            color,
            2,
        );
    }
    if values.len() == 1 {
        canvas.circle(plot.point(0.0, values[0]), 3, color);
    }
}

fn draw_grid(canvas: &mut Canvas, plot: &Plot, divisions: i32) {
    for step in 0..=divisions {
        let x = plot.left + (plot.right - plot.left) * step / divisions;
        let y = plot.top + (plot.bottom - plot.top) * step / divisions;
        canvas.line((x, plot.top), (x, plot.bottom), DISCORD_GRID, 1);
        canvas.line((plot.left, y), (plot.right, y), DISCORD_GRID, 1);
    }
}

#[derive(Clone, Copy)]
struct Plot {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
}

#[derive(Clone, Copy)]
struct PlotBounds {
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
}

impl PlotBounds {
    const fn unit() -> Self {
        Self {
            x_min: 0.0,
            x_max: 1.0,
            y_min: 0.0,
            y_max: 1.0,
        }
    }
}

impl Plot {
    const fn new((left, top, right, bottom): (i32, i32, i32, i32), bounds: PlotBounds) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
            x_min: bounds.x_min,
            x_max: bounds.x_max,
            y_min: bounds.y_min,
            y_max: bounds.y_max,
        }
    }

    fn point(&self, x: f64, y: f64) -> (i32, i32) {
        (self.x(x), self.y(y))
    }

    fn x(&self, value: f64) -> i32 {
        self.left
            + (((value - self.x_min) / (self.x_max - self.x_min)).clamp(0.0, 1.0)
                * f64::from(self.right - self.left)) as i32
    }

    fn y(&self, value: f64) -> i32 {
        self.bottom
            - (((value - self.y_min) / (self.y_max - self.y_min)).clamp(0.0, 1.0)
                * f64::from(self.bottom - self.top)) as i32
    }
}

struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: usize, height: usize, color: Rgba) -> Self {
        let mut pixels = vec![0; width * height * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color.0);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    fn set(&mut self, x: i32, y: i32, color: Rgba) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x < self.width && y < self.height {
            let offset = (y * self.width + x) * 4;
            if color.0[3] == u8::MAX {
                self.pixels[offset..offset + 4].copy_from_slice(&color.0);
                return;
            }
            let alpha = u16::from(color.0[3]);
            let inverse = u16::from(u8::MAX) - alpha;
            for channel in 0..3 {
                self.pixels[offset + channel] = u8::try_from(
                    (u16::from(color.0[channel]) * alpha
                        + u16::from(self.pixels[offset + channel]) * inverse
                        + 127)
                        / 255,
                )
                .unwrap_or(u8::MAX);
            }
            self.pixels[offset + 3] = u8::MAX;
        }
    }

    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        for y in top.min(bottom)..top.max(bottom) {
            for x in left.min(right)..left.max(right) {
                self.set(x, y, color);
            }
        }
    }

    fn outline_rect(
        &mut self,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        color: Rgba,
        width: i32,
    ) {
        for offset in 0..width {
            self.line(
                (left - offset, top - offset),
                (right + offset, top - offset),
                color,
                1,
            );
            self.line((right + offset, top), (right + offset, bottom), color, 1);
            self.line((right, bottom + offset), (left, bottom + offset), color, 1);
            self.line((left - offset, bottom), (left - offset, top), color, 1);
        }
    }

    fn line(&mut self, start: (i32, i32), end: (i32, i32), color: Rgba, width: i32) {
        let (mut x0, mut y0) = start;
        let (x1, y1) = end;
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;
        let radius = width.saturating_sub(1) / 2;
        loop {
            for oy in -radius..=radius {
                for ox in -radius..=radius {
                    self.set(x0 + ox, y0 + oy, color);
                }
            }
            if x0 == x1 && y0 == y1 {
                break;
            }
            let twice = error * 2;
            if twice >= dy {
                error += dy;
                x0 += sx;
            }
            if twice <= dx {
                error += dx;
                y0 += sy;
            }
        }
    }

    fn circle(&mut self, center: (i32, i32), radius: i32, color: Rgba) {
        for y in -radius..=radius {
            for x in -radius..=radius {
                if x * x + y * y <= radius * radius {
                    self.set(center.0 + x, center.1 + y, color);
                }
            }
        }
    }

    fn text(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        if let Some(font) = rating_font(scale >= 2) {
            self.truetype_text(x, y, text, color, rating_font_size(scale), font);
            return;
        }
        for (index, character) in text.chars().enumerate() {
            let x = x + i32::try_from(index).unwrap_or_default() * 6 * scale;
            for (row, bits) in glyph(character).into_iter().enumerate() {
                for column in 0..5_u8 {
                    if bits & (1 << (4 - column)) != 0 {
                        let left = x + i32::from(column) * scale;
                        let top = y + i32::try_from(row).unwrap_or_default() * scale;
                        self.fill_rect(left, top, left + scale, top + scale, color);
                    }
                }
            }
        }
    }

    fn text_centered(&mut self, text: &str, y: i32, color: Rgba, scale: i32) {
        self.text_centered_in(text, 0, self.width as i32, y, color, scale);
    }

    fn text_centered_in(
        &mut self,
        text: &str,
        left: i32,
        right: i32,
        y: i32,
        color: Rgba,
        scale: i32,
    ) {
        let width = rating_font(scale >= 2).map_or_else(
            || i32::try_from(text.chars().count()).unwrap_or_default() * 6 * scale,
            |font| truetype_text_width(text, rating_font_size(scale), font),
        );
        self.text(left + (right - left - width) / 2, y, text, color, scale);
    }

    fn truetype_text(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: Rgba,
        pixel_size: f32,
        font: &Font,
    ) {
        let ascent = font
            .horizontal_line_metrics(pixel_size)
            .map_or(pixel_size, |metrics| metrics.ascent);
        let baseline = y as f32 + ascent;
        let mut pen_x = x as f32;
        let mut previous = None;
        for character in text.chars() {
            if let Some(previous) = previous {
                pen_x += font
                    .horizontal_kern(previous, character, pixel_size)
                    .unwrap_or_default();
            }
            let (metrics, bitmap) = font.rasterize(character, pixel_size);
            let origin_x = pen_x.round() as i32 + metrics.xmin;
            let origin_y = baseline.round() as i32
                - metrics.ymin
                - i32::try_from(metrics.height).unwrap_or_default();
            for glyph_y in 0..metrics.height {
                for glyph_x in 0..metrics.width {
                    let alpha = bitmap[glyph_y * metrics.width + glyph_x];
                    if alpha == 0 {
                        continue;
                    }
                    self.set(
                        origin_x + i32::try_from(glyph_x).unwrap_or_default(),
                        origin_y + i32::try_from(glyph_y).unwrap_or_default(),
                        Rgba([color.0[0], color.0[1], color.0[2], alpha]),
                    );
                }
            }
            pen_x += metrics.advance_width;
            previous = Some(character);
        }
    }
}

fn rating_font(bold: bool) -> Option<&'static Font> {
    static REGULAR: OnceLock<Option<Font>> = OnceLock::new();
    static BOLD: OnceLock<Option<Font>> = OnceLock::new();
    let slot = if bold { &BOLD } else { &REGULAR };
    slot.get_or_init(|| {
        load_dejavu_font(if bold {
            DejaVuFace::SansBold
        } else {
            DejaVuFace::Sans
        })
        .ok()
        .and_then(|bytes| Font::from_bytes(bytes, FontSettings::default()).ok())
    })
    .as_ref()
}

fn rating_font_size(scale: i32) -> f32 {
    if scale >= 2 { 18.0 } else { 10.0 }
}

fn truetype_text_width(text: &str, pixel_size: f32, font: &Font) -> i32 {
    let mut width = 0.0_f32;
    let mut previous = None;
    for character in text.chars() {
        if let Some(previous) = previous {
            width += font
                .horizontal_kern(previous, character, pixel_size)
                .unwrap_or_default();
        }
        width += font.metrics(character, pixel_size).advance_width;
        previous = Some(character);
    }
    width.ceil() as i32
}

fn glyph(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [31, 4, 4, 4, 4, 4, 31],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 16, 30, 1, 1, 30],
        '6' => [14, 16, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 1, 14],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '%' => [25, 25, 2, 4, 8, 19, 19],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        ' ' => [0; 7],
        _ => [14, 17, 1, 2, 4, 0, 4],
    }
}

fn encode_png(canvas: &Canvas) -> Vec<u8> {
    let mut filtered = Vec::with_capacity((canvas.width * 4 + 1) * canvas.height);
    for row in canvas.pixels.chunks_exact(canvas.width * 4) {
        filtered.push(0);
        filtered.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks = filtered.chunks(65_535).collect::<Vec<_>>();
    for (index, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(index + 1 == blocks.len()));
        let length = u16::try_from(block.len()).unwrap_or(u16::MAX);
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&filtered).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&(canvas.width as u32).to_be_bytes());
    header.extend_from_slice(&(canvas.height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_chunk(&mut png, b"IHDR", &header);
    append_chunk(&mut png, b"IDAT", &zlib);
    append_chunk(&mut png, b"IEND", &[]);
    png
}

fn append_chunk(png: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    png.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(payload);
    let mut crc_data = kind.to_vec();
    crc_data.extend_from_slice(payload);
    png.extend_from_slice(&crc32(&crc_data).to_be_bytes());
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut first, mut second) = (1_u32, 0_u32);
    for byte in bytes {
        first = (first + u32::from(*byte)) % 65_521;
        second = (second + first) % 65_521;
    }
    (second << 16) | first
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320_u32 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[cfg(test)]
mod tests;
