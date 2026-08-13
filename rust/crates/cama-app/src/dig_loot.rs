//! Additive Rust policy for Dig loot, events, relic gifts, and presentation.
//!
//! The Python bot remains the live Discord and SQLite implementation. This
//! module keeps the migration boundary explicit: authored catalogs and policy
//! are pure, entropy is injected, and persistence is supplied through a typed
//! repository port. The in-memory adapter below is used for parity tests; a
//! Python-compatible SQLite adapter can implement the same port without
//! changing command or outcome behavior.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::OnceLock;

use cama_domain::dig_gear::{Artifact, unique_gear};
use cama_domain::economy_scaling::{scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta};
use cama_domain::embed_safety::EmbedModel;
use serde::{Deserialize, Serialize};

use crate::dig_service::{AutoBuyStatus, layer_at};

pub const MAX_INVENTORY_SLOTS: usize = 8;
pub const HARD_HAT_USES: i64 = 3;
pub const GRAPPLING_HOOK_USES: i64 = 5;
pub const REINFORCEMENT_SECONDS: i64 = 48 * 3_600;
/// Consumables reserved for a boss encounter.  A queued row is deliberately
/// left in the inventory until the boss runtime consumes it.
pub const BOSS_PREP_ITEM_IDS: [&str; 3] = ["tempered_whetstone", "warding_salts", "rescue_line"];
pub const PRESTIGE_RELIC_DROP_RATE: f64 = 0.10;
pub const RARITY_WEIGHTS: [(Rarity, u64); 4] = [
    (Rarity::Common, 70),
    (Rarity::Uncommon, 20),
    (Rarity::Rare, 15),
    (Rarity::Legendary, 10),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConsumableDef {
    pub id: &'static str,
    pub name: &'static str,
    pub cost: i64,
    pub bonus_blocks: i64,
}

pub const CONSUMABLES: [ConsumableDef; 13] = [
    ConsumableDef {
        id: "dynamite",
        name: "Dynamite",
        cost: 5,
        bonus_blocks: 5,
    },
    ConsumableDef {
        id: "hard_hat",
        name: "Hard Hat",
        cost: 8,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "lantern",
        name: "Lantern",
        cost: 4,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "reinforcement",
        name: "Reinforcement",
        cost: 6,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "torch",
        name: "Torch",
        cost: 6,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "grappling_hook",
        name: "Grappling Hook",
        cost: 10,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "sonar_pulse",
        name: "Sonar Pulse",
        cost: 8,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "depth_charge",
        name: "Depth Charge",
        cost: 15,
        bonus_blocks: 10,
    },
    ConsumableDef {
        id: "void_bait",
        name: "Void Bait",
        cost: 20,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "streak_charm",
        name: "Streak Charm",
        cost: 15,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "tempered_whetstone",
        name: "Tempered Whetstone",
        cost: 60,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "warding_salts",
        name: "Warding Salts",
        cost: 50,
        bonus_blocks: 0,
    },
    ConsumableDef {
        id: "rescue_line",
        name: "Rescue Line",
        cost: 40,
        bonus_blocks: 0,
    },
];

#[must_use]
pub fn consumable(item_id: &str) -> Option<&'static ConsumableDef> {
    CONSUMABLES.iter().find(|item| item.id == item_id)
}

#[must_use]
pub fn is_dig_consumable(item_id: &str) -> bool {
    matches!(
        item_id,
        "dynamite"
            | "hard_hat"
            | "lantern"
            | "torch"
            | "grappling_hook"
            | "depth_charge"
            | "reinforcement"
            | "sonar_pulse"
            | "void_bait"
    )
}

#[must_use]
pub fn is_boss_prep_item(item_id: &str) -> bool {
    BOSS_PREP_ITEM_IDS.contains(&item_id)
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Legendary,
}

impl Rarity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::Uncommon => "uncommon",
            Self::Rare => "rare",
            Self::Legendary => "legendary",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "uncommon" => Self::Uncommon,
            "rare" => Self::Rare,
            "legendary" => Self::Legendary,
            _ => Self::Common,
        }
    }

    #[must_use]
    pub fn weight(self) -> u64 {
        RARITY_WEIGHTS
            .iter()
            .find_map(|(rarity, weight)| (*rarity == self).then_some(*weight))
            .expect("every rarity has a weight")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventComplexity {
    Simple,
    Choice,
    Complex,
    Boon,
}

impl EventComplexity {
    fn parse(value: &str) -> Self {
        match value {
            "simple" => Self::Simple,
            "complex" => Self::Complex,
            "boon" => Self::Boon,
            _ => Self::Choice,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventDef {
    pub id: &'static str,
    pub min_depth: i64,
    pub max_depth: Option<i64>,
    pub layer: Option<&'static str>,
    pub rarity: Rarity,
    pub requires_dark: bool,
    pub min_prestige: i64,
    pub complexity: EventComplexity,
    pub has_safe_option: bool,
    pub has_desperate_option: bool,
    pub boon_count: usize,
    pub chain_only: bool,
    pub quest_event: bool,
}

/// The complete authored event policy carried by the Python catalog.
///
/// `EventDef` above intentionally remains the small, copyable eligibility
/// projection used by the dig-roll hot path.  This richer representation is
/// the application-facing policy boundary for event presentation and
/// settlement.  It is parsed from the checked-in Python catalog at compile
/// time (`include_str!`) and never reaches across the Discord or repository
/// layers.  Keeping the source text in the Rust artifact means a catalog
/// change cannot silently leave a stale hand-copied Rust table behind.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CanonicalEventDef {
    pub id: String,
    pub name: String,
    #[serde(rename = "description")]
    pub descriptions: Vec<String>,
    pub min_depth: Option<i64>,
    pub max_depth: Option<i64>,
    pub layer: Option<String>,
    #[serde(deserialize_with = "deserialize_rarity")]
    pub rarity: Rarity,
    #[serde(deserialize_with = "deserialize_event_complexity")]
    pub complexity: EventComplexity,
    pub safe_option: Option<CanonicalEventChoice>,
    pub risky_option: Option<CanonicalEventChoice>,
    pub desperate_option: Option<CanonicalEventChoice>,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub steps: Vec<CanonicalEventStep>,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub boon_options: Vec<CanonicalTempBuff>,
    pub buff_on_success: Option<CanonicalTempBuff>,
    pub requires_dark: bool,
    pub social: bool,
    pub ascii_art: Option<String>,
    pub splash: Option<CanonicalSplash>,
    pub guild_modifier_on_success: Option<serde_json::Value>,
    pub min_prestige: i64,
    pub next_event_id: Option<String>,
    pub chain_only: bool,
    pub quest_id: Option<String>,
    pub quest_step: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CanonicalEventStep {
    #[serde(rename = "description")]
    pub descriptions: Vec<String>,
    #[serde(default)]
    pub choices: Vec<CanonicalEventChoice>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CanonicalEventChoice {
    pub label: String,
    pub success: CanonicalEventOutcome,
    pub failure: Option<CanonicalEventOutcome>,
    pub success_chance: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CanonicalEventOutcome {
    pub description: String,
    pub advance: i64,
    pub jc: i64,
    pub cave_in: bool,
    pub streak_loss: i64,
    pub curse: Option<CanonicalTempCurse>,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub gear_reward_pool: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub consumable_reward_pool: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_null_vec")]
    pub artifact_reward_pool: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct CanonicalTempBuff {
    pub id: String,
    pub name: String,
    pub duration_digs: i64,
    pub effect: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct CanonicalTempCurse {
    pub id: String,
    pub name: String,
    pub duration_digs: i64,
    pub effect: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CanonicalSplash {
    pub strategy: String,
    pub victim_count: usize,
    pub penalty_jc: i64,
    pub trigger: String,
    pub mode: String,
}

/// A reward selected from an authored outcome after ownership and capacity
/// filters have been applied.  The event catalog intentionally stores pools,
/// not a pre-selected item: Python makes this choice at resolution time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CanonicalReward {
    Gear(String),
    Consumable(String),
    Artifact(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CanonicalRewardSelection {
    pub reward: Option<CanonicalReward>,
    pub duplicate_gear_reward: bool,
    pub eligible_gear: Vec<String>,
    pub eligible_consumables: Vec<String>,
    pub eligible_artifacts: Vec<String>,
}

/// Conditional draws made after Python selects an authored event outcome.
/// The runtime consumes these in order without duplicating choice policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CanonicalEventRandomPlan {
    pub jc_jitter: bool,
    pub advance_jitter: bool,
}

/// Presentation data for all four authored event UI modes.  Keeping the
/// complete description/step/boon payload here lets the application render a
/// simple, choice, complex, or boon event without a Python callback.
#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalEventPresentation {
    pub event_id: String,
    pub event_name: String,
    pub complexity: EventComplexity,
    pub descriptions: Vec<String>,
    pub steps: Vec<CanonicalEventStep>,
    pub boon_options: Vec<CanonicalTempBuff>,
    pub ascii_art: Option<String>,
    pub social: bool,
}

/// A deterministic, application-level event result.  Repository adapters
/// can apply the deltas in one transaction, while Discord adapters can render
/// the exact authored message and reward payload without reparsing Python.
#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalEventResolution {
    pub event_id: String,
    pub event_name: String,
    pub choice: String,
    pub complexity: EventComplexity,
    pub descriptions: Vec<String>,
    pub steps: Vec<CanonicalEventStep>,
    pub boon_options: Vec<CanonicalTempBuff>,
    pub ascii_art: Option<String>,
    pub social: bool,
    pub succeeded: bool,
    pub message: String,
    pub advance: i64,
    pub jc: i64,
    pub cave_in: bool,
    pub streak_loss: i64,
    pub streak_days_after: Option<i64>,
    pub curse: Option<CanonicalTempCurse>,
    /// Strengthened/extended curse payload written to the tunnel.  `curse`
    /// remains the authored payload used for the player-facing message.
    pub persisted_curse: Option<CanonicalTempCurse>,
    pub buff: Option<CanonicalTempBuff>,
    pub black_wax_seal_spent: bool,
    pub active_curse_remaining_after: Option<i64>,
    pub curse_cleared: bool,
    pub gear_reward_pool: Vec<String>,
    pub consumable_reward_pool: Vec<String>,
    pub artifact_reward_pool: Vec<String>,
    pub splash: Option<CanonicalSplash>,
    pub guild_modifier_on_success: Option<serde_json::Value>,
    pub quest_id: Option<String>,
    pub quest_step: Option<i64>,
    pub next_event_id: Option<String>,
    /// The actual follow-up selected by the generic chain policy.  The
    /// authored `next_event_id` remains exposed separately for UI/audit.
    pub chained_event_id: Option<String>,
    pub reward: Option<CanonicalReward>,
    pub duplicate_gear_reward: bool,
    pub splash_payout_ratio: Option<f64>,
    pub economy_gross_jc: i64,
    pub cruel_echoes: bool,
    pub boss_encounter: bool,
    pub random_plan: CanonicalEventRandomPlan,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct CanonicalQuestWire {
    quest_id: String,
    name: String,
    starter_prereq: CanonicalQuestPrerequisite,
    step_event_ids: Vec<String>,
    finale_kind: String,
    finale_payload: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CanonicalQuestPrerequisite {
    pub min_depth: i64,
    pub min_prestige: i64,
    pub system_predicate: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CanonicalQuestFinale {
    JcPlusGuildModifier {
        personal_jc: i64,
        modifier_id: String,
        duration_seconds: i64,
        modifier_payload: serde_json::Value,
    },
    RelicGrant {
        relic_base: String,
        relic_suffix: String,
        stat_pool: Vec<String>,
        roll_count: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalQuestDef {
    pub quest_id: String,
    pub name: String,
    pub starter_prereq: CanonicalQuestPrerequisite,
    pub step_event_ids: Vec<String>,
    pub finale: CanonicalQuestFinale,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CanonicalQuestProgress {
    Noop,
    Activated {
        quest_id: String,
        next_step: i64,
    },
    Advanced {
        quest_id: String,
        next_step: i64,
    },
    Completed {
        quest_id: String,
        finale: CanonicalQuestFinale,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum CanonicalQuestFinaleOutcome {
    JcPlusGuildModifier {
        personal_jc: i64,
        modifier_id: String,
        duration_seconds: i64,
        modifier_payload: serde_json::Value,
    },
    RelicGrant {
        relic_name: String,
        artifact_id: String,
        relic_stat_ids: Vec<String>,
    },
}

/// Stable FNV-1a checksum used by the parity harness to ensure the checked-in
/// generated snapshot is regenerated whenever the Python catalog changes.
#[must_use]
pub fn canonical_event_snapshot_hash() -> u64 {
    CANONICAL_EVENT_JSON
        .bytes()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
}

/// Resolve one authored event choice without touching persistence.  The
/// caller supplies the already-drawn success roll; all repository updates can
/// then be committed together with the returned deltas.
pub fn resolve_canonical_event(
    event_id: &str,
    choice: &str,
    success_roll: f64,
) -> Result<CanonicalEventResolution, String> {
    let event = canonical_event(event_id).ok_or_else(|| "Unknown event.".to_owned())?;
    if !success_roll.is_finite() || !(0.0..=1.0).contains(&success_roll) {
        return Err("Invalid event roll.".to_owned());
    }

    if let Some(index) = choice.strip_prefix("boon_") {
        let index = index
            .parse::<usize>()
            .map_err(|_| "Invalid boon selection.".to_owned())?;
        let Some(buff) = event.boon_options.get(index) else {
            return Err("Invalid boon selection.".to_owned());
        };
        return Ok(CanonicalEventResolution {
            event_id: event.id.clone(),
            event_name: event.name.clone(),
            choice: choice.to_owned(),
            complexity: event.complexity,
            descriptions: event.descriptions.clone(),
            steps: event.steps.clone(),
            boon_options: event.boon_options.clone(),
            ascii_art: event.ascii_art.clone(),
            social: event.social,
            succeeded: true,
            message: format!("You chose {}!", buff.name),
            advance: 0,
            jc: 0,
            cave_in: false,
            streak_loss: 0,
            streak_days_after: None,
            curse: None,
            persisted_curse: None,
            buff: Some(buff.clone()),
            black_wax_seal_spent: false,
            active_curse_remaining_after: None,
            curse_cleared: false,
            gear_reward_pool: Vec::new(),
            consumable_reward_pool: Vec::new(),
            artifact_reward_pool: Vec::new(),
            splash: None,
            guild_modifier_on_success: None,
            quest_id: None,
            quest_step: None,
            next_event_id: None,
            chained_event_id: None,
            reward: None,
            duplicate_gear_reward: false,
            splash_payout_ratio: None,
            economy_gross_jc: 0,
            cruel_echoes: false,
            boss_encounter: false,
            random_plan: CanonicalEventRandomPlan::default(),
        });
    }

    let option = match choice {
        "safe" => event.safe_option.as_ref(),
        "risky" => event.risky_option.as_ref(),
        "desperate" => event.desperate_option.as_ref(),
        _ => None,
    }
    .ok_or_else(|| format!("Invalid choice: {choice}"))?;
    let succeeded = success_roll < option.success_chance;
    let outcome = if succeeded {
        &option.success
    } else {
        option.failure.as_ref().unwrap_or(&option.success)
    };
    Ok(CanonicalEventResolution {
        event_id: event.id.clone(),
        event_name: event.name.clone(),
        choice: choice.to_owned(),
        complexity: event.complexity,
        descriptions: event.descriptions.clone(),
        steps: event.steps.clone(),
        boon_options: event.boon_options.clone(),
        ascii_art: event.ascii_art.clone(),
        social: event.social,
        succeeded,
        message: outcome.description.clone(),
        advance: outcome.advance,
        jc: outcome.jc,
        cave_in: outcome.cave_in,
        streak_loss: outcome.streak_loss,
        curse: outcome.curse.clone(),
        persisted_curse: None,
        buff: (succeeded && matches!(choice, "risky" | "desperate"))
            .then(|| event.buff_on_success.clone())
            .flatten(),
        black_wax_seal_spent: false,
        active_curse_remaining_after: None,
        curse_cleared: false,
        gear_reward_pool: outcome.gear_reward_pool.clone(),
        consumable_reward_pool: outcome.consumable_reward_pool.clone(),
        artifact_reward_pool: outcome.artifact_reward_pool.clone(),
        splash: event.splash.clone(),
        guild_modifier_on_success: (succeeded && matches!(choice, "risky" | "desperate"))
            .then(|| event.guild_modifier_on_success.clone())
            .flatten(),
        quest_id: event.quest_id.clone(),
        quest_step: event.quest_step,
        next_event_id: event.next_event_id.clone(),
        chained_event_id: None,
        reward: None,
        duplicate_gear_reward: false,
        splash_payout_ratio: None,
        economy_gross_jc: outcome.jc,
        cruel_echoes: false,
        streak_days_after: None,
        boss_encounter: false,
        random_plan: CanonicalEventRandomPlan {
            jc_jitter: outcome.jc != 0,
            advance_jitter: outcome.advance != 0,
        },
    })
}

pub const CANONICAL_EVENT_NEGATIVE_JC_MULTIPLIER: f64 = 1.30;
pub const CANONICAL_EVENT_CURSE_STRENGTH_MULTIPLIER: f64 = 1.50;
pub const CANONICAL_EVENT_CURSE_DURATION_BONUS_DIGS: i64 = 2;
pub const CANONICAL_EVENT_DEFLATION_MULTIPLIER: f64 = 1.375;
pub const CANONICAL_EVENT_DEPTH_SETBACK_MULTIPLIER: f64 = 1.25;
pub const CANONICAL_EVENT_POSITIVE_JC_MULTIPLIER: f64 = 0.65;
pub const CANONICAL_EVENT_CHAIN_CHANCE: f64 = 0.15;
pub const CANONICAL_EVENT_HARMFUL_WEIGHT_MULTIPLIER: f64 = 1.25;
pub const CANONICAL_LUMINOSITY_BRIGHT: i64 = 76;
pub const CANONICAL_LUMINOSITY_DIM: i64 = 26;
pub const CANONICAL_LUMINOSITY_DARK: i64 = 1;
pub const CANONICAL_LUMINOSITY_DIM_HARMFUL_WEIGHT: f64 = 1.10;
pub const CANONICAL_LUMINOSITY_DARK_HARMFUL_WEIGHT: f64 = 1.25;
pub const CANONICAL_LUMINOSITY_PITCH_HARMFUL_WEIGHT: f64 = 1.50;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanonicalEventRolls {
    pub success_roll: f64,
    /// The separate `random.random()` draw Python uses for a safe option
    /// without a failure branch under Cruel Echoes.  This is `None` for all
    /// other choices, so callers do not consume an RNG draw unnecessarily.
    pub cruel_echo_roll: Option<f64>,
    pub jc_jitter_multiplier: f64,
    pub advance_jitter: i64,
    /// Draw used for the P7+ chain chance.  It is only consumed when the
    /// caller has enabled chaining and the prestige gate is met.
    pub chain_roll: Option<f64>,
    /// Draw used to choose among equally eligible chained events.
    pub chain_selection_roll: Option<f64>,
    /// Draw used to choose one item from an outcome reward pool.
    pub reward_roll: Option<f64>,
}

impl Default for CanonicalEventRolls {
    fn default() -> Self {
        Self {
            success_roll: 0.0,
            cruel_echo_roll: None,
            jc_jitter_multiplier: 1.0,
            advance_jitter: 0,
            chain_roll: None,
            chain_selection_roll: None,
            reward_roll: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanonicalEventPolicy {
    pub luminosity: i64,
    pub current_streak: i64,
    pub current_depth: i64,
    pub prestige_level: i64,
    pub next_boss_boundary: Option<i64>,
    pub active_curse_remaining: Option<i64>,
    pub chained: bool,
    pub diviners_knot: bool,
    pub surveyors_loop: bool,
    pub ruinwager_edge: bool,
    pub black_wax_seal: bool,
    pub active_temp_curse: bool,
    pub cruel_safe_failure: f64,
    pub burning_ledger: bool,
    pub chain_jc_multiplier: f64,
    pub expedition_reward_bonus: f64,
    pub risky_success_bonus: f64,
    pub chipped_compass: bool,
    pub event_chain_enabled: bool,
    pub minigame_jc_delta_scale: f64,
    /// Optional daily-economy adjustment supplied by the application layer.
    /// The canonical default is one, while a real adapter can replace this
    /// pure multiplier with its guild policy before committing the result.
    pub economy_reward_multiplier: f64,
}

impl Default for CanonicalEventPolicy {
    fn default() -> Self {
        Self {
            luminosity: 100,
            current_streak: 0,
            current_depth: 0,
            prestige_level: 0,
            next_boss_boundary: None,
            active_curse_remaining: None,
            chained: false,
            diviners_knot: false,
            surveyors_loop: false,
            ruinwager_edge: false,
            black_wax_seal: false,
            active_temp_curse: false,
            cruel_safe_failure: 0.0,
            burning_ledger: false,
            chain_jc_multiplier: 1.0,
            expedition_reward_bonus: 0.0,
            risky_success_bonus: 0.0,
            chipped_compass: false,
            event_chain_enabled: false,
            minigame_jc_delta_scale: 1.0,
            economy_reward_multiplier: 1.0,
        }
    }
}

/// Resolve the generic event mechanics layered around every authored event.
/// This is deliberately pure: random rolls and the current tunnel/perk state
/// are inputs, while SQLite/economy/splash/quest ports consume the returned
/// typed payload in one settlement transaction.
pub fn resolve_canonical_event_with_policy(
    event_id: &str,
    requested_choice: &str,
    rolls: CanonicalEventRolls,
    policy: CanonicalEventPolicy,
) -> Result<CanonicalEventResolution, String> {
    let event = canonical_event(event_id).ok_or_else(|| "Unknown event.".to_owned())?;
    if !rolls.success_roll.is_finite()
        || !(0.0..=1.0).contains(&rolls.success_roll)
        || !rolls.jc_jitter_multiplier.is_finite()
        || rolls.jc_jitter_multiplier < 0.0
        || rolls
            .cruel_echo_roll
            .is_some_and(|roll| !roll.is_finite() || !(0.0..=1.0).contains(&roll))
        || rolls
            .chain_roll
            .is_some_and(|roll| !roll.is_finite() || !(0.0..=1.0).contains(&roll))
        || rolls
            .chain_selection_roll
            .is_some_and(|roll| !roll.is_finite() || !(0.0..=1.0).contains(&roll))
        || rolls
            .reward_roll
            .is_some_and(|roll| !roll.is_finite() || !(0.0..=1.0).contains(&roll))
        || !policy.minigame_jc_delta_scale.is_finite()
        || policy.minigame_jc_delta_scale < 0.0
        || !policy.economy_reward_multiplier.is_finite()
        || policy.economy_reward_multiplier < 0.0
    {
        return Err("Invalid event roll.".to_owned());
    }
    if requested_choice.starts_with("boon_") {
        return resolve_canonical_event(event_id, requested_choice, 0.0);
    }

    let choice =
        if requested_choice == "safe" && policy.luminosity <= 0 && event.risky_option.is_some() {
            "risky"
        } else {
            requested_choice
        };
    let option = match choice {
        "safe" => event.safe_option.as_ref(),
        "risky" => event.risky_option.as_ref(),
        "desperate" => event.desperate_option.as_ref(),
        _ => None,
    }
    .ok_or_else(|| format!("Invalid choice: {requested_choice}"))?;

    let mut success_chance = option.success_chance;
    if choice == "risky" && policy.diviners_knot {
        success_chance = (success_chance + 0.10).min(1.0);
    }
    if matches!(choice, "risky" | "desperate") {
        if policy.surveyors_loop {
            success_chance = (success_chance - 0.05).max(0.05);
        }
        if policy.ruinwager_edge {
            success_chance = (success_chance + 0.05).min(1.0);
        }
        if choice == "risky" && policy.active_temp_curse && policy.black_wax_seal {
            success_chance = (success_chance + 0.05).min(1.0);
        }
        success_chance = (success_chance - luminosity_risky_penalty(policy.luminosity)).max(0.05);
    }
    if choice == "safe" && option.failure.is_some() && policy.cruel_safe_failure > 0.0 {
        success_chance = success_chance.min(1.0 - policy.cruel_safe_failure);
    }

    let cruel_echoes = choice == "safe"
        && option.failure.is_none()
        && policy.cruel_safe_failure > 0.0
        && rolls
            .cruel_echo_roll
            .is_some_and(|roll| roll < policy.cruel_safe_failure);
    let succeeded = !cruel_echoes && rolls.success_roll < success_chance;
    let synthetic_cruel_outcome = CanonicalEventOutcome {
        description: "Cruel Echoes! Even safety betrays you. Lost 1 block and 1 JC.".to_owned(),
        advance: -1,
        jc: -1,
        cave_in: false,
        streak_loss: 0,
        curse: None,
        gear_reward_pool: Vec::new(),
        consumable_reward_pool: Vec::new(),
        artifact_reward_pool: Vec::new(),
    };
    let outcome = if cruel_echoes {
        &synthetic_cruel_outcome
    } else if succeeded {
        &option.success
    } else {
        option.failure.as_ref().unwrap_or(&option.success)
    };

    let mut advance = jitter_event_advance(outcome.advance, rolls.advance_jitter);
    if succeeded && choice == "safe" && policy.surveyors_loop {
        advance += 1;
    }
    if succeeded && choice == "safe" && policy.chipped_compass {
        advance += 1;
    }
    let (mut advance, boss_encounter) = cap_event_advance_at_boss_boundary(
        policy.next_boss_boundary,
        policy.current_depth.max(0),
        advance,
    );

    let mut jc = outcome.jc;
    if jc < 0 {
        jc = round_i64(jc as f64 * CANONICAL_EVENT_NEGATIVE_JC_MULTIPLIER);
    }
    if jc != 0 {
        jc = round_i64(jc as f64 * rolls.jc_jitter_multiplier);
    }
    if jc < 0 {
        jc = strengthen_event_penalty(jc);
        if !succeeded && matches!(choice, "risky" | "desperate") && policy.ruinwager_edge {
            jc = -((jc.unsigned_abs() as f64 * 1.25).ceil() as i64);
        }
    }
    if policy.burning_ledger {
        jc = if jc > 0 {
            round_i64(jc as f64 * 1.15)
        } else if jc < 0 {
            round_i64(jc as f64 * 1.25)
        } else {
            jc
        };
    }
    if policy.chained && jc > 0 && policy.chain_jc_multiplier > 1.0 {
        jc = (jc as f64 * policy.chain_jc_multiplier) as i64;
    }
    if policy.chained && jc > 0 && policy.expedition_reward_bonus > 0.0 {
        jc = (jc as f64 * (1.0 + policy.expedition_reward_bonus)) as i64;
    }
    if jc > 0 && event_has_negative_outcome(event) && policy.risky_success_bonus > 0.0 {
        jc = (jc as f64 * (1.0 + policy.risky_success_bonus)) as i64;
    }
    let economy_gross_jc = jc;
    jc = if jc > 0 {
        // Python first applies the central generated-JC scale, then today's
        // optional economy adjustment, and finally the Dig positive-yield
        // multiplier.  Keep each stage explicit so adapters can replace the
        // middle policy without accidentally scaling a loss or transfer.
        let structurally_scaled =
            scale_minigame_jc_delta(jc as f64, policy.minigame_jc_delta_scale);
        let economy_adjusted =
            round_i64(structurally_scaled as f64 * policy.economy_reward_multiplier);
        (economy_adjusted as f64 * CANONICAL_EVENT_POSITIVE_JC_MULTIPLIER + 0.5)
            .floor()
            .max(1.0) as i64
    } else if jc < 0 {
        scale_deflationary_minigame_jc_delta(jc as f64, policy.minigame_jc_delta_scale)
    } else {
        0
    };
    // Cruel Echoes is a synthetic branch in Python's resolver and bypasses
    // the ordinary event jitter/penalty scalars.
    if cruel_echoes {
        advance = -1;
        jc = -1;
    }

    // Chain selection is a follow-up application action.  The pure resolver
    // exposes the authored `next_event_id`; the runtime may fill a selected
    // chained id once its quest/eligibility repository context is available.
    let chained_event_id = None;

    let (streak_loss, streak_days_after) = if outcome.streak_loss > 0 {
        let current = policy.current_streak.max(0);
        let setback = outcome.streak_loss.saturating_add(current / 20);
        let after = current.saturating_sub(setback).max(0);
        (current - after, Some(after))
    } else {
        (0, None)
    };

    let persisted_curse = outcome.curse.as_ref().map(|curse| {
        let mut persisted = curse.clone();
        persisted.duration_digs += CANONICAL_EVENT_CURSE_DURATION_BONUS_DIGS;
        persisted.effect = strengthen_curse_effect(&curse.effect);
        persisted
    });
    let black_wax_seal_spent =
        succeeded && choice == "risky" && policy.active_temp_curse && policy.black_wax_seal;
    let active_curse_remaining_after = black_wax_seal_spent
        .then(|| {
            policy
                .active_curse_remaining
                .map(|remaining| remaining.saturating_sub(1))
        })
        .flatten();
    let curse_cleared = active_curse_remaining_after == Some(0);
    let splash = event.splash.as_ref().filter(|splash| {
        matches!(choice, "risky" | "desperate")
            && match splash.trigger.as_str() {
                "success" => succeeded,
                "failure" => !succeeded,
                "always" => true,
                _ => false,
            }
    });
    Ok(CanonicalEventResolution {
        event_id: event.id.clone(),
        event_name: event.name.clone(),
        choice: choice.to_owned(),
        complexity: event.complexity,
        descriptions: event.descriptions.clone(),
        steps: event.steps.clone(),
        boon_options: event.boon_options.clone(),
        ascii_art: event.ascii_art.clone(),
        social: event.social,
        succeeded,
        message: if cruel_echoes {
            synthetic_cruel_outcome.description.clone()
        } else {
            outcome.description.clone()
        },
        advance,
        jc,
        cave_in: outcome.cave_in,
        streak_loss,
        streak_days_after,
        curse: outcome.curse.clone(),
        persisted_curse,
        buff: (succeeded && matches!(choice, "risky" | "desperate"))
            .then(|| event.buff_on_success.clone())
            .flatten(),
        black_wax_seal_spent,
        active_curse_remaining_after,
        curse_cleared,
        gear_reward_pool: outcome.gear_reward_pool.clone(),
        consumable_reward_pool: outcome.consumable_reward_pool.clone(),
        artifact_reward_pool: outcome.artifact_reward_pool.clone(),
        splash: splash.cloned(),
        guild_modifier_on_success: (succeeded && matches!(choice, "risky" | "desperate"))
            .then(|| event.guild_modifier_on_success.clone())
            .flatten(),
        quest_id: event.quest_id.clone(),
        quest_step: event.quest_step,
        next_event_id: event.next_event_id.clone(),
        chained_event_id,
        reward: None,
        duplicate_gear_reward: false,
        splash_payout_ratio: None,
        economy_gross_jc,
        cruel_echoes,
        boss_encounter,
        random_plan: CanonicalEventRandomPlan {
            jc_jitter: !cruel_echoes && outcome.jc != 0,
            advance_jitter: !cruel_echoes && outcome.advance != 0,
        },
    })
}

/// Whether the caller must draw the extra Cruel Echoes roll before drawing
/// the ordinary success roll.  Python only calls `random.random()` for this
/// branch when the requested safe option has no failure payload.
#[must_use]
pub fn canonical_event_needs_cruel_echo_roll(
    event_id: &str,
    requested_choice: &str,
    policy: CanonicalEventPolicy,
) -> bool {
    if policy.cruel_safe_failure <= 0.0 || requested_choice != "safe" || policy.luminosity <= 0 {
        return false;
    }
    canonical_event(event_id)
        .and_then(|event| event.safe_option.as_ref())
        .is_some_and(|option| option.failure.is_none())
}

/// Python's `round()` uses ties-to-even.  Rust's `f64::round()` is
/// ties-away-from-zero, which diverges on authored values such as `2.5` and
/// `-1.5`; keep this helper local so every event scalar uses the Python rule.
fn round_i64(value: f64) -> i64 {
    debug_assert!(value.is_finite());
    let lower = value.floor();
    let fraction = value - lower;
    if fraction < 0.5 {
        lower as i64
    } else if fraction > 0.5 {
        (lower + 1.0) as i64
    } else {
        let lower_i = lower as i64;
        if lower_i % 2 == 0 {
            lower_i
        } else {
            (lower + 1.0) as i64
        }
    }
}

fn jitter_event_advance(authored: i64, jitter: i64) -> i64 {
    if authored == 0 {
        return 0;
    }
    let jittered = authored.saturating_add(jitter);
    if authored > 0 {
        jittered.max(1)
    } else {
        let signed = jittered.min(-1);
        -((signed.unsigned_abs() as f64 * CANONICAL_EVENT_DEPTH_SETBACK_MULTIPLIER).ceil() as i64)
    }
}

fn strengthen_event_penalty(amount: i64) -> i64 {
    if amount == 0 {
        return 0;
    }
    let magnitude =
        (amount.unsigned_abs() as f64 * CANONICAL_EVENT_DEFLATION_MULTIPLIER).ceil() as i64;
    if amount > 0 { magnitude } else { -magnitude }
}

fn luminosity_risky_penalty(luminosity: i64) -> f64 {
    if luminosity >= 76 {
        0.0
    } else if luminosity >= 26 {
        0.05
    } else if luminosity >= 1 {
        0.15
    } else {
        0.25
    }
}

fn strengthen_curse_effect(effect: &serde_json::Value) -> serde_json::Value {
    let Some(object) = effect.as_object() else {
        return effect.clone();
    };
    let mut scaled = serde_json::Map::new();
    for (key, value) in object {
        let Some(number) = value.as_f64() else {
            scaled.insert(key.clone(), value.clone());
            continue;
        };
        let number = number * CANONICAL_EVENT_CURSE_STRENGTH_MULTIPLIER;
        if matches!(key.as_str(), "cave_in_bonus" | "cooldown_penalty") {
            scaled.insert(key.clone(), serde_json::json!(number));
        } else {
            scaled.insert(key.clone(), serde_json::json!(round_i64(number)));
        }
    }
    serde_json::Value::Object(scaled)
}

fn event_has_negative_outcome(event: &CanonicalEventDef) -> bool {
    [
        event.safe_option.as_ref(),
        event.risky_option.as_ref(),
        event.desperate_option.as_ref(),
    ]
    .into_iter()
    .flatten()
    .flat_map(|option| [Some(&option.success), option.failure.as_ref()])
    .flatten()
    .any(|outcome| {
        outcome.jc < 0
            || outcome.advance < 0
            || outcome.cave_in
            || outcome.streak_loss > 0
            || outcome.curse.is_some()
    })
}

/// Apply Python's boss-boundary rule to a positive depth advance.  A dig may
/// land on the block immediately before the next boss, but it must not cross
/// that boundary; the runtime can use the returned flag to render the boss
/// encounter UI and defer the actual boss resolution.
fn cap_event_advance_at_boss_boundary(
    next_boss_boundary: Option<i64>,
    current_depth: i64,
    advance: i64,
) -> (i64, bool) {
    let Some(boundary) = next_boss_boundary else {
        return (advance, false);
    };
    if boundary < 0 || advance <= 0 || current_depth.saturating_add(advance) < boundary {
        return (advance, false);
    }
    (
        boundary
            .saturating_sub(1)
            .saturating_sub(current_depth)
            .max(0),
        true,
    )
}

fn deserialize_rarity<'de, D>(deserializer: D) -> Result<Rarity, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Ok(Rarity::parse(&value))
}

fn deserialize_event_complexity<'de, D>(deserializer: D) -> Result<EventComplexity, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Ok(EventComplexity::parse(&value))
}

fn deserialize_null_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

const CANONICAL_EVENT_JSON: &str = include_str!("dig_event_catalog.json");

/// Parse the generated canonical snapshot once per process. The snapshot is
/// produced from `services/dig_data/event_definitions.py` during the port and
/// is checked against the Python catalog by the parity tests.
#[must_use]
pub fn canonical_event_catalog() -> &'static [CanonicalEventDef] {
    static CATALOG: OnceLock<Vec<CanonicalEventDef>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let catalog: Vec<CanonicalEventDef> = serde_json::from_str(CANONICAL_EVENT_JSON)
            .unwrap_or_else(|error| panic!("invalid generated Dig event catalog: {error}"));
        validate_canonical_event_reward_pools(&catalog)
            .unwrap_or_else(|error| panic!("invalid generated Dig event reward pool: {error}"));
        catalog
    })
}

#[must_use]
pub fn canonical_event(event_id: &str) -> Option<&'static CanonicalEventDef> {
    canonical_event_catalog()
        .iter()
        .find(|event| event.id == event_id)
}

const CANONICAL_QUEST_JSON: &str = include_str!("dig_quest_catalog.json");

/// Stable checksum for the generated quest/finale snapshot.
#[must_use]
pub fn canonical_quest_snapshot_hash() -> u64 {
    CANONICAL_QUEST_JSON
        .bytes()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
}

#[must_use]
pub fn canonical_quest_catalog() -> &'static [CanonicalQuestDef] {
    static QUESTS: OnceLock<Vec<CanonicalQuestDef>> = OnceLock::new();
    QUESTS.get_or_init(|| {
        let wires = serde_json::from_str::<Vec<CanonicalQuestWire>>(CANONICAL_QUEST_JSON)
            .unwrap_or_else(|error| panic!("invalid generated Dig quest catalog: {error}"));
        wires.into_iter().map(canonical_quest_from_wire).collect()
    })
}

fn canonical_quest_from_wire(wire: CanonicalQuestWire) -> CanonicalQuestDef {
    let payload = wire
        .finale_payload
        .as_object()
        .unwrap_or_else(|| panic!("quest {} finale payload must be an object", wire.quest_id));
    let finale = match wire.finale_kind.as_str() {
        "jc_plus_guild_modifier" => CanonicalQuestFinale::JcPlusGuildModifier {
            personal_jc: json_i64(payload, "personal_jc", &wire.quest_id),
            modifier_id: json_string(payload, "modifier_id", &wire.quest_id),
            duration_seconds: json_i64(payload, "duration_seconds", &wire.quest_id),
            modifier_payload: payload
                .get("modifier_payload")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        },
        "relic_grant" => CanonicalQuestFinale::RelicGrant {
            relic_base: json_string(payload, "relic_base", &wire.quest_id),
            relic_suffix: json_string(payload, "relic_suffix", &wire.quest_id),
            stat_pool: json_strings(payload, "stat_pool", &wire.quest_id),
            roll_count: json_i64(payload, "roll_count", &wire.quest_id)
                .try_into()
                .unwrap_or_else(|_| panic!("quest {} roll_count is negative", wire.quest_id)),
        },
        kind => panic!("quest {} has unknown finale kind {kind}", wire.quest_id),
    };
    CanonicalQuestDef {
        quest_id: wire.quest_id,
        name: wire.name,
        starter_prereq: wire.starter_prereq,
        step_event_ids: wire.step_event_ids,
        finale,
    }
}

fn json_i64(
    payload: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    quest_id: &str,
) -> i64 {
    payload
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_else(|| panic!("quest {quest_id} finale is missing integer {key}"))
}

fn json_string(
    payload: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    quest_id: &str,
) -> String {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| panic!("quest {quest_id} finale is missing string {key}"))
}

fn json_strings(
    payload: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    quest_id: &str,
) -> Vec<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value.as_str().map(ToOwned::to_owned).unwrap_or_else(|| {
                        panic!("quest {quest_id} finale {key} must contain strings")
                    })
                })
                .collect()
        })
        .unwrap_or_else(|| panic!("quest {quest_id} finale is missing string array {key}"))
}

#[must_use]
pub fn canonical_quest_for_event(event_id: &str) -> Option<(&'static CanonicalQuestDef, i64)> {
    crate::dig_quest_policy::quest_for_event(canonical_quest_catalog(), event_id)
}

/// Port the quest service's event-pool eligibility.  The active quest path
/// deliberately returns its current stage without rechecking starter depth or
/// prestige; those checks are only for starting a new arc.  System predicates
/// are resolved by the caller and passed as a set of satisfied ids.
#[must_use]
pub fn canonical_eligible_quest_event_ids(
    depth: i64,
    prestige_level: i64,
    active_quest_id: Option<&str>,
    active_quest_step: Option<i64>,
    completed_quest_ids: &BTreeSet<String>,
    satisfied_system_predicates: &BTreeSet<String>,
) -> BTreeSet<String> {
    crate::dig_quest_policy::eligible_quest_event_ids(
        canonical_quest_catalog(),
        depth,
        prestige_level,
        active_quest_id,
        active_quest_step,
        completed_quest_ids,
        satisfied_system_predicates,
    )
}

/// Advance a quest after the caller has already established a successful
/// desperate outcome.  This is a pure state transition descriptor: adapters
/// persist `Activated`/`Advanced`, or dispatch `Completed.finale` atomically
/// with the event settlement.
#[must_use]
pub fn advance_canonical_quest_on_desperate_success(
    event_id: &str,
    active_quest_id: Option<&str>,
    active_quest_step: Option<i64>,
    completed_quest_ids: &BTreeSet<String>,
    depth: i64,
    prestige_level: i64,
    satisfied_system_predicates: &BTreeSet<String>,
) -> CanonicalQuestProgress {
    crate::dig_quest_policy::advance_quest_on_desperate_success(
        canonical_quest_catalog(),
        crate::dig_quest_policy::QuestProgressInput {
            event_id,
            active_quest_id,
            active_quest_step,
            completed_quest_ids,
            depth,
            prestige_level,
            satisfied_system_predicates,
        },
    )
}

/// Resolve a typed finale descriptor without persistence or ambient RNG.
/// Relic finales use one injected unit roll per `random.sample` selection and
/// preserve Python's exact encoded artifact-id/name shape.
pub fn resolve_canonical_quest_finale(
    finale: &CanonicalQuestFinale,
    selection_rolls: &[f64],
) -> Result<CanonicalQuestFinaleOutcome, String> {
    match finale {
        CanonicalQuestFinale::JcPlusGuildModifier {
            personal_jc,
            modifier_id,
            duration_seconds,
            modifier_payload,
        } => Ok(CanonicalQuestFinaleOutcome::JcPlusGuildModifier {
            personal_jc: *personal_jc,
            modifier_id: modifier_id.clone(),
            duration_seconds: *duration_seconds,
            modifier_payload: modifier_payload.clone(),
        }),
        CanonicalQuestFinale::RelicGrant {
            relic_base,
            relic_suffix,
            stat_pool,
            roll_count,
        } => {
            if stat_pool.len() < *roll_count || selection_rolls.len() < *roll_count {
                return Err("Quest finale stat pool or rolls are insufficient.".to_owned());
            }
            let mut remaining = stat_pool.clone();
            let mut chosen = Vec::with_capacity(*roll_count);
            for roll in selection_rolls.iter().take(*roll_count) {
                if !roll.is_finite() || !(0.0..=1.0).contains(roll) {
                    return Err("Invalid quest finale roll.".to_owned());
                }
                let index = (roll.min(1.0 - f64::EPSILON) * remaining.len() as f64) as usize;
                chosen.push(remaining.remove(index.min(remaining.len() - 1)));
            }
            Ok(CanonicalQuestFinaleOutcome::RelicGrant {
                relic_name: format!("{relic_base} of {relic_suffix}"),
                artifact_id: format!("pinnacle:{relic_base}:{relic_suffix}:{}", chosen.join(":")),
                relic_stat_ids: chosen,
            })
        }
    }
}

/// Project an authored event into the application-facing UI model.  The
/// caller chooses a description variant with `canonical_event_description`;
/// no display text is re-authored in the Rust port.
pub fn canonical_event_presentation(event_id: &str) -> Result<CanonicalEventPresentation, String> {
    let event = canonical_event(event_id).ok_or_else(|| "Unknown event.".to_owned())?;
    Ok(CanonicalEventPresentation {
        event_id: event.id.clone(),
        event_name: event.name.clone(),
        complexity: event.complexity,
        descriptions: event.descriptions.clone(),
        steps: event.steps.clone(),
        boon_options: event.boon_options.clone(),
        ascii_art: event.ascii_art.clone(),
        social: event.social,
    })
}

/// Select an authored flavor-text variant using a caller-provided unit roll.
/// Python's `random.choice` is equivalent to this floor-based index for a
/// uniform unit interval, and the explicit roll keeps the differential test
/// independent of RNG implementation details.
pub fn canonical_event_description(event_id: &str, roll: f64) -> Result<String, String> {
    let event = canonical_event(event_id).ok_or_else(|| "Unknown event.".to_owned())?;
    if !roll.is_finite() || !(0.0..=1.0).contains(&roll) {
        return Err("Invalid description roll.".to_owned());
    }
    if event.descriptions.is_empty() {
        return Ok(String::new());
    }
    let index = ((roll.min(1.0 - f64::EPSILON) * event.descriptions.len() as f64) as usize)
        .min(event.descriptions.len() - 1);
    Ok(event.descriptions[index].clone())
}

/// Return events visible to the random event picker under the same depth,
/// layer, darkness, prestige, quest, boss, and chain-only gates as Python.
/// Quest ids are supplied by the quest adapter because starter/system
/// predicates require repository context that this pure catalog layer does
/// not own.
pub fn canonical_eligible_events(
    depth: i64,
    luminosity: i64,
    prestige_level: i64,
    eligible_quest_ids: &BTreeSet<String>,
    include_quest_events: bool,
    in_boss: bool,
) -> Vec<&'static CanonicalEventDef> {
    let layer_name = layer_at(depth).name;
    canonical_event_catalog()
        .iter()
        .filter(|event| depth >= event.min_depth.unwrap_or(0))
        .filter(|event| event.max_depth.is_none_or(|maximum| depth <= maximum))
        .filter(|event| {
            event
                .layer
                .as_deref()
                .is_none_or(|layer| layer == layer_name)
        })
        .filter(|event| !event.requires_dark || luminosity <= 0)
        .filter(|event| prestige_level >= event.min_prestige)
        .filter(|event| !event.chain_only)
        .filter(|event| {
            if event.quest_id.is_none() {
                return true;
            }
            include_quest_events && !in_boss && eligible_quest_ids.contains(&event.id)
        })
        .collect()
}

fn canonical_luminosity_harmful_weight(luminosity: i64) -> f64 {
    let band = if luminosity >= CANONICAL_LUMINOSITY_BRIGHT {
        1.0
    } else if luminosity >= CANONICAL_LUMINOSITY_DIM {
        CANONICAL_LUMINOSITY_DIM_HARMFUL_WEIGHT
    } else if luminosity >= CANONICAL_LUMINOSITY_DARK {
        CANONICAL_LUMINOSITY_DARK_HARMFUL_WEIGHT
    } else {
        CANONICAL_LUMINOSITY_PITCH_HARMFUL_WEIGHT
    };
    CANONICAL_EVENT_HARMFUL_WEIGHT_MULTIPLIER * band
}

/// Match Python's rarity/luminosity weighting for one eligible event.
#[must_use]
pub fn canonical_event_weight(
    event: &CanonicalEventDef,
    luminosity: i64,
    rare_event_multiplier: f64,
    legendary_event_multiplier: f64,
    void_bait_active: bool,
) -> f64 {
    let rarity_multiplier = match event.rarity {
        Rarity::Rare => (1.0 + rare_event_multiplier) * if void_bait_active { 1.25 } else { 1.0 },
        Rarity::Legendary => {
            (1.0 + legendary_event_multiplier) * if void_bait_active { 1.5 } else { 1.0 }
        }
        _ => 1.0,
    };
    let mut weight = event.rarity.weight() as f64 * rarity_multiplier;
    if event_has_negative_outcome(event) {
        weight *= canonical_luminosity_harmful_weight(luminosity);
    }
    // Python duplicates darkness events in the eligible list at pitch black,
    // preserving ordinary events while making darkness events more likely.
    if luminosity <= 0 && event.requires_dark {
        weight *= 2.0;
    }
    weight
}

/// Repository-backed state needed to roll one canonical event.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalEventRollContext<'a> {
    pub depth: i64,
    pub luminosity: i64,
    pub prestige_level: i64,
    pub eligible_quest_ids: &'a BTreeSet<String>,
    pub include_quest_events: bool,
    pub in_boss: bool,
    pub void_bait_active: bool,
    pub rare_event_multiplier: f64,
    pub legendary_event_multiplier: f64,
}

/// Roll one canonical event with deterministic injected entropy.
pub fn roll_canonical_event(
    context: CanonicalEventRollContext<'_>,
    entropy: &mut impl LootEntropy,
) -> Option<&'static CanonicalEventDef> {
    let eligible = canonical_eligible_events(
        context.depth,
        context.luminosity,
        context.prestige_level,
        context.eligible_quest_ids,
        context.include_quest_events,
        context.in_boss,
    );
    let weighted = eligible
        .into_iter()
        .map(|event| {
            (
                event,
                canonical_event_weight(
                    event,
                    context.luminosity,
                    context.rare_event_multiplier,
                    context.legendary_event_multiplier,
                    context.void_bait_active,
                ),
            )
        })
        .filter(|(_, weight)| *weight > 0.0)
        .collect::<Vec<_>>();
    let total = weighted.iter().map(|(_, weight)| *weight).sum::<f64>();
    if total <= 0.0 {
        return None;
    }
    let mut threshold = entropy.unit().clamp(0.0, 1.0 - f64::EPSILON) * total;
    for (event, weight) in weighted {
        if threshold < weight {
            return Some(event);
        }
        threshold -= weight;
    }
    None
}

const CANONICAL_EVENT_UNIQUE_GEAR_IDS: &[&str] = &[
    "glassbreaker_pick",
    "surveyors_loop",
    "red_thread_band",
    "needle_pick",
    "briarplate",
    "nullweave_mantle",
    "anchor_boots",
    "loaded_die",
    "springheel_boots",
    "blood_locket",
    "ruinwager_signet",
];

fn canonical_unique_gear_exists(item_id: &str) -> bool {
    unique_gear(item_id).is_some() || CANONICAL_EVENT_UNIQUE_GEAR_IDS.contains(&item_id)
}

/// Reject authored reward identifiers that cannot be persisted or rendered.
/// The generated catalog calls this during its first process-wide load, which
/// matches Python's import-time validation rather than silently dropping a
/// typo during resolution.
pub fn validate_canonical_event_reward_pools(events: &[CanonicalEventDef]) -> Result<(), String> {
    let artifacts = artifact_catalog()
        .into_iter()
        .map(|artifact| artifact.id)
        .collect::<BTreeSet<_>>();
    for event in events {
        for outcome in [
            event.safe_option.as_ref(),
            event.risky_option.as_ref(),
            event.desperate_option.as_ref(),
        ]
        .into_iter()
        .flatten()
        .flat_map(|option| [Some(&option.success), option.failure.as_ref()])
        .flatten()
        {
            for item_id in &outcome.gear_reward_pool {
                if !canonical_unique_gear_exists(item_id) {
                    return Err(format!("{}: unknown gear reward {item_id}", event.id));
                }
            }
            for item_id in &outcome.consumable_reward_pool {
                if consumable(item_id).is_none() {
                    return Err(format!("{}: unknown consumable reward {item_id}", event.id));
                }
            }
            for item_id in &outcome.artifact_reward_pool {
                if !artifacts.contains(item_id.as_str()) {
                    return Err(format!("{}: unknown artifact reward {item_id}", event.id));
                }
            }
        }
    }
    Ok(())
}

/// Apply Python's ownership/capacity filtering and choose one reward from an
/// authored pool.  The candidate order is gear, consumable, artifact, which
/// matches the Python list concatenation before `random.choice`.
pub fn select_canonical_reward(
    outcome: &CanonicalEventOutcome,
    owned_gear: &BTreeSet<String>,
    owned_artifacts: &BTreeSet<String>,
    inventory_len: usize,
    inventory_capacity: usize,
    reward_roll: f64,
) -> Result<CanonicalRewardSelection, String> {
    if !reward_roll.is_finite() || !(0.0..=1.0).contains(&reward_roll) {
        return Err("Invalid reward roll.".to_owned());
    }
    let eligible_gear = outcome
        .gear_reward_pool
        .iter()
        .filter(|item_id| canonical_unique_gear_exists(item_id) && !owned_gear.contains(*item_id))
        .cloned()
        .collect::<Vec<_>>();
    let has_room = inventory_len < inventory_capacity;
    let eligible_consumables = outcome
        .consumable_reward_pool
        .iter()
        .filter(|item_id| has_room && consumable(item_id).is_some())
        .cloned()
        .collect::<Vec<_>>();
    let eligible_artifacts = outcome
        .artifact_reward_pool
        .iter()
        .filter(|item_id| {
            artifact_catalog()
                .iter()
                .any(|artifact| artifact.id == *item_id)
                && !owned_artifacts.contains(*item_id)
        })
        .cloned()
        .collect::<Vec<_>>();

    let mut candidates = Vec::with_capacity(
        eligible_gear.len() + eligible_consumables.len() + eligible_artifacts.len(),
    );
    candidates.extend(eligible_gear.iter().cloned().map(CanonicalReward::Gear));
    candidates.extend(
        eligible_consumables
            .iter()
            .cloned()
            .map(CanonicalReward::Consumable),
    );
    candidates.extend(
        eligible_artifacts
            .iter()
            .cloned()
            .map(CanonicalReward::Artifact),
    );
    let reward = if candidates.is_empty() {
        None
    } else {
        let index = ((reward_roll.min(1.0 - f64::EPSILON) * candidates.len() as f64) as usize)
            .min(candidates.len() - 1);
        Some(candidates[index].clone())
    };
    let duplicate_gear_reward = !outcome.gear_reward_pool.is_empty()
        && eligible_gear.is_empty()
        && outcome.consumable_reward_pool.is_empty()
        && outcome.artifact_reward_pool.is_empty()
        && outcome
            .gear_reward_pool
            .iter()
            .any(|item_id| owned_gear.contains(item_id));
    Ok(CanonicalRewardSelection {
        reward,
        duplicate_gear_reward,
        eligible_gear,
        eligible_consumables,
        eligible_artifacts,
    })
}

/// Complete input for canonical event resolution and non-JC reward selection.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalEventResolutionRequest<'a> {
    pub event_id: &'a str,
    pub requested_choice: &'a str,
    pub rolls: CanonicalEventRolls,
    pub policy: CanonicalEventPolicy,
    pub owned_gear: &'a BTreeSet<String>,
    pub owned_artifacts: &'a BTreeSet<String>,
    pub inventory_len: usize,
    pub inventory_capacity: usize,
}

/// Resolve an event and attach the ownership-filtered reward selected by the
/// application RNG.  The base resolver stays pure and context-light; this
/// wrapper is the typed seam for inventory/gear/artifact state.
pub fn resolve_canonical_event_with_policy_and_rewards(
    request: CanonicalEventResolutionRequest<'_>,
) -> Result<CanonicalEventResolution, String> {
    let mut resolution = resolve_canonical_event_with_policy(
        request.event_id,
        request.requested_choice,
        request.rolls,
        request.policy,
    )?;
    if let Some(reward_roll) = request.rolls.reward_roll {
        let outcome = CanonicalEventOutcome {
            description: resolution.message.clone(),
            advance: resolution.advance,
            jc: resolution.economy_gross_jc,
            cave_in: resolution.cave_in,
            streak_loss: resolution.streak_loss,
            curse: resolution.curse.clone(),
            gear_reward_pool: resolution.gear_reward_pool.clone(),
            consumable_reward_pool: resolution.consumable_reward_pool.clone(),
            artifact_reward_pool: resolution.artifact_reward_pool.clone(),
        };
        let selected = select_canonical_reward(
            &outcome,
            request.owned_gear,
            request.owned_artifacts,
            request.inventory_len,
            request.inventory_capacity,
            reward_roll,
        )?;
        resolution.reward = selected.reward;
        resolution.duplicate_gear_reward = selected.duplicate_gear_reward;
    }
    Ok(resolution)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanonicalSplashSettlement {
    pub payout_ratio: f64,
    pub jc_after: i64,
}

/// Scale a positive burn-event payout by the amount actually destroyed in the
/// splash transaction.  This mirrors the Python nominal-burn ratio and uses
/// Python's ties-to-even rounding for the resulting JC.
#[must_use]
pub fn scale_canonical_splash_payout(
    jc: i64,
    splash: &CanonicalSplash,
    total_burned: i64,
    minigame_jc_delta_scale: f64,
) -> CanonicalSplashSettlement {
    if jc <= 0 || splash.mode != "burn" {
        return CanonicalSplashSettlement {
            payout_ratio: 1.0,
            jc_after: jc,
        };
    }
    let nominal_burn =
        (splash.victim_count as i64).saturating_mul(scale_deflationary_minigame_jc_delta(
            strengthen_event_penalty(splash.penalty_jc) as f64,
            minigame_jc_delta_scale,
        ));
    let payout_ratio = if nominal_burn > 0 {
        (total_burned.max(0) as f64 / nominal_burn as f64).min(1.0)
    } else {
        0.0
    };
    CanonicalSplashSettlement {
        payout_ratio,
        jc_after: round_i64(jc as f64 * payout_ratio),
    }
}

/// Python's chain resolver: deterministic `next_event_id` first, then a
/// prestige-gated chance to select an event of the same or higher rarity.
#[must_use]
pub fn canonical_chain_event(
    trigger_event_id: &str,
    depth: i64,
    prestige_level: i64,
    chain_roll: Option<f64>,
    selection_roll: Option<f64>,
) -> Option<&'static CanonicalEventDef> {
    let trigger = canonical_event(trigger_event_id)?;
    if let Some(next_id) = &trigger.next_event_id
        && let Some(next) = canonical_event(next_id)
        && prestige_level >= next.min_prestige
    {
        return Some(next);
    }
    if prestige_level < 7 {
        return None;
    }
    let chain_roll = chain_roll?;
    if !chain_roll.is_finite()
        || !(0.0..=1.0).contains(&chain_roll)
        || chain_roll >= CANONICAL_EVENT_CHAIN_CHANCE
    {
        return None;
    }
    let selection_roll = selection_roll?;
    if !selection_roll.is_finite() || !(0.0..=1.0).contains(&selection_roll) {
        return None;
    }
    let minimum_rarity = rarity_rank(trigger.rarity);
    let candidates = canonical_event_catalog()
        .iter()
        .filter(|event| depth >= event.min_depth.unwrap_or(0))
        .filter(|event| event.max_depth.is_none_or(|maximum| depth <= maximum))
        .filter(|event| rarity_rank(event.rarity) >= minimum_rarity)
        .filter(|event| prestige_level >= event.min_prestige)
        .filter(|event| !event.chain_only && event.quest_id.is_none())
        .collect::<Vec<_>>();
    let total = candidates
        .iter()
        .map(|event| event.rarity.weight())
        .sum::<u64>();
    if total == 0 {
        return None;
    }
    let mut threshold = (selection_roll.min(1.0 - f64::EPSILON) * total as f64) as u64;
    for event in candidates {
        let weight = event.rarity.weight();
        if threshold < weight {
            return Some(event);
        }
        threshold -= weight;
    }
    None
}

fn rarity_rank(rarity: Rarity) -> u8 {
    match rarity {
        Rarity::Common => 0,
        Rarity::Uncommon => 1,
        Rarity::Rare => 2,
        Rarity::Legendary => 3,
    }
}
// Generated from the canonical event snapshot.  This compact projection keeps
// event eligibility copyable; authored descriptions, choices, outcomes, and
// special effects live in `dig_event_catalog.json` and are exposed through
// `canonical_event_catalog` above.
const EVENT_ROWS: &str = r#"underground_stream|_|_|_|common|0|0|choice|1|0|0|0|0
gas_pocket|10|_|_|common|0|0|choice|1|0|0|0|0
techies_cache|15|75|_|common|0|0|choice|1|0|0|0|0
meepo_clones|_|50|_|common|0|0|choice|1|0|0|0|0
cursed_chest|25|_|_|common|0|0|choice|1|0|0|0|0
lost_miner|_|_|_|common|0|0|choice|1|0|0|0|0
crystal_golem|40|90|_|common|0|0|choice|1|0|0|0|0
mushroom_grove|5|60|_|common|0|0|choice|1|0|0|0|0
magma_geyser|60|_|_|common|0|0|choice|1|0|0|0|0
ancient_elevator|20|_|_|common|0|0|choice|1|0|0|0|0
void_whispers|80|_|_|common|0|0|choice|1|0|0|0|0
friendly_mole|_|40|_|common|0|0|choice|1|0|0|0|0
worm_council|_|25|Dirt|common|0|0|simple|1|0|0|0|0
buried_lunch_box|_|25|Dirt|common|0|0|choice|1|0|0|0|0
dig_dog|_|25|Dirt|common|0|0|simple|1|0|0|0|0
root_maze|5|25|Dirt|common|0|0|choice|1|0|0|0|0
pickaxe_head_flies_off|_|30|Dirt|common|0|0|simple|1|0|0|0|0
toll_keeper|26|55|Stone|common|0|0|complex|1|0|0|0|0
gravity_pocket|26|55|Stone|common|0|0|choice|1|0|0|0|0
fossil_argument|26|55|Stone|common|0|0|choice|1|0|0|0|0
sandwich_lady|26|55|Stone|common|0|0|simple|1|0|0|0|0
echo_chamber|26|55|Stone|common|0|0|simple|1|0|0|0|0
mirror_tunnel|51|80|Crystal|common|0|0|complex|1|0|0|0|0
resonance_cascade|51|80|Crystal|common|0|0|choice|1|0|0|0|0
crystal_garden|51|80|Crystal|common|0|0|simple|1|0|0|0|0
gem_rock|51|80|Crystal|common|0|0|choice|1|0|0|0|0
prism_trap|51|80|Crystal|common|0|0|choice|1|0|0|0|0
lava_surfer|76|105|Magma|common|0|0|choice|1|0|0|0|0
forge_spirit|76|105|Magma|common|0|0|complex|1|0|0|0|0
volcanic_vent_gambit|76|_|Magma|common|0|0|choice|1|0|0|0|0
heat_mirage|76|105|Magma|common|0|0|simple|1|0|0|0|0
shooting_star|76|_|Magma|uncommon|0|0|simple|1|0|0|0|0
pudge_fishing|26|_|_|uncommon|0|0|choice|1|0|0|0|0
tinker_workshop|51|_|Crystal|uncommon|0|0|complex|1|0|0|0|0
the_burrow|101|_|_|uncommon|0|0|complex|1|0|0|0|0
arcanist_library|201|_|Frozen Core|rare|0|0|complex|1|0|0|0|0
the_dark_rift|101|155|Abyss|uncommon|0|0|complex|1|0|0|0|0
void_market|101|155|Abyss|uncommon|0|0|complex|1|0|0|0|0
abyssal_fishing|101|155|Abyss|uncommon|0|0|choice|1|0|0|0|0
gravity_inversion|101|_|Abyss|common|0|0|choice|1|0|0|0|0
whispering_walls_extended|101|155|Abyss|rare|0|0|complex|1|0|0|0|0
spore_storm|151|205|Fungal Depths|common|0|0|choice|1|0|0|0|0
mycelium_network|151|205|Fungal Depths|uncommon|0|0|complex|1|0|0|0|0
sporewalker|151|205|Fungal Depths|common|0|0|simple|1|0|0|0|0
bioluminescent_cathedral|151|205|Fungal Depths|rare|0|0|simple|1|0|0|0|0
time_eddy|201|280|Frozen Core|common|0|0|complex|1|0|0|0|0
frozen_ancient|201|280|Frozen Core|uncommon|0|0|choice|1|0|0|0|0
the_still_point|201|280|Frozen Core|rare|0|0|simple|1|0|0|0|0
paradox_loop|201|_|Frozen Core|rare|0|0|complex|1|0|0|0|0
the_cartographer|276|_|The Hollow|rare|0|0|complex|1|0|0|0|0
the_final_merchant|276|_|The Hollow|legendary|0|0|complex|1|0|0|0|0
memory_of_the_surface|276|_|The Hollow|uncommon|0|0|simple|1|0|0|0|0
things_in_the_dark|76|_|_|common|1|0|choice|1|0|0|0|0
the_lightless_path|76|_|_|common|1|0|complex|1|0|0|0|0
phosphor_vein|76|_|_|uncommon|1|0|simple|1|0|0|0|0
roshan_lair|276|_|The Hollow|legendary|0|0|complex|1|0|0|0|0
buying_gf|101|_|_|uncommon|0|0|simple|1|0|0|0|0
rock_golem_encounter|50|_|_|rare|0|0|simple|1|0|0|0|0
creeper_ambush|0|75|_|common|0|0|choice|1|1|0|0|0
abandoned_minecart|0|75|_|common|0|0|choice|1|0|0|0|0
enchanting_table|0|55|_|uncommon|0|0|boon|1|0|3|0|0
villager_trade|0|80|_|common|0|0|choice|1|0|0|0|0
enderman_stare|0|80|_|uncommon|0|0|choice|1|1|0|0|0
mob_spawner|0|75|_|uncommon|0|0|choice|1|1|0|0|0
witch_cauldron|0|75|_|common|0|0|boon|1|0|3|0|0
azurite_deposit|40|120|_|common|0|0|choice|1|0|0|0|0
crawler_breakdown|40|120|_|uncommon|0|0|choice|1|1|0|0|0
fossil_cache|40|120|_|common|0|0|choice|1|0|0|0|0
breach_encounter|40|170|_|rare|0|0|choice|1|1|0|0|0
vaal_side_area|40|170|_|rare|0|0|choice|1|1|0|0|0
syndicate_ambush|40|170|_|uncommon|0|0|choice|1|0|0|0|0
delve_smuggler|40|170|_|uncommon|0|0|boon|1|0|3|0|0
brann_bronzebeard|130|340|_|uncommon|0|0|boon|1|0|3|0|0
earthen_cache|130|340|_|common|0|0|choice|1|0|0|0|0
campfire_rest|130|350|_|common|0|0|choice|1|0|0|0|0
zekvir_shadow|130|340|_|rare|0|0|choice|1|1|0|0|0
dark_rider|130|340|_|uncommon|0|0|choice|1|1|0|0|0
titan_relic|130|340|_|rare|0|0|boon|1|0|3|0|0
candle_glow|130|340|_|common|0|0|choice|1|0|0|0|0
olympian_boon|100|_|_|uncommon|0|0|boon|1|0|3|0|0
charon_toll|100|_|_|uncommon|0|0|choice|1|1|0|0|0
sisyphus_boulder|100|_|_|common|0|0|choice|1|0|0|0|0
infernal_gate|100|_|_|rare|0|0|choice|1|1|0|0|0
riki_ambush|_|_|_|uncommon|0|0|choice|1|1|0|0|0
bounty_rune|_|_|_|common|0|0|choice|1|0|0|0|0
aghanim_trial|_|_|_|rare|0|0|choice|1|1|0|0|0
tormentor_encounter|75|_|_|rare|0|0|choice|1|1|0|0|0
neutral_item_drop|_|_|_|uncommon|0|0|boon|1|0|3|0|0
gambling_den|_|_|_|uncommon|0|0|choice|1|1|0|0|0
item_goblin|_|_|_|uncommon|0|0|choice|1|1|0|0|0
mystery_lever|_|_|_|common|0|0|choice|1|0|0|0|0
identity_thief|_|_|_|uncommon|0|0|choice|1|0|0|0|0
neow_blessing|_|_|_|legendary|0|0|boon|1|0|3|0|0
fools_vein|10|_|_|rare|0|0|choice|1|0|0|0|0
sirens_hollow|76|_|_|rare|0|0|choice|1|0|0|0|0
hex_cursed_shrine|51|_|_|rare|0|0|choice|1|0|0|0|0
tricksters_toll|151|_|_|rare|0|0|choice|1|0|0|0|0
gamblers_rest|101|_|_|rare|0|0|choice|1|0|0|0|0
lumber_collapse|10|_|_|legendary|0|0|choice|1|0|0|0|0
wealth_siphon|51|_|_|legendary|0|0|choice|1|0|0|0|0
tunnel_network_breach|101|_|_|legendary|0|0|choice|1|0|0|0|0
sulphite_drought|26|200|_|rare|0|0|choice|1|1|0|0|0
auls_bullet_hell|151|275|_|legendary|0|0|choice|1|1|0|0|0
kurgal_summons|101|200|_|legendary|0|0|choice|1|1|0|0|0
flare_starvation|_|_|_|legendary|1|0|choice|1|1|0|0|0
resonator_socket|51|275|_|rare|0|0|choice|1|1|0|0|0
branns_potion|_|_|_|legendary|0|0|choice|1|1|0|0|0
zekvir_challenge|201|275|_|legendary|0|0|choice|1|1|0|0|0
enchanted_candle|51|200|_|rare|0|0|choice|1|1|0|0|0
sporbit_cavern|101|200|_|rare|0|0|choice|1|1|0|0|0
coffer_key_gamble|26|275|_|rare|0|0|choice|1|1|0|0|0
chain_frost_cascade|101|275|_|legendary|0|0|choice|1|0|0|0|0
divine_rapier_drop|101|275|_|legendary|0|0|choice|1|1|0|0|0
enigma_blackhole|151|275|_|legendary|0|0|choice|1|1|0|0|0
io_tether_pact|26|200|_|legendary|0|0|choice|1|1|0|0|0
refresher_shard|76|275|_|legendary|0|0|choice|1|1|0|0|0
aegis_whisper|_|_|_|uncommon|0|0|choice|1|0|0|0|0
echoing_mime|_|_|_|uncommon|0|0|choice|1|0|0|0|0
crow_snipe|10|_|_|uncommon|0|0|choice|1|0|0|0|0
smoke_detour|26|_|_|rare|0|0|choice|1|0|0|0|0
strangers_lamp|26|_|_|rare|0|0|choice|1|0|0|0|0
drill_sergeant|26|_|_|rare|0|0|choice|1|0|0|0|0
pit_lords_toll|51|_|_|legendary|0|0|choice|1|0|0|0|0
damned_bottle|76|_|_|legendary|0|0|choice|1|0|0|0|0
roshpit_gambit|76|_|_|legendary|0|0|choice|1|0|0|0|0
wilderness_stalker|101|_|_|legendary|0|0|choice|1|0|0|0|0
wisps_tether|51|75|_|common|0|0|choice|1|0|0|0|0
tunnel_echoes|_|_|_|common|0|0|choice|1|0|0|0|0
stalled_caravan|_|25|_|common|0|0|choice|1|0|0|0|0
volatile_affix|51|75|_|uncommon|0|0|choice|1|0|0|0|0
rivals_cache|_|25|_|uncommon|0|0|choice|1|0|0|0|0
forsaken_pact|101|150|_|uncommon|0|0|choice|1|0|0|0|0
mapworks_drift|51|75|_|rare|0|0|choice|1|0|0|0|0
the_eye_opens|101|150|_|legendary|1|0|choice|1|1|0|0|0
hollow_court_overture|200|_|_|legendary|0|6|choice|1|0|0|0|0
hollow_court_audience|200|_|_|legendary|0|6|choice|1|0|0|1|0
hollow_court_recess|200|_|_|legendary|0|6|choice|1|0|0|1|0
crimson_drizzle|_|_|_|common|0|0|choice|1|0|0|0|0
glyph_pulse|_|_|_|common|0|0|choice|1|0|0|0|0
mossfire_echo|_|_|_|common|0|0|choice|1|0|0|0|0
mana_fountain_crack|_|_|_|common|0|0|choice|1|0|0|0|0
the_hooked_stranger|10|_|_|uncommon|0|0|choice|1|0|0|0|0
whispering_token|20|_|_|uncommon|0|0|choice|1|0|0|0|0
the_beasts_pit|80|_|_|uncommon|0|0|choice|1|0|0|0|0
aegis_denial|120|_|_|uncommon|0|0|choice|1|0|0|0|0
sanguine_pact|40|_|_|uncommon|0|0|choice|1|0|0|0|0
time_walker|30|_|_|uncommon|0|0|choice|1|0|0|0|0
bounty_marker|40|_|_|uncommon|0|0|choice|1|0|0|0|0
mages_archive|20|_|_|uncommon|0|0|choice|1|0|0|0|0
phantom_strike|30|_|_|uncommon|0|0|choice|1|0|0|0|0
silken_cocoon|10|_|_|uncommon|0|0|choice|1|0|0|0|0
the_mothers_mark|80|_|_|uncommon|0|0|choice|1|0|0|0|0
helltide_bell|_|_|_|legendary|0|0|choice|1|0|0|0|0
the_sundering|120|_|_|legendary|0|0|choice|1|0|0|0|0
the_black_kings_bargain|120|_|_|legendary|0|0|choice|1|0|0|0|0
the_old_gods_tongue|20|_|_|rare|0|0|choice|1|0|0|0|0
crimson_rain_ladder|20|_|_|uncommon|0|0|choice|1|0|0|0|0
agh_s1|25|_|_|common|0|0|choice|1|1|0|0|1
agh_s2|_|_|_|common|0|0|choice|1|1|0|0|1
agh_s3|_|_|_|uncommon|0|0|choice|1|1|0|0|1
agh_s4|_|_|_|uncommon|0|0|choice|1|1|0|0|1
agh_s5|_|_|_|rare|0|0|choice|1|1|0|0|1
necro_s1|_|_|_|common|0|2|choice|1|1|0|0|1
necro_s2|_|_|_|common|0|2|choice|1|1|0|0|1
necro_s3|_|_|_|uncommon|0|2|choice|1|1|0|0|1
necro_s4|_|_|_|uncommon|0|2|choice|1|1|0|0|1
necro_s5|_|_|_|rare|0|2|choice|1|1|0|0|1
bolas_s1|_|_|_|common|0|0|choice|1|1|0|0|1
bolas_s2|_|_|_|common|0|0|choice|1|1|0|0|1
bolas_s3|_|_|_|uncommon|0|0|choice|1|1|0|0|1
bolas_s4|_|_|_|uncommon|0|0|choice|1|1|0|0|1
bolas_s5|_|_|_|rare|0|0|choice|1|1|0|0|1
hungering_dark|10|_|_|uncommon|0|0|choice|1|0|0|0|0
deny_the_seam|20|_|_|uncommon|0|0|choice|1|0|0|0|0
turf_war|35|_|_|uncommon|0|0|choice|1|0|0|0|0
smoke_ambush|60|_|_|rare|0|0|choice|1|0|0|0|0
the_tear|90|_|_|rare|0|0|choice|1|0|0|0|0
the_deep_hunter|110|_|_|rare|0|0|choice|1|0|0|0|0
collapsed_armory|76|150|_|uncommon|0|0|choice|1|0|0|0|0
dead_prospectors_pack|76|200|_|uncommon|0|0|choice|1|0|0|0|0
spore_debt|_|_|Fungal Depths|uncommon|0|0|choice|1|0|0|0|0
clockwork_toll|_|_|Frozen Core|rare|0|0|choice|1|0|0|0|0
surveyors_cairn|25|100|_|uncommon|0|0|choice|1|0|0|0|0
ruinwager_vault|75|200|_|rare|0|0|choice|1|0|0|0|0
red_thread_shrine|125|_|_|rare|0|0|choice|1|0|0|0|0
collectors_roots|51|_|_|uncommon|0|0|choice|1|0|0|0|0
hungry_beacon|101|_|_|rare|0|0|choice|1|0|0|0|0
sealed_reliquary|20|_|_|uncommon|0|0|choice|1|0|0|0|0
grenths_tithe|10|_|_|uncommon|0|0|choice|1|0|0|0|0
rhystic_tollgate|51|_|_|legendary|0|0|choice|1|0|0|0|0
underworld_reclaims|75|_|_|legendary|0|0|choice|1|0|0|0|0
second_life_ledger|20|_|_|uncommon|0|0|choice|1|0|0|0|0
twin_scar_altar|51|_|_|rare|0|0|choice|1|0|0|0|0
ember_clearinghouse|75|_|_|legendary|0|0|choice|1|0|0|0|0
abandoned_forge|20|_|_|common|0|0|choice|1|0|0|0|0
salted_threshold|35|_|_|uncommon|0|0|choice|1|0|0|0|0
frayed_lifeline|50|_|_|common|0|0|choice|1|0|0|0|0
quartermaster_s_niche|75|_|_|uncommon|0|0|choice|1|0|0|0|0
collapsed_armory_cache|90|_|_|rare|0|0|choice|1|0|0|0|0
relic_bearing_strata|110|_|_|rare|0|0|choice|1|0|0|0|0
prospector_s_last_pack|140|_|_|rare|0|0|choice|1|0|0|0|0
song_below_stone|175|_|_|legendary|0|0|choice|1|0|0|0|0
first_descent_cartography|220|_|_|legendary|0|0|choice|1|0|0|0|0"#;

#[must_use]
pub fn event_catalog() -> Vec<EventDef> {
    EVENT_ROWS
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let columns = line.split('|').collect::<Vec<_>>();
            assert_eq!(columns.len(), 13, "invalid authored event row: {line}");
            EventDef {
                id: columns[0],
                min_depth: parse_optional_i64(columns[1]).unwrap_or(0),
                max_depth: parse_optional_i64(columns[2]),
                layer: (columns[3] != "_").then_some(columns[3]),
                rarity: Rarity::parse(columns[4]),
                requires_dark: columns[5] == "1",
                min_prestige: columns[6].parse().expect("valid event prestige"),
                complexity: EventComplexity::parse(columns[7]),
                has_safe_option: columns[8] == "1",
                has_desperate_option: columns[9] == "1",
                boon_count: columns[10].parse().expect("valid boon count"),
                chain_only: columns[11] == "1",
                quest_event: columns[12] == "1",
            }
        })
        .collect()
}

fn parse_optional_i64(value: &str) -> Option<i64> {
    (value != "_").then(|| value.parse().expect("valid optional integer"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactDef {
    pub id: &'static str,
    pub name: &'static str,
    pub layer: &'static str,
    pub rarity: Rarity,
    pub is_relic: bool,
    pub min_prestige: i64,
    pub trophy: bool,
}

const ARTIFACT_ROWS: &str = r#"mole_claws|Mole Claws|Dirt|Uncommon|1|0|0
crystal_compass|Crystal Compass|Crystal|Common|1|0|0
magma_heart|Magma Heart|Magma|Uncommon|1|0|0
obsidian_shield|Obsidian Shield|Magma|Common|1|0|0
root_network|Root Network|Stone|Uncommon|1|0|0
echo_stone|Echo Stone|Crystal|Rare|1|0|0
spore_cloak|Spore Cloak|Fungal Depths|Rare|1|0|0
frozen_clock|Frozen Clock|Frozen Core|Rare|1|0|0
hollow_eye|Hollow Eye|The Hollow|Legendary|1|0|0
mycelium_link|Mycelium Link|Fungal Depths|Common|1|0|0
hollow_fang|Hollow Fang|The Hollow|Legendary|1|5|0
echo_lantern|Echo Lantern|The Hollow|Rare|1|5|0
patient_stone|Patient Stone|The Hollow|Rare|1|5|0
prism_heart|Prism Heart|Crystal|Legendary|1|0|0
mana_conduit|Mana Conduit|Crystal|Uncommon|1|0|0
bloodstone|Bloodstone|Magma|Legendary|1|0|0
gamblers_charm|Gambler's Charm|Magma|Rare|1|0|0
vendetta_coin|Vendetta Coin|The Hollow|Legendary|1|0|0
mentors_lantern|Mentor's Lantern|Dirt|Uncommon|1|0|0
stormcaller|Stormcaller|Stone|Rare|1|0|0
slow_drip|Slow Drip|Dirt|Rare|1|0|0
weeping_fang|Weeping Fang|Fungal Depths|Uncommon|1|4|1
runebitten_shard|Runebitten Shard|Frozen Core|Legendary|1|4|1
aching_spine|Aching Spine|The Hollow|Legendary|1|4|1
listening_shard|Listening Shard|Fungal Depths|Rare|1|3|1
hateborn_ember|Hateborn Ember|Frozen Core|Rare|1|3|1
deepveined_coal|Deepveined Coal|The Hollow|Legendary|1|4|0
diviners_knot|Diviner's Knot|Frozen Core|Rare|1|4|0
pathfinders_spur|Pathfinder's Spur|Fungal Depths|Rare|1|4|0
aegis_fragment|Aegis Fragment|The Hollow|Legendary|1|0|0
midas_splinter|Midas Splinter|Dirt|Common|1|0|0
lucky_seam|Lucky Seam|Stone|Common|1|0|0
prospectors_streak|Prospector's Streak|Stone|Uncommon|1|0|0
chipped_compass|Chipped Compass|Dirt|Common|1|0|0
lantern_stub|Lantern Stub|Dirt|Common|1|0|0
paper_crane|Paper Crane|Crystal|Uncommon|1|0|0
bone_abacus|Bone Abacus|Stone|Uncommon|1|0|0
bottled_quake|Bottled Quake|Magma|Rare|1|0|0
black_wax_seal|Black Wax Seal|The Hollow|Rare|1|0|0
burning_ledger|Burning Ledger|The Hollow|Legendary|1|0|0
shifting_idol|Shifting Idol|Crystal|Legendary|1|0|0
first_light|First Light|Dirt|Rare|1|2|0
berserkers_mark|Berserker's Mark|Magma|Rare|1|3|0
gamblers_edge|Gambler's Edge|The Hollow|Rare|1|4|0
deaths_door|Death's Door|The Hollow|Legendary|1|5|1
miner_s_lullaby|The Miner's Lullaby|The Hollow|Legendary|0|0|0
map_of_the_first_descent|Map of the First Descent|Frozen Core|Legendary|0|0|0"#;

#[must_use]
pub fn artifact_catalog() -> Vec<ArtifactDef> {
    ARTIFACT_ROWS
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let columns = line.split('|').collect::<Vec<_>>();
            assert_eq!(columns.len(), 7, "invalid authored artifact row: {line}");
            ArtifactDef {
                id: columns[0],
                name: columns[1],
                layer: columns[2],
                rarity: Rarity::parse(&columns[3].to_ascii_lowercase()),
                is_relic: columns[4] == "1",
                min_prestige: columns[5].parse().expect("valid artifact prestige"),
                trophy: columns[6] == "1",
            }
        })
        .collect()
}

pub trait LootEntropy {
    fn unit(&mut self) -> f64;
    fn advance(&mut self, minimum: i64, maximum: i64) -> i64;

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            return 0;
        }
        ((self.unit().clamp(0.0, 1.0 - f64::EPSILON) * upper_bound as f64) as usize)
            .min(upper_bound - 1)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScriptedLootEntropy {
    units: VecDeque<f64>,
    advances: VecDeque<i64>,
}

impl ScriptedLootEntropy {
    #[must_use]
    pub fn new(
        units: impl IntoIterator<Item = f64>,
        advances: impl IntoIterator<Item = i64>,
    ) -> Self {
        Self {
            units: units.into_iter().collect(),
            advances: advances.into_iter().collect(),
        }
    }

    pub fn push_unit(&mut self, value: f64) {
        self.units.push_back(value);
    }

    pub fn push_advance(&mut self, value: i64) {
        self.advances.push_back(value);
    }
}

impl LootEntropy for ScriptedLootEntropy {
    fn unit(&mut self) -> f64 {
        self.units.pop_front().unwrap_or(0.99)
    }

    fn advance(&mut self, minimum: i64, maximum: i64) -> i64 {
        self.advances
            .pop_front()
            .unwrap_or(minimum)
            .clamp(minimum, maximum)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeededLootEntropy {
    state: u64,
}

impl SeededLootEntropy {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

impl LootEntropy for SeededLootEntropy {
    fn unit(&mut self) -> f64 {
        let numerator = self.next_u64() >> 11;
        numerator as f64 / (1_u64 << 53) as f64
    }

    fn advance(&mut self, minimum: i64, maximum: i64) -> i64 {
        let width = (maximum - minimum + 1).max(1) as u64;
        minimum + (self.next_u64() % width) as i64
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelLootState {
    pub depth: i64,
    pub luminosity: i64,
    pub injured: bool,
    pub hard_hat_charges: i64,
    pub reinforced_until: i64,
    pub void_bait_digs: i64,
    pub sonar_skip_pending: bool,
    pub grappling_hook_charges: i64,
    pub temp_buff: Option<String>,
}

impl Default for TunnelLootState {
    fn default() -> Self {
        Self {
            depth: 0,
            luminosity: 100,
            injured: false,
            hard_hat_charges: 0,
            reinforced_until: 0,
            void_bait_digs: 0,
            sonar_skip_pending: false,
            grappling_hook_charges: 0,
            temp_buff: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryItem {
    pub id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub item_type: &'static str,
    pub queued: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryError {
    InsufficientFunds,
    MissingTunnel,
    MissingItem,
    InvalidArtifact,
    ForcedWriteFailure,
}

pub trait LootRepository {
    fn has_tunnel(&self, discord_id: i64, guild_id: i64) -> bool;
    fn tunnel(&self, discord_id: i64, guild_id: i64) -> Option<TunnelLootState>;
    fn set_tunnel(&mut self, discord_id: i64, guild_id: i64, tunnel: TunnelLootState);
    fn balance(&self, discord_id: i64, guild_id: i64) -> i64;
    fn inventory(&self, discord_id: i64, guild_id: i64) -> Vec<InventoryItem>;
    fn atomic_buy_item(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        item_type: &'static str,
        cost: i64,
        queued: bool,
    ) -> Result<i64, RepositoryError>;
    fn queue_item(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        item_id: i64,
    ) -> Result<(), RepositoryError>;
    fn atomic_commit_dig(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        tunnel: TunnelLootState,
        consumed_item_ids: &[i64],
    ) -> Result<(), RepositoryError>;
    /// Commit the tunnel leg of an event settlement atomically with its JC
    /// delta.  Adapters that own reward/curse tables may override this
    /// compatibility default to apply those typed fields in the same SQL
    /// transaction; the default keeps older adapters source-compatible.
    fn atomic_commit_event(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        tunnel: TunnelLootState,
        balance_delta: i64,
    ) -> Result<(), RepositoryError> {
        if !self.has_tunnel(discord_id, guild_id) {
            return Err(RepositoryError::MissingTunnel);
        }
        let _ = balance_delta;
        self.set_tunnel(discord_id, guild_id, tunnel);
        Ok(())
    }
    fn add_artifact(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        artifact_id: &str,
        is_relic: bool,
    ) -> Result<i64, RepositoryError>;
    fn artifacts(&self, discord_id: i64, guild_id: i64) -> Vec<Artifact>;
    fn atomic_gift_relic(
        &mut self,
        giver_id: i64,
        receiver_id: i64,
        guild_id: i64,
        artifact_db_id: i64,
    ) -> Result<(), RepositoryError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteFault {
    Purchase,
    Dig,
    Gift,
    Event,
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryLootRepository {
    balances: BTreeMap<(i64, i64), i64>,
    tunnels: BTreeMap<(i64, i64), TunnelLootState>,
    inventory: Vec<InventoryItem>,
    artifacts: Vec<Artifact>,
    next_inventory_id: i64,
    next_artifact_id: i64,
    fail_next: Option<WriteFault>,
}

impl InMemoryLootRepository {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_inventory_id: 1,
            next_artifact_id: 1,
            ..Self::default()
        }
    }

    pub fn register_player(&mut self, discord_id: i64, guild_id: i64, balance: i64) {
        self.balances.insert((discord_id, guild_id), balance);
    }

    pub fn create_tunnel(&mut self, discord_id: i64, guild_id: i64) {
        self.tunnels.entry((discord_id, guild_id)).or_default();
    }

    pub fn fail_next_write(&mut self, fault: WriteFault) {
        self.fail_next = Some(fault);
    }

    fn take_fault(&mut self, expected: WriteFault) -> bool {
        if self.fail_next == Some(expected) {
            self.fail_next = None;
            true
        } else {
            false
        }
    }
}

impl LootRepository for InMemoryLootRepository {
    fn has_tunnel(&self, discord_id: i64, guild_id: i64) -> bool {
        self.tunnels.contains_key(&(discord_id, guild_id))
    }

    fn tunnel(&self, discord_id: i64, guild_id: i64) -> Option<TunnelLootState> {
        self.tunnels.get(&(discord_id, guild_id)).cloned()
    }

    fn set_tunnel(&mut self, discord_id: i64, guild_id: i64, tunnel: TunnelLootState) {
        self.tunnels.insert((discord_id, guild_id), tunnel);
    }

    fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
        self.balances
            .get(&(discord_id, guild_id))
            .copied()
            .unwrap_or_default()
    }

    fn inventory(&self, discord_id: i64, guild_id: i64) -> Vec<InventoryItem> {
        self.inventory
            .iter()
            .filter(|item| item.discord_id == discord_id && item.guild_id == guild_id)
            .cloned()
            .collect()
    }

    fn atomic_buy_item(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        item_type: &'static str,
        cost: i64,
        queued: bool,
    ) -> Result<i64, RepositoryError> {
        if !self.has_tunnel(discord_id, guild_id) {
            return Err(RepositoryError::MissingTunnel);
        }
        let mut staged = self.clone();
        let balance = staged.balances.entry((discord_id, guild_id)).or_default();
        if *balance < cost {
            return Err(RepositoryError::InsufficientFunds);
        }
        *balance -= cost;
        let item_id = staged.next_inventory_id;
        staged.next_inventory_id += 1;
        staged.inventory.push(InventoryItem {
            id: item_id,
            discord_id,
            guild_id,
            item_type,
            queued,
        });
        if self.take_fault(WriteFault::Purchase) {
            return Err(RepositoryError::ForcedWriteFailure);
        }
        staged.fail_next = self.fail_next;
        *self = staged;
        Ok(item_id)
    }

    fn queue_item(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        item_id: i64,
    ) -> Result<(), RepositoryError> {
        let Some(target) = self.inventory.iter().find(|item| {
            item.id == item_id && item.discord_id == discord_id && item.guild_id == guild_id
        }) else {
            return Err(RepositoryError::MissingItem);
        };
        if self.inventory.iter().any(|item| {
            item.discord_id == discord_id
                && item.guild_id == guild_id
                && item.item_type == target.item_type
                && item.queued
        }) {
            return Err(RepositoryError::MissingItem);
        }
        if let Some(item) = self.inventory.iter_mut().find(|item| item.id == item_id) {
            item.queued = true;
        }
        Ok(())
    }

    fn atomic_commit_dig(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        tunnel: TunnelLootState,
        consumed_item_ids: &[i64],
    ) -> Result<(), RepositoryError> {
        let mut staged = self.clone();
        if consumed_item_ids.iter().any(|item_id| {
            !staged.inventory.iter().any(|item| {
                item.id == *item_id
                    && item.discord_id == discord_id
                    && item.guild_id == guild_id
                    && item.queued
            })
        }) {
            return Err(RepositoryError::MissingItem);
        }
        staged
            .inventory
            .retain(|item| !consumed_item_ids.contains(&item.id));
        staged.tunnels.insert((discord_id, guild_id), tunnel);
        if self.take_fault(WriteFault::Dig) {
            return Err(RepositoryError::ForcedWriteFailure);
        }
        staged.fail_next = self.fail_next;
        *self = staged;
        Ok(())
    }

    fn atomic_commit_event(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        tunnel: TunnelLootState,
        balance_delta: i64,
    ) -> Result<(), RepositoryError> {
        if !self.has_tunnel(discord_id, guild_id) {
            return Err(RepositoryError::MissingTunnel);
        }
        let mut staged = self.clone();
        *staged.balances.entry((discord_id, guild_id)).or_default() += balance_delta;
        staged.tunnels.insert((discord_id, guild_id), tunnel);
        if self.take_fault(WriteFault::Event) {
            return Err(RepositoryError::ForcedWriteFailure);
        }
        staged.fail_next = self.fail_next;
        *self = staged;
        Ok(())
    }

    fn add_artifact(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        artifact_id: &str,
        is_relic: bool,
    ) -> Result<i64, RepositoryError> {
        let id = self.next_artifact_id;
        self.next_artifact_id += 1;
        self.artifacts.push(Artifact {
            id,
            discord_id,
            guild_id,
            artifact_id: artifact_id.to_owned(),
            is_relic,
            equipped: false,
        });
        Ok(id)
    }

    fn artifacts(&self, discord_id: i64, guild_id: i64) -> Vec<Artifact> {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.discord_id == discord_id && artifact.guild_id == guild_id)
            .cloned()
            .collect()
    }

    fn atomic_gift_relic(
        &mut self,
        giver_id: i64,
        receiver_id: i64,
        guild_id: i64,
        artifact_db_id: i64,
    ) -> Result<(), RepositoryError> {
        if !self.has_tunnel(receiver_id, guild_id) {
            return Err(RepositoryError::MissingTunnel);
        }
        let mut staged = self.clone();
        let Some(artifact) = staged.artifacts.iter_mut().find(|artifact| {
            artifact.id == artifact_db_id
                && artifact.discord_id == giver_id
                && artifact.guild_id == guild_id
                && artifact.is_relic
        }) else {
            return Err(RepositoryError::InvalidArtifact);
        };
        artifact.discord_id = receiver_id;
        artifact.equipped = false;
        if self.take_fault(WriteFault::Gift) {
            return Err(RepositoryError::ForcedWriteFailure);
        }
        staged.fail_next = self.fail_next;
        *self = staged;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LootActionResult {
    pub success: bool,
    pub error: Option<String>,
    pub item_id: Option<i64>,
    pub item: Option<&'static str>,
    pub cost: i64,
    pub queued: bool,
    pub artifact_id: Option<String>,
    pub artifact_name: Option<&'static str>,
    pub choice: Option<String>,
    pub buff_applied: Option<String>,
    /// Typed event settlement payload.  These fields are populated by
    /// `resolve_event`; non-event actions retain their zero/empty defaults.
    pub event_name: Option<String>,
    pub succeeded: Option<bool>,
    pub message: Option<String>,
    pub advance: i64,
    pub jc_delta: i64,
    pub cave_in: bool,
    pub streak_loss: i64,
    pub curse: Option<String>,
    pub gear_reward_pool: Vec<String>,
    pub consumable_reward_pool: Vec<String>,
    pub artifact_reward_pool: Vec<String>,
}

impl LootActionResult {
    fn error(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigLootOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub advance: i64,
    pub cave_in: bool,
    pub dynamite_bonus: i64,
    pub has_lantern: bool,
    pub event: Option<String>,
    pub event_preview_included: bool,
    pub event_preview: Option<String>,
    pub sonar_skipped: bool,
    /// The event-gate draw is kept separate from the catalog selection.  The
    /// application runtime can therefore apply the gate after the post-boss
    /// depth is known while preserving the single RNG draw used by Python.
    pub event_roll_bits: Option<u64>,
    pub items_used: Vec<&'static str>,
}

/// Environment modifiers supplied by the application orchestrator.
///
/// The original loot service deliberately owns only consumables and authored
/// event selection. Routes, weather, gear, and prestige policies live one
/// layer above it; this value lets that layer compose their effects without
/// forking the atomic loot commit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DigLootModifiers {
    pub cave_in_chance_bonus: f64,
    pub cave_in_chance_multiplier: f64,
    pub advance_bonus: i64,
    pub advance_min: Option<i64>,
    pub advance_max: Option<i64>,
    pub event_chance_multiplier: f64,
    pub luminosity_drain_multiplier: f64,
    pub luminosity_drain_bonus: i64,
    pub luminosity_pickaxe_reduction: bool,
    /// Injury is ticked by the application before the roll.  Carry the
    /// already-observed reduced-advance flag so the final charge still
    /// affects this Dig after its persisted JSON is cleared.
    pub injury_reduces_advance: bool,
    /// Yield modifiers are applied by the enclosing application settlement;
    /// the loot service carries them so weather/gear/relic policy is not lost
    /// between the policy graph and the final transaction.
    pub jc_multiplier: f64,
    pub jc_bonus: i64,
    pub artifact_multiplier: f64,
    pub cave_in_loss_bonus: i64,
    pub cave_in_loss_cap: Option<i64>,
    /// Let the application-level canonical event picker own catalog
    /// selection.  The loot service still consumes the event-gate draw and
    /// applies queued tunnel-item state, but it never performs the legacy
    /// catalog roll when this is enabled.
    pub defer_event_selection: bool,
}

impl Default for DigLootModifiers {
    fn default() -> Self {
        Self {
            cave_in_chance_bonus: 0.0,
            cave_in_chance_multiplier: 1.0,
            advance_bonus: 0,
            advance_min: None,
            advance_max: None,
            event_chance_multiplier: 1.0,
            luminosity_drain_multiplier: 1.0,
            luminosity_drain_bonus: 0,
            luminosity_pickaxe_reduction: false,
            injury_reduces_advance: false,
            jc_multiplier: 1.0,
            jc_bonus: 0,
            artifact_multiplier: 1.0,
            cave_in_loss_bonus: 0,
            cave_in_loss_cap: None,
            defer_event_selection: false,
        }
    }
}

pub struct DigLootService<R, E> {
    repository: R,
    entropy: E,
    events: Vec<EventDef>,
    artifacts: Vec<ArtifactDef>,
}

impl<R, E> DigLootService<R, E>
where
    R: LootRepository,
    E: LootEntropy,
{
    #[must_use]
    pub fn new(repository: R, entropy: E) -> Self {
        Self {
            repository,
            entropy,
            events: event_catalog(),
            artifacts: artifact_catalog(),
        }
    }

    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    pub const fn repository_mut(&mut self) -> &mut R {
        &mut self.repository
    }

    #[must_use]
    pub const fn entropy_mut(&mut self) -> &mut E {
        &mut self.entropy
    }

    #[must_use]
    pub fn events(&self) -> &[EventDef] {
        &self.events
    }

    pub fn buy_item(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        item_type: &str,
    ) -> LootActionResult {
        let Some(definition) = consumable(item_type) else {
            return LootActionResult::error(format!("Unknown item type: {item_type}"));
        };
        if !self.repository.has_tunnel(discord_id, guild_id) {
            return LootActionResult::error("You don't have a tunnel. Dig first!");
        }
        if self.repository.inventory(discord_id, guild_id).len() >= MAX_INVENTORY_SLOTS {
            return LootActionResult::error(format!(
                "Inventory full ({MAX_INVENTORY_SLOTS} items max)."
            ));
        }
        let auto_queue = matches!(definition.id, "hard_hat" | "torch")
            && !self
                .repository
                .inventory(discord_id, guild_id)
                .iter()
                .any(|item| item.queued && item.item_type == definition.id);
        match self.repository.atomic_buy_item(
            discord_id,
            guild_id,
            definition.id,
            definition.cost,
            auto_queue,
        ) {
            Ok(item_id) => LootActionResult {
                success: true,
                item_id: Some(item_id),
                item: Some(definition.name),
                cost: definition.cost,
                queued: auto_queue,
                ..LootActionResult::default()
            },
            Err(RepositoryError::InsufficientFunds) => LootActionResult::error(format!(
                "Costs {} JC but you only have {} JC.",
                definition.cost,
                self.repository.balance(discord_id, guild_id)
            )),
            Err(_) => LootActionResult::error("Item purchase did not commit."),
        }
    }

    pub fn queue_item(&mut self, discord_id: i64, guild_id: i64, item_id: i64) -> LootActionResult {
        match self.repository.queue_item(discord_id, guild_id, item_id) {
            Ok(()) => LootActionResult {
                success: true,
                queued: true,
                ..LootActionResult::default()
            },
            Err(_) => LootActionResult::error("That item is not in your inventory."),
        }
    }

    pub fn use_item(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        item_type: &str,
    ) -> LootActionResult {
        let Some(definition) = consumable(item_type) else {
            return LootActionResult::error(format!("Unknown item type: {item_type}"));
        };
        if definition.id == "streak_charm" {
            return LootActionResult::error("Streak Charm is passive and triggers automatically.");
        }
        let Some(item) = self
            .repository
            .inventory(discord_id, guild_id)
            .into_iter()
            .find(|item| item.item_type == definition.id && !item.queued)
        else {
            return LootActionResult::error(format!("You don't have a {}.", definition.name));
        };
        let mut result = self.queue_item(discord_id, guild_id, item.id);
        result.item = Some(definition.name);
        result
    }

    pub fn dig(&mut self, discord_id: i64, guild_id: i64, now: i64) -> DigLootOutcome {
        self.dig_with_modifiers(discord_id, guild_id, now, DigLootModifiers::default())
    }

    pub fn dig_with_modifiers(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        _now: i64,
        modifiers: DigLootModifiers,
    ) -> DigLootOutcome {
        let Some(mut tunnel) = self.repository.tunnel(discord_id, guild_id) else {
            return DigLootOutcome {
                error: Some("You don't have a tunnel.".to_owned()),
                ..DigLootOutcome::default()
            };
        };
        let queued = self
            .repository
            .inventory(discord_id, guild_id)
            .into_iter()
            .filter(|item| {
                item.queued
                    && is_dig_consumable(item.item_type)
                    && !is_boss_prep_item(item.item_type)
            })
            .collect::<Vec<_>>();
        let consumed_ids = queued.iter().map(|item| item.id).collect::<Vec<_>>();
        let mut items_used = Vec::new();
        let mut flat_advance_bonus = 0;
        let mut has_lantern = false;
        let mut sonar_used = false;
        let sonar_skip_active = tunnel.sonar_skip_pending;

        for item in &queued {
            items_used.push(item.item_type);
            match item.item_type {
                "dynamite" | "depth_charge" => {
                    flat_advance_bonus +=
                        consumable(item.item_type).map_or(0, |definition| definition.bonus_blocks);
                }
                "hard_hat" => {
                    tunnel.hard_hat_charges = tunnel.hard_hat_charges.saturating_add(HARD_HAT_USES)
                }
                "lantern" => has_lantern = true,
                // The application uses the pre-stage value for this Dig's
                // cave-loss cap, then carries this staged value into the
                // committed tunnel for the following Dig.
                "reinforcement" => tunnel.reinforced_until = _now + REINFORCEMENT_SECONDS,
                // Torch is restored after the ordinary drain, in the same
                // order as Python's _apply_luminosity_drain pipeline.
                "torch" => {}
                "grappling_hook" => {
                    tunnel.grappling_hook_charges = tunnel
                        .grappling_hook_charges
                        .saturating_add(GRAPPLING_HOOK_USES)
                }
                "void_bait" => tunnel.void_bait_digs = tunnel.void_bait_digs.saturating_add(3),
                "sonar_pulse" => sonar_used = true,
                _ => {}
            }
        }

        let layer = layer_at(tunnel.depth);
        let mut cave_in_chance = (layer.cave_in_chance + modifiers.cave_in_chance_bonus)
            * modifiers.cave_in_chance_multiplier.max(0.0);
        if has_lantern {
            cave_in_chance *= 0.5;
        }
        let cave_roll = self.entropy.unit();
        let would_cave = cave_roll < cave_in_chance;
        let cave_in = would_cave && tunnel.hard_hat_charges <= 0;
        if would_cave && tunnel.hard_hat_charges > 0 {
            tunnel.hard_hat_charges -= 1;
        }

        let minimum = modifiers
            .advance_min
            .unwrap_or(layer.advance_range.0)
            .max(0);
        let maximum = modifiers
            .advance_max
            .unwrap_or(layer.advance_range.1)
            .max(minimum);
        let base_advance = self.entropy.advance(minimum, maximum);
        let injury_adjusted = if tunnel.injured || modifiers.injury_reduces_advance {
            (base_advance as f64 * 0.5) as i64
        } else {
            base_advance
        };
        let advance = if cave_in {
            0
        } else {
            (injury_adjusted.max(1) + modifiers.advance_bonus).max(1) + flat_advance_bonus
        };
        tunnel.depth += advance;

        let mut base_luminosity_drain = layer.luminosity_drain;
        if modifiers.luminosity_pickaxe_reduction {
            base_luminosity_drain = base_luminosity_drain.saturating_sub(base_luminosity_drain / 4);
        }
        base_luminosity_drain =
            base_luminosity_drain.saturating_add(modifiers.luminosity_drain_bonus);
        let luminosity_drain =
            (base_luminosity_drain as f64 * modifiers.luminosity_drain_multiplier.max(0.0)) as i64;
        tunnel.luminosity = tunnel.luminosity.saturating_sub(luminosity_drain).max(0);

        let mut event = None;
        let mut sonar_skipped = false;
        let event_roll_bits = if cave_in {
            None
        } else {
            let event_roll = self.entropy.unit();
            if tunnel.void_bait_digs > 0 {
                tunnel.void_bait_digs -= 1;
            }
            if !modifiers.defer_event_selection {
                let mut event_chance = match layer.name {
                    "Crystal" | "Magma" => 0.27,
                    "Abyss" | "Frozen Core" => 0.31,
                    "Fungal Depths" => 0.38,
                    "The Hollow" => 0.45,
                    _ => 0.22,
                };
                event_chance *= modifiers.event_chance_multiplier.max(0.0);
                if event_roll < event_chance {
                    if sonar_skip_active {
                        sonar_skipped = true;
                        tunnel.sonar_skip_pending = false;
                    } else if let Some(rolled) = roll_event_from_catalog(
                        &self.events,
                        tunnel.depth,
                        tunnel.luminosity,
                        0,
                        false,
                        &mut self.entropy,
                    ) {
                        event = Some(rolled.id.to_owned());
                    }
                }
            }
            Some(event_roll.to_bits())
        };

        let event_preview = if sonar_used {
            eligible_events(&self.events, tunnel.depth, tunnel.luminosity, 0, false)
                .first()
                .map(|candidate| candidate.id.to_owned())
        } else {
            None
        };
        if sonar_used {
            tunnel.sonar_skip_pending = true;
        }

        if let Err(error) =
            self.repository
                .atomic_commit_dig(discord_id, guild_id, tunnel, &consumed_ids)
        {
            return DigLootOutcome {
                error: Some(format!("Dig did not commit: {error:?}")),
                ..DigLootOutcome::default()
            };
        }

        DigLootOutcome {
            success: true,
            advance,
            cave_in,
            dynamite_bonus: flat_advance_bonus,
            has_lantern,
            event,
            event_preview_included: sonar_used,
            event_preview,
            sonar_skipped,
            event_roll_bits,
            items_used,
            error: None,
        }
    }

    pub fn roll_event(
        &mut self,
        depth: i64,
        luminosity: i64,
        prestige_level: i64,
    ) -> Option<EventDef> {
        roll_event_from_catalog(
            &self.events,
            depth,
            luminosity,
            prestige_level,
            false,
            &mut self.entropy,
        )
        .copied()
    }

    pub fn resolve_event(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        event_id: &str,
        choice: &str,
    ) -> LootActionResult {
        let Some(mut tunnel) = self.repository.tunnel(discord_id, guild_id) else {
            return LootActionResult::error("You don't have a tunnel.");
        };
        // The application owns the success roll and the authored outcome
        // payload.  A caller that needs Python's additional jitter/scaling
        // supplies those as a later settlement step; no authored value is
        // discarded at this boundary.
        let success_roll = if choice.starts_with("boon_") {
            0.0
        } else {
            self.entropy.unit()
        };
        let resolution = match resolve_canonical_event(event_id, choice, success_roll) {
            Ok(resolution) => resolution,
            Err(message) => return LootActionResult::error(message),
        };

        if let Some(buff) = &resolution.buff {
            tunnel.temp_buff = Some(
                serde_json::json!({
                    "id": buff.id,
                    "name": buff.name,
                    "digs_remaining": buff.duration_digs,
                    "effect": buff.effect,
                })
                .to_string(),
            );
        }
        if resolution.advance != 0 {
            tunnel.depth = tunnel.depth.saturating_add(resolution.advance).max(0);
        }
        // The repository port is the atomic boundary.  Full SQLite adapters
        // override this method to include reward/curse/audit rows in the
        // same transaction; the compatibility implementation still commits
        // the tunnel snapshot for older adapters.
        if let Err(error) =
            self.repository
                .atomic_commit_event(discord_id, guild_id, tunnel, resolution.jc)
        {
            return LootActionResult::error(format!("Event did not commit: {error:?}"));
        }
        let buff_applied = resolution.buff.as_ref().map(|buff| buff.name.clone());
        LootActionResult {
            success: true,
            choice: Some(resolution.choice.clone()),
            buff_applied,
            event_name: Some(resolution.event_name),
            succeeded: Some(resolution.succeeded),
            message: Some(resolution.message),
            advance: resolution.advance,
            jc_delta: resolution.jc,
            cave_in: resolution.cave_in,
            streak_loss: resolution.streak_loss,
            curse: resolution
                .curse
                .map(|curse| serde_json::to_string(&curse).unwrap_or_default()),
            gear_reward_pool: resolution.gear_reward_pool,
            consumable_reward_pool: resolution.consumable_reward_pool,
            artifact_reward_pool: resolution.artifact_reward_pool,
            ..LootActionResult::default()
        }
    }

    pub fn add_artifact(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        artifact_id: &str,
    ) -> Result<i64, RepositoryError> {
        let is_relic = self
            .artifacts
            .iter()
            .find(|artifact| artifact.id == artifact_id)
            .is_some_and(|artifact| artifact.is_relic);
        self.repository
            .add_artifact(discord_id, guild_id, artifact_id, is_relic)
    }

    #[must_use]
    pub fn relic_autocomplete(&self, discord_id: i64, guild_id: i64) -> Vec<AutocompleteChoice> {
        self.repository
            .artifacts(discord_id, guild_id)
            .into_iter()
            .filter(|artifact| artifact.is_relic)
            .filter_map(|artifact| {
                self.artifacts
                    .iter()
                    .find(|definition| definition.id == artifact.artifact_id)
                    .map(|definition| AutocompleteChoice {
                        label: definition.name.to_owned(),
                        value: definition.id.to_owned(),
                    })
            })
            .collect()
    }

    pub fn gift_relic(
        &mut self,
        giver_id: i64,
        receiver_id: i64,
        guild_id: i64,
        selector: ArtifactSelector<'_>,
    ) -> LootActionResult {
        if giver_id == receiver_id {
            return LootActionResult::error("You can't gift to yourself.");
        }
        let target = self
            .repository
            .artifacts(giver_id, guild_id)
            .into_iter()
            .find(|artifact| match selector {
                ArtifactSelector::DatabaseId(id) => artifact.id == id,
                ArtifactSelector::CatalogId(id) => artifact.artifact_id == id,
            });
        let Some(target) = target else {
            return LootActionResult::error("You don't have that artifact.");
        };
        if !target.is_relic {
            return LootActionResult::error("Only relics can be gifted.");
        }
        let Some(definition) = self
            .artifacts
            .iter()
            .find(|definition| definition.id == target.artifact_id)
        else {
            return LootActionResult::error("Unknown relic.");
        };
        match self
            .repository
            .atomic_gift_relic(giver_id, receiver_id, guild_id, target.id)
        {
            Ok(()) => LootActionResult {
                success: true,
                artifact_id: Some(definition.id.to_owned()),
                artifact_name: Some(definition.name),
                ..LootActionResult::default()
            },
            Err(RepositoryError::MissingTunnel) => {
                LootActionResult::error("Receiver doesn't have a tunnel.")
            }
            Err(_) => LootActionResult::error("Relic gift did not commit."),
        }
    }

    pub fn maybe_drop_prestige_relic(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        prestige_level: i64,
    ) -> Option<ArtifactDef> {
        let owned = self.repository.artifacts(discord_id, guild_id);
        let eligible = self
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.is_relic
                    && artifact.min_prestige > 0
                    && artifact.min_prestige <= prestige_level
                    && !artifact.trophy
                    && !owned.iter().any(|row| row.artifact_id == artifact.id)
            })
            .copied()
            .collect::<Vec<_>>();
        if eligible.is_empty() || self.entropy.unit() >= PRESTIGE_RELIC_DROP_RATE {
            return None;
        }
        let choice = eligible[self.entropy.choose_index(eligible.len())];
        self.repository
            .add_artifact(discord_id, guild_id, choice.id, true)
            .ok()?;
        Some(choice)
    }
}

#[must_use]
pub fn eligible_events(
    events: &[EventDef],
    depth: i64,
    luminosity: i64,
    prestige_level: i64,
    include_quest_events: bool,
) -> Vec<&EventDef> {
    let layer_name = layer_at(depth).name;
    events
        .iter()
        .filter(|event| depth >= event.min_depth)
        .filter(|event| event.max_depth.is_none_or(|maximum| depth <= maximum))
        .filter(|event| event.layer.is_none_or(|layer| layer == layer_name))
        .filter(|event| !event.requires_dark || luminosity <= 0)
        .filter(|event| prestige_level >= event.min_prestige)
        .filter(|event| !event.chain_only)
        .filter(|event| include_quest_events || !event.quest_event)
        .collect()
}

fn roll_event_from_catalog<'a>(
    events: &'a [EventDef],
    depth: i64,
    luminosity: i64,
    prestige_level: i64,
    include_quest_events: bool,
    entropy: &mut impl LootEntropy,
) -> Option<&'a EventDef> {
    let eligible = eligible_events(
        events,
        depth,
        luminosity,
        prestige_level,
        include_quest_events,
    );
    let total = eligible
        .iter()
        .map(|event| event.rarity.weight())
        .sum::<u64>();
    if total == 0 {
        return None;
    }
    let mut threshold = (entropy.unit().clamp(0.0, 1.0 - f64::EPSILON) * total as f64) as u64;
    for event in eligible {
        let weight = event.rarity.weight();
        if threshold < weight {
            return Some(event);
        }
        threshold -= weight;
    }
    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactSelector<'a> {
    DatabaseId(i64),
    CatalogId(&'a str),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutocompleteChoice {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionReply {
    pub content: String,
    pub ephemeral: bool,
}

#[must_use]
pub fn gift_command_reply(result: &LootActionResult, receiver_name: &str) -> InteractionReply {
    if !result.success {
        return InteractionReply {
            content: result
                .error
                .clone()
                .unwrap_or_else(|| "Gift failed.".to_owned()),
            ephemeral: true,
        };
    }
    InteractionReply {
        content: format!(
            "You gifted **{}** to **{receiver_name}**.",
            result.artifact_name.unwrap_or("Unknown Relic")
        ),
        ephemeral: false,
    }
}

#[must_use]
pub fn trap_command_reply(result: &LootActionResult) -> InteractionReply {
    if result.success {
        InteractionReply {
            content: "Trap set.".to_owned(),
            ephemeral: false,
        }
    } else {
        InteractionReply {
            content: result
                .error
                .clone()
                .unwrap_or_else(|| "No trap placed.".to_owned()),
            ephemeral: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoPurchasePresentation {
    pub item: String,
    pub status: AutoBuyStatus,
    pub cost: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DigPresentation {
    pub depth: i64,
    pub advance: i64,
    pub pet_dig_bonus: i64,
    pub pet_name: Option<String>,
    pub streak_charm_used: bool,
    pub auto_purchases: Vec<AutoPurchasePresentation>,
    pub cave_in: bool,
    pub broken_gear: Vec<String>,
}

#[must_use]
pub fn build_dig_embed(result: &DigPresentation) -> EmbedModel {
    let mut embed = EmbedModel {
        title: Some("Tunnel Progress".to_owned()),
        ..EmbedModel::default()
    };
    let mut progress = format!(
        "Advanced **{}** blocks to depth **{}**.",
        result.advance, result.depth
    );
    if result.pet_dig_bonus > 0 {
        progress.push_str(&format!(
            "\n{} excavated +{} blocks.",
            result.pet_name.as_deref().unwrap_or("Your pet"),
            result.pet_dig_bonus
        ));
    }
    embed.add_field("Progress", progress, false);
    if result.streak_charm_used {
        embed.add_field(
            "Streak Charm",
            "Saved your daily streak after one missed day.",
            false,
        );
    }
    if !result.auto_purchases.is_empty() {
        let lines = result
            .auto_purchases
            .iter()
            .map(|purchase| match purchase.status {
                AutoBuyStatus::Purchased => format!("{}: bought", purchase.item),
                AutoBuyStatus::QueuedFromInventory => {
                    format!("{}: queued from inventory", purchase.item)
                }
                AutoBuyStatus::SkippedInsufficientBalance => {
                    format!("{}: skipped (need {} JC)", purchase.item, purchase.cost)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        embed.add_field("Auto-Buy", lines, false);
    }
    if result.cave_in && !result.broken_gear.is_empty() {
        embed.add_field(
            "Gear Broken",
            format!(
                "{} — effects disabled until repaired. Use **Repair All**.",
                result.broken_gear.join(", ")
            ),
            false,
        );
    }
    embed
}

#[cfg(test)]
#[path = "dig_loot/tests.rs"]
mod tests;
