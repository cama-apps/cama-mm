//! Discord- and storage-neutral orchestration for the Wheel of Fortune.
//!
//! Python remains the live command and schema authority.  This module captures
//! the decisions which are easy to get wrong at adapter boundaries: atomic
//! cooldown admission, wheel selection and calibration, audited credit routes,
//! hostile-loss destinations, shield-aware aggregation, and stable history
//! identities.  Discord rendering, random-number generation, and SQLite
//! transactions are deliberately ports or inputs rather than hidden globals.

use std::cmp::Ordering;

use cama_domain::economy_scaling::scale_minigame_jc_delta;
use cama_domain::game_date::{GAME_DAY_SECONDS, game_day_start_ts, next_game_day_start_ts};

use crate::economy_actions::apply_gamba_event_multiplier;

pub const WHEEL_LOSE_PENALTY_DAYS: i64 = 5;
pub const WHEEL_EXPLOSION_REWARD: i64 = 67;
pub const WHEEL_EXPLOSION_CHANCE: f64 = 0.01;
pub const WHEEL_GOLDEN_TOP_N: usize = 3;
pub const HOSTILE_LOSS_MIN_BALANCE: i64 = 50;
pub const LIGHTNING_BOLT_PCT_MIN: f64 = 0.02;
pub const LIGHTNING_BOLT_PCT_MAX: f64 = 0.0627;
pub const LIGHTNING_BOLT_MIN_TAX: i64 = 1;
pub const WHEEL_BANANA_PEEL_LOSS_MIN: i64 = 15;
pub const WHEEL_BANANA_PEEL_LOSS_MAX: i64 = 32;
pub const WHEEL_GREEN_SHELL_STEAL_MIN: i64 = 15;
pub const WHEEL_GREEN_SHELL_STEAL_MAX: i64 = 33;
pub const WHEEL_BOMB_OMB_VICTIM_LOSS_MIN: i64 = 10;
pub const WHEEL_BOMB_OMB_VICTIM_LOSS_MAX: i64 = 25;
pub const WHEEL_BOMB_OMB_VICTIM_COUNT: usize = 3;
pub const DIG_POSITIVE_JC_MULTIPLIER: f64 = 0.65;

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

#[must_use]
pub fn wheel_expected_value(
    kind: WheelKind,
    wedges: &[WheelWedge],
    policy: WheelEconomyPolicy,
) -> f64 {
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
pub fn cap_numeric_wins(wedges: &[WheelWedge], cap: i64) -> Vec<WheelWedge> {
    wedges
        .iter()
        .map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value > cap => WheelWedge {
                label: cap.to_string(),
                value: WheelValue::Numeric(cap),
                color: wedge.color,
            },
            _ => wedge.clone(),
        })
        .collect()
}

#[must_use]
pub fn scale_positive_dig_jc(amount: i64) -> i64 {
    if amount <= 0 {
        return amount;
    }
    (((amount as f64) * DIG_POSITIVE_JC_MULTIPLIER) + 0.5)
        .floor()
        .max(1.0) as i64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpinRequest {
    pub user_id: i64,
    pub guild_id: i64,
    pub now: i64,
    pub bonus_spin: bool,
    pub is_admin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpinAdmission {
    RejectedUnregistered,
    RejectedCooldown { hours: i64, minutes: i64 },
    Allowed,
}

pub trait SpinAdmissionPort {
    type Error;

    fn is_registered(&mut self, user_id: i64, guild_id: i64) -> Result<bool, Self::Error>;
    fn try_claim_wheel_spin(
        &mut self,
        user_id: i64,
        guild_id: i64,
        now: i64,
        current_window_started_at: i64,
    ) -> Result<bool, Self::Error>;
    fn last_wheel_spin(&mut self, user_id: i64, guild_id: i64) -> Result<Option<i64>, Self::Error>;
    fn set_last_wheel_spin(
        &mut self,
        user_id: i64,
        guild_id: i64,
        timestamp: i64,
    ) -> Result<(), Self::Error>;
}

/// Admit a spin using one atomic claim. Bonus spins preserve the regular claim;
/// admins bypass the check but still stamp the regular cooldown.
pub fn admit_spin<P: SpinAdmissionPort>(
    port: &mut P,
    request: SpinRequest,
) -> Result<SpinAdmission, P::Error> {
    if !port.is_registered(request.user_id, request.guild_id)? {
        return Ok(SpinAdmission::RejectedUnregistered);
    }
    if request.bonus_spin {
        return Ok(SpinAdmission::Allowed);
    }
    if request.is_admin {
        port.set_last_wheel_spin(request.user_id, request.guild_id, request.now)?;
        return Ok(SpinAdmission::Allowed);
    }
    if port.try_claim_wheel_spin(
        request.user_id,
        request.guild_id,
        request.now,
        wheel_window_start_at(request.now),
    )? {
        return Ok(SpinAdmission::Allowed);
    }
    let (hours, minutes) = match port.last_wheel_spin(request.user_id, request.guild_id)? {
        Some(last_spin) => {
            let remaining = next_wheel_reset_at(last_spin)
                .saturating_sub(request.now)
                .max(0);
            (
                remaining.div_euclid(3_600),
                remaining.rem_euclid(3_600) / 60,
            )
        }
        None => {
            let remaining = next_wheel_reset_at(request.now).saturating_sub(request.now);
            (
                remaining.div_euclid(3_600),
                remaining.rem_euclid(3_600) / 60,
            )
        }
    };
    Ok(SpinAdmission::RejectedCooldown { hours, minutes })
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
pub enum RevealKind {
    Wheel,
    Explosion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryPlan {
    pub defer_initial: bool,
    pub gif_uploads: usize,
    pub waits_millis: Vec<u64>,
    pub final_edits: usize,
}

#[must_use]
pub fn delivery_plan(kind: RevealKind, bonus_spin: bool) -> DeliveryPlan {
    DeliveryPlan {
        defer_initial: !bonus_spin,
        gif_uploads: 1,
        waits_millis: vec![
            match kind {
                RevealKind::Wheel => 15_000,
                RevealKind::Explosion => 8_000,
            },
            500,
        ],
        final_edits: 1,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GambaAudit {
    pub source: &'static str,
    pub actor_id: i64,
    pub related_type: &'static str,
    pub related_id: String,
    pub reason: String,
}

impl GambaAudit {
    #[must_use]
    pub fn new(actor_id: i64, related_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            source: "gamba",
            actor_id,
            related_type: "wheel_spin",
            related_id: related_id.into(),
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditRequest {
    pub user_id: i64,
    pub guild_id: i64,
    pub amount: i64,
    pub audit: GambaAudit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreditReceipt {
    pub new_balance: i64,
    pub garnished: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreditRoute {
    Garnishment,
    Direct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreditedOutcome {
    pub route: CreditRoute,
    pub new_balance: i64,
    pub garnished: i64,
}

pub trait CreditPort {
    type Error;

    fn add_garnishable_income(
        &mut self,
        request: &CreditRequest,
    ) -> Result<CreditReceipt, Self::Error>;
    fn adjust_balance(&mut self, request: &CreditRequest) -> Result<(), Self::Error>;
    fn balance(&mut self, user_id: i64, guild_id: i64) -> Result<i64, Self::Error>;
}

/// Route positive wheel income through garnishment only while the pre-credit
/// balance is negative. Direct credits deliberately refresh the balance after
/// writing, matching the Python helper's observable ordering.
pub fn credit_gamba_outcome<P: CreditPort>(
    port: &mut P,
    garnishment_available: bool,
    current_balance: i64,
    request: CreditRequest,
) -> Result<CreditedOutcome, P::Error> {
    if garnishment_available && current_balance < 0 {
        let receipt = port.add_garnishable_income(&request)?;
        return Ok(CreditedOutcome {
            route: CreditRoute::Garnishment,
            new_balance: receipt.new_balance,
            garnished: receipt.garnished,
        });
    }
    port.adjust_balance(&request)?;
    let new_balance = port.balance(request.user_id, request.guild_id)?;
    Ok(CreditedOutcome {
        route: CreditRoute::Direct,
        new_balance,
        garnished: 0,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumericSettlement {
    Credit(i64),
    Debit { amount: i64, reserve_credit: i64 },
    LoseTurn,
    PardonConsumed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumericPolicyInput {
    pub value: i64,
    pub is_dig_bonus: bool,
    pub blue_reduction: f64,
    pub is_bad_gamba: bool,
    pub pardon_active: bool,
}

#[must_use]
pub fn numeric_settlement(input: NumericPolicyInput) -> NumericSettlement {
    if input.value > 0 {
        let mut value = if input.is_dig_bonus {
            scale_positive_dig_jc(input.value)
        } else {
            input.value
        };
        if input.blue_reduction > 0.0 {
            value -= ((value as f64) * input.blue_reduction) as i64;
        }
        return NumericSettlement::Credit(value);
    }
    if input.value == 0 {
        return NumericSettlement::LoseTurn;
    }
    if input.is_bad_gamba && input.pardon_active {
        return NumericSettlement::PardonConsumed;
    }
    NumericSettlement::Debit {
        amount: input.value,
        reserve_credit: input.value.saturating_abs(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultField {
    pub name: &'static str,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumericWinPresentation {
    pub title: &'static str,
    pub description: String,
    pub fields: Vec<ResultField>,
}

/// Build the transport-neutral content used by the Discord result embed.
#[must_use]
pub fn numeric_win_presentation(value: i64, blue_reduction: f64) -> NumericWinPresentation {
    let fields = if blue_reduction > 0.0 {
        vec![ResultField {
            name: "🏝️ Blue Mana Tax",
            value: format!(
                "Winnings reduced by {}%",
                (blue_reduction.clamp(0.0, 1.0) * 100.0) as i64
            ),
        }]
    } else {
        Vec::new()
    };
    NumericWinPresentation {
        title: "🎉 Winner!",
        description: format!("You won **{value}**"),
        fields,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BonusSpinPresentation {
    pub field_name: &'static str,
    pub field_value: &'static str,
    pub next_spin_at: i64,
}

#[must_use]
pub fn bonus_spin_presentation(next_spin_at: i64) -> BonusSpinPresentation {
    BonusSpinPresentation {
        field_name: "⛏️ Unearthed in the Mine",
        field_value: concat!(
            "Your pickaxe struck a buried wheel mechanism, waking a free normal wheel spin ",
            "from the rock. Regular `/gamba` cooldown unchanged."
        ),
        next_spin_at,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostWinDeductions {
    pub bankruptcy_penalty: i64,
    pub vanity_tax: i64,
    pub net_gain: i64,
}

#[must_use]
pub fn post_win_deductions(
    gain: i64,
    bankruptcy_penalty_rate: Option<f64>,
    vanity_tax_rate: Option<f64>,
) -> PostWinDeductions {
    let gain = gain.max(0);
    let bankruptcy_penalty = bankruptcy_penalty_rate.map_or(0, |kept_rate| {
        ((gain as f64) * (1.0 - kept_rate.clamp(0.0, 1.0))) as i64
    });
    let vanity_tax =
        vanity_tax_rate.map_or(0, |rate| ((gain as f64) * rate.clamp(0.0, 0.5)) as i64);
    PostWinDeductions {
        bankruptcy_penalty,
        vanity_tax,
        net_gain: gain
            .saturating_sub(bankruptcy_penalty)
            .saturating_sub(vanity_tax),
    }
}

#[must_use]
pub fn adjust_penalty_games(current: i64, delta: i64) -> i64 {
    current.saturating_add(delta).max(0)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PardonState {
    pub active: bool,
}

impl PardonState {
    pub fn grant(&mut self) {
        self.active = true;
    }

    pub fn consume(&mut self) -> bool {
        std::mem::take(&mut self.active)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerSnapshot {
    pub discord_id: i64,
    pub name: String,
    pub cash_balance: i64,
    pub position_value: i64,
}

impl PlayerSnapshot {
    #[must_use]
    pub fn liquid(discord_id: i64, name: impl Into<String>, cash_balance: i64) -> Self {
        Self {
            discord_id,
            name: name.into(),
            cash_balance,
            position_value: 0,
        }
    }

    #[must_use]
    pub fn net_worth(&self) -> i64 {
        self.cash_balance.saturating_add(self.position_value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostileDestination {
    Burn,
    Player(i64),
    Reserve,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostileKind {
    RedShell,
    BlueShell,
    LightningBolt,
    BananaPeel,
    GreenShell,
    BombOmb,
    Commune,
    TrickleDown,
    Decay,
    Heist,
    MarketCrash,
    HostileTakeover,
    Emergency,
    Recession,
}

impl HostileKind {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RedShell => "RED_SHELL",
            Self::BlueShell => "BLUE_SHELL",
            Self::LightningBolt => "LIGHTNING_BOLT",
            Self::BananaPeel => "BANANA_PEEL",
            Self::GreenShell => "GREEN_SHELL",
            Self::BombOmb => "BOMB_OMB",
            Self::Commune => "COMMUNE",
            Self::TrickleDown => "TRICKLE_DOWN",
            Self::Decay => "DECAY",
            Self::Heist => "HEIST",
            Self::MarketCrash => "MARKET_CRASH",
            Self::HostileTakeover => "HOSTILE_TAKEOVER",
            Self::Emergency => "EMERGENCY",
            Self::Recession => "RECESSION",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostileLossRequest {
    pub victim_id: i64,
    pub guild_id: i64,
    pub amount: i64,
    pub kind: HostileKind,
    pub actor_id: i64,
    pub event_key: String,
    pub destination: HostileDestination,
    pub clamp_to_balance: bool,
    pub min_balance: Option<i64>,
    /// Legacy adapters debit victims first and apply one aggregate destination
    /// credit later. Central protection adapters settle both sides atomically.
    pub legacy_aggregate_transfer: bool,
    pub victim_balance: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostileLossReceipt {
    pub event_key: String,
    pub requested: i64,
    pub attempted: i64,
    pub absorbed: i64,
    pub applied: i64,
    pub victim_balance_before: i64,
    pub victim_balance_after: i64,
    pub destination_balance_after: Option<i64>,
    pub centralized: bool,
}

pub trait HostileLossPort {
    type Error;

    fn settle(&mut self, request: &HostileLossRequest) -> Result<HostileLossReceipt, Self::Error>;

    fn settle_batch(
        &mut self,
        requests: &[HostileLossRequest],
    ) -> Vec<Result<HostileLossReceipt, Self::Error>> {
        requests
            .iter()
            .map(|request| self.settle(request))
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchSettlement {
    pub total_applied: i64,
    pub applied_count: usize,
    pub failed_count: usize,
    pub shield_absorbed_total: i64,
    pub shielded_count: usize,
    pub centralized: bool,
    pub applied: Vec<(i64, i64)>,
}

impl BatchSettlement {
    #[must_use]
    pub fn needs_legacy_destination_credit(&self) -> bool {
        self.total_applied > 0 && !self.centralized
    }
}

/// Submit one victim list through the batch port and contain individual
/// settlement failures. Successful siblings remain committed and aggregated.
pub fn settle_hostile_batch<P: HostileLossPort>(
    port: &mut P,
    requests: &[HostileLossRequest],
) -> BatchSettlement {
    let mut aggregate = BatchSettlement::default();
    for (request, result) in requests.iter().zip(port.settle_batch(requests)) {
        match result {
            Ok(receipt) => {
                aggregate.centralized |= receipt.centralized;
                aggregate.total_applied = aggregate.total_applied.saturating_add(receipt.applied);
                if receipt.applied > 0 {
                    aggregate.applied_count += 1;
                    aggregate.applied.push((request.victim_id, receipt.applied));
                }
                if receipt.absorbed > 0 {
                    aggregate.shielded_count += 1;
                    aggregate.shield_absorbed_total = aggregate
                        .shield_absorbed_total
                        .saturating_add(receipt.absorbed);
                }
            }
            Err(_) => aggregate.failed_count += 1,
        }
    }
    aggregate
}

#[derive(Clone, Copy)]
struct HostileRequestOptions {
    destination: HostileDestination,
    clamp_to_balance: bool,
    min_balance: Option<i64>,
    legacy_aggregate_transfer: bool,
}

fn hostile_request(
    player: &PlayerSnapshot,
    guild_id: i64,
    actor_id: i64,
    event_prefix: &str,
    amount: i64,
    kind: HostileKind,
    options: HostileRequestOptions,
) -> HostileLossRequest {
    HostileLossRequest {
        victim_id: player.discord_id,
        guild_id,
        amount,
        kind,
        actor_id,
        event_key: format!("{event_prefix}:{}", player.discord_id),
        destination: options.destination,
        clamp_to_balance: options.clamp_to_balance,
        min_balance: options.min_balance,
        legacy_aggregate_transfer: options.legacy_aggregate_transfer,
        victim_balance: player.cash_balance,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellSettlement {
    pub victim_id: Option<i64>,
    pub amount: i64,
    pub victim_new_balance: Option<i64>,
    pub spinner_new_balance: Option<i64>,
    pub self_hit: bool,
    pub missed: bool,
    pub shield_absorbed: i64,
}

impl ShellSettlement {
    #[must_use]
    pub fn log_result(&self, blue_shell: bool) -> i64 {
        if self.missed {
            0
        } else if blue_shell && self.self_hit {
            -self.amount
        } else {
            self.amount
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn settle_red_shell<P: HostileLossPort>(
    port: &mut P,
    spinner_id: i64,
    guild_id: i64,
    event_prefix: &str,
    target: Option<&PlayerSnapshot>,
    roll: LossRoll,
    scale: f64,
) -> ShellSettlement {
    let Some(target) = target.filter(|player| player.cash_balance >= HOSTILE_LOSS_MIN_BALANCE)
    else {
        return ShellSettlement {
            missed: true,
            ..ShellSettlement::default()
        };
    };
    let request = hostile_request(
        target,
        guild_id,
        spinner_id,
        event_prefix,
        requested_percentage_or_flat(target.cash_balance, roll, scale),
        HostileKind::RedShell,
        HostileRequestOptions {
            destination: HostileDestination::Player(spinner_id),
            clamp_to_balance: false,
            min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
            legacy_aggregate_transfer: false,
        },
    );
    match port.settle(&request) {
        Ok(receipt) => ShellSettlement {
            victim_id: Some(target.discord_id),
            amount: receipt.applied,
            victim_new_balance: Some(receipt.victim_balance_after),
            spinner_new_balance: receipt.destination_balance_after,
            shield_absorbed: receipt.absorbed,
            ..ShellSettlement::default()
        },
        Err(_) => ShellSettlement {
            victim_id: Some(target.discord_id),
            missed: true,
            ..ShellSettlement::default()
        },
    }
}

#[must_use]
pub fn select_net_worth_leader(players: &[PlayerSnapshot]) -> Option<&PlayerSnapshot> {
    players.iter().max_by(|left, right| {
        left.net_worth()
            .cmp(&right.net_worth())
            .then_with(|| right.discord_id.cmp(&left.discord_id))
    })
}

pub trait NetWorthSnapshotPort {
    type Error;

    fn guild_net_worth_snapshot(
        &mut self,
        guild_id: i64,
    ) -> Result<Vec<PlayerSnapshot>, Self::Error>;
}

/// Load exactly one liquid-plus-position snapshot for Blue Shell selection.
/// The returned row is owned so later adapter activity cannot alter the chosen
/// target before settlement.
pub fn load_blue_shell_leader<P: NetWorthSnapshotPort>(
    port: &mut P,
    guild_id: i64,
) -> Result<Option<PlayerSnapshot>, P::Error> {
    let snapshot = port.guild_net_worth_snapshot(guild_id)?;
    Ok(select_net_worth_leader(&snapshot).cloned())
}

#[allow(clippy::too_many_arguments)]
pub fn settle_blue_shell<P: HostileLossPort>(
    port: &mut P,
    spinner: &PlayerSnapshot,
    guild_id: i64,
    event_prefix: &str,
    leader: Option<&PlayerSnapshot>,
    steal_roll: LossRoll,
    self_hit_roll: LossRoll,
    scale: f64,
) -> ShellSettlement {
    let Some(leader) = leader else {
        return ShellSettlement {
            missed: true,
            ..ShellSettlement::default()
        };
    };
    let self_hit = leader.discord_id == spinner.discord_id;
    let (target, roll, destination, minimum) = if self_hit {
        (spinner, self_hit_roll, HostileDestination::Reserve, None)
    } else {
        (
            leader,
            steal_roll,
            HostileDestination::Player(spinner.discord_id),
            None,
        )
    };
    let request = hostile_request(
        target,
        guild_id,
        spinner.discord_id,
        event_prefix,
        requested_percentage_or_flat(target.cash_balance, roll, scale),
        HostileKind::BlueShell,
        HostileRequestOptions {
            destination,
            clamp_to_balance: false,
            min_balance: minimum,
            legacy_aggregate_transfer: false,
        },
    );
    match port.settle(&request) {
        Ok(receipt) => ShellSettlement {
            victim_id: Some(target.discord_id),
            amount: receipt.applied,
            victim_new_balance: Some(receipt.victim_balance_after),
            spinner_new_balance: if self_hit {
                Some(receipt.victim_balance_after)
            } else {
                receipt.destination_balance_after
            },
            self_hit,
            shield_absorbed: receipt.absorbed,
            ..ShellSettlement::default()
        },
        Err(_) => ShellSettlement {
            victim_id: Some(target.discord_id),
            self_hit,
            missed: true,
            ..ShellSettlement::default()
        },
    }
}

#[must_use]
pub fn lightning_requests(
    players: &[PlayerSnapshot],
    spinner_id: i64,
    guild_id: i64,
    event_prefix: &str,
    tax_rate: f64,
    scale: f64,
) -> Vec<HostileLossRequest> {
    players
        .iter()
        .filter(|player| player.cash_balance >= HOSTILE_LOSS_MIN_BALANCE)
        .map(|player| {
            let amount = scale_minigame_jc_delta(
                LIGHTNING_BOLT_MIN_TAX.max(((player.cash_balance as f64) * tax_rate) as i64) as f64,
                scale,
            );
            hostile_request(
                player,
                guild_id,
                spinner_id,
                event_prefix,
                amount,
                HostileKind::LightningBolt,
                HostileRequestOptions {
                    destination: HostileDestination::Reserve,
                    clamp_to_balance: true,
                    min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
                    legacy_aggregate_transfer: true,
                },
            )
        })
        .collect()
}

#[must_use]
pub fn commune_requests(
    players: &[PlayerSnapshot],
    spinner_id: i64,
    guild_id: i64,
    event_prefix: &str,
) -> Vec<HostileLossRequest> {
    players
        .iter()
        .filter(|player| {
            player.discord_id != spinner_id && player.cash_balance >= HOSTILE_LOSS_MIN_BALANCE
        })
        .map(|player| {
            hostile_request(
                player,
                guild_id,
                spinner_id,
                event_prefix,
                1,
                HostileKind::Commune,
                HostileRequestOptions {
                    destination: HostileDestination::Player(spinner_id),
                    clamp_to_balance: true,
                    min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
                    legacy_aggregate_transfer: true,
                },
            )
        })
        .collect()
}

#[must_use]
pub fn trickle_down_requests(
    players: &[PlayerSnapshot],
    spinner_id: i64,
    guild_id: i64,
    event_prefix: &str,
    tax_rate: f64,
    scale: f64,
) -> Vec<HostileLossRequest> {
    players
        .iter()
        .filter(|player| {
            player.discord_id != spinner_id && player.cash_balance >= HOSTILE_LOSS_MIN_BALANCE
        })
        .map(|player| {
            let tax = scale_minigame_jc_delta(
                1.max(((player.cash_balance as f64) * tax_rate) as i64) as f64,
                scale,
            );
            hostile_request(
                player,
                guild_id,
                spinner_id,
                event_prefix,
                tax,
                HostileKind::TrickleDown,
                HostileRequestOptions {
                    destination: HostileDestination::Player(spinner_id),
                    clamp_to_balance: true,
                    min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
                    legacy_aggregate_transfer: true,
                },
            )
        })
        .collect()
}

#[must_use]
pub fn banana_request(
    victim: Option<&PlayerSnapshot>,
    spinner_id: i64,
    guild_id: i64,
    event_prefix: &str,
    rolled_loss: i64,
    scale: f64,
) -> Option<HostileLossRequest> {
    let victim = victim.filter(|player| player.cash_balance >= HOSTILE_LOSS_MIN_BALANCE)?;
    let requested = scale_minigame_jc_delta(rolled_loss as f64, scale)
        .min(victim.cash_balance)
        .max(0);
    (requested > 0).then(|| {
        hostile_request(
            victim,
            guild_id,
            spinner_id,
            event_prefix,
            requested,
            HostileKind::BananaPeel,
            HostileRequestOptions {
                destination: HostileDestination::Burn,
                clamp_to_balance: true,
                min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
                legacy_aggregate_transfer: false,
            },
        )
    })
}

#[must_use]
pub fn eligible_other_players(players: &[PlayerSnapshot], spinner_id: i64) -> Vec<&PlayerSnapshot> {
    players
        .iter()
        .filter(|player| {
            player.discord_id != spinner_id && player.cash_balance >= HOSTILE_LOSS_MIN_BALANCE
        })
        .collect()
}

#[must_use]
pub fn green_shell_request(
    victim: Option<&PlayerSnapshot>,
    spinner_id: i64,
    guild_id: i64,
    event_prefix: &str,
    rolled_steal: i64,
    scale: f64,
) -> Option<HostileLossRequest> {
    let victim = victim.filter(|player| {
        player.discord_id != spinner_id && player.cash_balance >= HOSTILE_LOSS_MIN_BALANCE
    })?;
    let requested = scale_minigame_jc_delta(rolled_steal as f64, scale)
        .min(victim.cash_balance)
        .max(0);
    (requested > 0).then(|| {
        hostile_request(
            victim,
            guild_id,
            spinner_id,
            event_prefix,
            requested,
            HostileKind::GreenShell,
            HostileRequestOptions {
                destination: HostileDestination::Player(spinner_id),
                clamp_to_balance: false,
                min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
                legacy_aggregate_transfer: false,
            },
        )
    })
}

#[must_use]
pub fn bomb_omb_requests(
    sampled_victims: &[&PlayerSnapshot],
    spinner_id: i64,
    guild_id: i64,
    event_prefix: &str,
    rolled_losses: &[i64],
    scale: f64,
) -> Vec<HostileLossRequest> {
    sampled_victims
        .iter()
        .take(WHEEL_BOMB_OMB_VICTIM_COUNT)
        .zip(rolled_losses)
        .filter_map(|(victim, loss)| {
            if victim.discord_id == spinner_id || victim.cash_balance < HOSTILE_LOSS_MIN_BALANCE {
                return None;
            }
            let amount = scale_minigame_jc_delta(*loss as f64, scale)
                .min(victim.cash_balance)
                .max(0);
            (amount > 0).then(|| {
                hostile_request(
                    victim,
                    guild_id,
                    spinner_id,
                    event_prefix,
                    amount,
                    HostileKind::BombOmb,
                    HostileRequestOptions {
                        destination: HostileDestination::Burn,
                        clamp_to_balance: true,
                        min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
                        legacy_aggregate_transfer: false,
                    },
                )
            })
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TakeoverSettlement {
    pub victim_id: Option<i64>,
    pub amount: i64,
    pub missed: bool,
    pub fallback_credit: i64,
    pub shield_absorbed: i64,
}

#[allow(clippy::too_many_arguments)]
pub fn settle_hostile_takeover<P: HostileLossPort>(
    port: &mut P,
    leaderboard: &[PlayerSnapshot],
    spinner_id: i64,
    guild_id: i64,
    event_prefix: &str,
    roll: LossRoll,
    scale: f64,
) -> TakeoverSettlement {
    let candidate = leaderboard.get(WHEEL_GOLDEN_TOP_N);
    let Some(victim) = candidate.filter(|player| player.cash_balance >= HOSTILE_LOSS_MIN_BALANCE)
    else {
        return TakeoverSettlement {
            missed: true,
            fallback_credit: scale_minigame_jc_delta(40.0, scale),
            ..TakeoverSettlement::default()
        };
    };
    let request = hostile_request(
        victim,
        guild_id,
        spinner_id,
        event_prefix,
        requested_percentage_or_flat(victim.cash_balance, roll, scale),
        HostileKind::HostileTakeover,
        HostileRequestOptions {
            destination: HostileDestination::Player(spinner_id),
            clamp_to_balance: false,
            min_balance: Some(HOSTILE_LOSS_MIN_BALANCE),
            legacy_aggregate_transfer: false,
        },
    );
    match port.settle(&request) {
        Ok(receipt) => TakeoverSettlement {
            victim_id: Some(victim.discord_id),
            amount: receipt.applied,
            shield_absorbed: receipt.absorbed,
            ..TakeoverSettlement::default()
        },
        Err(_) => TakeoverSettlement {
            victim_id: Some(victim.discord_id),
            missed: true,
            // Once an eligible target was chosen, failure is never converted
            // into the ineligible-target mint.
            fallback_credit: 0,
            ..TakeoverSettlement::default()
        },
    }
}

#[must_use]
pub fn stable_event_keys(requests: &[HostileLossRequest]) -> bool {
    let Some((first, _)) = requests
        .first()
        .and_then(|request| request.event_key.rsplit_once(':'))
    else {
        return requests.is_empty();
    };
    requests.iter().all(|request| {
        request
            .event_key
            .rsplit_once(':')
            .is_some_and(|(prefix, suffix)| {
                prefix == first && suffix == request.victim_id.to_string()
            })
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WheelOutcome {
    pub value: WheelValue,
    pub shell: ShellSettlement,
    pub lightning_total: i64,
    pub commune_total: i64,
    pub heist_total: i64,
    pub market_crash_total: i64,
    pub compound_amount: i64,
    pub trickle_total: i64,
    pub dividend_amount: i64,
    pub takeover: TakeoverSettlement,
    pub recession_self_loss: i64,
    pub green_shell_amount: i64,
}

impl WheelOutcome {
    #[must_use]
    pub fn log_result(&self) -> i64 {
        match self.value {
            WheelValue::Numeric(value) => value,
            WheelValue::Mechanic(WheelMechanic::RedShell) => self.shell.log_result(false),
            WheelValue::Mechanic(WheelMechanic::BlueShell) => self.shell.log_result(true),
            WheelValue::Mechanic(WheelMechanic::Commune) => self.commune_total,
            WheelValue::Mechanic(WheelMechanic::Heist) => self.heist_total,
            WheelValue::Mechanic(WheelMechanic::MarketCrash) => self.market_crash_total,
            WheelValue::Mechanic(WheelMechanic::CompoundInterest) => self.compound_amount,
            WheelValue::Mechanic(WheelMechanic::TrickleDown) => self.trickle_total,
            WheelValue::Mechanic(WheelMechanic::Dividend) => self.dividend_amount,
            WheelValue::Mechanic(WheelMechanic::HostileTakeover) => {
                if self.takeover.missed {
                    0
                } else {
                    self.takeover.amount
                }
            }
            WheelValue::Mechanic(WheelMechanic::Recession) => -self.recession_self_loss,
            WheelValue::Mechanic(WheelMechanic::GreenShell) => self.green_shell_amount,
            WheelValue::Mechanic(_) => 0,
        }
    }
}

impl Default for WheelOutcome {
    fn default() -> Self {
        Self {
            value: WheelValue::Numeric(0),
            shell: ShellSettlement::default(),
            lightning_total: 0,
            commune_total: 0,
            heist_total: 0,
            market_crash_total: 0,
            compound_amount: 0,
            trickle_total: 0,
            dividend_amount: 0,
            takeover: TakeoverSettlement::default(),
            recession_self_loss: 0,
            green_shell_amount: 0,
        }
    }
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

#[must_use]
pub fn deflation_destination(mechanic: WheelMechanic, spinner_id: i64) -> HostileDestination {
    match mechanic {
        WheelMechanic::GreenShell => HostileDestination::Player(spinner_id),
        WheelMechanic::BananaPeel | WheelMechanic::BombOmb => HostileDestination::Burn,
        _ => HostileDestination::Reserve,
    }
}

#[must_use]
pub fn compare_rainbow(left: &WheelWedge, right: &WheelWedge) -> Ordering {
    let left_key = hue_sort_key(left.color);
    let right_key = hue_sort_key(right.color);
    left_key
        .0
        .cmp(&right_key.0)
        .then_with(|| left_key.1.total_cmp(&right_key.1))
}

#[cfg(test)]
#[path = "wheel/tests.rs"]
mod tests;
