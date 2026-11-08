//! Deterministic sabotage-protection funding shared by live and fallback
//! orchestration: a blocked sabotage funds its tip only from the saboteur's
//! cost, so protection never mints currency.

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

#[cfg(test)]
#[path = "dig_sweep_fixes/tests.rs"]
mod tests;
