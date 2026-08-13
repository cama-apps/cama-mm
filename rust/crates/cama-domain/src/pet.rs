//! Pure domain model and balance data for cama pets.
//!
//! Pet state is derived from persisted timestamp/hunger anchors.  Nothing in
//! this module ticks on a scheduler, which keeps hunger, death, mood, and work
//! accrual deterministic across process downtime.  Arithmetic uses SQLite's
//! signed-integer domain and wider intermediates so the persisted boundary
//! values do not lose precision.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{self, Display, Formatter};

const SECONDS_PER_DAY: i64 = 86_400;

pub const MAX_HUNGER: i64 = 100;
pub const DEFAULT_HUNGER_DECAY_PER_DAY: i64 = 20;
pub const EGG_HATCH_SECONDS: i64 = 24 * 3_600;
pub const ADULT_AGE_SECONDS: i64 = 7 * 24 * 3_600;
pub const UNHATCHED_SPECIES: &str = "unhatched";
pub const PET_EAT_REWARD_MIN: i64 = 500;
pub const PET_EAT_REWARD_MAX: i64 = 1_000;
pub const PET_EAT_PENALTY_GAMES_MIN: i64 = 3;
pub const PET_EAT_PENALTY_GAMES_MAX: i64 = 5;

pub const MOOD_HAPPY_MIN: i64 = 70;
pub const MOOD_CONTENT_MIN: i64 = 40;
pub const MOOD_HUNGRY_MIN: i64 = 15;
pub const WARNING_HUNGER: i64 = 30;

pub const PET_NAME_MAX_LEN: i64 = 32;
pub const ADOPTION_FEES: [i64; 4] = [20, 50, 75, 100];
pub const RENAME_COST: i64 = 10;
pub const SUPPLY_STACK_CAP: i64 = 99;
pub const MAX_BUY_QTY: i64 = 25;
pub const FEED_CAP_PER_DAY: i64 = 4;
pub const MATCH_WIN_HUNGER: i64 = 10;
pub const AEGIS_REVIVE_HUNGER: i64 = 10;
pub const SALT_LICK_DURATION_SECONDS: i64 = 12 * 3_600;
pub const REFUND_MULT_MIN: i64 = 50;
pub const REFUND_MULT_MAX: i64 = 200;

pub const DIG_WORK_UNITS_PER_BLOCK: i64 = SECONDS_PER_DAY;
pub const DIG_WORK_RATE_HAPPY: i64 = 12;
pub const DIG_WORK_RATE_CONTENT: i64 = 8;
pub const DIG_WORK_RATE_HUNGRY: i64 = 3;
pub const DIG_WORK_CAP_BLOCKS: i64 = 36;
pub const DIG_WORK_PER_DIG_CAP_BLOCKS: i64 = 12;

pub const SPECIES_WEIGHT_TOTAL: i64 = 10_000;
pub const GILDED_EGG_PREMIUM: i64 = 230;
pub const PITY_THRESHOLD: i64 = 3;
pub const TRINKET_COST: i64 = 25;
pub const TRINKET_DUPE_REFUND: i64 = 10;

pub const BRAWL_WIN_HUNGER: i64 = 10;
pub const BRAWL_WIN_HUNGER_DAILY_CAP: i64 = 3;
pub const BRAWL_LOSS_HUNGER: i64 = 15;
pub const BRAWL_LOSS_FLOOR: i64 = 10;
pub const BRAWL_INSURANCE_REDUCTION: i64 = 5;
pub const BRAWL_WIN_XP: i64 = 2;
pub const BRAWL_LOSS_XP: i64 = 1;
pub const BRAWL_XP_DAILY_CAP: i64 = 3;
pub const BRAWL_XP_PER_LEVEL: i64 = 5;
pub const BRAWL_TRAINING_XP_CAP: i64 = 20;
pub const BRAWL_TRAINING_STAT_CAP: i64 = 2;
pub const BRAWL_TRAINING_LEVEL_CAP: i64 = 4;
pub const SOLO_TRAINING_SESSION_CAP: i64 = 3;
pub const SOLO_TRAINING_XP_PER_SESSION: i64 = 1;

pub const PET_BRAWL_ACCEPT_SECONDS: i64 = 180;
pub const PET_BRAWL_TURN_SECONDS: i64 = 30;
pub const PET_BRAWL_ACTIVE_TTL_SECONDS: i64 = 1_800;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PetStage {
    Egg,
    Baby,
    Adult,
}

impl PetStage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Egg => "egg",
            Self::Baby => "baby",
            Self::Adult => "adult",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PetMood {
    Happy,
    Content,
    Hungry,
    Starving,
}

impl PetMood {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Happy => "happy",
            Self::Content => "content",
            Self::Hungry => "hungry",
            Self::Starving => "starving",
        }
    }

    /// Art has only three mood variants; starving shares hungry art.
    #[must_use]
    pub const fn art_key(self) -> &'static str {
        match self {
            Self::Happy => "happy",
            Self::Content => "neutral",
            Self::Hungry | Self::Starving => "hungry",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SpeciesTier {
    Common,
    Uncommon,
    Rare,
    Legendary,
}

impl SpeciesTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::Uncommon => "uncommon",
            Self::Rare => "rare",
            Self::Legendary => "legendary",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PetSpecies {
    pub species_id: &'static str,
    pub display_name: &'static str,
    pub tier: SpeciesTier,
    pub weight: i64,
    pub blurb: &'static str,
    pub decay_pct: i64,
    pub restore_pct: i64,
    pub refund_bonus_pp: i64,
    pub match_feed_bonus: i64,
    pub salt_lick_pct: i64,
    pub food_cost_pct: i64,
    pub spit_one_in: i64,
    pub has_aegis: bool,
}

const fn species(
    species_id: &'static str,
    display_name: &'static str,
    tier: SpeciesTier,
    weight: i64,
    blurb: &'static str,
) -> PetSpecies {
    PetSpecies {
        species_id,
        display_name,
        tier,
        weight,
        blurb,
        decay_pct: 100,
        restore_pct: 100,
        refund_bonus_pp: 0,
        match_feed_bonus: 0,
        salt_lick_pct: 100,
        food_cost_pct: 100,
        spit_one_in: 0,
        has_aegis: false,
    }
}

pub static SPECIES: [PetSpecies; 15] = [
    species(
        "common_cama",
        "Common Cama",
        SpeciesTier::Common,
        1_550,
        "A camel-llama hybrid of no particular distinction. Its quirk is having none.",
    ),
    PetSpecies {
        decay_pct: 90,
        ..species(
            "dromedary_cross",
            "Dune Cama",
            SpeciesTier::Common,
            1_500,
            concat!(
                "Takes after its dromedary side: heroic eyelashes, closable nostrils, ",
                "and a modest fat reserve where a hump should be."
            ),
        )
    },
    species(
        "banana_ears",
        "Humming Cama",
        SpeciesTier::Common,
        1_500,
        concat!(
            "Llama-leaning, with the characteristic banana ears. Hums constantly. ",
            "Nobody knows why. It is communication."
        ),
    ),
    PetSpecies {
        match_feed_bonus: 2,
        ..species(
            "embergear_cama",
            "Embergear Cama",
            SpeciesTier::Common,
            1_500,
            concat!(
                "A brass harness hums beneath soot-dark wool. It gathers speed whenever ",
                "a match gets loud, then insists the tiny exhaust puff was a sneeze."
            ),
        )
    },
    PetSpecies {
        refund_bonus_pp: 10,
        ..species(
            "jopacama",
            "Gilded Cama",
            SpeciesTier::Uncommon,
            750,
            concat!(
                "Its wool grows in faint coin-shaped whorls. Auditors are suspicious ",
                "but can prove nothing."
            ),
        )
    },
    PetSpecies {
        decay_pct: 110,
        restore_pct: 110,
        ..species(
            "pudge_cama",
            "Ravenous Cama",
            SpeciesTier::Uncommon,
            750,
            concat!(
                "Round, perpetually famished, and faster at dinner than anything its size ",
                "should be. Something metal glints deep in its wool."
            ),
        )
    },
    PetSpecies {
        match_feed_bonus: 5,
        ..species(
            "courier_cama",
            "Pack Cama",
            SpeciesTier::Uncommon,
            750,
            concat!(
                "Saddlebags, a little flag, big anxious eyes. Lives to deliver. Everyone ",
                "screams when it dies."
            ),
        )
    },
    PetSpecies {
        salt_lick_pct: 125,
        ..species(
            "riverglow_cama",
            "Riverglow Cama",
            SpeciesTier::Uncommon,
            750,
            concat!(
                "Its hooves leave streaks of light on damp ground. It loiters near rivers ",
                "and keeps trying to climb into empty bottles."
            ),
        )
    },
    PetSpecies {
        has_aegis: true,
        ..species(
            "aegis_cama",
            "Shellback Cama",
            SpeciesTier::Rare,
            225,
            concat!(
                "Serene, with a faintly glowing shell across its back. Local legend says ",
                "it gets one more chance than everything else."
            ),
        )
    },
    PetSpecies {
        salt_lick_pct: 150,
        ..species(
            "invoker_cama",
            "Mystic Cama",
            SpeciesTier::Rare,
            225,
            concat!(
                "Violet-robed, insufferably learned. Three small orbs circle its head. ",
                "Enjoys salt more than most scholars admit."
            ),
        )
    },
    PetSpecies {
        food_cost_pct: 90,
        ..species(
            "crystal_cama",
            "Frostwool Cama",
            SpeciesTier::Rare,
            225,
            concat!(
                "Frost-blue wool that never melts. Somehow always finds the snacks at a ",
                "discount. A true support."
            ),
        )
    },
    PetSpecies {
        restore_pct: 105,
        ..species(
            "prismwool_cama",
            "Prismwool Cama",
            SpeciesTier::Rare,
            225,
            concat!(
                "Its fleece catches light in colors that were not there a moment ago. ",
                "Turning it slightly improves lunch for reasons nobody can explain."
            ),
        )
    },
    PetSpecies {
        spit_one_in: 20,
        ..species(
            "rama",
            "Rama the First",
            SpeciesTier::Legendary,
            10,
            concat!(
                "The original. Bred in Dubai in 1998 and described by science as ",
                "'a behavioral disappointment, displaying an extremely poor temperament'. ",
                "Only five ever existed. Yours spits."
            ),
        )
    },
    PetSpecies {
        decay_pct: 80,
        ..species(
            "moondrift_cama",
            "Moondrift Cama",
            SpeciesTier::Legendary,
            20,
            concat!(
                "Its fleece drifts between midnight blue and silver even indoors. It sleeps ",
                "through most meals, wakes for moonrise, and leaves tiny crescent hoofprints."
            ),
        )
    },
    PetSpecies {
        restore_pct: 120,
        ..species(
            "sunspun_cama",
            "Sunspun Cama",
            SpeciesTier::Legendary,
            20,
            concat!(
                "Sun-warm threads coil through its fleece. Breakfast vanishes in a flash of ",
                "gold; the satisfied glow lasts much longer."
            ),
        )
    },
];

static FALLBACK_SPECIES: PetSpecies = species(
    "unknown",
    "Cama of Unknown Origin",
    SpeciesTier::Common,
    0,
    "",
);

#[must_use]
pub fn get_species(species_id: &str) -> &'static PetSpecies {
    SPECIES
        .iter()
        .find(|candidate| candidate.species_id == species_id)
        .unwrap_or(&FALLBACK_SPECIES)
}

#[must_use]
pub fn species_ids_by_tier(tier: SpeciesTier) -> Vec<&'static str> {
    SPECIES
        .iter()
        .filter(|candidate| candidate.tier == tier)
        .map(|candidate| candidate.species_id)
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PetFood {
    pub item_id: &'static str,
    pub display_name: &'static str,
    pub cost: i64,
    pub restore: i64,
    pub emoji: &'static str,
    pub blurb: &'static str,
}

pub static FOOD_ITEMS: [PetFood; 3] = [
    PetFood {
        item_id: "tango",
        display_name: "Tango",
        cost: 9,
        restore: 25,
        emoji: "🌿",
        blurb: "Classic starter regen. Your cama eats a tree.",
    },
    PetFood {
        item_id: "alfalfa",
        display_name: "Alfalfa Bale",
        cost: 17,
        restore: 50,
        emoji: "🌾",
        blurb: "Hearty greens for a hearty hybrid.",
    },
    PetFood {
        item_id: "cheese",
        display_name: "Roshan's Cheese",
        cost: 30,
        restore: 100,
        emoji: "🧀",
        blurb: "Dropped on third death. Smells incredible.",
    },
];

pub static SALT_LICK: PetFood = PetFood {
    item_id: "salt_lick",
    display_name: "Salt Lick",
    cost: 8,
    restore: 0,
    emoji: "🧂",
    blurb: "No nutrition whatsoever. Pure bliss for 12 hours.",
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TierWeights {
    pub common: i64,
    pub uncommon: i64,
    pub rare: i64,
    pub legendary: i64,
}

pub const GILDED_TIER_WEIGHTS: TierWeights = TierWeights {
    common: 0,
    uncommon: 67,
    rare: 31,
    legendary: 2,
};

pub const PITY_TIER_WEIGHTS: TierWeights = TierWeights {
    common: 0,
    uncommon: 81,
    rare: 18,
    legendary: 1,
};

pub const SACRIFICE_TIER_WEIGHTS: [((SpeciesTier, bool), TierWeights); 8] = [
    (
        (SpeciesTier::Common, false),
        TierWeights {
            common: 55,
            uncommon: 33,
            rare: 11,
            legendary: 1,
        },
    ),
    (
        (SpeciesTier::Common, true),
        TierWeights {
            common: 40,
            uncommon: 42,
            rare: 16,
            legendary: 2,
        },
    ),
    (
        (SpeciesTier::Uncommon, false),
        TierWeights {
            common: 30,
            uncommon: 45,
            rare: 22,
            legendary: 3,
        },
    ),
    (
        (SpeciesTier::Uncommon, true),
        TierWeights {
            common: 15,
            uncommon: 50,
            rare: 31,
            legendary: 4,
        },
    ),
    (
        (SpeciesTier::Rare, false),
        TierWeights {
            common: 0,
            uncommon: 55,
            rare: 40,
            legendary: 5,
        },
    ),
    (
        (SpeciesTier::Rare, true),
        TierWeights {
            common: 0,
            uncommon: 40,
            rare: 52,
            legendary: 8,
        },
    ),
    (
        (SpeciesTier::Legendary, false),
        TierWeights {
            common: 0,
            uncommon: 40,
            rare: 50,
            legendary: 10,
        },
    ),
    (
        (SpeciesTier::Legendary, true),
        TierWeights {
            common: 0,
            uncommon: 30,
            rare: 57,
            legendary: 13,
        },
    ),
];

#[must_use]
pub fn sacrifice_tier_weights(tier: SpeciesTier, was_adult: bool) -> TierWeights {
    SACRIFICE_TIER_WEIGHTS
        .iter()
        .find(|((candidate_tier, candidate_adult), _)| {
            *candidate_tier == tier && *candidate_adult == was_adult
        })
        .map_or(
            TierWeights {
                common: 0,
                uncommon: 0,
                rare: 0,
                legendary: 0,
            },
            |(_, weights)| *weights,
        )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PetAccessory {
    pub accessory_id: &'static str,
    pub display_name: &'static str,
    pub tier: SpeciesTier,
    pub weight: i64,
    pub emoji: &'static str,
    pub blurb: &'static str,
    pub anchor: &'static str,
}

pub static ACCESSORIES: [PetAccessory; 8] = [
    PetAccessory {
        accessory_id: "red_bow",
        display_name: "Red Bow",
        tier: SpeciesTier::Common,
        weight: 250,
        emoji: "🎀",
        blurb: "A classic. Ties the whole cama together.",
        anchor: "headwear",
    },
    PetAccessory {
        accessory_id: "straw_hat",
        display_name: "Straw Hat",
        tier: SpeciesTier::Common,
        weight: 250,
        emoji: "👒",
        blurb: "Practical headwear for the discerning grazer.",
        anchor: "headwear",
    },
    PetAccessory {
        accessory_id: "bell_collar",
        display_name: "Bell Collar",
        tier: SpeciesTier::Common,
        weight: 250,
        emoji: "🔔",
        blurb: "Now you always know where it is. It hates that.",
        anchor: "neck",
    },
    PetAccessory {
        accessory_id: "wool_scarf",
        display_name: "Wool Scarf",
        tier: SpeciesTier::Uncommon,
        weight: 90,
        emoji: "🧣",
        blurb: "Wool on wool. Scandalous, cozy.",
        anchor: "neck",
    },
    PetAccessory {
        accessory_id: "flower_crown",
        display_name: "Flower Crown",
        tier: SpeciesTier::Uncommon,
        weight: 90,
        emoji: "🌸",
        blurb: "Woven from tombstone-garden flowers. Don't tell it that.",
        anchor: "headwear",
    },
    PetAccessory {
        accessory_id: "top_hat",
        display_name: "Top Hat",
        tier: SpeciesTier::Rare,
        weight: 30,
        emoji: "🎩",
        blurb: "Immediately becomes the most distinguished animal in the league.",
        anchor: "headwear",
    },
    PetAccessory {
        accessory_id: "aegis_charm",
        display_name: "Aegis Charm",
        tier: SpeciesTier::Rare,
        weight: 30,
        emoji: "🛡️",
        blurb: "A tiny replica. Grants nothing but confidence.",
        anchor: "chest",
    },
    PetAccessory {
        accessory_id: "divine_rapier_pin",
        display_name: "Divine Rapier Pin",
        tier: SpeciesTier::Legendary,
        weight: 10,
        emoji: "⚔️",
        blurb: "So shiny it feels like a throw waiting to happen.",
        anchor: "chest",
    },
];

static FALLBACK_ACCESSORY: PetAccessory = PetAccessory {
    accessory_id: "unknown",
    display_name: "Curious Trinket",
    tier: SpeciesTier::Common,
    weight: 0,
    emoji: "🎁",
    blurb: "",
    anchor: "headwear",
};

#[must_use]
pub fn get_accessory(accessory_id: &str) -> &'static PetAccessory {
    ACCESSORIES
        .iter()
        .find(|candidate| candidate.accessory_id == accessory_id)
        .unwrap_or(&FALLBACK_ACCESSORY)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PetError {
    InvalidDecayRate,
    UnknownFoodItem(String),
    InvalidDigWorkClaim,
    ArithmeticOutOfRange,
}

impl Display for PetError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDecayRate => formatter.write_str("decay rate must be positive"),
            Self::UnknownFoodItem(item_id) => write!(formatter, "unknown pet food: {item_id}"),
            Self::InvalidDigWorkClaim => {
                formatter.write_str("applied pet-work blocks exceed the available offer")
            }
            Self::ArithmeticOutOfRange => {
                formatter.write_str("derived pet value exceeds SQLite INTEGER range")
            }
        }
    }
}

impl Error for PetError {}

#[must_use]
pub fn adoption_fee(prior_deaths: i64) -> i64 {
    let index = prior_deaths.clamp(0, (ADOPTION_FEES.len() - 1) as i64) as usize;
    ADOPTION_FEES[index]
}

pub fn food_cost(species_id: &str, item_id: &str) -> Result<i64, PetError> {
    let item = if item_id == SALT_LICK.item_id {
        &SALT_LICK
    } else {
        FOOD_ITEMS
            .iter()
            .find(|candidate| candidate.item_id == item_id)
            .ok_or_else(|| PetError::UnknownFoodItem(item_id.to_owned()))?
    };
    Ok((item.cost * get_species(species_id).food_cost_pct / 100).max(1))
}

/// Required pre-training/pre-evolution columns in a pet row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetBase {
    pub pet_id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub name: String,
    pub species: String,
    pub adopted_at: i64,
    pub hatched_at: i64,
    pub adopt_fee: i64,
    pub last_fed_at: i64,
    pub hunger_at_last_fed: i64,
    pub times_fed: i64,
    pub feeds_today: i64,
    pub feed_date: Option<String>,
    pub week_consumed_jc: i64,
    pub week_key: Option<String>,
    pub prev_week_consumed_jc: i64,
    pub prev_week_key: Option<String>,
    pub pampered_until: Option<i64>,
    pub accessory: Option<String>,
    pub dig_work_units: i64,
    pub dig_work_at: i64,
    pub aegis_used: i64,
    pub hatch_announced_at: Option<i64>,
    pub died_at: Option<i64>,
    pub death_cause: Option<String>,
    pub death_announced_at: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pet {
    pub pet_id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub name: String,
    pub species: String,
    pub adopted_at: i64,
    pub hatched_at: i64,
    pub adopt_fee: i64,
    pub last_fed_at: i64,
    pub hunger_at_last_fed: i64,
    pub times_fed: i64,
    pub feeds_today: i64,
    pub feed_date: Option<String>,
    pub week_consumed_jc: i64,
    pub week_key: Option<String>,
    pub prev_week_consumed_jc: i64,
    pub prev_week_key: Option<String>,
    pub pampered_until: Option<i64>,
    pub accessory: Option<String>,
    pub dig_work_units: i64,
    pub dig_work_at: i64,
    pub aegis_used: i64,
    pub hatch_announced_at: Option<i64>,
    pub died_at: Option<i64>,
    pub death_cause: Option<String>,
    pub death_announced_at: Option<i64>,
    pub egg_tier: String,
    pub training_xp: i64,
    pub training_str: i64,
    pub training_int: i64,
    pub training_dex: i64,
    pub solo_training_sessions: i64,
    pub solo_training_recharged_at: Option<i64>,
    pub evolution_started_at: Option<i64>,
    pub evolution_due_at: Option<i64>,
    pub evolved_at: Option<i64>,
    pub evolution_calling: Option<String>,
    pub evolution_primary: Option<String>,
    pub evolution_secondary: Option<String>,
    pub evolution_announced_at: Option<i64>,
}

impl Pet {
    /// Construct from the original pet-row fields, applying all Python
    /// dataclass defaults added by later migrations.
    #[must_use]
    pub fn new(base: PetBase) -> Self {
        Self {
            pet_id: base.pet_id,
            discord_id: base.discord_id,
            guild_id: base.guild_id,
            name: base.name,
            species: base.species,
            adopted_at: base.adopted_at,
            hatched_at: base.hatched_at,
            adopt_fee: base.adopt_fee,
            last_fed_at: base.last_fed_at,
            hunger_at_last_fed: base.hunger_at_last_fed,
            times_fed: base.times_fed,
            feeds_today: base.feeds_today,
            feed_date: base.feed_date,
            week_consumed_jc: base.week_consumed_jc,
            week_key: base.week_key,
            prev_week_consumed_jc: base.prev_week_consumed_jc,
            prev_week_key: base.prev_week_key,
            pampered_until: base.pampered_until,
            accessory: base.accessory,
            dig_work_units: base.dig_work_units,
            dig_work_at: base.dig_work_at,
            aegis_used: base.aegis_used,
            hatch_announced_at: base.hatch_announced_at,
            died_at: base.died_at,
            death_cause: base.death_cause,
            death_announced_at: base.death_announced_at,
            egg_tier: "standard".to_owned(),
            training_xp: 0,
            training_str: 0,
            training_int: 0,
            training_dex: 0,
            solo_training_sessions: 3,
            solo_training_recharged_at: None,
            evolution_started_at: None,
            evolution_due_at: None,
            evolved_at: None,
            evolution_calling: None,
            evolution_primary: None,
            evolution_secondary: None,
            evolution_announced_at: None,
        }
    }

    pub fn from_row(row: &PetRow) -> Result<Self, PetRowError> {
        let mut pet = Self::new(PetBase {
            pet_id: required_integer(row, "pet_id")?,
            discord_id: required_integer(row, "discord_id")?,
            guild_id: required_integer(row, "guild_id")?,
            name: required_text(row, "name")?,
            species: required_text(row, "species")?,
            adopted_at: required_integer(row, "adopted_at")?,
            hatched_at: required_integer(row, "hatched_at")?,
            adopt_fee: required_integer(row, "adopt_fee")?,
            last_fed_at: required_integer(row, "last_fed_at")?,
            hunger_at_last_fed: required_integer(row, "hunger_at_last_fed")?,
            times_fed: required_integer(row, "times_fed")?,
            feeds_today: required_integer(row, "feeds_today")?,
            feed_date: required_nullable_text(row, "feed_date")?,
            week_consumed_jc: required_integer(row, "week_consumed_jc")?,
            week_key: required_nullable_text(row, "week_key")?,
            prev_week_consumed_jc: required_integer(row, "prev_week_consumed_jc")?,
            prev_week_key: required_nullable_text(row, "prev_week_key")?,
            pampered_until: required_nullable_integer(row, "pampered_until")?,
            accessory: required_nullable_text(row, "accessory")?,
            dig_work_units: required_integer(row, "dig_work_units")?,
            dig_work_at: required_integer(row, "dig_work_at")?,
            aegis_used: required_integer(row, "aegis_used")?,
            hatch_announced_at: required_nullable_integer(row, "hatch_announced_at")?,
            died_at: required_nullable_integer(row, "died_at")?,
            death_cause: required_nullable_text(row, "death_cause")?,
            death_announced_at: required_nullable_integer(row, "death_announced_at")?,
        });

        pet.egg_tier = default_text(row, "egg_tier", "standard")?;
        pet.training_xp = default_integer(row, "training_xp", 0)?;
        pet.training_str = default_integer(row, "training_str", 0)?;
        pet.training_int = default_integer(row, "training_int", 0)?;
        pet.training_dex = default_integer(row, "training_dex", 0)?;
        pet.solo_training_sessions = default_integer(row, "solo_training_sessions", 3)?;
        pet.solo_training_recharged_at =
            nullable_integer_or_missing(row, "solo_training_recharged_at")?;
        pet.evolution_started_at = nullable_integer_or_missing(row, "evolution_started_at")?;
        pet.evolution_due_at = nullable_integer_or_missing(row, "evolution_due_at")?;
        pet.evolved_at = nullable_integer_or_missing(row, "evolved_at")?;
        pet.evolution_calling = nullable_text_or_missing(row, "evolution_calling")?;
        pet.evolution_primary = nullable_text_or_missing(row, "evolution_primary")?;
        pet.evolution_secondary = nullable_text_or_missing(row, "evolution_secondary")?;
        pet.evolution_announced_at = nullable_integer_or_missing(row, "evolution_announced_at")?;
        Ok(pet)
    }

    #[must_use]
    pub fn current_hunger(&self, now: i64, decay_per_day: i64) -> i64 {
        if self.died_at.is_some() {
            return 0;
        }

        let elapsed = (i128::from(now) - i128::from(self.last_fed_at)).max(0);
        let numerator =
            elapsed * i128::from(decay_per_day) * i128::from(get_species(&self.species).decay_pct);
        let decayed = numerator.div_euclid(i128::from(SECONDS_PER_DAY * 100));
        narrow(i128::from(self.hunger_at_last_fed) - decayed)
            .unwrap_or_else(|_| if decayed.is_negative() { i64::MAX } else { 0 })
            .max(0)
    }

    pub fn starvation_time(&self, decay_per_day: i64) -> Result<i64, PetError> {
        let rate = i128::from(decay_per_day) * i128::from(get_species(&self.species).decay_pct);
        if rate <= 0 {
            return Err(PetError::InvalidDecayRate);
        }
        let points = i128::from(self.hunger_at_last_fed) * i128::from(SECONDS_PER_DAY) * 100;
        narrow(i128::from(self.last_fed_at) + ceil_div(points, rate))
    }

    pub fn is_starved(&self, now: i64, decay_per_day: i64) -> Result<bool, PetError> {
        Ok(self.died_at.is_none() && now >= self.starvation_time(decay_per_day)?)
    }

    pub fn hunger_crossing_time(
        &self,
        threshold: i64,
        decay_per_day: i64,
    ) -> Result<i64, PetError> {
        let rate = i128::from(decay_per_day) * i128::from(get_species(&self.species).decay_pct);
        if rate <= 0 {
            return Err(PetError::InvalidDecayRate);
        }
        let surplus = (self.hunger_at_last_fed - threshold).max(0);
        let points = i128::from(surplus) * i128::from(SECONDS_PER_DAY) * 100;
        narrow(i128::from(self.last_fed_at) + ceil_div(points, rate))
    }

    #[must_use]
    pub fn stage(&self, now: i64) -> PetStage {
        let reference = self.died_at.map_or(now, |died_at| now.min(died_at));
        if reference < self.hatched_at {
            PetStage::Egg
        } else if i128::from(reference)
            < i128::from(self.hatched_at) + i128::from(ADULT_AGE_SECONDS)
        {
            PetStage::Baby
        } else {
            PetStage::Adult
        }
    }

    #[must_use]
    pub fn mood(&self, now: i64, decay_per_day: i64) -> PetMood {
        let hunger = self.current_hunger(now, decay_per_day);
        if self.pampered_until.is_some_and(|until| until > now) && hunger >= MOOD_HUNGRY_MIN {
            return PetMood::Happy;
        }
        if hunger >= MOOD_HAPPY_MIN {
            PetMood::Happy
        } else if hunger >= MOOD_CONTENT_MIN {
            PetMood::Content
        } else if hunger >= MOOD_HUNGRY_MIN {
            PetMood::Hungry
        } else {
            PetMood::Starving
        }
    }

    #[must_use]
    pub fn art_mood(&self, now: i64, decay_per_day: i64) -> &'static str {
        self.mood(now, decay_per_day).art_key()
    }

    pub fn dig_work_units_between(
        &self,
        start: i64,
        end: i64,
        decay_per_day: i64,
    ) -> Result<i64, PetError> {
        let start = start.max(self.hatched_at);
        let stop = self
            .died_at
            .map_or_else(|| self.starvation_time(decay_per_day), Ok)?;
        let end = end.min(stop);
        if end <= start {
            return Ok(0);
        }

        let mut boundaries = BTreeSet::from([start, end]);
        for threshold in [
            MOOD_HAPPY_MIN - 1,
            MOOD_CONTENT_MIN - 1,
            MOOD_HUNGRY_MIN - 1,
        ] {
            let crossing = self.hunger_crossing_time(threshold, decay_per_day)?;
            if start < crossing && crossing < end {
                boundaries.insert(crossing);
            }
        }
        if let Some(until) = self.pampered_until
            && start < until
            && until < end
        {
            boundaries.insert(until);
        }

        let mut living = self.clone();
        living.died_at = None;
        let points: Vec<i64> = boundaries.into_iter().collect();
        let units = points.windows(2).fold(0_i128, |total, pair| {
            let rate = match living.mood(pair[0], decay_per_day) {
                PetMood::Happy => DIG_WORK_RATE_HAPPY,
                PetMood::Content => DIG_WORK_RATE_CONTENT,
                PetMood::Hungry => DIG_WORK_RATE_HUNGRY,
                PetMood::Starving => 0,
            };
            total + i128::from(pair[1] - pair[0]) * i128::from(rate)
        });
        narrow(units)
    }

    #[must_use]
    pub fn age_seconds(&self, now: i64) -> i64 {
        let reference = self.died_at.map_or(now, |died_at| now.min(died_at));
        narrow((i128::from(reference) - i128::from(self.hatched_at)).max(0)).unwrap_or(i64::MAX)
    }

    #[must_use]
    pub fn restored_hunger(&self, now: i64, decay_per_day: i64, restore: i64) -> i64 {
        let boosted = (i128::from(restore) * i128::from(get_species(&self.species).restore_pct))
            .div_euclid(100);
        narrow((i128::from(self.current_hunger(now, decay_per_day)) + boosted).min(100))
            .unwrap_or(if boosted.is_negative() { i64::MIN } else { 100 })
    }

    #[must_use]
    pub fn feeds_used_on(&self, game_date: &str) -> i64 {
        if self.feed_date.as_deref() == Some(game_date) {
            self.feeds_today
        } else {
            0
        }
    }

    #[must_use]
    pub fn consumed_in_week(&self, week_key: &str) -> i64 {
        if self.week_key.as_deref() == Some(week_key) {
            self.week_consumed_jc
        } else if self.prev_week_key.as_deref() == Some(week_key) {
            self.prev_week_consumed_jc
        } else {
            0
        }
    }
}

fn ceil_div(numerator: i128, denominator: i128) -> i128 {
    -(-numerator).div_euclid(denominator)
}

fn narrow(value: i128) -> Result<i64, PetError> {
    i64::try_from(value).map_err(|_| PetError::ArithmeticOutOfRange)
}

/// SQLite scalar values accepted by [`Pet::from_row`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SqliteValue {
    Integer(i64),
    Text(String),
    Null,
}

impl From<i64> for SqliteValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<&str> for SqliteValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for SqliteValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Option<i64>> for SqliteValue {
    fn from(value: Option<i64>) -> Self {
        value.map_or(Self::Null, Self::Integer)
    }
}

pub type PetRow = BTreeMap<String, SqliteValue>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PetRowError {
    MissingColumn(&'static str),
    WrongType {
        column: &'static str,
        expected: &'static str,
    },
}

impl Display for PetRowError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingColumn(column) => write!(formatter, "missing pet column: {column}"),
            Self::WrongType { column, expected } => {
                write!(formatter, "pet column {column} must be {expected}")
            }
        }
    }
}

impl Error for PetRowError {}

fn value<'row>(row: &'row PetRow, column: &'static str) -> Result<&'row SqliteValue, PetRowError> {
    row.get(column).ok_or(PetRowError::MissingColumn(column))
}

fn required_integer(row: &PetRow, column: &'static str) -> Result<i64, PetRowError> {
    match value(row, column)? {
        SqliteValue::Integer(value) => Ok(*value),
        _ => Err(PetRowError::WrongType {
            column,
            expected: "INTEGER",
        }),
    }
}

fn required_text(row: &PetRow, column: &'static str) -> Result<String, PetRowError> {
    match value(row, column)? {
        SqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(PetRowError::WrongType {
            column,
            expected: "TEXT",
        }),
    }
}

fn required_nullable_integer(
    row: &PetRow,
    column: &'static str,
) -> Result<Option<i64>, PetRowError> {
    match value(row, column)? {
        SqliteValue::Integer(value) => Ok(Some(*value)),
        SqliteValue::Null => Ok(None),
        SqliteValue::Text(_) => Err(PetRowError::WrongType {
            column,
            expected: "INTEGER or NULL",
        }),
    }
}

fn required_nullable_text(
    row: &PetRow,
    column: &'static str,
) -> Result<Option<String>, PetRowError> {
    match value(row, column)? {
        SqliteValue::Text(value) => Ok(Some(value.clone())),
        SqliteValue::Null => Ok(None),
        SqliteValue::Integer(_) => Err(PetRowError::WrongType {
            column,
            expected: "TEXT or NULL",
        }),
    }
}

fn default_integer(row: &PetRow, column: &'static str, default: i64) -> Result<i64, PetRowError> {
    match row.get(column) {
        None => Ok(default),
        Some(SqliteValue::Integer(value)) => Ok(*value),
        Some(_) => Err(PetRowError::WrongType {
            column,
            expected: "INTEGER",
        }),
    }
}

fn default_text(row: &PetRow, column: &'static str, default: &str) -> Result<String, PetRowError> {
    match row.get(column) {
        None => Ok(default.to_owned()),
        Some(SqliteValue::Text(value)) => Ok(value.clone()),
        Some(_) => Err(PetRowError::WrongType {
            column,
            expected: "TEXT",
        }),
    }
}

fn nullable_integer_or_missing(
    row: &PetRow,
    column: &'static str,
) -> Result<Option<i64>, PetRowError> {
    match row.get(column) {
        None | Some(SqliteValue::Null) => Ok(None),
        Some(SqliteValue::Integer(value)) => Ok(Some(*value)),
        Some(SqliteValue::Text(_)) => Err(PetRowError::WrongType {
            column,
            expected: "INTEGER or NULL",
        }),
    }
}

fn nullable_text_or_missing(
    row: &PetRow,
    column: &'static str,
) -> Result<Option<String>, PetRowError> {
    match row.get(column) {
        None | Some(SqliteValue::Null) => Ok(None),
        Some(SqliteValue::Text(value)) => Ok(Some(value.clone())),
        Some(SqliteValue::Integer(_)) => Err(PetRowError::WrongType {
            column,
            expected: "TEXT or NULL",
        }),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetDigWork {
    pub pet_id: i64,
    pub pet_name: String,
    pub expected_units: i64,
    pub expected_at: i64,
    pub accrued_units: i64,
    pub as_of: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PetDigWorkClaim {
    pub pet_id: i64,
    pub expected_units: i64,
    pub expected_at: i64,
    pub new_units: i64,
    pub new_at: i64,
}

impl PetDigWork {
    #[must_use]
    pub fn available_blocks(&self) -> i64 {
        (self.accrued_units.div_euclid(DIG_WORK_UNITS_PER_BLOCK)).min(DIG_WORK_PER_DIG_CAP_BLOCKS)
    }

    pub fn claim(&self, applied_blocks: i64) -> Result<PetDigWorkClaim, PetError> {
        if !(0 < applied_blocks && applied_blocks <= self.available_blocks()) {
            return Err(PetError::InvalidDigWorkClaim);
        }
        Ok(PetDigWorkClaim {
            pet_id: self.pet_id,
            expected_units: self.expected_units,
            expected_at: self.expected_at,
            new_units: self.accrued_units - applied_blocks * DIG_WORK_UNITS_PER_BLOCK,
            new_at: self.as_of,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetStatus {
    pub pet: Option<Pet>,
    pub hunger: i64,
    pub stage: Option<PetStage>,
    pub mood: Option<PetMood>,
    pub age_seconds: i64,
    pub supplies: Option<BTreeMap<String, i64>>,
    pub last_dead: Option<Pet>,
    pub dig_work_units: i64,
    pub dig_work_rate: i64,
    pub evolution_hint: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedOutcome {
    pub pet: Pet,
    pub item_id: String,
    pub old_hunger: i64,
    pub new_hunger: i64,
    pub remaining_qty: i64,
    pub feeds_left_today: i64,
    pub spat: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrinketOutcome {
    pub accessory_id: String,
    pub duplicate: bool,
    pub net_cost: i64,
    pub owned_count: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeathNotice {
    pub pet: Pet,
    pub eating_outcome: Option<BTreeMap<String, i64>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HatchNotice {
    pub pet: Pet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvolutionNotice {
    pub pet: Pet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefundPayout {
    pub discord_id: i64,
    pub consumed_jc: i64,
    pub multiplier_pct: i64,
    pub amount: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundNotice {
    pub guild_id: i64,
    pub week_key: String,
    pub payouts: Vec<RefundPayout>,
    pub total_paid: i64,
    pub scaled_down: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000;
    const RATE: i64 = DEFAULT_HUNGER_DECAY_PER_DAY;
    const DAY: i64 = 86_400;

    fn make_pet() -> Pet {
        Pet::new(PetBase {
            pet_id: 1,
            discord_id: 111,
            guild_id: 999,
            name: "Testcama".to_owned(),
            species: "common_cama".to_owned(),
            adopted_at: T0,
            hatched_at: T0 + EGG_HATCH_SECONDS,
            adopt_fee: 20,
            last_fed_at: T0 + EGG_HATCH_SECONDS,
            hunger_at_last_fed: 100,
            times_fed: 0,
            feeds_today: 0,
            feed_date: None,
            week_consumed_jc: 0,
            week_key: None,
            prev_week_consumed_jc: 0,
            prev_week_key: None,
            pampered_until: None,
            accessory: None,
            dig_work_units: 0,
            dig_work_at: T0 + EGG_HATCH_SECONDS,
            aegis_used: 0,
            hatch_announced_at: None,
            died_at: None,
            death_cause: None,
            death_announced_at: None,
        })
    }

    #[test]
    fn test_training_fields_default_to_an_untrained_pet() {
        let pet = make_pet();
        assert_eq!(pet.training_xp, 0);
        assert_eq!(pet.training_str, 0);
        assert_eq!(pet.training_int, 0);
        assert_eq!(pet.training_dex, 0);
        assert_eq!(pet.solo_training_sessions, 3);
        assert_eq!(pet.solo_training_recharged_at, None);
    }

    #[test]
    fn test_full_pet_loses_20_points_per_day() {
        let pet = make_pet();
        let hatch = pet.hatched_at;
        assert_eq!(pet.current_hunger(hatch, RATE), 100);
        assert_eq!(pet.current_hunger(hatch + DAY, RATE), 80);
        assert_eq!(pet.current_hunger(hatch + 2 * DAY + DAY / 2, RATE), 50);
    }

    #[test]
    fn test_hunger_floors_at_zero() {
        let pet = make_pet();
        assert_eq!(pet.current_hunger(pet.hatched_at + 30 * DAY, RATE), 0);
    }

    #[test]
    fn test_egg_does_not_decay_before_hatch() {
        let pet = make_pet();
        assert_eq!(pet.current_hunger(T0, RATE), 100);
        assert_eq!(pet.current_hunger(pet.hatched_at - 1, RATE), 100);
    }

    #[test]
    fn test_starvation_time_is_five_days_after_last_feed() {
        let pet = make_pet();
        assert_eq!(
            pet.starvation_time(RATE).unwrap(),
            pet.last_fed_at + 5 * DAY
        );
    }

    #[test]
    fn test_starvation_boundary_exact() {
        let pet = make_pet();
        let death = pet.starvation_time(RATE).unwrap();
        assert!(!pet.is_starved(death - 1, RATE).unwrap());
        assert!(pet.is_starved(death, RATE).unwrap());
    }

    #[test]
    fn test_starvation_moment_independent_of_discovery_time() {
        let pet = make_pet();
        let death = pet.starvation_time(RATE).unwrap();
        assert!(pet.is_starved(death + 3_600, RATE).unwrap());
        assert!(pet.is_starved(death + 21 * DAY, RATE).unwrap());
        assert_eq!(pet.starvation_time(RATE).unwrap(), death);
    }

    #[test]
    fn test_egg_cannot_starve_invariant() {
        let pet = make_pet();
        assert!(pet.starvation_time(RATE).unwrap() > pet.hatched_at);
        assert!(!pet.is_starved(pet.hatched_at, RATE).unwrap());
    }

    #[test]
    fn test_dead_pet_reports_zero_hunger_and_never_starves_again() {
        let mut pet = make_pet();
        pet.died_at = Some(T0 + 10 * DAY);
        assert_eq!(pet.current_hunger(T0 + 20 * DAY, RATE), 0);
        assert!(!pet.is_starved(T0 + 20 * DAY, RATE).unwrap());
    }

    #[test]
    fn test_hunger_crossing_time_for_warning() {
        let pet = make_pet();
        let crossing = pet.hunger_crossing_time(30, RATE).unwrap();
        assert_eq!(crossing, pet.last_fed_at + 3 * DAY + DAY / 2);
        assert_eq!(pet.current_hunger(crossing, RATE), 30);
    }

    #[test]
    fn test_dune_cama_decays_ten_percent_slower() {
        let mut pet = make_pet();
        pet.species = "dromedary_cross".to_owned();
        assert_eq!(pet.current_hunger(pet.last_fed_at + DAY, RATE), 82);
    }

    #[test]
    fn test_ravenous_cama_decays_faster_but_restores_more() {
        let mut pet = make_pet();
        pet.species = "pudge_cama".to_owned();
        assert_eq!(pet.current_hunger(pet.last_fed_at + DAY, RATE), 78);
        let now = pet.last_fed_at + 2 * DAY;
        assert_eq!(pet.restored_hunger(now, RATE, 25), 56 + 27);
    }

    #[test]
    fn test_restore_caps_at_max_hunger() {
        let pet = make_pet();
        let now = pet.last_fed_at + DAY;
        assert_eq!(pet.restored_hunger(now, RATE, 100), 100);
    }

    #[test]
    fn test_shellback_has_aegis_and_rama_spits() {
        assert!(get_species("aegis_cama").has_aegis);
        assert_eq!(get_species("rama").spit_one_in, 20);
        assert!(!get_species("common_cama").has_aegis);
    }

    #[test]
    fn test_frostwool_food_discount() {
        assert_eq!(food_cost("crystal_cama", "cheese").unwrap(), 27);
        assert_eq!(food_cost("common_cama", "cheese").unwrap(), 30);
    }

    #[test]
    fn test_unknown_species_falls_back_to_quirkless() {
        let mut pet = make_pet();
        pet.species = "retired_species".to_owned();
        assert_eq!(pet.current_hunger(pet.last_fed_at + DAY, RATE), 80);
    }

    #[test]
    fn test_stage_boundaries() {
        let pet = make_pet();
        assert_eq!(pet.stage(T0), PetStage::Egg);
        assert_eq!(pet.stage(pet.hatched_at - 1), PetStage::Egg);
        assert_eq!(pet.stage(pet.hatched_at), PetStage::Baby);
        assert_eq!(
            pet.stage(pet.hatched_at + ADULT_AGE_SECONDS - 1),
            PetStage::Baby
        );
        assert_eq!(
            pet.stage(pet.hatched_at + ADULT_AGE_SECONDS),
            PetStage::Adult
        );
    }

    #[test]
    fn test_dead_pet_stage_frozen_at_death() {
        let died = T0 + EGG_HATCH_SECONDS + 2 * DAY;
        let mut pet = make_pet();
        pet.died_at = Some(died);
        assert_eq!(pet.stage(died + 365 * DAY), PetStage::Baby);
    }

    #[test]
    fn test_age_zero_for_egg_and_frozen_at_death() {
        let pet = make_pet();
        assert_eq!(pet.age_seconds(T0 + 100), 0);
        let died = pet.hatched_at + 3 * DAY;
        let mut dead = make_pet();
        dead.died_at = Some(died);
        assert_eq!(dead.age_seconds(died + 50 * DAY), 3 * DAY);
    }

    #[test]
    fn test_mood_bands() {
        let pet = make_pet();
        let feed = pet.last_fed_at;
        assert_eq!(pet.mood(feed, RATE), PetMood::Happy);
        assert_eq!(pet.mood(feed + 2 * DAY, RATE), PetMood::Content);
        assert_eq!(pet.mood(feed + 4 * DAY, RATE), PetMood::Hungry);
        assert_eq!(pet.mood(feed + 4 * DAY + DAY / 2, RATE), PetMood::Starving);
    }

    #[test]
    fn test_pampered_overrides_unless_starving() {
        let mut pet = make_pet();
        pet.pampered_until = Some(T0 + 30 * DAY);
        let feed = pet.last_fed_at;
        assert_eq!(pet.mood(feed + 4 * DAY, RATE), PetMood::Happy);
        assert_eq!(pet.mood(feed + 4 * DAY + DAY / 2, RATE), PetMood::Starving);
    }

    #[test]
    fn test_art_mood_collapses_to_three_keys() {
        let pet = make_pet();
        let feed = pet.last_fed_at;
        assert_eq!(pet.art_mood(feed, RATE), "happy");
        assert_eq!(pet.art_mood(feed + 2 * DAY, RATE), "neutral");
        assert_eq!(pet.art_mood(feed + 4 * DAY, RATE), "hungry");
    }

    #[test]
    fn test_accrues_across_historical_mood_bands() {
        let pet = make_pet();
        let units = pet
            .dig_work_units_between(pet.hatched_at, pet.hatched_at + 2 * DAY, RATE)
            .unwrap();
        assert_eq!(units, 22 * DAY + DAY / 5);
    }

    #[test]
    fn test_pampering_uses_the_happy_rate_for_that_interval() {
        let mut pet = make_pet();
        pet.hunger_at_last_fed = 60;
        pet.pampered_until = Some(T0 + EGG_HATCH_SECONDS + DAY);
        assert_eq!(
            pet.dig_work_units_between(pet.hatched_at, pet.hatched_at + DAY, RATE)
                .unwrap(),
            12 * DAY
        );
    }

    #[test]
    fn test_eggs_and_time_after_starvation_do_not_produce() {
        let pet = make_pet();
        let units = pet
            .dig_work_units_between(pet.adopted_at, pet.hatched_at + 10 * DAY, RATE)
            .unwrap();
        assert_eq!(units, 34 * DAY + 7 * DAY / 20);
    }

    #[test]
    fn test_adoption_fee_escalates_and_clamps() {
        assert_eq!(adoption_fee(0), 20);
        assert_eq!(adoption_fee(1), 50);
        assert_eq!(adoption_fee(2), 75);
        assert_eq!(adoption_fee(3), 100);
        assert_eq!(adoption_fee(50), 100);
        assert_eq!(adoption_fee(-1), 20);
    }

    #[test]
    fn test_species_weights_sum_to_total() {
        assert_eq!(
            SPECIES.iter().map(|species| species.weight).sum::<i64>(),
            10_000
        );
    }

    #[test]
    fn test_new_species_have_distinct_care_quirks() {
        assert_eq!(
            (
                get_species("embergear_cama").display_name,
                get_species("embergear_cama").tier,
                get_species("embergear_cama").match_feed_bonus,
                get_species("embergear_cama").salt_lick_pct,
                get_species("embergear_cama").restore_pct,
            ),
            ("Embergear Cama", SpeciesTier::Common, 2, 100, 100)
        );
        assert_eq!(
            (
                get_species("riverglow_cama").display_name,
                get_species("riverglow_cama").tier,
                get_species("riverglow_cama").match_feed_bonus,
                get_species("riverglow_cama").salt_lick_pct,
                get_species("riverglow_cama").restore_pct,
            ),
            ("Riverglow Cama", SpeciesTier::Uncommon, 0, 125, 100)
        );
        assert_eq!(
            (
                get_species("prismwool_cama").display_name,
                get_species("prismwool_cama").tier,
                get_species("prismwool_cama").match_feed_bonus,
                get_species("prismwool_cama").salt_lick_pct,
                get_species("prismwool_cama").restore_pct,
            ),
            ("Prismwool Cama", SpeciesTier::Rare, 0, 100, 105)
        );
    }

    #[test]
    fn test_species_weights_define_reduced_legendary_odds() {
        let tier_weight = |tier| {
            SPECIES
                .iter()
                .filter(|species| species.tier == tier)
                .map(|species| species.weight)
                .sum::<i64>()
        };
        assert_eq!(tier_weight(SpeciesTier::Common), 6_050);
        assert_eq!(tier_weight(SpeciesTier::Uncommon), 3_000);
        assert_eq!(tier_weight(SpeciesTier::Rare), 900);
        assert_eq!(tier_weight(SpeciesTier::Legendary), 50);
    }

    #[test]
    fn test_legendary_species_are_rare_and_varied_across_hatch_paths() {
        let legendary: BTreeMap<_, _> = SPECIES
            .iter()
            .filter(|species| species.tier == SpeciesTier::Legendary)
            .map(|species| (species.species_id, species.weight))
            .collect();
        assert_eq!(
            legendary,
            BTreeMap::from([("rama", 10), ("moondrift_cama", 20), ("sunspun_cama", 20)])
        );
        assert_eq!(
            GILDED_TIER_WEIGHTS,
            TierWeights {
                common: 0,
                uncommon: 67,
                rare: 31,
                legendary: 2,
            }
        );
        assert_eq!(
            PITY_TIER_WEIGHTS,
            TierWeights {
                common: 0,
                uncommon: 81,
                rare: 18,
                legendary: 1,
            }
        );
        assert_eq!(
            SACRIFICE_TIER_WEIGHTS.map(|(key, weights)| (key, weights.legendary)),
            [
                ((SpeciesTier::Common, false), 1),
                ((SpeciesTier::Common, true), 2),
                ((SpeciesTier::Uncommon, false), 3),
                ((SpeciesTier::Uncommon, true), 4),
                ((SpeciesTier::Rare, false), 5),
                ((SpeciesTier::Rare, true), 8),
                ((SpeciesTier::Legendary, false), 10),
                ((SpeciesTier::Legendary, true), 13),
            ]
        );
    }

    #[test]
    fn test_new_legendary_species_have_distinct_care_quirks() {
        assert_eq!(
            (
                get_species("moondrift_cama").display_name,
                get_species("moondrift_cama").tier,
                get_species("moondrift_cama").decay_pct,
                get_species("moondrift_cama").restore_pct,
            ),
            ("Moondrift Cama", SpeciesTier::Legendary, 80, 100)
        );
        assert_eq!(
            (
                get_species("sunspun_cama").display_name,
                get_species("sunspun_cama").tier,
                get_species("sunspun_cama").decay_pct,
                get_species("sunspun_cama").restore_pct,
            ),
            ("Sunspun Cama", SpeciesTier::Legendary, 100, 120)
        );
    }

    #[test]
    fn test_feeds_used_on_resets_across_game_days() {
        let mut pet = make_pet();
        pet.feeds_today = 4;
        pet.feed_date = Some("2026-07-25".to_owned());
        assert_eq!(pet.feeds_used_on("2026-07-25"), 4);
        assert_eq!(pet.feeds_used_on("2026-07-26"), 0);
    }

    #[test]
    fn test_from_row_round_trip() {
        let row = PetRow::from([
            ("pet_id".to_owned(), 1_i64.into()),
            ("discord_id".to_owned(), 111_i64.into()),
            ("guild_id".to_owned(), 999_i64.into()),
            ("name".to_owned(), "Testcama".into()),
            ("species".to_owned(), "common_cama".into()),
            ("adopted_at".to_owned(), T0.into()),
            ("hatched_at".to_owned(), (T0 + EGG_HATCH_SECONDS).into()),
            ("adopt_fee".to_owned(), 20_i64.into()),
            ("last_fed_at".to_owned(), (T0 + EGG_HATCH_SECONDS).into()),
            ("hunger_at_last_fed".to_owned(), 100_i64.into()),
            ("times_fed".to_owned(), 0_i64.into()),
            ("feeds_today".to_owned(), 0_i64.into()),
            ("feed_date".to_owned(), SqliteValue::Null),
            ("week_consumed_jc".to_owned(), 0_i64.into()),
            ("week_key".to_owned(), SqliteValue::Null),
            ("prev_week_consumed_jc".to_owned(), 0_i64.into()),
            ("prev_week_key".to_owned(), SqliteValue::Null),
            ("pampered_until".to_owned(), SqliteValue::Null),
            ("accessory".to_owned(), SqliteValue::Null),
            ("dig_work_units".to_owned(), 0_i64.into()),
            ("dig_work_at".to_owned(), (T0 + EGG_HATCH_SECONDS).into()),
            ("aegis_used".to_owned(), 0_i64.into()),
            ("hatch_announced_at".to_owned(), SqliteValue::Null),
            ("died_at".to_owned(), SqliteValue::Null),
            ("death_cause".to_owned(), SqliteValue::Null),
            ("death_announced_at".to_owned(), SqliteValue::Null),
        ]);
        assert_eq!(Pet::from_row(&row).unwrap(), make_pet());
    }

    #[test]
    fn non_positive_decay_is_rejected_for_boundary_queries() {
        let pet = make_pet();
        assert_eq!(pet.starvation_time(0), Err(PetError::InvalidDecayRate));
        assert_eq!(
            pet.hunger_crossing_time(30, -1),
            Err(PetError::InvalidDecayRate)
        );
    }

    #[test]
    fn consumed_week_prefers_current_then_previous_slot() {
        let mut pet = make_pet();
        pet.week_key = Some("2026-W31".to_owned());
        pet.week_consumed_jc = 14;
        pet.prev_week_key = Some("2026-W30".to_owned());
        pet.prev_week_consumed_jc = 9;
        assert_eq!(pet.consumed_in_week("2026-W31"), 14);
        assert_eq!(pet.consumed_in_week("2026-W30"), 9);
        assert_eq!(pet.consumed_in_week("2026-W29"), 0);
    }

    #[test]
    fn dig_work_claim_is_capped_and_consumes_exact_units() {
        let offer = PetDigWork {
            pet_id: 3,
            pet_name: "Worker".to_owned(),
            expected_units: 99,
            expected_at: 100,
            accrued_units: 20 * DIG_WORK_UNITS_PER_BLOCK,
            as_of: 200,
        };
        assert_eq!(offer.available_blocks(), 12);
        let claim = offer.claim(12).unwrap();
        assert_eq!(claim.new_units, 8 * DIG_WORK_UNITS_PER_BLOCK);
        assert_eq!(offer.claim(13), Err(PetError::InvalidDigWorkClaim));
    }
}
