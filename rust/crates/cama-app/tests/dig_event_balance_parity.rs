//! Pure-policy counterparts for the remaining catalog balance checks.
//!
//! These tests intentionally stay at the canonical event boundary: the
//! Python tests use a service fixture for a few of these checks, but the
//! behavior under audit is the event policy itself.  Keeping the Rust cases
//! pure makes the authored values and the order of the policy scalars
//! visible without admitting a second persistence/runtime harness.

use cama_app::dig_loot::{
    CANONICAL_EVENT_CURSE_DURATION_BONUS_DIGS, CANONICAL_EVENT_CURSE_STRENGTH_MULTIPLIER,
    CanonicalEventChoice, CanonicalEventDef, CanonicalEventOutcome, CanonicalEventPolicy,
    CanonicalEventRolls, CanonicalSplash, EventComplexity, Rarity, canonical_event_catalog,
    canonical_event_weight, resolve_canonical_event_with_policy, scale_canonical_splash_payout,
};
use cama_domain::dig_splash::strengthen_dig_event_penalty;
use cama_domain::economy_scaling::{
    DEFAULT_MINIGAME_JC_DELTA_SCALE, scale_deflationary_minigame_jc_delta,
};

fn rolls(success_roll: f64) -> CanonicalEventRolls {
    CanonicalEventRolls {
        success_roll,
        ..CanonicalEventRolls::default()
    }
}

fn probe_outcome(advance: i64, jc: i64) -> CanonicalEventOutcome {
    CanonicalEventOutcome {
        description: "probe".to_owned(),
        advance,
        jc,
        cave_in: false,
        streak_loss: 0,
        curse: None,
        gear_reward_pool: Vec::new(),
        consumable_reward_pool: Vec::new(),
        artifact_reward_pool: Vec::new(),
    }
}

fn probe_event(id: &str, harmful: bool) -> CanonicalEventDef {
    let option = CanonicalEventChoice {
        label: "probe".to_owned(),
        success: probe_outcome(1, 0),
        failure: harmful.then(|| probe_outcome(-1, -1)),
        success_chance: 1.0,
    };
    CanonicalEventDef {
        id: id.to_owned(),
        name: id.to_owned(),
        descriptions: vec!["probe".to_owned()],
        min_depth: None,
        max_depth: None,
        layer: None,
        rarity: Rarity::Common,
        complexity: EventComplexity::Choice,
        safe_option: (!harmful).then_some(option.clone()),
        risky_option: harmful.then_some(option),
        desperate_option: None,
        steps: Vec::new(),
        boon_options: Vec::new(),
        buff_on_success: None,
        requires_dark: false,
        social: false,
        ascii_art: None,
        splash: None,
        guild_modifier_on_success: None,
        min_prestige: 0,
        next_event_id: None,
        chain_only: false,
        quest_id: None,
        quest_step: None,
    }
}

fn assert_low_light_policy_vector(
    luminosity: i64,
    expected_multiplier: f64,
    expected_success_chance: f64,
) {
    let safe_weight = canonical_event_weight(
        &probe_event("safe_weight_probe", false),
        luminosity,
        0.0,
        0.0,
        false,
    );
    let harmful_weight = canonical_event_weight(
        &probe_event("harmful_weight_probe", true),
        luminosity,
        0.0,
        0.0,
        false,
    );
    assert_eq!(safe_weight, 70.0);
    assert!(
        (harmful_weight / safe_weight - expected_multiplier).abs() < 1e-12,
        "luminosity {luminosity}: harmful/safe ratio was {}",
        harmful_weight / safe_weight
    );

    let policy = CanonicalEventPolicy {
        luminosity,
        ..CanonicalEventPolicy::default()
    };
    let below = resolve_canonical_event_with_policy(
        "hungering_dark",
        "risky",
        rolls(expected_success_chance - 1e-9),
        policy,
    )
    .expect("risky event just below the low-light threshold");
    let above = resolve_canonical_event_with_policy(
        "hungering_dark",
        "risky",
        rolls(expected_success_chance + 1e-9),
        policy,
    )
    .expect("risky event just above the low-light threshold");
    assert!(below.succeeded, "{luminosity}: {below:?}");
    assert!(!above.succeeded, "{luminosity}: {above:?}");
}

// test_dig_event_balance.py::test_low_light_biases_harmful_events_and_risky_success[100-1.25-0.0]
#[test]
fn dig_event_balance_bright_light_weights_harmful_events_and_preserves_risky_success() {
    assert_low_light_policy_vector(100, 1.25, 0.60);
}

// test_dig_event_balance.py::test_authored_event_penalties_round_away_from_zero[-5--7]
#[test]
fn dig_event_balance_authored_penalty_rounds_negative_five_to_negative_seven() {
    assert_eq!(strengthen_dig_event_penalty(-5), -7);
}

// test_dig_event_balance.py::test_authored_event_penalties_round_away_from_zero[-10--14]
#[test]
fn dig_event_balance_authored_penalty_rounds_negative_ten_to_negative_fourteen() {
    assert_eq!(strengthen_dig_event_penalty(-10), -14);
}

// test_dig_event_balance.py::test_low_light_biases_harmful_events_and_risky_success[50-1.375-0.05]
#[test]
fn dig_event_balance_dim_light_weights_harmful_events_and_reduces_risky_success() {
    assert_low_light_policy_vector(50, 1.25 * 1.10, 0.55);
}

// test_dig_event_balance.py::test_low_light_biases_harmful_events_and_risky_success[10-1.5625-0.15]
#[test]
fn dig_event_balance_dark_light_weights_harmful_events_and_reduces_risky_success() {
    assert_low_light_policy_vector(10, 1.25 * 1.25, 0.45);
}

// test_dig_event_balance.py::test_low_light_biases_harmful_events_and_risky_success[0-1.875-0.25]
#[test]
fn dig_event_balance_pitch_black_weights_harmful_events_and_reduces_risky_success() {
    assert_low_light_policy_vector(0, 1.25 * 1.50, 0.35);
}

// test_dig_event_balance.py::test_negative_actor_event_is_strengthened_before_economy_scaling
#[test]
fn dig_event_balance_negative_actor_event_strengthens_before_economy_scaling() {
    let result = resolve_canonical_event_with_policy(
        "hungering_dark",
        "risky",
        rolls(0.99),
        CanonicalEventPolicy::default(),
    )
    .expect("negative actor event resolves");
    assert!(!result.succeeded);
    // Authored -5 -> Python banker's-rounding negative-event multiplier -6,
    // then the event loss scalar -9, then deflationary economy scaling -10.
    assert_eq!(result.economy_gross_jc, -9);
    assert_eq!(result.jc, -10);
}

// test_dig_event_balance.py::test_negative_depth_event_setback_is_quarter_harsher
#[test]
fn dig_event_balance_negative_depth_setback_is_quarter_harsher() {
    let result = resolve_canonical_event_with_policy(
        "breach_encounter",
        "desperate",
        rolls(0.99),
        CanonicalEventPolicy::default(),
    )
    .expect("negative depth event resolves");
    // The authored desperate failure retreats four blocks; the policy applies
    // ceil(abs(-4) * 1.25), matching the Python service's -5 result.
    assert!(!result.succeeded);
    assert_eq!(result.advance, -5);
}

// test_dig_event_balance.py::test_every_risky_branch_outrewards_safe
#[test]
fn dig_event_balance_every_risky_branch_outrewards_safe() {
    let offenders = canonical_event_catalog()
        .iter()
        .filter(|event| event.complexity != EventComplexity::Boon && event.boon_options.is_empty())
        .filter_map(|event| {
            let safe = event.safe_option.as_ref()?;
            let risky = event.risky_option.as_ref()?;
            let risky_success = &risky.success;
            let safe_success = &safe.success;
            if risky_success.jc <= safe_success.jc
                && risky_success.gear_reward_pool.is_empty()
                && risky_success.consumable_reward_pool.is_empty()
                && risky_success.artifact_reward_pool.is_empty()
            {
                Some(format!(
                    "{}: risky success jc={} <= safe jc={}",
                    event.id, risky_success.jc, safe_success.jc
                ))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert!(
        offenders.is_empty(),
        "risky reward must beat safe:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn dig_event_balance_event_curses_are_stronger_and_last_two_extra_digs() {
    assert_eq!(CANONICAL_EVENT_CURSE_STRENGTH_MULTIPLIER, 1.5);
    assert_eq!(CANONICAL_EVENT_CURSE_DURATION_BONUS_DIGS, 2);
}

#[test]
fn dig_event_balance_every_risky_branch_is_genuinely_risky() {
    let offenders = canonical_event_catalog()
        .iter()
        .filter(|event| event.complexity != EventComplexity::Boon && event.boon_options.is_empty())
        .filter_map(|event| {
            let risky = event.risky_option.as_ref()?;
            let carries_threat = |outcome: &CanonicalEventOutcome| {
                outcome.cave_in
                    || outcome.streak_loss > 0
                    || outcome.curse.is_some()
                    || outcome.jc < 0
                    || outcome.advance < 0
            };
            let threatening = carries_threat(&risky.success)
                || risky.failure.as_ref().is_some_and(carries_threat);
            (!(risky.success_chance < 1.0 || threatening)).then_some(event.id.as_str())
        })
        .collect::<Vec<_>>();
    assert!(
        offenders.is_empty(),
        "risky branches with no downside: {offenders:?}"
    );
}

#[test]
fn dig_event_balance_burn_ratio_uses_strengthened_nominal_amount() {
    let splash = CanonicalSplash {
        strategy: "random_active".to_owned(),
        victim_count: 2,
        penalty_jc: 5,
        trigger: "success".to_owned(),
        mode: "burn".to_owned(),
    };
    let settlement = scale_canonical_splash_payout(10, &splash, 6, DEFAULT_MINIGAME_JC_DELTA_SCALE);
    let strengthened = strengthen_dig_event_penalty(5);
    let per_victim =
        scale_deflationary_minigame_jc_delta(strengthened as f64, DEFAULT_MINIGAME_JC_DELTA_SCALE);
    let expected_ratio = 6.0 / (2 * per_victim) as f64;
    assert!((settlement.payout_ratio - expected_ratio).abs() < 1e-12);
    assert_eq!(
        settlement.jc_after,
        (10.0 * expected_ratio).round_ties_even() as i64
    );
}
