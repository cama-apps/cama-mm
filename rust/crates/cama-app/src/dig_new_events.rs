//! Typed policy for the cross-player Dig event expansion.
//!
//! Python remains the live Discord runtime and catalog authority during the
//! migration. This module mirrors the authored events exercised by the
//! expansion tests, injects every random choice, and routes wallet/audit
//! mutations through an atomic economy port. The SQLite implementation opens
//! Python's existing schema and is supplied by `cama-db`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use cama_domain::economy_scaling::{
    DEFAULT_MINIGAME_JC_DELTA_SCALE, scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta,
};

use crate::dig_loot::Rarity;
use crate::dig_service::layer_at;

pub const HOSTILE_LOSS_MIN_BALANCE: i64 = 50;
pub const EVENT_CHAIN_CHANCE: f64 = 0.25;

pub const NEW_EVENT_IDS: [&str; 10] = [
    "aegis_whisper",
    "echoing_mime",
    "crow_snipe",
    "smoke_detour",
    "strangers_lamp",
    "drill_sergeant",
    "pit_lords_toll",
    "damned_bottle",
    "roshpit_gambit",
    "wilderness_stalker",
];

pub const ROGUELIKE_EVENT_IDS: [&str; 6] = [
    "collapsed_armory",
    "dead_prospectors_pack",
    "spore_debt",
    "clockwork_toll",
    "collectors_roots",
    "hungry_beacon",
];

pub const RUMOR_PASS_IDS: [&str; 8] = [
    "wisps_tether",
    "tunnel_echoes",
    "stalled_caravan",
    "volatile_affix",
    "rivals_cache",
    "forsaken_pact",
    "mapworks_drift",
    "the_eye_opens",
];

pub const HOLLOW_CHAIN_IDS: [&str; 3] = [
    "hollow_court_overture",
    "hollow_court_audience",
    "hollow_court_recess",
];

pub const EVENT_RATES: [(&str, f64); 8] = [
    ("Dirt", 0.22),
    ("Stone", 0.22),
    ("Crystal", 0.27),
    ("Magma", 0.27),
    ("Abyss", 0.31),
    ("Fungal Depths", 0.38),
    ("Frozen Core", 0.31),
    ("The Hollow", 0.45),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashStrategy {
    RandomActive,
    RichestN,
    ActiveDiggers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashTrigger {
    Success,
    Failure,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashMode {
    Burn,
    Grant,
    Steal,
}

impl SplashMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Burn => "burn",
            Self::Grant => "grant",
            Self::Steal => "steal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplashConfig {
    pub strategy: SplashStrategy,
    pub victim_count: usize,
    pub penalty_jc: i64,
    pub trigger: SplashTrigger,
    pub mode: SplashMode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurseEffect {
    CaveInBonus(f64),
    CooldownPenalty(f64),
    AdvanceBonus(i64),
    JcBonus(i64),
    LuminosityDrain(i64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempCurse {
    pub id: &'static str,
    pub name: &'static str,
    pub duration_digs: i64,
    pub effect: CurseEffect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TempBuff {
    pub id: &'static str,
    pub name: &'static str,
    pub duration_digs: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventOutcome {
    pub advance: i64,
    pub jc: i64,
    pub cave_in: bool,
    pub curse: Option<TempCurse>,
    pub gear_reward_pool: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventChoice {
    pub success: EventOutcome,
    pub failure: Option<EventOutcome>,
    pub success_chance: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub descriptions: &'static [&'static str],
    pub min_depth: Option<i64>,
    pub max_depth: Option<i64>,
    pub layer: Option<&'static str>,
    pub rarity: Rarity,
    pub safe_option: EventChoice,
    pub risky_option: EventChoice,
    pub desperate_option: Option<EventChoice>,
    pub buff_on_success: Option<TempBuff>,
    pub requires_dark: bool,
    pub social: bool,
    pub splash: Option<SplashConfig>,
    pub min_prestige: i64,
    pub next_event_id: Option<&'static str>,
    pub chain_only: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SerializedEvent {
    pub id: &'static str,
    pub name: &'static str,
    pub descriptions: Vec<&'static str>,
    pub min_depth: Option<i64>,
    pub max_depth: Option<i64>,
    pub layer: Option<&'static str>,
    pub rarity: Rarity,
    pub splash: Option<SplashConfig>,
    pub min_prestige: i64,
    pub next_event_id: Option<&'static str>,
    pub chain_only: bool,
}

const EMPTY_REWARD_POOL: &[&str] = &[];

const fn outcome(advance: i64, jc: i64, cave_in: bool) -> EventOutcome {
    EventOutcome {
        advance,
        jc,
        cave_in,
        curse: None,
        gear_reward_pool: EMPTY_REWARD_POOL,
    }
}

const fn choice(
    success: EventOutcome,
    failure: Option<EventOutcome>,
    success_chance: f64,
) -> EventChoice {
    EventChoice {
        success,
        failure,
        success_chance,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BaseEventSpec {
    id: &'static str,
    name: &'static str,
    descriptions: &'static [&'static str],
    min_depth: Option<i64>,
    max_depth: Option<i64>,
    rarity: Rarity,
    safe_jc: i64,
    risky_chance: f64,
    success_jc: i64,
    failure_advance: i64,
    failure_jc: i64,
}

fn base_event(spec: BaseEventSpec) -> EventDefinition {
    EventDefinition {
        id: spec.id,
        name: spec.name,
        descriptions: spec.descriptions,
        min_depth: spec.min_depth,
        max_depth: spec.max_depth,
        layer: None,
        rarity: spec.rarity,
        safe_option: choice(outcome(0, spec.safe_jc, false), None, 1.0),
        risky_option: choice(
            outcome(0, spec.success_jc, false),
            Some(outcome(spec.failure_advance, spec.failure_jc, false)),
            spec.risky_chance,
        ),
        desperate_option: None,
        buff_on_success: None,
        requires_dark: false,
        social: false,
        splash: None,
        min_prestige: 0,
        next_event_id: None,
        chain_only: false,
    }
}

#[must_use]
pub fn event_catalog() -> Vec<EventDefinition> {
    let mut events = new_cross_player_events();
    events.extend(roguelike_events());
    events.extend(rumor_pass_events());
    events.extend(hollow_chain_events());
    events
}

#[must_use]
pub fn serialized_event_pool() -> Vec<SerializedEvent> {
    event_catalog()
        .into_iter()
        .map(|event| SerializedEvent {
            id: event.id,
            name: event.name,
            descriptions: event.descriptions.to_vec(),
            min_depth: event.min_depth,
            max_depth: event.max_depth,
            layer: event.layer,
            rarity: event.rarity,
            splash: event.splash,
            min_prestige: event.min_prestige,
            next_event_id: event.next_event_id,
            chain_only: event.chain_only,
        })
        .collect()
}

#[must_use]
pub fn event_by_id(event_id: &str) -> Option<EventDefinition> {
    event_catalog()
        .into_iter()
        .find(|event| event.id == event_id)
}

fn new_cross_player_events() -> Vec<EventDefinition> {
    let specs = [
        (
            "aegis_whisper",
            "Aegis Whisper",
            &[
                "A half-buried shield rim glints in the dirt.",
                "Something circular is pressed into the wall.",
            ][..],
            None,
            Rarity::Uncommon,
        ),
        (
            "echoing_mime",
            "Echoing Mime",
            &[
                "A pale figure performs a silent show in the gloom.",
                "The mime tilts its head at you.",
            ][..],
            None,
            Rarity::Uncommon,
        ),
        (
            "crow_snipe",
            "Crow Snipe",
            &[
                "A delivery crow flaps past with a heavy pouch.",
                "A messenger bird passes overhead, laden and slow.",
            ][..],
            Some(10),
            Rarity::Uncommon,
        ),
        (
            "smoke_detour",
            "Smoke Detour",
            &[
                "A purple haze hides a shortcut through another tunnel.",
                "Smoke pools in a side passage.",
            ][..],
            Some(26),
            Rarity::Rare,
        ),
        (
            "strangers_lamp",
            "Stranger's Lamp",
            &[
                "An ornate brass lamp is wedged into the wall.",
                "The lamp is still warm.",
            ][..],
            Some(26),
            Rarity::Rare,
        ),
        (
            "drill_sergeant",
            "Drill Sergeant",
            &[
                "A spectral sergeant blocks the passage.",
                "The ghost raises a whistle and starts counting.",
            ][..],
            Some(26),
            Rarity::Rare,
        ),
        (
            "pit_lords_toll",
            "Pit Lord's Toll",
            &[
                "A horned figure rattles a cup of bone dice.",
                "A horned silhouette gestures at a pile of skulls.",
            ][..],
            Some(51),
            Rarity::Legendary,
        ),
        (
            "damned_bottle",
            "Damned Bottle",
            &[
                "An ornate bottle whispers two names in the dark.",
                "The bottle sweats a faint blue light.",
            ][..],
            Some(76),
            Rarity::Legendary,
        ),
        (
            "roshpit_gambit",
            "Roshpit Gambit",
            &[
                "An ancient pit yawns open.",
                "A pit older than the mine hums below.",
            ][..],
            Some(76),
            Rarity::Legendary,
        ),
        (
            "wilderness_stalker",
            "Wilderness Stalker",
            &[
                "A masked figure has been tracking you.",
                "Footprints follow yours through the dark.",
            ][..],
            Some(101),
            Rarity::Legendary,
        ),
    ];
    specs
        .into_iter()
        .map(|(id, name, descriptions, min_depth, rarity)| {
            let (safe_jc, risky_jc, failure_jc, chance) = match rarity {
                Rarity::Uncommon => (3, 14, -8, 0.62),
                Rarity::Rare => (3, 19, -10, 0.56),
                Rarity::Legendary if min_depth.is_some_and(|depth| depth >= 76) => {
                    (6, 59, -32, 0.50)
                }
                Rarity::Legendary => (6, 59, -32, 0.50),
                Rarity::Common => (3, 10, -5, 0.62),
            };
            let mut event = base_event(BaseEventSpec {
                id,
                name,
                descriptions,
                min_depth,
                max_depth: None,
                rarity,
                safe_jc,
                risky_chance: chance,
                success_jc: risky_jc,
                failure_advance: 0,
                failure_jc,
            });
            match id {
                "aegis_whisper" => {
                    event.buff_on_success = Some(TempBuff {
                        id: "aegis_aura",
                        name: "Aegis Aura",
                        duration_digs: 3,
                    });
                }
                "echoing_mime" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::ActiveDiggers,
                        victim_count: 2,
                        penalty_jc: 4,
                        trigger: SplashTrigger::Always,
                        mode: SplashMode::Burn,
                    });
                }
                "crow_snipe" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::RichestN,
                        victim_count: 1,
                        penalty_jc: 10,
                        trigger: SplashTrigger::Success,
                        mode: SplashMode::Steal,
                    });
                }
                "smoke_detour" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::ActiveDiggers,
                        victim_count: 2,
                        penalty_jc: 6,
                        trigger: SplashTrigger::Failure,
                        mode: SplashMode::Burn,
                    });
                }
                "strangers_lamp" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::RandomActive,
                        victim_count: 1,
                        penalty_jc: 5,
                        trigger: SplashTrigger::Success,
                        mode: SplashMode::Grant,
                    });
                }
                "drill_sergeant" => {
                    event.buff_on_success = Some(TempBuff {
                        id: "drilled",
                        name: "Drilled",
                        duration_digs: 4,
                    });
                }
                "pit_lords_toll" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::ActiveDiggers,
                        victim_count: 3,
                        penalty_jc: 8,
                        trigger: SplashTrigger::Failure,
                        mode: SplashMode::Burn,
                    });
                }
                "damned_bottle" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::RandomActive,
                        victim_count: 1,
                        penalty_jc: 15,
                        trigger: SplashTrigger::Success,
                        mode: SplashMode::Steal,
                    });
                }
                "roshpit_gambit" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::RichestN,
                        victim_count: 2,
                        penalty_jc: 12,
                        trigger: SplashTrigger::Success,
                        mode: SplashMode::Burn,
                    });
                }
                "wilderness_stalker" => {
                    event.safe_option.success.jc = 10;
                    event.risky_option.success.jc = 98;
                    if let Some(failure) = &mut event.risky_option.failure {
                        failure.jc = -52;
                    }
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::ActiveDiggers,
                        victim_count: 1,
                        penalty_jc: 20,
                        trigger: SplashTrigger::Success,
                        mode: SplashMode::Steal,
                    });
                }
                _ => {}
            }
            event
        })
        .collect()
}

fn roguelike_events() -> Vec<EventDefinition> {
    let mut armory = base_event(BaseEventSpec {
        id: "collapsed_armory",
        name: "Collapsed Armory",
        descriptions: &[
            "A buried armory rattles under a cracked slab.",
            "Rusty blades poke through a collapsed weapons rack.",
        ],
        min_depth: Some(76),
        max_depth: Some(150),
        rarity: Rarity::Uncommon,
        safe_jc: -6,
        risky_chance: 0.35,
        success_jc: 4,
        failure_advance: 0,
        failure_jc: -18,
    });
    armory.risky_option.success.gear_reward_pool = &["glassbreaker_pick", "needle_pick"];

    let mut prospectors = base_event(BaseEventSpec {
        id: "dead_prospectors_pack",
        name: "Dead Prospector's Pack",
        descriptions: &[
            "A dead prospector still hugs a travel pack.",
            "You find a miner who finally stopped digging.",
        ],
        min_depth: Some(76),
        max_depth: Some(200),
        rarity: Rarity::Uncommon,
        safe_jc: -5,
        risky_chance: 0.40,
        success_jc: 3,
        failure_advance: -2,
        failure_jc: -16,
    });
    prospectors.risky_option.success.gear_reward_pool = &["briarplate", "nullweave_mantle"];

    let mut spore = base_event(BaseEventSpec {
        id: "spore_debt",
        name: "Spore Debt",
        descriptions: &[
            "Mushrooms bloom into a ledger with your name.",
            "A fungal clerk unfolds from the wall.",
        ],
        min_depth: None,
        max_depth: None,
        rarity: Rarity::Uncommon,
        safe_jc: -4,
        risky_chance: 0.55,
        success_jc: 8,
        failure_advance: 0,
        failure_jc: -12,
    });
    spore.layer = Some("Fungal Depths");
    spore.risky_option.success.gear_reward_pool = &["springheel_boots", "anchor_boots"];
    spore.risky_option.failure.as_mut().unwrap().curse = Some(TempCurse {
        id: "spore_debt_curse",
        name: "Compound Spores",
        duration_digs: 3,
        effect: CurseEffect::CaveInBonus(0.08),
    });

    let mut clockwork = base_event(BaseEventSpec {
        id: "clockwork_toll",
        name: "Clockwork Toll",
        descriptions: &[
            "A frozen turnstile counts down.",
            "Brass hands reach from the ice.",
        ],
        min_depth: None,
        max_depth: None,
        rarity: Rarity::Rare,
        safe_jc: -5,
        risky_chance: 0.50,
        success_jc: 6,
        failure_advance: 0,
        failure_jc: -18,
    });
    clockwork.layer = Some("Frozen Core");
    clockwork.risky_option.success.gear_reward_pool = &["loaded_die", "blood_locket"];
    clockwork.risky_option.failure.as_mut().unwrap().curse = Some(TempCurse {
        id: "clockwork_toll_curse",
        name: "Borrowed Time",
        duration_digs: 3,
        effect: CurseEffect::CooldownPenalty(0.20),
    });

    let mut collectors = base_event(BaseEventSpec {
        id: "collectors_roots",
        name: "Collector's Roots",
        descriptions: &[
            "Roots knot around a coin press labeled COLLECTIONS.",
            "A root-fed ledger lists every active digger.",
        ],
        min_depth: Some(51),
        max_depth: None,
        rarity: Rarity::Uncommon,
        safe_jc: -4,
        risky_chance: 0.60,
        success_jc: 6,
        failure_advance: 0,
        failure_jc: -12,
    });
    collectors.social = true;
    collectors.splash = Some(SplashConfig {
        strategy: SplashStrategy::ActiveDiggers,
        victim_count: 2,
        penalty_jc: 8,
        trigger: SplashTrigger::Success,
        mode: SplashMode::Burn,
    });

    let mut beacon = base_event(BaseEventSpec {
        id: "hungry_beacon",
        name: "Hungry Beacon",
        descriptions: &[
            "A black beacon flashes whenever a digger finds coin.",
            "A signal lamp made of teeth sweeps distant tunnels.",
        ],
        min_depth: Some(101),
        max_depth: None,
        rarity: Rarity::Rare,
        safe_jc: -6,
        risky_chance: 0.50,
        success_jc: 10,
        failure_advance: 0,
        failure_jc: -20,
    });
    beacon.social = true;
    beacon.splash = Some(SplashConfig {
        strategy: SplashStrategy::ActiveDiggers,
        victim_count: 3,
        penalty_jc: 6,
        trigger: SplashTrigger::Success,
        mode: SplashMode::Burn,
    });

    vec![armory, prospectors, spore, clockwork, collectors, beacon]
}

fn rumor_pass_events() -> Vec<EventDefinition> {
    let specs = [
        (
            "wisps_tether",
            "Wisp's Tether",
            &[
                "A drifting mote of light recognizes you.",
                "The wisp leans toward your warmth.",
            ][..],
            Some(51),
            Some(75),
            Rarity::Common,
        ),
        (
            "tunnel_echoes",
            "Tunnel Echoes",
            &["Your pickaxe rings wrong.", "The walls carry your mistake."][..],
            None,
            None,
            Rarity::Common,
        ),
        (
            "stalled_caravan",
            "Stalled Caravan",
            &[
                "A wagon wheel lies half-sunk in dirt.",
                "Tracks lead nowhere.",
            ][..],
            None,
            Some(25),
            Rarity::Common,
        ),
        (
            "volatile_affix",
            "Volatile Affix",
            &[
                "A jagged crystal pulses out of rhythm.",
                "The shard hums against your palm.",
            ][..],
            Some(51),
            Some(75),
            Rarity::Uncommon,
        ),
        (
            "rivals_cache",
            "Rival's Cache",
            &[
                "A camouflaged stash waits behind false stone.",
                "Fresh tool marks cross the wall.",
            ][..],
            None,
            Some(25),
            Rarity::Uncommon,
        ),
        (
            "forsaken_pact",
            "Forsaken Pact",
            &[
                "A voice from the cracks offers a deal.",
                "The dark proposes careful terms.",
            ][..],
            Some(101),
            Some(150),
            Rarity::Uncommon,
        ),
        (
            "mapworks_drift",
            "Mapworks Drift",
            &[
                "A folded map unfurls itself.",
                "Lines redraw themselves on parchment.",
            ][..],
            Some(51),
            Some(75),
            Rarity::Rare,
        ),
        (
            "the_eye_opens",
            "The Eye Opens",
            &[
                "A seam in the rock parts and an eye looks back.",
                "The dark watches you watching it.",
            ][..],
            Some(101),
            Some(150),
            Rarity::Legendary,
        ),
    ];

    specs
        .into_iter()
        .map(|(id, name, descriptions, min_depth, max_depth, rarity)| {
            let mut event = base_event(BaseEventSpec {
                id,
                name,
                descriptions,
                min_depth,
                max_depth,
                rarity,
                safe_jc: if min_depth.is_some_and(|depth| depth >= 101) {
                    10
                } else {
                    3
                },
                risky_chance: if rarity == Rarity::Legendary {
                    0.50
                } else {
                    0.62
                },
                success_jc: if rarity == Rarity::Legendary { 98 } else { 14 },
                failure_advance: 0,
                failure_jc: if rarity == Rarity::Legendary { -52 } else { -8 },
            });
            match id {
                "wisps_tether" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::RandomActive,
                        victim_count: 1,
                        penalty_jc: 4,
                        trigger: SplashTrigger::Success,
                        mode: SplashMode::Grant,
                    });
                }
                "tunnel_echoes" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::ActiveDiggers,
                        victim_count: 1,
                        penalty_jc: 3,
                        trigger: SplashTrigger::Failure,
                        mode: SplashMode::Burn,
                    });
                }
                "volatile_affix" => {
                    event.buff_on_success = Some(TempBuff {
                        id: "affix_hum",
                        name: "Affix Hum",
                        duration_digs: 3,
                    });
                }
                "rivals_cache" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::RichestN,
                        victim_count: 1,
                        penalty_jc: 8,
                        trigger: SplashTrigger::Success,
                        mode: SplashMode::Steal,
                    });
                }
                "forsaken_pact" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::ActiveDiggers,
                        victim_count: 2,
                        penalty_jc: 5,
                        trigger: SplashTrigger::Failure,
                        mode: SplashMode::Burn,
                    });
                }
                "mapworks_drift" => {
                    event.social = true;
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::RandomActive,
                        victim_count: 2,
                        penalty_jc: 8,
                        trigger: SplashTrigger::Success,
                        mode: SplashMode::Grant,
                    });
                }
                "the_eye_opens" => {
                    event.social = true;
                    event.requires_dark = true;
                    event.buff_on_success = Some(TempBuff {
                        id: "whispered_insight",
                        name: "Whispered Insight",
                        duration_digs: 3,
                    });
                    event.desperate_option = Some(choice(
                        outcome(1, 157, false),
                        Some(outcome(-5, 0, false)),
                        0.40,
                    ));
                    event.splash = Some(SplashConfig {
                        strategy: SplashStrategy::RichestN,
                        victim_count: 1,
                        penalty_jc: 28,
                        trigger: SplashTrigger::Success,
                        mode: SplashMode::Steal,
                    });
                }
                _ => {}
            }
            event
        })
        .collect()
}

fn hollow_chain_events() -> Vec<EventDefinition> {
    let specs = [
        (
            "hollow_court_overture",
            "Overture of the Hollow Court",
            &[
                "Something old turns to face you in the dark.",
                "A doorway appears where none should be.",
            ][..],
            Some("hollow_court_audience"),
            false,
        ),
        (
            "hollow_court_audience",
            "Audience with the Hollow Court",
            &[
                "Figures take their seats around you.",
                "Three thrones wait in silence.",
            ][..],
            Some("hollow_court_recess"),
            true,
        ),
        (
            "hollow_court_recess",
            "The Court Recesses",
            &[
                "The figures stand without sound.",
                "A recess is called in the dark.",
            ][..],
            None,
            true,
        ),
    ];
    specs
        .into_iter()
        .map(|(id, name, descriptions, next_event_id, chain_only)| {
            let mut event = base_event(BaseEventSpec {
                id,
                name,
                descriptions,
                min_depth: Some(200),
                max_depth: None,
                rarity: Rarity::Legendary,
                safe_jc: 10,
                risky_chance: 0.50,
                success_jc: 98,
                failure_advance: 0,
                failure_jc: 0,
            });
            event.min_prestige = 6;
            event.next_event_id = next_event_id;
            event.chain_only = chain_only;
            event
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventContext {
    pub depth: i64,
    pub luminosity: i64,
    pub prestige_level: i64,
}

#[must_use]
pub fn eligible_random_events(context: EventContext) -> Vec<EventDefinition> {
    let layer = layer_at(context.depth);
    event_catalog()
        .into_iter()
        .filter(|event| {
            !event.chain_only
                && event
                    .min_depth
                    .is_none_or(|minimum| context.depth >= minimum)
                && event
                    .max_depth
                    .is_none_or(|maximum| context.depth <= maximum)
                && event.layer.is_none_or(|required| required == layer.name)
                && (!event.requires_dark || context.luminosity <= 0)
                && context.prestige_level >= event.min_prestige
        })
        .collect()
}

pub trait EventEntropy {
    fn unit_interval(&mut self) -> f64;

    fn weighted_index(&mut self, weights: &[u64]) -> usize {
        assert!(
            !weights.is_empty(),
            "weighted selection requires candidates"
        );
        let total = weights.iter().copied().sum::<u64>();
        assert!(total > 0, "weighted selection requires positive weight");
        let mut cursor =
            (self.unit_interval().clamp(0.0, 1.0 - f64::EPSILON) * total as f64) as u64;
        for (index, weight) in weights.iter().copied().enumerate() {
            if cursor < weight {
                return index;
            }
            cursor -= weight;
        }
        weights.len() - 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RolledEvent {
    pub event: EventDefinition,
    pub chained: bool,
}

#[must_use]
pub fn roll_event(context: EventContext, entropy: &mut impl EventEntropy) -> Option<RolledEvent> {
    let events = eligible_random_events(context);
    if events.is_empty() {
        return None;
    }
    let weights = events
        .iter()
        .map(|event| event.rarity.weight())
        .collect::<Vec<_>>();
    let index = entropy.weighted_index(&weights);
    Some(RolledEvent {
        event: events[index],
        chained: false,
    })
}

#[must_use]
pub fn chain_event(
    depth: i64,
    prestige_level: i64,
    trigger_rarity: Rarity,
    trigger_event_id: Option<&str>,
    entropy: &mut impl EventEntropy,
) -> Option<RolledEvent> {
    if let Some(trigger) = trigger_event_id.and_then(event_by_id)
        && let Some(next) = trigger.next_event_id.and_then(event_by_id)
        && prestige_level >= next.min_prestige
    {
        return Some(RolledEvent {
            event: next,
            chained: true,
        });
    }

    if prestige_level < 7 || entropy.unit_interval() >= EVENT_CHAIN_CHANCE {
        return None;
    }
    let minimum_rank = rarity_rank(trigger_rarity);
    let events = eligible_random_events(EventContext {
        depth,
        luminosity: 100,
        prestige_level,
    })
    .into_iter()
    .filter(|event| rarity_rank(event.rarity) >= minimum_rank)
    .collect::<Vec<_>>();
    if events.is_empty() {
        return None;
    }
    let weights = events
        .iter()
        .map(|event| event.rarity.weight())
        .collect::<Vec<_>>();
    let index = entropy.weighted_index(&weights);
    Some(RolledEvent {
        event: events[index],
        chained: true,
    })
}

const fn rarity_rank(rarity: Rarity) -> u8 {
    match rarity {
        Rarity::Common => 0,
        Rarity::Uncommon => 1,
        Rarity::Rare => 2,
        Rarity::Legendary => 3,
    }
}

#[must_use]
pub fn event_rate(layer_name: &str) -> f64 {
    EVENT_RATES
        .iter()
        .find_map(|(layer, rate)| (*layer == layer_name).then_some(*rate))
        .unwrap_or(0.22)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplashCandidate {
    pub discord_id: u64,
    pub balance: i64,
}

/// A tunnel row used by the pure deepest-target selector.
///
/// The Python ``_select_deepest_n`` helper receives rows already ordered by
/// ``depth DESC`` from ``DigRepository.get_leaderboard``.  Keeping the depth
/// in a small policy value lets the Rust seam reproduce that selection
/// without coupling the event policy to a database implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplashDepthCandidate {
    pub discord_id: u64,
    pub depth: i64,
}

/// Select the wealthiest positive-balance player, optionally excluding the
/// caster.  This mirrors ``pick_splash_target(..., mode="wealthiest")``.
///
/// ``candidates`` represents the positive-balance rows returned by the player
/// repository.  The helper sorts locally so callers do not have to rely on a
/// particular SQL tie order.
#[must_use]
pub fn select_wealthiest_target(
    candidates: &[SplashCandidate],
    exclude_id: Option<u64>,
) -> Option<u64> {
    candidates
        .iter()
        .filter(|candidate| candidate.balance > 0 && Some(candidate.discord_id) != exclude_id)
        .max_by(|left, right| {
            left.balance
                .cmp(&right.balance)
                .then_with(|| right.discord_id.cmp(&left.discord_id))
        })
        .map(|candidate| candidate.discord_id)
}

/// Select one recent digger with probability proportional to positive
/// balance.  If no candidate has a positive balance, selection falls back to
/// a uniform choice, matching the Python splash helper.
///
/// The candidate list is expected to be the recent-digger query result.  The
/// optional exclusion is applied here as a second defensive boundary so the
/// caster can never be selected by this pure policy seam.
pub fn select_random_recent_weighted(
    candidates: &[SplashCandidate],
    exclude_id: Option<u64>,
    entropy: &mut impl SplashEntropy,
) -> Option<u64> {
    let pool = candidates
        .iter()
        .filter(|candidate| Some(candidate.discord_id) != exclude_id)
        .collect::<Vec<_>>();
    if pool.is_empty() {
        return None;
    }

    let weighted = pool
        .iter()
        .filter_map(|candidate| {
            (candidate.balance > 0).then_some((candidate.discord_id, candidate.balance as u64))
        })
        .collect::<Vec<_>>();
    if weighted.is_empty() {
        return Some(pool[entropy.index(pool.len()) % pool.len()].discord_id);
    }

    let weights = weighted
        .iter()
        .map(|(_, weight)| *weight)
        .collect::<Vec<_>>();
    let index = weighted_index(&weights, entropy);
    Some(weighted[index].0)
}

/// Select the deepest positive-depth players in descending depth order,
/// excluding the caster.  This mirrors ``_select_deepest_n`` and returns at
/// most ``count`` IDs.
#[must_use]
pub fn select_deepest_n(
    candidates: &[SplashDepthCandidate],
    exclude_id: u64,
    count: usize,
) -> Vec<u64> {
    let mut pool = candidates
        .iter()
        .filter(|candidate| candidate.discord_id != exclude_id && candidate.depth > 0)
        .copied()
        .collect::<Vec<_>>();
    pool.sort_by(|left, right| {
        right
            .depth
            .cmp(&left.depth)
            .then_with(|| left.discord_id.cmp(&right.discord_id))
    });
    pool.into_iter()
        .take(count)
        .map(|candidate| candidate.discord_id)
        .collect()
}

fn weighted_index(weights: &[u64], entropy: &mut impl SplashEntropy) -> usize {
    debug_assert!(!weights.is_empty());
    let total = weights.iter().copied().sum::<u64>();
    if total == 0 {
        return entropy.index(weights.len()) % weights.len();
    }
    // ``index`` takes a usize while balances are persisted as i64.  The
    // normal application range is far below usize::MAX; saturating here keeps
    // malformed/extreme fixture values deterministic and fail-soft.
    let cursor = entropy.index(total.try_into().unwrap_or(usize::MAX)) as u64;
    let cursor = cursor.min(total.saturating_sub(1));
    let mut offset = cursor;
    for (index, weight) in weights.iter().copied().enumerate() {
        if offset < weight {
            return index;
        }
        offset -= weight;
    }
    weights.len() - 1
}

pub trait SplashCandidatePort {
    type Error;

    fn candidates(
        &self,
        strategy: SplashStrategy,
        guild_id: u64,
        digger_id: u64,
        requested: usize,
    ) -> Result<Vec<SplashCandidate>, Self::Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplashVictimRequest {
    pub guild_id: u64,
    pub digger_id: u64,
    pub victim_id: u64,
    pub event_name: String,
    pub event_key: String,
    pub mode: SplashMode,
    pub amount: i64,
    pub minimum_victim_balance: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashVictimSettlement {
    Applied { amount: i64 },
    Skipped,
}

/// Atomic wallet + audit seam for one selected victim.
///
/// An implementation must change every wallet implicated by `mode` and write
/// the matching victim/thief audit rows in one transaction. Repeating an
/// `event_key` must return the original settlement without another mutation.
pub trait SplashEconomyPort {
    type Error;

    fn settle_victim_atomic(
        &self,
        request: SplashVictimRequest,
    ) -> Result<SplashVictimSettlement, Self::Error>;
}

pub trait SplashEntropy {
    fn index(&mut self, upper_exclusive: usize) -> usize;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplashResult {
    pub strategy: SplashStrategy,
    pub event_name: String,
    pub victims: Vec<(u64, i64)>,
    pub total_burned: i64,
    pub mode: SplashMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplashPayload {
    pub strategy: SplashStrategy,
    pub event_name: String,
    pub victims: Vec<(u64, i64)>,
    pub total_burned: i64,
    pub mode: &'static str,
}

#[must_use]
pub fn splash_to_payload(result: &SplashResult) -> SplashPayload {
    SplashPayload {
        strategy: result.strategy,
        event_name: result.event_name.clone(),
        victims: result.victims.clone(),
        total_burned: result.total_burned,
        mode: result.mode.as_str(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveSplashError<CandidateError, EconomyError> {
    Candidate(CandidateError),
    Economy(EconomyError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveSplashRequest {
    pub guild_id: u64,
    pub digger_id: u64,
    pub event_name: String,
    pub event_key: String,
    pub config: SplashConfig,
    pub minigame_jc_delta_scale_micros: i64,
}

impl ResolveSplashRequest {
    #[must_use]
    pub fn with_default_scale(
        guild_id: u64,
        digger_id: u64,
        event_name: impl Into<String>,
        event_key: impl Into<String>,
        config: SplashConfig,
    ) -> Self {
        Self {
            guild_id,
            digger_id,
            event_name: event_name.into(),
            event_key: event_key.into(),
            config,
            minigame_jc_delta_scale_micros: (DEFAULT_MINIGAME_JC_DELTA_SCALE * 1_000_000.0) as i64,
        }
    }
}

pub fn resolve_splash<C, E>(
    candidates: &C,
    economy: &E,
    request: ResolveSplashRequest,
    entropy: &mut impl SplashEntropy,
) -> Result<SplashResult, ResolveSplashError<C::Error, E::Error>>
where
    C: SplashCandidatePort,
    E: SplashEconomyPort,
{
    let config = request.config;
    let mut pool = candidates
        .candidates(
            config.strategy,
            request.guild_id,
            request.digger_id,
            config.victim_count,
        )
        .map_err(ResolveSplashError::Candidate)?;
    pool.retain(|candidate| candidate.discord_id != request.digger_id);
    let mut seen = BTreeSet::new();
    pool.retain(|candidate| seen.insert(candidate.discord_id));
    let selected = sample_without_replacement(pool, config.victim_count, entropy);
    let scale = request.minigame_jc_delta_scale_micros as f64 / 1_000_000.0;
    let amount = match config.mode {
        SplashMode::Burn => scale_deflationary_minigame_jc_delta(config.penalty_jc as f64, scale),
        SplashMode::Grant | SplashMode::Steal => {
            scale_minigame_jc_delta(config.penalty_jc as f64, scale)
        }
    };
    let mut victims = Vec::new();
    for candidate in selected {
        let settlement = economy
            .settle_victim_atomic(SplashVictimRequest {
                guild_id: request.guild_id,
                digger_id: request.digger_id,
                victim_id: candidate.discord_id,
                event_name: request.event_name.clone(),
                event_key: format!("{}:{}", request.event_key, candidate.discord_id),
                mode: config.mode,
                amount,
                minimum_victim_balance: HOSTILE_LOSS_MIN_BALANCE,
            })
            .map_err(ResolveSplashError::Economy)?;
        if let SplashVictimSettlement::Applied { amount } = settlement {
            victims.push((candidate.discord_id, amount));
        }
    }
    let total_burned = victims.iter().map(|(_, amount)| amount).sum();
    Ok(SplashResult {
        strategy: config.strategy,
        event_name: request.event_name,
        victims,
        total_burned,
        mode: config.mode,
    })
}

fn sample_without_replacement(
    mut pool: Vec<SplashCandidate>,
    count: usize,
    entropy: &mut impl SplashEntropy,
) -> Vec<SplashCandidate> {
    let mut selected = Vec::with_capacity(count.min(pool.len()));
    while !pool.is_empty() && selected.len() < count {
        let index = entropy.index(pool.len()) % pool.len();
        selected.push(pool.swap_remove(index));
    }
    selected
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplashAuditAction {
    pub guild_id: u64,
    pub actor_id: u64,
    pub target_id: Option<u64>,
    pub action_type: &'static str,
    pub jc_delta: i64,
    pub event_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MemoryPlayer {
    balance: i64,
    registered: bool,
    active_digger: bool,
}

#[derive(Clone, Debug, Default)]
struct MemorySplashState {
    players: BTreeMap<(u64, u64), MemoryPlayer>,
    actions: Vec<SplashAuditAction>,
    claims: BTreeMap<(u64, String), SplashVictimSettlement>,
    fail_next: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InMemorySplashError {
    InjectedFailure,
    InvalidAmount,
    ArithmeticOverflow,
}

/// Mutex-serialized executable reference implementation of both splash ports.
/// It clones state before a settlement and swaps only after every wallet and
/// audit mutation succeeds, giving tests the same rollback boundary expected
/// from the SQLite transaction.
#[derive(Debug, Default)]
pub struct InMemorySplashState {
    state: Mutex<MemorySplashState>,
}

impl InMemorySplashState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_player(
        &self,
        guild_id: u64,
        discord_id: u64,
        balance: i64,
        active_digger: bool,
    ) {
        self.state
            .lock()
            .expect("splash-state mutex poisoned")
            .players
            .insert(
                (guild_id, discord_id),
                MemoryPlayer {
                    balance,
                    registered: true,
                    active_digger,
                },
            );
    }

    pub fn fail_next_settlement(&self) {
        self.state
            .lock()
            .expect("splash-state mutex poisoned")
            .fail_next = true;
    }

    #[must_use]
    pub fn balance(&self, guild_id: u64, discord_id: u64) -> Option<i64> {
        self.state
            .lock()
            .expect("splash-state mutex poisoned")
            .players
            .get(&(guild_id, discord_id))
            .map(|player| player.balance)
    }

    #[must_use]
    pub fn actions(&self) -> Vec<SplashAuditAction> {
        self.state
            .lock()
            .expect("splash-state mutex poisoned")
            .actions
            .clone()
    }
}

impl SplashCandidatePort for InMemorySplashState {
    type Error = InMemorySplashError;

    fn candidates(
        &self,
        strategy: SplashStrategy,
        guild_id: u64,
        digger_id: u64,
        requested: usize,
    ) -> Result<Vec<SplashCandidate>, Self::Error> {
        let state = self.state.lock().expect("splash-state mutex poisoned");
        let mut candidates = state
            .players
            .iter()
            .filter(|((guild, discord_id), player)| {
                *guild == guild_id
                    && *discord_id != digger_id
                    && player.registered
                    && (strategy != SplashStrategy::ActiveDiggers || player.active_digger)
            })
            .map(|((_, discord_id), player)| SplashCandidate {
                discord_id: *discord_id,
                balance: player.balance,
            })
            .collect::<Vec<_>>();
        if strategy == SplashStrategy::RichestN {
            candidates.sort_by(|left, right| {
                right
                    .balance
                    .cmp(&left.balance)
                    .then_with(|| left.discord_id.cmp(&right.discord_id))
            });
            candidates.truncate(requested);
        }
        Ok(candidates)
    }
}

impl SplashEconomyPort for InMemorySplashState {
    type Error = InMemorySplashError;

    fn settle_victim_atomic(
        &self,
        request: SplashVictimRequest,
    ) -> Result<SplashVictimSettlement, Self::Error> {
        if request.amount <= 0 {
            return Err(InMemorySplashError::InvalidAmount);
        }
        let mut state = self.state.lock().expect("splash-state mutex poisoned");
        if state.fail_next {
            state.fail_next = false;
            return Err(InMemorySplashError::InjectedFailure);
        }
        let claim_key = (request.guild_id, request.event_key.clone());
        if let Some(settlement) = state.claims.get(&claim_key) {
            return Ok(*settlement);
        }

        let Some(victim) = state
            .players
            .get(&(request.guild_id, request.victim_id))
            .copied()
        else {
            return Ok(SplashVictimSettlement::Skipped);
        };
        if matches!(request.mode, SplashMode::Burn | SplashMode::Steal)
            && victim.balance < request.minimum_victim_balance
        {
            state
                .claims
                .insert(claim_key, SplashVictimSettlement::Skipped);
            return Ok(SplashVictimSettlement::Skipped);
        }

        let mut working = state.clone();
        let actual = match request.mode {
            SplashMode::Burn => request.amount.min(victim.balance),
            SplashMode::Grant | SplashMode::Steal => request.amount,
        };
        let victim_delta = if request.mode == SplashMode::Grant {
            actual
        } else {
            -actual
        };
        let victim_state = working
            .players
            .get_mut(&(request.guild_id, request.victim_id))
            .expect("victim copied into transaction");
        victim_state.balance = victim_state
            .balance
            .checked_add(victim_delta)
            .ok_or(InMemorySplashError::ArithmeticOverflow)?;
        working.actions.push(SplashAuditAction {
            guild_id: request.guild_id,
            actor_id: request.victim_id,
            target_id: (request.mode == SplashMode::Steal).then_some(request.digger_id),
            action_type: "splash_victim",
            jc_delta: victim_delta,
            event_key: request.event_key.clone(),
        });

        if request.mode == SplashMode::Steal {
            let Some(digger) = working
                .players
                .get_mut(&(request.guild_id, request.digger_id))
            else {
                return Ok(SplashVictimSettlement::Skipped);
            };
            digger.balance = digger
                .balance
                .checked_add(actual)
                .ok_or(InMemorySplashError::ArithmeticOverflow)?;
            working.actions.push(SplashAuditAction {
                guild_id: request.guild_id,
                actor_id: request.digger_id,
                target_id: Some(request.victim_id),
                action_type: "splash_thief",
                jc_delta: actual,
                event_key: request.event_key.clone(),
            });
        }

        let settlement = SplashVictimSettlement::Applied { amount: actual };
        working.claims.insert(claim_key, settlement);
        *state = working;
        Ok(settlement)
    }
}

#[cfg(test)]
#[path = "dig_new_events/tests.rs"]
mod tests;
