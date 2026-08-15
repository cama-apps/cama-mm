//! Deterministic Dig policy shared by live and fallback orchestration.
//!
//! Discord transport, scheduling, and persistence stay outside this module.
//! The policies here make the previously divergent paths consume a dig and
//! mutate protection charges identically, while keeping money movement and
//! stale-duel ordering explicit for an atomic repository adapter.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RewardOutcome {
    pub credited: i64,
    pub event_adjustments: u8,
}

/// Apply the daily economy adjustment to an already-scaled reward exactly once.
pub fn regular_dig_reward(
    scaled_reward: i64,
    adjust_reward: impl FnOnce(i64) -> i64,
) -> RewardOutcome {
    RewardOutcome {
        credited: adjust_reward(scaled_reward).max(0),
        event_adjustments: 1,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigPath {
    DeterministicFallback,
    DirectMessage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigOutcome {
    pub path: DigPath,
    pub success: bool,
    pub cave_in: bool,
    pub dig_consumed: bool,
}

#[must_use]
pub const fn fallback_outcome(cave_in: bool) -> DigOutcome {
    DigOutcome {
        path: DigPath::DeterministicFallback,
        success: true,
        cave_in,
        dig_consumed: true,
    }
}

#[must_use]
pub const fn direct_message_outcome(cave_in: bool) -> DigOutcome {
    DigOutcome {
        path: DigPath::DirectMessage,
        success: true,
        cave_in,
        dig_consumed: true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectionSource {
    Shield,
    Ward,
    AegisCharge,
    DuplicateShield,
}

impl ProtectionSource {
    #[must_use]
    pub const fn audit_action(self) -> &'static str {
        match self {
            Self::Ward => "sabotage_ward_blocked",
            Self::Shield | Self::AegisCharge | Self::DuplicateShield => "sabotage_blocked",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FundedProtection {
    pub source: ProtectionSource,
    pub actor_delta: i64,
    pub target_delta: i64,
    pub audit_action: &'static str,
}

impl FundedProtection {
    #[must_use]
    pub const fn net_delta(self) -> i64 {
        self.actor_delta + self.target_delta
    }
}

/// Fund a protection tip only from the saboteur's cost; never mint currency.
#[must_use]
pub fn funded_protection(
    source: ProtectionSource,
    cost: i64,
    requested_tip: i64,
) -> FundedProtection {
    let bounded_cost = cost.max(0);
    let target_delta = if source == ProtectionSource::DuplicateShield {
        0
    } else {
        requested_tip.clamp(0, bounded_cost)
    };
    FundedProtection {
        source,
        actor_delta: -bounded_cost,
        target_delta,
        audit_action: source.audit_action(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaleDuel {
    pub wager: i64,
    pub carried_wager: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DuelStartRequest {
    pub balance: i64,
    pub stale: Option<StaleDuel>,
    pub new_wager: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuelStartOutcome {
    Started {
        balance_after_forfeit: i64,
        cleared_carried_wager: bool,
    },
    InsufficientAfterForfeit {
        balance_after_forfeit: i64,
        required: i64,
        cleared_carried_wager: bool,
    },
}

/// Claim/forfeit stale state before validating a replacement wager.
#[must_use]
pub fn plan_duel_start(request: DuelStartRequest) -> DuelStartOutcome {
    let (forfeit, cleared_carried_wager) = request.stale.map_or((0, false), |stale| {
        (stale.wager.max(0), stale.carried_wager == Some(stale.wager))
    });
    let balance_after_forfeit = request.balance.saturating_sub(forfeit);
    let required = request.new_wager.max(0);
    if balance_after_forfeit < required {
        DuelStartOutcome::InsufficientAfterForfeit {
            balance_after_forfeit,
            required,
            cleared_carried_wager,
        }
    } else {
        DuelStartOutcome::Started {
            balance_after_forfeit,
            cleared_carried_wager,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consumable {
    Reinforcement,
    SonarPulse,
    GrapplingHook,
    VoidBait,
    HardHat,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChargeColumns {
    pub hard_hat_charges: i64,
    pub grappling_hook_charges: i64,
    pub sonar_skip_pending: bool,
    pub reinforced_until: i64,
    pub void_bait_digs: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionPath {
    Live,
    Deterministic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChargeTransition {
    pub columns: ChargeColumns,
    pub cave_in: bool,
}

/// Resolve both runtime paths through one consumable state transition.
#[must_use]
pub fn resolve_consumable_dig(
    _path: ExecutionPath,
    initial: ChargeColumns,
    consumable: Consumable,
    cave_roll: f64,
    now: i64,
) -> ChargeTransition {
    resolve_charge_transition(initial, consumable, cave_roll < 0.01, now)
}

fn resolve_charge_transition(
    mut columns: ChargeColumns,
    consumable: Consumable,
    would_cave: bool,
    now: i64,
) -> ChargeTransition {
    let old_sonar_pending = columns.sonar_skip_pending;
    match consumable {
        Consumable::Reinforcement => columns.reinforced_until = now.saturating_add(3_600),
        Consumable::SonarPulse => {}
        Consumable::GrapplingHook => columns.grappling_hook_charges = 3,
        Consumable::VoidBait => columns.void_bait_digs = 3,
        Consumable::HardHat => columns.hard_hat_charges = 3,
    }

    let mut cave_in = would_cave;
    if would_cave && columns.hard_hat_charges > 0 {
        columns.hard_hat_charges -= 1;
        cave_in = false;
    } else if would_cave && columns.grappling_hook_charges > 0 {
        columns.grappling_hook_charges -= 1;
    }
    if columns.void_bait_digs > 0 {
        columns.void_bait_digs -= 1;
    }

    // Consume the old pending skip first, then arm a newly queued pulse.
    if old_sonar_pending {
        columns.sonar_skip_pending = false;
    }
    if consumable == Consumable::SonarPulse {
        columns.sonar_skip_pending = true;
    }
    ChargeTransition { columns, cave_in }
}

#[must_use]
pub fn sabotage_cooldown_consumed(action_type: &str) -> bool {
    action_type == "sabotage"
}

#[cfg(test)]
#[path = "dig_sweep_fixes/tests.rs"]
mod tests;
