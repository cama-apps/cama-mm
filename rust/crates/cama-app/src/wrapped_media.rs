//! Native story-card renderer for the live Cama Wrapped story.
//!
//! Semantic story data becomes bold portrait cards. Every card is painted from
//! one timeline, so an animated reveal always ends on exactly the still card.

use std::io::Read;
use std::sync::OnceLock;

use crate::drawing::{GambaPoint, GambaStats};
use flate2::read::ZlibDecoder;
use fontdue::{Font, FontSettings};
use thiserror::Error;

mod artwork;
mod awards;
mod beats;
mod cards;
mod motifs;
mod motion;
mod shapes;
mod stage;

pub const WRAPPED_SLIDE_WIDTH: usize = 720;
pub const WRAPPED_SLIDE_HEIGHT: usize = 900;

/// Larger reveals fall back to the still card rather than risk a slow upload.
const MAX_ANIMATION_BYTES: usize = 4 * 1024 * 1024;

/// Exact typed payload for the Gamba page in `/wrapped`.
///
/// The story draws its own profit curve from these points. `/profile` keeps
/// its separate chart and attachment lifecycle.
#[derive(Clone, Debug, PartialEq)]
pub struct WrappedGambaData {
    pub degen_score: i32,
    pub degen_title: String,
    pub points: Vec<GambaPoint>,
    pub stats: GambaStats,
}

/// One ranked row, tile, or chart bar. `primary` is the loud half of the row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WrappedRow {
    pub primary: String,
    pub secondary: String,
    pub value: f64,
    /// A picture beside the row, such as a hero portrait. Images that do not
    /// decode are skipped.
    pub image_png: Option<Vec<u8>>,
}

impl WrappedRow {
    #[must_use]
    pub fn new(primary: impl Into<String>, secondary: impl Into<String>) -> Self {
        Self {
            primary: primary.into(),
            secondary: secondary.into(),
            value: 0.0,
            image_png: None,
        }
    }

    #[must_use]
    pub fn bar(primary: impl Into<String>, value: f64) -> Self {
        Self {
            primary: primary.into(),
            secondary: String::new(),
            value,
            image_png: None,
        }
    }
}

/// How a card arranges its content.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WrappedLayout {
    Cover,
    BigNumber,
    TopList,
    Bars,
    Curve,
    Tiles,
    Spotlight,
    Persona,
    Award,
    Recap,
    #[default]
    Details,
    /// A setup beat with the number, then a full-card flip to a punchline.
    TwoBeat,
}

/// The color story and animated background of a card.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WrappedTheme {
    #[default]
    Plain,
    Cover,
    Queue,
    QueueAlt,
    Heroes,
    Months,
    Squad,
    Rivals,
    Rating,
    GambaUp,
    GambaDown,
    Dig,
    DigDeep,
    Pet,
    PetGrave,
    Mafia,
    MafiaRed,
    Persona,
    Awards,
    Recap,
    /// Two-beat setups: coral and ink for a roast.
    BeatRoast,
    /// Two-beat setups: cobalt and yellow for a popoff.
    BeatPopoff,
    /// More roast setups, so neighboring cards never repeat a color: red
    /// and cream, violet and lime, plum and yellow.
    BeatRoastRed,
    BeatRoastViolet,
    BeatRoastPlum,
    /// More popoff setups: yellow and cobalt, lime and ink, pink and plum,
    /// aqua and teal.
    BeatPopoffYellow,
    BeatPopoffLime,
    BeatPopoffPink,
    BeatPopoffAqua,
}

/// Roast setups, the approved mockup palette first.
const ROAST_PALETTES: [WrappedTheme; 4] = [
    WrappedTheme::BeatRoast,
    WrappedTheme::BeatRoastRed,
    WrappedTheme::BeatRoastViolet,
    WrappedTheme::BeatRoastPlum,
];
/// Popoff setups, the approved mockup palette first.
const POPOFF_PALETTES: [WrappedTheme; 5] = [
    WrappedTheme::BeatPopoff,
    WrappedTheme::BeatPopoffYellow,
    WrappedTheme::BeatPopoffLime,
    WrappedTheme::BeatPopoffPink,
    WrappedTheme::BeatPopoffAqua,
];

impl WrappedTheme {
    /// The interchangeable two-beat palettes this theme belongs to.
    fn beat_family(self) -> Option<&'static [Self]> {
        if ROAST_PALETTES.contains(&self) {
            Some(&ROAST_PALETTES)
        } else if POPOFF_PALETTES.contains(&self) {
            Some(&POPOFF_PALETTES)
        } else {
            None
        }
    }

    /// Whether this is one of the roast two-beat palettes.
    #[must_use]
    pub fn is_roast_beat(self) -> bool {
        ROAST_PALETTES.contains(&self)
    }

    /// Whether this is one of the popoff two-beat palettes.
    #[must_use]
    pub fn is_popoff_beat(self) -> bool {
        POPOFF_PALETTES.contains(&self)
    }
}

/// How a two-beat card lands its punchline. Each look is type, hero art and
/// clean graphics only.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WrappedPunch {
    /// The punchline in big type on the setup's contrast colors.
    #[default]
    Plain,
    /// A letterboxed hero still under a serif title.
    Cinema,
    /// A team gold lead line from `series`, green above zero and red below.
    GoldGraph,
    /// A rising `series` line under condensed caps.
    Stonks,
    /// A falling `series` line under condensed caps.
    NotStonks,
    /// A win-chance bar filled to the percent in `stat_value`.
    Calculated,
    /// A defeat banner over the K/D/A `value`.
    TaskFailed,
    /// One loss tile per `series` entry.
    LossTiles,
    /// A keycap pressed once.
    PressF,
    /// A rank bar with the player's marker at the bottom end.
    SkillIssue,
    /// A digital clock showing the `h:mm AM` `value`.
    Clock,
}

impl WrappedPunch {
    pub const ALL: [Self; 11] = [
        Self::Plain,
        Self::Cinema,
        Self::GoldGraph,
        Self::Stonks,
        Self::NotStonks,
        Self::Calculated,
        Self::TaskFailed,
        Self::LossTiles,
        Self::PressF,
        Self::SkillIssue,
        Self::Clock,
    ];
}

/// The copy of a two-beat card. The setup beat shows `setup` over the big
/// `value` and `setup_label`; the punchline shows `punch` with `credit`, the
/// small fact line that lets the finished still stand alone.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WrappedBeat {
    pub setup: String,
    pub value: String,
    pub setup_label: String,
    pub punch: String,
    pub credit: String,
    pub treatment: WrappedPunch,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WrappedSlideData {
    pub layout: WrappedLayout,
    pub theme: WrappedTheme,
    pub artwork: WrappedArtwork,
    /// Trusted bundled art for this player, such as their pet, a boss scene,
    /// or a hero render. Never user-supplied bytes.
    pub art_png: Option<Vec<u8>>,
    pub chapter_index: usize,
    pub chapter_count: usize,
    pub kind: String,
    pub title: String,
    pub username: String,
    pub year_label: String,
    pub headline: String,
    pub stat_value: String,
    pub stat_label: String,
    pub badge: String,
    pub lines: Vec<String>,
    pub rows: Vec<WrappedRow>,
    pub accent: [u8; 3],
    pub series: Vec<f64>,
    pub secondary_series: Vec<f64>,
    pub gamba: Option<WrappedGambaData>,
    /// Copy for a `TwoBeat` card. Its hero art travels in `art_png`.
    pub beat: Option<WrappedBeat>,
}

impl WrappedSlideData {
    #[must_use]
    pub fn new(kind: impl Into<String>, title: impl Into<String>) -> Self {
        let kind = kind.into();
        let (layout, theme) = default_style(&kind);
        Self {
            layout,
            theme,
            artwork: WrappedArtwork::None,
            art_png: None,
            chapter_index: 0,
            chapter_count: 1,
            kind,
            title: title.into(),
            username: String::new(),
            year_label: String::new(),
            headline: String::new(),
            stat_value: String::new(),
            stat_label: String::new(),
            badge: String::new(),
            lines: Vec::new(),
            rows: Vec::new(),
            accent: [88, 101, 242],
            series: Vec::new(),
            secondary_series: Vec::new(),
            gamba: None,
            beat: None,
        }
    }
}

fn default_style(kind: &str) -> (WrappedLayout, WrappedTheme) {
    match kind {
        "story_intro" => (WrappedLayout::Cover, WrappedTheme::Cover),
        "story_finale" => (WrappedLayout::Recap, WrappedTheme::Recap),
        "story_games" | "story_playtime" => (WrappedLayout::BigNumber, WrappedTheme::Queue),
        "story_moment" => (WrappedLayout::BigNumber, WrappedTheme::QueueAlt),
        "story_superlative" => (WrappedLayout::Award, WrappedTheme::Awards),
        "story_hero" => (WrappedLayout::TopList, WrappedTheme::Heroes),
        "story_lanes" | "story_months" => (WrappedLayout::Bars, WrappedTheme::Months),
        "chart_rating" => (WrappedLayout::Curve, WrappedTheme::Rating),
        "chart_gamba" => (WrappedLayout::Curve, WrappedTheme::GambaUp),
        _ => (WrappedLayout::Details, WrappedTheme::Plain),
    }
}

/// Bundled character art shown beside a big reveal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WrappedArtwork {
    #[default]
    None,
    Ward,
    Courier,
}

#[derive(Debug, Error)]
pub enum WrappedMediaError {
    #[error("Wrapped slide text exceeds the supported image layout")]
    OversizedText,
    #[error("Wrapped animation could not be encoded: {0}")]
    Animation(#[from] gif::EncodingError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedRenderedMedia {
    pub filename: &'static str,
    pub bytes: Vec<u8>,
}

/// Animations play one reveal and end on the complete still card. Detail pages
/// and still mode always use PNG.
pub fn render_wrapped_media(
    slide: &WrappedSlideData,
    animated: bool,
) -> Result<WrappedRenderedMedia, WrappedMediaError> {
    validate_slide(slide)?;
    if !animated || slide.layout == WrappedLayout::Details {
        return Ok(still(slide));
    }
    let bytes = motion::animate(slide)?;
    if bytes.len() > MAX_ANIMATION_BYTES {
        return Ok(still(slide));
    }
    Ok(WrappedRenderedMedia {
        filename: "wrapped_slide.gif",
        bytes,
    })
}

fn still(slide: &WrappedSlideData) -> WrappedRenderedMedia {
    WrappedRenderedMedia {
        filename: "wrapped_slide.png",
        bytes: encode_png(&stage::render_still(slide)),
    }
}

/// Render one story page as a seekable, Discord-ready RGBA PNG.
pub fn render_wrapped_slide(slide: &WrappedSlideData) -> Result<Vec<u8>, WrappedMediaError> {
    validate_slide(slide)?;
    Ok(encode_png(&stage::render_still(slide)))
}

/// The background colors a card shows, first frame to last: one for most
/// cards, the setup's and then the punchline's for a two-beat card.
#[must_use]
pub fn slide_backgrounds(slide: &WrappedSlideData) -> Vec<[u8; 3]> {
    match slide
        .beat
        .as_ref()
        .filter(|_| slide.layout == WrappedLayout::TwoBeat)
    {
        Some(beat) => beat_backgrounds(slide.theme, beat.treatment).to_vec(),
        None => vec![theme_background(slide.theme)],
    }
}

/// The background color a card in `theme` shows outside a two-beat card.
#[must_use]
pub fn theme_background(theme: WrappedTheme) -> [u8; 3] {
    rgb(stage::palette(theme).bg)
}

fn beat_backgrounds(theme: WrappedTheme, treatment: WrappedPunch) -> [[u8; 3]; 2] {
    [
        rgb(stage::palette(theme).bg),
        rgb(beats::look(theme, treatment).bg),
    ]
}

fn rgb(color: Rgba) -> [u8; 3] {
    [color.0[0], color.0[1], color.0[2]]
}

/// Channels at or below this all read as black on a phone screen.
const NEAR_BLACK: u8 = 48;
/// Backgrounds closer than this in RGB look like one color side by side.
const ALIKE_DISTANCE: i32 = 48;

/// Whether two backgrounds read as the same color next to each other: close
/// in RGB, or both near-black, since black, ink and navy cards look alike.
#[must_use]
pub fn backgrounds_alike(a: [u8; 3], b: [u8; 3]) -> bool {
    let near_black = |color: [u8; 3]| color.iter().all(|channel| *channel <= NEAR_BLACK);
    let distance = a
        .iter()
        .zip(b)
        .map(|(first, second)| (i32::from(*first) - i32::from(second)).pow(2))
        .sum::<i32>();
    distance < ALIKE_DISTANCE.pow(2) || (near_black(a) && near_black(b))
}

/// Whether any color of `a` is alike to any color of `b`.
#[must_use]
pub fn any_backgrounds_alike(a: &[[u8; 3]], b: &[[u8; 3]]) -> bool {
    a.iter()
        .any(|first| b.iter().any(|second| backgrounds_alike(*first, *second)))
}

/// The background colors of a card that no palette change can move: a fixed
/// card's, and a meme punchline's, which its treatment sets. A plain
/// two-beat card can take any palette of its kind, so it has none.
#[must_use]
pub fn fixed_backgrounds(slide: &WrappedSlideData) -> Vec<[u8; 3]> {
    let recolorable = slide.layout == WrappedLayout::TwoBeat && slide.theme.beat_family().is_some();
    match slide.beat.as_ref().filter(|_| recolorable) {
        Some(beat) if beat.treatment == WrappedPunch::Plain => Vec::new(),
        Some(beat) => vec![rgb(beats::look(slide.theme, beat.treatment).bg)],
        None => slide_backgrounds(slide),
    }
}

/// Recolors a story's two-beat cards, in order, so that no two neighboring
/// cards share a background or two that look alike. Each card keeps its kind
/// (roast or popoff) and takes the next palette of that kind that clashes
/// with neither the card before it nor a fixed card after it, so long runs
/// also rotate colors.
pub fn vary_beat_palettes(slides: &mut [WrappedSlideData]) {
    let mut next_turn = [0_usize; 2];
    for index in 0..slides.len() {
        let slide = &slides[index];
        let (Some(beat), Some(family)) = (slide.beat.as_ref(), slide.theme.beat_family()) else {
            continue;
        };
        if slide.layout != WrappedLayout::TwoBeat {
            continue;
        }
        let treatment = beat.treatment;
        let turn = usize::from(slide.theme.is_roast_beat());
        let before = index
            .checked_sub(1)
            .map(|previous| slide_backgrounds(&slides[previous]))
            .unwrap_or_default();
        // A later two-beat card picks its own colors around this one, except
        // a meme punchline's, which its treatment fixes.
        let after = slides
            .get(index + 1)
            .map(fixed_backgrounds)
            .unwrap_or_default();
        let start = next_turn[turn];
        let clear_of = |candidate: usize, neighbors: &[&[[u8; 3]]]| {
            let colors = beat_backgrounds(family[candidate], treatment);
            neighbors
                .iter()
                .all(|taken| !any_backgrounds_alike(&colors, taken))
        };
        // When the card after is fixed in a like color, at least keep clear
        // of the card before.
        let order = || (0..family.len()).map(|offset| (start + offset) % family.len());
        let pick = order()
            .find(|candidate| clear_of(*candidate, &[&before, &after]))
            .or_else(|| order().find(|candidate| clear_of(*candidate, &[&before])))
            .unwrap_or(start % family.len());
        slides[index].theme = family[pick];
        next_turn[turn] = pick + 1;
    }
}

/// Whether two-beat copy fits its card at the designed type size, without
/// shrinking below the layout's minimum or breaking a word, under every
/// punchline treatment.
#[must_use]
pub fn two_beat_copy_fits(setup: &str, punch: &str) -> bool {
    beats::copy_fits(setup, punch)
}

fn validate_slide(slide: &WrappedSlideData) -> Result<(), WrappedMediaError> {
    let row_text = slide
        .rows
        .iter()
        .flat_map(|row| [&row.primary, &row.secondary]);
    let beat_text = slide.beat.iter().flat_map(|beat| {
        [
            &beat.setup,
            &beat.value,
            &beat.setup_label,
            &beat.punch,
            &beat.credit,
        ]
    });
    if slide
        .lines
        .iter()
        .chain(row_text)
        .chain(beat_text)
        .chain([
            &slide.title,
            &slide.username,
            &slide.year_label,
            &slide.headline,
            &slide.stat_value,
            &slide.stat_label,
            &slide.badge,
        ])
        .any(|line| line.chars().count() > 500)
    {
        return Err(WrappedMediaError::OversizedText);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rgba([u8; 4]);

impl Rgba {
    const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self([red, green, blue, 255])
    }

    fn scaled_alpha(self, coverage: f32) -> Self {
        let alpha = (f32::from(self.0[3]) * coverage.clamp(0.0, 1.0)).round() as u8;
        Self([self.0[0], self.0[1], self.0[2], alpha])
    }
}

#[derive(Clone, Debug)]
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
        if x >= self.width || y >= self.height {
            return;
        }
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

#[derive(Clone, Debug)]
struct DecodedPng {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

fn decode_png(bytes: &[u8]) -> Option<DecodedPng> {
    decode_png_within(bytes, 1024)
}

/// Trusted hero art arrives as Steam's full 1440 px renders. The decoded size
/// stays under the same memory bound as other art.
fn decode_art_png(bytes: &[u8]) -> Option<DecodedPng> {
    decode_png_within(bytes, 2048)
}

fn decode_png_within(bytes: &[u8], longest_side: usize) -> Option<DecodedPng> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut offset = 8_usize;
    let (mut width, mut height, mut color_type) = (0_usize, 0_usize, 0_u8);
    let mut idat = Vec::new();
    while offset.checked_add(12)? <= bytes.len() {
        let length = usize::try_from(u32::from_be_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
        .ok()?;
        let kind = bytes.get(offset + 4..offset + 8)?;
        let data = bytes.get(offset + 8..offset.checked_add(8 + length)?)?;
        offset = offset.checked_add(12 + length)?;
        match kind {
            b"IHDR" => {
                width =
                    usize::try_from(u32::from_be_bytes(data.get(0..4)?.try_into().ok()?)).ok()?;
                height =
                    usize::try_from(u32::from_be_bytes(data.get(4..8)?.try_into().ok()?)).ok()?;
                if data.get(8) != Some(&8) || data.get(10..13) != Some(&[0, 0, 0]) {
                    return None;
                }
                color_type = *data.get(9)?;
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
    }
    if width == 0 || height == 0 || width > longest_side || height > longest_side {
        return None;
    }
    if width.checked_mul(height)?.checked_mul(4)? > 8 * 1024 * 1024 {
        return None;
    }
    let channels = match color_type {
        2 => 3,
        6 => 4,
        _ => return None,
    };
    let row_bytes = width.checked_mul(channels)?;
    let expected = height.checked_mul(row_bytes.checked_add(1)?)?;
    if expected > 8 * 1024 * 1024 {
        return None;
    }
    let mut filtered = Vec::new();
    ZlibDecoder::new(idat.as_slice())
        .take(expected as u64 + 1)
        .read_to_end(&mut filtered)
        .ok()?;
    if filtered.len() != expected {
        return None;
    }
    let mut raw = vec![0_u8; width * height * channels];
    for row in 0..height {
        let source = &filtered[row * (row_bytes + 1)..(row + 1) * (row_bytes + 1)];
        let filter = source[0];
        for index in 0..row_bytes {
            let x = source[index + 1];
            let left = if index >= channels {
                raw[row * row_bytes + index - channels]
            } else {
                0
            };
            let up = if row > 0 {
                raw[(row - 1) * row_bytes + index]
            } else {
                0
            };
            let upper_left = if row > 0 && index >= channels {
                raw[(row - 1) * row_bytes + index - channels]
            } else {
                0
            };
            raw[row * row_bytes + index] = match filter {
                0 => x,
                1 => x.wrapping_add(left),
                2 => x.wrapping_add(up),
                3 => x.wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                4 => x.wrapping_add(paeth(left, up, upper_left)),
                _ => return None,
            };
        }
    }
    let mut pixels = Vec::with_capacity(width * height * 4);
    for pixel in raw.chunks_exact(channels) {
        pixels.extend_from_slice(&[
            pixel[0],
            pixel[1],
            pixel[2],
            if channels == 4 { pixel[3] } else { 255 },
        ]);
    }
    Some(DecodedPng {
        width,
        height,
        pixels,
    })
}

fn paeth(left: u8, up: u8, upper_left: u8) -> u8 {
    let prediction = i32::from(left) + i32::from(up) - i32::from(upper_left);
    let left_distance = (prediction - i32::from(left)).abs();
    let up_distance = (prediction - i32::from(up)).abs();
    let diagonal_distance = (prediction - i32::from(upper_left)).abs();
    if left_distance <= up_distance && left_distance <= diagonal_distance {
        left
    } else if up_distance <= diagonal_distance {
        up
    } else {
        upper_left
    }
}

fn encode_png(canvas: &Canvas) -> Vec<u8> {
    crate::png_encoder::encode_rgba_png(canvas.width, canvas.height, &canvas.pixels)
}

#[cfg(test)]
#[path = "wrapped_media/tests.rs"]
mod tests;
