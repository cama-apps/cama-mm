//! Exact public policy counterparts for Python event-outcome jitter behavior.

use cama_app::dig_loot::{
    CanonicalEventPolicy, CanonicalEventRolls, resolve_canonical_event_with_policy,
};

#[test]
fn test_event_jc_jitter_covers_expected_range_and_mean() {
    let rolls = (0..=100)
        .map(|step| {
            resolve_canonical_event_with_policy(
                "underground_stream",
                "safe",
                CanonicalEventRolls {
                    success_roll: 0.0,
                    jc_jitter_multiplier: 0.5 + f64::from(step) / 100.0,
                    ..CanonicalEventRolls::default()
                },
                CanonicalEventPolicy::default(),
            )
            .expect("safe stream outcome")
            .jc
        })
        .collect::<Vec<_>>();

    assert_eq!(rolls.iter().copied().min(), Some(1));
    assert_eq!(rolls.iter().copied().max(), Some(3));
    let mean = rolls.iter().sum::<i64>() as f64 / rolls.len() as f64;
    assert!((1.7..=2.3).contains(&mean), "unexpected mean {mean}");
    assert!(rolls.windows(2).any(|pair| pair[0] != pair[1]));
}

#[test]
fn test_event_zero_jc_outcome_stays_zero() {
    for multiplier in [0.0, 0.5, 1.0, 1.5, 100.0] {
        let result = resolve_canonical_event_with_policy(
            "abandoned_forge",
            "safe",
            CanonicalEventRolls {
                success_roll: 0.0,
                jc_jitter_multiplier: multiplier,
                ..CanonicalEventRolls::default()
            },
            CanonicalEventPolicy::default(),
        )
        .expect("zero-JC forge outcome");

        assert_eq!(result.economy_gross_jc, 0);
        assert_eq!(result.jc, 0);
        assert!(!result.random_plan.jc_jitter);
    }
}

#[test]
fn test_event_zero_advance_outcome_stays_zero() {
    for jitter in -2..=2 {
        let result = resolve_canonical_event_with_policy(
            "underground_stream",
            "safe",
            CanonicalEventRolls {
                success_roll: 0.0,
                advance_jitter: jitter,
                ..CanonicalEventRolls::default()
            },
            CanonicalEventPolicy::default(),
        )
        .expect("zero-advance stream outcome");

        assert_eq!(result.advance, 0);
        assert!(!result.random_plan.advance_jitter);
    }
}

#[test]
fn test_event_curse_payload_is_not_jittered() {
    let resolve = |jc_jitter_multiplier, advance_jitter| {
        resolve_canonical_event_with_policy(
            "spore_debt",
            "risky",
            CanonicalEventRolls {
                success_roll: 0.99,
                jc_jitter_multiplier,
                advance_jitter,
                ..CanonicalEventRolls::default()
            },
            CanonicalEventPolicy::default(),
        )
        .expect("spore-debt failure")
        .persisted_curse
        .expect("strengthened curse")
    };

    let low = resolve(0.5, -2);
    let high = resolve(1.5, 2);
    assert_eq!(low, high);
    assert_eq!(low.id, "spore_debt_curse");
    assert_eq!(low.duration_digs, 5);
    assert_eq!(low.effect["cave_in_bonus"], serde_json::json!(0.12));
}

#[test]
fn test_event_negative_jc_stays_negative_and_varies() {
    let deltas = [0.5, 0.75, 1.0, 1.25, 1.5]
        .into_iter()
        .map(|jc_jitter_multiplier| {
            resolve_canonical_event_with_policy(
                "hungering_dark",
                "risky",
                CanonicalEventRolls {
                    success_roll: 0.99,
                    jc_jitter_multiplier,
                    ..CanonicalEventRolls::default()
                },
                CanonicalEventPolicy::default(),
            )
            .expect("negative event outcome")
            .jc
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(deltas.iter().all(|delta| *delta < 0), "{deltas:?}");
    assert!(
        deltas.len() >= 3,
        "negative payout jitter did not vary: {deltas:?}"
    );
}

#[test]
fn test_event_streak_loss_payload_is_not_jittered() {
    let seen = [(0.5, -2), (0.75, -1), (1.0, 0), (1.25, 1), (1.5, 2)]
        .into_iter()
        .map(|(jc_jitter_multiplier, advance_jitter)| {
            resolve_canonical_event_with_policy(
                "meepo_clones",
                "risky",
                CanonicalEventRolls {
                    success_roll: 0.99,
                    jc_jitter_multiplier,
                    advance_jitter,
                    ..CanonicalEventRolls::default()
                },
                CanonicalEventPolicy {
                    current_streak: 10,
                    ..CanonicalEventPolicy::default()
                },
            )
            .expect("streak-loss event outcome")
            .streak_loss
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(seen, std::collections::BTreeSet::from([3]));
}
