//! One card at one instant: the palette, the type engine, and the timeline.
//!
//! Layouts ask the stage for eased progress instead of tracking frames, so
//! `t = 1.0` is by construction the finished still card.

use std::collections::HashMap;
use std::rc::Rc;

use fontdue::Metrics;

use super::*;

pub(super) const WIDTH: i32 = WRAPPED_SLIDE_WIDTH as i32;
pub(super) const HEIGHT: i32 = WRAPPED_SLIDE_HEIGHT as i32;

const POPPINS_BLACK: &[u8] =
    include_bytes!("../../../../../assets/wrapped/fonts/Poppins-Black.ttf");
const POPPINS_BOLD: &[u8] = include_bytes!("../../../../../assets/wrapped/fonts/Poppins-Bold.ttf");
const POPPINS_MEDIUM: &[u8] =
    include_bytes!("../../../../../assets/wrapped/fonts/Poppins-Medium.ttf");

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Face {
    Black,
    Bold,
    Medium,
}

impl Face {
    fn font(self) -> &'static Font {
        fn parse(bytes: &'static [u8]) -> Font {
            Font::from_bytes(bytes, FontSettings::default()).expect("bundled Poppins parses")
        }
        static BLACK: OnceLock<Font> = OnceLock::new();
        static BOLD: OnceLock<Font> = OnceLock::new();
        static MEDIUM: OnceLock<Font> = OnceLock::new();
        match self {
            Self::Black => BLACK.get_or_init(|| parse(POPPINS_BLACK)),
            Self::Bold => BOLD.get_or_init(|| parse(POPPINS_BOLD)),
            Self::Medium => MEDIUM.get_or_init(|| parse(POPPINS_MEDIUM)),
        }
    }

    /// Display sizes are set tight; reading sizes get room to breathe.
    const fn leading(self) -> f32 {
        match self {
            Self::Black => 1.02,
            Self::Bold => 1.12,
            Self::Medium => 1.22,
        }
    }

    const fn tracking(self) -> f32 {
        match self {
            Self::Black => -0.025,
            Self::Bold => -0.01,
            Self::Medium => 0.0,
        }
    }

    /// Names may use scripts Poppins lacks; DejaVu covers most of them when the
    /// runtime image provides it.
    fn font_for(self, character: char) -> &'static Font {
        let primary = self.font();
        if primary.lookup_glyph_index(character) != 0 {
            return primary;
        }
        crate::font_assets::dejavu_font(crate::font_assets::DejaVuFace::SansBold)
            .filter(|font| font.lookup_glyph_index(character) != 0)
            .unwrap_or(primary)
    }
}

pub(super) fn text_width(value: &str, size: f32, face: Face) -> f32 {
    let tracking = face.tracking() * size;
    value
        .chars()
        .map(|character| {
            face.font_for(character)
                .metrics(character, size)
                .advance_width
                + tracking
        })
        .sum()
}

const FIRST_BASELINE: f32 = 0.9;
const DESCENT: f32 = 0.27;

fn block_height(rows: usize, size: f32, face: Face) -> f32 {
    if rows == 0 {
        return 0.0;
    }
    size * (FIRST_BASELINE + DESCENT + (rows - 1) as f32 * face.leading())
}

/// Wrap on measured glyph widths, shrinking until the block fits its box. Only
/// extreme input reaches the visible ellipsis.
pub(super) fn fit_text(
    value: &str,
    face: Face,
    maximum: f32,
    width: i32,
    height: i32,
) -> (f32, Vec<String>) {
    const SMALLEST: f32 = 14.0;
    let width = width as f32;
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
                if text_width(&joined, size, face) <= width {
                    line = joined;
                    continue;
                }
                if !line.is_empty() {
                    rows.push(std::mem::take(&mut line));
                }
                for character in word.chars() {
                    let mut next = line.clone();
                    next.push(character);
                    if !line.is_empty() && text_width(&next, size, face) > width {
                        rows.push(std::mem::take(&mut line));
                    }
                    line.push(character);
                }
            }
            if !line.is_empty() {
                rows.push(line);
            }
        }
        if block_height(rows.len(), size, face) <= height as f32 {
            return (size, rows);
        }
        if size <= SMALLEST {
            let mut kept = rows.len();
            while kept > 1 && block_height(kept, size, face) > height as f32 {
                kept -= 1;
            }
            rows.truncate(kept);
            if let Some(last) = rows.last_mut() {
                while !last.is_empty() && text_width(&format!("{last}…"), size, face) > width {
                    last.pop();
                }
                last.push('…');
            }
            return (size, rows);
        }
        size = (size - if size > 40.0 { 2.0 } else { 1.0 }).max(SMALLEST);
    }
}

/// Spin every digit run in `value` up from zero, leaving its words in place.
pub(super) fn count_up(value: &str, progress: f32) -> String {
    if progress >= 1.0 {
        return value.to_owned();
    }
    let chars = value.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len()
            && (chars[index].is_ascii_digit()
                || (chars[index] == ',' && chars.get(index + 1).is_some_and(char::is_ascii_digit)))
        {
            index += 1;
        }
        let run = chars[start..index].iter().collect::<String>();
        let digits = run.replace(',', "");
        match digits.parse::<i64>() {
            Ok(number) => {
                let current = (number as f64 * f64::from(progress.max(0.0))) as i64;
                if run.contains(',') || digits.len() >= 4 {
                    output.push_str(&cama_domain::formatting::format_grouped(current));
                } else {
                    output.push_str(&current.to_string());
                }
            }
            Err(_) => output.push_str(&run),
        }
    }
    output
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Palette {
    pub(super) bg: Rgba,
    pub(super) fg: Rgba,
    pub(super) accent: Rgba,
    /// A quiet relative of the background for moving shapes behind the type.
    pub(super) soft: Rgba,
    /// Readable text on an `accent` fill.
    pub(super) on_accent: Rgba,
}

const CREAM: Rgba = Rgba::rgb(255, 245, 225);
const INK: Rgba = Rgba::rgb(17, 16, 28);
const LIME: Rgba = Rgba::rgb(198, 244, 50);
const YELLOW: Rgba = Rgba::rgb(255, 210, 30);
const RED: Rgba = Rgba::rgb(242, 51, 75);
const ORANGE: Rgba = Rgba::rgb(255, 122, 26);
const VIOLET: Rgba = Rgba::rgb(92, 33, 240);
const COBALT: Rgba = Rgba::rgb(34, 40, 220);
const PLUM: Rgba = Rgba::rgb(60, 12, 80);
const AQUA: Rgba = Rgba::rgb(18, 216, 196);
const RADIANT: Rgba = Rgba::rgb(46, 184, 92);

pub(super) fn palette(theme: WrappedTheme) -> Palette {
    let (bg, fg, accent, soft, on_accent) = match theme {
        WrappedTheme::Plain => (CREAM, INK, VIOLET, CREAM, CREAM),
        WrappedTheme::Cover => (VIOLET, CREAM, LIME, Rgba::rgb(118, 66, 250), INK),
        WrappedTheme::Queue => (COBALT, CREAM, LIME, Rgba::rgb(60, 70, 240), INK),
        WrappedTheme::QueueAlt => (LIME, INK, COBALT, Rgba::rgb(174, 224, 34), CREAM),
        WrappedTheme::Heroes => (
            Rgba::rgb(255, 77, 157),
            INK,
            PLUM,
            Rgba::rgb(255, 118, 182),
            CREAM,
        ),
        WrappedTheme::Months => (ORANGE, INK, PLUM, Rgba::rgb(255, 148, 66), CREAM),
        WrappedTheme::Squad => (RADIANT, INK, PLUM, Rgba::rgb(74, 206, 116), CREAM),
        WrappedTheme::Rivals => (
            Rgba::rgb(196, 20, 52),
            CREAM,
            YELLOW,
            Rgba::rgb(170, 14, 44),
            INK,
        ),
        WrappedTheme::Rating => (
            Rgba::rgb(16, 18, 46),
            CREAM,
            AQUA,
            Rgba::rgb(30, 34, 76),
            INK,
        ),
        WrappedTheme::GambaUp => (
            Rgba::rgb(8, 92, 64),
            CREAM,
            YELLOW,
            Rgba::rgb(14, 116, 82),
            INK,
        ),
        WrappedTheme::GambaDown => (
            Rgba::rgb(132, 14, 38),
            CREAM,
            YELLOW,
            Rgba::rgb(164, 28, 54),
            INK,
        ),
        WrappedTheme::Dig => (
            Rgba::rgb(62, 34, 18),
            CREAM,
            YELLOW,
            Rgba::rgb(86, 48, 26),
            INK,
        ),
        WrappedTheme::DigDeep => (
            Rgba::rgb(24, 10, 36),
            CREAM,
            ORANGE,
            Rgba::rgb(58, 20, 66),
            INK,
        ),
        WrappedTheme::Pet => (
            Rgba::rgb(255, 176, 210),
            PLUM,
            Rgba::rgb(176, 16, 96),
            Rgba::rgb(255, 200, 224),
            CREAM,
        ),
        WrappedTheme::PetGrave => (
            Rgba::rgb(44, 42, 72),
            CREAM,
            Rgba::rgb(176, 166, 255),
            Rgba::rgb(60, 58, 94),
            INK,
        ),
        WrappedTheme::Mafia => (
            Rgba::rgb(13, 12, 24),
            CREAM,
            RED,
            Rgba::rgb(36, 34, 58),
            INK,
        ),
        WrappedTheme::MafiaRed => (RED, INK, CREAM, Rgba::rgb(212, 32, 60), INK),
        WrappedTheme::Persona => (YELLOW, INK, VIOLET, Rgba::rgb(255, 226, 92), CREAM),
        WrappedTheme::Recap => (INK, CREAM, LIME, Rgba::rgb(36, 34, 54), INK),
    };
    Palette {
        bg,
        fg,
        accent,
        soft,
        on_accent,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Align {
    Left,
    Center,
}

/// Bundled art resampled once, so each frame only has to copy it.
pub(super) struct PreparedArt {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) pixels: Vec<u8>,
}

/// A rasterized glyph, shared between frames.
type Glyph = Rc<(Metrics, Vec<u8>)>;
/// Text wrapped to a box: the size it fit at and its lines.
type Fit = (f32, Rc<Vec<String>>);

pub(super) struct Stage<'a> {
    pub(super) canvas: Canvas,
    pub(super) slide: &'a WrappedSlideData,
    pub(super) palette: Palette,
    pub(super) t: f32,
    pub(super) art: Option<PreparedArt>,
    /// Pictures for ranked rows, by row index.
    pub(super) row_art: Vec<Option<PreparedArt>>,
    pub(super) avatars: Vec<Option<DecodedPng>>,
    glyphs: HashMap<(Face, char, u32), Glyph>,
    fits: HashMap<(String, Face, u32, i32, i32), Fit>,
}

impl<'a> Stage<'a> {
    pub(super) fn new(slide: &'a WrappedSlideData) -> Self {
        let palette = palette(slide.theme);
        Self {
            canvas: Canvas::new(WRAPPED_SLIDE_WIDTH, WRAPPED_SLIDE_HEIGHT, palette.bg),
            slide,
            palette,
            t: 1.0,
            art: artwork::prepare(slide),
            row_art: artwork::prepare_rows(slide),
            avatars: slide
                .avatars
                .iter()
                .map(|avatar| decode_png(&avatar.png))
                .collect(),
            glyphs: HashMap::new(),
            fits: HashMap::new(),
        }
    }

    pub(super) fn paint(&mut self, t: f32) -> &Canvas {
        self.t = t.clamp(0.0, 1.0);
        self.canvas.clear(self.palette.bg);
        motifs::paint(self);
        cards::paint(self);
        &self.canvas
    }

    fn progress(&self, start: f32, end: f32) -> f32 {
        if self.t >= 1.0 {
            return 1.0;
        }
        ((self.t - start) / (end - start).max(0.001)).clamp(0.0, 1.0)
    }

    /// Eased progress through a window of the timeline.
    pub(super) fn ease(&self, start: f32, end: f32) -> f32 {
        1.0 - (1.0 - self.progress(start, end)).powi(3)
    }

    /// Like `ease`, overshooting slightly before it settles.
    pub(super) fn pop(&self, start: f32, end: f32) -> f32 {
        let p = self.progress(start, end) - 1.0;
        1.0 + 2.7 * p.powi(3) + 1.7 * p.powi(2)
    }

    fn glyph(&mut self, face: Face, character: char, size: f32) -> Glyph {
        self.glyphs
            .entry((face, character, (size * 4.0) as u32))
            .or_insert_with(|| Rc::new(face.font_for(character).rasterize(character, size)))
            .clone()
    }

    fn fit(&mut self, value: &str, face: Face, size: f32, width: i32, height: i32) -> Fit {
        let key = (value.to_owned(), face, (size * 4.0) as u32, width, height);
        self.fits
            .entry(key)
            .or_insert_with(|| {
                let (size, rows) = fit_text(value, face, size, width, height);
                (size, Rc::new(rows))
            })
            .clone()
    }

    /// Draw one line with its baseline at `y`, clipped to `clip` rows.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn line(
        &mut self,
        value: &str,
        x: f32,
        y: f32,
        size: f32,
        face: Face,
        color: Rgba,
        clip: (i32, i32),
    ) {
        let tracking = face.tracking() * size;
        let baseline = y.round() as i32;
        let mut pen = x;
        for character in value.chars() {
            let glyph = self.glyph(face, character, size);
            let (metrics, bitmap) = (&glyph.0, &glyph.1);
            let left = pen.round() as i32 + metrics.xmin;
            let top = baseline - metrics.ymin - metrics.height as i32;
            let from_row = (clip.0 - top).max(0) as usize;
            let to_row = ((clip.1 - top).max(0) as usize).min(metrics.height);
            for row in from_row..to_row {
                for column in 0..metrics.width {
                    let alpha = bitmap[row * metrics.width + column];
                    if alpha > 0 {
                        self.canvas.set(
                            left + column as i32,
                            top + row as i32,
                            color.scaled_alpha(f32::from(alpha) / 255.0),
                        );
                    }
                }
            }
            pen += metrics.advance_width + tracking;
        }
    }

    /// Wrapped text fitted to `rect` that rises line by line from behind a mask
    /// during `rise`. Returns the bottom of the finished block, so a layout can
    /// flow around however the copy happened to wrap.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn block(
        &mut self,
        value: &str,
        rect: (i32, i32, i32, i32),
        size: f32,
        face: Face,
        color: Rgba,
        align: Align,
        rise: (f32, f32),
    ) -> i32 {
        if value.trim().is_empty() || rect.3 <= 0 {
            return rect.1;
        }
        let (size, rows) = self.fit(value, face, size, rect.2, rect.3);
        let advance = size * face.leading();
        let bottom = rect.1 + rect.3;
        let top = rect.1 as f32;
        for (index, row) in rows.iter().enumerate() {
            let stagger = index as f32 * 0.05;
            let shown = self.ease(rise.0 + stagger, rise.1 + stagger);
            if shown <= 0.0 {
                continue;
            }
            let baseline = top + size * FIRST_BASELINE + index as f32 * advance;
            let clip_top = (baseline - size * FIRST_BASELINE).floor() as i32;
            let clip_bottom = ((baseline + size * DESCENT).ceil() as i32).min(bottom);
            let x = match align {
                Align::Left => rect.0 as f32,
                Align::Center => {
                    rect.0 as f32 + (rect.2 as f32 - text_width(row, size, face)).max(0.0) / 2.0
                }
            };
            let travel = (1.0 - shown) * size * 1.2;
            self.line(
                row,
                x,
                baseline + travel,
                size,
                face,
                color,
                (clip_top.max(rect.1), clip_bottom),
            );
        }
        rect.1 + block_height(rows.len(), size, face).ceil() as i32
    }

    /// One oversized line sized for its final value, so a counter never makes
    /// the type jump while it spins.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn stat(
        &mut self,
        final_value: &str,
        shown: &str,
        rect: (i32, i32, i32, i32),
        maximum: f32,
        color: Rgba,
        align: Align,
        rise: (f32, f32),
    ) -> i32 {
        if final_value.trim().is_empty() {
            return rect.1;
        }
        let width = text_width(final_value, maximum, Face::Black);
        let fitted = if width > rect.2 as f32 {
            maximum * rect.2 as f32 / width
        } else {
            maximum
        };
        let size = fitted.min(rect.3 as f32 / (FIRST_BASELINE + DESCENT));
        let baseline = rect.1 as f32 + size * FIRST_BASELINE;
        let x = match align {
            Align::Left => rect.0 as f32,
            Align::Center => {
                rect.0 as f32
                    + (rect.2 as f32 - text_width(final_value, size, Face::Black)).max(0.0) / 2.0
            }
        };
        let shown_amount = self.ease(rise.0, rise.1);
        if shown_amount > 0.0 {
            let clip = (rect.1, (baseline + size * DESCENT).ceil() as i32);
            self.line(
                shown,
                x,
                baseline + (1.0 - shown_amount) * size,
                size,
                Face::Black,
                color,
                clip,
            );
        }
        rect.1 + (size * (FIRST_BASELINE + DESCENT)).ceil() as i32
    }

    /// A rounded label that pops in. Returns its bottom.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pill(
        &mut self,
        value: &str,
        x: i32,
        y: i32,
        maximum_width: i32,
        size: f32,
        fill: Rgba,
        ink: Rgba,
        start: f32,
    ) -> i32 {
        if value.trim().is_empty() {
            return y;
        }
        let padding = size * 0.75;
        let size = pill_size(value, maximum_width, size);
        let width = text_width(value, size, Face::Bold) + padding * 2.0;
        let height = size * 1.9;
        let scale = self.pop(start, start + 0.2);
        if self.t >= start && scale > 0.01 {
            let (cx, cy) = (x as f32 + width / 2.0, y as f32 + height / 2.0);
            let (half_width, half_height) = (width / 2.0 * scale, height / 2.0 * scale);
            self.canvas.rounded_rect(
                (
                    cx - half_width,
                    cy - half_height,
                    cx + half_width,
                    cy + half_height,
                ),
                half_height,
                fill,
            );
            if scale > 0.75 {
                self.line(
                    value,
                    x as f32 + padding,
                    y as f32 + height / 2.0 + size * 0.35,
                    size,
                    Face::Bold,
                    ink,
                    (0, HEIGHT),
                );
            }
        }
        y + height.ceil() as i32
    }
}

/// Shrink a pill's type until it fits, keeping its padding.
fn pill_size(value: &str, maximum_width: i32, size: f32) -> f32 {
    let available = maximum_width as f32 - size * 1.5;
    let width = text_width(value, size, Face::Bold);
    if width > available {
        size * available / width
    } else {
        size
    }
}

impl Stage<'_> {
    pub(super) fn pill_width(&self, value: &str, maximum_width: i32, size: f32) -> i32 {
        let fitted = pill_size(value, maximum_width, size);
        (text_width(value, fitted, Face::Bold) + size * 1.5).ceil() as i32
    }
}

pub(super) fn render_still(slide: &WrappedSlideData) -> Canvas {
    let mut stage = Stage::new(slide);
    stage.paint(1.0);
    stage.canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_spin_digits_and_keep_words() {
        assert_eq!(count_up("185h 42m", 1.0), "185h 42m");
        assert_eq!(count_up("185h 42m", 0.0), "0h 0m");
        assert_eq!(count_up("1,200", 0.5), "600");
        assert_eq!(count_up("12,000", 0.5), "6,000");
        assert_eq!(count_up("+4,000 JC", 0.25), "+1,000 JC");
        assert_eq!(count_up("The enabler", 0.5), "The enabler");
    }

    #[test]
    fn a_wrapped_block_never_overflows_its_box() {
        for value in [
            "Alexandria the Unreasonably Determined",
            "éééééééééééééééééééééééééééééééééééééééé",
            "A very long award with actual statistics and a very detailed explanation of how it was earned.",
            "Юлия Лебедева",
        ] {
            let (size, rows) = fit_text(value, Face::Black, 44.0, 320, 100);
            assert!(
                rows.iter()
                    .all(|row| text_width(row, size, Face::Black) <= 320.0)
            );
            assert!(block_height(rows.len(), size, Face::Black) <= 100.0);
        }
    }

    #[test]
    fn hopeless_copy_shrinks_then_truncates_visibly() {
        let value = "word ".repeat(200);
        let (size, rows) = fit_text(&value, Face::Medium, 40.0, 200, 60);
        assert_eq!(size, 14.0);
        assert!(rows.last().unwrap().ends_with('…'));
        assert!(block_height(rows.len(), size, Face::Medium) <= 60.0);
    }

    #[test]
    fn rendered_text_stays_inside_its_box() {
        let slide = WrappedSlideData::new("awards", "Wrapped");
        let mut stage = Stage::new(&slide);
        let (blank, rect) = (Rgba::rgb(1, 2, 3), (10, 10, 320, 100));
        for value in [
            "Alexandria the Unreasonably Determined",
            "gyp éééééééééééééééééééééééé",
        ] {
            stage.canvas = Canvas::new(400, 150, blank);
            stage.block(
                value,
                rect,
                44.0,
                Face::Black,
                CREAM,
                Align::Left,
                (0.0, 0.0),
            );
            assert!(stage.canvas.pixels.chunks_exact(4).any(|p| p != blank.0));
            for y in 0..150 {
                for x in 0..400 {
                    if !(10..330).contains(&x) || !(10..110).contains(&y) {
                        assert_eq!(&stage.canvas.pixels[(y * 400 + x) * 4..][..4], blank.0);
                    }
                }
            }
        }
    }

    #[test]
    fn every_theme_keeps_its_text_readable() {
        fn luminance(color: Rgba) -> f32 {
            let channel = |value: u8| {
                let value = f32::from(value) / 255.0;
                if value <= 0.03928 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.0[0])
                + 0.7152 * channel(color.0[1])
                + 0.0722 * channel(color.0[2])
        }
        fn contrast(a: Rgba, b: Rgba) -> f32 {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        }
        for theme in [
            WrappedTheme::Plain,
            WrappedTheme::Cover,
            WrappedTheme::Queue,
            WrappedTheme::QueueAlt,
            WrappedTheme::Heroes,
            WrappedTheme::Months,
            WrappedTheme::Squad,
            WrappedTheme::Rivals,
            WrappedTheme::Rating,
            WrappedTheme::GambaUp,
            WrappedTheme::GambaDown,
            WrappedTheme::Dig,
            WrappedTheme::DigDeep,
            WrappedTheme::Pet,
            WrappedTheme::PetGrave,
            WrappedTheme::Mafia,
            WrappedTheme::MafiaRed,
            WrappedTheme::Persona,
            WrappedTheme::Recap,
        ] {
            let p = palette(theme);
            assert!(contrast(p.fg, p.bg) >= 4.5, "{theme:?} text on background");
            assert!(
                contrast(p.fg, p.soft) >= 3.0,
                "{theme:?} text on moving shapes"
            );
            assert!(
                contrast(p.on_accent, p.accent) >= 4.5,
                "{theme:?} text on accent"
            );
            // Accent color carries the big numbers, which are large display type.
            assert!(contrast(p.accent, p.bg) >= 3.0, "{theme:?} big numbers");
        }
    }
}
