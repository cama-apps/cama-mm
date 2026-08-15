//! Dig-native Neon GIF, narrator, and chance-routing policy.
//!
//! Routing decisions remain independent of Discord adapters: entropy,
//! cooldowns, captions, and rendering are explicit ports. The prediction
//! big-win renderer is production native; other Dig scenes retain their small
//! contract animation until their individual runtime providers are promoted.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::dig_assets::{LAYER_PALETTES, LayerPalette};
use crate::dig_flavor::LlmMessage;
use crate::neon_bigwin_media::render_big_win;
use crate::neon_degen::{
    COLOR_BLACK, COLOR_CYAN, COLOR_DARK_RED, COLOR_DIM_CYAN, COLOR_DIM_GREEN, COLOR_GREEN,
    COLOR_RED, COLOR_YELLOW, GifAsset, NEON_GIF_HEIGHT, NEON_GIF_WIDTH, NeonCanvas, NeonResult,
    ansi_block, render_don_win, render_neon_animation, render_sized_neon_animation,
};

pub const NEON_DIG_CHANCE: f64 = 0.12;
pub const NEON_BIGWIN_FLOOR: f64 = 0.05;
pub const NEON_BIGWIN_FULL_PAYOUT: i64 = 5_000;
pub const NEON_BIGWIN_MIN_PAYOUT: i64 = 500;
pub const NEON_LLM_CHANCE: f64 = 0.60;
pub const DISCORD_GIF_LIMIT: usize = 4 * 1_024 * 1_024;

pub const DIG_NARRATOR_SYSTEM_PROMPT: &str = concat!(
    "You are an ancient, indifferent presence of the deep earth, narrating the ",
    "fate of a lone digger. You speak in omen and image, never in explanation.\n\n",
    "Voice rules:\n",
    "- One or two short lines. Cryptic, mythic, weighty — like an epitaph or a riddle.\n",
    "- NEVER name game mechanics, numbers, currencies, items, depths, or systems. ",
    "Only image, omen, and consequence.\n",
    "- No emojis. No exclamation marks. No modern slang. No instructions to the reader.\n",
    "- You are not helpful. You are old, vast, and certain.\n",
    "- Do not use the digger's name. Do not break the spell."
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigVoice {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub affinity: &'static [&'static str],
}

pub const DIG_VOICES: [DigVoice; 7] = [
    DigVoice {
        key: "the_deep",
        name: "THE DEEP",
        description: "vast and indifferent, speaking in geologic time",
        affinity: &[],
    },
    DigVoice {
        key: "the_stone",
        name: "THE STONE",
        description: "terse, riddling, mineral and cold",
        affinity: &[],
    },
    DigVoice {
        key: "old_pick",
        name: "OLD PICK",
        description: "a long-dead miner's dry, weary murmur",
        affinity: &[],
    },
    DigVoice {
        key: "the_vein",
        name: "THE VEIN",
        description: "covetous and glittering, hungry for what is unearthed",
        affinity: &["rare_relic", "legendary_relic", "boss_victory"],
    },
    DigVoice {
        key: "the_damp",
        name: "THE DAMP",
        description: "creeping, cold, patient — the dark that waits",
        affinity: &["cave_in"],
    },
    DigVoice {
        key: "the_lantern",
        name: "THE LANTERN",
        description: "the last light; wry, almost kind, flickering",
        affinity: &["boss_victory", "rare_relic"],
    },
    DigVoice {
        key: "a_drowned_map",
        name: "A DROWNED MAP",
        description: "measures all distances, mourns nothing, charts the descent",
        affinity: &["pinnacle", "prestige", "cave_in"],
    },
];

const BOSS_VICTORY_LINES: &[&str] = &[
    "the guardian kept its post for an age. it keeps nothing now.",
    "something that never knelt has knelt.",
    "the dark is quieter by one old hunger.",
];
const RARE_RELIC_LINES: &[&str] = &[
    "the deep gives up one of its own. it will want it back.",
    "older than the tunnel, older than the hand that holds it.",
    "it was waiting. it is always waiting.",
];
const LEGENDARY_RELIC_LINES: &[&str] = &[
    "it waited longer than your name will last.",
    "the deep opens its hand. it does this once an age.",
    "something old surfaces, and remembers being held.",
];
const CAVE_IN_LINES: &[&str] = &[
    "the dark keeps what the dark is owed.",
    "stone forgets you were ever here.",
    "the way down closes like a mouth.",
];
const PINNACLE_LINES: &[&str] = &[
    "there is no further down. only this.",
    "the descent ends where the world does.",
    "you have reached the floor of everything.",
];
const PRESTIGE_LINES: &[&str] = &[
    "what goes down far enough comes back changed.",
    "the dark exhales, and lets one rise.",
    "you climb out wearing the deep like a second skin.",
];
const DEFAULT_LINES: &[&str] = &[
    "the deep notices. it rarely does.",
    "something shifts in the dark, and is still again.",
];

pub trait DigNeonRandomPort {
    fn roll(&mut self, chance: f64) -> bool;
    fn choose_index(&mut self, option_count: usize) -> usize;
}

#[derive(Clone, Debug)]
pub struct SeededDigNeonRandom {
    state: u64,
}

impl SeededDigNeonRandom {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        let mut state = self.state.max(1);
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.state = state;
        state
    }

    fn sample(&mut self) -> f64 {
        self.next() as f64 / u64::MAX as f64
    }
}

impl DigNeonRandomPort for SeededDigNeonRandom {
    fn roll(&mut self, chance: f64) -> bool {
        if chance.is_nan() || chance <= 0.0 {
            return false;
        }
        if chance >= 1.0 {
            return true;
        }
        self.sample() < chance
    }

    fn choose_index(&mut self, option_count: usize) -> usize {
        if option_count == 0 {
            return 0;
        }
        usize::try_from(self.next() % option_count as u64).unwrap_or(0)
    }
}

#[derive(Clone, Debug)]
pub struct ScriptedDigNeonRandom {
    rolls: VecDeque<bool>,
    default_roll: bool,
    choice_cursor: usize,
    observed_chances: Vec<f64>,
}

impl ScriptedDigNeonRandom {
    #[must_use]
    pub fn always(value: bool) -> Self {
        Self {
            rolls: VecDeque::new(),
            default_roll: value,
            choice_cursor: 0,
            observed_chances: Vec::new(),
        }
    }

    pub fn queue_rolls(&mut self, rolls: impl IntoIterator<Item = bool>) {
        self.rolls.extend(rolls);
    }

    #[must_use]
    pub fn observed_chances(&self) -> &[f64] {
        &self.observed_chances
    }
}

impl DigNeonRandomPort for ScriptedDigNeonRandom {
    fn roll(&mut self, chance: f64) -> bool {
        self.observed_chances.push(chance);
        self.rolls.pop_front().unwrap_or(self.default_roll)
    }

    fn choose_index(&mut self, option_count: usize) -> usize {
        if option_count == 0 {
            return 0;
        }
        let selected = self.choice_cursor % option_count;
        self.choice_cursor = self.choice_cursor.wrapping_add(1);
        selected
    }
}

#[must_use]
pub fn pick_dig_voice<R>(event_key: Option<&str>, random: &mut R) -> &'static DigVoice
where
    R: DigNeonRandomPort,
{
    let matching = event_key.map_or_else(Vec::new, |event_key| {
        DIG_VOICES
            .iter()
            .filter(|voice| voice.affinity.contains(&event_key))
            .collect::<Vec<_>>()
    });
    if !matching.is_empty() && random.roll(0.70) {
        return matching[random.choose_index(matching.len()) % matching.len()];
    }
    &DIG_VOICES[random.choose_index(DIG_VOICES.len()) % DIG_VOICES.len()]
}

#[must_use]
pub fn fallback_line<R>(event_key: &str, random: &mut R) -> &'static str
where
    R: DigNeonRandomPort,
{
    let lines = match event_key {
        "boss_victory" => BOSS_VICTORY_LINES,
        "rare_relic" => RARE_RELIC_LINES,
        "legendary_relic" => LEGENDARY_RELIC_LINES,
        "cave_in" => CAVE_IN_LINES,
        "pinnacle" => PINNACLE_LINES,
        "prestige" => PRESTIGE_LINES,
        _ => DEFAULT_LINES,
    };
    lines[random.choose_index(lines.len()) % lines.len()]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigRevealMotion {
    Victory,
    Unearth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BigWinSource {
    Match,
    Prediction,
    Gamba,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BigWinFlavor {
    BigWin,
    TopDog,
    Underdog,
}

#[must_use]
pub fn layer_palette(layer_name: &str) -> &'static LayerPalette {
    LAYER_PALETTES
        .iter()
        .find(|palette| palette.name == layer_name)
        .unwrap_or(&LAYER_PALETTES[0])
}

const DIG_ANIMATION_HOLD_MS: u32 = 60_000;

fn dig_durations(frame_count: usize, early_ms: u32, late_ms: u32) -> Vec<u32> {
    let mut durations = (0..frame_count)
        .map(|frame| {
            let progress = frame as f64 / frame_count.saturating_sub(1).max(1) as f64;
            if progress < 0.65 { early_ms } else { late_ms }
        })
        .collect::<Vec<_>>();
    if let Some(last) = durations.last_mut() {
        *last = DIG_ANIMATION_HOLD_MS;
    }
    durations
}

fn dig_scene_seed(parts: &[&str]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn dig_next_seed(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn bounded_scene_text(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect::<String>()
}

fn nearest_neon_color(rgb: (u8, u8, u8)) -> u8 {
    const NEON_COLORS: &[(u8, (u8, u8, u8))] = &[
        (COLOR_GREEN, (0, 255, 65)),
        (COLOR_CYAN, (0, 255, 255)),
        (3, (255, 0, 128)),
        (COLOR_RED, (255, 30, 30)),
        (COLOR_YELLOW, (255, 220, 0)),
        (COLOR_DIM_GREEN, (0, 120, 30)),
        (COLOR_DIM_CYAN, (0, 100, 100)),
    ];
    NEON_COLORS
        .iter()
        .min_by_key(|(_, candidate)| {
            let distance = |left: u8, right: u8| {
                let difference = i32::from(left) - i32::from(right);
                difference * difference
            };
            distance(rgb.0, candidate.0)
                + distance(rgb.1, candidate.1)
                + distance(rgb.2, candidate.2)
        })
        .map_or(COLOR_GREEN, |(index, _)| *index)
}

fn layer_neon_colors(palette: &LayerPalette) -> (u8, u8, u8) {
    (
        nearest_neon_color(palette.colors[1]),
        nearest_neon_color(palette.colors[2]),
        nearest_neon_color(palette.colors[3]),
    )
}

fn draw_dig_frame_grid(canvas: &mut NeonCanvas, shadow: u8, accent: u8, frame: usize) {
    canvas.rect(
        12,
        12,
        i32::from(NEON_GIF_WIDTH) - 13,
        i32::from(NEON_GIF_HEIGHT) - 13,
        shadow,
    );
    for x in (24..380).step_by(40) {
        canvas.line(x, 30, x, 275, shadow);
    }
    for y in (40..280).step_by(32) {
        canvas.line(24, y, 376, y, shadow);
    }
    if frame.is_multiple_of(2) {
        canvas.line(
            24,
            30 + (frame as i32 * 7) % 240,
            376,
            30 + (frame as i32 * 7) % 240,
            accent,
        );
    }
}

fn draw_dig_particles(canvas: &mut NeonCanvas, seed: u64, frame: usize, color: u8, count: usize) {
    let mut state = seed.wrapping_add(frame as u64 * 0x9e37_79b9);
    for index in 0..count {
        let sample = dig_next_seed(&mut state);
        let x = 24 + (sample % 352) as i32;
        let y = 35 + ((sample >> 16) % 238) as i32;
        if !(index + frame).is_multiple_of(3) {
            canvas.pixel(x, y, color);
        } else {
            canvas.fill_rect(x - 1, y - 1, x + 1, y + 1, color);
        }
    }
}

fn draw_dig_player(canvas: &mut NeonCanvas, x: i32, y: i32, color: u8) {
    canvas.rect(x, y, x + 24, y + 30, color);
    canvas.fill_rect(x + 8, y + 8, x + 16, y + 16, color);
    canvas.line(x + 22, y + 4, x + 34, y - 8, color);
    canvas.line(x + 34, y - 8, x + 39, y - 2, color);
}

fn draw_dig_relic(canvas: &mut NeonCanvas, x: i32, y: i32, size: i32, color: u8, glow: u8) {
    canvas.line(x, y - size, x + size, y, color);
    canvas.line(x + size, y, x, y + size, color);
    canvas.line(x, y + size, x - size, y, color);
    canvas.line(x - size, y, x, y - size, color);
    canvas.rect(x - size / 2, y - size / 2, x + size / 2, y + size / 2, glow);
}

#[must_use]
pub fn animate_dig_reveal(
    layer_name: &str,
    motion: DigRevealMotion,
    title: &str,
    sub_lines: &[&str],
    sprite_id: Option<&str>,
) -> GifAsset {
    let palette = layer_palette(layer_name);
    let layer_label = bounded_scene_text(layer_name, 42);
    let title = bounded_scene_text(title, 42);
    let sprite = bounded_scene_text(sprite_id.unwrap_or("crystal"), 28);
    let sub_lines = sub_lines
        .iter()
        .map(|line| bounded_scene_text(line, 42))
        .collect::<Vec<_>>();
    let seed = dig_scene_seed(&[
        layer_name,
        title.as_str(),
        sprite.as_str(),
        match motion {
            DigRevealMotion::Victory => "victory",
            DigRevealMotion::Unearth => "unearth",
        },
    ]);
    let durations = dig_durations(26, 80, 110);
    let frame_count = durations.len();
    let (shadow, accent, highlight) = layer_neon_colors(palette);
    let kind = match motion {
        DigRevealMotion::Victory => "dig_reveal_victory",
        DigRevealMotion::Unearth => "dig_reveal_unearth",
    };
    render_neon_animation(kind, &durations, seed, move |frame, canvas| {
        let progress = frame as f64 / (frame_count.saturating_sub(1).max(1)) as f64;
        canvas.fill(COLOR_BLACK);
        draw_dig_frame_grid(canvas, shadow, accent, frame);
        canvas.text_centered("DIG REVEAL", 22, highlight, 2);
        canvas.text_centered(&layer_label, 48, shadow, 1);
        draw_dig_particles(canvas, seed, frame, accent, 24);
        match motion {
            DigRevealMotion::Victory => {
                let boss_x = 245 - (progress * 55.0) as i32;
                let boss_y = 105;
                canvas.rect(boss_x, boss_y, boss_x + 48, boss_y + 48, accent);
                canvas.line(boss_x, boss_y, boss_x + 48, boss_y + 48, highlight);
                canvas.line(boss_x + 48, boss_y, boss_x, boss_y + 48, highlight);
                draw_dig_player(canvas, 100, 145, highlight);
                canvas.text_centered("GUARDIAN FALLS", 218, accent, 2);
            }
            DigRevealMotion::Unearth => {
                let relic_size = 12 + (progress * 34.0) as i32;
                draw_dig_relic(canvas, 200, 132, relic_size, highlight, accent);
                let curtain_top = 160 - (progress * 95.0) as i32;
                canvas.fill_rect(130, curtain_top, 270, 177, shadow);
                draw_dig_relic(canvas, 200, 132, relic_size, highlight, accent);
                canvas.text_centered("UNEARTHED", 218, accent, 2);
            }
        }
        canvas.text_centered(&title, 246, highlight, 1);
        for (index, line) in sub_lines.iter().enumerate().take(2) {
            canvas.text_centered(line, 260 + index as i32 * 14, shadow, 1);
        }
        canvas.text_left(&format!("SPRITE: {sprite}"), 22, 282, shadow, 1);
    })
}

#[must_use]
pub fn animate_legendary_relic(relic_name: &str) -> GifAsset {
    let name = bounded_scene_text(relic_name, 42);
    let seed = dig_scene_seed(&["legendary_relic", relic_name]);
    let durations = dig_durations(34, 90, 130);
    let frame_count = durations.len();
    render_neon_animation(
        "dig_legendary_relic",
        &durations,
        seed,
        move |frame, canvas| {
            let progress = frame as f64 / (frame_count.saturating_sub(1).max(1)) as f64;
            canvas.fill(COLOR_BLACK);
            canvas.rect(12, 12, 387, 287, COLOR_DIM_CYAN);
            canvas.text_centered("LEGENDARY RELIC", 26, COLOR_YELLOW, 2);
            let size = 12 + (progress * 46.0) as i32;
            let y = 240 - (progress * 105.0) as i32;
            draw_dig_relic(canvas, 200, y, size, COLOR_YELLOW, COLOR_CYAN);
            draw_dig_particles(canvas, seed, frame, COLOR_CYAN, 38);
            canvas.line(200 - size * 2, y, 200 + size * 2, y, COLOR_DIM_CYAN);
            canvas.text_centered(&name, 258, COLOR_GREEN, 1);
            canvas.text_centered("THE DEEP OPENS ITS HAND", 278, COLOR_DIM_GREEN, 1);
        },
    )
}

#[must_use]
pub fn animate_cave_in(layer_name: &str, depth_before: i64, depth_after: i64) -> GifAsset {
    let palette = layer_palette(layer_name);
    let layer_label = bounded_scene_text(layer_name, 42);
    let before = depth_before.to_string();
    let after = depth_after.to_string();
    let seed = dig_scene_seed(&["cave_in", layer_name, &before, &after]);
    let durations = dig_durations(28, 70, 110);
    let frame_count = durations.len();
    let (shadow, accent, highlight) = layer_neon_colors(palette);
    render_neon_animation("dig_cave_in", &durations, seed, move |frame, canvas| {
        let progress = frame as f64 / (frame_count.saturating_sub(1).max(1)) as f64;
        canvas.fill(COLOR_BLACK);
        draw_dig_frame_grid(canvas, shadow, accent, frame);
        canvas.text_centered("CAVE IN", 24, COLOR_RED, 2);
        canvas.text_centered(&layer_label, 50, shadow, 1);
        let current =
            (depth_before as f64 + (depth_after - depth_before) as f64 * progress).round();
        canvas.text_centered(&format!("DEPTH {current:.0}"), 118, highlight, 3);
        let darkness = (progress * 150.0) as i32;
        canvas.fill_rect(48, 88 + darkness / 6, 352, 275, COLOR_DARK_RED);
        for index in 0..12 {
            let mut state = seed
                .wrapping_add(index as u64 * 0x517c_c1b7)
                .wrapping_add(frame as u64 * 0x9e37_79b9);
            let x = 30 + (dig_next_seed(&mut state) % 340) as i32;
            let y = 70 + ((dig_next_seed(&mut state) >> 16) % 190) as i32;
            let size = 2 + (dig_next_seed(&mut state) % 8) as i32;
            canvas.fill_rect(x, y, x + size, y + size / 2, accent);
        }
        canvas.text_centered(&format!("{before} -> {after}"), 250, highlight, 1);
        canvas.text_centered("THE DARK CLOSES IN", 272, shadow, 1);
    })
}

// The Python Dig prestige/Pinnacle renderer is a deliberately quiet 320x180
// dungeon attachment, rather than one of the 400x300 terminal animations.
// Keep its palette local to this family so changing the terminal palette does
// not silently change the live `/dig` prestige attachment.
const DIG_PINNACLE_WIDTH: u16 = 320;
const DIG_PINNACLE_HEIGHT: u16 = 180;
const DIG_PINNACLE_BG_TERMINAL: u8 = 0;
const DIG_PINNACLE_BG_PRESTIGE_START: u8 = 1;
const DIG_PINNACLE_GLOW_START: u8 = 17;
const DIG_PINNACLE_PRESTIGE_GLOW_START: u8 = 41;
const DIG_PINNACLE_PRESTIGE_TITLE: u8 = 25;
const DIG_PINNACLE_TERMINAL_TITLE: u8 = 26;
const DIG_PINNACLE_PRESTIGE_SUBTITLE: u8 = 27;
const DIG_PINNACLE_TERMINAL_SUBTITLE: u8 = 28;
const DIG_PINNACLE_PLAYER: u8 = 29;
const DIG_PINNACLE_PICKAXE: u8 = 30;
const DIG_PINNACLE_PRESTIGE_TITLE_DIM: u8 = 31;
const DIG_PINNACLE_TERMINAL_TITLE_DIM: u8 = 32;
const DIG_PINNACLE_PRESTIGE_SUBTITLE_DIM: u8 = 33;
const DIG_PINNACLE_TERMINAL_SUBTITLE_DIM: u8 = 34;
const DIG_PINNACLE_PRESTIGE_TITLE_MID: u8 = 35;
const DIG_PINNACLE_TERMINAL_TITLE_MID: u8 = 36;
const DIG_PINNACLE_PRESTIGE_SUBTITLE_MID: u8 = 37;
const DIG_PINNACLE_TERMINAL_SUBTITLE_MID: u8 = 38;
const DIG_PINNACLE_PICKAXE_HEAD: u8 = 39;
const DIG_PINNACLE_PLAYER_EYE: u8 = 40;

const DIG_PINNACLE_PALETTE: &[u8] = &[
    // terminal background
    6, 5, 9, // prestige backgrounds: Python's int(10 + 60*t), clamped into 16 bands
    10, 6, 0, 14, 10, 0, 18, 14, 0, 22, 18, 2, 26, 22, 6, 30, 26, 10, 34, 30, 14, 38, 34, 18, 42,
    38, 22, 46, 42, 26, 50, 46, 30, 54, 50, 34, 58, 54, 38, 62, 58, 42, 66, 62, 46, 70, 66, 50,
    // terminal glow bands, from outer/dim to inner/bright
    12, 9, 10, 16, 12, 12, 24, 16, 14, 32, 23, 16, 45, 32, 20, 67, 47, 26, 91, 64, 33, 111, 76,
    39, // titles, subtitles, player, and pickaxe
    255, 240, 200, 235, 195, 140, 204, 192, 160, 188, 156, 112, 255, 255, 100, 180, 180, 180,
    // staged title/subtitle colors for Python's zero-to-full alpha ramp
    60, 50, 32, 55, 45, 32, 50, 45, 35, 45, 38, 28, 160, 145, 100, 150, 120, 80, 120, 110, 90, 110,
    90, 65, // distinct 200,200,200 point and black eye pixels
    200, 200, 200, 0, 0, 0,
    // prestige glow bands: the authored prestige background is already
    // brighter, so the foreground threshold is reached farther out.
    70, 66, 50, 76, 72, 53, 84, 78, 57, 100, 92, 66, 118, 108, 75, 140, 128, 87, 165, 150, 103, 190,
    170, 115,
];

fn dig_pinnacle_ease(linear_progress: f64) -> f64 {
    let progress = linear_progress.clamp(0.0, 1.0);
    progress * progress * (3.0 - 2.0 * progress)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DigPinnacleTextStage {
    Hidden,
    Dim,
    Mid,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DigPinnaclePhaseState {
    linear_progress: f64,
    progress: f64,
    player_top: i32,
    glow_center_y: i32,
    title_stage: DigPinnacleTextStage,
    subtitle_stage: DigPinnacleTextStage,
}

fn dig_pinnacle_text_stage(progress: f64) -> DigPinnacleTextStage {
    let alpha = ((progress - 0.45) / 0.55).clamp(0.0, 1.0);
    if alpha <= 0.0 {
        DigPinnacleTextStage::Hidden
    // Python alpha-blends the full title color. Around 25% opacity its
    // brightest glyph pixels cross the visual foreground threshold, so move
    // to the middle palette band at the same point.
    } else if alpha < 0.25 {
        DigPinnacleTextStage::Dim
    } else if alpha < 0.78 {
        DigPinnacleTextStage::Mid
    } else {
        DigPinnacleTextStage::Full
    }
}

fn dig_pinnacle_phase_state(
    frame: usize,
    frame_count: usize,
    prestige: bool,
) -> DigPinnaclePhaseState {
    let linear_progress = frame as f64 / (frame_count.saturating_sub(1).max(1)) as f64;
    let progress = dig_pinnacle_ease(linear_progress);
    let player_top = if prestige {
        i32::from(DIG_PINNACLE_HEIGHT / 2)
            - (i32::from(DIG_PINNACLE_HEIGHT / 3) as f64 * progress) as i32
    } else {
        i32::from(DIG_PINNACLE_HEIGHT / 3)
            + (i32::from(DIG_PINNACLE_HEIGHT / 3) as f64 * progress) as i32
    };
    let glow_center_y = if prestige {
        i32::from(DIG_PINNACLE_HEIGHT / 2)
    } else {
        i32::from(DIG_PINNACLE_HEIGHT / 2) + 10
    };
    let text_stage = dig_pinnacle_text_stage(progress);
    DigPinnaclePhaseState {
        linear_progress,
        progress,
        player_top,
        glow_center_y,
        title_stage: text_stage,
        subtitle_stage: text_stage,
    }
}

fn staged_text_color(progress: f64, full: u8, middle: u8, dim: u8) -> Option<u8> {
    match dig_pinnacle_text_stage(progress) {
        DigPinnacleTextStage::Hidden => None,
        DigPinnacleTextStage::Dim => Some(dim),
        DigPinnacleTextStage::Mid => Some(middle),
        DigPinnacleTextStage::Full => Some(full),
    }
}

fn dig_pinnacle_glow_band_count(progress: f64) -> usize {
    if progress <= 0.0 {
        0
    } else {
        (progress * 8.0).ceil() as usize
    }
    .min(8)
}

fn draw_dig_pinnacle_glow(
    canvas: &mut NeonCanvas,
    center_x: i32,
    center_y: i32,
    radius: i32,
    progress: f64,
    prestige: bool,
) {
    let radius = radius.max(0);
    let active_bands = dig_pinnacle_glow_band_count(progress);
    let color_start = if prestige {
        DIG_PINNACLE_PRESTIGE_GLOW_START
    } else {
        DIG_PINNACLE_GLOW_START
    };
    for band in (0..active_bands.min(8)).rev() {
        let band_radius = radius.saturating_mul((band + 1) as i32) / 8;
        let color = color_start + u8::try_from(7 - band).unwrap_or(0);
        canvas.circle(center_x, center_y, band_radius, color);
    }
}

fn draw_dig_pinnacle_player(canvas: &mut NeonCanvas, center_x: i32, top: i32) {
    let left = center_x - 16;
    // Exact alpha mask of Python's authored 16x16 sprite, enlarged with
    // nearest-neighbour 2x2 blocks. `K` is the opaque black eye pixel and
    // `g` is the one-pixel pickaxe; the source point at (14,2) has its own
    // 200,200,200 color while the diagonal shaft is 180,180,180.
    const MASK: [&str; 16] = [
        "................",
        "......PPPP......",
        ".....PPPPPP...g.",
        "....PPPPPPPP..g.",
        "....PPKPPKPP.g..",
        "....PPPPPPPPg...",
        "....PPPPPPPg....",
        ".....PPPPPP.....",
        ".....PPPPPP.....",
        ".....PPPPPP.....",
        ".....PPPPPP.....",
        ".....PPPPPP.....",
        ".....PPPPPP.....",
        ".....PPPPPP.....",
        ".....PPPPPP.....",
        "................",
    ];
    for (source_y, row) in MASK.iter().enumerate() {
        for (source_x, marker) in row.chars().enumerate() {
            let color = match marker {
                'P' => DIG_PINNACLE_PLAYER,
                'K' => DIG_PINNACLE_PLAYER_EYE,
                'g' if source_y == 2 && source_x == 14 => DIG_PINNACLE_PICKAXE_HEAD,
                'g' => DIG_PINNACLE_PICKAXE,
                _ => continue,
            };
            let x = left + i32::try_from(source_x * 2).unwrap_or(0);
            let y = top + i32::try_from(source_y * 2).unwrap_or(0);
            canvas.fill_rect(x, y, x + 1, y + 1, color);
        }
    }
}

#[must_use]
pub fn animate_pinnacle(prestige: bool) -> GifAsset {
    let kind = if prestige {
        "dig_prestige"
    } else {
        "dig_pinnacle"
    };
    // The final hold is intentionally part of the live Python attachment
    // contract. Pillow may coalesce the first duplicate logical frame when
    // decoding, but the authored timeline is 17x90ms + 12x130ms + 60s.
    let mut durations = vec![90; 17];
    durations.extend([130; 12]);
    durations.push(DIG_ANIMATION_HOLD_MS);
    let frame_count = durations.len();
    render_sized_neon_animation(
        kind,
        DIG_PINNACLE_WIDTH,
        DIG_PINNACLE_HEIGHT,
        DIG_PINNACLE_PALETTE,
        &durations,
        move |frame, canvas| {
            let phase = dig_pinnacle_phase_state(frame, frame_count, prestige);
            canvas.fill(if prestige {
                DIG_PINNACLE_BG_PRESTIGE_START
                    + u8::try_from((phase.progress * 15.0).round() as usize)
                        .unwrap_or(15)
                        .min(15)
            } else {
                DIG_PINNACLE_BG_TERMINAL
            });
            let radius = if prestige {
                40 + (130.0 * phase.progress) as i32
            } else {
                20 + (80.0 * phase.progress) as i32
            };
            draw_dig_pinnacle_glow(
                canvas,
                160,
                phase.glow_center_y,
                radius,
                phase.progress,
                prestige,
            );
            draw_dig_pinnacle_player(canvas, 160, phase.player_top);
            let title_color = if prestige {
                staged_text_color(
                    phase.progress,
                    DIG_PINNACLE_PRESTIGE_TITLE,
                    DIG_PINNACLE_PRESTIGE_TITLE_MID,
                    DIG_PINNACLE_PRESTIGE_TITLE_DIM,
                )
            } else {
                staged_text_color(
                    phase.progress,
                    DIG_PINNACLE_TERMINAL_TITLE,
                    DIG_PINNACLE_TERMINAL_TITLE_MID,
                    DIG_PINNACLE_TERMINAL_TITLE_DIM,
                )
            };
            if let Some(color) = title_color {
                canvas.text_centered_dejavu(
                    if prestige {
                        "ASCENSION"
                    } else {
                        "THE PINNACLE"
                    },
                    i32::from(DIG_PINNACLE_HEIGHT) - 46,
                    color,
                    18.0,
                    true,
                );
            }
            let subtitle_color = if prestige {
                staged_text_color(
                    phase.progress,
                    DIG_PINNACLE_PRESTIGE_SUBTITLE,
                    DIG_PINNACLE_PRESTIGE_SUBTITLE_MID,
                    DIG_PINNACLE_PRESTIGE_SUBTITLE_DIM,
                )
            } else {
                staged_text_color(
                    phase.progress,
                    DIG_PINNACLE_TERMINAL_SUBTITLE,
                    DIG_PINNACLE_TERMINAL_SUBTITLE_MID,
                    DIG_PINNACLE_TERMINAL_SUBTITLE_DIM,
                )
            };
            if let Some(color) = subtitle_color {
                canvas.text_centered_dejavu(
                    if prestige { "PRESTIGE" } else { "DEPTH 350" },
                    i32::from(DIG_PINNACLE_HEIGHT) - 26,
                    color,
                    12.0,
                    false,
                );
            }
        },
    )
}

#[must_use]
pub fn create_bigwin_gif(
    name: &str,
    payout: i64,
    source: BigWinSource,
    flavor: BigWinFlavor,
) -> GifAsset {
    render_big_win(name, payout, source, flavor)
        .expect("the in-memory native Neon GIF encoder writes to Vec without I/O")
}

pub trait DigNeonRenderPort: Send + Sync {
    fn reveal(
        &self,
        layer_name: &str,
        motion: DigRevealMotion,
        title: &str,
        sub_lines: &[&str],
        sprite_id: Option<&str>,
    ) -> Result<GifAsset, String>;
    fn legendary_relic(&self, relic_name: &str) -> Result<GifAsset, String>;
    fn cave_in(
        &self,
        layer_name: &str,
        depth_before: i64,
        depth_after: i64,
    ) -> Result<GifAsset, String>;
    fn pinnacle(&self, prestige: bool) -> Result<GifAsset, String>;
    fn big_win(
        &self,
        name: &str,
        payout: i64,
        source: BigWinSource,
        flavor: BigWinFlavor,
    ) -> Result<GifAsset, String>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContractDigNeonRenderer;

impl DigNeonRenderPort for ContractDigNeonRenderer {
    fn reveal(
        &self,
        layer_name: &str,
        motion: DigRevealMotion,
        title: &str,
        sub_lines: &[&str],
        sprite_id: Option<&str>,
    ) -> Result<GifAsset, String> {
        Ok(animate_dig_reveal(
            layer_name, motion, title, sub_lines, sprite_id,
        ))
    }

    fn legendary_relic(&self, relic_name: &str) -> Result<GifAsset, String> {
        Ok(animate_legendary_relic(relic_name))
    }

    fn cave_in(
        &self,
        layer_name: &str,
        depth_before: i64,
        depth_after: i64,
    ) -> Result<GifAsset, String> {
        Ok(animate_cave_in(layer_name, depth_before, depth_after))
    }

    fn pinnacle(&self, prestige: bool) -> Result<GifAsset, String> {
        Ok(animate_pinnacle(prestige))
    }

    fn big_win(
        &self,
        name: &str,
        payout: i64,
        source: BigWinSource,
        flavor: BigWinFlavor,
    ) -> Result<GifAsset, String> {
        Ok(create_bigwin_gif(name, payout, source, flavor))
    }
}

pub trait DigNeonCooldownPort {
    fn is_ready(&self, discord_id: u64, guild_id: Option<u64>) -> bool;
    fn mark_fired(&mut self, discord_id: u64, guild_id: Option<u64>);
}

#[derive(Clone, Debug)]
pub struct MemoryDigNeonCooldown {
    cooldown_seconds: u64,
    now_seconds: u64,
    last_fired: HashMap<(u64, u64), u64>,
}

impl Default for MemoryDigNeonCooldown {
    fn default() -> Self {
        Self {
            cooldown_seconds: 60,
            now_seconds: 10_000,
            last_fired: HashMap::new(),
        }
    }
}

impl MemoryDigNeonCooldown {
    pub fn advance(&mut self, seconds: u64) {
        self.now_seconds = self.now_seconds.saturating_add(seconds);
    }
}

impl DigNeonCooldownPort for MemoryDigNeonCooldown {
    fn is_ready(&self, discord_id: u64, guild_id: Option<u64>) -> bool {
        let key = (discord_id, guild_id.unwrap_or(0));
        self.last_fired.get(&key).is_none_or(|last_fired| {
            self.now_seconds.saturating_sub(*last_fired) >= self.cooldown_seconds
        })
    }

    fn mark_fired(&mut self, discord_id: u64, guild_id: Option<u64>) {
        self.last_fired
            .insert((discord_id, guild_id.unwrap_or(0)), self.now_seconds);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigCaptionRequest {
    pub messages: Vec<LlmMessage>,
    pub temperature: f64,
    pub max_tokens: u32,
    pub feature: &'static str,
}

pub trait DigCaptionPort: Send + Sync {
    fn complete(&self, request: DigCaptionRequest) -> Result<Option<String>, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BossVictory<'a> {
    pub boss_name: &'a str,
    pub boundary: i64,
    pub layer_name: &'a str,
    pub jc_delta: i64,
    pub gear_drop: bool,
    pub trophy_relic_drop: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelicFound<'a> {
    pub relic_name: &'a str,
    pub rarity: &'a str,
    pub layer_name: &'a str,
}

pub struct DigNeonService<R, C>
where
    R: DigNeonRandomPort,
    C: DigNeonCooldownPort,
{
    enabled: bool,
    dig_llm_enabled: bool,
    ai_enabled: bool,
    dig_chance: f64,
    random: R,
    cooldown: C,
    renderer: Arc<dyn DigNeonRenderPort>,
    caption: Option<Arc<dyn DigCaptionPort>>,
}

impl<R, C> DigNeonService<R, C>
where
    R: DigNeonRandomPort,
    C: DigNeonCooldownPort,
{
    #[must_use]
    pub fn new(random: R, cooldown: C) -> Self {
        Self {
            enabled: true,
            dig_llm_enabled: true,
            ai_enabled: true,
            dig_chance: NEON_DIG_CHANCE,
            random,
            cooldown,
            renderer: Arc::new(ContractDigNeonRenderer),
            caption: None,
        }
    }

    #[must_use]
    pub fn with_renderer(mut self, renderer: Arc<dyn DigNeonRenderPort>) -> Self {
        self.renderer = renderer;
        self
    }

    #[must_use]
    pub fn with_caption(mut self, caption: Arc<dyn DigCaptionPort>) -> Self {
        self.caption = Some(caption);
        self
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn set_dig_llm_enabled(&mut self, enabled: bool) {
        self.dig_llm_enabled = enabled;
    }

    pub fn set_ai_enabled(&mut self, enabled: bool) {
        self.ai_enabled = enabled;
    }

    /// Set the base probability used by ordinary Dig Neon events.
    ///
    /// Retain the parsed Python configuration value verbatim. The random
    /// port applies Python's comparison semantics for values outside the
    /// probability interval, including infinities and NaN. Pinnacle and
    /// prestige remain their separate authored 0.95 rolls.
    pub fn set_dig_chance(&mut self, chance: f64) {
        self.dig_chance = chance;
    }

    #[must_use]
    pub const fn dig_chance(&self) -> f64 {
        self.dig_chance
    }

    #[must_use]
    pub const fn random(&self) -> &R {
        &self.random
    }

    pub fn random_mut(&mut self) -> &mut R {
        &mut self.random
    }

    #[must_use]
    pub const fn cooldown(&self) -> &C {
        &self.cooldown
    }

    pub fn cooldown_mut(&mut self) -> &mut C {
        &mut self.cooldown
    }

    pub fn dig_caption(
        &mut self,
        event_key: &str,
        event_description: &str,
        _guild_id: Option<u64>,
    ) -> String {
        let voice = *pick_dig_voice(Some(event_key), &mut self.random);
        let fallback = fallback_line(event_key, &mut self.random).to_owned();
        let line = if self.dig_llm_enabled
            && self.ai_enabled
            && self.caption.is_some()
            && self.random.roll(NEON_LLM_CHANCE)
        {
            let prompt = format!(
                "A lone digger {event_description}, far beneath the earth. Speak one or two short, cryptic lines as {} - {}. Only image and omen. Never name mechanics, numbers, or items.",
                voice.name, voice.description
            );
            let request = DigCaptionRequest {
                messages: vec![
                    LlmMessage {
                        role: "system",
                        content: DIG_NARRATOR_SYSTEM_PROMPT.to_owned(),
                    },
                    LlmMessage {
                        role: "user",
                        content: prompt,
                    },
                ],
                temperature: 1.0,
                max_tokens: 120,
                feature: "neon.dig_caption",
            };
            self.caption
                .as_ref()
                .and_then(|caption| caption.complete(request).ok().flatten())
                .map(|line| clean_caption(&line))
                .filter(|line| !line.is_empty())
                .unwrap_or(fallback)
        } else {
            fallback
        };
        format!("> *{line}*\n> — {}", voice.name)
    }

    pub fn on_dig_boss_victory(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        event: BossVictory<'_>,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.cooldown.is_ready(discord_id, guild_id) {
            return None;
        }
        let (gif, event_key, description) = if event.boundary >= 350 {
            if !self.random.roll(0.95) {
                return None;
            }
            (
                self.renderer.pinnacle(false).ok()?,
                "pinnacle",
                "has reached the Pinnacle, the floor of the world".to_owned(),
            )
        } else {
            let mut chance = scaled_chance(event.boundary as f64, self.dig_chance, 0.30, 350.0);
            if event.gear_drop || event.trophy_relic_drop {
                chance = (chance + 0.10).min(0.45);
            }
            if !self.random.roll(chance) {
                return None;
            }
            let title = if event.boss_name.is_empty() {
                "THE GUARDIAN".to_owned()
            } else {
                event.boss_name.to_uppercase()
            };
            let payout = (event.jc_delta != 0).then(|| format!("+{} jc", event.jc_delta));
            let sub_lines = payout.as_deref().into_iter().collect::<Vec<_>>();
            (
                self.renderer
                    .reveal(
                        event.layer_name,
                        DigRevealMotion::Victory,
                        &title,
                        &sub_lines,
                        None,
                    )
                    .ok()?,
                "boss_victory",
                format!("has struck down the guardian {}", event.boss_name),
            )
        };
        let text = self.dig_caption(event_key, &description, guild_id);
        self.cooldown.mark_fired(discord_id, guild_id);
        Some(neon_with_gif(Some(text), gif))
    }

    pub fn on_dig_relic_found(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        event: RelicFound<'_>,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.cooldown.is_ready(discord_id, guild_id) {
            return None;
        }
        let rarity = event.rarity.to_ascii_lowercase();
        let (gif, event_key, description) = if rarity == "legendary" {
            if !self.random.roll(0.95) {
                return None;
            }
            (
                self.renderer.legendary_relic(event.relic_name).ok()?,
                "legendary_relic",
                format!("has unearthed the legendary {}", event.relic_name),
            )
        } else if rarity == "rare" {
            if !self.random.roll(self.dig_chance) {
                return None;
            }
            (
                self.renderer
                    .reveal(
                        event.layer_name,
                        DigRevealMotion::Unearth,
                        event.relic_name,
                        &[],
                        Some("crystal"),
                    )
                    .ok()?,
                "rare_relic",
                format!("has unearthed the rare {}", event.relic_name),
            )
        } else {
            return None;
        };
        let text = self.dig_caption(event_key, &description, guild_id);
        self.cooldown.mark_fired(discord_id, guild_id);
        Some(neon_with_gif(Some(text), gif))
    }

    pub fn on_dig_cave_in(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        depth_before: i64,
        depth_after: i64,
        layer_name: &str,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        let lost = depth_before - depth_after;
        if lost <= 0 || !self.cooldown.is_ready(discord_id, guild_id) {
            return None;
        }
        let chance = scaled_chance(lost as f64, self.dig_chance, 0.40, 40.0);
        if !self.random.roll(chance) {
            return None;
        }
        let gif = self
            .renderer
            .cave_in(layer_name, depth_before, depth_after)
            .ok()?;
        let text = self.dig_caption(
            "cave_in",
            "has lost their footing to a cave-in, the dark swallowing the way down",
            guild_id,
        );
        self.cooldown.mark_fired(discord_id, guild_id);
        Some(neon_with_gif(Some(text), gif))
    }

    pub fn on_dig_prestige(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
    ) -> Option<NeonResult> {
        if !self.enabled || !self.cooldown.is_ready(discord_id, guild_id) || !self.random.roll(0.95)
        {
            return None;
        }
        let gif = self.renderer.pinnacle(true).ok()?;
        let text = self.dig_caption(
            "prestige",
            "has ascended, prestiging beyond the deepest dark",
            guild_id,
        );
        self.cooldown.mark_fired(discord_id, guild_id);
        Some(neon_with_gif(Some(text), gif))
    }

    pub fn on_big_win(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        source: BigWinSource,
        payout: i64,
        flavor: BigWinFlavor,
    ) -> Option<NeonResult> {
        if !self.enabled
            || payout < NEON_BIGWIN_MIN_PAYOUT
            || !self.cooldown.is_ready(discord_id, guild_id)
        {
            return None;
        }
        let chance = scaled_chance(
            payout as f64,
            NEON_BIGWIN_FLOOR,
            0.95,
            NEON_BIGWIN_FULL_PAYOUT as f64,
        );
        if !self.random.roll(chance) {
            return None;
        }
        let name = format!("Client {discord_id}");
        let gif = self.renderer.big_win(&name, payout, source, flavor).ok()?;
        let text = ansi_block(&format!(
            "[LEDGER] +{payout} JC settled. The house keeps receipts on winners too."
        ));
        self.cooldown.mark_fired(discord_id, guild_id);
        Some(neon_with_gif(Some(text), gif))
    }

    pub fn on_wheel_result(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        result_value: i64,
        _new_balance: i64,
    ) -> Option<NeonResult> {
        if result_value > 0 {
            return self.on_big_win(
                discord_id,
                guild_id,
                BigWinSource::Gamba,
                result_value,
                BigWinFlavor::BigWin,
            );
        }
        None
    }

    pub fn on_double_or_nothing(
        &mut self,
        discord_id: u64,
        guild_id: Option<u64>,
        won: bool,
        balance_at_risk: i64,
        final_balance: i64,
    ) -> Option<NeonResult> {
        if !self.enabled {
            return None;
        }
        if won {
            if let Some(result) = self.on_big_win(
                discord_id,
                guild_id,
                BigWinSource::Gamba,
                balance_at_risk,
                BigWinFlavor::BigWin,
            ) {
                return Some(result);
            }
            self.cooldown.mark_fired(discord_id, guild_id);
            return Some(NeonResult {
                layer: 1,
                text_block: Some(render_don_win(
                    &format!("Client {discord_id}"),
                    final_balance,
                )),
                gif_file: None,
                footer_text: None,
            });
        }
        None
    }
}

#[must_use]
pub fn scaled_chance(score: f64, floor: f64, cap: f64, full_at: f64) -> f64 {
    if full_at <= 0.0 {
        return cap;
    }
    let fraction = (score / full_at).clamp(0.0, 1.0);
    floor + (cap - floor) * fraction
}

fn clean_caption(value: &str) -> String {
    let mut cleaned = value.trim();
    if let Some(after_open) = cleaned.strip_prefix("```") {
        cleaned = after_open.split_once('\n').map_or("", |(_, body)| body);
        cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned).trim();
    }
    cleaned.trim_matches('"').trim().to_owned()
}

fn neon_with_gif(text_block: Option<String>, gif_file: GifAsset) -> NeonResult {
    NeonResult {
        layer: 3,
        text_block,
        gif_file: Some(gif_file),
        footer_text: None,
    }
}

#[cfg(test)]
#[path = "dig_neon/tests.rs"]
mod tests;
