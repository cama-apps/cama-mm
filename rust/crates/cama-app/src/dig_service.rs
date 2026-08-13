//! Discord- and database-independent orchestration for the core `/dig` loop.
//!
//! The Python service is still the only live implementation.  This module
//! mirrors its deterministic service policy behind typed inputs so the Rust
//! migration can exercise payout ordering, cooldowns, progression gates,
//! PvP, boss encounters, and atomic outcome application without starting a
//! second Discord client or database writer.

use std::collections::{BTreeMap, BTreeSet};

use cama_domain::dig_cave_in::{CAVE_IN_BLOCK_LOSS_RANGES, cave_in_band};
use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_stats::{MinerStats, miner_stat_effects};

pub const FREE_DIG_COOLDOWN_SECONDS: i64 = 3_600;
pub const CHEER_COOLDOWN_SECONDS: i64 = 45;
pub const CHEER_DURATION_SECONDS: i64 = 3_600;
pub const PAID_DIG_COSTS_PER_DAY: [i64; 8] = [3, 5, 10, 20, 40, 60, 80, 100];
pub const FIRST_DIG_ADVANCE_RANGE: (i64, i64) = (3, 7);
pub const FIRST_DIG_JC_RANGE: (i64, i64) = (1, 5);
pub const MAIN_DIG_ADVANCE_CAP: i64 = 10;
pub const BOOSTED_DIG_ADVANCE_CAP: i64 = 20;
pub const BASE_DIG_JC_PAYOUT_CAP: i64 = 20;
pub const DIG_STREAK_JC_PAYOUT_CAP: i64 = 10;
pub const DIG_STARTING_STAT_POINTS: i64 = 5;
pub const MINER_RESPEC_COST: i64 = 50;
pub const SABOTAGE_BASE_COST: i64 = 5;
pub const SABOTAGE_COST_DIVISOR: i64 = 5;
pub const SABOTAGE_DAMAGE_RANGE: (i64, i64) = (3, 8);
pub const SABOTAGE_COOLDOWN_SECONDS: i64 = 43_200;
pub const INSURANCE_BASE_COST: i64 = 5;
pub const INSURANCE_COST_DEPTH_DIVISOR: i64 = 25;
pub const INSURANCE_DURATION_SECONDS: i64 = 86_400;
pub const BOSS_WAGER_CAP: i64 = 1_000;
pub const BOSS_LOSS_REPAIR_BILL: i64 = 8;
pub const PINNACLE_DEPTH: i64 = 350;
pub const BOSS_BOUNDARIES: [i64; 7] = [25, 50, 75, 100, 150, 200, 275];

/// Fixed-point denominator for composed Dig yield multipliers.
///
/// Python composes the live yield factors as floats and truncates the result
/// once when it converts the multiplied base roll to `int`. Keeping the
/// multiplier inputs in millionths lets the Rust policy preserve that order
/// without carrying binary floating-point rounding into the payout boundary.
pub const DIG_YIELD_MULTIPLIER_SCALE: i64 = 1_000_000;
/// Fixed-point denominator for authored reward and perk multipliers.
pub const DIG_REWARD_BASIS_POINTS: i64 = 10_000;

pub const MILESTONES: [(i64, i64); 9] = [
    (25, 3),
    (50, 6),
    (75, 12),
    (100, 20),
    (150, 30),
    (200, 50),
    (275, 80),
    (350, 100),
    (400, 150),
];

pub const STREAKS: [(i64, i64); 4] = [(3, 1), (7, 3), (14, 6), (30, 10)];

/// Profit-only deductions shared by normal Dig yield and boss-victory yield.
/// Basis points keep the transport-neutral mirror deterministic while matching
/// Python's configured 75% bankruptcy keep and 10% vanity-tax rates exactly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigProfitPolicy {
    /// Fraction kept after bankruptcy. `10_000` means no active penalty.
    pub bankruptcy_keep_basis_points: i64,
    /// Vanity tax on generated profit. `0` means the player is not taxable.
    pub vanity_tax_basis_points: i64,
}

impl Default for DigProfitPolicy {
    fn default() -> Self {
        Self {
            bankruptcy_keep_basis_points: 10_000,
            vanity_tax_basis_points: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DigProfitSettlement {
    pub gross: i64,
    pub net: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
}

/// Apply a non-negative basis-point multiplier with Python's positive-value
/// floor (`int(amount * multiplier)`). The i128 numerator keeps large but
/// valid persisted amounts from overflowing before the authored truncation.
#[must_use]
pub fn scale_dig_reward_basis_points(amount: i64, basis_points: i64) -> i64 {
    if amount <= 0 || basis_points <= 0 {
        return 0;
    }
    let scaled =
        (i128::from(amount) * i128::from(basis_points)) / i128::from(DIG_REWARD_BASIS_POINTS);
    scaled.min(i128::from(i64::MAX)) as i64
}

/// Compose and apply one or more non-negative fixed-point yield factors.
///
/// The product is divided exactly once, after all factors have been applied,
/// matching Python's `int(base * factor_a * factor_b * ...)` behavior. An
/// empty factor list is an identity operation. Negative factors are treated
/// as zero because a generated positive Dig reward cannot become negative
/// through a yield multiplier.
#[must_use]
pub fn scale_dig_yield_once(amount: i64, multipliers_millionths: &[i64]) -> i64 {
    if amount <= 0 {
        return 0;
    }
    if multipliers_millionths.is_empty() {
        return amount;
    }

    let mut numerator = i128::from(amount);
    let mut denominator = 1_i128;
    for multiplier in multipliers_millionths {
        numerator = numerator.saturating_mul(i128::from((*multiplier).max(0)));
        denominator = denominator.saturating_mul(i128::from(DIG_YIELD_MULTIPLIER_SCALE));
    }

    (numerator / denominator).min(i128::from(i64::MAX)) as i64
}

fn percent_to_yield_millionths(percent: i64) -> i64 {
    percent
        .max(0)
        .saturating_mul(DIG_YIELD_MULTIPLIER_SCALE / 100)
}

fn effective_yield_multiplier(explicit: Option<i64>, legacy_percent: i64) -> i64 {
    explicit.unwrap_or_else(|| percent_to_yield_millionths(legacy_percent))
}

pub(crate) fn scale_dig_minigame_jc(amount: i64, scale_millionths: i64) -> i64 {
    if amount == 0 {
        return 0;
    }
    let magnitude = ((i128::from(amount.unsigned_abs())
        .saturating_mul(i128::from(scale_millionths.max(0)))
        + i128::from(DIG_YIELD_MULTIPLIER_SCALE / 2))
        / i128::from(DIG_YIELD_MULTIPLIER_SCALE))
    .max(1)
    .min(i128::from(i64::MAX)) as i64;
    if amount > 0 { magnitude } else { -magnitude }
}

fn scale_dig_helltide_tax(amount: i64) -> i64 {
    let base = amount.max(0);
    let stronger = ((i128::from(base) * 11 + 5) / 10).min(i128::from(i64::MAX)) as i64;
    if base < 4 {
        stronger
    } else if stronger <= base {
        base.saturating_add(1)
    } else {
        stronger
    }
}

/// Apply both profit policies independently to the same positive gross yield.
/// The withheld bankruptcy share is floored, rather than the kept share, so a
/// 1-2 JC trivia- or Dig-sized reward is never rounded down to zero.
#[must_use]
pub fn apply_jc_profit_policies(gross: i64, policy: DigProfitPolicy) -> DigProfitSettlement {
    let gross = gross.max(0);
    let keep_basis_points = policy.bankruptcy_keep_basis_points.clamp(0, 10_000);
    let bankruptcy_penalty =
        scale_dig_reward_basis_points(gross, 10_000_i64.saturating_sub(keep_basis_points));
    let vanity_tax =
        scale_dig_reward_basis_points(gross, policy.vanity_tax_basis_points.clamp(0, 10_000));
    DigProfitSettlement {
        gross,
        net: gross
            .saturating_sub(bankruptcy_penalty)
            .saturating_sub(vanity_tax),
        bankruptcy_penalty,
        vanity_tax,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    pub name: &'static str,
    pub min_depth: i64,
    pub max_depth: Option<i64>,
    pub cave_in_chance: f64,
    pub jc_range: (i64, i64),
    pub advance_range: (i64, i64),
    pub luminosity_drain: i64,
}

pub const LAYERS: [Layer; 8] = [
    Layer::new("Dirt", 0, Some(25), 0.05, (0, 1), (1, 3), 0),
    Layer::new("Stone", 26, Some(50), 0.10, (0, 1), (1, 3), 0),
    Layer::new("Crystal", 51, Some(75), 0.18, (0, 2), (1, 2), 0),
    Layer::new("Magma", 76, Some(100), 0.25, (1, 3), (1, 2), 3),
    Layer::new("Abyss", 101, Some(150), 0.35, (1, 4), (1, 2), 5),
    Layer::new("Fungal Depths", 151, Some(200), 0.40, (1, 5), (1, 2), 2),
    Layer::new("Frozen Core", 201, Some(275), 0.45, (2, 4), (1, 2), 7),
    Layer::new("The Hollow", 276, None, 0.50, (2, 6), (1, 1), 10),
];

impl Layer {
    const fn new(
        name: &'static str,
        min_depth: i64,
        max_depth: Option<i64>,
        cave_in_chance: f64,
        jc_range: (i64, i64),
        advance_range: (i64, i64),
        luminosity_drain: i64,
    ) -> Self {
        Self {
            name,
            min_depth,
            max_depth,
            cave_in_chance,
            jc_range,
            advance_range,
            luminosity_drain,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PickaxeTier {
    pub name: &'static str,
    pub advance_bonus: i64,
    pub depth_required: i64,
    pub jc_cost: i64,
    pub prestige_required: i64,
}

pub const PICKAXE_TIERS: [PickaxeTier; 8] = [
    PickaxeTier::new("Wooden", 0, 0, 0, 0),
    PickaxeTier::new("Stone", 1, 25, 15, 0),
    PickaxeTier::new("Iron", 1, 50, 50, 0),
    PickaxeTier::new("Diamond", 2, 75, 150, 0),
    PickaxeTier::new("Obsidian", 3, 100, 300, 1),
    PickaxeTier::new("Stormrend", 3, 150, 450, 2),
    PickaxeTier::new("Frostforged", 3, 200, 600, 3),
    PickaxeTier::new("Void-Touched", 4, 275, 1_200, 5),
];

impl PickaxeTier {
    const fn new(
        name: &'static str,
        advance_bonus: i64,
        depth_required: i64,
        jc_cost: i64,
        prestige_required: i64,
    ) -> Self {
        Self {
            name,
            advance_bonus,
            depth_required,
            jc_cost,
            prestige_required,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tip {
    pub min_depth: i64,
    pub max_depth: Option<i64>,
    pub text: &'static str,
}

pub const DIG_TIPS: [Tip; 4] = [
    Tip {
        min_depth: 0,
        max_depth: Some(10),
        text: "Your first paid dig each day is the cheapest.",
    },
    Tip {
        min_depth: 0,
        max_depth: Some(25),
        text: "Helping another miner consumes your free-dig cooldown.",
    },
    Tip {
        min_depth: 25,
        max_depth: Some(100),
        text: "Scout bosses with a lantern before choosing a risk tier.",
    },
    Tip {
        min_depth: 50,
        max_depth: None,
        text: "Low luminosity raises both danger and rewards.",
    },
];

#[must_use]
pub fn layer_at(depth: i64) -> Layer {
    LAYERS
        .iter()
        .rev()
        .find(|layer| depth >= layer.min_depth)
        .copied()
        .unwrap_or(LAYERS[0])
}

#[must_use]
pub fn eligible_tips(depth: i64) -> Vec<&'static str> {
    DIG_TIPS
        .iter()
        .filter(|tip| {
            depth >= tip.min_depth && tip.max_depth.is_none_or(|maximum| depth <= maximum)
        })
        .map(|tip| tip.text)
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionContractSteal {
    pub prediction_id: i64,
    pub side: &'static str,
    pub contracts: i64,
}

#[must_use]
pub fn append_prediction_contract_steal(
    description: &str,
    sabotage_hit: bool,
    steal: Option<&PredictionContractSteal>,
) -> String {
    let Some(steal) = steal.filter(|_| sabotage_hit) else {
        return description.to_owned();
    };
    format!(
        "{description}\nStole **{} {}** prediction contracts from market **#{}**.",
        steal.contracts,
        steal.side.to_ascii_uppercase(),
        steal.prediction_id
    )
}

#[must_use]
pub fn prestige_cave_in_multiplier(prestige_level: i64) -> f64 {
    0.9 + prestige_level.max(0) as f64 * 0.03
}

#[must_use]
pub fn paid_dig_cost(paid_count: usize, stamina: i64, ascension_markup: f64) -> i64 {
    let base = PAID_DIG_COSTS_PER_DAY[paid_count.min(PAID_DIG_COSTS_PER_DAY.len() - 1)];
    let marked_up = (base as f64 * (1.0 + ascension_markup.max(0.0))) as i64;
    let effects = miner_stat_effects(
        MinerStats::new(0, 0, stamina.max(0)).expect("normalized stamina is non-negative"),
    );
    ((marked_up as f64 * effects.paid_cost_multiplier) as i64).max(1)
}

#[must_use]
pub fn cooldown_duration(
    stamina: i64,
    restless: bool,
    slower_cooldown_injury: bool,
    mana_reduction_seconds: i64,
) -> i64 {
    let before_stamina = if slower_cooldown_injury {
        6 * 3_600
    } else {
        FREE_DIG_COOLDOWN_SECONDS
            + if restless {
                FREE_DIG_COOLDOWN_SECONDS
            } else {
                0
            }
    };
    let effects = miner_stat_effects(
        MinerStats::new(0, 0, stamina.max(0)).expect("normalized stamina is non-negative"),
    );
    ((before_stamina as f64 * effects.cooldown_multiplier) as i64)
        .max(1)
        .saturating_sub(mana_reduction_seconds.max(0))
        .max(1)
}

/// Parse the one injury variant that overrides Dig's free cooldown.
/// Malformed legacy JSON is deliberately fail-soft because this read runs on
/// every Dig/status request.
#[must_use]
pub fn has_slower_cooldown_injury(injury_state_json: Option<&str>) -> bool {
    injury_state_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|kind| kind == "slower_cooldown")
}

#[must_use]
pub fn cooldown_remaining(last_dig_at: Option<i64>, now: i64, duration: i64) -> i64 {
    last_dig_at.map_or(0, |last| (duration - (now - last)).max(0))
}

#[must_use]
pub fn bulk_ready_times(
    requested_ids: &[i64],
    tunnel_last_digs: &BTreeMap<i64, Option<i64>>,
    now: i64,
) -> BTreeMap<i64, Option<i64>> {
    requested_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|discord_id| {
            let ready_at = tunnel_last_digs
                .get(&discord_id)
                .copied()
                .flatten()
                .map(|last| last + FREE_DIG_COOLDOWN_SECONDS)
                .filter(|ready| *ready > now);
            (discord_id, ready_at)
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MinerAllocation {
    pub strength: i64,
    pub smarts: i64,
    pub stamina: i64,
    pub stat_points: i64,
}

impl Default for MinerAllocation {
    fn default() -> Self {
        Self {
            strength: 0,
            smarts: 0,
            stamina: 0,
            stat_points: DIG_STARTING_STAT_POINTS,
        }
    }
}

impl MinerAllocation {
    #[must_use]
    pub const fn spent_points(self) -> i64 {
        self.strength + self.smarts + self.stamina
    }

    #[must_use]
    pub const fn unspent_points(self) -> i64 {
        self.stat_points - self.spent_points()
    }

    pub fn allocate(&mut self, strength: i64, smarts: i64, stamina: i64) -> Result<(), String> {
        if strength < 0 || smarts < 0 || stamina < 0 {
            return Err("S stats cannot be negative.".to_owned());
        }
        let spend = strength + smarts + stamina;
        if spend == 0 {
            return Err("Spend at least one point.".to_owned());
        }
        if spend > self.unspent_points() {
            return Err(format!(
                "That spends {spend} points, but you only have {} unspent.",
                self.unspent_points()
            ));
        }
        self.strength += strength;
        self.smarts += smarts;
        self.stamina += stamina;
        Ok(())
    }

    pub fn respec(&mut self, balance: &mut i64) -> Result<i64, String> {
        let returned = self.spent_points();
        if returned == 0 {
            return Err("You don't have any allocated S points to reset.".to_owned());
        }
        if *balance < MINER_RESPEC_COST {
            return Err("You need 50 JC to respec your S points.".to_owned());
        }
        *balance -= MINER_RESPEC_COST;
        self.strength = 0;
        self.smarts = 0;
        self.stamina = 0;
        Ok(returned)
    }
}

#[must_use]
pub fn sanitize_backstory(value: &str, max_chars: usize) -> String {
    value
        .replace('@', "(at)")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoBuyStatus {
    Purchased,
    QueuedFromInventory,
    SkippedInsufficientBalance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutoBuyResult {
    pub item: &'static str,
    pub cost: i64,
    pub status: AutoBuyStatus,
}

pub fn auto_buy_item(
    inventory_count: &mut i64,
    balance: &mut i64,
    item: &'static str,
    cost: i64,
) -> AutoBuyResult {
    if *inventory_count > 0 {
        *inventory_count -= 1;
        return AutoBuyResult {
            item,
            cost: 0,
            status: AutoBuyStatus::QueuedFromInventory,
        };
    }
    if *balance >= cost {
        *balance -= cost;
        AutoBuyResult {
            item,
            cost,
            status: AutoBuyStatus::Purchased,
        }
    } else {
        AutoBuyResult {
            item,
            cost,
            status: AutoBuyStatus::SkippedInsufficientBalance,
        }
    }
}

#[must_use]
pub fn dig_advance_cap(stats: MinerAllocation, dynamite: bool, depth_charge: bool) -> i64 {
    let stat_effects = miner_stat_effects(
        MinerStats::new(stats.strength, stats.smarts, stats.stamina)
            .expect("miner allocations are non-negative"),
    );
    if stat_effects.advance_min_bonus > 0
        || stat_effects.advance_max_bonus > 0
        || dynamite
        || depth_charge
    {
        BOOSTED_DIG_ADVANCE_CAP
    } else {
        MAIN_DIG_ADVANCE_CAP
    }
}

#[must_use]
pub fn capped_advance(raw: i64, stats: MinerAllocation, dynamite: bool, depth_charge: bool) -> i64 {
    raw.max(0)
        .min(dig_advance_cap(stats, dynamite, depth_charge))
}

#[must_use]
pub fn next_undefeated_boss(defeated: &BTreeSet<i64>) -> Option<i64> {
    BOSS_BOUNDARIES
        .into_iter()
        .chain([PINNACLE_DEPTH])
        .find(|boundary| !defeated.contains(boundary))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdvanceResult {
    pub advance: i64,
    pub depth_after: i64,
    pub boss_encounter: Option<i64>,
}

#[must_use]
pub fn apply_boss_gate(
    depth: i64,
    requested_advance: i64,
    defeated: &BTreeSet<i64>,
) -> AdvanceResult {
    let Some(boundary) = next_undefeated_boss(defeated) else {
        return AdvanceResult {
            advance: requested_advance.max(0),
            depth_after: depth + requested_advance.max(0),
            boss_encounter: None,
        };
    };
    if depth >= boundary - 1 {
        return AdvanceResult {
            advance: 0,
            depth_after: depth,
            boss_encounter: Some(boundary),
        };
    }
    if depth + requested_advance >= boundary {
        let advance = boundary - 1 - depth;
        return AdvanceResult {
            advance,
            depth_after: boundary - 1,
            boss_encounter: Some(boundary),
        };
    }
    AdvanceResult {
        advance: requested_advance.max(0),
        depth_after: depth + requested_advance.max(0),
        boss_encounter: None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TempBuff {
    pub id: &'static str,
    pub digs_remaining: i64,
    pub advance_bonus: i64,
    pub yield_percent: i64,
}

impl TempBuff {
    pub fn consume(&mut self) {
        self.digs_remaining = self.digs_remaining.saturating_sub(1);
    }

    #[must_use]
    pub const fn active(&self) -> bool {
        self.digs_remaining > 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelState {
    pub depth: i64,
    pub max_depth: i64,
    pub balance: i64,
    pub total_digs: i64,
    pub last_dig_at: Option<i64>,
    pub luminosity: i64,
    pub stats: MinerAllocation,
    pub paid_digs_today: usize,
    pub paid_dig_day: Option<i64>,
    pub queued_consumables: Vec<&'static str>,
    pub boss_preparation: Vec<&'static str>,
    pub artifacts: Vec<&'static str>,
    pub awarded_bosses: BTreeSet<i64>,
    pub defeated_bosses: BTreeSet<i64>,
    pub buff: Option<TempBuff>,
}

impl Default for TunnelState {
    fn default() -> Self {
        Self {
            depth: 0,
            max_depth: 0,
            balance: 100,
            total_digs: 0,
            last_dig_at: None,
            luminosity: 100,
            stats: MinerAllocation::default(),
            paid_digs_today: 0,
            paid_dig_day: None,
            queued_consumables: Vec::new(),
            boss_preparation: Vec::new(),
            artifacts: Vec::new(),
            awarded_bosses: BTreeSet::new(),
            defeated_bosses: BTreeSet::new(),
            buff: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigOutcomeInput {
    pub advance: i64,
    pub gross_jc: i64,
    /// Explicit cave-in discriminator.  A grappling hook can absorb all
    /// blocks while the Dig is still a cave-in (no advancement/milestones).
    pub cave_in: bool,
    pub cave_in_loss: i64,
    pub dynamite: bool,
    pub depth_charge: bool,
    /// A temporary yield buff scales both loot and the base-payout ceiling.
    pub yield_buff_percent: i64,
    /// Exact temporary yield-buff multiplier, when the caller has a
    /// fractional authored value such as `1.75`. `None` preserves the legacy
    /// integer-percent input above for source compatibility.
    pub yield_buff_multiplier_millionths: Option<i64>,
    /// A mana/weather combo scales loot but does not lift the ceiling.
    pub weather_yield_percent: i64,
    /// Exact composition of all non-buff yield factors. When present this
    /// supersedes `weather_yield_percent`; the two factors are composed with
    /// the temporary buff and truncated once at the base-roll boundary.
    pub yield_multiplier_millionths: Option<i64>,
    /// Flat weather/gear/mutation JC bonuses are applied before the base cap
    /// and minigame scale, matching the Python normal-Dig pipeline.
    pub flat_jc_bonus: i64,
    /// Fully composed base loot after yield factors, flat rewards,
    /// corruption/mutation, Mana variance, and Prospector's Streak. Live Dig
    /// supplies this because those stages share its request entropy; the pure
    /// fallback keeps composing the legacy fields above.
    pub pre_cap_jc: Option<i64>,
    pub overgrowth_bonus: i64,
    /// Persisted daily-economy multiplier represented in basis points.  The
    /// integer form keeps this policy input `Eq` while retaining Python's
    /// half-up reward rounding at the settlement boundary.
    pub economy_reward_multiplier_basis_points: i64,
    /// Cave/event branches apply the daily economy before the central
    /// positive-JC scale; ordinary Digs apply it after structural bonuses.
    pub economy_before_positive_scale: bool,
    /// Deployment minigame scale in millionths. Non-zero authored rewards
    /// retain Python's half-up rounding and minimum magnitude of one.
    pub minigame_jc_delta_scale_millionths: i64,
    /// Daily streak bonus is a separate bucket and is included in the
    /// economy-event basis for ordinary positive Digs only.
    pub streak_bonus: i64,
    /// Patient Step's multiplier for the daily streak bucket. Python floors
    /// this bucket before it joins the minigame scale.
    pub streak_bonus_multiplier_basis_points: i64,
    /// Ascension's milestone multiplier. Each crossed authored milestone is
    /// floored independently before the milestone buckets are summed.
    pub milestone_multiplier_basis_points: i64,
    /// Request-local Plains/Blue deduction after the daily economy event and
    /// before Helltide/profit policies. Plains is populated only when its
    /// separate nonprofit credit succeeded; Blue is a deflationary sink.
    pub mana_yield_tax: i64,
    pub helltide_tax: i64,
    pub authored_event: bool,
    /// Ordinary by default; the live adapter supplies the player's snapshot.
    pub profit_policy: DigProfitPolicy,
}

impl Default for DigOutcomeInput {
    fn default() -> Self {
        Self {
            advance: 1,
            gross_jc: 0,
            cave_in: false,
            cave_in_loss: 0,
            dynamite: false,
            depth_charge: false,
            yield_buff_percent: 100,
            yield_buff_multiplier_millionths: None,
            weather_yield_percent: 100,
            yield_multiplier_millionths: None,
            flat_jc_bonus: 0,
            pre_cap_jc: None,
            overgrowth_bonus: 0,
            economy_reward_multiplier_basis_points: 10_000,
            economy_before_positive_scale: false,
            minigame_jc_delta_scale_millionths: DIG_YIELD_MULTIPLIER_SCALE,
            streak_bonus: 0,
            streak_bonus_multiplier_basis_points: DIG_REWARD_BASIS_POINTS,
            milestone_multiplier_basis_points: DIG_REWARD_BASIS_POINTS,
            mana_yield_tax: 0,
            helltide_tax: 0,
            authored_event: false,
            profit_policy: DigProfitPolicy::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DigOutcome {
    pub advance: i64,
    pub depth_after: i64,
    pub max_depth_after: i64,
    pub gross_jc: i64,
    pub economy_gross_jc: i64,
    pub economy_adjusted_jc: i64,
    pub jc_earned: i64,
    pub milestone_bonus: i64,
    pub streak_bonus: i64,
    pub overgrowth_bonus: i64,
    pub mana_yield_tax: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub cave_in: bool,
    pub boss_encounter: Option<i64>,
}

pub fn apply_dig_outcome(state: &mut TunnelState, input: DigOutcomeInput, now: i64) -> DigOutcome {
    let old_max_depth = state.max_depth;
    let cave_in = input.cave_in || input.cave_in_loss > 0;
    let (advance, depth_after, boss_encounter) = if cave_in {
        (0, (state.depth - input.cave_in_loss).max(0), None)
    } else {
        let raw = if input.authored_event {
            input.advance.max(0)
        } else {
            capped_advance(
                input.advance,
                state.stats,
                input.dynamite,
                input.depth_charge,
            )
        };
        let gated = apply_boss_gate(state.depth, raw, &state.defeated_bosses);
        (gated.advance, gated.depth_after, gated.boss_encounter)
    };
    state.depth = depth_after;
    state.max_depth = state.max_depth.max(depth_after);
    state.total_digs += 1;
    state.last_dig_at = Some(now);

    let buff_multiplier = effective_yield_multiplier(
        input.yield_buff_multiplier_millionths,
        input.yield_buff_percent,
    );
    let yield_multiplier = effective_yield_multiplier(
        input.yield_multiplier_millionths,
        input.weather_yield_percent,
    );
    // Python composes the weather/relic/ascension factors and the temporary
    // buff before its one `int(...)` conversion. Keep that truncation at this
    // boundary instead of truncating each factor in sequence.
    let adjusted_base = input.pre_cap_jc.map_or_else(
        || {
            scale_dig_yield_once(input.gross_jc.max(0), &[yield_multiplier, buff_multiplier])
                .saturating_add(input.flat_jc_bonus)
                .max(0)
        },
        |pre_cap_jc| pre_cap_jc.max(0),
    );
    let lifted_cap = scale_dig_yield_once(BASE_DIG_JC_PAYOUT_CAP, &[buff_multiplier]);
    let capped_base = adjusted_base.min(lifted_cap);
    let milestone_bonus = if cave_in {
        0
    } else {
        let multiplier = input.milestone_multiplier_basis_points.clamp(0, i64::MAX);
        MILESTONES
            .iter()
            .filter(|(depth, _)| old_max_depth < *depth && state.max_depth >= *depth)
            .map(|(_, bonus)| scale_dig_reward_basis_points(*bonus, multiplier))
            .sum()
    };
    let apply_economy = |amount: i64| {
        if amount <= 0 {
            0
        } else {
            // Daily economy uses half-up rounding on an already integer
            // bucket, unlike the floor used for authored yield/perk factors.
            let numerator = i128::from(amount).saturating_mul(i128::from(
                input.economy_reward_multiplier_basis_points.max(0),
            ));
            ((numerator + i128::from(DIG_REWARD_BASIS_POINTS / 2))
                / i128::from(DIG_REWARD_BASIS_POINTS))
            .min(i128::from(i64::MAX)) as i64
        }
    };
    let streak_bonus = if cave_in {
        0
    } else {
        scale_dig_reward_basis_points(
            input.streak_bonus.max(0),
            input
                .streak_bonus_multiplier_basis_points
                .clamp(0, i64::MAX),
        )
        .min(DIG_STREAK_JC_PAYOUT_CAP)
    };
    // Normal Dig structural additions (milestone and streak) are part of the
    // minigame basis. Cave/event branches intentionally leave those buckets at
    // zero and apply the economy event before the central positive scale.
    let structural_base = capped_base.saturating_add(if input.economy_before_positive_scale {
        0
    } else {
        milestone_bonus.saturating_add(streak_bonus)
    });
    let gross_jc = scale_dig_minigame_jc(
        structural_base.max(0),
        input.minigame_jc_delta_scale_millionths,
    );
    let scaled_base = if input.economy_before_positive_scale {
        scale_positive_dig_jc(apply_economy(gross_jc))
    } else {
        scale_positive_dig_jc(gross_jc)
    };
    let tax = if cave_in {
        0
    } else {
        scale_dig_helltide_tax(input.helltide_tax)
    };
    let overgrowth_bonus = if cave_in {
        0
    } else {
        input.overgrowth_bonus.max(0)
    };
    let economy_gross_jc = if input.economy_before_positive_scale {
        gross_jc
    } else {
        (scaled_base + overgrowth_bonus).max(0)
    };
    let economy_adjusted_jc = if input.economy_before_positive_scale {
        apply_economy(gross_jc)
    } else {
        apply_economy(economy_gross_jc)
    };
    let positive_jc = if input.economy_before_positive_scale {
        scaled_base
    } else {
        economy_adjusted_jc
    };
    let mana_yield_tax = input.mana_yield_tax.max(0).min(positive_jc);
    let pre_policy_jc = positive_jc
        .saturating_sub(mana_yield_tax)
        .saturating_sub(tax)
        .max(0);
    let profit = apply_jc_profit_policies(pre_policy_jc, input.profit_policy);
    let jc_earned = profit.net;
    state.balance += jc_earned;
    if let Some(buff) = state.buff.as_mut() {
        buff.consume();
        if !buff.active() {
            state.buff = None;
        }
    }

    DigOutcome {
        advance,
        depth_after,
        max_depth_after: state.max_depth,
        gross_jc,
        economy_gross_jc,
        economy_adjusted_jc,
        jc_earned,
        milestone_bonus,
        streak_bonus,
        overgrowth_bonus,
        mana_yield_tax,
        bankruptcy_penalty: profit.bankruptcy_penalty,
        vanity_tax: profit.vanity_tax,
        cave_in,
        boss_encounter,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirstDigOutcome {
    pub is_first_dig: bool,
    pub dig_consumed: bool,
    pub cave_in: bool,
    pub advance: i64,
    pub jc_earned: i64,
}

pub fn apply_first_dig(
    state: &mut TunnelState,
    advance_roll: i64,
    jc_roll: i64,
    daily_adjusted_jc: i64,
    now: i64,
) -> FirstDigOutcome {
    let advance = advance_roll.clamp(FIRST_DIG_ADVANCE_RANGE.0, FIRST_DIG_ADVANCE_RANGE.1);
    let _gross = jc_roll.clamp(FIRST_DIG_JC_RANGE.0, FIRST_DIG_JC_RANGE.1);
    let jc_earned = scale_positive_dig_jc(daily_adjusted_jc.max(0));
    state.depth = advance;
    state.max_depth = advance;
    state.total_digs = 1;
    state.last_dig_at = Some(now);
    state.balance += jc_earned;
    FirstDigOutcome {
        is_first_dig: true,
        dig_consumed: false,
        cave_in: false,
        advance,
        jc_earned,
    }
}

pub fn atomic_state_update<T, E>(
    state: &mut TunnelState,
    operation: impl FnOnce(&mut TunnelState) -> Result<T, E>,
) -> Result<T, E> {
    let mut staged = state.clone();
    let result = operation(&mut staged)?;
    *state = staged;
    Ok(result)
}

#[must_use]
pub fn cave_in_loss(depth: i64, rolled_loss: i64, reinforcement_active: bool) -> i64 {
    let (minimum, maximum) = CAVE_IN_BLOCK_LOSS_RANGES[cave_in_band(depth)];
    let loss = rolled_loss.clamp(i64::from(minimum), i64::from(maximum));
    if reinforcement_active {
        loss.min(8)
    } else {
        loss
    }
}

#[must_use]
pub fn cave_in_medical_bill(depth: i64) -> i64 {
    (depth / 10).max(1)
}

#[must_use]
pub fn milestone_bonus(old_max_depth: i64, new_depth: i64) -> i64 {
    MILESTONES
        .iter()
        .filter(|(depth, _)| old_max_depth < *depth && new_depth >= *depth)
        .map(|(_, bonus)| *bonus)
        .sum()
}

#[must_use]
pub fn prestige_layer_multiplier(prestige: i64) -> f64 {
    match prestige {
        i64::MIN..=0 => 1.0,
        1 => 1.18,
        2 => 1.15,
        3 => 1.13,
        4 => 1.08,
        _ => 1.01,
    }
}

#[must_use]
pub fn help_advance(depth: i64, rolled_advance: i64, defeated: &BTreeSet<i64>) -> AdvanceResult {
    apply_boss_gate(depth, rolled_advance, defeated)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SabotageInput {
    pub actor_id: i64,
    pub target_id: i64,
    pub target_depth: i64,
    pub actor_balance: i64,
    pub rolled_damage: i64,
    pub hit: bool,
    pub trapped: bool,
    pub insured: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SabotageOutcome {
    pub cost: i64,
    pub damage: i64,
    pub attacker_block_reward: i64,
    pub actor_balance_after: i64,
    pub target_depth_after: i64,
    pub victim_credit: i64,
    pub trapped: bool,
    pub insurance_applied: bool,
}

pub fn resolve_sabotage(input: SabotageInput) -> Result<SabotageOutcome, &'static str> {
    if input.actor_id == input.target_id {
        return Err("You can't sabotage yourself.");
    }
    let cost = SABOTAGE_BASE_COST.max(input.target_depth / SABOTAGE_COST_DIVISOR);
    if input.actor_balance < cost {
        return Err("Insufficient balance for sabotage.");
    }
    if input.trapped {
        return Ok(SabotageOutcome {
            cost,
            damage: 0,
            attacker_block_reward: 0,
            actor_balance_after: input.actor_balance - cost - cost.min(10),
            target_depth_after: input.target_depth,
            victim_credit: cost,
            trapped: true,
            insurance_applied: false,
        });
    }
    if !input.hit {
        return Ok(SabotageOutcome {
            cost,
            damage: 0,
            attacker_block_reward: 0,
            actor_balance_after: input.actor_balance - cost,
            target_depth_after: input.target_depth,
            victim_credit: 0,
            trapped: false,
            insurance_applied: false,
        });
    }
    let rolled = input
        .rolled_damage
        .clamp(SABOTAGE_DAMAGE_RANGE.0, SABOTAGE_DAMAGE_RANGE.1);
    let damage = if input.insured {
        (rolled / 2).max(1)
    } else {
        rolled
    };
    let attacker_block_reward = match input.target_depth {
        ..100 => 3,
        100..250 => 5,
        _ => 7,
    };
    Ok(SabotageOutcome {
        cost,
        damage,
        attacker_block_reward,
        actor_balance_after: input.actor_balance - cost,
        target_depth_after: (input.target_depth - damage).max(0),
        victim_credit: 0,
        trapped: false,
        insurance_applied: input.insured,
    })
}

/// Total victim credit for a trap-blocked sabotage after the daily economy
/// event has adjusted the authored tip. The sabotage cost is transferred
/// separately from the newly generated, centrally scaled tip.
#[must_use]
pub fn blocked_sabotage_victim_credit(cost: i64, adjusted_tip_gross: i64) -> i64 {
    cost.max(0) + scale_positive_dig_jc(adjusted_tip_gross.max(0))
}

#[must_use]
pub fn insurance_cost(depth: i64) -> i64 {
    INSURANCE_BASE_COST + depth.max(0) / INSURANCE_COST_DEPTH_DIVISOR
}

#[must_use]
pub const fn insurance_active(insured_until: i64, now: i64) -> bool {
    insured_until > now
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PredictionPosition {
    pub contracts: i64,
    pub cost_basis: i64,
}

pub fn steal_prediction_contracts(
    victim: &mut PredictionPosition,
    attacker: &mut PredictionPosition,
    requested: i64,
) -> i64 {
    let stolen = requested.max(0).min(victim.contracts);
    if stolen == 0 || victim.contracts == 0 {
        return 0;
    }
    let basis = victim.cost_basis * stolen / victim.contracts;
    victim.contracts -= stolen;
    victim.cost_basis -= basis;
    attacker.contracts += stolen;
    attacker.cost_basis += basis;
    stolen
}

#[must_use]
pub fn boss_base_reward(boundary: i64, prestige_level: i64) -> i64 {
    let base = match boundary {
        25 => 15,
        50 => 20,
        75 => 25,
        100 => 30,
        150 => 40,
        200 => 50,
        275 => 65,
        _ => 15,
    };
    let multiplier = 1.0 + prestige_level.max(0) as f64 * 0.125;
    (base as f64 * multiplier) as i64
}

pub fn validate_boss_wager(
    wager: i64,
    balance: i64,
    final_phase: bool,
) -> Result<(), &'static str> {
    if wager > BOSS_WAGER_CAP {
        return Err("Boss wagers cannot exceed 1,000 JC.");
    }
    if wager > 0 && !final_phase {
        return Err("Boss wagers are only accepted in the final phase.");
    }
    if wager > balance {
        return Err("Insufficient balance for that boss wager.");
    }
    Ok(())
}

/// Debit for a lost regular boss phase. Mandatory no-wager phases are free;
/// choosing a zero wager in a wager-eligible phase incurs the repair bill.
#[must_use]
pub const fn boss_loss_debit(wager: i64, forced_no_wager_phase: bool) -> i64 {
    if wager > 0 {
        wager
    } else if forced_no_wager_phase {
        0
    } else {
        BOSS_LOSS_REPAIR_BILL
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BossResolution {
    pub won: bool,
    pub balance_after: i64,
    pub depth_after: i64,
    pub payout: i64,
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub knockback: i64,
}

#[must_use]
pub fn resolve_boss(
    depth: i64,
    balance: i64,
    wager: i64,
    won: bool,
    base_reward: i64,
    wager_profit: i64,
    knockback_roll: i64,
) -> BossResolution {
    resolve_boss_with_policy(
        depth,
        balance,
        wager,
        won,
        base_reward,
        wager_profit,
        knockback_roll,
        DigProfitPolicy::default(),
    )
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn resolve_boss_with_policy(
    depth: i64,
    balance: i64,
    wager: i64,
    won: bool,
    base_reward: i64,
    wager_profit: i64,
    knockback_roll: i64,
    profit_policy: DigProfitPolicy,
) -> BossResolution {
    if won {
        let gross_payout = scale_positive_dig_jc(base_reward) + wager_profit.max(0);
        let profit = apply_jc_profit_policies(gross_payout, profit_policy);
        BossResolution {
            won: true,
            balance_after: balance + profit.net,
            depth_after: depth + 2,
            payout: profit.net,
            bankruptcy_penalty: profit.bankruptcy_penalty,
            vanity_tax: profit.vanity_tax,
            knockback: 0,
        }
    } else {
        let knockback = knockback_roll.clamp(11, 20);
        BossResolution {
            won: false,
            balance_after: balance - wager.max(0),
            depth_after: (depth - knockback).max(0),
            payout: 0,
            bankruptcy_penalty: 0,
            vanity_tax: 0,
            knockback,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreakOutcome {
    pub days: i64,
    pub charm_used: bool,
}

#[must_use]
pub fn resolve_streak(
    existing_days: i64,
    last_day: Option<i64>,
    today: i64,
    has_charm: bool,
) -> StreakOutcome {
    match last_day {
        Some(last) if last == today => StreakOutcome {
            days: existing_days,
            charm_used: false,
        },
        Some(last) if last == today - 1 => StreakOutcome {
            days: existing_days + 1,
            charm_used: false,
        },
        Some(last) if last == today - 2 && existing_days > 0 && has_charm => StreakOutcome {
            days: existing_days + 1,
            charm_used: true,
        },
        _ => StreakOutcome {
            days: 1,
            charm_used: false,
        },
    }
}

#[must_use]
pub fn streak_bonus(days: i64) -> i64 {
    STREAKS
        .iter()
        .filter(|(threshold, _)| days >= *threshold)
        .map(|(_, bonus)| *bonus)
        .max()
        .unwrap_or(0)
        .min(DIG_STREAK_JC_PAYOUT_CAP)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LuminosityLevel {
    Bright,
    Dim,
    Dark,
    PitchBlack,
}

#[must_use]
pub fn luminosity_level(luminosity: i64) -> LuminosityLevel {
    match luminosity.clamp(0, 100) {
        76.. => LuminosityLevel::Bright,
        26..=75 => LuminosityLevel::Dim,
        1..=25 => LuminosityLevel::Dark,
        _ => LuminosityLevel::PitchBlack,
    }
}

#[must_use]
pub fn luminosity_cave_in_bonus(luminosity: i64) -> f64 {
    match luminosity_level(luminosity) {
        LuminosityLevel::Bright => 0.0,
        LuminosityLevel::Dim => 0.08,
        LuminosityLevel::Dark => 0.20,
        LuminosityLevel::PitchBlack => 0.35,
    }
}

#[must_use]
pub fn event_chance(base: f64, luminosity: i64) -> f64 {
    let multiplier = match luminosity_level(luminosity) {
        LuminosityLevel::Bright => 1.0,
        LuminosityLevel::Dim => 1.5,
        LuminosityLevel::Dark => 2.5,
        LuminosityLevel::PitchBlack => 3.0,
    };
    (base * multiplier).min(0.75)
}

#[must_use]
pub fn risky_success_chance(authored: f64, luminosity: i64) -> f64 {
    let penalty = match luminosity_level(luminosity) {
        LuminosityLevel::Bright => 0.0,
        LuminosityLevel::Dim => 0.05,
        LuminosityLevel::Dark => 0.15,
        LuminosityLevel::PitchBlack => 0.25,
    };
    (authored - penalty).max(0.05)
}

#[must_use]
pub fn resolved_event_choice(requested: &'static str, luminosity: i64) -> &'static str {
    if luminosity <= 0 && requested == "safe" {
        "risky"
    } else {
        requested
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cheer {
    pub cheerer_id: i64,
    pub created_at: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CheerBook {
    cheers: Vec<Cheer>,
    last_cheer_by_user: BTreeMap<i64, i64>,
}

impl CheerBook {
    pub fn cheer(&mut self, cheerer_id: i64, target_id: i64, now: i64) -> Result<(), &'static str> {
        if cheerer_id == target_id {
            return Err("You cannot cheer for yourself.");
        }
        if self
            .last_cheer_by_user
            .get(&cheerer_id)
            .is_some_and(|last| now - *last < CHEER_COOLDOWN_SECONDS)
        {
            return Err("Cheer is on cooldown.");
        }
        self.cheers
            .retain(|cheer| now - cheer.created_at <= CHEER_DURATION_SECONDS);
        if self.cheers.len() >= 3 {
            return Err("The target already has the maximum cheer boost.");
        }
        self.cheers.push(Cheer {
            cheerer_id,
            created_at: now,
        });
        self.last_cheer_by_user.insert(cheerer_id, now);
        Ok(())
    }

    #[must_use]
    pub fn active_count(&self, now: i64) -> usize {
        self.cheers
            .iter()
            .filter(|cheer| now - cheer.created_at <= CHEER_DURATION_SECONDS)
            .count()
    }

    #[must_use]
    pub fn hit_bonus(&self, now: i64) -> f64 {
        self.active_count(now) as f64 * 0.05
    }
}

#[cfg(test)]
mod tests;
