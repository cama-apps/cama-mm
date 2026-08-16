//! Pure Dig gear domain model and transactional aggregate.
//!
//! The Python implementation currently spreads this behavior across the gear
//! model, constants, service mixin, repository, and Discord embed builder. This
//! module keeps the same policy in one bounded slice. Persistence and Discord
//! adapters can implement the same commands around this aggregate without
//! changing the formulas or state transitions.

use std::collections::{BTreeMap, BTreeSet};

pub const GEAR_MAX_DURABILITY: i32 = 20;
pub const GEAR_REPAIR_COST_PCT: f64 = 0.10;
pub const GEAR_BOSS_DROP_RATE: f64 = 0.07;
pub const PLAYER_HIT_FLOOR: f64 = 0.05;
pub const PLAYER_HIT_CEILING: f64 = 0.90;
pub const RELIC_SLOTS_BASE: i32 = 1;
pub const RELIC_SLOTS_MAX: i32 = 6;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GearSlot {
    Weapon,
    Armor,
    Boots,
    Amulet,
    Ring,
    Relic,
}

impl GearSlot {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Weapon => "weapon",
            Self::Armor => "armor",
            Self::Boots => "boots",
            Self::Amulet => "amulet",
            Self::Ring => "ring",
            Self::Relic => "relic",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "weapon" => Some(Self::Weapon),
            "armor" => Some(Self::Armor),
            "boots" => Some(Self::Boots),
            "amulet" => Some(Self::Amulet),
            "ring" => Some(Self::Ring),
            "relic" => Some(Self::Relic),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GearDefinition {
    pub name: &'static str,
    pub tier: u8,
    pub slot: GearSlot,
    pub player_dmg: i32,
    pub player_hit: f64,
    pub player_hp_bonus: i32,
    pub boss_hit_reduction: f64,
    pub crit_chance: f64,
    pub crit_bonus: i32,
    pub advance_bonus: i32,
    pub cave_in_reduction: f64,
    pub loot_bonus: i32,
    pub shop_price: i64,
    pub depth_required: i64,
    pub prestige_required: i32,
}

#[allow(
    clippy::too_many_arguments,
    reason = "the authored gear table mirrors the Python dataclass columns"
)]
const fn gear(
    name: &'static str,
    tier: u8,
    slot: GearSlot,
    player_dmg: i32,
    player_hit: f64,
    player_hp_bonus: i32,
    boss_hit_reduction: f64,
    crit_chance: f64,
    crit_bonus: i32,
    advance_bonus: i32,
    cave_in_reduction: f64,
    loot_bonus: i32,
    shop_price: i64,
    depth_required: i64,
    prestige_required: i32,
) -> GearDefinition {
    GearDefinition {
        name,
        tier,
        slot,
        player_dmg,
        player_hit,
        player_hp_bonus,
        boss_hit_reduction,
        crit_chance,
        crit_bonus,
        advance_bonus,
        cave_in_reduction,
        loot_bonus,
        shop_price,
        depth_required,
        prestige_required,
    }
}

pub static WEAPON_TIERS: [GearDefinition; 8] = [
    gear(
        "Wooden Pickaxe",
        0,
        GearSlot::Weapon,
        0,
        0.00,
        0,
        0.0,
        0.0,
        0,
        0,
        0.00,
        0,
        0,
        0,
        0,
    ),
    gear(
        "Stone Pickaxe",
        1,
        GearSlot::Weapon,
        0,
        0.01,
        0,
        0.0,
        0.0,
        0,
        1,
        0.00,
        0,
        15,
        25,
        0,
    ),
    gear(
        "Iron Pickaxe",
        2,
        GearSlot::Weapon,
        0,
        0.02,
        0,
        0.0,
        0.0,
        0,
        1,
        0.05,
        0,
        50,
        50,
        0,
    ),
    gear(
        "Diamond Pickaxe",
        3,
        GearSlot::Weapon,
        1,
        0.03,
        0,
        0.0,
        0.0,
        0,
        2,
        0.05,
        2,
        150,
        75,
        0,
    ),
    gear(
        "Obsidian Pickaxe",
        4,
        GearSlot::Weapon,
        1,
        0.04,
        0,
        0.0,
        0.0,
        0,
        3,
        0.10,
        3,
        300,
        100,
        1,
    ),
    gear(
        "Stormrend Pickaxe",
        5,
        GearSlot::Weapon,
        1,
        0.045,
        0,
        0.0,
        0.0,
        0,
        3,
        0.15,
        3,
        450,
        150,
        2,
    ),
    gear(
        "Frostforged Pickaxe",
        6,
        GearSlot::Weapon,
        2,
        0.05,
        0,
        0.0,
        0.0,
        0,
        3,
        0.20,
        3,
        600,
        200,
        3,
    ),
    gear(
        "Void-Touched Pickaxe",
        7,
        GearSlot::Weapon,
        2,
        0.07,
        0,
        0.0,
        0.0,
        0,
        4,
        0.20,
        5,
        1_200,
        275,
        5,
    ),
];

pub static ARMOR_TIERS: [GearDefinition; 8] = [
    gear(
        "Wooden Chestguard",
        0,
        GearSlot::Armor,
        0,
        0.0,
        0,
        0.0,
        0.0,
        0,
        0,
        0.0,
        0,
        0,
        0,
        0,
    ),
    gear(
        "Stone Cuirass",
        1,
        GearSlot::Armor,
        0,
        0.0,
        0,
        0.0,
        0.0,
        0,
        0,
        0.0,
        0,
        20,
        25,
        0,
    ),
    gear(
        "Iron Hauberk",
        2,
        GearSlot::Armor,
        0,
        0.0,
        1,
        0.0,
        0.0,
        0,
        0,
        0.0,
        0,
        60,
        50,
        0,
    ),
    gear(
        "Diamond Breastplate",
        3,
        GearSlot::Armor,
        0,
        0.0,
        2,
        0.0,
        0.0,
        0,
        0,
        0.0,
        0,
        180,
        75,
        0,
    ),
    gear(
        "Obsidian Brigandine",
        4,
        GearSlot::Armor,
        0,
        0.0,
        3,
        0.0,
        0.0,
        0,
        0,
        0.0,
        0,
        350,
        100,
        1,
    ),
    gear(
        "Stormrend Lamellar",
        5,
        GearSlot::Armor,
        0,
        0.0,
        3,
        0.0,
        0.0,
        0,
        0,
        0.0,
        0,
        525,
        150,
        2,
    ),
    gear(
        "Frostforged Carapace",
        6,
        GearSlot::Armor,
        0,
        0.0,
        3,
        0.0,
        0.0,
        0,
        0,
        0.0,
        0,
        700,
        200,
        3,
    ),
    gear(
        "Void-Touched Bulwark",
        7,
        GearSlot::Armor,
        0,
        0.0,
        4,
        0.0,
        0.0,
        0,
        0,
        0.0,
        0,
        1_400,
        275,
        5,
    ),
];

pub static BOOTS_TIERS: [GearDefinition; 8] = [
    gear(
        "Wooden Clogs",
        0,
        GearSlot::Boots,
        0,
        0.0,
        0,
        0.00,
        0.0,
        0,
        0,
        0.0,
        0,
        0,
        0,
        0,
    ),
    gear(
        "Stone Galoshes",
        1,
        GearSlot::Boots,
        0,
        0.0,
        0,
        0.02,
        0.0,
        0,
        0,
        0.0,
        0,
        25,
        25,
        0,
    ),
    gear(
        "Iron Sabatons",
        2,
        GearSlot::Boots,
        0,
        0.0,
        0,
        0.04,
        0.0,
        0,
        0,
        0.0,
        0,
        70,
        50,
        0,
    ),
    gear(
        "Diamond Greaves",
        3,
        GearSlot::Boots,
        0,
        0.0,
        0,
        0.06,
        0.0,
        0,
        0,
        0.0,
        0,
        200,
        75,
        0,
    ),
    gear(
        "Obsidian Stompers",
        4,
        GearSlot::Boots,
        0,
        0.0,
        0,
        0.08,
        0.0,
        0,
        0,
        0.0,
        0,
        400,
        100,
        1,
    ),
    gear(
        "Stormrend Striders",
        5,
        GearSlot::Boots,
        0,
        0.0,
        0,
        0.09,
        0.0,
        0,
        0,
        0.0,
        0,
        600,
        150,
        2,
    ),
    gear(
        "Frostforged Treads",
        6,
        GearSlot::Boots,
        0,
        0.0,
        0,
        0.10,
        0.0,
        0,
        0,
        0.0,
        0,
        800,
        200,
        3,
    ),
    gear(
        "Void-Touched Sollerets",
        7,
        GearSlot::Boots,
        0,
        0.0,
        0,
        0.13,
        0.0,
        0,
        0,
        0.0,
        0,
        1_500,
        275,
        5,
    ),
];

pub static AMULET_TIERS: [GearDefinition; 8] = [
    gear(
        "Twine Cord",
        0,
        GearSlot::Amulet,
        0,
        0.0,
        0,
        0.0,
        0.00,
        0,
        0,
        0.0,
        0,
        0,
        0,
        0,
    ),
    gear(
        "Stone Pendant",
        1,
        GearSlot::Amulet,
        0,
        0.0,
        0,
        0.0,
        0.02,
        0,
        0,
        0.0,
        0,
        25,
        25,
        0,
    ),
    gear(
        "Iron Talisman",
        2,
        GearSlot::Amulet,
        0,
        0.0,
        0,
        0.0,
        0.04,
        0,
        0,
        0.0,
        0,
        70,
        50,
        0,
    ),
    gear(
        "Diamond Charm",
        3,
        GearSlot::Amulet,
        0,
        0.0,
        0,
        0.0,
        0.06,
        0,
        0,
        0.0,
        0,
        200,
        75,
        0,
    ),
    gear(
        "Obsidian Amulet",
        4,
        GearSlot::Amulet,
        0,
        0.0,
        0,
        0.0,
        0.07,
        1,
        0,
        0.0,
        0,
        400,
        100,
        1,
    ),
    gear(
        "Stormrend Necklace",
        5,
        GearSlot::Amulet,
        0,
        0.0,
        0,
        0.0,
        0.08,
        1,
        0,
        0.0,
        0,
        600,
        150,
        2,
    ),
    gear(
        "Frostforged Pendant",
        6,
        GearSlot::Amulet,
        0,
        0.0,
        0,
        0.0,
        0.09,
        1,
        0,
        0.0,
        0,
        800,
        200,
        3,
    ),
    gear(
        "Void-Touched Talisman",
        7,
        GearSlot::Amulet,
        0,
        0.0,
        0,
        0.0,
        0.10,
        1,
        0,
        0.0,
        0,
        1_500,
        275,
        5,
    ),
];

#[must_use]
pub fn tier_table(slot: GearSlot) -> Option<&'static [GearDefinition; 8]> {
    match slot {
        GearSlot::Weapon => Some(&WEAPON_TIERS),
        GearSlot::Armor => Some(&ARMOR_TIERS),
        GearSlot::Boots => Some(&BOOTS_TIERS),
        GearSlot::Amulet => Some(&AMULET_TIERS),
        GearSlot::Ring | GearSlot::Relic => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UniqueGearDefinition {
    pub item_id: &'static str,
    pub name: &'static str,
    pub slot: GearSlot,
    pub reference_tier: u8,
    pub repair_value: i64,
    pub max_durability: i32,
    pub player_dmg: i32,
    pub player_hit: f64,
    pub player_hp_bonus: i32,
    pub boss_hit_reduction: f64,
    pub crit_chance: f64,
    pub crit_bonus: i32,
    pub advance_bonus: i32,
    pub cave_in_reduction: f64,
    pub loot_bonus: i32,
    pub effect_id: Option<&'static str>,
    pub effect_summary: &'static str,
}

pub const GLASSBREAKER_PICK: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "glassbreaker_pick",
    name: "Glassbreaker Pick",
    slot: GearSlot::Weapon,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 8,
    player_dmg: 2,
    player_hit: -0.08,
    player_hp_bonus: 0,
    boss_hit_reduction: 0.0,
    crit_chance: 0.0,
    crit_bonus: 0,
    advance_bonus: 2,
    cave_in_reduction: 0.05,
    loot_bonus: 2,
    effect_id: None,
    effect_summary: "Diamond dig bonuses; +2 boss damage; -8% hit chance.",
};

pub const NEEDLE_PICK: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "needle_pick",
    name: "Needle Pick",
    slot: GearSlot::Weapon,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 16,
    player_dmg: 0,
    player_hit: 0.08,
    player_hp_bonus: 0,
    boss_hit_reduction: 0.0,
    crit_chance: 0.03,
    crit_bonus: 0,
    advance_bonus: 2,
    cave_in_reduction: 0.05,
    loot_bonus: 2,
    effect_id: None,
    effect_summary: "Diamond dig bonuses; +8% hit chance; +3% crit chance.",
};

pub const BRIARPLATE: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "briarplate",
    name: "Briarplate",
    slot: GearSlot::Armor,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 14,
    player_dmg: 0,
    player_hit: 0.0,
    player_hp_bonus: 1,
    boss_hit_reduction: 0.0,
    crit_chance: 0.0,
    crit_bonus: 0,
    advance_bonus: 0,
    cave_in_reduction: 0.0,
    loot_bonus: 0,
    effect_id: Some("reflect_first_hit"),
    effect_summary: "+1 HP; reflect 1 damage on the first boss hit.",
};

pub const NULLWEAVE_MANTLE: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "nullweave_mantle",
    name: "Nullweave Mantle",
    slot: GearSlot::Armor,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 12,
    player_dmg: 0,
    player_hit: 0.0,
    player_hp_bonus: 0,
    boss_hit_reduction: 0.0,
    crit_chance: 0.0,
    crit_bonus: 0,
    advance_bonus: 0,
    cave_in_reduction: 0.0,
    loot_bonus: 0,
    effect_id: Some("block_first_status"),
    effect_summary: "Block the first boss status effect.",
};

pub const SPRINGHEEL_BOOTS: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "springheel_boots",
    name: "Springheel Boots",
    slot: GearSlot::Boots,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 14,
    player_dmg: 0,
    player_hit: 0.0,
    player_hp_bonus: 0,
    boss_hit_reduction: 0.04,
    crit_chance: 0.0,
    crit_bonus: 0,
    advance_bonus: 0,
    cave_in_reduction: 0.0,
    loot_bonus: 0,
    effect_id: Some("springheel_counter"),
    effect_summary: "-4% boss hit chance; counter the first boss miss for 1 damage.",
};

pub const ANCHOR_BOOTS: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "anchor_boots",
    name: "Anchor Boots",
    slot: GearSlot::Boots,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 16,
    player_dmg: 0,
    player_hit: 0.0,
    player_hp_bonus: 1,
    boss_hit_reduction: 0.0,
    crit_chance: 0.0,
    crit_bonus: 0,
    advance_bonus: 0,
    cave_in_reduction: 0.0,
    loot_bonus: 0,
    effect_id: Some("block_first_skip"),
    effect_summary: "+1 HP; block the first skipped player round.",
};

pub const LOADED_DIE: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "loaded_die",
    name: "Loaded Die",
    slot: GearSlot::Amulet,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 12,
    player_dmg: 0,
    player_hit: -0.05,
    player_hp_bonus: 0,
    boss_hit_reduction: 0.0,
    crit_chance: 0.10,
    crit_bonus: 1,
    advance_bonus: 0,
    cave_in_reduction: 0.0,
    loot_bonus: 0,
    effect_id: None,
    effect_summary: "-5% hit chance; +10% crit chance; +1 crit damage.",
};

pub const BLOOD_LOCKET: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "blood_locket",
    name: "Blood Locket",
    slot: GearSlot::Amulet,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 14,
    player_dmg: 0,
    player_hit: 0.0,
    player_hp_bonus: -1,
    boss_hit_reduction: 0.0,
    crit_chance: 0.05,
    crit_bonus: 0,
    advance_bonus: 0,
    cave_in_reduction: 0.0,
    loot_bonus: 0,
    effect_id: Some("heal_first_crit"),
    effect_summary: "-1 HP; +5% crit chance; first crit heals 1 HP.",
};

pub const SURVEYORS_LOOP: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "surveyors_loop",
    name: "Surveyor's Loop",
    slot: GearSlot::Ring,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 14,
    player_dmg: 0,
    player_hit: 0.0,
    player_hp_bonus: 0,
    boss_hit_reduction: 0.0,
    crit_chance: 0.0,
    crit_bonus: 0,
    advance_bonus: 0,
    cave_in_reduction: 0.0,
    loot_bonus: 0,
    effect_id: Some("safe_event_stride"),
    effect_summary: "Safe event successes gain +1 block; risky and desperate choices lose 5% success chance.",
};

pub const RUINWAGER_SIGNET: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "ruinwager_signet",
    name: "Ruinwager Signet",
    slot: GearSlot::Ring,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 14,
    player_dmg: 0,
    player_hit: 0.0,
    player_hp_bonus: 0,
    boss_hit_reduction: 0.0,
    crit_chance: 0.0,
    crit_bonus: 0,
    advance_bonus: 0,
    cave_in_reduction: 0.0,
    loot_bonus: 0,
    effect_id: Some("risky_event_edge"),
    effect_summary: "Risky and desperate choices gain +5% success chance; failed JC losses are 25% harsher.",
};

pub const RED_THREAD_BAND: UniqueGearDefinition = UniqueGearDefinition {
    item_id: "red_thread_band",
    name: "Red Thread Band",
    slot: GearSlot::Ring,
    reference_tier: 3,
    repair_value: 200,
    max_durability: 14,
    player_dmg: 0,
    player_hit: 0.0,
    player_hp_bonus: -1,
    boss_hit_reduction: 0.0,
    crit_chance: 0.0,
    crit_bonus: 0,
    advance_bonus: 0,
    cave_in_reduction: 0.0,
    loot_bonus: 0,
    effect_id: Some("reroll_first_miss"),
    effect_summary: "-1 HP; reroll the first missed boss attack each fight.",
};

#[must_use]
pub fn unique_gear(item_id: &str) -> Option<&'static UniqueGearDefinition> {
    match item_id {
        "glassbreaker_pick" => Some(&GLASSBREAKER_PICK),
        "needle_pick" => Some(&NEEDLE_PICK),
        "briarplate" => Some(&BRIARPLATE),
        "nullweave_mantle" => Some(&NULLWEAVE_MANTLE),
        "springheel_boots" => Some(&SPRINGHEEL_BOOTS),
        "anchor_boots" => Some(&ANCHOR_BOOTS),
        "loaded_die" => Some(&LOADED_DIE),
        "blood_locket" => Some(&BLOOD_LOCKET),
        "surveyors_loop" => Some(&SURVEYORS_LOOP),
        "ruinwager_signet" => Some(&RUINWAGER_SIGNET),
        "red_thread_band" => Some(&RED_THREAD_BAND),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GearPiece {
    pub id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub slot: GearSlot,
    pub tier: u8,
    pub durability: i32,
    pub equipped: bool,
    pub acquired_at: i64,
    pub source: String,
    pub item_id: Option<String>,
}

impl GearPiece {
    #[must_use]
    pub fn max_durability(&self) -> i32 {
        self.item_id
            .as_deref()
            .and_then(unique_gear)
            .map_or(GEAR_MAX_DURABILITY, |definition| definition.max_durability)
    }

    #[must_use]
    pub fn name(&self) -> Option<&'static str> {
        if let Some(item_id) = self.item_id.as_deref() {
            return unique_gear(item_id).map(|definition| definition.name);
        }
        tier_table(self.slot)
            .and_then(|table| table.get(usize::from(self.tier)))
            .map(|definition| definition.name)
    }

    #[must_use]
    pub fn effect_summary(&self) -> Option<&'static str> {
        self.item_id
            .as_deref()
            .and_then(unique_gear)
            .map(|definition| definition.effect_summary)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CombatModifiers {
    pub player_dmg: i32,
    pub player_hit: f64,
    pub player_hp_bonus: i32,
    pub boss_hit_reduction: f64,
    pub crit_chance: f64,
    pub crit_bonus: i32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GearLoadout {
    pub weapon: Option<GearPiece>,
    pub armor: Option<GearPiece>,
    pub boots: Option<GearPiece>,
    pub amulet: Option<GearPiece>,
    pub ring: Option<GearPiece>,
    pub relics: Vec<String>,
}

impl GearLoadout {
    #[must_use]
    pub fn combat_modifiers(&self) -> CombatModifiers {
        let mut result = CombatModifiers::default();
        for piece in [
            self.weapon.as_ref(),
            self.armor.as_ref(),
            self.boots.as_ref(),
            self.amulet.as_ref(),
            self.ring.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter(|piece| piece.durability > 0)
        {
            let modifiers = piece_modifiers(piece);
            result.player_dmg += modifiers.player_dmg;
            result.player_hit += modifiers.player_hit;
            result.player_hp_bonus += modifiers.player_hp_bonus;
            result.boss_hit_reduction += modifiers.boss_hit_reduction;
            result.crit_chance += modifiers.crit_chance;
            result.crit_bonus += modifiers.crit_bonus;
        }
        result
    }
}

fn piece_modifiers(piece: &GearPiece) -> CombatModifiers {
    if let Some(item_id) = piece.item_id.as_deref() {
        return unique_gear(item_id).map_or_else(CombatModifiers::default, |unique| {
            CombatModifiers {
                player_dmg: unique.player_dmg,
                player_hit: unique.player_hit,
                player_hp_bonus: unique.player_hp_bonus,
                boss_hit_reduction: unique.boss_hit_reduction,
                crit_chance: unique.crit_chance,
                crit_bonus: unique.crit_bonus,
            }
        });
    }
    tier_table(piece.slot)
        .and_then(|table| table.get(usize::from(piece.tier)))
        .map_or_else(CombatModifiers::default, |definition| CombatModifiers {
            player_dmg: definition.player_dmg,
            player_hit: definition.player_hit,
            player_hp_bonus: definition.player_hp_bonus,
            boss_hit_reduction: definition.boss_hit_reduction,
            crit_chance: definition.crit_chance,
            crit_bonus: definition.crit_bonus,
        })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CombatStats {
    pub player_hp: i32,
    pub boss_hp: i32,
    pub player_hit: f64,
    pub player_dmg: i32,
    pub boss_hit: f64,
    pub boss_dmg: i32,
    pub crit_chance: f64,
    pub crit_bonus: i32,
}

impl CombatStats {
    #[must_use]
    pub const fn cautious() -> Self {
        Self {
            player_hp: 5,
            boss_hp: 4,
            player_hit: 0.60,
            player_dmg: 1,
            boss_hit: 0.30,
            boss_dmg: 1,
            crit_chance: 0.0,
            crit_bonus: 0,
        }
    }

    #[must_use]
    pub const fn bold() -> Self {
        Self {
            player_hp: 3,
            boss_hp: 5,
            player_hit: 0.42,
            player_dmg: 2,
            boss_hit: 0.45,
            boss_dmg: 1,
            crit_chance: 0.15,
            crit_bonus: 1,
        }
    }

    #[must_use]
    pub const fn reckless() -> Self {
        Self {
            player_hp: 2,
            boss_hp: 6,
            player_hit: 0.18,
            player_dmg: 3,
            boss_hit: 0.60,
            boss_dmg: 1,
            crit_chance: 0.30,
            crit_bonus: 1,
        }
    }
}

#[must_use]
pub fn apply_gear_to_combat(base: CombatStats, loadout: &GearLoadout) -> CombatStats {
    let modifiers = loadout.combat_modifiers();
    CombatStats {
        player_hp: base.player_hp + modifiers.player_hp_bonus,
        boss_hp: base.boss_hp,
        player_hit: (base.player_hit + modifiers.player_hit)
            .clamp(PLAYER_HIT_FLOOR, PLAYER_HIT_CEILING),
        player_dmg: base.player_dmg + modifiers.player_dmg,
        boss_hit: (base.boss_hit - modifiers.boss_hit_reduction).max(0.05),
        boss_dmg: base.boss_dmg,
        crit_chance: base.crit_chance + modifiers.crit_chance,
        crit_bonus: base.crit_bonus + modifiers.crit_bonus,
    }
}

#[must_use]
pub const fn drop_tier_at_depth(depth: i64) -> Option<u8> {
    match depth {
        100 => Some(4),
        150 => Some(5),
        200 => Some(6),
        275 => Some(7),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Tunnel {
    pub depth: i64,
    pub max_depth: i64,
    pub prestige_level: i32,
    pub pickaxe_tier: u8,
    pub relic_trim_notice: bool,
    pub cavein_free_streak: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Artifact {
    pub id: i64,
    pub discord_id: i64,
    pub guild_id: i64,
    pub artifact_id: String,
    pub is_relic: bool,
    pub equipped: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    UniqueEquippedSlot,
    ForcedInsertFailure,
    MissingGear,
}

#[derive(Clone, Debug, Default)]
pub struct GearStore {
    next_gear_id: i64,
    next_artifact_id: i64,
    clock: i64,
    pieces: Vec<GearPiece>,
    tunnels: BTreeMap<(i64, i64), Tunnel>,
    balances: BTreeMap<(i64, i64), i64>,
    artifacts: Vec<Artifact>,
    pub bulk_repair_calls: usize,
    pub fail_next_shop_insert: bool,
}

impl GearStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_gear_id: 1,
            next_artifact_id: 1,
            clock: 1,
            ..Self::default()
        }
    }

    /// Rehydrate one player's aggregate from a repository snapshot while
    /// preserving database row identities for guarded write-back.
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "repository rehydration carries the complete persisted identity fence"
    )]
    pub fn from_persisted_player(
        discord_id: i64,
        guild_id: i64,
        tunnel: Tunnel,
        balance: i64,
        pieces: Vec<GearPiece>,
        artifacts: Vec<Artifact>,
        next_gear_id: i64,
        next_artifact_id: i64,
        clock: i64,
    ) -> Self {
        Self {
            next_gear_id,
            next_artifact_id,
            clock,
            pieces,
            tunnels: BTreeMap::from([((discord_id, guild_id), tunnel)]),
            balances: BTreeMap::from([((discord_id, guild_id), balance)]),
            artifacts,
            bulk_repair_calls: 0,
            fail_next_shop_insert: false,
        }
    }

    /// Clone the rows owned by one player for repository diff/commit.
    #[must_use]
    pub fn persisted_gear(&self, discord_id: i64, guild_id: i64) -> Vec<GearPiece> {
        self.pieces
            .iter()
            .filter(|piece| piece.discord_id == discord_id && piece.guild_id == guild_id)
            .cloned()
            .collect()
    }

    /// Clone the artifact rows owned by one player for repository diff/commit.
    #[must_use]
    pub fn persisted_artifacts(&self, discord_id: i64, guild_id: i64) -> Vec<Artifact> {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.discord_id == discord_id && artifact.guild_id == guild_id)
            .cloned()
            .collect()
    }

    pub fn create_tunnel(&mut self, discord_id: i64, guild_id: i64) -> i64 {
        self.tunnels.entry((discord_id, guild_id)).or_default();
        let starter = self.add_gear(
            discord_id,
            guild_id,
            GearSlot::Weapon,
            0,
            Some(GEAR_MAX_DURABILITY),
            "starter",
            None,
        );
        self.equip_gear(starter, discord_id, guild_id, GearSlot::Weapon)
            .expect("new tunnel has no conflicting weapon");
        starter
    }

    pub fn set_tunnel(&mut self, discord_id: i64, guild_id: i64, tunnel: Tunnel) {
        self.tunnels.insert((discord_id, guild_id), tunnel);
    }

    #[must_use]
    pub fn tunnel(&self, discord_id: i64, guild_id: i64) -> Option<&Tunnel> {
        self.tunnels.get(&(discord_id, guild_id))
    }

    pub fn tunnel_mut(&mut self, discord_id: i64, guild_id: i64) -> Option<&mut Tunnel> {
        self.tunnels.get_mut(&(discord_id, guild_id))
    }

    pub fn set_balance(&mut self, discord_id: i64, guild_id: i64, balance: i64) {
        self.balances.insert((discord_id, guild_id), balance);
    }

    #[must_use]
    pub fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
        self.balances
            .get(&(discord_id, guild_id))
            .copied()
            .unwrap_or(0)
    }

    pub fn add_balance(&mut self, discord_id: i64, guild_id: i64, delta: i64) {
        *self.balances.entry((discord_id, guild_id)).or_default() += delta;
    }

    pub fn try_debit(&mut self, discord_id: i64, guild_id: i64, amount: i64) -> bool {
        let balance = self.balances.entry((discord_id, guild_id)).or_default();
        if amount < 0 || *balance < amount {
            return false;
        }
        *balance -= amount;
        true
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "repository command mirrors one persisted dig_gear row"
    )]
    pub fn add_gear(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        slot: GearSlot,
        tier: u8,
        durability: Option<i32>,
        source: &str,
        item_id: Option<&str>,
    ) -> i64 {
        let id = self.next_gear_id;
        self.next_gear_id += 1;
        let acquired_at = self.clock;
        self.clock += 1;
        self.pieces.push(GearPiece {
            id,
            discord_id,
            guild_id,
            slot,
            tier,
            durability: durability.unwrap_or(GEAR_MAX_DURABILITY),
            equipped: false,
            acquired_at,
            source: source.to_owned(),
            item_id: item_id.map(str::to_owned),
        });
        id
    }

    #[must_use]
    pub fn gear_by_id(&self, gear_id: i64) -> Option<&GearPiece> {
        self.pieces.iter().find(|piece| piece.id == gear_id)
    }

    pub fn gear_by_id_mut(&mut self, gear_id: i64) -> Option<&mut GearPiece> {
        self.pieces.iter_mut().find(|piece| piece.id == gear_id)
    }

    #[must_use]
    pub fn gear(&self, discord_id: i64, guild_id: i64) -> Vec<&GearPiece> {
        let mut rows = self
            .pieces
            .iter()
            .filter(|piece| piece.discord_id == discord_id && piece.guild_id == guild_id)
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            left.slot
                .as_str()
                .cmp(right.slot.as_str())
                .then_with(|| right.tier.cmp(&left.tier))
                .then_with(|| right.acquired_at.cmp(&left.acquired_at))
        });
        rows
    }

    #[must_use]
    pub fn equipped_gear(&self, discord_id: i64, guild_id: i64) -> BTreeMap<GearSlot, &GearPiece> {
        self.pieces
            .iter()
            .filter(|piece| {
                piece.discord_id == discord_id && piece.guild_id == guild_id && piece.equipped
            })
            .map(|piece| (piece.slot, piece))
            .collect()
    }

    pub fn equip_gear(
        &mut self,
        gear_id: i64,
        discord_id: i64,
        guild_id: i64,
        slot: GearSlot,
    ) -> Result<(), StoreError> {
        if !self.pieces.iter().any(|piece| piece.id == gear_id) {
            return Err(StoreError::MissingGear);
        }
        for piece in &mut self.pieces {
            if piece.discord_id == discord_id && piece.guild_id == guild_id && piece.slot == slot {
                piece.equipped = piece.id == gear_id;
            }
        }
        Ok(())
    }

    pub fn manual_set_equipped(&mut self, gear_id: i64, equipped: bool) -> Result<(), StoreError> {
        let Some(target) = self.gear_by_id(gear_id).cloned() else {
            return Err(StoreError::MissingGear);
        };
        if equipped
            && self.pieces.iter().any(|piece| {
                piece.id != gear_id
                    && piece.discord_id == target.discord_id
                    && piece.guild_id == target.guild_id
                    && piece.slot == target.slot
                    && piece.equipped
            })
        {
            return Err(StoreError::UniqueEquippedSlot);
        }
        if let Some(piece) = self.gear_by_id_mut(gear_id) {
            piece.equipped = equipped;
        }
        Ok(())
    }

    pub fn unequip_gear(&mut self, gear_id: i64) {
        if let Some(piece) = self.gear_by_id_mut(gear_id) {
            piece.equipped = false;
        }
    }

    pub fn tick_gear_durability(&mut self, discord_id: i64, guild_id: i64) -> Vec<i64> {
        let mut broken = Vec::new();
        for piece in &mut self.pieces {
            if piece.discord_id == discord_id && piece.guild_id == guild_id && piece.equipped {
                if piece.durability == 1 {
                    broken.push(piece.id);
                }
                piece.durability = (piece.durability - 1).max(0);
            }
        }
        broken
    }

    pub fn tick_gear_durability_ids(&mut self, gear_ids: &[i64]) -> Vec<i64> {
        let selected = gear_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut broken = Vec::new();
        for piece in &mut self.pieces {
            if selected.contains(&piece.id) {
                if piece.durability == 1 {
                    broken.push(piece.id);
                }
                piece.durability = (piece.durability - 1).max(0);
            }
        }
        broken
    }

    pub fn repair_gear(&mut self, gear_id: i64, to_durability: i32) {
        if let Some(piece) = self.gear_by_id_mut(gear_id) {
            piece.durability = to_durability;
        }
    }

    pub fn repair_gear_bulk(&mut self, repairs: &[(i64, i32)]) -> usize {
        if repairs.is_empty() {
            return 0;
        }
        self.bulk_repair_calls += 1;
        let mut changed = 0;
        for &(gear_id, durability) in repairs {
            if let Some(piece) = self.gear_by_id_mut(gear_id) {
                piece.durability = durability;
                changed += 1;
            }
        }
        changed
    }

    pub fn atomic_purchase_gear(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        cost: i64,
        slot: GearSlot,
        tier: u8,
    ) -> Result<Option<i64>, StoreError> {
        if self.balance(discord_id, guild_id) < cost {
            return Ok(None);
        }
        let old_balance = self.balance(discord_id, guild_id);
        let _ = self.try_debit(discord_id, guild_id, cost);
        if self.fail_next_shop_insert {
            self.fail_next_shop_insert = false;
            self.set_balance(discord_id, guild_id, old_balance);
            return Err(StoreError::ForcedInsertFailure);
        }
        Ok(Some(self.add_gear(
            discord_id, guild_id, slot, tier, None, "shop", None,
        )))
    }

    pub fn add_artifact(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        artifact_id: &str,
        is_relic: bool,
    ) -> i64 {
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
        id
    }

    #[must_use]
    pub fn artifacts(&self, discord_id: i64, guild_id: i64) -> Vec<&Artifact> {
        self.artifacts
            .iter()
            .filter(|artifact| artifact.discord_id == discord_id && artifact.guild_id == guild_id)
            .collect()
    }

    pub fn set_artifact_equipped(&mut self, artifact_id: i64, equipped: bool) {
        if let Some(artifact) = self
            .artifacts
            .iter_mut()
            .find(|artifact| artifact.id == artifact_id)
        {
            artifact.equipped = equipped;
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActionResult {
    pub success: bool,
    pub error: Option<String>,
    pub gear_id: Option<i64>,
    pub slot: Option<GearSlot>,
    pub cost: i64,
    pub repaired: usize,
    pub cap: Option<i32>,
}

impl ActionResult {
    fn ok() -> Self {
        Self {
            success: true,
            ..Self::default()
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GearDrop {
    pub gear_id: i64,
    pub slot: GearSlot,
    pub tier: u8,
    pub name: &'static str,
}

pub trait GearRng {
    fn random(&mut self) -> f64;
    fn choose_slot(&mut self) -> GearSlot;
}

#[derive(Clone, Debug)]
pub struct LcgRng {
    state: u64,
}

impl LcgRng {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.state
    }
}

impl GearRng for LcgRng {
    fn random(&mut self) -> f64 {
        (self.next() >> 11) as f64 / ((1_u64 << 53) as f64)
    }

    fn choose_slot(&mut self) -> GearSlot {
        match self.next() % 4 {
            0 => GearSlot::Weapon,
            1 => GearSlot::Armor,
            2 => GearSlot::Boots,
            _ => GearSlot::Amulet,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GearService {
    pub store: GearStore,
}

impl GearService {
    #[must_use]
    pub fn fixture() -> Self {
        let mut store = GearStore::new();
        store.create_tunnel(111, 0);
        store.set_balance(111, 0, 5_000);
        if let Some(tunnel) = store.tunnel_mut(111, 0) {
            tunnel.depth = 100;
            tunnel.max_depth = 100;
            tunnel.prestige_level = 1;
        }
        Self { store }
    }

    pub fn maybe_drop_gear<R: GearRng>(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        at_boss: i64,
        rng: &mut R,
    ) -> Option<GearDrop> {
        let tier = drop_tier_at_depth(at_boss)?;
        if rng.random() >= GEAR_BOSS_DROP_RATE {
            return None;
        }
        let slot = rng.choose_slot();
        let definition = tier_table(slot)?[usize::from(tier)];
        let gear_id =
            self.store
                .add_gear(discord_id, guild_id, slot, tier, None, "boss_drop", None);
        Some(GearDrop {
            gear_id,
            slot,
            tier,
            name: definition.name,
        })
    }

    #[must_use]
    pub fn loadout(&self, discord_id: i64, guild_id: i64) -> GearLoadout {
        let equipped = self.store.equipped_gear(discord_id, guild_id);
        let relics = self
            .store
            .artifacts(discord_id, guild_id)
            .into_iter()
            .filter(|artifact| artifact.is_relic && artifact.equipped)
            .map(|artifact| artifact.artifact_id.clone())
            .collect();
        GearLoadout {
            weapon: equipped
                .get(&GearSlot::Weapon)
                .map(|piece| (*piece).clone()),
            armor: equipped.get(&GearSlot::Armor).map(|piece| (*piece).clone()),
            boots: equipped.get(&GearSlot::Boots).map(|piece| (*piece).clone()),
            amulet: equipped
                .get(&GearSlot::Amulet)
                .map(|piece| (*piece).clone()),
            ring: equipped.get(&GearSlot::Ring).map(|piece| (*piece).clone()),
            relics,
        }
    }

    pub fn equip_gear(&mut self, discord_id: i64, guild_id: i64, gear_id: i64) -> ActionResult {
        let Some(row) = self.store.gear_by_id(gear_id).cloned() else {
            return ActionResult::error("That gear piece doesn't exist.");
        };
        if row.discord_id != discord_id || row.guild_id != guild_id {
            return ActionResult::error("That gear piece doesn't belong to you.");
        }
        if row.durability <= 0 {
            return ActionResult::error("That piece is broken — repair it first.");
        }
        if row.equipped {
            return ActionResult::error("That piece is already equipped.");
        }
        if self
            .store
            .equip_gear(gear_id, discord_id, guild_id, row.slot)
            .is_err()
        {
            return ActionResult::error("That gear piece doesn't exist.");
        }
        ActionResult {
            gear_id: Some(gear_id),
            slot: Some(row.slot),
            ..ActionResult::ok()
        }
    }

    pub fn unequip_gear(&mut self, discord_id: i64, guild_id: i64, gear_id: i64) -> ActionResult {
        let Some(row) = self.store.gear_by_id(gear_id).cloned() else {
            return ActionResult::error("That gear piece doesn't exist.");
        };
        if row.discord_id != discord_id || row.guild_id != guild_id {
            return ActionResult::error("That gear piece doesn't belong to you.");
        }
        self.store.unequip_gear(gear_id);
        ActionResult {
            gear_id: Some(gear_id),
            slot: Some(row.slot),
            ..ActionResult::ok()
        }
    }

    #[must_use]
    pub fn compute_repair_cost(
        &self,
        slot: GearSlot,
        tier: u8,
        item_id: Option<&str>,
        durability: i32,
        max_durability: Option<i32>,
    ) -> i64 {
        repair_cost(slot, tier, item_id, durability, max_durability)
    }

    #[must_use]
    pub fn compute_repair_all_cost(&self, discord_id: i64, guild_id: i64) -> i64 {
        self.store
            .gear(discord_id, guild_id)
            .into_iter()
            .filter(|piece| piece.durability < piece.max_durability())
            .map(|piece| {
                repair_cost(
                    piece.slot,
                    piece.tier,
                    piece.item_id.as_deref(),
                    piece.durability,
                    Some(piece.max_durability()),
                )
            })
            .sum()
    }

    pub fn repair_gear(&mut self, discord_id: i64, guild_id: i64, gear_id: i64) -> ActionResult {
        let Some(piece) = self.store.gear_by_id(gear_id).cloned() else {
            return ActionResult::error("That gear piece doesn't exist.");
        };
        if piece.discord_id != discord_id || piece.guild_id != guild_id {
            return ActionResult::error("That gear piece doesn't belong to you.");
        }
        let maximum = piece.max_durability();
        if piece.durability >= maximum {
            return ActionResult::error("That piece is already at full durability.");
        }
        let cost = repair_cost(
            piece.slot,
            piece.tier,
            piece.item_id.as_deref(),
            piece.durability,
            Some(maximum),
        );
        if cost > 0 && !self.store.try_debit(discord_id, guild_id, cost) {
            return ActionResult::error(format!(
                "Repair costs {cost} JC; you only have {}.",
                self.store.balance(discord_id, guild_id)
            ));
        }
        self.store.repair_gear(gear_id, maximum);
        ActionResult {
            gear_id: Some(gear_id),
            cost,
            ..ActionResult::ok()
        }
    }

    pub fn repair_all_gear(&mut self, discord_id: i64, guild_id: i64) -> ActionResult {
        let repairs = self
            .store
            .gear(discord_id, guild_id)
            .into_iter()
            .filter(|piece| piece.durability < piece.max_durability())
            .map(|piece| (piece.id, piece.max_durability()))
            .collect::<Vec<_>>();
        if repairs.is_empty() {
            return ActionResult::error("Nothing to repair.");
        }
        let cost = self.compute_repair_all_cost(discord_id, guild_id);
        if cost > 0 && !self.store.try_debit(discord_id, guild_id, cost) {
            return ActionResult::error(format!(
                "Total repair costs {cost} JC; you only have {}.",
                self.store.balance(discord_id, guild_id)
            ));
        }
        self.store.repair_gear_bulk(&repairs);
        ActionResult {
            cost,
            repaired: repairs.len(),
            ..ActionResult::ok()
        }
    }

    pub fn buy_gear(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        slot: &str,
        tier: i32,
    ) -> Result<ActionResult, StoreError> {
        let Some(slot) = GearSlot::parse(slot) else {
            return Ok(ActionResult::error("Invalid gear slot."));
        };
        let Some(table) = tier_table(slot) else {
            return Ok(ActionResult::error("Invalid gear tier."));
        };
        let Ok(index) = usize::try_from(tier) else {
            return Ok(ActionResult::error("Invalid gear tier."));
        };
        let Some(definition) = table.get(index) else {
            return Ok(ActionResult::error("Invalid gear tier."));
        };
        let Some(tunnel) = self.store.tunnel(discord_id, guild_id) else {
            return Ok(ActionResult::error("You don't have a tunnel."));
        };
        let persistent_depth = tunnel.depth.max(tunnel.max_depth);
        if persistent_depth < definition.depth_required {
            return Ok(ActionResult::error(format!(
                "{} requires depth {}.",
                definition.name, definition.depth_required
            )));
        }
        if tunnel.prestige_level < definition.prestige_required {
            return Ok(ActionResult::error(format!(
                "{} requires prestige {}.",
                definition.name, definition.prestige_required
            )));
        }
        let Some(gear_id) = self.store.atomic_purchase_gear(
            discord_id,
            guild_id,
            definition.shop_price,
            slot,
            definition.tier,
        )?
        else {
            return Ok(ActionResult::error(format!(
                "{} costs {} JC; you have {}.",
                definition.name,
                definition.shop_price,
                self.store.balance(discord_id, guild_id)
            )));
        };
        Ok(ActionResult {
            gear_id: Some(gear_id),
            slot: Some(slot),
            cost: definition.shop_price,
            ..ActionResult::ok()
        })
    }

    #[must_use]
    pub const fn relic_slot_cap(prestige: i32) -> i32 {
        if prestige + RELIC_SLOTS_BASE < RELIC_SLOTS_MAX {
            prestige + RELIC_SLOTS_BASE
        } else {
            RELIC_SLOTS_MAX
        }
    }

    pub fn equip_relic(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        artifact_db_id: i64,
    ) -> ActionResult {
        let artifacts = self.store.artifacts(discord_id, guild_id);
        let Some(target) = artifacts
            .iter()
            .find(|artifact| artifact.id == artifact_db_id)
            .map(|artifact| (*artifact).clone())
        else {
            return ActionResult::error("That relic isn't in your inventory.");
        };
        if !target.is_relic {
            return ActionResult::error("That artifact isn't a relic and can't be equipped.");
        }
        if target.equipped {
            return ActionResult::error("That relic is already equipped.");
        }
        if artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == target.artifact_id && artifact.equipped)
        {
            return ActionResult::error("You already have that relic equipped.");
        }
        let prestige = self
            .store
            .tunnel(discord_id, guild_id)
            .map_or(0, |tunnel| tunnel.prestige_level);
        let cap = Self::relic_slot_cap(prestige);
        let equipped_count = artifacts
            .iter()
            .filter(|artifact| artifact.is_relic && artifact.equipped)
            .count();
        if i32::try_from(equipped_count).unwrap_or(i32::MAX) >= cap {
            return ActionResult::error(format!(
                "You've hit your relic cap ({cap}). Unequip one first."
            ));
        }
        self.store.set_artifact_equipped(artifact_db_id, true);
        ActionResult {
            cap: Some(cap),
            ..ActionResult::ok()
        }
    }

    pub fn unequip_relic(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        artifact_db_id: i64,
    ) -> ActionResult {
        if !self
            .store
            .artifacts(discord_id, guild_id)
            .iter()
            .any(|artifact| artifact.id == artifact_db_id)
        {
            return ActionResult::error("That relic isn't in your inventory.");
        }
        self.store.set_artifact_equipped(artifact_db_id, false);
        ActionResult::ok()
    }

    pub fn pop_relic_trim_notice(&mut self, discord_id: i64, guild_id: i64) -> bool {
        let Some(tunnel) = self.store.tunnel_mut(discord_id, guild_id) else {
            return false;
        };
        if !tunnel.relic_trim_notice {
            return false;
        }
        tunnel.relic_trim_notice = false;
        true
    }

    pub fn apply_dig_outcome(&mut self, discord_id: i64, guild_id: i64, cave_in: bool) {
        if let Some(tunnel) = self.store.tunnel_mut(discord_id, guild_id) {
            tunnel.cavein_free_streak = if cave_in {
                0
            } else {
                tunnel.cavein_free_streak + 1
            };
        }
    }

    #[must_use]
    pub fn active_pickaxe_tier(&self, discord_id: i64, guild_id: i64) -> u8 {
        let equipped = self.store.equipped_gear(discord_id, guild_id);
        if let Some(weapon) = equipped.get(&GearSlot::Weapon) {
            return if weapon.durability > 0 {
                weapon.tier
            } else {
                0
            };
        }
        self.store
            .tunnel(discord_id, guild_id)
            .map_or(0, |tunnel| tunnel.pickaxe_tier)
    }

    #[must_use]
    pub fn active_pickaxe_data(&self, discord_id: i64, guild_id: i64) -> Option<PickaxeData> {
        let equipped = self.store.equipped_gear(discord_id, guild_id);
        if let Some(weapon) = equipped.get(&GearSlot::Weapon) {
            if weapon.durability <= 0 {
                return None;
            }
            if let Some(unique) = weapon.item_id.as_deref().and_then(unique_gear) {
                return Some(PickaxeData {
                    name: unique.name,
                    advance_bonus: unique.advance_bonus,
                    cave_in_reduction: unique.cave_in_reduction,
                    loot_bonus: unique.loot_bonus,
                });
            }
            return WEAPON_TIERS
                .get(usize::from(weapon.tier))
                .map(PickaxeData::from);
        }
        self.store
            .tunnel(discord_id, guild_id)
            .and_then(|tunnel| WEAPON_TIERS.get(usize::from(tunnel.pickaxe_tier)))
            .map(PickaxeData::from)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickaxeData {
    pub name: &'static str,
    pub advance_bonus: i32,
    pub cave_in_reduction: f64,
    pub loot_bonus: i32,
}

impl From<&GearDefinition> for PickaxeData {
    fn from(definition: &GearDefinition) -> Self {
        Self {
            name: definition.name,
            advance_bonus: definition.advance_bonus,
            cave_in_reduction: definition.cave_in_reduction,
            loot_bonus: definition.loot_bonus,
        }
    }
}

fn python_round(value: f64) -> i64 {
    let floor = value.floor();
    let fraction = value - floor;
    if (fraction - 0.5).abs() < f64::EPSILON {
        let floor_i64 = floor as i64;
        if floor_i64 % 2 == 0 {
            floor_i64
        } else {
            floor_i64 + 1
        }
    } else {
        value.round() as i64
    }
}

#[must_use]
pub fn repair_cost(
    slot: GearSlot,
    tier: u8,
    item_id: Option<&str>,
    durability: i32,
    max_durability: Option<i32>,
) -> i64 {
    let (repair_value, authored_max) = if let Some(item_id) = item_id {
        let Some(unique) = unique_gear(item_id) else {
            return 0;
        };
        (unique.repair_value, unique.max_durability)
    } else {
        let Some(definition) = tier_table(slot).and_then(|table| table.get(usize::from(tier)))
        else {
            return 0;
        };
        (definition.shop_price, GEAR_MAX_DURABILITY)
    };
    let full_cost = python_round(repair_value as f64 * GEAR_REPAIR_COST_PCT);
    let maximum = max_durability.unwrap_or(authored_max).max(1);
    let missing = (maximum - durability.min(maximum)).max(0);
    if missing == 0 || full_cost == 0 {
        return 0;
    }
    ((full_cost * i64::from(missing) + i64::from(maximum) - 1) / i64::from(maximum)).max(1)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GearEmbed {
    pub fields: Vec<EmbedField>,
    pub footer: String,
}

#[must_use]
pub fn build_gear_embed(
    loadout: &GearLoadout,
    relic_cap: i32,
    inventory_count: usize,
    damaged_count: usize,
) -> GearEmbed {
    let mut fields = Vec::new();
    for (name, piece) in [
        ("Weapon", loadout.weapon.as_ref()),
        ("Armor", loadout.armor.as_ref()),
        ("Boots", loadout.boots.as_ref()),
        ("Amulet", loadout.amulet.as_ref()),
        ("Ring", loadout.ring.as_ref()),
    ] {
        let value = piece.map_or_else(
            || "_— Empty —_".to_owned(),
            |piece| {
                let mut line = format!(
                    "**{}** ({}/{})",
                    piece.name().unwrap_or("Unknown"),
                    piece.durability,
                    piece.max_durability()
                );
                if piece.durability <= 0 {
                    line.push_str("\n**BROKEN — effects disabled until repaired.**");
                }
                if let Some(effect) = piece.effect_summary() {
                    line.push('\n');
                    line.push_str(effect);
                }
                line
            },
        );
        fields.push(EmbedField {
            name: name.to_owned(),
            value,
        });
    }
    let relic_count = loadout.relics.len();
    let mut relic_value = if loadout.relics.is_empty() {
        "_None equipped_".to_owned()
    } else {
        loadout
            .relics
            .iter()
            .map(|id| format!("• {}", format_relic_label(id, true)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if i32::try_from(relic_count).unwrap_or(i32::MAX) > relic_cap {
        relic_value.push_str(&format!(
            "\n⚠ Over cap — unequip {}.",
            i32::try_from(relic_count).unwrap_or(i32::MAX) - relic_cap
        ));
    }
    fields.push(EmbedField {
        name: format!("Relics ({relic_count}/{relic_cap} equipped)"),
        value: relic_value,
    });
    let mut footer = format!("{inventory_count} owned");
    if damaged_count > 0 {
        footer.push_str(&format!(" • {damaged_count} damaged"));
    }
    footer.push_str(" • Buy gear via /dig shop");
    GearEmbed { fields, footer }
}

const PINNACLE_STATS: [(&str, &str, &str); 15] = [
    ("hp_plus_1", "Tougher skin", "Scales"),
    ("hit_plus_002", "Steadier hands", "Precision"),
    ("dmg_plus_per_100", "Stronger with depth", "Depths"),
    ("boss_hit_minus", "Bosses miss more often", "Feints"),
    ("boss_hp_minus_10", "Bosses arrive weakened", "Withering"),
    ("boss_payout_5", "Bosses pay better", "Tribute"),
    ("jc_plus_5", "Richer veins", "Riches"),
    ("cave_in_minus_5", "Steadier ceilings", "Bedrock"),
    ("lum_refill_2", "Brighter mornings", "Dawn"),
    ("durability_minus", "Gear lasts longer", "Patience"),
    ("inventory_plus_1", "Roomier pack", "Satchels"),
    (
        "streak_immunity",
        "Streak persists, once per delve",
        "Persistence",
    ),
    ("extra_relic_slot", "Another relic finds room", "Communion"),
    ("scout_free", "Scouting comes cheap", "Scouting"),
    ("cheer_buff", "Cheers ring louder", "Acclaim"),
];

pub const PINNACLE_RELIC_SUFFIX_POOL: [&str; 12] = [
    "Echoes",
    "Hunger",
    "Patience",
    "Ruin",
    "Bloom",
    "Silence",
    "Endings",
    "First Light",
    "Last Breath",
    "Hollow",
    "Persistence",
    "Forgotten Things",
];

pub const QUEST_RELIC_SUFFIXES: [&str; 2] = ["Long Silence", "Scheming Hand"];

fn pinnacle_stat(stat_id: &str) -> Option<(&'static str, &'static str)> {
    PINNACLE_STATS
        .iter()
        .find(|(id, _, _)| *id == stat_id)
        .map(|(_, label, title)| (*label, *title))
}

#[must_use]
pub fn format_relic_label(artifact_id: &str, with_stats: bool) -> String {
    if artifact_id.is_empty() {
        return "?".to_owned();
    }
    let Some(rest) = artifact_id.strip_prefix("pinnacle:") else {
        return match artifact_id {
            "frozen_clock" => "Frozen Clock".to_owned(),
            "mole_claws" => "Mole Claws".to_owned(),
            "magma_heart" => "Magma Heart".to_owned(),
            "crystal_compass" => "Crystal Compass".to_owned(),
            "echo_stone" => "Echo Stone".to_owned(),
            "spore_cloak" => "Spore Cloak".to_owned(),
            "root_network" => "Root Network".to_owned(),
            "prospectors_streak" => "Prospector's Streak".to_owned(),
            _ => artifact_id.to_owned(),
        };
    };
    let parts = rest.split(':').collect::<Vec<_>>();
    if parts.len() < 2 {
        return artifact_id.to_owned();
    }
    let base = parts[0];
    let stored_suffix = parts[1];
    let recognized = parts[2..]
        .iter()
        .filter_map(|id| pinnacle_stat(id).map(|(label, title)| (*id, label, title)))
        .collect::<Vec<_>>();
    let legacy_suffix =
        stored_suffix.is_empty() || PINNACLE_RELIC_SUFFIX_POOL.contains(&stored_suffix);
    let suffix = if !recognized.is_empty() && legacy_suffix {
        let mut titles = recognized
            .iter()
            .map(|(_, _, title)| *title)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        titles.sort_by_key(|title| title.to_lowercase());
        titles.join(" and ")
    } else {
        stored_suffix.to_owned()
    };
    let mut label = if suffix.is_empty() {
        base.to_owned()
    } else {
        format!("{base} of {suffix}")
    };
    if with_stats && !recognized.is_empty() {
        label.push_str(&format!(
            " ({})",
            recognized
                .iter()
                .map(|(_, stat_label, _)| *stat_label)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    label
}

#[cfg(test)]
mod tests;
