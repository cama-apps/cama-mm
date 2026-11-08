use cama_db::dig_carry_wager::{CarryProgressEntry, CarryTunnelKey};

use super::*;

fn pinnacle_snapshot(
    pinnacle_phase: i64,
    status: &str,
    wager: PersistedWager,
    risk: Option<&str>,
) -> CarryTunnelSnapshot {
    CarryTunnelSnapshot {
        key: CarryTunnelKey {
            discord_id: 1,
            guild_id: Some(2),
        },
        depth: PINNACLE_BOUNDARY - 1,
        pinnacle_phase,
        boss_progress: "{}".to_owned(),
        balance: Some(100),
        entries: vec![(
            PINNACLE_BOUNDARY,
            CarryProgressEntry {
                status: Some(status.to_owned()),
                has_wager_marker: !matches!(wager, PersistedWager::Missing),
                wager,
                has_risk_marker: risk.is_some(),
                risk_tier: risk.map(str::to_owned),
            },
        )],
    }
}

#[test]
fn a_coherent_pinnacle_carry_is_read_back() {
    let snapshot = pinnacle_snapshot(
        2,
        "phase1_defeated",
        PersistedWager::Integer(75),
        Some("bold"),
    );
    assert_eq!(
        active_pinnacle_carry(&snapshot, PINNACLE_BOUNDARY),
        Some(CarriedWager {
            wager: 75,
            risk_tier: RiskTier::Bold,
            boundary: PINNACLE_BOUNDARY,
        })
    );
    assert_eq!(active_pinnacle_carry(&snapshot, 275), None);
}

#[test]
fn a_carry_that_does_not_match_the_phase_or_is_malformed_is_ignored() {
    for snapshot in [
        pinnacle_snapshot(
            3,
            "phase1_defeated",
            PersistedWager::Integer(75),
            Some("bold"),
        ),
        pinnacle_snapshot(1, "active", PersistedWager::Integer(75), Some("bold")),
        pinnacle_snapshot(
            2,
            "phase1_defeated",
            PersistedWager::Integer(-5),
            Some("bold"),
        ),
        pinnacle_snapshot(2, "phase1_defeated", PersistedWager::Invalid, Some("bold")),
        pinnacle_snapshot(
            2,
            "phase1_defeated",
            PersistedWager::Integer(75),
            Some("wild"),
        ),
        pinnacle_snapshot(2, "phase1_defeated", PersistedWager::Integer(75), None),
    ] {
        assert_eq!(active_pinnacle_carry(&snapshot, PINNACLE_BOUNDARY), None);
    }
}

#[test]
fn pinnacle_wager_profit_rejects_negative_and_pays_nothing_for_free_fights() {
    assert!(matches!(
        pinnacle_wager_profit(-1, RiskTier::Bold, 0.5),
        Err(CarryPolicyError::NegativeWager(-1))
    ));
    assert_eq!(pinnacle_wager_profit(0, RiskTier::Bold, 0.5), Ok(0));
    assert!(pinnacle_wager_profit(100, RiskTier::Reckless, 0.3).expect("profit") > 0);
}
