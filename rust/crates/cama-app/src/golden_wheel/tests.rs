use std::collections::HashSet;

use super::*;

fn static_golden() -> Vec<WheelWedge> {
    wheel_catalog(WheelKind::Golden, WheelEconomyPolicy::default())
}

fn representative_economy<'a>(
    other_top_balances: &'a [i64],
    bottom_player_balances: &'a [i64],
) -> LiveGoldenEconomy<'a> {
    LiveGoldenEconomy {
        spinner_balance: 2_500,
        other_top_balances,
        rank_next_balance: Some(1_200),
        total_positive_balance: 25_000,
        bottom_player_balances,
    }
}

#[test]
fn test_golden_wheel_has_24_wedges() {
    assert_eq!(static_golden().len(), 24);
}

#[test]
fn test_golden_wheel_all_overextended_negative() {
    let overextended = static_golden()
        .into_iter()
        .filter(|wedge| wedge.color == "#4a3000")
        .collect::<Vec<_>>();
    assert_eq!(overextended.len(), 2);
    assert!(
        overextended
            .iter()
            .all(|wedge| { matches!(wedge.value, WheelValue::Numeric(value) if value < 0) })
    );
}

#[test]
fn test_golden_wheel_contains_crown_jewel() {
    let wedges = static_golden();
    let crown = wedges
        .iter()
        .filter(|wedge| wedge.color == "#fffacd")
        .collect::<Vec<_>>();
    assert_eq!(crown.len(), 1);
    assert_eq!(crown[0].label, "250");
    assert_eq!(crown[0].value, WheelValue::Numeric(250));
}

#[test]
fn test_golden_wheel_contains_new_mechanics() {
    let values = static_golden()
        .into_iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Mechanic(value) => Some(value),
            WheelValue::Numeric(_) => None,
        })
        .collect::<HashSet<_>>();
    for mechanic in [
        WheelMechanic::Heist,
        WheelMechanic::MarketCrash,
        WheelMechanic::CompoundInterest,
        WheelMechanic::TrickleDown,
        WheelMechanic::Dividend,
        WheelMechanic::HostileTakeover,
        WheelMechanic::Recession,
    ] {
        assert!(values.contains(&mechanic), "missing {mechanic:?}");
    }
}

#[test]
fn test_golden_wheel_contains_shell_mechanics() {
    let values = static_golden()
        .into_iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Mechanic(value) => Some(value),
            WheelValue::Numeric(_) => None,
        })
        .collect::<HashSet<_>>();
    assert!(values.contains(&WheelMechanic::RedShell));
    assert!(values.contains(&WheelMechanic::BlueShell));
}

#[test]
fn test_golden_wheel_ev_approximately_correct() {
    let other_top = [2_000, 1_800];
    let bottom = [
        4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 15, 16, 17, 18, 19, 20, 21, 22, 12, 10, 8, 7, 6, 5, 4, 4,
        5, 6, 7, 8,
    ];
    let policy = WheelEconomyPolicy::default();
    let economy = representative_economy(&other_top, &bottom);
    let wedges = compute_live_golden_wedges(economy, policy);
    let expected_value = live_golden_expected_value(&wedges, economy, policy);
    let target = scale_minigame_jc_delta(policy.golden_target_ev, policy.minigame_scale) as f64;
    assert!((expected_value - target).abs() < 1.0);
    assert!(expected_value < 0.0);
}

#[test]
fn test_live_golden_wheel_overextended_label_shows_value() {
    let other_top = [2_000, 1_800];
    let bottom = [10; 30];
    let wedges = compute_live_golden_wedges(
        representative_economy(&other_top, &bottom),
        WheelEconomyPolicy::default(),
    );
    let overextended = wedges
        .into_iter()
        .filter(|wedge| wedge.color == "#4a3000")
        .collect::<Vec<_>>();
    assert_eq!(overextended.len(), 2);
    for wedge in overextended {
        let WheelValue::Numeric(value) = wedge.value else {
            panic!("OVEREXTENDED must be numeric");
        };
        assert!(value < 0);
        assert_ne!(wedge.label, "OVEREXTENDED");
        assert_eq!(wedge.label, value.to_string());
    }
}

#[test]
fn test_live_golden_wheel_in_rainbow_order() {
    let other_top = [2_000, 1_800];
    let bottom = [10; 30];
    let live = compute_live_golden_wedges(
        representative_economy(&other_top, &bottom),
        WheelEconomyPolicy::default(),
    );
    assert_eq!(
        live.iter().map(|wedge| wedge.color).collect::<Vec<_>>(),
        static_golden()
            .iter()
            .map(|wedge| wedge.color)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_heist_appears_twice() {
    assert_eq!(
        static_golden()
            .iter()
            .filter(|wedge| wedge.value == WheelValue::Mechanic(WheelMechanic::Heist))
            .count(),
        2
    );
}
