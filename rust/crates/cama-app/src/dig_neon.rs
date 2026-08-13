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
use crate::neon_degen::{GifAsset, NeonResult, ansi_block, render_don_win};

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
        self.sample() < chance.clamp(0.0, 1.0)
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

fn animated_gif(kind: &'static str) -> GifAsset {
    // Valid GIF89a with a global two-color palette and two independently
    // seekable 1x1 image frames. The render port can replace it with the full
    // Python-equivalent scene without changing routing or attachment policy.
    const TWO_FRAME_GIF: &[u8] = b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff\
        !\xf9\x04\0\x0a\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0\
        !\xf9\x04\0\x0a\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;";
    GifAsset {
        kind,
        bytes: TWO_FRAME_GIF.to_vec(),
        frame_durations_ms: vec![100, 100],
        shared_palette: true,
    }
}

#[must_use]
pub fn animate_dig_reveal(
    layer_name: &str,
    motion: DigRevealMotion,
    _title: &str,
    _sub_lines: &[&str],
    _sprite_id: Option<&str>,
) -> GifAsset {
    let _palette = layer_palette(layer_name);
    match motion {
        DigRevealMotion::Victory => animated_gif("dig_reveal_victory"),
        DigRevealMotion::Unearth => animated_gif("dig_reveal_unearth"),
    }
}

#[must_use]
pub fn animate_legendary_relic(_relic_name: &str) -> GifAsset {
    animated_gif("dig_legendary_relic")
}

#[must_use]
pub fn animate_cave_in(layer_name: &str, _depth_before: i64, _depth_after: i64) -> GifAsset {
    let _palette = layer_palette(layer_name);
    animated_gif("dig_cave_in")
}

#[must_use]
pub fn animate_pinnacle(prestige: bool) -> GifAsset {
    if prestige {
        animated_gif("dig_prestige")
    } else {
        animated_gif("dig_pinnacle")
    }
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
            let mut chance = scaled_chance(event.boundary as f64, NEON_DIG_CHANCE, 0.30, 350.0);
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
            if !self.random.roll(NEON_DIG_CHANCE) {
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
        let chance = scaled_chance(lost as f64, NEON_DIG_CHANCE, 0.40, 40.0);
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
