//! Wheel of Fortune policy shared by the runtime betting provider: spin
//! windows and cooldowns, wedge catalogs and calibration, wheel selection,
//! explosion rewards, and canonical outcome codes. The live spin mechanics
//! and settlement are implemented by the runtime provider.

pub use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::economy_scaling::scale_minigame_jc_delta;
use cama_domain::game_date::{GAME_DAY_SECONDS, game_day_start_ts, next_game_day_start_ts};

use crate::economy_actions::apply_gamba_event_multiplier;

pub const WHEEL_LOSE_PENALTY_DAYS: i64 = 5;
pub const WHEEL_EXPLOSION_REWARD: i64 = 67;
pub const WHEEL_EXPLOSION_CHANCE: f64 = 0.01;
pub const WHEEL_GOLDEN_TOP_N: usize = 3;
pub const HOSTILE_LOSS_MIN_BALANCE: i64 = 50;
pub const WHEEL_BANANA_PEEL_LOSS_MIN: i64 = 15;
pub const WHEEL_BANANA_PEEL_LOSS_MAX: i64 = 32;
pub const WHEEL_GREEN_SHELL_STEAL_MIN: i64 = 15;
pub const WHEEL_GREEN_SHELL_STEAL_MAX: i64 = 33;
pub const WHEEL_BOMB_OMB_VICTIM_LOSS_MIN: i64 = 10;
pub const WHEEL_BOMB_OMB_VICTIM_LOSS_MAX: i64 = 25;
pub const WHEEL_BOMB_OMB_VICTIM_COUNT: usize = 3;

/// Start of the wheel window containing `timestamp`: 4 AM in fixed UTC-8.
///
/// Runtime and persisted timestamps are expected to be ordinary Unix times.
/// The fallback fails closed for an out-of-range value without panicking on a
/// corrupt database timestamp.
#[must_use]
pub fn wheel_window_start_at(timestamp: i64) -> i64 {
    game_day_start_ts(timestamp).unwrap_or(timestamp)
}

/// First fixed daily reset strictly after the window containing `timestamp`.
#[must_use]
pub fn next_wheel_reset_at(timestamp: i64) -> i64 {
    next_game_day_start_ts(timestamp).unwrap_or_else(|_| timestamp.saturating_add(GAME_DAY_SECONDS))
}

/// Whether a regular spin may be claimed in the current fixed daily window.
#[must_use]
pub fn wheel_spin_is_available(now: i64, last_spin: Option<i64>) -> bool {
    last_spin.is_none_or(|last| last < wheel_window_start_at(now))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelKind {
    Regular,
    Bankrupt,
    Golden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WheelMechanic {
    RedShell,
    BlueShell,
    LightningBolt,
    BananaPeel,
    GreenShell,
    BombOmb,
    ExtendOne,
    ExtendTwo,
    Jailbreak,
    ChainReaction,
    TownTrial,
    Discover,
    Emergency,
    Commune,
    Comeback,
    Heist,
    MarketCrash,
    CompoundInterest,
    TrickleDown,
    Dividend,
    HostileTakeover,
    Recession,
    Eruption,
    Overgrowth,
    Decay,
}

impl WheelMechanic {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RedShell => "RED_SHELL",
            Self::BlueShell => "BLUE_SHELL",
            Self::LightningBolt => "LIGHTNING_BOLT",
            Self::BananaPeel => "BANANA_PEEL",
            Self::GreenShell => "GREEN_SHELL",
            Self::BombOmb => "BOMB_OMB",
            Self::ExtendOne => "EXTEND_1",
            Self::ExtendTwo => "EXTEND_2",
            Self::Jailbreak => "JAILBREAK",
            Self::ChainReaction => "CHAIN_REACTION",
            Self::TownTrial => "TOWN_TRIAL",
            Self::Discover => "DISCOVER",
            Self::Emergency => "EMERGENCY",
            Self::Commune => "COMMUNE",
            Self::Comeback => "COMEBACK",
            Self::Heist => "HEIST",
            Self::MarketCrash => "MARKET_CRASH",
            Self::CompoundInterest => "COMPOUND_INTEREST",
            Self::TrickleDown => "TRICKLE_DOWN",
            Self::Dividend => "DIVIDEND",
            Self::HostileTakeover => "HOSTILE_TAKEOVER",
            Self::Recession => "RECESSION",
            Self::Eruption => "ERUPTION",
            Self::Overgrowth => "OVERGROWTH",
            Self::Decay => "DECAY",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelValue {
    Numeric(i64),
    Mechanic(WheelMechanic),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WheelWedge {
    pub label: String,
    pub value: WheelValue,
    pub color: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelEconomyPolicy {
    pub minigame_scale: f64,
    pub regular_target_ev: f64,
    pub bankrupt_target_ev: f64,
    pub golden_target_ev: f64,
}

impl Default for WheelEconomyPolicy {
    fn default() -> Self {
        Self {
            minigame_scale: 1.0,
            regular_target_ev: -27.5,
            bankrupt_target_ev: 12.0,
            golden_target_ev: -82.5,
        }
    }
}

#[derive(Clone, Copy)]
enum BaseValue {
    Numeric(i64),
    Mechanic(WheelMechanic),
}

#[derive(Clone, Copy)]
struct BaseWedge {
    label: &'static str,
    value: BaseValue,
    color: &'static str,
}

const fn numeric(label: &'static str, value: i64, color: &'static str) -> BaseWedge {
    BaseWedge {
        label,
        value: BaseValue::Numeric(value),
        color,
    }
}

const fn mechanic(label: &'static str, value: WheelMechanic, color: &'static str) -> BaseWedge {
    BaseWedge {
        label,
        value: BaseValue::Mechanic(value),
        color,
    }
}

const REGULAR_BASE: [BaseWedge; 24] = [
    mechanic("BOMB", WheelMechanic::BombOmb, "#0d0d0d"),
    numeric("BANKRUPT", -100, "#1a1a1a"),
    numeric("BANKRUPT", -100, "#1a1a1a"),
    mechanic("GREEN", WheelMechanic::GreenShell, "#228b22"),
    numeric("30", 30, "#2980b9"),
    numeric("5", 5, "#2d5a27"),
    numeric("25", 25, "#3498db"),
    mechanic("BLUE", WheelMechanic::BlueShell, "#3498db"),
    numeric("10", 10, "#3d7a37"),
    numeric("10", 10, "#3d7a37"),
    numeric("LOSE", 0, "#4a4a4a"),
    numeric("15", 15, "#4d9a47"),
    numeric("20", 20, "#5dba57"),
    numeric("50", 50, "#7d3c98"),
    numeric("50", 50, "#7d3c98"),
    numeric("40", 40, "#9b59b6"),
    numeric("80", 80, "#c0392b"),
    numeric("70", 70, "#d35400"),
    numeric("60", 60, "#e67e22"),
    mechanic("RED", WheelMechanic::RedShell, "#e74c3c"),
    numeric("100", 100, "#f1c40f"),
    numeric("100", 100, "#f1c40f"),
    mechanic("BOLT", WheelMechanic::LightningBolt, "#f39c12"),
    mechanic("BANANA", WheelMechanic::BananaPeel, "#ffe14a"),
];

const BANKRUPT_BASE: [BaseWedge; 24] = [
    mechanic("CLUTCH", WheelMechanic::Comeback, "#0a1a2a"),
    mechanic("JAIL", WheelMechanic::Jailbreak, "#0a2a0a"),
    mechanic("BOMB", WheelMechanic::BombOmb, "#0d0d0d"),
    numeric("BANKRUPT", -100, "#1a1a1a"),
    numeric("BANKRUPT", -100, "#1a1a1a"),
    mechanic("CHAIN", WheelMechanic::ChainReaction, "#1a1a3a"),
    mechanic("SEIZE", WheelMechanic::Commune, "#1a2a1a"),
    mechanic("FIND", WheelMechanic::Discover, "#1a2a2a"),
    mechanic("GREEN", WheelMechanic::GreenShell, "#228b22"),
    mechanic("SOS", WheelMechanic::Emergency, "#2a1a00"),
    mechanic("TRIAL", WheelMechanic::TownTrial, "#2a1a1a"),
    numeric("30", 30, "#2d5a27"),
    mechanic("BLUE", WheelMechanic::BlueShell, "#3498db"),
    numeric("50", 50, "#3a3a1a"),
    numeric("50", 50, "#3d7a37"),
    numeric("LOSE", 0, "#4a4a4a"),
    numeric("75", 75, "#4d9a47"),
    mechanic("+2", WheelMechanic::ExtendTwo, "#660000"),
    mechanic("+1", WheelMechanic::ExtendOne, "#8b0000"),
    mechanic("RED", WheelMechanic::RedShell, "#e74c3c"),
    numeric("100", 100, "#f1c40f"),
    numeric("100", 100, "#f1c40f"),
    mechanic("BOLT", WheelMechanic::LightningBolt, "#f39c12"),
    mechanic("BANANA", WheelMechanic::BananaPeel, "#ffe14a"),
];

const GOLDEN_BASE: [BaseWedge; 24] = [
    mechanic("BOMB", WheelMechanic::BombOmb, "#0d0d0d"),
    mechanic("GREEN", WheelMechanic::GreenShell, "#228b22"),
    mechanic("RECESSION", WheelMechanic::Recession, "#3a0a0a"),
    mechanic("BLUE", WheelMechanic::BlueShell, "#4080c0"),
    numeric("OVEREXTENDED", -398, "#4a3000"),
    numeric("OVEREXTENDED", -398, "#4a3000"),
    mechanic("DIVIDEND", WheelMechanic::Dividend, "#4a7000"),
    mechanic("TRICKLE", WheelMechanic::TrickleDown, "#5c7a00"),
    mechanic("TAKEOVER", WheelMechanic::HostileTakeover, "#6a2a80"),
    mechanic("COMPOUND", WheelMechanic::CompoundInterest, "#6b8c00"),
    mechanic("HEIST", WheelMechanic::Heist, "#7a5c00"),
    mechanic("HEIST", WheelMechanic::Heist, "#7a5c00"),
    mechanic("CRASH", WheelMechanic::MarketCrash, "#8a4000"),
    numeric("20", 20, "#b8860b"),
    numeric("30", 30, "#c8a000"),
    mechanic("RED", WheelMechanic::RedShell, "#cc6600"),
    numeric("50", 50, "#d4a000"),
    numeric("40", 40, "#daa520"),
    numeric("60", 60, "#e8b400"),
    numeric("150", 150, "#e8d080"),
    numeric("80", 80, "#f0c000"),
    numeric("100", 100, "#f5c500"),
    mechanic("BANANA", WheelMechanic::BananaPeel, "#ffe14a"),
    numeric("CROWN", 250, "#fffacd"),
];

fn estimated_ev(kind: WheelKind, mechanic: WheelMechanic) -> f64 {
    match kind {
        WheelKind::Regular => match mechanic {
            WheelMechanic::RedShell | WheelMechanic::GreenShell => 0.0,
            WheelMechanic::BlueShell => -4.0,
            WheelMechanic::LightningBolt => -65.0,
            WheelMechanic::BananaPeel => -23.5,
            WheelMechanic::BombOmb => -52.5,
            _ => 0.0,
        },
        WheelKind::Bankrupt => match mechanic {
            WheelMechanic::Comeback => 15.0,
            WheelMechanic::Jailbreak => 10.0,
            WheelMechanic::BombOmb => -52.5,
            WheelMechanic::ChainReaction => -25.0,
            WheelMechanic::Commune => 8.0,
            WheelMechanic::Discover => 5.0,
            WheelMechanic::GreenShell
            | WheelMechanic::Emergency
            | WheelMechanic::TownTrial
            | WheelMechanic::LightningBolt => 0.0,
            WheelMechanic::ExtendTwo => -20.0,
            WheelMechanic::ExtendOne => -10.0,
            WheelMechanic::RedShell => 5.0,
            WheelMechanic::BananaPeel => -23.5,
            WheelMechanic::BlueShell => 8.0,
            _ => 0.0,
        },
        WheelKind::Golden => match mechanic {
            WheelMechanic::RedShell | WheelMechanic::GreenShell => 0.0,
            WheelMechanic::BlueShell => -4.0,
            WheelMechanic::Heist => 33.0,
            WheelMechanic::MarketCrash | WheelMechanic::HostileTakeover => 35.0,
            WheelMechanic::CompoundInterest => 100.0,
            WheelMechanic::TrickleDown => 65.0,
            WheelMechanic::Dividend => 10.0,
            WheelMechanic::Recession => -200.0,
            WheelMechanic::BananaPeel => -23.5,
            WheelMechanic::BombOmb => -52.5,
            _ => 0.0,
        },
    }
}

fn base_for(kind: WheelKind) -> &'static [BaseWedge; 24] {
    match kind {
        WheelKind::Regular => &REGULAR_BASE,
        WheelKind::Bankrupt => &BANKRUPT_BASE,
        WheelKind::Golden => &GOLDEN_BASE,
    }
}

fn target_ev(kind: WheelKind, policy: WheelEconomyPolicy) -> f64 {
    match kind {
        WheelKind::Regular => policy.regular_target_ev,
        WheelKind::Bankrupt => policy.bankrupt_target_ev,
        WheelKind::Golden => policy.golden_target_ev,
    }
}

fn hue_sort_key(color: &str) -> (i32, f64) {
    let raw = color.strip_prefix('#').unwrap_or(color);
    let component = |offset: usize| {
        u8::from_str_radix(raw.get(offset..offset + 2).unwrap_or("00"), 16).unwrap_or(0) as f64
            / 255.0
    };
    let (red, green, blue) = (component(0), component(2), component(4));
    let maximum = red.max(green).max(blue);
    let minimum = red.min(green).min(blue);
    let delta = maximum - minimum;
    let hue = if delta == 0.0 {
        0.0
    } else if maximum == red {
        ((green - blue) / delta).rem_euclid(6.0) / 6.0
    } else if maximum == green {
        (((blue - red) / delta) + 2.0) / 6.0
    } else {
        (((red - green) / delta) + 4.0) / 6.0
    };
    (((hue * 12.0).round() as i32).rem_euclid(12), maximum)
}

/// Build one 24-slice catalog and calibrate its two negative numeric wedges.
#[must_use]
pub fn wheel_catalog(kind: WheelKind, policy: WheelEconomyPolicy) -> Vec<WheelWedge> {
    let base = base_for(kind);
    let non_negative_sum = base
        .iter()
        .filter_map(|wedge| match wedge.value {
            BaseValue::Numeric(value) if value >= 0 => {
                Some(scale_minigame_jc_delta(value as f64, policy.minigame_scale))
            }
            _ => None,
        })
        .sum::<i64>();
    let special_sum = base
        .iter()
        .filter_map(|wedge| match wedge.value {
            BaseValue::Mechanic(value) => Some(scale_minigame_jc_delta(
                estimated_ev(kind, value),
                policy.minigame_scale,
            )),
            BaseValue::Numeric(_) => None,
        })
        .sum::<i64>();
    let negative_count = base
        .iter()
        .filter(|wedge| matches!(wedge.value, BaseValue::Numeric(value) if value < 0))
        .count() as i64;
    let target_sum = scale_minigame_jc_delta(target_ev(kind, policy), policy.minigame_scale)
        * i64::try_from(base.len()).expect("wheel length fits i64");
    let calibrated_loss = if negative_count == 0 {
        -1
    } else {
        ((target_sum - non_negative_sum - special_sum) / negative_count).min(-1)
    };

    let mut wedges = base
        .iter()
        .map(|wedge| match wedge.value {
            BaseValue::Numeric(value) => {
                let resolved = if value < 0 {
                    calibrated_loss
                } else {
                    scale_minigame_jc_delta(value as f64, policy.minigame_scale)
                };
                WheelWedge {
                    label: resolved.to_string(),
                    value: WheelValue::Numeric(resolved),
                    color: wedge.color,
                }
            }
            BaseValue::Mechanic(value) => WheelWedge {
                label: wedge.label.to_owned(),
                value: WheelValue::Mechanic(value),
                color: wedge.color,
            },
        })
        .collect::<Vec<_>>();
    wedges.sort_by(|left, right| {
        let left_key = hue_sort_key(left.color);
        let right_key = hue_sort_key(right.color);
        left_key
            .0
            .cmp(&right_key.0)
            .then_with(|| left_key.1.total_cmp(&right_key.1))
    });
    wedges
}

#[cfg(test)]
fn wheel_expected_value(kind: WheelKind, wedges: &[WheelWedge], policy: WheelEconomyPolicy) -> f64 {
    let total = wedges
        .iter()
        .map(|wedge| match wedge.value {
            WheelValue::Numeric(value) => value,
            WheelValue::Mechanic(value) => {
                scale_minigame_jc_delta(estimated_ev(kind, value), policy.minigame_scale)
            }
        })
        .sum::<i64>();
    total as f64 / wedges.len() as f64
}

/// Golden wins over bankrupt only for direct callers; live selection never
/// requests both.  This matches Python's `get_wheel_wedges` precedence.
#[must_use]
pub fn get_wheel_wedges(
    is_bankrupt: bool,
    is_golden: bool,
    policy: WheelEconomyPolicy,
) -> Vec<WheelWedge> {
    let kind = if is_golden {
        WheelKind::Golden
    } else if is_bankrupt {
        WheelKind::Bankrupt
    } else {
        WheelKind::Regular
    };
    wheel_catalog(kind, policy)
}

#[must_use]
pub fn select_wheel_kind(
    spinner_id: i64,
    balance: i64,
    bonus_spin: bool,
    visible_top_ids: &[i64],
) -> WheelKind {
    if bonus_spin {
        WheelKind::Regular
    } else if balance < 0 {
        WheelKind::Bankrupt
    } else if visible_top_ids
        .iter()
        .take(WHEEL_GOLDEN_TOP_N)
        .any(|candidate| *candidate == spinner_id)
    {
        WheelKind::Golden
    } else {
        WheelKind::Regular
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CooldownOutcome {
    LoseTurn,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CooldownPlan {
    pub next_spin_at: i64,
    pub replacement_last_spin: Option<i64>,
    pub schedule_reminder: bool,
}

#[must_use]
pub fn cooldown_after_resolution(
    now: i64,
    bonus_spin: bool,
    last_regular_spin: Option<i64>,
    outcome: CooldownOutcome,
) -> CooldownPlan {
    if bonus_spin {
        return CooldownPlan {
            next_spin_at: last_regular_spin.map_or(now, |last| now.max(next_wheel_reset_at(last))),
            replacement_last_spin: None,
            schedule_reminder: false,
        };
    }
    if outcome == CooldownOutcome::LoseTurn {
        let next_spin_at = wheel_window_start_at(now)
            .saturating_add(WHEEL_LOSE_PENALTY_DAYS.saturating_mul(GAME_DAY_SECONDS));
        return CooldownPlan {
            next_spin_at,
            // `last_wheel_spin` remains the admission anchor. Placing the
            // synthetic anchor immediately before the target window keeps the
            // five-day penalty aligned to the same fixed daily reset.
            replacement_last_spin: Some(next_spin_at.saturating_sub(1)),
            schedule_reminder: true,
        };
    }
    CooldownPlan {
        next_spin_at: next_wheel_reset_at(now),
        replacement_last_spin: None,
        schedule_reminder: true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LossRoll {
    pub percentage: f64,
    pub flat: i64,
}

#[must_use]
pub fn requested_percentage_or_flat(balance: i64, roll: LossRoll, scale: f64) -> i64 {
    let percentage = ((balance as f64) * roll.percentage).max(1.0) as i64;
    scale_minigame_jc_delta(percentage.max(roll.flat) as f64, scale)
}

#[must_use]
pub fn canonical_outcome_code(
    landed: &WheelWedge,
    resolved: WheelValue,
    kind: WheelKind,
) -> String {
    match landed.value {
        WheelValue::Mechanic(mechanic) => mechanic.code().to_owned(),
        WheelValue::Numeric(value) if value < 0 => match kind {
            WheelKind::Golden => "OVEREXTENDED".to_owned(),
            WheelKind::Regular | WheelKind::Bankrupt => "BANKRUPT".to_owned(),
        },
        WheelValue::Numeric(0) => "LOSE".to_owned(),
        WheelValue::Numeric(_) if landed.color.eq_ignore_ascii_case("#fffacd") => {
            "CROWN".to_owned()
        }
        WheelValue::Numeric(_) => match resolved {
            WheelValue::Numeric(value) => format!("NUMERIC_{value}"),
            WheelValue::Mechanic(mechanic) => mechanic.code().to_owned(),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExplosionResolution {
    pub gross_reward: i64,
    pub credited_reward: i64,
    pub log_result: i64,
    pub wheel_kind: WheelKind,
    pub is_bonus: bool,
}

#[must_use]
pub fn resolve_explosion(
    bonus_spin: bool,
    balance: i64,
    win_multiplier: f64,
    minigame_scale: f64,
) -> ExplosionResolution {
    let central = scale_minigame_jc_delta(WHEEL_EXPLOSION_REWARD as f64, minigame_scale);
    let adjusted = apply_gamba_event_multiplier(central, win_multiplier, 1.0);
    ExplosionResolution {
        gross_reward: WHEEL_EXPLOSION_REWARD,
        credited_reward: if bonus_spin {
            scale_positive_dig_jc(adjusted)
        } else {
            adjusted
        },
        log_result: WHEEL_EXPLOSION_REWARD,
        wheel_kind: if bonus_spin || balance >= 0 {
            WheelKind::Regular
        } else {
            WheelKind::Bankrupt
        },
        is_bonus: bonus_spin,
    }
}

#[cfg(test)]
#[path = "wheel/tests.rs"]
mod tests;
