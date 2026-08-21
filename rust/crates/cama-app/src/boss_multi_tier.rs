//! Additive, transport-neutral parity policy for multi-tier regular bosses.
//!
//! Python remains the production, schema, and Discord runtime authority.  This
//! module deliberately owns no SQLite connection and no gateway types.  It
//! models the behavior asserted by `tests/test_boss_multi_tier.py` behind
//! explicit repository, entropy, clock, economy-event, bankruptcy,
//! gear/repair, and outcome-sink ports.  The in-memory adapters are test
//! doubles, not claims of a production cutover.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::LazyLock;

use cama_domain::dig_economy::{
    WagerMultiplier, WinChance, effective_wager_multiplier, scale_positive_dig_jc,
    settled_wager_multiplier,
};
use serde::Deserialize;
use thiserror::Error;

pub use crate::boss_duel::RiskTier;
use crate::boss_duel::{
    PHASE_TRANSITION_EVENTS, PetAssist, boss_base_reward, boss_payout_multiplier, transition_event,
};
#[cfg(test)]
use crate::dig_bosses::BOSSES;
pub use crate::dig_bosses::{
    BOSS_BOUNDARIES, BossDefinition, BossProgress, BossProgressEntry, BossProgressValue, BossStatus,
};
use crate::dig_bosses::{
    CombatStats, DuelOddsInput, PINNACLE_DEPTH, boss_by_id, boss_pool_for_tier,
    duel_win_probability_with_damage_bonus, regular_phase, scale_boss_stats,
};
use crate::dig_relic_rework::{
    RelicEntropy, RelicSet, ShiftingIdolStats, apply_shifting_idol_stats,
};

pub const PHASE_TWO_MIN_PRESTIGE: i32 = 2;
pub const PHASE_THREE_MIN_PRESTIGE: i32 = 5;
pub const PHASE_THREE_MIN_TIER: i32 = 100;
pub const FREE_FIGHT_ACCURACY_MULTIPLIER: f64 = 0.70;
pub const PLAYER_HIT_FLOOR: f64 = 0.05;
pub const WAGERED_PLAYER_HIT_FLOOR: f64 = 0.10;
pub const PLAYER_HIT_CEILING: f64 = 0.90;
pub const BOSS_ROUND_CAP: u8 = 20;
pub const BOSS_HP_REGEN_PER_DAY: i32 = 1;
pub const BOSS_LOSS_KNOCKBACK_MIN: i32 = 11;
pub const BOSS_LOSS_KNOCKBACK_MAX: i32 = 20;
pub const BOSS_LOSS_EXTRA_COOLDOWN_SECONDS: i64 = 3_600;
pub const ECHO_WINDOW_SECONDS: i64 = 86_400;
pub const ECHO_WAGER_PROFIT_MULTIPLIER: f64 = 0.70;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct GuildId(pub i64);

impl GuildId {
    #[must_use]
    pub const fn from_optional(value: Option<i64>) -> Self {
        match value {
            Some(value) => Self(value),
            None => Self(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PlayerKey {
    pub player_id: i64,
    pub guild_id: GuildId,
}

impl PlayerKey {
    #[must_use]
    pub const fn new(player_id: i64, guild_id: Option<i64>) -> Self {
        Self {
            player_id,
            guild_id: GuildId::from_optional(guild_id),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CurseKind {
    HalveNextWager,
    NoScoutNextDig,
    DrainNextReward,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StingerCurseState {
    pub active: BTreeSet<CurseKind>,
    pub boss_id: Option<String>,
}

impl StingerCurseState {
    #[must_use]
    pub fn contains(&self, curse: CurseKind) -> bool {
        self.active.contains(&curse)
    }

    pub fn insert(&mut self, curse: CurseKind, boss_id: &str) {
        self.active.insert(curse);
        self.boss_id = Some(boss_id.to_owned());
    }

    pub fn consume(&mut self, curse: CurseKind) -> bool {
        let consumed = self.active.remove(&curse);
        if self.active.is_empty() {
            self.boss_id = None;
        }
        consumed
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StingerDefinition {
    pub id: &'static str,
    pub extra_knockback: i32,
    pub extended_cooldown_seconds: i64,
    pub cursed_status: Option<CurseKind>,
    pub flavor_on_loss: &'static str,
}

macro_rules! stinger {
    ($id:literal, $knockback:literal, $cooldown:literal, $curse:expr, $flavor:literal) => {
        StingerDefinition {
            id: $id,
            extra_knockback: $knockback,
            extended_cooldown_seconds: $cooldown,
            cursed_status: $curse,
            flavor_on_loss: $flavor,
        }
    };
}

pub const STINGERS: [StingerDefinition; 33] = [
    stinger!(
        "grothak_crumble",
        4,
        0,
        None,
        "Grothak slams a boulder down behind you as you flee."
    ),
    stinger!(
        "pudge_drag",
        8,
        0,
        None,
        "The Butcher drags you back toward his pile of meat."
    ),
    stinger!(
        "ogre_blast",
        0,
        900,
        None,
        "Your ears ring for hours after the twin-skulled bellow."
    ),
    stinger!(
        "crystalia_shard",
        0,
        0,
        Some(CurseKind::DrainNextReward),
        "A crystal shard lodges under your skin — it pulses dully."
    ),
    stinger!(
        "cm_freeze",
        0,
        0,
        Some(CurseKind::HalveNextWager),
        "You thaw out slowly. Your nerve is shot."
    ),
    stinger!(
        "tusk_kick",
        10,
        0,
        None,
        "The warlord's tusked kick launches you a long way back."
    ),
    stinger!(
        "magmus_burn",
        0,
        1_350,
        None,
        "Your gear smolders long after you escape the heat."
    ),
    stinger!(
        "lina_scorch",
        0,
        900,
        Some(CurseKind::DrainNextReward),
        "The scorchwitch's fire leaves scars that ache every payday."
    ),
    stinger!(
        "doom_brand",
        8,
        0,
        Some(CurseKind::HalveNextWager),
        "The deathbringer's brand refuses to fade. You dig scared."
    ),
    stinger!(
        "void_collapse",
        12,
        0,
        None,
        "The Void Warden folds the tunnel behind you."
    ),
    stinger!(
        "spectre_haunting",
        0,
        0,
        Some(CurseKind::NoScoutNextDig),
        "A spectral copy of yourself walks behind you now. Nothing feels certain."
    ),
    stinger!(
        "void_spirit_exile",
        15,
        450,
        None,
        "You're dimensionally mispunched and come out several floors up."
    ),
    stinger!(
        "sporeling_rot",
        0,
        1_800,
        None,
        "Spores colonize your lungs. You cough for a long while."
    ),
    stinger!(
        "treant_entangle",
        4,
        900,
        None,
        "Roots bind your boots. Hard to pick up the pace."
    ),
    stinger!(
        "broodmother_webbing",
        0,
        0,
        Some(CurseKind::HalveNextWager),
        "You rip out of the web, leaving too much coin behind."
    ),
    stinger!(
        "aegis_reversal",
        4,
        900,
        None,
        "The aegis spends its return against you and throws you back up the shaft."
    ),
    stinger!(
        "cairn_aftershock",
        6,
        0,
        None,
        "The General's stomp sends falling stone after you, carrying you farther up the shaft."
    ),
    stinger!(
        "xalatath_unraveling",
        0,
        0,
        Some(CurseKind::NoScoutNextDig),
        "Something was said. You can't unhear it. The path won't show itself."
    ),
    stinger!(
        "chronofrost_stillness",
        0,
        2_700,
        None,
        "Time stuck to you on the way out. You move slow for a while."
    ),
    stinger!(
        "void_chrono",
        0,
        0,
        Some(CurseKind::NoScoutNextDig),
        "The timeless one rewinds the last few minutes — you forget the way down."
    ),
    stinger!(
        "weaver_unmake",
        0,
        0,
        Some(CurseKind::DrainNextReward),
        "The skitterwing pulled a thread from your timeline. Your next prize is thinner."
    ),
    stinger!(
        "heartspire_doubt",
        0,
        0,
        Some(CurseKind::HalveNextWager),
        "The Heartspire leaves a bad draw in your hands. You wager smaller next time."
    ),
    stinger!(
        "croupier_collection",
        0,
        1_200,
        None,
        "Brass collectors lock your gear in a cage until the hourglass empties."
    ),
    stinger!(
        "lilith_hemorrhage",
        0,
        1_800,
        Some(CurseKind::DrainNextReward),
        "The bleeding doesn't stop on the way back. The next descent is slower for it."
    ),
    stinger!(
        "nameless_erase",
        0,
        0,
        Some(CurseKind::NoScoutNextDig),
        "You come out without a clear memory of the descent."
    ),
    stinger!(
        "oracle_fate",
        0,
        0,
        Some(CurseKind::HalveNextWager),
        "The blindfolded seer showed you a bad future. Your hand is shaking."
    ),
    stinger!(
        "terrorblade_sundering",
        8,
        900,
        Some(CurseKind::DrainNextReward),
        "The sundered prince swaps something of yours for something of his. You don't want to know what."
    ),
    stinger!(
        "emberwright_slag",
        0,
        1_800,
        None,
        "Molten slag hardens across your gear. The next descent waits for it to cool."
    ),
    stinger!(
        "saltveil_pressgang",
        10,
        900,
        None,
        "The Commodore's drowned crew hauls you even farther back to bail the buried ship."
    ),
    stinger!(
        "underlord_atrophy",
        12,
        900,
        None,
        "The pit pulled. You came out a long way back, with less to show for it."
    ),
    stinger!(
        "blightcoil_venom",
        0,
        1_800,
        Some(CurseKind::DrainNextReward),
        "The spores settle into your lungs. Your next haul tastes thin and green."
    ),
    stinger!(
        "rimebound_soulchill",
        0,
        2_700,
        Some(CurseKind::HalveNextWager),
        "The cold gets into your hands. You won't trust them with a real bet for a while."
    ),
    stinger!(
        "spineback_rend",
        16,
        0,
        None,
        "A spine the size of a door hurls you back up the shaft. You land far from where you fell."
    ),
];

#[must_use]
pub fn stinger_by_id(stinger_id: &str) -> Option<&'static StingerDefinition> {
    STINGERS.iter().find(|stinger| stinger.id == stinger_id)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Combatant {
    Player,
    Boss,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MechanicStatusEffect {
    Burn,
    Silence,
    Bleed,
    Frostbite,
    Reveal,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct OutcomeRoll {
    pub probability: f64,
    pub player_hp_delta: i32,
    pub boss_hp_delta: i32,
    pub skip_next_round_for: Option<Combatant>,
    pub status_effect: Option<MechanicStatusEffect>,
    pub narrative: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MechanicOption {
    pub label: String,
    pub flavor: String,
    pub outcome_rolls: Vec<OutcomeRoll>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct MechanicDefinition {
    pub id: String,
    pub archetype: String,
    pub trigger_round: u8,
    pub prompt_title: String,
    pub prompt_description: String,
    pub options: Vec<MechanicOption>,
    #[serde(rename = "safe_option_idx")]
    pub safe_option_index: usize,
}

static MECHANIC_CATALOG: LazyLock<BTreeMap<String, MechanicDefinition>> = LazyLock::new(|| {
    let catalog: BTreeMap<String, MechanicDefinition> =
        serde_json::from_str(include_str!("../data/boss_mechanics.json"))
            .expect("the bundled boss mechanic catalog must be valid JSON");
    validate_mechanic_catalog(&catalog);
    catalog
});

fn validate_mechanic_catalog(catalog: &BTreeMap<String, MechanicDefinition>) {
    for (key, mechanic) in catalog {
        assert_eq!(key, &mechanic.id, "boss mechanic key must match its id");
        assert_eq!(
            mechanic.options.len(),
            3,
            "boss mechanic {key} must expose exactly three options"
        );
        assert!(
            mechanic.safe_option_index < mechanic.options.len(),
            "boss mechanic {key} has an invalid safe option"
        );
        for option in &mechanic.options {
            assert!(
                !option.outcome_rolls.is_empty(),
                "boss mechanic {key} has an option without outcomes"
            );
            let probability = option
                .outcome_rolls
                .iter()
                .map(|outcome| outcome.probability)
                .sum::<f64>();
            assert!(
                (probability - 1.0).abs() < 1e-9,
                "boss mechanic {key} option probabilities must sum to one"
            );
        }
    }
}

#[must_use]
pub fn mechanic_by_id(mechanic_id: &str) -> Option<MechanicDefinition> {
    MECHANIC_CATALOG.get(mechanic_id).cloned()
}

/// Regular boss fights preserve one opening exchange and then surface the
/// rolled mechanic before round two. Pinnacle phases retain their separate
/// authored-timing engine and do not call this helper.
const fn regular_mechanic_trigger_round(_mechanic: &MechanicDefinition) -> u8 {
    2
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptOption {
    pub option_index: usize,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingPrompt {
    pub mechanic_id: String,
    pub prompt_title: String,
    pub prompt_description: String,
    pub options: Vec<PromptOption>,
    pub safe_option_index: usize,
}

impl From<&MechanicDefinition> for PendingPrompt {
    fn from(mechanic: &MechanicDefinition) -> Self {
        Self {
            mechanic_id: mechanic.id.clone(),
            prompt_title: mechanic.prompt_title.clone(),
            prompt_description: mechanic.prompt_description.clone(),
            options: mechanic
                .options
                .iter()
                .enumerate()
                .map(|(option_index, option)| PromptOption {
                    option_index,
                    label: option.label.clone(),
                })
                .collect(),
            safe_option_index: mechanic.safe_option_index,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum GearSlot {
    Weapon,
    Armor,
    Boots,
    Amulet,
    Ring,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GearRecord {
    pub id: i64,
    pub slot: GearSlot,
    pub durability: i32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GearSnapshot {
    pub gear_ids: Vec<i64>,
    pub armor_id: Option<i64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GearWearResult {
    pub broken_ids: Vec<i64>,
}

pub trait GearRepairPort {
    fn snapshot(&self, key: PlayerKey) -> GearSnapshot;
    fn repair_bill(&self) -> i64;
    fn tick_resolved_fight(
        &mut self,
        key: PlayerKey,
        snapshot: &GearSnapshot,
        won: bool,
    ) -> GearWearResult;
}

/// Per-attempt combat inputs loaded outside the pure boss catalog. The
/// production adapter composes gear, mana, active cheers, luminosity, relics,
/// and pet state once; tests can keep the neutral default.
#[derive(Clone, Debug, PartialEq)]
pub struct BossCombatPreparation {
    pub base_combat: CombatState,
    pub player_hit_offset: f64,
    pub boss_damage_delta: i32,
    pub boss_hp_multiplier: f64,
    pub player_damage_multiplier: f64,
    pub player_damage_after_phase_delta: i32,
    pub damage_variance_modifier: f64,
    pub boss_no_crit_against: bool,
    pub shifting_idol: bool,
    pub pet_assist: Option<PetAssist>,
    pub boss_preparation: Option<BossPreparationState>,
}

impl BossCombatPreparation {
    #[must_use]
    pub fn neutral(base_combat: CombatState) -> Self {
        Self {
            base_combat,
            player_hit_offset: 0.0,
            boss_damage_delta: 0,
            boss_hp_multiplier: 1.0,
            player_damage_multiplier: 1.0,
            player_damage_after_phase_delta: 0,
            damage_variance_modifier: 0.0,
            boss_no_crit_against: false,
            shifting_idol: false,
            pet_assist: None,
            boss_preparation: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BossCombatPreparationRequest<'a> {
    pub key: PlayerKey,
    pub tunnel: &'a TunnelState,
    pub boss: &'a BossDefinition,
    pub boundary: i32,
    pub risk_tier: RiskTier,
    pub wager: i64,
    pub echo_applied: bool,
    pub now: i64,
}

pub trait BossCombatPreparationPort {
    fn prepare(
        &self,
        request: BossCombatPreparationRequest<'_>,
        base_combat: CombatState,
    ) -> BossCombatPreparation;

    fn take_error(&self) -> Option<String> {
        None
    }
}

/// Claims any queued one-attempt preparation after command validation but
/// before boss locking/stat construction. Production persistence must update
/// boss progress and consume the exact inventory row in one transaction.
pub trait BossPreparationActivationPort {
    fn activate(&self, key: PlayerKey, boundary: i32) -> Result<(), RepositoryError>;

    fn take_error(&self) -> Option<String> {
        None
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoBossPreparationActivation;

impl BossPreparationActivationPort for NoBossPreparationActivation {
    fn activate(&self, _key: PlayerKey, _boundary: i32) -> Result<(), RepositoryError> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NeutralBossCombatPreparation;

impl BossCombatPreparationPort for NeutralBossCombatPreparation {
    fn prepare(
        &self,
        _request: BossCombatPreparationRequest<'_>,
        base_combat: CombatState,
    ) -> BossCombatPreparation {
        BossCombatPreparation::neutral(base_combat)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InMemoryGearRepair {
    pieces: BTreeMap<(PlayerKey, i64), GearRecord>,
    equipped: BTreeMap<(PlayerKey, GearSlot), i64>,
    next_id: i64,
    repair_bill: i64,
}

impl Default for InMemoryGearRepair {
    fn default() -> Self {
        Self {
            pieces: BTreeMap::new(),
            equipped: BTreeMap::new(),
            next_id: 0,
            repair_bill: 8,
        }
    }
}

impl InMemoryGearRepair {
    pub fn add_piece(&mut self, key: PlayerKey, slot: GearSlot, durability: i32) -> i64 {
        self.next_id += 1;
        let id = self.next_id;
        self.pieces.insert(
            (key, id),
            GearRecord {
                id,
                slot,
                durability,
            },
        );
        id
    }

    pub fn equip(&mut self, key: PlayerKey, gear_id: i64) -> Result<(), GearPortError> {
        let piece = self
            .pieces
            .get(&(key, gear_id))
            .ok_or(GearPortError::MissingPiece(gear_id))?;
        self.equipped.insert((key, piece.slot), gear_id);
        Ok(())
    }

    #[must_use]
    pub fn piece(&self, key: PlayerKey, gear_id: i64) -> Option<&GearRecord> {
        self.pieces.get(&(key, gear_id))
    }

    pub fn set_repair_bill(&mut self, amount: i64) {
        self.repair_bill = amount.max(0);
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum GearPortError {
    #[error("gear piece {0} does not exist")]
    MissingPiece(i64),
}

impl GearRepairPort for InMemoryGearRepair {
    fn snapshot(&self, key: PlayerKey) -> GearSnapshot {
        let mut pairs: Vec<(GearSlot, i64)> = self
            .equipped
            .iter()
            .filter_map(|((candidate, slot), gear_id)| {
                (*candidate == key).then_some((*slot, *gear_id))
            })
            .collect();
        pairs.sort_by_key(|(slot, _)| *slot);
        GearSnapshot {
            gear_ids: pairs.iter().map(|(_, gear_id)| *gear_id).collect(),
            armor_id: pairs
                .iter()
                .find_map(|(slot, gear_id)| (*slot == GearSlot::Armor).then_some(*gear_id)),
        }
    }

    fn repair_bill(&self) -> i64 {
        self.repair_bill
    }

    fn tick_resolved_fight(
        &mut self,
        key: PlayerKey,
        snapshot: &GearSnapshot,
        won: bool,
    ) -> GearWearResult {
        let mut broken_ids = Vec::new();
        for gear_id in &snapshot.gear_ids {
            if let Some(piece) = self.pieces.get_mut(&(key, *gear_id)) {
                let was_usable = piece.durability > 0;
                piece.durability = (piece.durability - 1).max(0);
                if was_usable && piece.durability == 0 {
                    broken_ids.push(*gear_id);
                }
            }
        }
        if !won
            && let Some(armor_id) = snapshot.armor_id
            && let Some(piece) = self.pieces.get_mut(&(key, armor_id))
        {
            let was_usable = piece.durability > 0;
            piece.durability = (piece.durability - 1).max(0);
            if was_usable && piece.durability == 0 && !broken_ids.contains(&armor_id) {
                broken_ids.push(armor_id);
            }
        }
        GearWearResult { broken_ids }
    }
}

pub trait EntropyPort {
    fn next_unit(&mut self) -> f64;
    fn choose_index(&mut self, upper_bound: usize) -> usize;
    fn inclusive_i32(&mut self, minimum: i32, maximum: i32) -> i32;
}

struct ShiftingIdolEntropy<'a, E>(&'a mut E);

impl<E: EntropyPort> RelicEntropy for ShiftingIdolEntropy<'_, E> {
    fn unit(&mut self) -> f64 {
        match self.0.choose_index(3) {
            0 => 0.0,
            1 => 0.5,
            _ => 0.99,
        }
    }
}

/// Apply the shared Shifting Idol policy while preserving the boss engine's
/// existing single `choose_index(3)` entropy draw.
pub(crate) fn apply_shifting_idol_attempt_stats(
    enabled: bool,
    player_hp: i32,
    player_hit: f64,
    critical_chance: f64,
    entropy: &mut impl EntropyPort,
) -> ShiftingIdolStats {
    let relics = if enabled {
        RelicSet::new(["shifting_idol"])
    } else {
        RelicSet::default()
    };
    apply_shifting_idol_stats(
        &relics,
        i64::from(player_hp),
        player_hit,
        critical_chance,
        &mut ShiftingIdolEntropy(entropy),
    )
}

#[derive(Clone, Debug, Default)]
pub struct SequenceEntropy {
    units: VecDeque<f64>,
    choices: VecDeque<usize>,
    integers: VecDeque<i32>,
    fallback_unit: f64,
}

impl SequenceEntropy {
    #[must_use]
    pub fn new(units: Vec<f64>, choices: Vec<usize>, integers: Vec<i32>) -> Self {
        Self {
            units: units.into(),
            choices: choices.into(),
            integers: integers.into(),
            fallback_unit: 0.99,
        }
    }

    #[must_use]
    pub fn constant(unit: f64) -> Self {
        Self {
            fallback_unit: unit,
            ..Self::default()
        }
    }
}

impl EntropyPort for SequenceEntropy {
    fn next_unit(&mut self) -> f64 {
        self.units.pop_front().unwrap_or(self.fallback_unit)
    }

    fn choose_index(&mut self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            return 0;
        }
        self.choices.pop_front().unwrap_or(0) % upper_bound
    }

    fn inclusive_i32(&mut self, minimum: i32, maximum: i32) -> i32 {
        self.integers
            .pop_front()
            .unwrap_or(minimum)
            .clamp(minimum, maximum)
    }
}

pub trait ClockPort {
    fn now(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FixedClock {
    pub unix_seconds: i64,
}

impl ClockPort for FixedClock {
    fn now(&self) -> i64 {
        self.unix_seconds
    }
}

pub trait EconomyEventPort {
    fn adjust_reward(&mut self, guild_id: GuildId, amount: i64) -> i64;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoEconomyEvent;

impl EconomyEventPort for NoEconomyEvent {
    fn adjust_reward(&mut self, _guild_id: GuildId, amount: i64) -> i64 {
        amount
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiplyingEconomyEvent {
    pub multiplier: i64,
    pub calls: Vec<(GuildId, i64)>,
}

impl EconomyEventPort for MultiplyingEconomyEvent {
    fn adjust_reward(&mut self, guild_id: GuildId, amount: i64) -> i64 {
        self.calls.push((guild_id, amount));
        amount.saturating_mul(self.multiplier)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BankruptcyAdjustment {
    pub penalized: i64,
    pub penalty_applied: i64,
    /// Profit-only vanity deduction computed from the same gross winnings.
    pub vanity_tax: i64,
    /// Profit-only low-priority deduction, computed from that same basis so
    /// the two taxes stack additively rather than compounding.
    pub low_priority_tax: i64,
}

pub trait BankruptcyPort {
    fn apply_penalty(&mut self, key: PlayerKey, positive_winnings: i64) -> BankruptcyAdjustment;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoBankruptcy;

impl BankruptcyPort for NoBankruptcy {
    fn apply_penalty(&mut self, _key: PlayerKey, positive_winnings: i64) -> BankruptcyAdjustment {
        BankruptcyAdjustment {
            penalized: positive_winnings,
            penalty_applied: 0,
            vanity_tax: 0,
            low_priority_tax: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HalvingBankruptcy {
    pub calls: Vec<(PlayerKey, i64)>,
}

impl BankruptcyPort for HalvingBankruptcy {
    fn apply_penalty(&mut self, key: PlayerKey, positive_winnings: i64) -> BankruptcyAdjustment {
        self.calls.push((key, positive_winnings));
        let penalized = positive_winnings / 2;
        BankruptcyAdjustment {
            penalized,
            penalty_applied: positive_winnings - penalized,
            vanity_tax: 0,
            low_priority_tax: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BossNotice {
    PromptReady {
        key: PlayerKey,
        boss_id: String,
        prompt: PendingPrompt,
    },
    Resolved {
        key: PlayerKey,
        boss_id: String,
        won: bool,
        jc_delta: i64,
    },
}

pub trait BossOutcomeSinkPort {
    fn emit(&mut self, notice: BossNotice);
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InMemoryBossOutcomeSink {
    pub notices: Vec<BossNotice>,
}

impl BossOutcomeSinkPort for InMemoryBossOutcomeSink {
    fn emit(&mut self, notice: BossNotice) {
        self.notices.push(notice);
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CombatEffects {
    pub skip_next_round_for: Option<Combatant>,
    pub burn_rounds_remaining: u8,
    pub bleed_rounds_remaining: u8,
    pub silenced_next_round: bool,
    pub frostbite_next_round: bool,
    pub boss_exposed_next_round: bool,
    pub pet_assist: Option<PetAssist>,
    pub shifting_idol_bonus: Option<String>,
    pub boss_preparation: Option<BossPreparationState>,
    pub trophy_start_hp: Option<i32>,
    pub trophy_venom_rounds: u8,
    pub trophy_lifesteal: bool,
    pub trophy_regrowth: bool,
    pub trophy_forewarned: bool,
    pub trophy_laststand: bool,
    pub relic_berserk_rage: Option<u8>,
    pub relic_double_hit: bool,
    pub relic_deaths_door: bool,
    pub relic_paper_crane: bool,
    pub relic_bottled_quake: bool,
    pub gear_reflect_first_hit: bool,
    pub gear_block_first_status: bool,
    pub gear_springheel_counter: bool,
    pub gear_block_first_skip: bool,
    pub gear_heal_first_crit: bool,
    pub gear_reroll_first_miss: bool,
    pub paper_crane_blocked: Option<MechanicStatusEffect>,
    pub nullweave_blocked: Option<MechanicStatusEffect>,
    pub anchor_boots_blocked: bool,
}

impl CombatEffects {
    fn has_seeded_attempt_effect(&self) -> bool {
        self.trophy_venom_rounds > 0
            || self.trophy_lifesteal
            || self.trophy_regrowth
            || self.trophy_forewarned
            || self.trophy_laststand
            || self.relic_berserk_rage.is_some()
            || self.relic_double_hit
            || self.relic_deaths_door
            || self.relic_paper_crane
            || self.relic_bottled_quake
            || self.gear_reflect_first_hit
            || self.gear_block_first_status
            || self.gear_springheel_counter
            || self.gear_block_first_skip
            || self.gear_heal_first_crit
            || self.gear_reroll_first_miss
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BossPreparationState {
    pub item_type: String,
    pub used: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CombatState {
    pub player_hp: i32,
    pub boss_hp: i32,
    pub player_hit: f64,
    pub player_damage: i32,
    pub boss_hit: f64,
    pub boss_damage: i32,
    pub critical_chance: f64,
    pub critical_bonus: i32,
    pub effects: CombatEffects,
}

fn combat_win_probability(combat: &CombatState) -> f64 {
    duel_win_probability_with_damage_bonus(
        DuelOddsInput {
            player_hp: combat.player_hp,
            boss_hp: combat.boss_hp,
            player_hit: combat.player_hit,
            player_damage: combat.player_damage,
            boss_hit: combat.boss_hit,
            boss_damage: combat.boss_damage,
            critical_chance: combat.critical_chance,
            critical_bonus: combat.critical_bonus,
        },
        combat
            .effects
            .pet_assist
            .as_ref()
            .map_or(0, |assist| assist.bonus_percent),
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoundRecord {
    pub round: u8,
    pub player_hp: i32,
    pub boss_hp: i32,
    pub player_hit: bool,
    pub boss_hit: Option<bool>,
    pub pet_assist: Option<PetAssist>,
    pub pet_assist_damage: i32,
    pub effect_log: RoundEffectLog,
    pub mechanic: Option<MechanicRoundRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MechanicRoundRecord {
    pub mechanic_id: String,
    pub option_index: usize,
    pub option_label: String,
    pub narrative: String,
    pub warding_salts_blocked: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RoundEffectLog {
    pub critical: bool,
    pub venom: bool,
    pub deaths_door: bool,
    pub bottled_quake: bool,
    pub berserk_bonus: u8,
    pub laststand: bool,
    pub double_hit: bool,
    pub blood_locket_heal: bool,
    pub lifesteal: bool,
    pub forewarned: bool,
    pub briarplate_reflect: bool,
    pub springheel_counter: bool,
    pub red_thread_reroll: bool,
    pub regrowth: bool,
    pub skipped_player: bool,
    pub skipped_boss: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PausedBossDuel {
    pub boss_id: String,
    pub boundary: i32,
    pub mechanic_id: String,
    pub risk_tier: RiskTier,
    pub wager: i64,
    pub combat: CombatState,
    pub round_num: u8,
    pub round_log: Vec<RoundRecord>,
    pub pending_prompt: PendingPrompt,
    pub attempts_this_fight: i32,
    pub initial_win_chance: f64,
    pub payout_multiplier: f64,
    pub player_hp_max: i32,
    pub boss_hp_max: i32,
    pub starting_boss_hp: i32,
    pub gear_snapshot: GearSnapshot,
    pub forced_no_wager_phase: bool,
    pub echo_applied: bool,
    pub echo_killer_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EchoRecord {
    pub boss_id: String,
    pub boundary: i32,
    pub killer_id: i64,
    pub weakened_until: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TunnelState {
    pub depth: i32,
    pub max_depth: i32,
    pub prestige_level: i32,
    pub balance: i64,
    pub boss_progress: BossProgress,
    pub stinger_curse: StingerCurseState,
    pub lanterns: u32,
    pub has_great_lantern: bool,
    pub boss_attempts: i32,
    pub last_dig_at: i64,
    pub luminosity: i32,
    pub revision: u64,
}

impl Default for TunnelState {
    fn default() -> Self {
        Self {
            depth: 0,
            max_depth: 0,
            prestige_level: 0,
            balance: 0,
            boss_progress: canonical_boss_progress(),
            stinger_curse: StingerCurseState::default(),
            lanterns: 0,
            has_great_lantern: false,
            boss_attempts: 0,
            last_dig_at: 0,
            luminosity: 100,
            revision: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BossAuditRecord {
    pub boss_id: String,
    pub boundary: i32,
    pub won: bool,
    pub jc_delta: i64,
    pub depth_after: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepositoryCommit {
    pub next_tunnel: TunnelState,
    pub audit: Option<BossAuditRecord>,
    pub echo: Option<EchoRecord>,
    /// Separate ledger debit applied after crediting the gross Dig payout.
    pub vanity_tax: i64,
    /// Sibling debit for a player in low priority, recorded on its own row.
    pub low_priority_tax: i64,
    pub preparation_mutation: BossPreparationMutation,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BossPreparationMutation {
    #[default]
    Preserve,
    MarkUsed {
        boundary: i32,
    },
    Clear {
        boundary: i32,
    },
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum RepositoryError {
    #[error("the tunnel does not exist")]
    MissingTunnel,
    #[error("the tunnel changed before the guarded commit")]
    Conflict,
    #[error("an injected repository failure occurred")]
    InjectedFailure,
}

pub trait BossRepositoryPort {
    fn load_tunnel(&self, key: PlayerKey) -> Option<TunnelState>;
    fn commit(
        &mut self,
        key: PlayerKey,
        expected_revision: u64,
        commit: RepositoryCommit,
    ) -> Result<(), RepositoryError>;
    fn save_active_duel(
        &mut self,
        key: PlayerKey,
        duel: PausedBossDuel,
    ) -> Result<(), RepositoryError>;
    fn claim_active_duel(
        &mut self,
        key: PlayerKey,
    ) -> Result<Option<PausedBossDuel>, RepositoryError>;
    fn active_duel(&self, key: PlayerKey) -> Option<PausedBossDuel>;
    fn active_echo(&self, guild_id: GuildId, boss_id: &str, now: i64) -> Option<EchoRecord>;
    fn audits(&self) -> &[BossAuditRecord];
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InMemoryBossRepository {
    tunnels: BTreeMap<PlayerKey, TunnelState>,
    active_duels: BTreeMap<PlayerKey, PausedBossDuel>,
    echoes: BTreeMap<(GuildId, String), EchoRecord>,
    audit_log: Vec<BossAuditRecord>,
    fail_next_commit: bool,
}

impl InMemoryBossRepository {
    pub fn insert_tunnel(&mut self, key: PlayerKey, tunnel: TunnelState) {
        self.tunnels.insert(key, tunnel);
    }

    pub fn inject_commit_failure(&mut self) {
        self.fail_next_commit = true;
    }
}

impl BossRepositoryPort for InMemoryBossRepository {
    fn load_tunnel(&self, key: PlayerKey) -> Option<TunnelState> {
        self.tunnels.get(&key).cloned()
    }

    fn commit(
        &mut self,
        key: PlayerKey,
        expected_revision: u64,
        mut commit: RepositoryCommit,
    ) -> Result<(), RepositoryError> {
        if self.fail_next_commit {
            self.fail_next_commit = false;
            return Err(RepositoryError::InjectedFailure);
        }
        let current = self
            .tunnels
            .get(&key)
            .ok_or(RepositoryError::MissingTunnel)?;
        if current.revision != expected_revision {
            return Err(RepositoryError::Conflict);
        }
        commit.next_tunnel.revision = expected_revision.saturating_add(1);
        self.tunnels.insert(key, commit.next_tunnel);
        if let Some(audit) = commit.audit {
            self.audit_log.push(audit);
        }
        if let Some(echo) = commit.echo {
            self.echoes
                .insert((key.guild_id, echo.boss_id.clone()), echo);
        }
        Ok(())
    }

    fn save_active_duel(
        &mut self,
        key: PlayerKey,
        duel: PausedBossDuel,
    ) -> Result<(), RepositoryError> {
        if !self.tunnels.contains_key(&key) {
            return Err(RepositoryError::MissingTunnel);
        }
        self.active_duels.insert(key, duel);
        Ok(())
    }

    fn claim_active_duel(
        &mut self,
        key: PlayerKey,
    ) -> Result<Option<PausedBossDuel>, RepositoryError> {
        Ok(self.active_duels.remove(&key))
    }

    fn active_duel(&self, key: PlayerKey) -> Option<PausedBossDuel> {
        self.active_duels.get(&key).cloned()
    }

    fn active_echo(&self, guild_id: GuildId, boss_id: &str, now: i64) -> Option<EchoRecord> {
        self.echoes
            .get(&(guild_id, boss_id.to_owned()))
            .filter(|echo| echo.weakened_until > now)
            .cloned()
    }

    fn audits(&self) -> &[BossAuditRecord] {
        &self.audit_log
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MechanicOutcome {
    pub narrative: String,
    pub player_hp: i32,
    pub boss_hp: i32,
    pub effects: CombatEffects,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VictoryEconomy {
    pub gross_base_reward: i64,
    pub scaled_base_reward: i64,
    pub gross_payout: i64,
    pub credited: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub low_priority_tax: i64,
    pub wager_profit_after_event: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedFight {
    pub won: bool,
    pub boss_id: String,
    pub boundary: i32,
    pub wager: i64,
    pub win_chance: f64,
    pub jc_delta: i64,
    pub gross_payout: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub low_priority_tax: i64,
    pub new_depth: i32,
    pub boss_hp_remaining: i32,
    pub boss_hp_max: i32,
    pub starting_boss_hp: i32,
    pub extra_knockback: i32,
    pub extra_cooldown_seconds: i64,
    pub round_log: Vec<RoundRecord>,
    pub gear_wear: GearWearResult,
    pub phase_transition: Option<BossStatus>,
    pub boss_preparation: Option<BossPreparationState>,
    pub rescue_line_used: bool,
    pub warding_salts_blocked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StartBossDuelOutcome {
    Paused(Box<PausedBossDuel>),
    Resolved(ResolvedFight),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoutRiskDetails {
    pub win_chance: f64,
    pub free_fight_chance: f64,
    pub multiplier: f64,
    pub player_hp: i32,
    pub boss_hp: i32,
    pub player_hit: f64,
    pub boss_hit: f64,
    pub player_damage: i32,
    pub boss_damage: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoutResult {
    pub boss_id: String,
    pub boundary: i32,
    pub odds: ScoutOdds,
    pub pet_assist: Option<PetAssist>,
    pub echo_applied: bool,
    pub echo_killer_id: Option<i64>,
    pub enhanced: bool,
    pub mechanic_pool: Vec<String>,
    pub stinger: Option<ScoutStinger>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoutStinger {
    pub id: String,
    pub extra_knockback: i32,
    pub extended_cooldown_seconds: i64,
    pub cursed_status: Option<CurseKind>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoutOdds {
    pub cautious: ScoutRiskDetails,
    pub bold: ScoutRiskDetails,
    pub reckless: ScoutRiskDetails,
}

impl ScoutOdds {
    #[must_use]
    pub const fn for_risk(&self, risk: RiskTier) -> &ScoutRiskDetails {
        match risk {
            RiskTier::Cautious => &self.cautious,
            RiskTier::Bold => &self.bold,
            RiskTier::Reckless => &self.reckless,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BossServiceError {
    #[error("the player does not have a tunnel")]
    MissingTunnel,
    #[error("the player is not at a boss boundary")]
    NotAtBossBoundary,
    #[error("no boss is available at tier {0}")]
    EmptyBossPool(i32),
    #[error("the wager must be non-negative")]
    NegativeWager,
    #[error("the player cannot afford a wager of {wager} with balance {balance}")]
    InsufficientBalance { wager: i64, balance: i64 },
    #[error("wagers are only allowed on the final regular-boss phase")]
    WagerNotAllowed,
    #[error("there is no active duel to resume")]
    NoActiveDuel,
    #[error("the active duel references unknown boss {0}")]
    UnknownBoss(String),
    #[error("the active duel references unknown mechanic {0}")]
    UnknownMechanic(String),
    #[error("a Lantern is required to scout")]
    LanternRequired,
    #[error("a boss curse blocks this scout; the Lantern was not consumed")]
    ScoutCurseBlocked,
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

#[must_use]
pub fn canonical_boss_progress() -> BossProgress {
    BOSS_BOUNDARIES
        .into_iter()
        .map(|boundary| {
            (
                boundary.to_string(),
                BossProgressValue::Legacy(BossStatus::Active),
            )
        })
        .collect()
}

#[must_use]
pub fn progress_statuses(progress: &BossProgress) -> BTreeMap<i32, BossStatus> {
    let mut normalized: BTreeMap<i32, BossStatus> = BOSS_BOUNDARIES
        .into_iter()
        .map(|boundary| (boundary, BossStatus::Active))
        .collect();
    for (key, value) in progress {
        if let Ok(boundary) = key.parse::<i32>() {
            normalized.insert(boundary, value.status());
        }
    }
    normalized
}

#[must_use]
pub fn next_boss_boundary(progress: &BossProgress) -> Option<i32> {
    let statuses = progress_statuses(progress);
    for boundary in BOSS_BOUNDARIES {
        if statuses
            .get(&boundary)
            .copied()
            .unwrap_or(BossStatus::Active)
            .unfinished()
        {
            return Some(boundary);
        }
    }
    let pinnacle_status = progress
        .get(&PINNACLE_DEPTH.to_string())
        .map(BossProgressValue::status);
    let all_regular_defeated = BOSS_BOUNDARIES
        .iter()
        .all(|boundary| statuses.get(boundary).copied() == Some(BossStatus::Defeated));
    (all_regular_defeated && pinnacle_status.is_none_or(BossStatus::unfinished))
        .then_some(PINNACLE_DEPTH)
}

#[must_use]
pub fn at_boss_boundary(depth: i32, progress: &BossProgress) -> Option<i32> {
    let statuses = progress_statuses(progress);
    BOSS_BOUNDARIES.into_iter().find(|boundary| {
        depth >= boundary - 1
            && statuses
                .get(boundary)
                .copied()
                .unwrap_or(BossStatus::Active)
                .unfinished()
    })
}

#[must_use]
pub fn locked_boss_id(progress: &BossProgress, boundary: i32, prestige_level: i32) -> Option<&str> {
    if let Some(BossProgressValue::Entry(entry)) = progress.get(&boundary.to_string())
        && let Some(boss_id) = entry.boss_id.as_deref()
        && !boss_id.is_empty()
    {
        return Some(boss_id);
    }
    boss_pool_for_tier(boundary, prestige_level)
        .first()
        .map(|boss| boss.boss_id)
}

#[must_use]
pub fn regular_boss_wager_allowed(
    progress: &BossProgress,
    boundary: i32,
    prestige_level: i32,
) -> bool {
    let status = progress
        .get(&boundary.to_string())
        .map_or(BossStatus::Active, BossProgressValue::status);
    let has_phase_two = prestige_level >= PHASE_TWO_MIN_PRESTIGE && status == BossStatus::Active;
    let has_phase_three = prestige_level >= PHASE_THREE_MIN_PRESTIGE
        && boundary >= PHASE_THREE_MIN_TIER
        && status == BossStatus::PhaseOneDefeated;
    !(has_phase_two || has_phase_three)
}

#[must_use]
pub fn forced_no_wager_phase(
    progress: &BossProgress,
    boundary: i32,
    prestige_level: i32,
    wager: i64,
) -> bool {
    wager == 0 && !regular_boss_wager_allowed(progress, boundary, prestige_level)
}

fn tier_player_hit_penalty(boundary: i32) -> f64 {
    match boundary {
        50 => 0.01,
        75 => 0.02,
        100 => 0.04,
        150 => 0.06,
        200 | 275 => 0.07,
        _ => 0.0,
    }
}

fn prestige_player_hit_penalty(prestige: i32) -> f64 {
    match prestige {
        i32::MIN..=0 => 0.0,
        1 => 0.03,
        2 => 0.05,
        3 => 0.08,
        4 => 0.11,
        5 => 0.14,
        6 => 0.165,
        _ => 0.19,
    }
}

fn base_combat(risk: RiskTier) -> CombatState {
    let (player_hp, boss_hp, player_hit, player_damage, boss_hit, critical_chance) = match risk {
        RiskTier::Cautious => (5, 4, 0.60, 1, 0.30, 0.0),
        RiskTier::Bold => (3, 5, 0.42, 2, 0.45, 0.15),
        RiskTier::Reckless => (2, 6, 0.18, 3, 0.60, 0.30),
    };
    CombatState {
        player_hp,
        boss_hp,
        player_hit,
        player_damage,
        boss_hit,
        boss_damage: 1,
        critical_chance,
        critical_bonus: i32::from(risk != RiskTier::Cautious),
        effects: CombatEffects::default(),
    }
}

#[must_use]
pub fn effective_player_hit(
    base_hit: f64,
    boundary: i32,
    prestige_level: i32,
    wager: i64,
    forced_no_wager: bool,
) -> f64 {
    let mut hit =
        base_hit - tier_player_hit_penalty(boundary) - prestige_player_hit_penalty(prestige_level);
    if wager == 0 && !forced_no_wager {
        hit *= FREE_FIGHT_ACCURACY_MULTIPLIER;
    }
    let floor = if wager > 0 {
        WAGERED_PLAYER_HIT_FLOOR
    } else {
        PLAYER_HIT_FLOOR
    };
    hit.clamp(floor, PLAYER_HIT_CEILING)
}

#[must_use]
pub fn resolve_persisted_hp(
    progress: &BossProgress,
    boundary: i32,
    fresh_hp: i32,
    now: i64,
) -> (i32, i32) {
    let Some(BossProgressValue::Entry(entry)) = progress.get(&boundary.to_string()) else {
        return (fresh_hp, fresh_hp);
    };
    let (Some(remaining), Some(maximum)) = (entry.hp_remaining, entry.hp_max) else {
        return (fresh_hp, fresh_hp);
    };
    let maximum = maximum.max(1);
    let mut remaining = remaining.clamp(1, maximum);
    if let Some(last_engaged_at) = entry.last_engaged_at {
        let days = now.saturating_sub(last_engaged_at).max(0) / 86_400;
        let regenerated = days.saturating_mul(i64::from(BOSS_HP_REGEN_PER_DAY));
        remaining = i64::from(remaining)
            .saturating_add(regenerated)
            .min(i64::from(maximum)) as i32;
    }
    (remaining.max(1), maximum)
}

fn progress_entry_mut(progress: &mut BossProgress, boundary: i32) -> &mut BossProgressEntry {
    let key = boundary.to_string();
    let replacement = match progress.remove(&key) {
        Some(BossProgressValue::Entry(entry)) => entry,
        Some(BossProgressValue::Legacy(status)) => BossProgressEntry {
            status,
            ..BossProgressEntry::default()
        },
        None => BossProgressEntry::default(),
    };
    progress.insert(key.clone(), BossProgressValue::Entry(replacement));
    match progress.get_mut(&key) {
        Some(BossProgressValue::Entry(entry)) => entry,
        _ => unreachable!("the entry was inserted immediately above"),
    }
}

pub fn persist_boss_hp(
    progress: &mut BossProgress,
    boundary: i32,
    boss_id: &str,
    ending_hp: i32,
    hp_max: i32,
    outcome: &str,
    now: i64,
) {
    let entry = progress_entry_mut(progress, boundary);
    if entry.boss_id.as_deref().is_none_or(str::is_empty) {
        entry.boss_id = Some(boss_id.to_owned());
    }
    entry.hp_remaining = Some(ending_hp.max(0));
    entry.hp_max = Some(hp_max.max(1));
    entry.last_engaged_at = Some(now);
    entry.last_outcome = Some(outcome.to_owned());
    entry.first_meet_seen = true;
}

fn phase_after_win(status: BossStatus, boundary: i32, prestige: i32) -> Option<BossStatus> {
    if status == BossStatus::Active && prestige >= PHASE_TWO_MIN_PRESTIGE {
        return Some(BossStatus::PhaseOneDefeated);
    }
    if status == BossStatus::PhaseOneDefeated
        && prestige >= PHASE_THREE_MIN_PRESTIGE
        && boundary >= PHASE_THREE_MIN_TIER
    {
        return Some(BossStatus::PhaseTwoDefeated);
    }
    None
}

fn apply_status_effect(effects: &mut CombatEffects, status_effect: MechanicStatusEffect) {
    match status_effect {
        MechanicStatusEffect::Burn => effects.burn_rounds_remaining = 2,
        MechanicStatusEffect::Silence => effects.silenced_next_round = true,
        MechanicStatusEffect::Bleed => effects.bleed_rounds_remaining = 3,
        MechanicStatusEffect::Frostbite => effects.frostbite_next_round = true,
        MechanicStatusEffect::Reveal => effects.boss_exposed_next_round = true,
    }
}

pub fn apply_option_outcome(
    option: &MechanicOption,
    player_hp: i32,
    boss_hp: i32,
    effects: &CombatEffects,
    entropy: &mut impl EntropyPort,
) -> MechanicOutcome {
    let value = entropy.next_unit();
    let mut cumulative = 0.0;
    let chosen = option
        .outcome_rolls
        .iter()
        .find(|outcome| {
            cumulative += outcome.probability;
            value < cumulative
        })
        .or_else(|| option.outcome_rolls.last())
        .expect("mechanic options always contain at least one outcome");
    let mut next_effects = effects.clone();
    if let Some(skip) = chosen.skip_next_round_for {
        if skip == Combatant::Player && next_effects.gear_block_first_skip {
            next_effects.gear_block_first_skip = false;
            next_effects.anchor_boots_blocked = true;
        } else {
            next_effects.skip_next_round_for = Some(skip);
        }
    }
    if let Some(status_effect) = chosen.status_effect {
        if next_effects.relic_paper_crane {
            next_effects.relic_paper_crane = false;
            next_effects.paper_crane_blocked = Some(status_effect);
        } else if next_effects.gear_block_first_status {
            next_effects.gear_block_first_status = false;
            next_effects.nullweave_blocked = Some(status_effect);
        } else {
            apply_status_effect(&mut next_effects, status_effect);
        }
    }
    MechanicOutcome {
        narrative: chosen.narrative.clone(),
        player_hp: player_hp + chosen.player_hp_delta,
        boss_hp: boss_hp + chosen.boss_hp_delta,
        effects: next_effects,
    }
}

/// Resolve one automatic boss round while mutating one-shot combat effects.
/// Regular and pinnacle application services share this engine so a mechanic
/// pause cannot change trophy, relic, unique-gear, mana, or pet semantics.
#[must_use]
pub fn run_combat_round(
    round: u8,
    combat: &mut CombatState,
    entropy: &mut impl EntropyPort,
) -> (RoundRecord, Option<bool>) {
    let hp_at_round_start = combat.player_hp;
    let mut effect_log = RoundEffectLog::default();
    if combat.effects.boss_exposed_next_round {
        combat.boss_hp -= 1;
        combat.effects.boss_exposed_next_round = false;
    }
    if combat.effects.burn_rounds_remaining > 0 {
        combat.player_hp -= 1;
        combat.effects.burn_rounds_remaining -= 1;
    }
    if combat.effects.bleed_rounds_remaining > 0 {
        combat.player_hp -= 1;
        combat.effects.bleed_rounds_remaining -= 1;
    }
    if combat.effects.trophy_venom_rounds > 0 {
        combat.boss_hp -= 1;
        combat.effects.trophy_venom_rounds -= 1;
        effect_log.venom = true;
    }
    if combat.boss_hp <= 0 {
        return (
            RoundRecord {
                round,
                player_hp: combat.player_hp.max(0),
                boss_hp: combat.boss_hp.max(0),
                player_hit: false,
                boss_hit: None,
                pet_assist: combat.effects.pet_assist.clone(),
                pet_assist_damage: 0,
                effect_log,
                mechanic: None,
            },
            Some(true),
        );
    }
    if combat.player_hp <= 0 {
        if combat.effects.relic_deaths_door && entropy.next_unit() < 0.40 {
            combat.player_hp = 1;
            combat.effects.relic_deaths_door = false;
            effect_log.deaths_door = true;
        } else {
            return (
                RoundRecord {
                    round,
                    player_hp: 0,
                    boss_hp: combat.boss_hp.max(0),
                    player_hit: false,
                    boss_hit: None,
                    pet_assist: combat.effects.pet_assist.clone(),
                    pet_assist_damage: 0,
                    effect_log,
                    mechanic: None,
                },
                Some(false),
            );
        }
    }

    let skip = combat.effects.skip_next_round_for.take();
    let silenced = std::mem::take(&mut combat.effects.silenced_next_round);
    let frostbite = std::mem::take(&mut combat.effects.frostbite_next_round);
    let effective_player_hit = if silenced { 0.0 } else { combat.player_hit };
    let player_hit = if skip == Some(Combatant::Player) {
        effect_log.skipped_player = true;
        false
    } else {
        let mut landed = entropy.next_unit() < effective_player_hit;
        if !landed && effective_player_hit > 0.0 && combat.effects.gear_reroll_first_miss {
            combat.effects.gear_reroll_first_miss = false;
            effect_log.red_thread_reroll = true;
            landed = entropy.next_unit() < effective_player_hit;
        }
        landed
    };
    let mut pet_assist_damage = 0;
    if player_hit {
        let mut damage = combat.player_damage;
        if combat.effects.relic_bottled_quake {
            damage = damage.saturating_add(1);
            combat.effects.relic_bottled_quake = false;
            effect_log.bottled_quake = true;
        }
        if let Some(rage) = combat.effects.relic_berserk_rage {
            let bonus = rage.min(2);
            damage = damage.saturating_add(i32::from(bonus));
            effect_log.berserk_bonus = bonus;
        }
        if combat.player_hp == 1 && combat.effects.trophy_laststand {
            damage = damage.saturating_add(1);
            effect_log.laststand = true;
        }
        if combat.effects.relic_double_hit && entropy.next_unit() < 0.10 {
            damage = damage.saturating_mul(2);
            effect_log.double_hit = true;
        }
        if combat.critical_chance > 0.0 && entropy.next_unit() < combat.critical_chance {
            damage += combat.critical_bonus;
            effect_log.critical = true;
            if combat.effects.gear_heal_first_crit {
                let maximum = combat
                    .effects
                    .trophy_start_hp
                    .unwrap_or_else(|| combat.player_hp.saturating_add(1));
                combat.player_hp = combat.player_hp.saturating_add(1).min(maximum);
                combat.effects.gear_heal_first_crit = false;
                effect_log.blood_locket_heal = true;
            }
        }
        if let Some(assist) = &combat.effects.pet_assist {
            let scaled = damage.max(0) * assist.bonus_percent.max(0);
            pet_assist_damage = scaled / 100;
            let remainder = scaled % 100;
            pet_assist_damage +=
                i32::from(remainder > 0 && entropy.next_unit() < f64::from(remainder) / 100.0);
            damage = damage.saturating_add(pet_assist_damage);
        }
        combat.boss_hp -= damage;
        if combat.effects.trophy_lifesteal {
            combat.player_hp = combat.player_hp.saturating_add(1);
            combat.effects.trophy_lifesteal = false;
            effect_log.lifesteal = true;
        }
    }
    if combat.boss_hp <= 0 {
        return (
            RoundRecord {
                round,
                player_hp: combat.player_hp.max(0),
                boss_hp: 0,
                player_hit,
                boss_hit: None,
                pet_assist: combat.effects.pet_assist.clone(),
                pet_assist_damage,
                effect_log,
                mechanic: None,
            },
            Some(true),
        );
    }

    let boss_hit = if skip == Some(Combatant::Boss) {
        effect_log.skipped_boss = true;
        false
    } else if round == 1 && combat.effects.trophy_forewarned {
        effect_log.forewarned = true;
        false
    } else {
        entropy.next_unit() < combat.boss_hit
    };
    if boss_hit {
        combat.player_hp -= combat.boss_damage + i32::from(frostbite);
        if combat.effects.gear_reflect_first_hit {
            combat.boss_hp -= 1;
            combat.effects.gear_reflect_first_hit = false;
            effect_log.briarplate_reflect = true;
        }
    } else if skip != Some(Combatant::Boss) && combat.effects.gear_springheel_counter {
        combat.boss_hp -= 1;
        combat.effects.gear_springheel_counter = false;
        effect_log.springheel_counter = true;
    }
    if combat.boss_hp <= 0 {
        return (
            RoundRecord {
                round,
                player_hp: combat.player_hp.max(0),
                boss_hp: 0,
                player_hit,
                boss_hit: Some(boss_hit),
                pet_assist: combat.effects.pet_assist.clone(),
                pet_assist_damage,
                effect_log,
                mechanic: None,
            },
            Some(true),
        );
    }
    if let Some(rage) = combat.effects.relic_berserk_rage.as_mut()
        && combat.player_hp < hp_at_round_start
    {
        *rage = rage.saturating_add(1).min(2);
    }
    if combat.player_hp <= 0 {
        if combat.effects.relic_deaths_door && entropy.next_unit() < 0.40 {
            combat.player_hp = 1;
            combat.effects.relic_deaths_door = false;
            effect_log.deaths_door = true;
        } else {
            return (
                RoundRecord {
                    round,
                    player_hp: 0,
                    boss_hp: combat.boss_hp.max(0),
                    player_hit,
                    boss_hit: Some(boss_hit),
                    pet_assist: combat.effects.pet_assist.clone(),
                    pet_assist_damage,
                    effect_log,
                    mechanic: None,
                },
                Some(false),
            );
        }
    }
    if combat.effects.trophy_regrowth
        && combat.player_hp == hp_at_round_start
        && combat.player_hp < combat.effects.trophy_start_hp.unwrap_or(combat.player_hp)
    {
        combat.player_hp = combat.player_hp.saturating_add(1);
        effect_log.regrowth = true;
    }
    (
        RoundRecord {
            round,
            player_hp: combat.player_hp.max(0),
            boss_hp: combat.boss_hp.max(0),
            player_hit,
            boss_hit: Some(boss_hit),
            pet_assist: combat.effects.pet_assist.clone(),
            pet_assist_damage,
            effect_log,
            mechanic: None,
        },
        None,
    )
}

#[cfg(test)]
fn run_round(
    round: u8,
    combat: &mut CombatState,
    entropy: &mut impl EntropyPort,
) -> (RoundRecord, Option<bool>) {
    run_combat_round(round, combat, entropy)
}

fn wager_profit(
    boundary: i32,
    prestige: i32,
    risk: RiskTier,
    wager: i64,
    win_chance: f64,
    echo_applied: bool,
    drain_next_reward: bool,
) -> i64 {
    if wager <= 0 {
        return 0;
    }
    let base = WagerMultiplier::new(boss_payout_multiplier(boundary, risk))
        .expect("authored boss payout multipliers are valid");
    let chance =
        WinChance::new(win_chance.clamp(0.0, 1.0)).expect("the clamped win chance is valid");
    let tapered = effective_wager_multiplier(base, chance);
    let ascension = (prestige >= 4)
        .then(|| WagerMultiplier::new(1.5).expect("the authored ascension multiplier is valid"));
    let capped = settled_wager_multiplier(tapered, ascension).value();
    let mut profit = ((wager as f64) * (capped - 1.0)).max(0.0) as i64;
    if drain_next_reward {
        let drain = ((wager as f64) * capped * 0.25).round() as i64;
        profit = (profit - drain).max(0);
    }
    if echo_applied {
        profit = ((profit as f64) * ECHO_WAGER_PROFIT_MULTIPLIER) as i64;
    }
    profit
}

fn displayed_wager_multiplier(
    boundary: i32,
    prestige: i32,
    risk: RiskTier,
    win_chance: f64,
    echo_applied: bool,
) -> f64 {
    let base = WagerMultiplier::new(boss_payout_multiplier(boundary, risk))
        .expect("authored boss payout multipliers are valid");
    let chance =
        WinChance::new(win_chance.clamp(0.0, 1.0)).expect("the clamped win chance is valid");
    let tapered = effective_wager_multiplier(base, chance);
    let ascension = (prestige >= 4)
        .then(|| WagerMultiplier::new(1.5).expect("the authored ascension multiplier is valid"));
    let settled = settled_wager_multiplier(tapered, ascension).value();
    if echo_applied {
        1.0 + (settled - 1.0) * ECHO_WAGER_PROFIT_MULTIPLIER
    } else {
        settled
    }
}

pub struct BossMultiTierService<R, E, C, V, B, G, S> {
    pub repository: R,
    pub entropy: E,
    pub clock: C,
    pub economy_event: V,
    pub bankruptcy: B,
    pub gear_repair: G,
    pub outcome_sink: S,
    combat_preparation: Box<dyn BossCombatPreparationPort>,
    preparation_activation: Box<dyn BossPreparationActivationPort>,
}

impl<R, E, C, V, B, G, S> BossMultiTierService<R, E, C, V, B, G, S>
where
    R: BossRepositoryPort,
    E: EntropyPort,
    C: ClockPort,
    V: EconomyEventPort,
    B: BankruptcyPort,
    G: GearRepairPort,
    S: BossOutcomeSinkPort,
{
    #[must_use]
    pub fn new(
        repository: R,
        entropy: E,
        clock: C,
        economy_event: V,
        bankruptcy: B,
        gear_repair: G,
        outcome_sink: S,
    ) -> Self {
        Self {
            repository,
            entropy,
            clock,
            economy_event,
            bankruptcy,
            gear_repair,
            outcome_sink,
            combat_preparation: Box::new(NeutralBossCombatPreparation),
            preparation_activation: Box::new(NoBossPreparationActivation),
        }
    }

    #[must_use]
    pub fn with_combat_preparation(
        mut self,
        combat_preparation: impl BossCombatPreparationPort + 'static,
    ) -> Self {
        self.combat_preparation = Box::new(combat_preparation);
        self
    }

    pub fn take_combat_preparation_error(&self) -> Option<String> {
        self.combat_preparation.take_error()
    }

    #[must_use]
    pub fn with_preparation_activation(
        mut self,
        preparation_activation: impl BossPreparationActivationPort + 'static,
    ) -> Self {
        self.preparation_activation = Box::new(preparation_activation);
        self
    }

    pub fn take_preparation_activation_error(&self) -> Option<String> {
        self.preparation_activation.take_error()
    }

    pub fn ensure_boss_locked(
        &mut self,
        key: PlayerKey,
        boundary: i32,
    ) -> Result<&'static BossDefinition, BossServiceError> {
        loop {
            let tunnel = self
                .repository
                .load_tunnel(key)
                .ok_or(BossServiceError::MissingTunnel)?;
            let progress_key = boundary.to_string();
            if let Some(BossProgressValue::Entry(entry)) = tunnel.boss_progress.get(&progress_key)
                && let Some(existing) = entry.boss_id.as_deref().and_then(boss_by_id)
            {
                return Ok(existing);
            }
            let pool = boss_pool_for_tier(boundary, tunnel.prestige_level);
            if pool.is_empty() {
                return Err(BossServiceError::EmptyBossPool(boundary));
            }
            let selected = pool[self.entropy.choose_index(pool.len())];
            let mut next = tunnel.clone();
            let entry = progress_entry_mut(&mut next.boss_progress, boundary);
            entry.boss_id = Some(selected.boss_id.to_owned());
            match self.repository.commit(
                key,
                tunnel.revision,
                RepositoryCommit {
                    next_tunnel: next,
                    ..RepositoryCommit::default()
                },
            ) {
                Ok(()) => return Ok(selected),
                Err(RepositoryError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn resolve_new_wager(&mut self, key: PlayerKey, wager: i64) -> Result<i64, BossServiceError> {
        if wager < 0 {
            return Err(BossServiceError::NegativeWager);
        }
        if wager == 0 {
            return Ok(0);
        }
        loop {
            let tunnel = self
                .repository
                .load_tunnel(key)
                .ok_or(BossServiceError::MissingTunnel)?;
            let cursed = tunnel.stinger_curse.contains(CurseKind::HalveNextWager);
            let effective = if cursed { wager / 2 + wager % 2 } else { wager };
            if tunnel.balance < effective {
                return Err(BossServiceError::InsufficientBalance {
                    wager: effective,
                    balance: tunnel.balance,
                });
            }
            if !cursed {
                return Ok(effective);
            }
            let mut next = tunnel.clone();
            next.stinger_curse.consume(CurseKind::HalveNextWager);
            match self.repository.commit(
                key,
                tunnel.revision,
                RepositoryCommit {
                    next_tunnel: next,
                    ..RepositoryCommit::default()
                },
            ) {
                Ok(()) => return Ok(effective),
                Err(RepositoryError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn build_combat(&mut self, request: BuildCombatRequest<'_>) -> (CombatState, i32, bool, bool) {
        let BuildCombatRequest {
            key,
            tunnel,
            boss,
            boundary,
            risk,
            wager,
            echo_applied,
            consume_phase_event,
        } = request;
        let preparation = self.combat_preparation.prepare(
            BossCombatPreparationRequest {
                key,
                tunnel,
                boss,
                boundary,
                risk_tier: risk,
                wager,
                echo_applied,
                now: self.clock.now(),
            },
            base_combat(risk),
        );
        let mut base = preparation.base_combat;
        let scaled = scale_boss_stats(
            CombatStats {
                player_hp: base.player_hp,
                boss_hp: base.boss_hp,
                player_hit: base.player_hit,
                player_damage: base.player_damage,
                boss_hit: base.boss_hit,
                boss_damage: base.boss_damage,
            },
            boss.boss_id,
            boundary,
            tunnel.prestige_level,
            echo_applied,
        );
        let forced = forced_no_wager_phase(
            &tunnel.boss_progress,
            boundary,
            tunnel.prestige_level,
            wager,
        );
        let progress_entry = tunnel
            .boss_progress
            .get(&boundary.to_string())
            .and_then(|value| match value {
                BossProgressValue::Entry(entry) => Some(entry.clone()),
                BossProgressValue::Legacy(_) => None,
            });
        let phase_number = progress_entry
            .as_ref()
            .and_then(|entry| match entry.status {
                BossStatus::PhaseOneDefeated => Some(2),
                BossStatus::PhaseTwoDefeated => Some(3),
                BossStatus::Active | BossStatus::Defeated => None,
            });
        let phase_hit_penalty = phase_number
            .and_then(|phase| regular_phase(boss.boss_id, phase))
            .map_or(0.0, |phase| phase.win_odds_penalty.abs());
        let phase_event = progress_entry
            .as_ref()
            .and_then(|entry| entry.pending_phase_event_id.as_deref())
            .and_then(transition_event);
        let wager_skin_bonus = if wager <= 0 {
            0.0
        } else {
            (wager as f64 / 500.0).min(1.0) * 0.03
        };
        let mut player_hit = effective_player_hit(
            base.player_hit + preparation.player_hit_offset + wager_skin_bonus - phase_hit_penalty,
            boundary,
            tunnel.prestige_level,
            wager,
            forced,
        );
        if let Some(event) = phase_event {
            player_hit = (player_hit + event.player_hit_offset).clamp(
                if wager > 0 {
                    WAGERED_PLAYER_HIT_FLOOR
                } else {
                    PLAYER_HIT_FLOOR
                },
                PLAYER_HIT_CEILING,
            );
        }
        let mut fresh_boss_hp = scaled
            .boss_hp
            .saturating_add(phase_event.map_or(0, |event| event.boss_hp_delta))
            .max(1);
        fresh_boss_hp = ((f64::from(fresh_boss_hp) * preparation.boss_hp_multiplier) as i32).max(1);
        let (boss_hp, boss_hp_max) = resolve_persisted_hp(
            &tunnel.boss_progress,
            boundary,
            fresh_boss_hp,
            self.clock.now(),
        );
        let luminosity_damage = if preparation.boss_no_crit_against {
            0
        } else {
            preparation.boss_damage_delta
        };
        let mut boss_damage = scaled
            .boss_damage
            .saturating_add(luminosity_damage)
            .saturating_add(phase_event.map_or(0, |event| event.boss_damage_delta))
            .max(1);
        let mut player_damage =
            ((f64::from(base.player_damage) * preparation.player_damage_multiplier) as i32).max(1);
        if preparation.damage_variance_modifier != 0.0 && self.entropy.next_unit() < 0.5 {
            let multiplier = 1.0 + preparation.damage_variance_modifier;
            player_damage = ((f64::from(player_damage) * multiplier) as i32).max(1);
            boss_damage = ((f64::from(boss_damage) * multiplier) as i32).max(1);
        }
        player_damage = player_damage
            .saturating_add(phase_event.map_or(0, |event| event.player_damage_delta))
            .max(0)
            .saturating_add(preparation.player_damage_after_phase_delta);
        let mut player_hp = base
            .player_hp
            .saturating_add(phase_event.map_or(0, |event| event.player_hp_delta))
            .max(1);
        let shifting_idol = apply_shifting_idol_attempt_stats(
            preparation.shifting_idol,
            player_hp,
            player_hit,
            base.critical_chance,
            &mut self.entropy,
        );
        player_hp = i32::try_from(shifting_idol.player_hp).unwrap_or(i32::MAX);
        player_hit = shifting_idol.player_hit;
        base.critical_chance = shifting_idol.crit_chance;
        base.effects.shifting_idol_bonus =
            shifting_idol.bonus.map(|bonus| bonus.as_str().to_owned());
        base.effects.pet_assist = preparation.pet_assist;
        base.effects.boss_preparation = preparation.boss_preparation;
        if base.effects.has_seeded_attempt_effect() {
            base.effects.trophy_start_hp = Some(player_hp);
        }
        let consumed_phase_event = consume_phase_event && phase_event.is_some();
        if consumed_phase_event {
            let entry = progress_entry_mut(&mut tunnel.boss_progress, boundary);
            entry.pending_phase_event_id = None;
        }
        (
            CombatState {
                player_hp,
                boss_hp,
                player_hit,
                player_damage,
                boss_hit: (scaled.boss_hit
                    + phase_event.map_or(0.0, |event| event.boss_hit_offset))
                .clamp(0.05, 0.95),
                boss_damage,
                ..base
            },
            boss_hp_max,
            forced,
            consumed_phase_event,
        )
    }

    fn economy_for_victory(
        &mut self,
        key: PlayerKey,
        input: &VictoryEconomyInput,
    ) -> VictoryEconomy {
        let base = boss_base_reward(input.boundary, input.prestige_level);
        let gross_base = if base > 0 {
            self.economy_event.adjust_reward(key.guild_id, base)
        } else {
            base
        };
        let scaled_base = scale_positive_dig_jc(gross_base);
        let profit = wager_profit(
            input.boundary,
            input.prestige_level,
            input.risk_tier,
            input.wager,
            input.win_chance,
            input.echo_applied,
            input.drain_next_reward,
        );
        let event_profit = if profit > 0 {
            self.economy_event.adjust_reward(key.guild_id, profit)
        } else {
            profit
        };
        let bankruptcy = self
            .bankruptcy
            .apply_penalty(key, scaled_base + event_profit);
        VictoryEconomy {
            gross_base_reward: gross_base,
            scaled_base_reward: scaled_base,
            gross_payout: gross_base + event_profit,
            credited: bankruptcy.penalized,
            bankruptcy_penalty: bankruptcy.penalty_applied,
            vanity_tax: bankruptcy.vanity_tax,
            low_priority_tax: bankruptcy.low_priority_tax,
            wager_profit_after_event: event_profit,
        }
    }

    fn persist_consumed_phase_event(
        &mut self,
        key: PlayerKey,
        before: &TunnelState,
        prepared: &TunnelState,
    ) -> Result<(), BossServiceError> {
        if before.boss_progress == prepared.boss_progress
            && before.luminosity == prepared.luminosity
        {
            return Ok(());
        }
        self.repository.commit(
            key,
            before.revision,
            RepositoryCommit {
                next_tunnel: prepared.clone(),
                ..RepositoryCommit::default()
            },
        )?;
        Ok(())
    }

    /// Settle a finished fight, retrying the guarded commit on a conflict.
    ///
    /// A resume has already claimed (deleted) the paused duel row, so a
    /// concurrent write that loses the revision race must not abandon the
    /// settlement: the wager would escape and no later start could forfeit it.
    /// The fight outcome, round log, and gear snapshot are fixed inputs, and
    /// nothing is committed before the guarded commit, so replaying the
    /// settlement against the fresh tunnel is safe.
    fn resolve_fight(
        &mut self,
        input: ResolveFightInput,
    ) -> Result<ResolvedFight, BossServiceError> {
        for _ in 0..RESOLVE_FIGHT_COMMIT_ATTEMPTS {
            match self.resolve_fight_once(input.clone()) {
                Err(BossServiceError::Repository(RepositoryError::Conflict)) => {}
                settled => return settled,
            }
        }
        Err(BossServiceError::Repository(RepositoryError::Conflict))
    }

    fn resolve_fight_once(
        &mut self,
        input: ResolveFightInput,
    ) -> Result<ResolvedFight, BossServiceError> {
        let mut tunnel = self
            .repository
            .load_tunnel(input.key)
            .ok_or(BossServiceError::MissingTunnel)?;
        if input.consumed_phase_event {
            progress_entry_mut(&mut tunnel.boss_progress, input.boundary).pending_phase_event_id =
                None;
        }
        let status = tunnel
            .boss_progress
            .get(&input.boundary.to_string())
            .map_or(BossStatus::Active, BossProgressValue::status);
        let now = self.clock.now();

        if input.won {
            if let Some(next_phase) = phase_after_win(status, input.boundary, tunnel.prestige_level)
            {
                let mut next = tunnel.clone();
                let entry = progress_entry_mut(&mut next.boss_progress, input.boundary);
                entry.status = next_phase;
                entry.hp_remaining = None;
                entry.hp_max = None;
                entry.pending_phase_event_id = Some(
                    PHASE_TRANSITION_EVENTS
                        [self.entropy.choose_index(PHASE_TRANSITION_EVENTS.len())]
                    .id
                    .to_owned(),
                );
                next.boss_attempts = next.boss_attempts.saturating_add(1);
                next.last_dig_at = now;
                self.repository.commit(
                    input.key,
                    tunnel.revision,
                    RepositoryCommit {
                        next_tunnel: next,
                        audit: Some(BossAuditRecord {
                            boss_id: input.boss_id.clone(),
                            boundary: input.boundary,
                            won: true,
                            jc_delta: 0,
                            depth_after: tunnel.depth,
                        }),
                        echo: None,
                        vanity_tax: 0,
                        low_priority_tax: 0,
                        preparation_mutation: BossPreparationMutation::Preserve,
                    },
                )?;
                let gear_wear =
                    self.gear_repair
                        .tick_resolved_fight(input.key, &input.gear_snapshot, true);
                let resolved = ResolvedFight {
                    won: true,
                    boss_id: input.boss_id,
                    boundary: input.boundary,
                    wager: input.wager,
                    win_chance: input.win_chance,
                    jc_delta: 0,
                    gross_payout: 0,
                    bankruptcy_penalty: 0,
                    vanity_tax: 0,
                    low_priority_tax: 0,
                    new_depth: tunnel.depth,
                    boss_hp_remaining: 0,
                    boss_hp_max: input.boss_hp_max,
                    starting_boss_hp: input.starting_boss_hp,
                    extra_knockback: 0,
                    extra_cooldown_seconds: 0,
                    round_log: input.round_log,
                    gear_wear,
                    phase_transition: Some(next_phase),
                    boss_preparation: input.boss_preparation,
                    rescue_line_used: false,
                    warding_salts_blocked: input.warding_salts_blocked,
                };
                self.outcome_sink.emit(BossNotice::Resolved {
                    key: input.key,
                    boss_id: resolved.boss_id.clone(),
                    won: true,
                    jc_delta: 0,
                });
                return Ok(resolved);
            }

            let drain = tunnel.stinger_curse.contains(CurseKind::DrainNextReward);
            let economy = self.economy_for_victory(
                input.key,
                &VictoryEconomyInput {
                    boundary: input.boundary,
                    prestige_level: tunnel.prestige_level,
                    risk_tier: input.risk_tier,
                    wager: input.wager,
                    win_chance: input.win_chance,
                    echo_applied: input.echo_applied,
                    drain_next_reward: drain,
                },
            );
            let mut next = tunnel.clone();
            next.balance = next.balance.saturating_add(economy.credited);
            next.depth = input.boundary;
            next.max_depth = next.max_depth.max(input.boundary);
            next.boss_attempts = 0;
            next.last_dig_at = now;
            if drain {
                next.stinger_curse.consume(CurseKind::DrainNextReward);
            }
            let entry = progress_entry_mut(&mut next.boss_progress, input.boundary);
            entry.status = BossStatus::Defeated;
            entry.boss_id = Some(input.boss_id.clone());
            entry.hp_remaining = Some(0);
            entry.hp_max = Some(input.boss_hp_max.max(1));
            entry.last_engaged_at = Some(now);
            entry.last_outcome = Some("defeated".to_owned());
            entry.first_meet_seen = true;
            entry.pending_phase_event_id = None;
            self.repository.commit(
                input.key,
                tunnel.revision,
                RepositoryCommit {
                    next_tunnel: next,
                    audit: Some(BossAuditRecord {
                        boss_id: input.boss_id.clone(),
                        boundary: input.boundary,
                        won: true,
                        jc_delta: economy.credited,
                        depth_after: input.boundary,
                    }),
                    echo: Some(EchoRecord {
                        boss_id: input.boss_id.clone(),
                        boundary: input.boundary,
                        killer_id: input.key.player_id,
                        weakened_until: now.saturating_add(ECHO_WINDOW_SECONDS),
                    }),
                    vanity_tax: economy.vanity_tax,
                    low_priority_tax: economy.low_priority_tax,
                    preparation_mutation: BossPreparationMutation::Clear {
                        boundary: input.boundary,
                    },
                },
            )?;
            let gear_wear =
                self.gear_repair
                    .tick_resolved_fight(input.key, &input.gear_snapshot, true);
            let resolved = ResolvedFight {
                won: true,
                boss_id: input.boss_id,
                boundary: input.boundary,
                wager: input.wager,
                win_chance: input.win_chance,
                jc_delta: economy.credited,
                gross_payout: economy.gross_payout,
                bankruptcy_penalty: economy.bankruptcy_penalty,
                vanity_tax: economy.vanity_tax,
                low_priority_tax: economy.low_priority_tax,
                new_depth: input.boundary,
                boss_hp_remaining: 0,
                boss_hp_max: input.boss_hp_max,
                starting_boss_hp: input.starting_boss_hp,
                extra_knockback: 0,
                extra_cooldown_seconds: 0,
                round_log: input.round_log,
                gear_wear,
                phase_transition: None,
                boss_preparation: input.boss_preparation,
                rescue_line_used: false,
                warding_salts_blocked: input.warding_salts_blocked,
            };
            self.outcome_sink.emit(BossNotice::Resolved {
                key: input.key,
                boss_id: resolved.boss_id.clone(),
                won: true,
                jc_delta: resolved.jc_delta,
            });
            return Ok(resolved);
        }

        let boss = boss_by_id(&input.boss_id)
            .ok_or_else(|| BossServiceError::UnknownBoss(input.boss_id.clone()))?;
        let stinger = stinger_by_id(boss.stinger_id);
        let extra_knockback = stinger.map_or(0, |definition| definition.extra_knockback);
        let extra_cooldown = stinger.map_or(0, |definition| definition.extended_cooldown_seconds);
        let raw_knockback = self
            .entropy
            .inclusive_i32(BOSS_LOSS_KNOCKBACK_MIN, BOSS_LOSS_KNOCKBACK_MAX)
            .saturating_add(extra_knockback);
        let rescue_line_used = input
            .boss_preparation
            .as_ref()
            .is_some_and(|preparation| preparation.item_type == "rescue_line");
        let knockback = if rescue_line_used {
            raw_knockback / 2 + raw_knockback % 2
        } else {
            raw_knockback
        };
        let requested_debit = if input.wager > 0 {
            input.wager
        } else if input.forced_no_wager_phase {
            0
        } else {
            self.gear_repair.repair_bill()
        };
        let actual_debit = requested_debit.min(tunnel.balance.max(0));
        let mut next = tunnel.clone();
        next.balance = next.balance.saturating_sub(actual_debit);
        next.depth = (next.depth - knockback).max(0);
        next.boss_attempts = next.boss_attempts.saturating_add(1);
        next.last_dig_at = now
            .saturating_add(BOSS_LOSS_EXTRA_COOLDOWN_SECONDS)
            .saturating_add(extra_cooldown);
        persist_boss_hp(
            &mut next.boss_progress,
            input.boundary,
            &input.boss_id,
            input.ending_boss_hp,
            input.boss_hp_max,
            "loss",
            now,
        );
        if let Some(curse) = stinger.and_then(|definition| definition.cursed_status) {
            next.stinger_curse.insert(curse, &input.boss_id);
        }
        let depth_after = next.depth;
        self.repository.commit(
            input.key,
            tunnel.revision,
            RepositoryCommit {
                next_tunnel: next,
                audit: Some(BossAuditRecord {
                    boss_id: input.boss_id.clone(),
                    boundary: input.boundary,
                    won: false,
                    jc_delta: -actual_debit,
                    depth_after,
                }),
                echo: None,
                vanity_tax: 0,
                low_priority_tax: 0,
                preparation_mutation: BossPreparationMutation::Clear {
                    boundary: input.boundary,
                },
            },
        )?;
        let gear_wear =
            self.gear_repair
                .tick_resolved_fight(input.key, &input.gear_snapshot, rescue_line_used);
        let resolved = ResolvedFight {
            won: false,
            boss_id: input.boss_id,
            boundary: input.boundary,
            wager: input.wager,
            win_chance: input.win_chance,
            jc_delta: -actual_debit,
            gross_payout: 0,
            bankruptcy_penalty: 0,
            vanity_tax: 0,
            low_priority_tax: 0,
            new_depth: depth_after,
            boss_hp_remaining: input.ending_boss_hp.max(0),
            boss_hp_max: input.boss_hp_max,
            starting_boss_hp: input.starting_boss_hp,
            extra_knockback,
            extra_cooldown_seconds: extra_cooldown,
            round_log: input.round_log,
            gear_wear,
            phase_transition: None,
            boss_preparation: input.boss_preparation,
            rescue_line_used,
            warding_salts_blocked: input.warding_salts_blocked,
        };
        self.outcome_sink.emit(BossNotice::Resolved {
            key: input.key,
            boss_id: resolved.boss_id.clone(),
            won: false,
            jc_delta: resolved.jc_delta,
        });
        Ok(resolved)
    }

    pub fn start_boss_duel(
        &mut self,
        key: PlayerKey,
        risk_tier: RiskTier,
        wager: i64,
    ) -> Result<StartBossDuelOutcome, BossServiceError> {
        let initial = self
            .repository
            .load_tunnel(key)
            .ok_or(BossServiceError::MissingTunnel)?;
        let boundary = at_boss_boundary(initial.depth, &initial.boss_progress)
            .ok_or(BossServiceError::NotAtBossBoundary)?;
        if wager > 0
            && !regular_boss_wager_allowed(&initial.boss_progress, boundary, initial.prestige_level)
        {
            return Err(BossServiceError::WagerNotAllowed);
        }
        let wager = self.resolve_new_wager(key, wager)?;
        self.preparation_activation.activate(key, boundary)?;
        let boss = self.ensure_boss_locked(key, boundary)?;
        let mut tunnel = self
            .repository
            .load_tunnel(key)
            .ok_or(BossServiceError::MissingTunnel)?;
        let tunnel_before_preparation = tunnel.clone();
        let mechanic_id = boss.mechanic_pool[self.entropy.choose_index(boss.mechanic_pool.len())];
        let mechanic = mechanic_by_id(mechanic_id)
            .ok_or_else(|| BossServiceError::UnknownMechanic(mechanic_id.to_owned()))?;
        let active_echo = self
            .repository
            .active_echo(key.guild_id, boss.boss_id, self.clock.now());
        let echo_applied = active_echo
            .as_ref()
            .is_some_and(|echo| echo.killer_id != key.player_id);
        let (mut combat, boss_hp_max, forced, consumed_phase_event) =
            self.build_combat(BuildCombatRequest {
                key,
                tunnel: &mut tunnel,
                boss,
                boundary,
                risk: risk_tier,
                wager,
                echo_applied,
                consume_phase_event: true,
            });
        self.persist_consumed_phase_event(key, &tunnel_before_preparation, &tunnel)?;
        let player_hp_max = combat.player_hp.max(1);
        let starting_boss_hp = combat.boss_hp;
        let initial_win_chance = combat_win_probability(&combat);
        let gear_snapshot = self.gear_repair.snapshot(key);
        let mut round_log = Vec::new();
        let mechanic_trigger_round = regular_mechanic_trigger_round(&mechanic);
        for round_num in 1..=BOSS_ROUND_CAP {
            if round_num == mechanic_trigger_round && combat.player_hp > 0 && combat.boss_hp > 0 {
                let paused = PausedBossDuel {
                    boss_id: boss.boss_id.to_owned(),
                    boundary,
                    mechanic_id: mechanic.id.clone(),
                    risk_tier,
                    wager,
                    combat,
                    round_num,
                    round_log,
                    pending_prompt: PendingPrompt::from(&mechanic),
                    attempts_this_fight: tunnel.boss_attempts.saturating_add(1),
                    initial_win_chance,
                    payout_multiplier: boss_payout_multiplier(boundary, risk_tier),
                    player_hp_max,
                    boss_hp_max,
                    starting_boss_hp,
                    gear_snapshot,
                    forced_no_wager_phase: forced,
                    echo_applied,
                    echo_killer_id: active_echo.as_ref().map(|echo| echo.killer_id),
                };
                self.repository.save_active_duel(key, paused.clone())?;
                self.outcome_sink.emit(BossNotice::PromptReady {
                    key,
                    boss_id: paused.boss_id.clone(),
                    prompt: paused.pending_prompt.clone(),
                });
                return Ok(StartBossDuelOutcome::Paused(Box::new(paused)));
            }
            let (record, terminal) = run_combat_round(round_num, &mut combat, &mut self.entropy);
            round_log.push(record);
            if let Some(won) = terminal {
                return self
                    .resolve_fight(ResolveFightInput {
                        key,
                        boss_id: boss.boss_id.to_owned(),
                        boundary,
                        risk_tier,
                        wager,
                        won,
                        ending_boss_hp: combat.boss_hp,
                        boss_hp_max,
                        starting_boss_hp,
                        win_chance: initial_win_chance,
                        echo_applied,
                        forced_no_wager_phase: forced,
                        consumed_phase_event,
                        boss_preparation: combat.effects.boss_preparation.clone(),
                        warding_salts_blocked: false,
                        gear_snapshot,
                        round_log,
                    })
                    .map(StartBossDuelOutcome::Resolved);
            }
        }
        self.resolve_fight(ResolveFightInput {
            key,
            boss_id: boss.boss_id.to_owned(),
            boundary,
            risk_tier,
            wager,
            won: false,
            ending_boss_hp: combat.boss_hp,
            boss_hp_max,
            starting_boss_hp,
            win_chance: initial_win_chance,
            echo_applied,
            forced_no_wager_phase: forced,
            consumed_phase_event,
            boss_preparation: combat.effects.boss_preparation.clone(),
            warding_salts_blocked: false,
            gear_snapshot,
            round_log,
        })
        .map(StartBossDuelOutcome::Resolved)
    }

    pub fn resume_boss_duel(
        &mut self,
        key: PlayerKey,
        requested_option_index: usize,
    ) -> Result<ResolvedFight, BossServiceError> {
        let mut paused = self
            .repository
            .claim_active_duel(key)?
            .ok_or(BossServiceError::NoActiveDuel)?;
        if boss_by_id(&paused.boss_id).is_none() {
            return Err(BossServiceError::UnknownBoss(paused.boss_id));
        }
        let mechanic = mechanic_by_id(&paused.mechanic_id)
            .ok_or_else(|| BossServiceError::UnknownMechanic(paused.mechanic_id.clone()))?;
        let option_index = if requested_option_index < mechanic.options.len() {
            requested_option_index
        } else {
            mechanic.safe_option_index
        };
        let warding_salts_blocked =
            paused
                .combat
                .effects
                .boss_preparation
                .as_mut()
                .is_some_and(|preparation| {
                    if preparation.item_type == "warding_salts" && !preparation.used {
                        preparation.used = true;
                        true
                    } else {
                        false
                    }
                });
        let narrative = if warding_salts_blocked {
            let tunnel = self
                .repository
                .load_tunnel(key)
                .ok_or(BossServiceError::MissingTunnel)?;
            self.repository.commit(
                key,
                tunnel.revision,
                RepositoryCommit {
                    next_tunnel: tunnel,
                    preparation_mutation: BossPreparationMutation::MarkUsed {
                        boundary: paused.boundary,
                    },
                    ..RepositoryCommit::default()
                },
            )?;
            "The warding salts flare and swallow the boss mechanic.".to_owned()
        } else {
            let outcome = apply_option_outcome(
                &mechanic.options[option_index],
                paused.combat.player_hp,
                paused.combat.boss_hp,
                &paused.combat.effects,
                &mut self.entropy,
            );
            paused.combat.player_hp = outcome.player_hp;
            paused.combat.boss_hp = outcome.boss_hp;
            paused.combat.effects = outcome.effects;
            outcome.narrative
        };
        paused.round_log.push(RoundRecord {
            round: paused.round_num,
            player_hp: paused.combat.player_hp.max(0),
            boss_hp: paused.combat.boss_hp.max(0),
            player_hit: false,
            boss_hit: None,
            pet_assist: paused.combat.effects.pet_assist.clone(),
            pet_assist_damage: 0,
            effect_log: RoundEffectLog::default(),
            mechanic: Some(MechanicRoundRecord {
                mechanic_id: paused.mechanic_id.clone(),
                option_index,
                option_label: mechanic.options[option_index].label.clone(),
                narrative,
                warding_salts_blocked,
            }),
        });
        let mut won = if paused.combat.boss_hp <= 0 {
            Some(true)
        } else if paused.combat.player_hp <= 0 {
            Some(false)
        } else {
            None
        };
        if won.is_none() {
            for round_num in paused.round_num.saturating_add(1)..=BOSS_ROUND_CAP {
                let (record, terminal) =
                    run_combat_round(round_num, &mut paused.combat, &mut self.entropy);
                paused.round_log.push(record);
                if terminal.is_some() {
                    won = terminal;
                    break;
                }
            }
        }
        self.resolve_fight(ResolveFightInput {
            key,
            boss_id: paused.boss_id,
            boundary: paused.boundary,
            risk_tier: paused.risk_tier,
            wager: paused.wager,
            won: won.unwrap_or(false),
            ending_boss_hp: paused.combat.boss_hp,
            boss_hp_max: paused.boss_hp_max,
            starting_boss_hp: paused.starting_boss_hp,
            win_chance: paused.initial_win_chance,
            echo_applied: paused.echo_applied,
            forced_no_wager_phase: paused.forced_no_wager_phase,
            consumed_phase_event: false,
            boss_preparation: paused.combat.effects.boss_preparation,
            warding_salts_blocked,
            gear_snapshot: paused.gear_snapshot,
            round_log: paused.round_log,
        })
    }

    pub fn fight_boss(
        &mut self,
        key: PlayerKey,
        risk_tier: RiskTier,
        wager: i64,
    ) -> Result<ResolvedFight, BossServiceError> {
        self.fight_boss_with_override(key, risk_tier, wager, None)
    }

    pub fn fight_boss_with_override(
        &mut self,
        key: PlayerKey,
        risk_tier: RiskTier,
        wager: i64,
        combat_override: Option<CombatState>,
    ) -> Result<ResolvedFight, BossServiceError> {
        let initial = self
            .repository
            .load_tunnel(key)
            .ok_or(BossServiceError::MissingTunnel)?;
        let boundary = at_boss_boundary(initial.depth, &initial.boss_progress)
            .ok_or(BossServiceError::NotAtBossBoundary)?;
        if wager > 0
            && !regular_boss_wager_allowed(&initial.boss_progress, boundary, initial.prestige_level)
        {
            return Err(BossServiceError::WagerNotAllowed);
        }
        let wager = self.resolve_new_wager(key, wager)?;
        self.preparation_activation.activate(key, boundary)?;
        let boss = self.ensure_boss_locked(key, boundary)?;
        let mut tunnel = self
            .repository
            .load_tunnel(key)
            .ok_or(BossServiceError::MissingTunnel)?;
        let echo = self
            .repository
            .active_echo(key.guild_id, boss.boss_id, self.clock.now());
        let echo_applied = echo
            .as_ref()
            .is_some_and(|record| record.killer_id != key.player_id);
        let (default_combat, boss_hp_max, forced, consumed_phase_event) =
            self.build_combat(BuildCombatRequest {
                key,
                tunnel: &mut tunnel,
                boss,
                boundary,
                risk: risk_tier,
                wager,
                echo_applied,
                consume_phase_event: true,
            });
        let has_override = combat_override.is_some();
        let mut combat = combat_override.unwrap_or(default_combat);
        let boss_hp_max = if has_override {
            combat.boss_hp.max(1)
        } else {
            boss_hp_max
        };
        let starting_boss_hp = combat.boss_hp;
        let win_chance = combat_win_probability(&combat);
        let gear_snapshot = self.gear_repair.snapshot(key);
        let mut round_log = Vec::new();
        let mut won = None;
        for round_num in 1..=BOSS_ROUND_CAP {
            let (record, terminal) = run_combat_round(round_num, &mut combat, &mut self.entropy);
            round_log.push(record);
            if terminal.is_some() {
                won = terminal;
                break;
            }
        }
        self.resolve_fight(ResolveFightInput {
            key,
            boss_id: boss.boss_id.to_owned(),
            boundary,
            risk_tier,
            wager,
            won: won.unwrap_or(false),
            ending_boss_hp: combat.boss_hp,
            boss_hp_max,
            starting_boss_hp,
            win_chance,
            echo_applied,
            forced_no_wager_phase: forced,
            consumed_phase_event,
            boss_preparation: combat.effects.boss_preparation.clone(),
            warding_salts_blocked: false,
            gear_snapshot,
            round_log,
        })
    }

    pub fn scout_boss(&mut self, key: PlayerKey) -> Result<ScoutResult, BossServiceError> {
        loop {
            let tunnel = self
                .repository
                .load_tunnel(key)
                .ok_or(BossServiceError::MissingTunnel)?;
            let boundary = at_boss_boundary(tunnel.depth, &tunnel.boss_progress)
                .ok_or(BossServiceError::NotAtBossBoundary)?;
            if !tunnel.has_great_lantern && tunnel.lanterns == 0 {
                return Err(BossServiceError::LanternRequired);
            }
            if tunnel.stinger_curse.contains(CurseKind::NoScoutNextDig) {
                let mut next = tunnel.clone();
                next.stinger_curse.consume(CurseKind::NoScoutNextDig);
                match self.repository.commit(
                    key,
                    tunnel.revision,
                    RepositoryCommit {
                        next_tunnel: next,
                        ..RepositoryCommit::default()
                    },
                ) {
                    Ok(()) => return Err(BossServiceError::ScoutCurseBlocked),
                    Err(RepositoryError::Conflict) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            let boss = self.ensure_boss_locked(key, boundary)?;
            let fresh = self
                .repository
                .load_tunnel(key)
                .ok_or(BossServiceError::MissingTunnel)?;
            let echo = self
                .repository
                .active_echo(key.guild_id, boss.boss_id, self.clock.now());
            let echo_applied = echo
                .as_ref()
                .is_some_and(|record| record.killer_id != key.player_id);
            let (cautious, _, _, _) = self.build_combat(BuildCombatRequest {
                key,
                tunnel: &mut fresh.clone(),
                boss,
                boundary,
                risk: RiskTier::Cautious,
                wager: 1,
                echo_applied,
                consume_phase_event: false,
            });
            let (bold, _, _, _) = self.build_combat(BuildCombatRequest {
                key,
                tunnel: &mut fresh.clone(),
                boss,
                boundary,
                risk: RiskTier::Bold,
                wager: 1,
                echo_applied,
                consume_phase_event: false,
            });
            let (reckless, _, _, _) = self.build_combat(BuildCombatRequest {
                key,
                tunnel: &mut fresh.clone(),
                boss,
                boundary,
                risk: RiskTier::Reckless,
                wager: 1,
                echo_applied,
                consume_phase_event: false,
            });
            let details = |combat: &CombatState, risk: RiskTier| {
                let win_chance = combat_win_probability(combat);
                let mut free = combat.clone();
                free.player_hit = (free.player_hit * FREE_FIGHT_ACCURACY_MULTIPLIER)
                    .clamp(PLAYER_HIT_FLOOR, PLAYER_HIT_CEILING);
                ScoutRiskDetails {
                    win_chance,
                    free_fight_chance: combat_win_probability(&free),
                    multiplier: displayed_wager_multiplier(
                        boundary,
                        fresh.prestige_level,
                        risk,
                        win_chance,
                        echo_applied,
                    ),
                    player_hp: combat.player_hp,
                    boss_hp: combat.boss_hp,
                    player_hit: combat.player_hit,
                    boss_hit: combat.boss_hit,
                    player_damage: combat.player_damage,
                    boss_damage: combat.boss_damage,
                }
            };
            let odds = ScoutOdds {
                cautious: details(&cautious, RiskTier::Cautious),
                bold: details(&bold, RiskTier::Bold),
                reckless: details(&reckless, RiskTier::Reckless),
            };
            let pet_assist = cautious.effects.pet_assist.clone();
            let enhanced = fresh.has_great_lantern;
            let mechanic_pool = if enhanced {
                boss.mechanic_pool
                    .iter()
                    .map(|mechanic_id| (*mechanic_id).to_owned())
                    .collect()
            } else {
                Vec::new()
            };
            let stinger = enhanced
                .then_some(boss.stinger_id)
                .and_then(stinger_by_id)
                .map(|definition| ScoutStinger {
                    id: definition.id.to_owned(),
                    extra_knockback: definition.extra_knockback,
                    extended_cooldown_seconds: definition.extended_cooldown_seconds,
                    cursed_status: definition.cursed_status,
                });
            let mut next = fresh.clone();
            if !next.has_great_lantern {
                next.lanterns = next.lanterns.saturating_sub(1);
            }
            match self.repository.commit(
                key,
                fresh.revision,
                RepositoryCommit {
                    next_tunnel: next,
                    ..RepositoryCommit::default()
                },
            ) {
                Ok(()) => {
                    return Ok(ScoutResult {
                        boss_id: boss.boss_id.to_owned(),
                        boundary,
                        odds,
                        pet_assist,
                        echo_applied,
                        echo_killer_id: echo.as_ref().map(|record| record.killer_id),
                        enhanced,
                        mechanic_pool,
                        stinger,
                    });
                }
                Err(RepositoryError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub fn retreat_boss(&mut self, key: PlayerKey) -> Result<i32, BossServiceError> {
        let tunnel = self
            .repository
            .load_tunnel(key)
            .ok_or(BossServiceError::MissingTunnel)?;
        let boundary = at_boss_boundary(tunnel.depth, &tunnel.boss_progress)
            .ok_or(BossServiceError::NotAtBossBoundary)?;
        let loss = self.entropy.inclusive_i32(2, 3);
        let mut next = tunnel.clone();
        next.depth = (next.depth - loss).max(0);
        let entry = progress_entry_mut(&mut next.boss_progress, boundary);
        entry.last_outcome = Some("retreat".to_owned());
        entry.first_meet_seen = true;
        self.repository.commit(
            key,
            tunnel.revision,
            RepositoryCommit {
                next_tunnel: next,
                audit: None,
                echo: None,
                vanity_tax: 0,
                low_priority_tax: 0,
                preparation_mutation: BossPreparationMutation::Preserve,
            },
        )?;
        Ok(loss)
    }
}

struct BuildCombatRequest<'a> {
    key: PlayerKey,
    tunnel: &'a mut TunnelState,
    boss: &'a BossDefinition,
    boundary: i32,
    risk: RiskTier,
    wager: i64,
    echo_applied: bool,
    consume_phase_event: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct VictoryEconomyInput {
    boundary: i32,
    prestige_level: i32,
    risk_tier: RiskTier,
    wager: i64,
    win_chance: f64,
    echo_applied: bool,
    drain_next_reward: bool,
}

/// Bounded retries for the settlement commit, so a pathological writer cannot
/// spin here forever.
const RESOLVE_FIGHT_COMMIT_ATTEMPTS: usize = 8;

#[derive(Clone, Debug, PartialEq)]
struct ResolveFightInput {
    key: PlayerKey,
    boss_id: String,
    boundary: i32,
    risk_tier: RiskTier,
    wager: i64,
    won: bool,
    ending_boss_hp: i32,
    boss_hp_max: i32,
    starting_boss_hp: i32,
    win_chance: f64,
    echo_applied: bool,
    forced_no_wager_phase: bool,
    consumed_phase_event: bool,
    boss_preparation: Option<BossPreparationState>,
    warding_salts_blocked: bool,
    gear_snapshot: GearSnapshot,
    round_log: Vec<RoundRecord>,
}

#[cfg(test)]
#[path = "boss_multi_tier/tests.rs"]
mod tests;
