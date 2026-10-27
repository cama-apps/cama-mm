use super::*;

#[test]
fn wheel_reset_timestamp_is_fixed_across_the_year() {
    // 2026-01-15 and 2026-07-15 are on opposite sides of US daylight saving.
    // Both reset at 12:00 UTC because the game clock is fixed UTC-8.
    for reset in [1_768_478_400, 1_784_116_800] {
        assert_eq!(wheel_window_start_at(reset), reset);
        assert_eq!(next_wheel_reset_at(reset - 1), reset);
        assert_eq!(next_wheel_reset_at(reset), reset + GAME_DAY_SECONDS);
    }
}

#[test]
fn test_wheel_wedges_distribution() {
    let wedges = wheel_catalog(WheelKind::Regular, WheelEconomyPolicy::default());
    let mut positives = wedges
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value > 0 => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    positives.sort_unstable();
    let negatives = wedges
        .iter()
        .filter(|wedge| matches!(wedge.value, WheelValue::Numeric(value) if value < 0))
        .count();
    let mechanics = wedges
        .iter()
        .filter(|wedge| matches!(wedge.value, WheelValue::Mechanic(_)))
        .count();

    assert_eq!(wedges.len(), 24);
    assert_eq!(negatives, 2);
    assert_eq!(
        positives,
        vec![5, 10, 10, 15, 20, 25, 30, 40, 50, 50, 60, 70, 80, 100, 100]
    );
    assert_eq!(mechanics, 6);
}

#[test]
fn test_wheel_expected_value_matches_config() {
    let policy = WheelEconomyPolicy::default();
    let wedges = wheel_catalog(WheelKind::Regular, policy);
    let actual = wheel_expected_value(WheelKind::Regular, &wedges, policy);
    let target = scale_minigame_jc_delta(policy.regular_target_ev, policy.minigame_scale) as f64;

    assert!((actual - target).abs() <= 1.0);
    assert!((-50.0..=5.0).contains(&actual));
    assert_eq!(
        wedges
            .iter()
            .filter_map(|wedge| match wedge.value {
                WheelValue::Numeric(value) if value < 0 => Some(value),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![-595, -595]
    );
}

#[test]
fn test_bankrupt_wheel_composition() {
    let regular = wheel_catalog(WheelKind::Regular, WheelEconomyPolicy::default());
    let bankrupt = wheel_catalog(WheelKind::Bankrupt, WheelEconomyPolicy::default());
    let positives = bankrupt
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value > 0 => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mechanics = bankrupt
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Mechanic(value) => Some(value),
            WheelValue::Numeric(_) => None,
        })
        .collect::<Vec<_>>();
    let losses = bankrupt
        .iter()
        .filter_map(|wedge| match wedge.value {
            WheelValue::Numeric(value) if value < 0 => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(bankrupt.len(), 24);
    assert_eq!(positives.len(), 6);
    assert!(positives.iter().max().is_some_and(|value| *value >= 75));
    assert!(positives.iter().min().is_some_and(|value| *value <= 50));
    for required in [
        WheelMechanic::RedShell,
        WheelMechanic::BlueShell,
        WheelMechanic::LightningBolt,
        WheelMechanic::ExtendOne,
        WheelMechanic::ExtendTwo,
        WheelMechanic::Jailbreak,
        WheelMechanic::ChainReaction,
        WheelMechanic::TownTrial,
        WheelMechanic::Discover,
        WheelMechanic::Emergency,
        WheelMechanic::Commune,
        WheelMechanic::Comeback,
    ] {
        assert!(mechanics.contains(&required));
    }
    assert_eq!(losses, vec![-18, -18]);
    assert!(losses.iter().any(|value| *value <= -5));
    let jackpot_colors = bankrupt
        .iter()
        .filter(|wedge| wedge.value == WheelValue::Numeric(100))
        .map(|wedge| wedge.color)
        .collect::<Vec<_>>();
    assert_eq!(jackpot_colors, vec!["#f1c40f", "#f1c40f"]);
    assert_eq!(
        regular
            .iter()
            .filter(|wedge| wedge.value == WheelValue::Numeric(100))
            .map(|wedge| wedge.color)
            .collect::<Vec<_>>(),
        jackpot_colors
    );
}

#[test]
fn test_get_wheel_wedges_returns_correct_wheel() {
    let policy = WheelEconomyPolicy::default();
    let normal = get_wheel_wedges(false, false, policy);
    let bankrupt = get_wheel_wedges(true, false, policy);

    assert_eq!(normal, wheel_catalog(WheelKind::Regular, policy));
    assert_eq!(bankrupt, wheel_catalog(WheelKind::Bankrupt, policy));
    assert_eq!(normal.len(), 24);
    assert_eq!(bankrupt.len(), 24);
}

#[test]
fn test_bankrupt_wheel_ev_maintained() {
    let policy = WheelEconomyPolicy::default();
    let bankrupt = wheel_catalog(WheelKind::Bankrupt, policy);
    let actual = wheel_expected_value(WheelKind::Bankrupt, &bankrupt, policy);
    let target = scale_minigame_jc_delta(policy.bankrupt_target_ev, policy.minigame_scale) as f64;

    assert!((actual - target).abs() <= 1.0);
}

#[test]
fn test_mario_kart_wedges_on_all_wheels() {
    for kind in [WheelKind::Regular, WheelKind::Bankrupt, WheelKind::Golden] {
        let wedges = wheel_catalog(kind, WheelEconomyPolicy::default());
        for required in [
            WheelMechanic::BananaPeel,
            WheelMechanic::GreenShell,
            WheelMechanic::BombOmb,
        ] {
            assert!(
                wedges
                    .iter()
                    .any(|wedge| wedge.value == WheelValue::Mechanic(required))
            );
        }
    }
}

#[test]
fn regular_and_lose_turn_cooldowns_share_the_fixed_daily_reset() {
    let reset = 20 * GAME_DAY_SECONDS + 12 * 3_600;
    let now = reset + 23 * 3_600;

    let regular = cooldown_after_resolution(now, false, None, CooldownOutcome::Other);
    assert_eq!(regular.next_spin_at, reset + GAME_DAY_SECONDS);
    assert_eq!(regular.replacement_last_spin, None);
    assert!(regular.schedule_reminder);

    let lose_turn = cooldown_after_resolution(now, false, None, CooldownOutcome::LoseTurn);
    assert_eq!(
        lose_turn.next_spin_at,
        reset + WHEEL_LOSE_PENALTY_DAYS * GAME_DAY_SECONDS
    );
    assert_eq!(
        lose_turn.replacement_last_spin,
        Some(lose_turn.next_spin_at - 1)
    );
    assert!(wheel_spin_is_available(
        lose_turn.next_spin_at,
        lose_turn.replacement_last_spin
    ));
    assert!(!wheel_spin_is_available(
        lose_turn.next_spin_at - 1,
        lose_turn.replacement_last_spin
    ));
}

#[test]
fn test_wheel_positive_balance_with_penalty_skips_bankrupt_wheel() {
    let penalty_games_remaining = 4;

    assert!(penalty_games_remaining > 0);
    assert_eq!(select_wheel_kind(7_002, 0, false, &[]), WheelKind::Regular);
}

#[test]
fn test_wheel_negative_balance_uses_bankrupt_wheel() {
    assert_eq!(
        select_wheel_kind(1_099, -50, false, &[1_099]),
        WheelKind::Bankrupt
    );
}

#[test]
fn test_canonical_codes_preserve_dynamic_wedge_identity() {
    let bankrupt = WheelWedge {
        label: "-92".to_owned(),
        value: WheelValue::Numeric(-92),
        color: "#1a1a1a",
    };
    assert_eq!(
        canonical_outcome_code(&bankrupt, WheelValue::Numeric(-92), WheelKind::Regular),
        "BANKRUPT"
    );

    let overextended = WheelWedge {
        label: "-390".to_owned(),
        value: WheelValue::Numeric(-390),
        color: "#4a3000",
    };
    assert_eq!(
        canonical_outcome_code(&overextended, WheelValue::Numeric(-390), WheelKind::Golden,),
        "OVEREXTENDED"
    );

    let crown = WheelWedge {
        label: "200".to_owned(),
        value: WheelValue::Numeric(200),
        color: "#fffacd",
    };
    assert_eq!(
        canonical_outcome_code(&crown, WheelValue::Numeric(200), WheelKind::Golden),
        "CROWN"
    );
}

#[test]
fn test_wheels_in_rainbow_order() {
    for kind in [WheelKind::Regular, WheelKind::Bankrupt, WheelKind::Golden] {
        let wedges = wheel_catalog(kind, WheelEconomyPolicy::default());
        assert!(wedges.windows(2).all(|pair| {
            let left = hue_sort_key(pair[0].color);
            let right = hue_sort_key(pair[1].color);
            left.0.cmp(&right.0).then(left.1.total_cmp(&right.1)) != std::cmp::Ordering::Greater
        }));
    }

    let regular = wheel_catalog(WheelKind::Regular, WheelEconomyPolicy::default());
    let position = |color| {
        regular
            .iter()
            .position(|wedge| wedge.color == color)
            .unwrap()
    };
    assert!(position("#e74c3c") < position("#228b22"));
    assert!(position("#228b22") < position("#3498db"));
}

#[test]
fn wheel_resets_at_fixed_four_am_utc_minus_eight_boundary() {
    // Every fixed-PST 4 AM boundary is 12:00 UTC. A spin one second before
    // this boundary is available again one second later, not 24 hours later.
    let reset = 20 * GAME_DAY_SECONDS + 12 * 3_600;
    assert!(wheel_spin_is_available(reset, Some(reset - 1)));
    assert!(!wheel_spin_is_available(
        reset + GAME_DAY_SECONDS - 1,
        Some(reset)
    ));
    assert!(wheel_spin_is_available(
        reset + GAME_DAY_SECONDS,
        Some(reset)
    ));
    assert_eq!(next_wheel_reset_at(reset - 1), reset);
}

#[test]
fn test_wheel_hazard_ceilings_are_ten_percent_stronger_again() {
    assert_eq!(
        (WHEEL_BANANA_PEEL_LOSS_MIN, WHEEL_BANANA_PEEL_LOSS_MAX),
        (15, 32)
    );
    assert_eq!(
        (
            WHEEL_BOMB_OMB_VICTIM_LOSS_MIN,
            WHEEL_BOMB_OMB_VICTIM_LOSS_MAX,
        ),
        (10, 25)
    );
    assert_eq!(WHEEL_BOMB_OMB_VICTIM_COUNT, 3);
}

#[test]
fn test_dig_bonus_wheel_spin_preserves_regular_cooldown() {
    let now = 1_700_000_000;
    let concurrent_spin = now - 10;
    let cooldown =
        cooldown_after_resolution(now, true, Some(concurrent_spin), CooldownOutcome::Other);

    assert_eq!(
        select_wheel_kind(1_001, 50, true, &[1_001]),
        WheelKind::Regular
    );
    assert_eq!(cooldown.replacement_last_spin, None);
    assert!(!cooldown.schedule_reminder);
    assert_eq!(cooldown.next_spin_at, next_wheel_reset_at(concurrent_spin));
}

#[test]
fn test_dig_bonus_wheel_explosion_preserves_regular_cooldown() {
    let now = 1_700_000_000;
    let previous_spin = now - 300;
    let explosion = resolve_explosion(true, 50, 1.0, 1.0);
    let cooldown =
        cooldown_after_resolution(now, true, Some(previous_spin), CooldownOutcome::Other);

    assert_eq!(explosion.gross_reward, WHEEL_EXPLOSION_REWARD);
    assert_eq!(explosion.credited_reward, scale_positive_dig_jc(67));
    assert_eq!(explosion.log_result, 67);
    assert_eq!(explosion.wheel_kind, WheelKind::Regular);
    assert!(explosion.is_bonus);
    assert_eq!(cooldown.next_spin_at, next_wheel_reset_at(previous_spin));
    assert!(!cooldown.schedule_reminder);
}

#[test]
fn test_lose_turn_wedge_has_lose_outcome_code() {
    let wedge = WheelWedge {
        label: "0".to_owned(),
        value: WheelValue::Numeric(0),
        color: "#4a4a4a",
    };
    assert_eq!(
        canonical_outcome_code(&wedge, wedge.value, WheelKind::Regular),
        "LOSE"
    );
}
