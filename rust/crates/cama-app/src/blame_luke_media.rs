//! Native one-shot renderer for the Ministry of Accountability animation.
//!
//! This replaces `utils.blame_luke_drawing` without shelling out to Python or
//! discovering host fonts.  A bundled 5x7 glyph set and one animation-wide
//! indexed palette keep the attachment deterministic and comfortably within
//! Discord's upload limits.

use std::borrow::Cow;

use gif::{Encoder, Frame};
use thiserror::Error;

pub const BLAME_LUKE_WIDTH: u16 = 720;
pub const BLAME_LUKE_HEIGHT: u16 = 420;
pub const BLAME_LUKE_FINAL_HOLD_MS: u32 = 8_000;
pub const BLAME_LUKE_REASONS: [&str; 17] = [
    "Luke hosted a lobby while invisible",
    "Luke marked himself ready and vanished",
    "Egg of dog",
    "Luke deleted his messages",
    "Luke pinned his own message and called it evidence.",
    "Luke changed the rules halfway through enforcing them",
    "Luke deleted the evidence and posted a GIF",
    "Luke moved everyone to another channel except himself",
    "Luke made a poll and ignored the result",
    "Luke gave the role to the wrong person",
    "Luke blamed the permissions he configured",
    "Luke posted a message in a read-only channel",
    "Luke abused admin perms to gamba",
    "ash",
    "Luke forgot to update roles",
    "Luke: exists",
    "Luke is still not immortal",
];

pub const BLAME_LUKE_FRAME_COUNT: usize = BLAME_LUKE_REASONS.len() + 4;

const PIXEL_COUNT: usize = BLAME_LUKE_WIDTH as usize * BLAME_LUKE_HEIGHT as usize;
const BACKGROUND: u8 = 0;
const PAPER: u8 = 1;
const INK: u8 = 2;
const MUTED_INK: u8 = 3;
const ACCENT: u8 = 4;
const DANGER: u8 = 5;
const WHITE: u8 = 6;
const SHADOW: u8 = 7;
const PIN_DARK: u8 = 8;
const PIN_LIGHT: u8 = 9;
const PIN_METAL: u8 = 10;
const PAPER_LINE: u8 = 11;
const MUTED_BLUE: u8 = 12;

const PALETTE: &[u8] = &[
    13, 18, 29, // background
    238, 232, 211, // paper
    26, 31, 39, // ink
    92, 94, 91, // muted ink
    245, 187, 54, // accent
    196, 48, 54, // danger
    244, 245, 247, // white
    4, 7, 12, // shadow
    95, 21, 25, // pin dark
    255, 127, 127, // pin highlight
    174, 178, 181, // pin metal
    180, 174, 155, // paper rule
    131, 145, 166, // muted blue
    231, 115, 119, // footer red
    224, 61, 68, // pin red
    255, 255, 255, // palette padding
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlameLukeGif {
    pub bytes: Vec<u8>,
    pub frame_durations_ms: Vec<u32>,
    pub selected_index: usize,
}

#[derive(Debug, Error)]
pub enum BlameLukeRenderError {
    #[error("at least one Blame Luke reason is required")]
    EmptyReasons,
    #[error("selected Blame Luke reason must be present in reasons")]
    SelectedReasonMissing,
    #[error("GIF encoding failed: {0}")]
    Gif(#[from] gif::EncodingError),
}

/// Render the production 17-reason animation for one selected index.
pub fn render_blame_luke(selected_index: usize) -> Result<BlameLukeGif, BlameLukeRenderError> {
    render_blame_luke_from_reasons(&BLAME_LUKE_REASONS, selected_index)
}

/// Equivalent of Python's injectable `reasons=` drawing seam.
pub fn render_blame_luke_reason(
    selected_reason: &str,
    reasons: &[&str],
) -> Result<BlameLukeGif, BlameLukeRenderError> {
    let selected_index = reasons
        .iter()
        .position(|reason| *reason == selected_reason)
        .ok_or(if reasons.is_empty() {
            BlameLukeRenderError::EmptyReasons
        } else {
            BlameLukeRenderError::SelectedReasonMissing
        })?;
    render_blame_luke_from_reasons(reasons, selected_index)
}

fn render_blame_luke_from_reasons(
    reasons: &[&str],
    selected_index: usize,
) -> Result<BlameLukeGif, BlameLukeRenderError> {
    if reasons.is_empty() {
        return Err(BlameLukeRenderError::EmptyReasons);
    }
    if selected_index >= reasons.len() {
        return Err(BlameLukeRenderError::SelectedReasonMissing);
    }

    let order = scan_order(reasons.len(), selected_index);
    let durations = frame_durations_ms(reasons.len());
    let mut bytes = Vec::new();
    {
        let mut encoder = Encoder::new(&mut bytes, BLAME_LUKE_WIDTH, BLAME_LUKE_HEIGHT, PALETTE)?;
        // Intentionally omit `set_repeat`: Pillow's source has no NETSCAPE
        // extension and therefore plays once.
        for (scan_position, reason_index) in order.iter().copied().enumerate() {
            let canvas = draw_frame(reasons[reason_index], reason_index, reasons.len(), false, 0);
            write_frame(&mut encoder, canvas, durations[scan_position])?;
        }
        for (pin_index, progress_percent) in [25, 60, 100].into_iter().enumerate() {
            let canvas = draw_frame(
                reasons[selected_index],
                selected_index,
                reasons.len(),
                false,
                progress_percent,
            );
            write_frame(&mut encoder, canvas, durations[reasons.len() + pin_index])?;
        }
        let canvas = draw_frame(
            reasons[selected_index],
            selected_index,
            reasons.len(),
            true,
            100,
        );
        write_frame(
            &mut encoder,
            canvas,
            *durations.last().expect("final duration exists"),
        )?;
    }
    Ok(BlameLukeGif {
        bytes,
        frame_durations_ms: durations,
        selected_index,
    })
}

fn write_frame(
    encoder: &mut Encoder<&mut Vec<u8>>,
    canvas: Canvas,
    duration_ms: u32,
) -> Result<(), gif::EncodingError> {
    let mut frame = Frame {
        width: BLAME_LUKE_WIDTH,
        height: BLAME_LUKE_HEIGHT,
        delay: u16::try_from(duration_ms / 10).expect("Blame Luke duration fits GIF centiseconds"),
        buffer: Cow::Owned(canvas.pixels),
        ..Frame::default()
    };
    frame.dispose = gif::DisposalMethod::Background;
    encoder.write_frame(&frame)
}

#[must_use]
pub fn frame_durations_ms(reason_count: usize) -> Vec<u32> {
    let mut durations = Vec::with_capacity(reason_count + 4);
    for index in 0..reason_count {
        let source_ms = 170_u32.saturating_sub(u32::try_from(index).unwrap_or(u32::MAX) * 7);
        durations.push((source_ms.max(75) / 10) * 10);
    }
    durations.extend([90, 110, 160, BLAME_LUKE_FINAL_HOLD_MS]);
    durations
}

fn scan_order(reason_count: usize, selected_index: usize) -> Vec<usize> {
    (1..=reason_count)
        .map(|offset| (selected_index + offset) % reason_count)
        .collect()
}

fn draw_frame(
    reason: &str,
    candidate_index: usize,
    candidate_count: usize,
    final_frame: bool,
    pin_progress_percent: i32,
) -> Canvas {
    let mut canvas = Canvas::new();
    canvas.fill_rect(0, 0, 720, 7, ACCENT);
    canvas.text("MINISTRY OF ACCOUNTABILITY", 36, 27, WHITE, 3, true);
    canvas.text(
        "OFFICIAL BLAME ALLOCATION DEPARTMENT",
        36,
        61,
        MUTED_BLUE,
        2,
        false,
    );

    canvas.fill_rect(42, 108, 690, 333, SHADOW);
    canvas.fill_rect(36, 101, 684, 326, PAPER);
    canvas.outline_rect(
        36,
        101,
        684,
        326,
        if final_frame { DANGER } else { ACCENT },
        if final_frame { 5 } else { 3 },
    );
    canvas.text(
        if final_frame {
            "FINAL FINDING"
        } else {
            "SCANNING PLAUSIBLE CAUSE"
        },
        63,
        125,
        if final_frame { DANGER } else { MUTED_INK },
        2,
        true,
    );
    let counter = format!("{:02}/{:02}", candidate_index + 1, candidate_count);
    let counter_x = 655 - text_width(&counter, 2);
    canvas.text(&counter, counter_x, 125, MUTED_INK, 2, true);
    canvas.fill_rect(63, 153, 657, 155, PAPER_LINE);

    let lines = wrapped_lines(reason, 560, 4);
    let block_height = i32::try_from(lines.len()).unwrap_or_default() * 38;
    let mut y = 175 + (120 - block_height).max(0) / 2;
    for line in lines {
        let x = 360 - text_width(&line, 4) / 2;
        canvas.text(&line, x, y, INK, 4, true);
        y += 38;
    }

    if pin_progress_percent > 0 {
        let pin_y = 96 - ((100 - pin_progress_percent) * 100 / 100);
        draw_pin(&mut canvas, 360, pin_y);
    }

    if final_frame {
        draw_stamp(&mut canvas);
        canvas.text_centered(
            "CASE CLOSED | EVIDENCE OPTIONAL | APPEALS DENIED",
            397,
            13,
            1,
            false,
        );
    } else {
        canvas.text_centered(
            "CROSS-REFERENCING CHAT LOGS, VIBES, AND RECENT LUKE ACTIVITY...",
            381,
            MUTED_BLUE,
            1,
            false,
        );
    }
    canvas
}

fn draw_pin(canvas: &mut Canvas, x: i32, y: i32) {
    canvas.circle(x, y + 6, 19, PIN_DARK);
    canvas.circle(x, y, 18, 14);
    canvas.circle(x - 5, y - 5, 7, PIN_LIGHT);
    canvas.fill_triangle((x - 5, y + 17), (x + 5, y + 17), (x, y + 51), PIN_METAL);
}

fn draw_stamp(canvas: &mut Canvas) {
    canvas.outline_rect(108, 329, 612, 389, DANGER, 5);
    canvas.text_centered("OFFICIALLY PINNED ON LUKE", 347, DANGER, 3, true);
}

fn wrapped_lines(text: &str, max_width: i32, scale: i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };
        if !current.is_empty() && text_width(&candidate, scale) > max_width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn text_width(text: &str, scale: i32) -> i32 {
    i32::try_from(text.chars().count())
        .unwrap_or(i32::MAX)
        .saturating_mul(6 * scale)
        .saturating_sub(scale)
}

struct Canvas {
    pixels: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        Self {
            pixels: vec![BACKGROUND; PIXEL_COUNT],
        }
    }

    fn set(&mut self, x: i32, y: i32, color: u8) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x < BLAME_LUKE_WIDTH as usize && y < BLAME_LUKE_HEIGHT as usize {
            self.pixels[y * BLAME_LUKE_WIDTH as usize + x] = color;
        }
    }

    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: u8) {
        for y in top..bottom {
            for x in left..right {
                self.set(x, y, color);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn outline_rect(
        &mut self,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        color: u8,
        width: i32,
    ) {
        self.fill_rect(left, top, right, top + width, color);
        self.fill_rect(left, bottom - width, right, bottom, color);
        self.fill_rect(left, top, left + width, bottom, color);
        self.fill_rect(right - width, top, right, bottom, color);
    }

    fn circle(&mut self, center_x: i32, center_y: i32, radius: i32, color: u8) {
        for y in -radius..=radius {
            for x in -radius..=radius {
                if x * x + y * y <= radius * radius {
                    self.set(center_x + x, center_y + y, color);
                }
            }
        }
    }

    fn fill_triangle(&mut self, a: (i32, i32), b: (i32, i32), c: (i32, i32), color: u8) {
        let min_x = a.0.min(b.0).min(c.0);
        let max_x = a.0.max(b.0).max(c.0);
        let min_y = a.1.min(b.1).min(c.1);
        let max_y = a.1.max(b.1).max(c.1);
        let area = edge(a, b, c);
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let point = (x, y);
                let w0 = edge(b, c, point);
                let w1 = edge(c, a, point);
                let w2 = edge(a, b, point);
                if (area >= 0 && w0 >= 0 && w1 >= 0 && w2 >= 0)
                    || (area < 0 && w0 <= 0 && w1 <= 0 && w2 <= 0)
                {
                    self.set(x, y, color);
                }
            }
        }
    }

    fn text_centered(&mut self, text: &str, y: i32, color: u8, scale: i32, bold: bool) {
        self.text(
            text,
            360 - text_width(text, scale) / 2,
            y,
            color,
            scale,
            bold,
        );
    }

    fn text(&mut self, text: &str, x: i32, y: i32, color: u8, scale: i32, bold: bool) {
        let advance = 6 * scale;
        for (index, character) in text.chars().enumerate() {
            let offset = i32::try_from(index)
                .unwrap_or(i32::MAX)
                .saturating_mul(advance);
            self.glyph(character, x.saturating_add(offset), y, color, scale, bold);
        }
    }

    fn glyph(&mut self, character: char, x: i32, y: i32, color: u8, scale: i32, bold: bool) {
        for (row, bits) in glyph_rows(character).into_iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                let base_x = x + column * scale;
                let base_y = y + i32::try_from(row).unwrap_or_default() * scale;
                for dy in 0..scale {
                    for dx in 0..scale {
                        self.set(base_x + dx, base_y + dy, color);
                    }
                    if bold {
                        self.set(base_x + scale, base_y + dy, color);
                    }
                }
            }
        }
    }
}

fn edge(a: (i32, i32), b: (i32, i32), point: (i32, i32)) -> i32 {
    (point.0 - a.0) * (b.1 - a.1) - (point.1 - a.1) * (b.0 - a.0)
}

#[allow(clippy::too_many_lines)]
fn glyph_rows(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'B' => [0x1e, 0x11, 0x11, 0x1e, 0x11, 0x11, 0x1e],
        'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        'D' => [0x1e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1e],
        'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        'G' => [0x0e, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0f],
        'H' => [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
        'I' => [0x0e, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0e],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x12, 0x12, 0x0c],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1f],
        'M' => [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'P' => [0x1e, 0x11, 0x11, 0x1e, 0x10, 0x10, 0x10],
        'Q' => [0x0e, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0d],
        'R' => [0x1e, 0x11, 0x11, 0x1e, 0x14, 0x12, 0x11],
        'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        'T' => [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0a, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0a],
        'X' => [0x11, 0x11, 0x0a, 0x04, 0x0a, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0a, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1f],
        '0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        '1' => [0x04, 0x0c, 0x14, 0x04, 0x04, 0x04, 0x1f],
        '2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        '3' => [0x1e, 0x01, 0x01, 0x0e, 0x01, 0x01, 0x1e],
        '4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        '5' => [0x1f, 0x10, 0x10, 0x1e, 0x01, 0x01, 0x1e],
        '6' => [0x0e, 0x10, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        '7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        '9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x01, 0x0e],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x0c, 0x0c, 0x08],
        '-' => [0x00, 0x00, 0x00, 0x1f, 0x00, 0x00, 0x00],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        '\'' => [0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        ' ' => [0; 7],
        _ => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io::Cursor;

    use gif::{ColorOutput, DecodeOptions};

    use super::*;

    #[test]
    fn production_gif_scans_all_seventeen_reasons_then_pins_and_holds() {
        let selected_index = 4;
        let asset = render_blame_luke(selected_index).expect("render Blame Luke GIF");
        assert_eq!(asset.selected_index, selected_index);
        assert_eq!(asset.frame_durations_ms.len(), BLAME_LUKE_FRAME_COUNT);
        assert_eq!(
            asset.frame_durations_ms.last(),
            Some(&BLAME_LUKE_FINAL_HOLD_MS)
        );
        assert!(asset.bytes.len() < 8 * 1_024 * 1_024);
        assert!(
            !asset
                .bytes
                .windows(11)
                .any(|window| window == b"NETSCAPE2.0"),
            "one-shot GIF must not contain a loop extension"
        );

        let mut options = DecodeOptions::new();
        options.set_color_output(ColorOutput::Indexed);
        let mut decoder = options
            .read_info(Cursor::new(&asset.bytes))
            .expect("decode native GIF");
        assert_eq!(
            (decoder.width(), decoder.height()),
            (BLAME_LUKE_WIDTH, BLAME_LUKE_HEIGHT)
        );
        assert_eq!(decoder.global_palette(), Some(PALETTE));

        let mut frames = Vec::new();
        let mut decoded_durations = Vec::new();
        while let Some(frame) = decoder.read_next_frame().expect("read native frame") {
            assert!(frame.palette.is_none(), "frames share the global palette");
            frames.push(frame.buffer.to_vec());
            decoded_durations.push(u32::from(frame.delay) * 10);
        }
        assert_eq!(frames.len(), BLAME_LUKE_FRAME_COUNT);
        assert_eq!(decoded_durations, asset.frame_durations_ms);
        assert_eq!(decoded_durations.last(), Some(&8_000));

        let unique_scan_frames = frames[..BLAME_LUKE_REASONS.len()]
            .iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(unique_scan_frames.len(), BLAME_LUKE_REASONS.len());
        assert_ne!(frames[17], frames[18]);
        assert_ne!(frames[18], frames[19]);
        assert_ne!(frames[19], frames[20]);
        assert!(frames[20].iter().filter(|pixel| **pixel == DANGER).count() > 4_000);
    }

    #[test]
    fn scan_starts_after_winner_and_naturally_lands_on_it() {
        let order = scan_order(BLAME_LUKE_REASONS.len(), 4);
        assert_eq!(order.len(), 17);
        assert_eq!(order[0], 5);
        assert_eq!(order[16], 4);
        assert_eq!(order.iter().copied().collect::<BTreeSet<_>>().len(), 17);
    }

    #[test]
    fn injectable_reason_list_rejects_a_reason_outside_the_candidates() {
        let error = render_blame_luke_reason("It was obviously Dave.", &["Luke did it."])
            .expect_err("unknown selected reason must fail");
        assert!(matches!(error, BlameLukeRenderError::SelectedReasonMissing));
    }
}
