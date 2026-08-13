//! Typed, fail-soft Dig narration for the side-by-side Rust application.
//!
//! The deterministic Dig resolver remains authoritative. This module accepts
//! an already-resolved outcome, builds the same bounded prompt context as the
//! Python service, validates the single tool call, and applies only the two
//! permitted side effects: a capped balance nudge and a per-guild memory
//! rewrite. Network, Discord, configuration, and persistence are ports so the
//! parity policy can run locally without starting a second bot or DB writer.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::ai_services::{
    AIService, Message, MessageRole, ToolCallResult as AiToolCallResult,
    ToolChoice as AiToolChoice, ToolDefinition as AiToolDefinition, ToolProperty as AiToolProperty,
    ToolPropertySchema as AiToolPropertySchema, ToolRequest, Value as AiValue,
};
use crate::dig_service::{BASE_DIG_JC_PAYOUT_CAP, DigOutcomeInput, TunnelState, apply_dig_outcome};
use cama_domain::guild_config::GuildConfigStore;

pub const NARRATIVE_MAX_CHARS: usize = 300;
pub const NPC_LINE_MAX_CHARS: usize = 200;
pub const MEMORY_MAX_BYTES: usize = 2_048;
pub const CALLBACK_MAX_CHARS: usize = 200;
pub const BONUS_CAP_BIG_PCT: f64 = 10.0;
pub const BONUS_CAP_SMALL_PCT: f64 = 5.0;
pub const BONUS_CAP_BIG_CHANCE: f64 = 0.10;
pub const NARRATE_DIG_TOOL_NAME: &str = "narrate_dig";
pub const NARRATE_SPLASH_TOOL_NAME: &str = "narrate_splash";
pub const CATASTROPHIC_PREFIX: &str = "💥";

pub const CATASTROPHIC_LINES: [&str; 8] = [
    "Far below, a tunnel folds shut. Stone settles for a long time.",
    "A great groan rolls through the rock. Somewhere in the dark, a miner is buried.",
    "Beams snap. Lanterns wink out. The earth swallows a tunnel whole.",
    "The deep gives a sound like a closing door. Then silence.",
    "Dust climbs out of a shaft that no longer goes anywhere.",
    "A pickaxe rings once, then stops. The ceiling has come for it.",
    "The hill shifts. Birds lift from leagues away.",
    "Below the bones of the world, a chamber that was now isn't.",
];

/// Narrow entropy seam for canonical catastrophic-line selection.
pub trait FlameEntropy {
    /// Return an index in `0..upper_exclusive`.
    fn index(&mut self, upper_exclusive: usize) -> usize;
}

#[must_use]
pub fn pick_catastrophic_line<E: FlameEntropy + ?Sized>(entropy: &mut E) -> &'static str {
    CATASTROPHIC_LINES[entropy.index(CATASTROPHIC_LINES.len())]
}

/// Failure returned when a channel cannot create its detached send future.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlameSendError;

/// Failure returned when no runtime task can be detached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlameTaskError;

pub type FlameSendFuture =
    Pin<Box<dyn Future<Output = Result<(), FlameSendError>> + Send + 'static>>;

/// Discord-neutral channel seam. `Ok(None)` represents a channel-like value
/// without a usable `send` method; `Err` represents a synchronous send error.
pub trait AtmosphericChannel {
    fn send(&self, content: String) -> Result<Option<FlameSendFuture>, FlameSendError>;
}

/// Runtime-neutral fire-and-forget task seam. A caller without a running
/// event loop passes `None`, causing the already-created future to be dropped.
pub trait AtmosphericTaskSpawner {
    fn spawn(&self, task: FlameSendFuture) -> Result<(), FlameTaskError>;
}

/// Format and detach one atmospheric channel post without letting transport
/// or scheduler failure escape into the committed Dig flow.
pub fn post_atmospheric<C, S>(channel: Option<&C>, spawner: Option<&S>, prefix: &str, line: &str)
where
    C: AtmosphericChannel,
    S: AtmosphericTaskSpawner,
{
    if channel.is_none() || line.is_empty() {
        return;
    }
    let formatted = format!("{prefix} *{}*", line.trim());
    let Some(channel) = channel else {
        return;
    };
    let Ok(Some(task)) = channel.send(formatted) else {
        return;
    };
    let Some(spawner) = spawner else {
        return;
    };
    let _ = spawner.spawn(task);
}

/// Pick and detach one canonical catastrophic cave-in post.
pub fn post_catastrophic<C, S, E>(channel: Option<&C>, spawner: Option<&S>, entropy: &mut E)
where
    C: AtmosphericChannel,
    S: AtmosphericTaskSpawner,
    E: FlameEntropy + ?Sized,
{
    post_atmospheric(
        channel,
        spawner,
        CATASTROPHIC_PREFIX,
        pick_catastrophic_line(entropy),
    );
}

/// Immutable canonical NPC entry used in Dig DM prompt context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigNpc {
    npc_id: &'static str,
    title: &'static str,
    voice: &'static str,
    triggers: &'static str,
    sample_lines: &'static [&'static str],
}

impl DigNpc {
    #[must_use]
    pub const fn npc_id(self) -> &'static str {
        self.npc_id
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        self.title
    }

    #[must_use]
    pub const fn voice(self) -> &'static str {
        self.voice
    }

    #[must_use]
    pub const fn triggers(self) -> &'static str {
        self.triggers
    }

    #[must_use]
    pub const fn sample_lines(self) -> &'static [&'static str] {
        self.sample_lines
    }
}

const THE_SURVEYOR_LINES: [&str; 3] = [
    "Shaft 1923 went bad here too. Nothing rebuilt it.",
    "Stone reads the same in any layer. Grain runs east. Yours runs through it.",
    "I marked this passage twenty years ago. Nobody asked me back.",
];

const THE_OLD_HAND_LINES: [&str; 3] = [
    "Walked away from worse. Walk on.",
    "First one's the bad one. Each after, you carry a little less.",
    "You're alright, kid. Keep your hands where you can see them.",
];

const THE_ONE_WHO_COUNTS_LINES: [&str; 3] = [
    "There is a wall somewhere. New marks have appeared on it.",
    "You feel counted. You do not know by what.",
    "Something in the dark has finished tallying. It begins again.",
];

const THE_LISTENER_LINES: [&str; 3] = [
    "The dark has been listening. The dark has been listening.",
    "They say if you hear your own name down here, do not turn.",
    "The names go in. The names do not always come back.",
];

const THE_FOREMAN_LINES: [&str; 3] = [
    "Half of what you dug today, gone before sundown.",
    "You earn it down here. You spend it up there. The arithmetic does the rest.",
    "Nobody's asking how you sleep. They're asking what you owe.",
];

/// Canonical insertion order from Python's `NPCS` dict literal.
pub const NPCS: &[DigNpc] = &[
    DigNpc {
        npc_id: "the_surveyor",
        title: "the Surveyor",
        voice: "industrial_grim",
        triggers: "Layer transitions, hesitation at boundaries, depth milestones. Speaks in short fragments about old shafts.",
        sample_lines: &THE_SURVEYOR_LINES,
    },
    DigNpc {
        npc_id: "the_old_hand",
        title: "the Old Hand",
        voice: "industrial_grim",
        triggers: "After cave-ins, after long streaks, after debt or loss. Sympathetic but dry. Never sentimental.",
        sample_lines: &THE_OLD_HAND_LINES,
    },
    DigNpc {
        npc_id: "the_one_who_counts",
        title: "the One Who Counts",
        voice: "cosmic_dread",
        triggers: "Boss boundaries, prestige resets, deep layers. Rarely speaks. Is felt rather than seen. Marks tally on the wall.",
        sample_lines: &THE_ONE_WHO_COUNTS_LINES,
    },
    DigNpc {
        npc_id: "the_listener",
        title: "the Listener",
        voice: "cryptic_folkloric",
        triggers: "Low luminosity, risky-streak digs, after a vow or grudge. Speaks like local superstition. Repeats herself.",
        sample_lines: &THE_LISTENER_LINES,
    },
    DigNpc {
        npc_id: "the_foreman",
        title: "the Foreman",
        voice: "industrial_grim",
        triggers: "Big JC hauls, after debt, near shop activity, after losses to the bombs / bets. Pragmatic, transactional, mocks waste.",
        sample_lines: &THE_FOREMAN_LINES,
    },
];

pub const VALID_VOICES: [&str; 3] = ["cosmic_dread", "industrial_grim", "cryptic_folkloric"];

#[must_use]
pub fn npc_by_id(npc_id: &str) -> Option<DigNpc> {
    NPCS.iter().copied().find(|npc| npc.npc_id() == npc_id)
}

/// Flatten the canon roster into the prompt-injection bullet representation.
#[must_use]
pub fn roster_lines() -> Vec<String> {
    NPCS.iter()
        .map(|npc| {
            format!(
                "- {} ({}, {}): {}",
                npc.npc_id(),
                npc.title(),
                npc.voice(),
                npc.triggers()
            )
        })
        .collect()
}

pub const SPLASH_NARRATION_SYSTEM_PROMPT: &str = "You write a single short prose beat — 1 to 2 sentences, ≤200 characters total — for the moment a tunnel event reaches other players in a Dota 2 inhouse league Discord bot's mining minigame.\n\nThe mechanical effects (JC movement, victims chosen) are already resolved. Do not restate amounts, rolls, success chances, rarity, percentages, or JC. Match the supplied event flavor, address victims by their Discord display names, and distinguish steal, burn, and grant. Output only a narrate_splash tool call.";

pub const PICKAXE_TIER_NAMES: [&str; 7] = [
    "Wooden",
    "Stone",
    "Iron",
    "Diamond",
    "Obsidian",
    "Frostforged",
    "Void-Touched",
];

pub const TONE_PROFILES: [(&str, &str); 3] = [
    (
        "cosmic_dread",
        "Cosmic / mythic dread. The depths are old and indifferent. The player is small. Something ancient watches. Hushed, ominous, sparing with words. Reference Annihilation, Dark Souls item descriptions. Avoid action verbs when stillness will do.",
    ),
    (
        "industrial_grim",
        "Industrial / blue-collar grim. Tunnels are dirty, the pickaxes are real, the danger is mechanical. Workers, debt, dust. Terse and stoic, dark humor when something goes wrong. Reference Disco Elysium worker dialogue, early Coen Brothers.",
    ),
    (
        "cryptic_folkloric",
        "Cryptic / folkloric. Local superstition, half-remembered rules, things you don't say aloud. Oblique and rhythmic, repetition and refrains. Reference Annie Proulx, regional folklore, weird Americana. The narration is allowed to speak in fragments.",
    ),
];

pub const PLAY_STYLE_DESCRIPTIONS: [(&str, &str); 6] = [
    (
        "cautious_grinder",
        "plays it safe, rarely takes risky event options, steady and methodical",
    ),
    (
        "reckless_degen",
        "takes every desperate option, thrill-seeker, high risk tolerance",
    ),
    (
        "calculated_risk_taker",
        "balances risk and reward, picks risky options when odds favor them",
    ),
    (
        "balanced_explorer",
        "mixes safe and risky choices evenly, no strong preference",
    ),
    (
        "social_butterfly",
        "frequently helps other players and cheers in boss fights, community-oriented",
    ),
    ("unknown", "new player, still developing their style"),
];

pub const NARRATE_DIG_PROPERTIES: [&str; 7] = [
    "narrative",
    "tone",
    "callback_reference",
    "picked_event_id",
    "npc_appearance",
    "flavor_bonus_pct",
    "memory_update",
];

pub const NARRATE_DIG_REQUIRED: [&str; 2] = ["narrative", "tone"];
pub const SPLASH_NARRATION_PROPERTIES: [&str; 1] = ["narrative"];
pub const SPLASH_NARRATION_REQUIRED: [&str; 1] = ["narrative"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    pub kind: &'static str,
    pub name: &'static str,
    pub properties: &'static [&'static str],
    pub required: &'static [&'static str],
}

pub const NARRATE_DIG_TOOL: ToolDefinition = ToolDefinition {
    kind: "function",
    name: NARRATE_DIG_TOOL_NAME,
    properties: &NARRATE_DIG_PROPERTIES,
    required: &NARRATE_DIG_REQUIRED,
};

pub const SPLASH_NARRATION_TOOL: ToolDefinition = ToolDefinition {
    kind: "function",
    name: NARRATE_SPLASH_TOOL_NAME,
    properties: &SPLASH_NARRATION_PROPERTIES,
    required: &SPLASH_NARRATION_REQUIRED,
};

fn ai_string_property(name: &str, description: &str) -> AiToolProperty {
    AiToolProperty {
        name: name.to_owned(),
        description: description.to_owned(),
        enum_values: Vec::new(),
        schema: AiToolPropertySchema::String,
    }
}

/// Build the OpenAI-compatible schema for Python's `NARRATE_DIG_TOOL`.
///
/// This deliberately remains a function instead of a static value because
/// the provider-neutral AI service owns `String` fields.  Keep descriptions,
/// required fields, and the intentionally open nested object in lockstep with
/// `services/dig_llm_prompts.py`; post-call validation remains authoritative.
#[must_use]
pub fn narrate_dig_ai_tool_definition() -> AiToolDefinition {
    AiToolDefinition {
        name: NARRATE_DIG_TOOL_NAME.to_owned(),
        description: "Add flavor on top of an already-resolved dig outcome. Mechanics are immutable; this call writes prose and optional embellishments (NPC line, narrative-only event pick, small JC nudge, memory note).".to_owned(),
        properties: vec![
            ai_string_property(
                "narrative",
                "2-4 sentences, ≤300 chars total. Atmospheric, on the rolled tone profile. NEVER include numbers, JC amounts, block counts, depths, or percentages — those live in the embed fields below your prose.",
            ),
            AiToolProperty {
                name: "tone".to_owned(),
                description: "MUST match the tone profile injected into the system prompt for this dig. If you return a different one, the validator will overwrite it.".to_owned(),
                enum_values: TONE_PROFILES
                    .iter()
                    .map(|(tone, _)| (*tone).to_owned())
                    .collect(),
                schema: AiToolPropertySchema::String,
            },
            ai_string_property(
                "callback_reference",
                "Optional. A short fragment from prior memory worth calling back to (vow, NPC encounter, recurring motif). Empty string if nothing fits.",
            ),
            ai_string_property(
                "picked_event_id",
                "Optional. When the user message lists ELIGIBLE EVENTS, pick exactly one id from that list. Out-of-list ids are silently dropped. Empty string for no event.",
            ),
            AiToolProperty {
                name: "npc_appearance".to_owned(),
                description: "Optional. Either a roster id (preferred) or an invented title; plus the NPC's single short line. Invented names will be persisted to memory.".to_owned(),
                enum_values: Vec::new(),
                schema: AiToolPropertySchema::Object {
                    // Python's schema does not mark either nested field as
                    // required and does not close the object.  The validator
                    // accepts only a complete id+line pair, preserving that
                    // fail-soft behavior after provider decoding.
                    properties: vec![
                        ai_string_property(
                            "id_or_name",
                            "Roster id (e.g. 'the_old_hand') or invented title.",
                        ),
                        ai_string_property("line", "One short line, ≤200 chars, in the NPC's voice."),
                    ],
                    required: Vec::new(),
                    additional_properties: None,
                },
            },
            AiToolProperty {
                name: "flavor_bonus_pct".to_owned(),
                description: "Optional. JC nudge as a signed percent of jc_earned. Will be CLAMPED to ±cap (announced in system prompt). Use sparingly — only when the moment earns it. 0 or omitted = no nudge.".to_owned(),
                enum_values: Vec::new(),
                schema: AiToolPropertySchema::Number,
            },
            ai_string_property(
                "memory_update",
                "Optional. Rewrite your scratchpad — what you want to remember about this player for next time. ≤2KB. Omit to leave existing memory unchanged.",
            ),
        ],
        required: NARRATE_DIG_REQUIRED
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        additional_properties: None,
    }
}

/// Build the OpenAI-compatible schema for Python's `SPLASH_NARRATION_TOOL`.
#[must_use]
pub fn splash_narration_ai_tool_definition() -> AiToolDefinition {
    AiToolDefinition {
        name: NARRATE_SPLASH_TOOL_NAME.to_owned(),
        description: "Narrate the moment a tunnel event reaches other players. Mechanics are already resolved — the JC has moved. Output is one short paragraph, no numbers, no mechanics.".to_owned(),
        properties: vec![ai_string_property(
            "narrative",
            "1-2 sentences (≤200 characters total). Name the victim(s) by Discord display name. Match the event's vibe (luminous / ominous / etc.). Do NOT mention JC amounts, rolls, success rates, or rarity.",
        )],
        required: SPLASH_NARRATION_REQUIRED
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        additional_properties: None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NpcAppearance {
    pub id_or_name: String,
    pub line: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlavorToolArgs {
    pub narrative: Option<String>,
    pub tone: Option<String>,
    pub callback_reference: Option<String>,
    pub picked_event_id: Option<String>,
    pub npc_appearance: Option<NpcAppearance>,
    pub flavor_bonus_pct: Option<f64>,
    pub memory_update: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlavorResult {
    pub narrative: String,
    pub tone: String,
    pub callback_reference: String,
    pub picked_event_id: Option<String>,
    pub npc_appearance: Option<NpcAppearance>,
    pub flavor_bonus_pct: f64,
    pub memory_update: Option<String>,
}

#[must_use]
pub fn classify_play_style(histogram: &BTreeMap<String, i64>) -> &'static str {
    let total = histogram.values().sum::<i64>();
    if total < 5 {
        return "unknown";
    }
    let fraction = |key: &str| *histogram.get(key).unwrap_or(&0) as f64 / total as f64;
    if fraction("help") > 0.3 {
        "social_butterfly"
    } else if fraction("safe") > 0.7 {
        "cautious_grinder"
    } else if fraction("desperate") > 0.15 {
        "reckless_degen"
    } else if fraction("risky") > 0.4 {
        "calculated_risk_taker"
    } else {
        "balanced_explorer"
    }
}

fn truncate_chars(value: &str, maximum: usize) -> String {
    if value.chars().count() <= maximum {
        return value.to_owned();
    }
    let body = value
        .chars()
        .take(maximum.saturating_sub(3))
        .collect::<String>();
    format!("{}...", body.trim_end())
}

fn truncate_utf8_bytes(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut end = maximum.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[must_use]
pub fn validate_flavor_args(
    args: &FlavorToolArgs,
    eligible_event_ids: &BTreeSet<String>,
    rolled_tone: &str,
    bonus_cap_pct: f64,
) -> Option<FlavorResult> {
    let narrative = args.narrative.as_deref().unwrap_or_default().trim();
    if narrative.is_empty() || narrative.chars().any(char::is_numeric) {
        return None;
    }
    let narrative = truncate_chars(narrative, NARRATIVE_MAX_CHARS);
    let returned_tone = args.tone.as_deref().unwrap_or(rolled_tone);
    let tone = if TONE_PROFILES
        .iter()
        .any(|(candidate, _)| *candidate == returned_tone)
    {
        returned_tone
    } else {
        rolled_tone
    }
    .to_owned();
    let callback_reference = truncate_chars(
        args.callback_reference
            .as_deref()
            .unwrap_or_default()
            .trim(),
        CALLBACK_MAX_CHARS,
    );
    let picked_event_id = args
        .picked_event_id
        .as_deref()
        .map(str::trim)
        .filter(|event_id| !event_id.is_empty() && eligible_event_ids.contains(*event_id))
        .map(str::to_owned);
    let npc_appearance = args.npc_appearance.as_ref().and_then(|npc| {
        let id_or_name = npc.id_or_name.trim();
        let line = npc.line.trim();
        if id_or_name.is_empty() || line.is_empty() {
            None
        } else {
            Some(NpcAppearance {
                id_or_name: id_or_name.chars().take(80).collect(),
                line: truncate_chars(line, NPC_LINE_MAX_CHARS),
            })
        }
    });
    let cap = bonus_cap_pct.abs();
    let flavor_bonus_pct = args.flavor_bonus_pct.unwrap_or(0.0).clamp(-cap, cap);
    let memory_update = args
        .memory_update
        .as_deref()
        .map(|memory| truncate_utf8_bytes(memory, MEMORY_MAX_BYTES));
    Some(FlavorResult {
        narrative,
        tone,
        callback_reference,
        picked_event_id,
        npc_appearance,
        flavor_bonus_pct,
        memory_update,
    })
}

#[must_use]
pub fn validate_splash_narrative(narrative: &str) -> String {
    let narrative = narrative.trim();
    if narrative.is_empty() {
        String::new()
    } else {
        truncate_chars(narrative, 200)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedSplashVictim {
    pub name: String,
    pub amount: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SplashVictim {
    pub discord_id: i64,
    pub amount: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct SplashNarrationInput<'a> {
    pub digger_id: i64,
    pub guild_id: i64,
    pub event_name: &'a str,
    pub event_description: &'a str,
    pub splash_mode: &'a str,
    pub victims: &'a [SplashVictim],
    pub digger_layer: Option<&'a str>,
}

#[must_use]
pub fn build_splash_narration_messages(
    digger_name: &str,
    digger_layer: &str,
    event_name: &str,
    event_description: &str,
    splash_mode: &str,
    victims: &[NamedSplashVictim],
) -> Vec<LlmMessage> {
    let victim_lines = if victims.is_empty() {
        "- (no resolvable names)".to_owned()
    } else {
        victims
            .iter()
            .map(|victim| format!("- {}", victim.name))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let user = [
        format!("=== DIGGER ===\n{digger_name} (in the {digger_layer})"),
        format!("=== EVENT ===\n{event_name}\n{event_description}"),
        format!("=== SPLASH MODE ===\n{splash_mode}"),
        format!("=== VICTIMS ===\n{victim_lines}"),
    ]
    .join("\n\n");
    vec![
        LlmMessage {
            role: "system",
            content: SPLASH_NARRATION_SYSTEM_PROMPT.to_owned(),
        },
        LlmMessage {
            role: "user",
            content: user,
        },
    ]
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SplashAftermath {
    pub mode: String,
    pub victims: Vec<SplashVictim>,
    pub llm_narrative: Option<String>,
    pub absorbed_total: i64,
    pub shielded_count: i64,
}

#[must_use]
pub fn splash_aftermath_lines(splash: &SplashAftermath) -> Vec<String> {
    let sign = if splash.mode == "grant" { "+" } else { "-" };
    let mut lines = Vec::new();
    let narrative = splash.llm_narrative.as_deref().unwrap_or_default().trim();
    if !narrative.is_empty() {
        lines.push(format!("*{narrative}*"));
    }
    lines.extend(splash.victims.iter().map(|victim| {
        format!(
            "<@{}>: {sign}{} {}",
            victim.discord_id,
            victim.amount,
            cama_domain::formatting::JOPACOIN_EMOTE
        )
    }));
    if splash.absorbed_total > 0 {
        lines.push(format!(
            "🌾 {} White shield activation(s) absorbed {} {}.",
            splash.shielded_count,
            splash.absorbed_total,
            cama_domain::formatting::JOPACOIN_EMOTE
        ));
    }
    lines
}

#[must_use]
pub fn layer_name(depth: i64) -> &'static str {
    match depth {
        0..=25 => "Dirt",
        26..=50 => "Stone",
        51..=75 => "Crystal",
        76..=100 => "Magma",
        101..=150 => "Abyss",
        151..=200 => "Fungal Depths",
        201..=275 => "Frozen Core",
        _ => "The Hollow",
    }
}

fn tone_guide(tone: &str) -> &'static str {
    TONE_PROFILES
        .iter()
        .find_map(|(key, guide)| (*key == tone).then_some(*guide))
        .unwrap_or(TONE_PROFILES[1].1)
}

#[must_use]
pub fn build_dig_flavor_system_prompt(tone: &str, bonus_cap_pct: f64) -> String {
    format!(
        "You are an additive flavor layer on top of a Dota 2 inhouse Discord bot's tunnel-mining minigame. The mechanical outcome of this dig (advance, JC, cave-in, milestones, splashes) HAS ALREADY BEEN COMPUTED AND PERSISTED. You do not decide what happened. You write prose and select on-tone color around it.\n\nTONE PROFILE FOR THIS DIG: {tone}\n{}\n\nYOUR JOB: Call narrate_dig with at least narrative + tone. You may also pass:\n  - picked_event_id — pick from the ELIGIBLE EVENTS list in the user message, when there is one. Out-of-list ids are silently dropped.\n  - npc_appearance — invoke a canon NPC from the NPC ROSTER, OR invent a new titled figure when none fits. Invented names are persisted to memory and can recur. Roster and invention are weighted equally — pick whichever the moment calls for. Use sparingly; not every dig needs an NPC.\n  - flavor_bonus_pct — your one tiny mechanical knob. CAP FOR THIS DIG: ±{}%. Out-of-range values are clamped silently. Use only when a narrative beat earns the swing. Most digs should be 0.\n  - memory_update — rewrite your scratchpad. Up to 2KB. Keep what matters; drop what's stale. Omit to leave it unchanged.\n\nHARD RULES:\n  - NUMBERS ARE FORBIDDEN IN PROSE. No JC amounts, no block counts, no depth integers, no percentages, no times. The embed shows mechanics; your narrative is atmosphere only. Narration containing digits will be rejected and replaced with canned text.\n  - STAY ON THE TONE PROFILE above. Reread it if you forget which voice.\n  - Tight: ≤300 characters, 2-4 sentences max.\n  - Reference the active player by their Discord display name. Other miners' names are fine when narratively useful.\n  - Only narrate what ACTUALLY HAPPENED. The DIG OUTCOME section is authoritative. Don't describe a cave-in if there was none, don't claim a milestone the player didn't cross.\n  - Don't expose mechanics. Don't say \"luminosity\" — say \"light\", \"glow\", \"the dark\". Don't name S stats — show them indirectly.\n\nNPC TIPS:\n  - Each canon NPC has triggers describing when they fit. Honor them.\n  - Invented NPCs should be titled (no proper names). \"The Watcher\", \"the Saw-bones\", \"the One Who Listens\" — never \"Eli\" or \"Marda\".\n  - One NPC per dig at most. Most digs have none.\n\nOUTPUT: Always call narrate_dig. No plain text responses.",
        tone_guide(tone),
        bonus_cap_pct as i64
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlavorBuff {
    pub name: String,
    pub digs_remaining: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlavorWeather {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelFlavorState {
    pub tunnel_name: String,
    pub depth: i64,
    pub pickaxe_tier: i64,
    pub prestige_level: i64,
    pub luminosity: i64,
    pub streak_days: i64,
    pub total_digs: i64,
    pub total_jc_earned: i64,
    pub max_depth: i64,
    pub miner_about: String,
    pub strength: i64,
    pub smarts: i64,
    pub stamina: i64,
    pub stat_points: i64,
    pub relics: Vec<String>,
    pub buffs: Vec<FlavorBuff>,
    pub mutations: Vec<String>,
    pub weather: Option<FlavorWeather>,
    pub boss_progress: BTreeMap<String, String>,
    pub boss_attempts: i64,
    pub best_run_score: i64,
    pub cheer_data: Option<String>,
    pub revenge_until: i64,
    pub revenge_type: String,
    pub trap_active: bool,
    pub insured_until: i64,
    pub reinforced_until: i64,
    pub injury_state: Option<String>,
}

impl Default for TunnelFlavorState {
    fn default() -> Self {
        Self {
            tunnel_name: "Unknown Tunnel".to_owned(),
            depth: 0,
            pickaxe_tier: 0,
            prestige_level: 0,
            luminosity: 100,
            streak_days: 0,
            total_digs: 0,
            total_jc_earned: 0,
            max_depth: 0,
            miner_about: String::new(),
            strength: 0,
            smarts: 0,
            stamina: 0,
            stat_points: 5,
            relics: Vec::new(),
            buffs: Vec::new(),
            mutations: Vec::new(),
            weather: None,
            boss_progress: BTreeMap::new(),
            boss_attempts: 0,
            best_run_score: 0,
            cheer_data: None,
            revenge_until: 0,
            revenge_type: String::new(),
            trap_active: false,
            insured_until: 0,
            reinforced_until: 0,
            injury_state: None,
        }
    }
}

#[must_use]
pub fn build_player_state_context(tunnel: &TunnelFlavorState, balance: i64) -> String {
    let pickaxe_name = usize::try_from(tunnel.pickaxe_tier)
        .ok()
        .and_then(|tier| PICKAXE_TIER_NAMES.get(tier))
        .copied()
        .unwrap_or("Wooden");
    let spent = tunnel.strength + tunnel.smarts + tunnel.stamina;
    let mut lines = vec![
        format!("Tunnel: {}", tunnel.tunnel_name),
        format!("Depth: {} ({})", tunnel.depth, layer_name(tunnel.depth)),
        format!("Pickaxe: {pickaxe_name} (tier {})", tunnel.pickaxe_tier),
        format!("Prestige: {}", tunnel.prestige_level),
        format!("Luminosity: {}/100", tunnel.luminosity),
        format!("Streak: {} days", tunnel.streak_days),
        format!("Total digs: {}", tunnel.total_digs),
        format!("Balance: {balance} JC"),
        format!(
            "S stats: Strength {}, Smarts {}, Stamina {} ({} unspent)",
            tunnel.strength,
            tunnel.smarts,
            tunnel.stamina,
            (tunnel.stat_points - spent).max(0)
        ),
    ];
    if !tunnel.miner_about.is_empty() {
        lines.push(format!("Backstory: {}", tunnel.miner_about));
    }
    if !tunnel.relics.is_empty() {
        lines.push(format!("Relics: {}", tunnel.relics.join(", ")));
    }
    let active_buffs = tunnel
        .buffs
        .iter()
        .filter(|buff| buff.digs_remaining > 0)
        .map(|buff| format!("{} ({} digs left)", buff.name, buff.digs_remaining))
        .collect::<Vec<_>>();
    if !active_buffs.is_empty() {
        lines.push(format!("Buffs: {}", active_buffs.join(", ")));
    }
    if !tunnel.mutations.is_empty() {
        lines.push(format!("Mutations: {}", tunnel.mutations.join(", ")));
    }
    if let Some(weather) = tunnel
        .weather
        .as_ref()
        .filter(|weather| !weather.name.is_empty())
    {
        lines.push(format!(
            "Weather: {} - {}",
            weather.name, weather.description
        ));
    }
    lines.join("\n")
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigPersonality {
    pub play_style: String,
    pub choice_histogram: BTreeMap<String, i64>,
    pub notable_moments: Vec<String>,
    pub social_summary: BTreeMap<String, i64>,
}

impl DigPersonality {
    fn is_blank(&self) -> bool {
        self.play_style.is_empty()
            && self.choice_histogram.is_empty()
            && self.notable_moments.is_empty()
            && self.social_summary.is_empty()
    }
}

fn play_style_description(style: &str) -> &'static str {
    PLAY_STYLE_DESCRIPTIONS
        .iter()
        .find_map(|(key, description)| (*key == style).then_some(*description))
        .unwrap_or(PLAY_STYLE_DESCRIPTIONS[5].1)
}

#[must_use]
pub fn build_personality_context(personality: Option<&DigPersonality>) -> String {
    let Some(personality) = personality.filter(|personality| !personality.is_blank()) else {
        return "New player, no history yet.".to_owned();
    };
    let style = if personality.play_style.is_empty() {
        "unknown"
    } else {
        &personality.play_style
    };
    let mut parts = vec![format!(
        "Play style: {style} ({})",
        play_style_description(style)
    )];
    let histogram = personality
        .choice_histogram
        .iter()
        .filter(|(_, count)| **count != 0)
        .map(|(choice, count)| format!("{choice}: {count}"))
        .collect::<Vec<_>>();
    if !histogram.is_empty() {
        parts.push(format!("Choice history: {}", histogram.join(", ")));
    }
    if !personality.notable_moments.is_empty() {
        let start = personality.notable_moments.len().saturating_sub(5);
        parts.push(format!(
            "Notable moments: {}",
            personality.notable_moments[start..].join(" | ")
        ));
    }
    let social = personality
        .social_summary
        .iter()
        .filter(|(_, count)| **count != 0)
        .map(|(name, count)| format!("{}: {count}", name.replace('_', " ")))
        .collect::<Vec<_>>();
    if !social.is_empty() {
        parts.push(format!("Social: {}", social.join(", ")));
    }
    parts.join("\n")
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CaveInFlavorDetail {
    pub block_loss: i64,
    pub message: String,
    pub kind: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BossFlavorInfo {
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtifactFlavorInfo {
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LuminosityFlavorInfo {
    pub level: String,
    pub current: i64,
    pub drained: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EligibleEvent {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DigFlavorOutcome {
    /// The committed `dig_actions.id` that owns this result.  Production
    /// flavor bonus settlement uses this identity as its idempotency key;
    /// policy-only callers may leave it unset.
    pub dig_action_id: Option<i64>,
    pub success: bool,
    pub dig_consumed: Option<bool>,
    pub boss_pending: bool,
    pub advance: i64,
    pub jc_earned: i64,
    pub depth_before: i64,
    pub depth_after: i64,
    pub cave_in: bool,
    pub cave_in_detail: Option<CaveInFlavorDetail>,
    pub event: Option<EligibleEvent>,
    pub available_events: Vec<EligibleEvent>,
    pub boss_encounter: bool,
    pub boss_info: Option<BossFlavorInfo>,
    pub artifact: Option<ArtifactFlavorInfo>,
    pub luminosity_info: Option<LuminosityFlavorInfo>,
    pub weather: Option<FlavorWeather>,
    pub corruption: Option<String>,
    pub mutations: Vec<String>,
    pub milestone_bonus: i64,
    pub streak_bonus: i64,
    pub nonstreak_jc: Option<i64>,
    pub nonstreak_cap: Option<i64>,
    pub won: bool,
    pub boss_name: String,
    pub boundary: i64,
    pub risk_tier: String,
    pub win_chance: f64,
    pub payout: i64,
    pub jc_delta: i64,
    pub stat_point_awarded: bool,
    pub dialogue: String,
    pub knockback: i64,
    pub new_depth: i64,
    pub phase: i64,
    pub llm_narrative: Option<String>,
    pub llm_tone: Option<String>,
    pub llm_callback: Option<String>,
    pub llm_npc: Option<NpcAppearance>,
    pub llm_picked_event_id: Option<String>,
    pub llm_jc_delta: Option<i64>,
}

#[must_use]
pub fn build_dig_outcome_context(result: &DigFlavorOutcome) -> String {
    let mut lines = Vec::new();
    if result.depth_before != 0 && result.depth_after != 0 {
        lines.push(format!(
            "Depth: {} → {}",
            result.depth_before, result.depth_after
        ));
        let before = layer_name(result.depth_before);
        let after = layer_name(result.depth_after);
        if before != after {
            lines.push(format!("LAYER TRANSITION: {before} → {after}"));
        }
    }
    lines.push(format!(
        "Advance: +{} blocks, JC earned: {}",
        result.advance, result.jc_earned
    ));
    if result.cave_in {
        let detail = result.cave_in_detail.as_ref().cloned().unwrap_or_default();
        let message = if detail.message.is_empty() {
            "Cave-in occurred."
        } else {
            &detail.message
        };
        lines.push(format!(
            "CAVE-IN: lost {} blocks. {message}",
            detail.block_loss
        ));
    }
    if let Some(event) = &result.event {
        let name = if event.name.is_empty() {
            &event.id
        } else {
            &event.name
        };
        lines.push(format!("Event: [{name}] {}", event.description));
    }
    if result.boss_encounter {
        let boss_name = result
            .boss_info
            .as_ref()
            .map_or("Unknown Boss", |boss| boss.name.as_str());
        lines.push(format!(
            "BOSS ENCOUNTER: {boss_name} blocks the path ahead!"
        ));
    }
    if let Some(artifact) = &result.artifact {
        lines.push(format!("Artifact found: {}", artifact.name));
    }
    if result.milestone_bonus != 0 {
        lines.push(format!("Milestone bonus: +{} JC", result.milestone_bonus));
    }
    if result.streak_bonus != 0 {
        lines.push(format!("Streak bonus: +{} JC", result.streak_bonus));
    }
    if let Some(luminosity) = &result.luminosity_info {
        let mut parts = vec![format!(
            "Luminosity: {}/100 ({})",
            luminosity.current, luminosity.level
        )];
        if luminosity.drained != 0 {
            parts.push(format!("drained {}", luminosity.drained));
        }
        lines.push(parts.join(", "));
    }
    if let Some(weather) = &result.weather {
        lines.push(format!(
            "Weather: {} - {}",
            weather.name, weather.description
        ));
    }
    if let Some(corruption) = &result.corruption {
        lines.push(format!("Corruption: {corruption}"));
    }
    if !result.mutations.is_empty() {
        lines.push(format!("Active mutations: {}", result.mutations.join(", ")));
    }
    lines.join("\n")
}

#[must_use]
pub fn build_boss_outcome_context(result: &DigFlavorOutcome) -> String {
    let boss_name = if result.boss_name.is_empty() {
        "Unknown Boss"
    } else {
        &result.boss_name
    };
    let risk = if result.risk_tier.is_empty() {
        "cautious"
    } else {
        &result.risk_tier
    };
    let mut lines = vec![
        format!("Boss: {boss_name} (depth {})", result.boundary),
        format!("Result: {}", if result.won { "VICTORY" } else { "DEFEAT" }),
        format!(
            "Risk: {risk} ({}% chance)",
            (result.win_chance * 100.0) as i64
        ),
    ];
    if result.won {
        let payout = if result.payout != 0 {
            result.payout
        } else if result.jc_delta != 0 {
            result.jc_delta
        } else {
            result.jc_earned
        };
        lines.push(format!("JC earned: +{payout}"));
        if result.stat_point_awarded {
            lines.push("FIRST CLEAR BONUS: +1 S stat point!".to_owned());
        }
        if !result.dialogue.is_empty() {
            lines.push(format!("Boss final words: \"{}\"", result.dialogue));
        }
    } else {
        let jc_lost = result.jc_delta.saturating_abs();
        if jc_lost != 0 {
            lines.push(format!("JC lost: -{jc_lost}"));
        }
        if result.knockback != 0 {
            lines.push(format!("Knocked back: {} blocks", result.knockback));
        }
        lines.push(format!("New depth: {}", result.new_depth));
    }
    if result.phase == 2 {
        lines.push("This was the PHASE 2 (harder) form of the boss!".to_owned());
    }
    lines.join("\n")
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SocialActionDetail {
    pub damage: i64,
    pub advance: i64,
    pub target_id: Option<i64>,
    pub cave_in: bool,
    pub block_loss: i64,
    pub event_id: Option<String>,
    pub won: Option<bool>,
    pub risk: Option<String>,
    pub boundary: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigAction {
    pub guild_id: i64,
    pub actor_id: i64,
    pub target_id: Option<i64>,
    pub action_type: String,
    pub depth_before: i64,
    pub depth_after: i64,
    pub jc_delta: i64,
    pub detail: SocialActionDetail,
    pub created_at: i64,
}

#[must_use]
pub fn build_multiplayer_context(actions: &[DigAction], names: &BTreeMap<i64, String>) -> String {
    build_multiplayer_context_with_state(actions, &TunnelFlavorState::default(), 0, names)
}

#[must_use]
pub fn build_multiplayer_context_with_state(
    actions: &[DigAction],
    tunnel: &TunnelFlavorState,
    rank: i64,
    names: &BTreeMap<i64, String>,
) -> String {
    if actions.is_empty()
        && tunnel.cheer_data.is_none()
        && tunnel.revenge_until <= unix_now()
        && !tunnel.trap_active
        && tunnel.insured_until <= unix_now()
        && tunnel.reinforced_until <= unix_now()
        && tunnel.injury_state.is_none()
        && rank <= 0
    {
        return String::new();
    }
    let name = |id: Option<i64>| {
        id.and_then(|id| names.get(&id).cloned())
            .unwrap_or_else(|| "someone".to_owned())
    };
    let mut sabotage_received = Vec::new();
    let mut sabotage_dealt = Vec::new();
    let mut help_received = Vec::new();
    let mut help_given = Vec::new();
    let mut other = Vec::<(String, usize)>::new();
    for action in actions {
        match action.action_type.as_str() {
            "sabotage" => {
                if let Some(target) = action.detail.target_id {
                    sabotage_dealt.push(format!(
                        "-{} to {}",
                        action.detail.damage,
                        name(Some(target))
                    ));
                } else {
                    sabotage_received.push(format!(
                        "-{} from {}",
                        action.detail.damage,
                        name(Some(action.actor_id))
                    ));
                }
            }
            "help" => {
                if let Some(target) = action.detail.target_id {
                    help_given.push(format!(
                        "+{} to {}",
                        action.detail.advance,
                        name(Some(target))
                    ));
                } else {
                    help_received.push(format!(
                        "+{} from {}",
                        action.detail.advance,
                        name(Some(action.actor_id))
                    ));
                }
            }
            action_type => {
                if let Some((_, count)) = other.iter_mut().find(|(kind, _)| kind == action_type) {
                    *count += 1;
                } else {
                    other.push((action_type.to_owned(), 1));
                }
            }
        }
    }
    let mut lines = Vec::new();
    if !sabotage_received.is_empty() || !sabotage_dealt.is_empty() {
        let mut parts = Vec::new();
        if !sabotage_received.is_empty() {
            parts.push(format!("hit by: {}", sabotage_received.join(", ")));
        }
        if !sabotage_dealt.is_empty() {
            parts.push(format!("dealt: {}", sabotage_dealt.join(", ")));
        }
        lines.push(format!("Sabotage: {}", parts.join("; ")));
    }
    if !help_received.is_empty() || !help_given.is_empty() {
        let mut parts = Vec::new();
        if !help_received.is_empty() {
            parts.push(format!("received: {}", help_received.join(", ")));
        }
        if !help_given.is_empty() {
            parts.push(format!("gave: {}", help_given.join(", ")));
        }
        lines.push(format!("Help: {}", parts.join("; ")));
    }
    lines.extend(other.into_iter().map(|(kind, count)| {
        let mut chars = kind.chars();
        let capitalized = chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().collect::<String>()
        }) + chars.as_str();
        format!("{capitalized}: {count}")
    }));
    let now = unix_now();
    if let Some(raw) = tunnel.cheer_data.as_deref() {
        let active = parse_json_array(raw)
            .into_iter()
            .filter(|entry| {
                entry
                    .get("expires_at")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0)
                    > now
            })
            .collect::<Vec<_>>();
        if !active.is_empty() {
            let boost = (active.len() * 5).min(15);
            let cheerer_names = active
                .iter()
                .map(|entry| name(entry.get("cheerer_id").and_then(serde_json::Value::as_i64)))
                .collect::<Vec<_>>();
            lines.push(format!(
                "Cheers: {} (+{boost}% boss boost, +{} advance)",
                cheerer_names.join(", "),
                active.len()
            ));
        }
    }
    if tunnel.revenge_until > now {
        lines.push(format!(
            "Revenge: {} boost active ({}h left)",
            if tunnel.revenge_type.is_empty() {
                "unknown"
            } else {
                &tunnel.revenge_type
            },
            ((tunnel.revenge_until - now) / 3_600).max(1)
        ));
    }
    if tunnel.trap_active {
        lines.push("Trap: armed".to_owned());
    }
    let mut protections = Vec::new();
    if tunnel.insured_until > now {
        protections.push("insured");
    }
    if tunnel.reinforced_until > now {
        protections.push("reinforced");
    }
    if !protections.is_empty() {
        lines.push(format!("Protection: {}", protections.join(", ")));
    }
    if let Some(raw) = tunnel.injury_state.as_deref()
        && let Some(injury) = parse_json_object(raw)
    {
        let remaining = injury
            .get("digs_remaining")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        if remaining > 0 {
            let injury_type = injury
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("injury");
            lines.push(format!(
                "Injury: {injury_type} ({remaining} digs remaining)"
            ));
        }
    }
    if rank > 0 {
        lines.push(format!("Rank: #{rank} in guild"));
    }
    lines.join("\n")
}

#[must_use]
pub fn build_dig_history_context(
    recent_actions: &[DigAction],
    tunnel: &TunnelFlavorState,
) -> String {
    if recent_actions.is_empty() && *tunnel == TunnelFlavorState::default() {
        return String::new();
    }
    let digs = recent_actions
        .iter()
        .filter(|action| action.action_type == "dig")
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    if !digs.is_empty() {
        let cave_ins = digs.iter().filter(|action| action.detail.cave_in).count();
        let last_cave_in = digs
            .iter()
            .enumerate()
            .find(|(_, action)| action.detail.cave_in);
        let last_cave_in_loss = last_cave_in.map_or(0, |(_, action)| action.detail.block_loss);
        let last_cave_in_ago = last_cave_in.map(|(index, _)| index + 1);
        let mut parts = vec![format!(
            "{} advances, {cave_ins} cave-ins",
            digs.len() - cave_ins
        )];
        if let Some(ago) = last_cave_in_ago {
            parts.push(format!(
                "last cave-in: {ago} digs ago, -{last_cave_in_loss} blocks"
            ));
        }
        lines.push(format!("Last {} digs: {}", digs.len(), parts.join(", ")));
        let oldest = digs.last().expect("non-empty digs");
        let newest = digs.first().expect("non-empty digs");
        let from = oldest.depth_before;
        let to = if newest.depth_after != 0 {
            newest.depth_after
        } else {
            newest.depth_before
        };
        let delta = to - from;
        let mut trend = format!(
            "Depth trend: {from}->{to} ({}{delta})",
            if delta >= 0 { "+" } else { "" }
        );
        if tunnel.max_depth != 0 {
            trend.push_str(&format!(" | Record: {}", tunnel.max_depth));
        }
        lines.push(trend);
    }
    if tunnel.total_digs != 0 || tunnel.total_jc_earned != 0 {
        lines.push(format!(
            "Lifetime: {} digs, {} JC earned",
            tunnel.total_digs, tunnel.total_jc_earned
        ));
    }
    if !tunnel.boss_progress.is_empty() {
        let mut boss_parts = Vec::new();
        let mut attempts = tunnel.boss_attempts;
        for boundary in crate::dig_bosses::BOSS_BOUNDARIES {
            let status = tunnel
                .boss_progress
                .get(&boundary.to_string())
                .map(String::as_str)
                .unwrap_or("active");
            let name = short_boss_name(i64::from(boundary));
            match status {
                "defeated" => boss_parts.push(format!("{name} ✓")),
                "phase1_defeated" => boss_parts.push(format!("{name} (phase 2)")),
                _ if attempts != 0 => {
                    boss_parts.push(format!("{name} ({attempts} attempts)"));
                    attempts = 0;
                }
                _ => {}
            }
        }
        if !boss_parts.is_empty() {
            lines.push(format!("Bosses: {}", boss_parts.join(", ")));
        }
    }
    if tunnel.prestige_level != 0 {
        lines.push(format!(
            "Prestige: level {}, best run score {}",
            tunnel.prestige_level, tunnel.best_run_score
        ));
    }
    let event_ids = recent_actions
        .iter()
        .filter(|action| action.action_type == "event")
        .filter_map(|action| action.detail.event_id.clone())
        .fold(Vec::<String>::new(), |mut ids, id| {
            if !ids.contains(&id) {
                ids.push(id);
            }
            ids
        });
    if !event_ids.is_empty() {
        lines.push(format!(
            "Recent events: {}",
            event_ids.into_iter().take(5).collect::<Vec<_>>().join(", ")
        ));
    }
    let boss_fights = recent_actions
        .iter()
        .filter(|action| action.action_type == "boss_fight")
        .take(3)
        .map(|action| {
            let outcome = if action.detail.won == Some(true) {
                "won"
            } else {
                "lost"
            };
            let boundary = action.detail.boundary.unwrap_or(0);
            let risk = action.detail.risk.as_deref().unwrap_or("?");
            format!("{outcome} vs {} ({risk})", short_boss_name(boundary))
        })
        .collect::<Vec<_>>();
    if !boss_fights.is_empty() {
        lines.push(format!("Boss fights: {}", boss_fights.join(", ")));
    }
    lines.join("\n")
}

fn short_boss_name(boundary: i64) -> String {
    crate::dig_bosses::full_boss_pool_for_tier(boundary as i32)
        .first()
        .map(|boss| {
            boss.name
                .strip_prefix("The ")
                .unwrap_or(boss.name)
                .to_owned()
        })
        .unwrap_or_else(|| format!("boss@{boundary}"))
}

fn parse_json_array(raw: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
}

fn parse_json_object(raw: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigFlavorRequest {
    pub messages: Vec<LlmMessage>,
    pub tool: ToolDefinition,
    pub max_tokens: i64,
    pub feature: String,
    pub timeout_millis: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OptionalMessageSections<'a> {
    pub multiplayer: &'a str,
    pub history: &'a str,
    pub dm_context: &'a str,
    pub eligible_events: &'a str,
}

#[must_use]
pub fn build_messages(
    system_prompt: &str,
    player_state: &str,
    personality: &str,
    outcome: &str,
    optional: OptionalMessageSections<'_>,
) -> Vec<LlmMessage> {
    let mut sections = vec![
        format!("=== PLAYER STATE ===\n{player_state}"),
        format!("=== PERSONALITY ===\n{personality}"),
    ];
    if !optional.dm_context.is_empty() {
        sections.push(format!("=== DM CONTEXT ===\n{}", optional.dm_context));
    }
    if !optional.history.is_empty() {
        sections.push(format!("=== DIG HISTORY ===\n{}", optional.history));
    }
    sections.push(format!("=== DIG OUTCOME ===\n{outcome}"));
    if !optional.eligible_events.is_empty() {
        sections.push(format!(
            "=== ELIGIBLE EVENTS ===\n{}",
            optional.eligible_events
        ));
    }
    if !optional.multiplayer.is_empty() {
        sections.push(format!("=== MULTIPLAYER ===\n{}", optional.multiplayer));
    }
    vec![
        LlmMessage {
            role: "system",
            content: system_prompt.to_owned(),
        },
        LlmMessage {
            role: "user",
            content: sections.join("\n\n"),
        },
    ]
}

#[derive(Clone, Debug, PartialEq)]
pub struct DigToolCallResult {
    pub tool_name: String,
    pub tool_args: FlavorToolArgs,
}

pub trait DigFlavorAiPort: Send + Sync {
    fn call_with_tools(&self, request: DigFlavorRequest) -> Result<DigToolCallResult, String>;
}

/// Production bridge from the Dig-specific request shape to the shared
/// provider-neutral [`AIService`].  The deterministic Dig service owns all
/// validation; this bridge only serializes messages/tools and decodes the
/// provider's typed JSON value tree.
#[derive(Clone)]
pub struct AIServiceDigFlavorAiPort {
    ai: Arc<AIService>,
}

impl AIServiceDigFlavorAiPort {
    #[must_use]
    pub fn new(ai: Arc<AIService>) -> Self {
        Self { ai }
    }

    #[must_use]
    pub fn ai(&self) -> &Arc<AIService> {
        &self.ai
    }
}

impl DigFlavorAiPort for AIServiceDigFlavorAiPort {
    fn call_with_tools(&self, request: DigFlavorRequest) -> Result<DigToolCallResult, String> {
        let timeout = Duration::from_millis(request.timeout_millis);
        let tool = match request.tool.name {
            NARRATE_DIG_TOOL_NAME => narrate_dig_ai_tool_definition(),
            NARRATE_SPLASH_TOOL_NAME => splash_narration_ai_tool_definition(),
            other => {
                return Err(format!("unsupported Dig flavor tool {other}"));
            }
        };
        let messages = request
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    "system" => MessageRole::System,
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "tool" => MessageRole::Tool,
                    _ => MessageRole::User,
                };
                Message::new(role, message.content)
            })
            .collect();
        let result = self.ai.call_with_tools_with_timeout(
            ToolRequest {
                messages,
                tools: vec![tool],
                tool_choice: AiToolChoice::Required(request.tool.name.to_owned()),
                max_tokens: u32::try_from(request.max_tokens.max(0)).ok(),
                temperature: None,
                feature: request.feature,
            },
            timeout,
        );
        decode_ai_tool_call(result)
    }
}

fn decode_ai_tool_call(result: AiToolCallResult) -> Result<DigToolCallResult, String> {
    let Some(tool_name) = result.tool_name else {
        return Err("provider returned no Dig flavor tool call".to_owned());
    };
    Ok(DigToolCallResult {
        tool_name,
        tool_args: flavor_tool_args_from_values(&result.tool_args),
    })
}

fn value_text(values: &BTreeMap<String, AiValue>, key: &str) -> Option<String> {
    values.get(key).and_then(AiValue::as_str).map(str::to_owned)
}

fn value_object<'a>(
    values: &'a BTreeMap<String, AiValue>,
    key: &str,
) -> Option<&'a BTreeMap<String, AiValue>> {
    match values.get(key) {
        Some(AiValue::Object(value)) => Some(value),
        _ => None,
    }
}

fn flavor_tool_args_from_values(values: &BTreeMap<String, AiValue>) -> FlavorToolArgs {
    let npc_appearance = value_object(values, "npc_appearance").and_then(|npc| {
        Some(NpcAppearance {
            id_or_name: value_text(npc, "id_or_name")?,
            line: value_text(npc, "line")?,
        })
    });
    FlavorToolArgs {
        narrative: value_text(values, "narrative"),
        tone: value_text(values, "tone"),
        callback_reference: value_text(values, "callback_reference"),
        picked_event_id: value_text(values, "picked_event_id"),
        npc_appearance,
        flavor_bonus_pct: values.get("flavor_bonus_pct").and_then(AiValue::as_f64),
        memory_update: value_text(values, "memory_update"),
    }
}

pub trait DigFlavorConfigPort: Send + Sync {
    fn ai_enabled(&self, guild_id: i64) -> Result<bool, String>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigDmContext {
    pub memory_blob: String,
    pub last_match_summary: String,
    pub economy_state: String,
    pub server_activity: String,
    pub npc_roster: Vec<String>,
}

impl DigDmContext {
    #[must_use]
    pub fn render(&self) -> String {
        // Keep this text byte-for-byte shaped like Python's
        // `render_dm_context_section`: the labels are prompt boundaries and
        // roster entries are already bullet-prefixed by the canonical NPC
        // policy module.
        let mut lines = Vec::new();
        if self.memory_blob.trim().is_empty() {
            lines.push("NARRATIVE MEMORY: empty (no prior notes on this player).".to_owned());
        } else {
            lines.push("NARRATIVE MEMORY (your previous notes on this player):".to_owned());
            lines.push(self.memory_blob.trim().to_owned());
        }
        lines.push(format!(
            "LAST INHOUSE MATCH: {}",
            if self.last_match_summary.is_empty() {
                "No recent inhouse matches."
            } else {
                &self.last_match_summary
            }
        ));
        lines.push(format!(
            "ECONOMY: {}",
            if self.economy_state.is_empty() {
                "No outstanding loan."
            } else {
                &self.economy_state
            }
        ));
        lines.push(format!(
            "SERVER ACTIVITY: {}",
            if self.server_activity.is_empty() {
                "Server is quiet — no other miners and no boss kills in the last day."
            } else {
                &self.server_activity
            }
        ));
        lines.push("NPC ROSTER (canon — pick from these or invent if none fit):".to_owned());
        lines.extend(self.npc_roster.iter().cloned());
        lines.join("\n")
    }
}

pub trait DigFlavorContextPort: Send + Sync {
    fn build(&self, discord_id: i64, guild_id: i64) -> Result<DigDmContext, String>;
}

pub trait DigFlavorRng: Send {
    fn choose_tone(&mut self, tone_count: usize) -> usize;
    fn sample(&mut self) -> f64;
}

#[derive(Clone, Debug)]
pub struct SeededDigFlavorRng {
    state: u64,
}

impl SeededDigFlavorRng {
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
}

impl DigFlavorRng for SeededDigFlavorRng {
    fn choose_tone(&mut self, tone_count: usize) -> usize {
        usize::try_from(self.next()).unwrap_or(0) % tone_count.max(1)
    }

    fn sample(&mut self) -> f64 {
        self.next() as f64 / u64::MAX as f64
    }
}

impl FlameEntropy for SeededDigFlavorRng {
    fn index(&mut self, upper_exclusive: usize) -> usize {
        assert!(upper_exclusive > 0, "cannot pick from an empty flame pool");
        self.choose_tone(upper_exclusive)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GatheredFlavorData {
    pub tunnel: Option<TunnelFlavorState>,
    pub balance: i64,
    pub personality: Option<DigPersonality>,
    pub social_actions: Vec<DigAction>,
    pub recent_actions: Vec<DigAction>,
    pub player_rank: i64,
    pub names: BTreeMap<i64, String>,
}

/// The intentionally narrow read set needed by splash narration.  Unlike a
/// full flavor gather this contains no balance, personality, action history,
/// rank, or DM context; production adapters can therefore resolve display
/// names and the digger's layer with two small guild-scoped queries.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GatheredSplashFlavorData {
    pub digger_name: Option<String>,
    pub digger_depth: i64,
    pub names: BTreeMap<i64, String>,
}

pub trait DigFlavorDataPort: Send + Sync {
    fn gather(&self, discord_id: i64, guild_id: i64) -> Result<GatheredFlavorData, String>;

    /// Read only the splash name/depth projection.  The default keeps test
    /// and transitional adapters source-compatible; production adapters must
    /// override it so `narrate_splash` does not fetch the full flavor bundle.
    fn gather_splash(
        &self,
        digger_id: i64,
        guild_id: i64,
        victim_ids: &[i64],
    ) -> Result<GatheredSplashFlavorData, String> {
        let gathered = self.gather(digger_id, guild_id)?;
        let digger_name = gathered.names.get(&digger_id).cloned();
        let names = victim_ids
            .iter()
            .filter_map(|id| gathered.names.get(id).cloned().map(|name| (*id, name)))
            .collect();
        Ok(GatheredSplashFlavorData {
            digger_name,
            digger_depth: gathered.tunnel.as_ref().map_or(0, |tunnel| tunnel.depth),
            names,
        })
    }

    fn add_balance(&self, discord_id: i64, guild_id: i64, delta: i64) -> Result<(), String>;

    /// Apply a flavor nudge against the committed Dig action.  The default is
    /// intentionally compatible with the in-memory parity adapter; a
    /// production implementation must atomically claim `dig_action_id` and
    /// write the central economy ledger before changing the wallet.
    fn apply_flavor_bonus(
        &self,
        _dig_action_id: Option<i64>,
        discord_id: i64,
        guild_id: i64,
        delta: i64,
    ) -> Result<FlavorBonusReceipt, String> {
        self.add_balance(discord_id, guild_id, delta)?;
        Ok(FlavorBonusReceipt { delta })
    }

    /// Recover a previously committed post-commit flavor phase before any
    /// new AI request.  Transitional adapters return no receipt; the SQLite
    /// adapter reads the typed marker keyed by `dig_action_id`.
    fn get_flavor_receipt(
        &self,
        _dig_action_id: i64,
        _discord_id: i64,
        _guild_id: i64,
    ) -> Result<Option<FlavorDeliveryReceipt>, String> {
        Ok(None)
    }

    /// Atomically persist validated prose, optional memory, and the actual
    /// bonus receipt.  The default preserves old in-memory test adapters; the
    /// production SQLite implementation overrides it with one DB transaction.
    fn apply_flavor_receipt(
        &self,
        receipt: FlavorDeliveryReceipt,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<FlavorDeliveryReceipt, String> {
        let bonus = if receipt.state == FlavorDeliveryState::Applied {
            self.apply_flavor_bonus(
                Some(receipt.source_action_id),
                discord_id,
                guild_id,
                receipt.bonus.delta,
            )?
        } else {
            FlavorBonusReceipt::default()
        };
        if let Some(memory_update) = receipt.memory_update.as_deref() {
            self.set_dm_memory(discord_id, guild_id, memory_update)?;
        }
        Ok(FlavorDeliveryReceipt { bonus, ..receipt })
    }

    fn set_dm_memory(&self, discord_id: i64, guild_id: i64, value: &str) -> Result<(), String>;
    fn get_personality(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigPersonality>, String>;
    fn upsert_personality(
        &self,
        discord_id: i64,
        guild_id: i64,
        personality: DigPersonality,
    ) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FlavorBonusReceipt {
    /// The delta that is actually represented by the durable ledger row.  On
    /// an idempotent replay this may differ from the newly sampled request.
    pub delta: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlavorDeliveryState {
    Applied,
    Skipped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlavorDeliveryNpcAppearance {
    pub id_or_name: String,
    pub line: String,
}

/// Full durable flavor phase result used by the provider's post-commit
/// outbox.  `bonus.delta` is always the persisted wallet delta, including on
/// an idempotent replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlavorDeliveryReceipt {
    pub source_action_id: i64,
    pub state: FlavorDeliveryState,
    pub narrative: Option<String>,
    pub tone: Option<String>,
    pub callback_reference: Option<String>,
    pub npc_appearance: Option<FlavorDeliveryNpcAppearance>,
    pub picked_event_id: Option<String>,
    pub memory_update: Option<String>,
    pub bonus: FlavorBonusReceipt,
}

/// Production data port over the already-migrated Python SQLite schema.
///
/// The repository owns the action-identity transaction for the optional JC
/// nudge.  In particular, `add_balance` intentionally refuses a context-free
/// mutation; callers must use `DigFlavorOutcome::dig_action_id` through
/// `apply_flavor_bonus` so a retried narration cannot mint a second bonus.
#[derive(Clone, Debug)]
pub struct SqliteDigFlavorDataAdapter {
    repository: Arc<cama_db::dig_flavor_repository::DigFlavorRepository>,
}

impl SqliteDigFlavorDataAdapter {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self::from_repository(Arc::new(
            cama_db::dig_flavor_repository::DigFlavorRepository::new(path),
        ))
    }

    #[must_use]
    pub fn from_repository(
        repository: Arc<cama_db::dig_flavor_repository::DigFlavorRepository>,
    ) -> Self {
        Self { repository }
    }

    #[must_use]
    pub fn repository(&self) -> &Arc<cama_db::dig_flavor_repository::DigFlavorRepository> {
        &self.repository
    }
}

impl DigFlavorDataPort for SqliteDigFlavorDataAdapter {
    fn gather(&self, discord_id: i64, guild_id: i64) -> Result<GatheredFlavorData, String> {
        self.repository
            .gather(discord_id, guild_id, unix_now())
            .map(map_db_gathered)
            .map_err(|error| error.to_string())
    }

    fn gather_splash(
        &self,
        digger_id: i64,
        guild_id: i64,
        victim_ids: &[i64],
    ) -> Result<GatheredSplashFlavorData, String> {
        self.repository
            .gather_splash(digger_id, guild_id, victim_ids)
            .map(|gathered| GatheredSplashFlavorData {
                digger_name: gathered.digger_name,
                digger_depth: gathered.digger_depth,
                names: gathered.names,
            })
            .map_err(|error| error.to_string())
    }

    fn add_balance(&self, _discord_id: i64, _guild_id: i64, _delta: i64) -> Result<(), String> {
        Err(
            "production Dig flavor balance mutations require a committed dig action identity"
                .to_owned(),
        )
    }

    fn apply_flavor_bonus(
        &self,
        dig_action_id: Option<i64>,
        discord_id: i64,
        guild_id: i64,
        delta: i64,
    ) -> Result<FlavorBonusReceipt, String> {
        // A duplicate claim is a successful idempotent replay.  The service
        // can therefore reconstruct the same flavor result without minting a
        // second wallet or ledger row.
        self.repository
            .claim_flavor_bonus(dig_action_id, discord_id, guild_id, delta, unix_now())
            .map(|claim| FlavorBonusReceipt { delta: claim.delta })
            .map_err(|error| error.to_string())
    }

    fn get_flavor_receipt(
        &self,
        dig_action_id: i64,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<FlavorDeliveryReceipt>, String> {
        self.repository
            .get_flavor_receipt(dig_action_id, discord_id, guild_id)
            .map(|receipt| receipt.map(map_db_flavor_receipt))
            .map_err(|error| error.to_string())
    }

    fn apply_flavor_receipt(
        &self,
        receipt: FlavorDeliveryReceipt,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<FlavorDeliveryReceipt, String> {
        self.repository
            .apply_flavor_receipt(
                to_db_flavor_receipt(&receipt),
                discord_id,
                guild_id,
                unix_now(),
            )
            .map(map_db_flavor_receipt)
            .map_err(|error| error.to_string())
    }

    fn set_dm_memory(&self, discord_id: i64, guild_id: i64, value: &str) -> Result<(), String> {
        self.repository
            .set_dm_memory(discord_id, guild_id, value, unix_now())
            .map_err(|error| error.to_string())
    }

    fn get_personality(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigPersonality>, String> {
        self.repository
            .get_personality(discord_id, guild_id)
            .map(|personality| personality.map(map_db_personality))
            .map_err(|error| error.to_string())
    }

    fn upsert_personality(
        &self,
        discord_id: i64,
        guild_id: i64,
        personality: DigPersonality,
    ) -> Result<(), String> {
        self.repository
            .upsert_personality(
                discord_id,
                guild_id,
                &cama_db::dig_flavor_repository::DigFlavorPersonality {
                    play_style: personality.play_style,
                    choice_histogram: personality.choice_histogram,
                    notable_moments: personality.notable_moments,
                    social_summary: personality.social_summary,
                },
                unix_now(),
            )
            .map_err(|error| error.to_string())
    }
}

/// SQLite implementation of the guild AI feature switch used by flavor.
#[derive(Clone, Debug)]
pub struct SqliteDigFlavorConfigAdapter {
    repository: Arc<cama_db::guild_config_repository::GuildConfigRepository>,
}

impl SqliteDigFlavorConfigAdapter {
    #[must_use]
    pub fn new(path: impl AsRef<Path>, ai_features_default: bool) -> Self {
        Self {
            repository: Arc::new(
                cama_db::guild_config_repository::GuildConfigRepository::new(
                    path,
                    ai_features_default,
                ),
            ),
        }
    }

    #[must_use]
    pub fn from_repository(
        repository: Arc<cama_db::guild_config_repository::GuildConfigRepository>,
    ) -> Self {
        Self { repository }
    }
}

impl DigFlavorConfigPort for SqliteDigFlavorConfigAdapter {
    fn ai_enabled(&self, guild_id: i64) -> Result<bool, String> {
        GuildConfigStore::get_ai_enabled(&*self.repository, guild_id)
            .map_err(|error| error.to_string())
    }
}

/// Read-only production DM context adapter.  The constructor receives the
/// canonical, bullet-prefixed NPC roster from the app policy owner so this
/// SQLite bridge does not duplicate or silently drift prompt data.
#[derive(Clone, Debug)]
pub struct SqliteDigFlavorContextAdapter {
    flavor: Arc<cama_db::dig_flavor_repository::DigFlavorRepository>,
    loans: Arc<cama_db::loan_repository::LoanRepository>,
    npc_roster: Vec<String>,
}

impl SqliteDigFlavorContextAdapter {
    #[must_use]
    pub fn new(path: impl AsRef<Path>, npc_roster: Vec<String>) -> Self {
        Self::from_repositories(
            Arc::new(cama_db::dig_flavor_repository::DigFlavorRepository::new(
                path.as_ref(),
            )),
            Arc::new(cama_db::loan_repository::LoanRepository::new(path)),
            npc_roster,
        )
    }

    #[must_use]
    pub fn from_repositories(
        flavor: Arc<cama_db::dig_flavor_repository::DigFlavorRepository>,
        loans: Arc<cama_db::loan_repository::LoanRepository>,
        npc_roster: Vec<String>,
    ) -> Self {
        Self {
            flavor,
            loans,
            npc_roster,
        }
    }
}

impl DigFlavorContextPort for SqliteDigFlavorContextAdapter {
    fn build(&self, discord_id: i64, guild_id: i64) -> Result<DigDmContext, String> {
        let memory_blob = self
            .flavor
            .get_dm_memory(discord_id, guild_id)
            .map_err(|error| error.to_string())?;
        let last_match_summary = self
            .flavor
            .recent_match_summary(discord_id, guild_id)
            .map_err(|error| error.to_string())?;
        let loan_state = self
            .loans
            .get_state(discord_id, Some(guild_id))
            .map_err(|error| error.to_string())?;
        let recent_diggers = self
            .flavor
            .recent_diggers(
                guild_id,
                unix_now().saturating_sub(86_400),
                Some(discord_id),
            )
            .map_err(|error| error.to_string())?;
        let recent_boss_kills = self
            .flavor
            .recent_boss_kills(guild_id, unix_now().saturating_sub(86_400))
            .map_err(|error| error.to_string())?;
        Ok(DigDmContext {
            memory_blob,
            last_match_summary,
            economy_state: summarize_loan_state(loan_state),
            server_activity: summarize_server_activity(recent_diggers.len(), recent_boss_kills),
            npc_roster: self.npc_roster.clone(),
        })
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_secs().try_into().unwrap_or(i64::MAX)
        })
}

fn map_db_flavor_receipt(
    receipt: cama_db::dig_flavor_repository::DigFlavorReceipt,
) -> FlavorDeliveryReceipt {
    FlavorDeliveryReceipt {
        source_action_id: receipt.source_action_id,
        state: match receipt.state {
            cama_db::dig_flavor_repository::DigFlavorReceiptState::Applied => {
                FlavorDeliveryState::Applied
            }
            cama_db::dig_flavor_repository::DigFlavorReceiptState::Skipped => {
                FlavorDeliveryState::Skipped
            }
        },
        narrative: receipt.narrative,
        tone: receipt.tone,
        callback_reference: receipt.callback_reference,
        npc_appearance: receipt
            .npc_appearance
            .map(|npc| FlavorDeliveryNpcAppearance {
                id_or_name: npc.id_or_name,
                line: npc.line,
            }),
        picked_event_id: receipt.picked_event_id,
        memory_update: receipt.memory_update,
        bonus: FlavorBonusReceipt {
            delta: receipt.bonus_delta,
        },
    }
}

fn to_db_flavor_receipt(
    receipt: &FlavorDeliveryReceipt,
) -> cama_db::dig_flavor_repository::DigFlavorReceipt {
    cama_db::dig_flavor_repository::DigFlavorReceipt {
        source_action_id: receipt.source_action_id,
        state: match receipt.state {
            FlavorDeliveryState::Applied => {
                cama_db::dig_flavor_repository::DigFlavorReceiptState::Applied
            }
            FlavorDeliveryState::Skipped => {
                cama_db::dig_flavor_repository::DigFlavorReceiptState::Skipped
            }
        },
        narrative: receipt.narrative.clone(),
        tone: receipt.tone.clone(),
        callback_reference: receipt.callback_reference.clone(),
        npc_appearance: receipt.npc_appearance.as_ref().map(|npc| {
            cama_db::dig_flavor_repository::DigFlavorNpcAppearance {
                id_or_name: npc.id_or_name.clone(),
                line: npc.line.clone(),
            }
        }),
        picked_event_id: receipt.picked_event_id.clone(),
        memory_update: receipt.memory_update.clone(),
        bonus_delta: receipt.bonus.delta,
    }
}

fn map_db_gathered(
    gathered: cama_db::dig_flavor_repository::DigFlavorGathered,
) -> GatheredFlavorData {
    GatheredFlavorData {
        tunnel: gathered.tunnel.map(map_db_tunnel),
        balance: gathered.balance,
        personality: gathered.personality.map(map_db_personality),
        social_actions: gathered
            .social_actions
            .into_iter()
            .map(map_db_action)
            .collect(),
        recent_actions: gathered
            .recent_actions
            .into_iter()
            .map(map_db_action)
            .collect(),
        player_rank: gathered.player_rank,
        names: gathered.names,
    }
}

fn map_db_tunnel(tunnel: cama_db::dig_flavor_repository::DigFlavorTunnel) -> TunnelFlavorState {
    TunnelFlavorState {
        tunnel_name: tunnel.tunnel_name,
        depth: tunnel.depth,
        pickaxe_tier: tunnel.pickaxe_tier,
        prestige_level: tunnel.prestige_level,
        luminosity: tunnel.luminosity,
        streak_days: tunnel.streak_days,
        total_digs: tunnel.total_digs,
        total_jc_earned: tunnel.total_jc_earned,
        max_depth: tunnel.max_depth,
        miner_about: tunnel.miner_about,
        strength: tunnel.stat_strength,
        smarts: tunnel.stat_smarts,
        stamina: tunnel.stat_stamina,
        stat_points: tunnel.stat_points,
        relics: tunnel.equipped_relics,
        buffs: tunnel
            .temp_buffs
            .into_iter()
            .map(|buff| FlavorBuff {
                name: buff.name,
                digs_remaining: buff.digs_remaining,
            })
            .collect(),
        mutations: tunnel.mutations,
        weather: tunnel
            .weather_name
            .zip(tunnel.weather_description)
            .map(|(name, description)| FlavorWeather { name, description }),
        boss_progress: tunnel.boss_progress,
        boss_attempts: tunnel.boss_attempts,
        best_run_score: tunnel.best_run_score,
        cheer_data: tunnel.cheer_data,
        revenge_until: tunnel.revenge_until,
        revenge_type: if tunnel.revenge_type.is_empty() {
            "None".to_owned()
        } else {
            tunnel.revenge_type
        },
        trap_active: tunnel.trap_active,
        insured_until: tunnel.insured_until,
        reinforced_until: tunnel.reinforced_until,
        injury_state: tunnel.injury_state,
    }
}

fn map_db_personality(
    personality: cama_db::dig_flavor_repository::DigFlavorPersonality,
) -> DigPersonality {
    DigPersonality {
        play_style: personality.play_style,
        choice_histogram: personality.choice_histogram,
        notable_moments: personality.notable_moments,
        social_summary: personality.social_summary,
    }
}

fn map_db_action(action: cama_db::dig_flavor_repository::DigFlavorAction) -> DigAction {
    DigAction {
        guild_id: action.guild_id,
        actor_id: action.actor_id,
        target_id: action.target_id,
        action_type: action.action_type,
        depth_before: action.depth_before,
        depth_after: action.depth_after,
        jc_delta: action.jc_delta,
        detail: SocialActionDetail {
            damage: action.damage,
            advance: action.advance,
            target_id: action.detail_target_id,
            cave_in: action.cave_in,
            block_loss: action.block_loss,
            event_id: action.event_id,
            won: action.won,
            risk: action.risk,
            boundary: action.boundary,
        },
        created_at: action.created_at,
    }
}

fn summarize_loan_state(state: Option<cama_db::loan_repository::LoanStateRecord>) -> String {
    let Some(state) = state else {
        return "No outstanding loan.".to_owned();
    };
    let total = state
        .outstanding_principal
        .saturating_add(state.outstanding_fee);
    if total <= 0 {
        "No outstanding loan.".to_owned()
    } else {
        format!("Outstanding loan: owes the bank a debt of {total}.")
    }
}

fn summarize_server_activity(recent_diggers: usize, recent_boss_kills: i64) -> String {
    if recent_diggers == 0 && recent_boss_kills == 0 {
        return "Server is quiet — no other miners and no boss kills in the last day.".to_owned();
    }
    let mut parts = Vec::new();
    match recent_diggers {
        0 => {}
        1 => parts.push("1 other miner has dug in the last day".to_owned()),
        count => parts.push(format!("{count} other miners have dug in the last day")),
    }
    match recent_boss_kills {
        0 => {}
        1 => parts.push("1 boss has fallen recently".to_owned()),
        count => parts.push(format!("{count} bosses have fallen recently")),
    }
    format!("{}.", parts.join("; "))
}

pub struct DigFlavorService {
    ai: Arc<dyn DigFlavorAiPort>,
    data: Arc<dyn DigFlavorDataPort>,
    context: Arc<dyn DigFlavorContextPort>,
    config: Option<Arc<dyn DigFlavorConfigPort>>,
    rng: Mutex<Box<dyn DigFlavorRng>>,
}

fn apply_delivery_receipt_to_result(
    result: &mut DigFlavorOutcome,
    receipt: &FlavorDeliveryReceipt,
) {
    result.llm_narrative = None;
    result.llm_tone = None;
    result.llm_callback = None;
    result.llm_npc = None;
    result.llm_picked_event_id = None;
    result.llm_jc_delta = None;
    if receipt.state != FlavorDeliveryState::Applied {
        return;
    }
    result.llm_narrative.clone_from(&receipt.narrative);
    result.llm_tone.clone_from(&receipt.tone);
    result.llm_callback.clone_from(&receipt.callback_reference);
    result.llm_npc = receipt.npc_appearance.as_ref().map(|npc| NpcAppearance {
        id_or_name: npc.id_or_name.clone(),
        line: npc.line.clone(),
    });
    result
        .llm_picked_event_id
        .clone_from(&receipt.picked_event_id);
    if receipt.bonus.delta != 0 {
        result.llm_jc_delta = Some(receipt.bonus.delta);
    }
}

fn delivery_skip_receipt(source_action_id: i64) -> FlavorDeliveryReceipt {
    FlavorDeliveryReceipt {
        source_action_id,
        state: FlavorDeliveryState::Skipped,
        narrative: None,
        tone: None,
        callback_reference: None,
        npc_appearance: None,
        picked_event_id: None,
        memory_update: None,
        bonus: FlavorBonusReceipt::default(),
    }
}

impl DigFlavorService {
    #[must_use]
    pub fn new(
        ai: Arc<dyn DigFlavorAiPort>,
        data: Arc<dyn DigFlavorDataPort>,
        context: Arc<dyn DigFlavorContextPort>,
        rng: Box<dyn DigFlavorRng>,
    ) -> Self {
        Self {
            ai,
            data,
            context,
            config: None,
            rng: Mutex::new(rng),
        }
    }

    #[must_use]
    pub fn with_config(mut self, config: Arc<dyn DigFlavorConfigPort>) -> Self {
        self.config = Some(config);
        self
    }

    fn persist_flavor_skip(&self, result: &DigFlavorOutcome, discord_id: i64, guild_id: i64) {
        let Some(source_action_id) = result.dig_action_id else {
            return;
        };
        let _ignored = self.data.apply_flavor_receipt(
            delivery_skip_receipt(source_action_id),
            discord_id,
            guild_id,
        );
    }

    /// Generate optional prose for an already-committed cross-player splash.
    ///
    /// The operation is deliberately fail-soft: configuration, lookup, and AI
    /// failures all return an empty string so deterministic victim lines remain
    /// available to the caller.
    #[must_use]
    pub fn narrate_splash(&self, input: SplashNarrationInput<'_>) -> String {
        if input.victims.is_empty() {
            return String::new();
        }
        if self
            .config
            .as_ref()
            .is_some_and(|config| !matches!(config.ai_enabled(input.guild_id), Ok(true)))
        {
            return String::new();
        }
        let victim_ids = input
            .victims
            .iter()
            .map(|victim| victim.discord_id)
            .collect::<Vec<_>>();
        let Ok(gathered) = self
            .data
            .gather_splash(input.digger_id, input.guild_id, &victim_ids)
        else {
            return String::new();
        };
        let digger_name = gathered.digger_name.as_deref().unwrap_or("the digger");
        let digger_layer = input.digger_layer.map_or_else(
            || layer_name(gathered.digger_depth).to_owned(),
            str::to_owned,
        );
        let victims = input
            .victims
            .iter()
            .map(|victim| NamedSplashVictim {
                name: gathered
                    .names
                    .get(&victim.discord_id)
                    .cloned()
                    .unwrap_or_else(|| "a stranger".to_owned()),
                amount: victim.amount,
            })
            .collect::<Vec<_>>();
        let request = DigFlavorRequest {
            messages: build_splash_narration_messages(
                digger_name,
                &digger_layer,
                input.event_name,
                input.event_description,
                input.splash_mode,
                &victims,
            ),
            tool: SPLASH_NARRATION_TOOL,
            max_tokens: 200,
            feature: "dig.splash".to_owned(),
            timeout_millis: 3_000,
        };
        let Ok(call) = self.ai.call_with_tools(request) else {
            return String::new();
        };
        if call.tool_name != NARRATE_SPLASH_TOOL_NAME {
            return String::new();
        }
        validate_splash_narrative(call.tool_args.narrative.as_deref().unwrap_or_default())
    }

    pub fn flavor(
        &self,
        result: &mut DigFlavorOutcome,
        discord_id: i64,
        guild_id: i64,
        is_boss: bool,
    ) {
        // A committed receipt is authoritative.  Recover it before checking
        // the feature switch or gathering prompt context so a crash/retry
        // cannot issue a second AI request or sample a different bonus.
        if let Some(source_action_id) = result.dig_action_id {
            match self
                .data
                .get_flavor_receipt(source_action_id, discord_id, guild_id)
            {
                Ok(Some(receipt)) => {
                    apply_delivery_receipt_to_result(result, &receipt);
                    return;
                }
                Ok(None) => {}
                Err(_) => return,
            }
        }
        if !is_boss && result.dig_consumed == Some(false) {
            self.persist_flavor_skip(result, discord_id, guild_id);
            return;
        }
        if self
            .config
            .as_ref()
            .is_some_and(|config| !matches!(config.ai_enabled(guild_id), Ok(true)))
        {
            self.persist_flavor_skip(result, discord_id, guild_id);
            return;
        }
        let Ok(mut gathered) = self.data.gather(discord_id, guild_id) else {
            self.persist_flavor_skip(result, discord_id, guild_id);
            return;
        };
        let Ok(dm_context) = self.context.build(discord_id, guild_id) else {
            self.persist_flavor_skip(result, discord_id, guild_id);
            return;
        };
        let (tone, bonus_cap) = {
            let mut rng = self
                .rng
                .lock()
                .expect("Dig flavor RNG mutex is not poisoned");
            let tone = TONE_PROFILES[rng.choose_tone(TONE_PROFILES.len()) % TONE_PROFILES.len()].0;
            let cap = if rng.sample() < BONUS_CAP_BIG_CHANCE {
                BONUS_CAP_BIG_PCT
            } else {
                BONUS_CAP_SMALL_PCT
            };
            (tone, cap)
        };
        let tunnel = gathered
            .tunnel
            .get_or_insert_with(TunnelFlavorState::default);
        tunnel.depth = result.depth_after;
        let eligible_ids = result
            .event
            .iter()
            .chain(&result.available_events)
            .filter(|event| !event.id.is_empty())
            .map(|event| event.id.clone())
            .collect::<BTreeSet<_>>();
        let eligible = eligible_ids.iter().cloned().collect::<Vec<_>>().join(", ");
        let outcome_context = if is_boss {
            build_boss_outcome_context(result)
        } else {
            build_dig_outcome_context(result)
        };
        let multiplayer_context = build_multiplayer_context_with_state(
            &gathered.social_actions,
            tunnel,
            gathered.player_rank,
            &gathered.names,
        );
        let history_context = build_dig_history_context(&gathered.recent_actions, tunnel);
        let rendered_dm_context = dm_context.render();
        let messages = build_messages(
            &build_dig_flavor_system_prompt(tone, bonus_cap),
            &build_player_state_context(tunnel, gathered.balance),
            &build_personality_context(gathered.personality.as_ref()),
            &outcome_context,
            OptionalMessageSections {
                multiplayer: &multiplayer_context,
                history: &history_context,
                dm_context: &rendered_dm_context,
                eligible_events: &eligible,
            },
        );
        let request = DigFlavorRequest {
            messages,
            tool: NARRATE_DIG_TOOL,
            max_tokens: 600,
            feature: if is_boss {
                "dig.boss".to_owned()
            } else {
                "dig.flavor".to_owned()
            },
            timeout_millis: 5_000,
        };
        let Ok(call) = self.ai.call_with_tools(request) else {
            self.persist_flavor_skip(result, discord_id, guild_id);
            return;
        };
        if call.tool_name != NARRATE_DIG_TOOL_NAME {
            self.persist_flavor_skip(result, discord_id, guild_id);
            return;
        }
        let Some(validated) = validate_flavor_args(&call.tool_args, &eligible_ids, tone, bonus_cap)
        else {
            self.persist_flavor_skip(result, discord_id, guild_id);
            return;
        };
        if self
            .apply_validated(result, &validated, discord_id, guild_id, is_boss)
            .is_err()
        {
            self.persist_flavor_skip(result, discord_id, guild_id);
        }
    }

    fn apply_validated(
        &self,
        result: &mut DigFlavorOutcome,
        validated: &FlavorResult,
        discord_id: i64,
        guild_id: i64,
        is_boss: bool,
    ) -> Result<(), String> {
        let delta = Self::flavor_bonus_delta(result, validated.flavor_bonus_pct, is_boss);
        if let Some(source_action_id) = result.dig_action_id {
            let receipt = FlavorDeliveryReceipt {
                source_action_id,
                state: FlavorDeliveryState::Applied,
                narrative: Some(validated.narrative.clone()),
                tone: Some(validated.tone.clone()),
                callback_reference: (!validated.callback_reference.is_empty())
                    .then(|| validated.callback_reference.clone()),
                npc_appearance: validated.npc_appearance.as_ref().map(|npc| {
                    FlavorDeliveryNpcAppearance {
                        id_or_name: npc.id_or_name.clone(),
                        line: npc.line.clone(),
                    }
                }),
                picked_event_id: validated.picked_event_id.clone(),
                memory_update: validated.memory_update.clone(),
                bonus: FlavorBonusReceipt { delta },
            };
            let persisted = self
                .data
                .apply_flavor_receipt(receipt, discord_id, guild_id)?;
            apply_delivery_receipt_to_result(result, &persisted);
            return Ok(());
        }

        result.llm_narrative = Some(validated.narrative.clone());
        result.llm_tone = Some(validated.tone.clone());
        if !validated.callback_reference.is_empty() {
            result.llm_callback = Some(validated.callback_reference.clone());
        }
        result.llm_npc.clone_from(&validated.npc_appearance);
        result
            .llm_picked_event_id
            .clone_from(&validated.picked_event_id);
        if delta != 0
            && let Ok(receipt) = self
                .data
                .apply_flavor_bonus(None, discord_id, guild_id, delta)
            && receipt.delta != 0
        {
            result.llm_jc_delta = Some(receipt.delta);
        }
        if let Some(memory) = &validated.memory_update {
            let _ignored = self.data.set_dm_memory(discord_id, guild_id, memory);
        }
        Ok(())
    }

    fn flavor_bonus_delta(result: &DigFlavorOutcome, flavor_bonus_pct: f64, is_boss: bool) -> i64 {
        if result.jc_earned == 0 || flavor_bonus_pct.abs() <= f64::EPSILON {
            return 0;
        }
        let mut delta = python_round(result.jc_earned as f64 * flavor_bonus_pct / 100.0);
        if delta > 0 && !is_boss {
            let nonstreak = result
                .nonstreak_jc
                .unwrap_or(result.jc_earned - result.milestone_bonus - result.streak_bonus);
            let cap = result.nonstreak_cap.unwrap_or(BASE_DIG_JC_PAYOUT_CAP);
            delta = delta.min((cap - nonstreak).max(0));
        }
        delta
    }

    pub fn update_personality(
        &self,
        discord_id: i64,
        guild_id: i64,
        choice: Option<&str>,
        details: Option<&BTreeMap<String, bool>>,
    ) {
        let Ok(existing) = self.data.get_personality(discord_id, guild_id) else {
            return;
        };
        let mut personality = existing.unwrap_or_else(|| DigPersonality {
            play_style: "unknown".to_owned(),
            ..DigPersonality::default()
        });
        const VALID_CHOICES: [&str; 7] = [
            "safe",
            "risky",
            "desperate",
            "fight",
            "retreat",
            "scout",
            "help",
        ];
        if let Some(choice) = choice.filter(|choice| VALID_CHOICES.contains(choice)) {
            *personality
                .choice_histogram
                .entry(choice.to_owned())
                .or_default() += 1;
        }
        if let Some(moment) = details.and_then(extract_notable_moment) {
            personality.notable_moments.push(moment.to_owned());
            let drain = personality.notable_moments.len().saturating_sub(10);
            personality.notable_moments.drain(..drain);
        }
        personality.play_style = classify_play_style(&personality.choice_histogram).to_owned();
        let _ignored = self
            .data
            .upsert_personality(discord_id, guild_id, personality);
    }
}

fn python_round(value: f64) -> i64 {
    let floor = value.floor();
    let fraction = value - floor;
    if (fraction - 0.5).abs() <= f64::EPSILON {
        let floor = floor as i64;
        if floor % 2 == 0 { floor } else { floor + 1 }
    } else {
        value.round() as i64
    }
}

fn extract_notable_moment(details: &BTreeMap<String, bool>) -> Option<&'static str> {
    [
        ("first_boss_kill", "Slew their first boss"),
        ("prestige", "Ascended to a new prestige level"),
        ("artifact_found", "Discovered a rare artifact"),
        ("cave_in_streak", "Suffered a devastating cave-in streak"),
    ]
    .into_iter()
    .find_map(|(key, description)| {
        details
            .get(key)
            .copied()
            .unwrap_or(false)
            .then_some(description)
    })
}

#[derive(Default)]
struct MemoryRepositoryState {
    balances: BTreeMap<(i64, i64), i64>,
    names: BTreeMap<(i64, i64), String>,
    tunnels: BTreeMap<(i64, i64), TunnelFlavorState>,
    memories: BTreeMap<(i64, i64), String>,
    personalities: BTreeMap<(i64, i64), DigPersonality>,
    actions: Vec<DigAction>,
}

/// Reference in-memory adapter used by parity tests and local prompt previews.
/// Production cutover still requires a `rusqlite` adapter over the Python
/// schema; this type intentionally never opens the shared database.
#[derive(Default)]
pub struct MemoryDigFlavorRepository {
    state: Mutex<MemoryRepositoryState>,
}

impl MemoryDigFlavorRepository {
    pub fn register_player(&self, discord_id: i64, guild_id: i64, name: &str, balance: i64) {
        let mut state = self
            .state
            .lock()
            .expect("memory repository is not poisoned");
        state.balances.insert((discord_id, guild_id), balance);
        state.names.insert((discord_id, guild_id), name.to_owned());
    }

    pub fn create_tunnel(&self, discord_id: i64, guild_id: i64, name: &str) {
        self.state
            .lock()
            .expect("memory repository is not poisoned")
            .tunnels
            .insert(
                (discord_id, guild_id),
                TunnelFlavorState {
                    tunnel_name: name.to_owned(),
                    ..TunnelFlavorState::default()
                },
            );
    }

    #[must_use]
    pub fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
        *self
            .state
            .lock()
            .expect("memory repository is not poisoned")
            .balances
            .get(&(discord_id, guild_id))
            .unwrap_or(&0)
    }

    #[must_use]
    pub fn dm_memory(&self, discord_id: i64, guild_id: i64) -> String {
        self.state
            .lock()
            .expect("memory repository is not poisoned")
            .memories
            .get(&(discord_id, guild_id))
            .cloned()
            .unwrap_or_default()
    }

    pub fn write_dm_memory(&self, discord_id: i64, guild_id: i64, value: &str) {
        let value = truncate_utf8_bytes(value, MEMORY_MAX_BYTES);
        self.state
            .lock()
            .expect("memory repository is not poisoned")
            .memories
            .insert((discord_id, guild_id), value);
    }

    #[must_use]
    pub fn personality(&self, discord_id: i64, guild_id: i64) -> Option<DigPersonality> {
        self.state
            .lock()
            .expect("memory repository is not poisoned")
            .personalities
            .get(&(discord_id, guild_id))
            .cloned()
    }

    pub fn write_personality(&self, discord_id: i64, guild_id: i64, personality: DigPersonality) {
        self.state
            .lock()
            .expect("memory repository is not poisoned")
            .personalities
            .insert((discord_id, guild_id), personality);
    }

    pub fn log_action(&self, action: DigAction) {
        self.state
            .lock()
            .expect("memory repository is not poisoned")
            .actions
            .push(action);
    }

    #[must_use]
    pub fn recent_social_actions(&self, discord_id: i64, guild_id: i64) -> Vec<DigAction> {
        self.state
            .lock()
            .expect("memory repository is not poisoned")
            .actions
            .iter()
            .filter(|action| {
                action.guild_id == guild_id
                    && (action.actor_id == discord_id || action.target_id == Some(discord_id))
                    && matches!(action.action_type.as_str(), "sabotage" | "help" | "cheer")
            })
            .take(20)
            .cloned()
            .collect()
    }
}

impl DigFlavorDataPort for MemoryDigFlavorRepository {
    fn gather(&self, discord_id: i64, guild_id: i64) -> Result<GatheredFlavorData, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        let names = state
            .names
            .iter()
            .filter(|((_, action_guild), _)| *action_guild == guild_id)
            .map(|((id, _), name)| (*id, name.clone()))
            .collect();
        let social_actions = state
            .actions
            .iter()
            .filter(|action| {
                action.guild_id == guild_id
                    && (action.actor_id == discord_id || action.target_id == Some(discord_id))
                    && matches!(action.action_type.as_str(), "sabotage" | "help" | "cheer")
            })
            .cloned()
            .collect();
        let recent_actions = state
            .actions
            .iter()
            .filter(|action| {
                action.guild_id == guild_id
                    && (action.actor_id == discord_id || action.target_id == Some(discord_id))
            })
            .cloned()
            .collect();
        Ok(GatheredFlavorData {
            tunnel: state.tunnels.get(&(discord_id, guild_id)).cloned(),
            balance: *state.balances.get(&(discord_id, guild_id)).unwrap_or(&0),
            personality: state.personalities.get(&(discord_id, guild_id)).cloned(),
            social_actions,
            recent_actions,
            player_rank: 0,
            names,
        })
    }

    fn gather_splash(
        &self,
        digger_id: i64,
        guild_id: i64,
        victim_ids: &[i64],
    ) -> Result<GatheredSplashFlavorData, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        let digger_name = state.names.get(&(digger_id, guild_id)).cloned();
        let names = victim_ids
            .iter()
            .filter_map(|id| {
                state
                    .names
                    .get(&(*id, guild_id))
                    .cloned()
                    .map(|name| (*id, name))
            })
            .collect();
        Ok(GatheredSplashFlavorData {
            digger_name,
            digger_depth: state
                .tunnels
                .get(&(digger_id, guild_id))
                .map_or(0, |tunnel| tunnel.depth),
            names,
        })
    }

    fn add_balance(&self, discord_id: i64, guild_id: i64, delta: i64) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        *state.balances.entry((discord_id, guild_id)).or_default() += delta;
        Ok(())
    }

    fn set_dm_memory(&self, discord_id: i64, guild_id: i64, value: &str) -> Result<(), String> {
        self.write_dm_memory(discord_id, guild_id, value);
        Ok(())
    }

    fn get_personality(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigPersonality>, String> {
        Ok(self.personality(discord_id, guild_id))
    }

    fn upsert_personality(
        &self,
        discord_id: i64,
        guild_id: i64,
        personality: DigPersonality,
    ) -> Result<(), String> {
        self.write_personality(discord_id, guild_id, personality);
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigTerminalFailure {
    pub success: bool,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigPreconditions {
    pub depth_before: i64,
}

#[must_use]
pub fn dig_with_preconditions(
    tunnel: Option<&TunnelState>,
) -> (Option<DigTerminalFailure>, Option<DigPreconditions>) {
    tunnel.map_or_else(
        || {
            (
                Some(DigTerminalFailure {
                    success: false,
                    message: "You need to register first.".to_owned(),
                }),
                None,
            )
        },
        |tunnel| {
            (
                None,
                Some(DigPreconditions {
                    depth_before: tunnel.depth,
                }),
            )
        },
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedDigFlavorOutcome {
    pub success: bool,
    pub advance: i64,
    pub depth_after: i64,
    pub cave_in: bool,
    pub cave_in_detail: Option<CaveInFlavorDetail>,
    pub llm_narrative: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedDigFlavorInput {
    pub advance: i64,
    pub jc_earned: i64,
    pub cave_in: bool,
    pub cave_in_block_loss: i64,
    pub cave_in_type: String,
}

#[must_use]
pub fn apply_resolved_dig_outcome(
    tunnel: &mut TunnelState,
    input: &ResolvedDigFlavorInput,
    now: i64,
) -> AppliedDigFlavorOutcome {
    let resolved = apply_dig_outcome(
        tunnel,
        DigOutcomeInput {
            advance: input.advance,
            gross_jc: input.jc_earned,
            cave_in_loss: if input.cave_in {
                input.cave_in_block_loss
            } else {
                0
            },
            ..DigOutcomeInput::default()
        },
        now,
    );
    AppliedDigFlavorOutcome {
        success: true,
        advance: resolved.advance,
        depth_after: resolved.depth_after,
        cave_in: resolved.cave_in,
        cave_in_detail: resolved.cave_in.then(|| CaveInFlavorDetail {
            block_loss: input.cave_in_block_loss,
            message: String::new(),
            kind: input.cave_in_type.clone(),
        }),
        llm_narrative: None,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod splash_narration_tests;
