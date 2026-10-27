use std::collections::{BTreeMap, BTreeSet};

use cama_domain::dig_economy::scale_positive_dig_jc;

use super::*;

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-12,
        "expected {actual} to be within 1e-12 of {expected}"
    );
}

fn started_tunnel(depth: i64) -> TunnelState {
    TunnelState {
        depth,
        max_depth: depth,
        total_digs: 1,
        last_dig_at: Some(1_000_000),
        ..TunnelState::default()
    }
}

fn outcome(advance: i64, gross_jc: i64) -> DigOutcomeInput {
    DigOutcomeInput {
        advance,
        gross_jc,
        ..DigOutcomeInput::default()
    }
}

#[test]
fn test_paid_dig_ramp_is_strictly_increasing() {
    assert!(
        PAID_DIG_COSTS_PER_DAY
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    );
}

#[test]
fn test_paid_dig_cost_levels_off_after_seed_ladder() {
    assert_eq!(
        (4..=8)
            .map(|paid_count| paid_dig_cost(paid_count, 0, 0.0))
            .collect::<Vec<_>>(),
        vec![40, 60, 80, 100, 100]
    );
    assert_eq!(paid_dig_cost(5, 13, 0.0), 30);
}

macro_rules! multiplier_anchor {
    ($name:ident, $prestige:expr, $expected:expr) => {
        #[test]
        fn $name() {
            assert_close(prestige_cave_in_multiplier($prestige), $expected);
        }
    };
}

multiplier_anchor!(test_multiplier_anchors_0_0_9, 0, 0.9);
multiplier_anchor!(test_multiplier_anchors_3_0_99, 3, 0.99);
multiplier_anchor!(test_multiplier_anchors_10_1_2, 10, 1.20);

#[test]
fn test_first_dig_creates_tunnel() {
    let mut tunnel = TunnelState::default();
    let result = apply_first_dig(&mut tunnel, 6, 4, 4, 1_000_000);
    assert!(result.is_first_dig);
    assert!(!result.dig_consumed);
    assert!((3..=7).contains(&result.advance));
    assert!((1..=5).contains(&result.jc_earned));
    assert_eq!(tunnel.total_digs, 1);
}

#[test]
fn test_first_dig_no_cave_in() {
    for roll in -10..20 {
        let mut tunnel = TunnelState::default();
        assert!(!apply_first_dig(&mut tunnel, roll, roll, roll, 10).cave_in);
    }
}

#[test]
fn test_first_dig_runs_daily_event_after_central_scale() {
    let mut tunnel = TunnelState::default();
    let result = apply_first_dig(&mut tunnel, 3, 5, 2, 1_000_000);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(2));
}

#[test]
fn test_dig_advances_depth() {
    let mut tunnel = started_tunnel(5);
    let result = apply_dig_outcome(&mut tunnel, outcome(3, 0), 1_003_601);
    assert_eq!(result.depth_after, 8);
    assert_eq!(result.advance, 3);
}

#[test]
fn test_dig_earns_jc() {
    let mut tunnel = started_tunnel(5);
    let result = apply_dig_outcome(&mut tunnel, outcome(1, 4), 1_003_601);
    assert!(result.jc_earned >= 0);
}

#[test]
fn test_dig_reuses_one_request_local_mana_effects_snapshot() {
    let mut lookups = 0;
    let snapshot = {
        lookups += 1;
        cama_domain::mana::ManaEffects::for_color(Some("White"), Some("Plains"))
    };
    let first_use = snapshot.dig_yield_variance;
    let second_use = snapshot.boss_loot_mult;
    assert_eq!(lookups, 1);
    assert_eq!(first_use, 0.0);
    assert!(second_use > 0.0);
}

#[test]
fn test_mana_snapshot_preserves_standalone_modifier_outcomes() {
    let effects = cama_domain::mana::ManaEffects::for_color(Some("Green"), Some("Forest"));
    let supplied =
        effective_dig_cooldown(0, None, None, None, effects.dig_cooldown_reduction_seconds);
    let standalone = effective_dig_cooldown(0, None, None, None, 30);
    assert_eq!(supplied, standalone);
}

#[test]
fn test_base_dig_payout_is_capped_at_20() {
    let mut tunnel = started_tunnel(10);
    let result = apply_dig_outcome(&mut tunnel, outcome(1, 25), 2_000_000);
    assert_eq!(result.gross_jc, BASE_DIG_JC_PAYOUT_CAP);
    assert_eq!(
        result.jc_earned,
        scale_positive_dig_jc(BASE_DIG_JC_PAYOUT_CAP)
    );
}

#[test]
fn test_dig_scales_generated_jc_before_mana_taxes() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 25);
    input.helltide_tax = 5;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.gross_jc, 20);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(20) - 6);
}

#[test]
fn test_composed_yield_factors_truncate_once_at_base_roll_boundary() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 7);
    // 7 * 1.50 * 1.75 = 18.375, so Python's composed-float `int(...)`
    // boundary yields 18. Sequential integer truncation would incorrectly
    // produce 17.
    input.yield_multiplier_millionths = Some(1_500_000);
    input.yield_buff_multiplier_millionths = Some(1_750_000);
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.gross_jc, 18);
}

#[test]
fn test_fractional_yield_buff_lifts_base_cap_at_same_fixed_point_boundary() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 100);
    input.yield_buff_multiplier_millionths = Some(1_750_000);
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.gross_jc, 35);
}

#[test]
fn test_request_local_pre_cap_reward_is_not_multiplied_twice() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 7);
    input.yield_multiplier_millionths = Some(1_500_000);
    input.yield_buff_multiplier_millionths = Some(1_750_000);
    // The live adapter already composed 7 * 1.5 * 1.75, applied its shared
    // entropy policies, and arrived at 19. The buff still lifts the cap to 35
    // but must not multiply this request-local base a second time.
    input.pre_cap_jc = Some(19);
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.gross_jc, 19);
}

#[test]
fn test_milestone_multiplier_floors_each_crossed_reward() {
    let mut tunnel = started_tunnel(24);
    tunnel.defeated_bosses = [25, 50].into_iter().collect();
    let mut input = outcome(30, 0);
    input.authored_event = true;
    input.milestone_multiplier_basis_points = 15_000;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    // 3*1.5 floors to 4 and 6*1.5 floors to 9; do not multiply the summed
    // bucket and round once (which would obscure authored milestone edges).
    assert_eq!(result.milestone_bonus, 13);
    assert_eq!(result.gross_jc, 13);
}

#[test]
fn test_patient_step_multiplier_is_applied_before_minigame_scale() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 1);
    input.streak_bonus = 3;
    input.streak_bonus_multiplier_basis_points = 15_000;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    // Patient Step turns 3 into floor(4.5)=4; the structural basis is then
    // 1+4=5 and positive scaling settles it at 3 JC.
    assert_eq!(result.streak_bonus, 4);
    assert_eq!(result.gross_jc, 5);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(5));
}

#[test]
fn test_overgrowth_is_added_after_positive_scale_before_daily_economy() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 10);
    input.overgrowth_bonus = 10;
    input.economy_reward_multiplier_basis_points = 5_000;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    // positive(10)=7, then +10 Overgrowth=17, then daily half-up 0.5x=9.
    assert_eq!(result.gross_jc, 10);
    assert_eq!(result.economy_gross_jc, 17);
    assert_eq!(result.economy_adjusted_jc, 9);
    assert_eq!(result.jc_earned, 9);
}

#[test]
fn test_helltide_tax_precedes_independent_profit_deductions() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 10);
    input.helltide_tax = 3;
    input.profit_policy = DigProfitPolicy {
        bankruptcy_penalty_bps_per_game: 2_500,
        bankruptcy_penalty_games_remaining: 1,
        vanity_tax_basis_points: 1_000,
        low_priority_tax_basis_points: 0,
    };
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    // positive(10)=7, Helltide's deflationary 3 remains 3, leaving 4;
    // bankruptcy floors 25% of 4 to 1 and vanity floors 10% to 0.
    assert_eq!(result.jc_earned, 3);
    assert_eq!(result.bankruptcy_penalty, 1);
    assert_eq!(result.vanity_tax, 0);
}

#[test]
fn test_mana_yield_tax_precedes_helltide_and_independent_profit_deductions() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 20);
    input.mana_yield_tax = 2;
    input.helltide_tax = 3;
    input.profit_policy = DigProfitPolicy {
        bankruptcy_penalty_bps_per_game: 2_500,
        bankruptcy_penalty_games_remaining: 1,
        vanity_tax_basis_points: 1_000,
        low_priority_tax_basis_points: 0,
    };
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    // positive(20)=13, Mana removes 2, Helltide removes 3, then both profit
    // policies independently inspect 8 (bankruptcy 2, vanity 0).
    assert_eq!(result.economy_adjusted_jc, 13);
    assert_eq!(result.mana_yield_tax, 2);
    assert_eq!(result.bankruptcy_penalty, 2);
    assert_eq!(result.vanity_tax, 0);
    assert_eq!(result.jc_earned, 6);
}

#[test]
fn test_configured_minigame_scale_uses_half_up_before_positive_scale() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 5);
    input.minigame_jc_delta_scale_millionths = 500_000;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(
        result.gross_jc, 3,
        "2.5 rounds half-up before payout scaling"
    );
    assert_eq!(result.jc_earned, scale_positive_dig_jc(3));

    let mut minimum_tunnel = started_tunnel(10);
    input.minigame_jc_delta_scale_millionths = 0;
    let minimum = apply_dig_outcome(&mut minimum_tunnel, input, 2_000_000);
    assert_eq!(
        minimum.gross_jc, 1,
        "non-zero generated JC keeps a one-JC floor"
    );
}

#[test]
fn test_cave_in_reward_returns_before_helltide_tax() {
    let mut tunnel = started_tunnel(50);
    let mut input = outcome(0, 3);
    input.cave_in_loss = 5;
    input.economy_before_positive_scale = true;
    input.helltide_tax = 5;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert!(result.cave_in);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(3));
}

#[test]
fn test_deterministic_dig_outcome_scales_full_positive_payout_before_sinks() {
    let mut tunnel = started_tunnel(10);
    tunnel.balance = 100;

    let result = apply_dig_outcome(&mut tunnel, outcome(1, 15), 2_000_000);

    assert_eq!(result.gross_jc, 15);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(15));
    assert_eq!(tunnel.balance, 100 + scale_positive_dig_jc(15));
}

#[test]
fn test_stacked_base_dig_bonuses_are_capped_after_modifiers() {
    let mut tunnel = started_tunnel(10);
    let result = apply_dig_outcome(&mut tunnel, outcome(1, 200), 2_000_000);
    assert_eq!(result.gross_jc, 20);
}

#[test]
fn test_precondition_base_dig_range_is_capped_after_modifiers() {
    let range = layer_at(10).jc_range;
    let modified = (
        range.0.min(BASE_DIG_JC_PAYOUT_CAP),
        200_i64.min(BASE_DIG_JC_PAYOUT_CAP),
    );
    assert!(modified.0 <= 20 && modified.1 <= 20);
}

#[test]
fn test_overgrowth_bonus_is_added_after_base_payout_scaling() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 25);
    input.overgrowth_bonus = 10;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(20) + 10);
    assert_eq!(result.overgrowth_bonus, 10);
}

#[test]
fn test_prospectors_streak_relic_is_included_in_base_payout_cap() {
    let mut tunnel = started_tunnel(10);
    let result = apply_dig_outcome(&mut tunnel, outcome(1, 15 + 20), 2_000_000);
    assert_eq!(result.gross_jc, 20);
}

#[test]
fn test_streak_bonus_is_capped_after_perk_multiplier() {
    assert_eq!(streak_bonus(30), STREAKS[3].1);
    assert_eq!(streak_bonus(300), DIG_STREAK_JC_PAYOUT_CAP);
}

#[test]
fn test_dynamite_cache_yield_buff_boosts_loot_and_lifts_cap() {
    let mut plain = started_tunnel(10);
    let plain_result = apply_dig_outcome(&mut plain, outcome(1, 12), 2_000_000);
    let mut buffed = started_tunnel(10);
    let mut buffed_input = outcome(1, 12);
    buffed_input.yield_buff_percent = 175;
    let buffed_result = apply_dig_outcome(&mut buffed, buffed_input, 2_000_000);
    assert_eq!(plain_result.gross_jc, 12);
    assert_eq!(buffed_result.gross_jc, 21);
    let mut huge = started_tunnel(10);
    buffed_input.gross_jc = 100;
    assert_eq!(
        apply_dig_outcome(&mut huge, buffed_input, 2_000_000).gross_jc,
        35
    );
}

#[test]
fn test_dig_increments_total_digs() {
    let mut tunnel = started_tunnel(10);
    apply_dig_outcome(&mut tunnel, outcome(1, 0), 2_000_000);
    assert_eq!(tunnel.total_digs, 2);
}

#[test]
fn test_dig_updates_last_dig_at() {
    let mut tunnel = started_tunnel(10);
    apply_dig_outcome(&mut tunnel, outcome(1, 0), 2_000_000);
    assert_eq!(tunnel.last_dig_at, Some(2_000_000));
}

#[test]
fn test_dig_not_registered() {
    fn require_registered(registered: bool) -> Result<(), &'static str> {
        registered
            .then_some(())
            .ok_or("You need to register first.")
    }
    assert!(require_registered(false).is_err());
}

#[test]
fn test_dig_cooldown_blocks_free_dig() {
    assert_eq!(cooldown_remaining(Some(1_000_000), 1_001_800, 3_600), 1_800);
}

#[test]
fn dig_bankruptcy_cooldown_base_is_exactly_one_hour() {
    assert_eq!(FREE_DIG_COOLDOWN_SECONDS, 3_600);
}

#[test]
fn dig_bankruptcy_cooldown_bankrupt_player_uses_flat_cooldown() {
    // Bankruptcy is intentionally absent from the cooldown policy surface.
    let _bankruptcy_penalty_games_remaining = 5;
    assert_eq!(effective_dig_cooldown(0, None, None, None, 0), 3_600);
}

#[test]
fn dig_bankruptcy_cooldown_non_bankrupt_player_uses_flat_cooldown() {
    let _bankruptcy_penalty_games_remaining = 0;
    assert_eq!(effective_dig_cooldown(0, None, None, None, 0), 3_600);
}

#[test]
fn dig_bankruptcy_cooldown_injury_override_is_not_halved() {
    let injury = r#"{"type":"slower_cooldown"}"#;
    assert_eq!(
        effective_dig_cooldown(0, Some(RESTLESS), Some(injury), None, 0),
        6 * 3_600
    );
}

#[test]
fn dig_bankruptcy_cooldown_malformed_injury_state_does_not_crash() {
    assert_eq!(
        effective_dig_cooldown(0, None, Some("{not valid json"), None, 0),
        3_600
    );
}

#[test]
fn test_stamina_13_caps_cooldown_after_mutations_plain() {
    assert_eq!(effective_dig_cooldown(13, None, None, None, 0), 1_800);
}

#[test]
fn test_stamina_13_caps_cooldown_after_mutations_restless() {
    assert_eq!(
        effective_dig_cooldown(13, Some(RESTLESS), None, None, 0),
        3_600
    );
}

const RESTLESS: &str = r#"[{"id":"restless"}]"#;

#[test]
fn test_cooldown_curse_applies_after_stamina_and_caps_at_twenty_five_percent() {
    let over_cap = r#"{"digs_remaining":2,"effect":{"cooldown_penalty":0.30}}"#;
    assert_eq!(
        effective_dig_cooldown(0, None, None, Some(over_cap), 0),
        4_500
    );
}

#[test]
fn test_cooldown_curse_reads_legacy_effects_key_and_ignores_expired_curses() {
    let legacy = r#"{"digs_remaining":2,"effects":{"cooldown_penalty":0.20}}"#;
    let expired = r#"{"digs_remaining":0,"effect":{"cooldown_penalty":0.20}}"#;
    assert_eq!(
        effective_dig_cooldown(0, None, None, Some(legacy), 0),
        4_320
    );
    assert_eq!(
        effective_dig_cooldown(0, None, None, Some(expired), 0),
        3_600
    );
}

#[test]
fn test_mana_reduction_runs_after_the_curse_and_floors_at_zero() {
    let curse = r#"{"digs_remaining":1,"effect":{"cooldown_penalty":0.25}}"#;
    assert_eq!(
        effective_dig_cooldown(0, None, None, Some(curse), 30),
        4_470
    );
    assert_eq!(effective_dig_cooldown(0, None, None, None, i64::MAX), 0);
}

#[test]
fn test_forest_cooldown_is_ready_at_3570_seconds() {
    let duration = effective_dig_cooldown(0, None, None, None, 30);
    assert_eq!(duration, 3_570);
    assert_eq!(cooldown_remaining(Some(1_000_000), 1_003_569, duration), 1);
    assert_eq!(cooldown_remaining(Some(1_000_000), 1_003_570, duration), 0);
}

#[test]
fn test_dig_cooldown_allows_paid_dig() {
    let mut balance = 200;
    let cost = paid_dig_cost(0, 0, 0.0);
    balance -= cost;
    assert_eq!(cost, 3);
    assert_eq!(balance, 197);
}

#[test]
fn test_paid_dig_escalating_cost() {
    assert_eq!(
        (0..PAID_DIG_COSTS_PER_DAY.len())
            .map(|index| paid_dig_cost(index, 0, 0.0))
            .collect::<Vec<_>>(),
        PAID_DIG_COSTS_PER_DAY
    );
}

#[test]
fn test_paid_dig_cost_resets_daily() {
    let mut tunnel = started_tunnel(10);
    tunnel.paid_dig_day = Some(1);
    tunnel.paid_digs_today = 1;
    let today = 2;
    let count = if tunnel.paid_dig_day == Some(today) {
        tunnel.paid_digs_today
    } else {
        0
    };
    assert_eq!(paid_dig_cost(count, 0, 0.0), 3);
}

#[test]
fn test_paid_dig_insufficient_funds() {
    let balance = 0;
    assert!(balance < paid_dig_cost(0, 0, 0.0));
}

#[test]
fn test_set_profile_and_stats() {
    let story = sanitize_backstory("Former cartographer @everyone who fears ceilings.", 200);
    assert!(story.contains("(at)everyone"));
}

#[test]
fn test_set_miner_auto_buy_settings() {
    let mut settings = BTreeMap::from([("torch", false), ("hard_hat", false)]);
    settings.insert("torch", true);
    settings.insert("hard_hat", true);
    settings.insert("torch", false);
    assert_eq!(
        settings,
        BTreeMap::from([("torch", false), ("hard_hat", true)])
    );
}

#[test]
fn test_backstory_can_only_be_set_once() {
    let mut backstory: Option<String> = None;
    let first = sanitize_backstory("Escaped from a mushroom commune.", 200);
    if backstory.is_none() {
        backstory = Some(first);
    }
    let second_allowed = backstory.is_none();
    assert!(!second_allowed);
}

#[test]
fn test_profile_created_tunnel_still_gets_first_dig() {
    let mut tunnel = TunnelState::default();
    let _profile_story = sanitize_backstory("Keeps receipts for every rock.", 200);
    let result = apply_first_dig(&mut tunnel, 3, 1, 1, 1_000_000);
    assert!(result.is_first_dig);
    assert!(!result.cave_in);
}

#[test]
fn test_strength_and_smarts_affect_preconditions() {
    let strength = miner_stat_effects(MinerStats::new(5, 0, 0, 0).expect("valid stats"));
    let smarts = miner_stat_effects(MinerStats::new(0, 5, 0, 0).expect("valid stats"));
    assert_eq!(strength.advance_min_bonus, 1);
    assert_eq!(strength.advance_max_bonus, 2);
    assert_close((0.05 - smarts.cave_in_reduction).max(0.01), 0.01);
}

#[test]
fn test_stamina_reduces_cooldown_and_paid_cost() {
    assert!(effective_dig_cooldown(5, None, None, None, 0) < FREE_DIG_COOLDOWN_SECONDS);
    assert_eq!(paid_dig_cost(0, 5, 0.0), 2);
}

#[test]
fn test_overgrowth_does_not_bypass_dig_cooldown() {
    let _overgrowth_active = true;
    assert!(cooldown_remaining(Some(1_000_000), 1_000_060, 3_600) > 0);
}

#[test]
fn test_dig_applies_blood_pact_skim_to_payout() {
    let gross = 5;
    let skim = gross.min(3);
    assert_eq!(gross - skim, 2);
}

#[test]
fn test_boss_first_clear_awards_stat_point_once() {
    let mut tunnel = TunnelState::default();
    assert!(tunnel.awarded_bosses.insert(25));
    tunnel.stats.stat_points += 1;
    assert!(!tunnel.awarded_bosses.insert(25));
    assert_eq!(tunnel.stats.stat_points, 6);
}

#[test]
fn test_prestige_resets_boss_stat_awards_for_new_run() {
    let mut tunnel = TunnelState::default();
    tunnel.awarded_bosses.insert(25);
    tunnel.stats.stat_points = 6;
    tunnel.awarded_bosses.clear();
    assert!(tunnel.awarded_bosses.insert(25));
    tunnel.stats.stat_points += 1;
    assert_eq!(tunnel.stats.stat_points, 7);
}

#[test]
fn test_legacy_global_boss_awards_do_not_block_prestiged_run() {
    let legacy_global = BTreeSet::from([25]);
    let mut current_run = BTreeSet::new();
    assert!(legacy_global.contains(&25));
    assert!(current_run.insert(25));
}

#[test]
fn test_sabotage_cooldown_per_target() {
    let first_at = 1_000_000;
    assert!(1_000_060 - first_at < SABOTAGE_COOLDOWN_SECONDS);
    assert!(1_000_000 + SABOTAGE_COOLDOWN_SECONDS + 1 - first_at > SABOTAGE_COOLDOWN_SECONDS);
}

#[test]
fn test_set_trap_free_daily() {
    let traps_set_today = 0;
    let cost = if traps_set_today == 0 { 0 } else { 5 };
    assert_eq!(cost, 0);
}

fn parked_at(depth: i64, raw: &str) -> Option<i64> {
    parked_boss_boundary(depth, &defeated_boundaries_from_json(raw))
}

#[test]
fn parked_boss_boundary_uses_only_canonical_boundaries() {
    assert_eq!(parked_at(24, r#"{"25":"active"}"#), Some(25));
    assert_eq!(
        parked_at(24, r#"{"25":{"status":"phase1_defeated"}}"#),
        Some(25)
    );
    assert_eq!(
        parked_at(12, r#"{"13":"active"}"#),
        None,
        "noncanonical boundaries must not be treated as boss encounters"
    );
    assert_eq!(
        parked_at(23, "{}"),
        None,
        "one block short of the park depth"
    );
    assert_eq!(parked_at(24, r#"{"25":"defeated"}"#), None);
    assert_eq!(parked_at(49, r#"{"25":"defeated"}"#), Some(50));
    assert_eq!(
        parked_at(250, r#"{"25":"defeated","50":"defeated"}"#),
        Some(75)
    );
}

#[test]
fn parked_boss_boundary_treats_everything_but_defeated_as_unfinished() {
    // The dig gate parks a player on every boundary whose status is not
    // "defeated", so each of these shapes must open the encounter.
    assert_eq!(
        parked_at(24, "{}"),
        Some(25),
        "a fresh tunnel has no entries"
    );
    assert_eq!(
        parked_at(24, r#"{"25":{"boss_id":"grothak"}}"#),
        Some(25),
        "an entry without a status field"
    );
    assert_eq!(
        parked_at(24, r#"{"25":"pending"}"#),
        Some(25),
        "a noncanonical status"
    );
    for invalid in ["", "null", "[]", "not json", r#"{"25":7}"#] {
        assert_eq!(
            parked_at(24, invalid),
            Some(25),
            "unreadable progress {invalid:?} reads as empty, like the gate"
        );
    }

    let regular_cleared = |pinnacle: &str| {
        format!(
            r#"{{"25":"defeated","50":"defeated","75":"defeated","100":"defeated","150":"defeated","200":"defeated","275":"defeated"{pinnacle}}}"#
        )
    };
    assert_eq!(parked_at(349, &regular_cleared("")), Some(350));
    assert_eq!(parked_at(500, &regular_cleared("")), Some(350));
    assert_eq!(
        parked_at(349, &regular_cleared(r#","350":"pending""#)),
        Some(350)
    );
    assert_eq!(
        parked_at(349, &regular_cleared(r#","350":{"boss_id":"x"}"#)),
        Some(350)
    );
    assert_eq!(
        parked_at(349, &regular_cleared(r#","350":"defeated""#)),
        None
    );
    assert_eq!(parked_at(348, &regular_cleared("")), None);
}

#[test]
fn defeated_boundaries_accept_flat_and_nested_shapes() {
    let defeated = defeated_boundaries_from_json(
        r#"{"25":"defeated","50":{"status":"defeated"},"75":"active","100":{"status":"phase1_defeated"},"350":"defeated","13":"defeated"}"#,
    );
    assert_eq!(defeated, BTreeSet::from([13, 25, 50, 350]));
    assert_eq!(next_undefeated_boss(&defeated), Some(75));
}

#[test]
fn test_boss_blocks_advancement() {
    let result = apply_boss_gate(24, 3, &BTreeSet::new());
    assert_eq!(result.depth_after, 24);
    assert_eq!(result.boss_encounter, Some(25));
}

#[test]
fn test_event_does_not_skip_boss_boundary() {
    let result = apply_boss_gate(24, 18, &BTreeSet::new());
    assert_eq!(result.advance, 0);
    assert_eq!(result.depth_after, 24);
    assert_eq!(result.boss_encounter, Some(25));
}

#[test]
fn test_boss_retreat() {
    let depth = 24;
    let retreat_loss = 2_i64.clamp(1, 3);
    assert!((1..=3).contains(&(depth - (depth - retreat_loss))));
}

#[test]
fn test_boss_all_defeated_enables_prestige() {
    let partial = BOSS_BOUNDARIES[..3]
        .iter()
        .copied()
        .map(i64::from)
        .collect::<BTreeSet<_>>();
    assert_ne!(next_undefeated_boss(&partial), None);
    let all = BOSS_BOUNDARIES
        .into_iter()
        .chain([PINNACLE_DEPTH])
        .map(i64::from)
        .collect::<BTreeSet<_>>();
    assert_eq!(next_undefeated_boss(&all), None);
}

#[test]
fn test_streak_bonus_at_thresholds() {
    assert_eq!(streak_bonus(3), 1);
    assert_eq!(streak_bonus(7), 3);
    assert_eq!(streak_bonus(14), 6);
    assert_eq!(streak_bonus(30), 10);
}

#[test]
fn test_bankruptcy_debuff_shared_dig_profit_helper() {
    let penalized = apply_jc_profit_policies(
        100,
        DigProfitPolicy {
            bankruptcy_penalty_bps_per_game: 2_500,
            bankruptcy_penalty_games_remaining: 1,
            vanity_tax_basis_points: 0,
            low_priority_tax_basis_points: 0,
        },
    );
    assert_eq!(
        penalized,
        DigProfitSettlement {
            gross: 100,
            net: 75,
            bankruptcy_penalty: 25,
            vanity_tax: 0,
            low_priority_tax: 0,
        }
    );
    assert_eq!(
        apply_jc_profit_policies(100, DigProfitPolicy::default()).net,
        100
    );
    assert_eq!(
        apply_jc_profit_policies(
            0,
            DigProfitPolicy {
                bankruptcy_penalty_bps_per_game: 2_500,
                bankruptcy_penalty_games_remaining: 1,
                vanity_tax_basis_points: 0,
                low_priority_tax_basis_points: 0,
            },
        ),
        DigProfitSettlement::default()
    );
}

#[test]
fn test_bankruptcy_debuff_dig_vanity_tax_applies_independently() {
    assert_eq!(
        apply_jc_profit_policies(
            250,
            DigProfitPolicy {
                bankruptcy_penalty_bps_per_game: 0,
                bankruptcy_penalty_games_remaining: 1,
                vanity_tax_basis_points: 1_000,
                low_priority_tax_basis_points: 0,
            },
        ),
        DigProfitSettlement {
            gross: 250,
            net: 225,
            bankruptcy_penalty: 0,
            vanity_tax: 25,
            low_priority_tax: 0,
        }
    );
}

#[test]
fn test_low_priority_tax_stacks_additively_on_the_same_profit_basis() {
    assert_eq!(
        apply_jc_profit_policies(
            250,
            DigProfitPolicy {
                bankruptcy_penalty_bps_per_game: 0,
                bankruptcy_penalty_games_remaining: 1,
                vanity_tax_basis_points: 1_000,
                low_priority_tax_basis_points: 1_000,
            },
        ),
        DigProfitSettlement {
            gross: 250,
            net: 200,
            bankruptcy_penalty: 0,
            vanity_tax: 25,
            low_priority_tax: 25,
        }
    );
}

#[test]
fn test_combined_withholding_never_exceeds_the_profit_basis() {
    let settlement = apply_jc_profit_policies(
        100,
        DigProfitPolicy {
            bankruptcy_penalty_bps_per_game: 9_000,
            bankruptcy_penalty_games_remaining: 1,
            vanity_tax_basis_points: 1_000,
            low_priority_tax_basis_points: 1_000,
        },
    );
    assert_eq!(settlement.bankruptcy_penalty, 90);
    assert_eq!(settlement.vanity_tax, 10);
    assert_eq!(settlement.low_priority_tax, 0);
    assert_eq!(settlement.net, 0);
}

#[test]
fn test_bankruptcy_debuff_normal_dig_routes_yield_but_first_dig_is_exempt() {
    let policy = DigProfitPolicy {
        bankruptcy_penalty_bps_per_game: 2_500,
        bankruptcy_penalty_games_remaining: 1,
        vanity_tax_basis_points: 0,
        low_priority_tax_basis_points: 0,
    };
    let mut welcome = TunnelState::default();
    let welcome_before = welcome.balance;
    let welcome_result = apply_first_dig(&mut welcome, 3, 5, 5, 1_000_000);
    assert_eq!(welcome.balance - welcome_before, welcome_result.jc_earned);

    let mut ordinary = welcome.clone();
    let mut penalized = welcome;
    let ordinary_result = apply_dig_outcome(&mut ordinary, outcome(1, 20), 2_000_000);
    let mut penalized_input = outcome(1, 20);
    penalized_input.profit_policy = policy;
    let penalized_result = apply_dig_outcome(&mut penalized, penalized_input, 2_000_000);
    let expected_penalty = ordinary_result.jc_earned * 2_500 / 10_000;
    assert_eq!(penalized_result.bankruptcy_penalty, expected_penalty);
    assert_eq!(
        penalized_result.jc_earned + penalized_result.bankruptcy_penalty,
        ordinary_result.jc_earned
    );
    assert_eq!(penalized.balance - ordinary.balance, -expected_penalty);
}

#[test]
fn test_upgrade_pickaxe_requirements() {
    let stone = PICKAXE_TIERS[1];
    let cannot_upgrade = 10 < stone.depth_required;
    let can_upgrade = 25 >= stone.depth_required && 500 >= stone.jc_cost;
    assert!(cannot_upgrade);
    assert!(can_upgrade);
}

#[test]
fn test_pickaxe_advance_bonus() {
    assert_eq!(PICKAXE_TIERS[1].advance_bonus, 1);
    assert!(layer_at(10).advance_range.0 + PICKAXE_TIERS[1].advance_bonus >= 2);
}

#[test]
fn test_scout_boss_shows_configured_odds() {
    let cautious_base = 0.75_f64;
    let depth_penalty = 25.0 / 100.0 * 0.05;
    let configured = cautious_base - depth_penalty;
    assert!(configured > 0.70);
}

#[test]
fn test_fight_boss_reckless_high_roll_loses() {
    let reckless_hit_chance = 0.20;
    assert!(0.99 > reckless_hit_chance);
}

#[test]
fn test_fight_boss_cautious_low_roll_wins() {
    let cautious_hit_chance = 0.75;
    assert!(0.01 < cautious_hit_chance);
}

#[test]
fn test_eight_layers_exist() {
    assert_eq!(LAYERS.len(), 8);
    assert!(LAYERS.iter().any(|layer| layer.name == "Fungal Depths"));
    assert!(LAYERS.iter().any(|layer| layer.name == "Frozen Core"));
    assert!(LAYERS.iter().any(|layer| layer.name == "The Hollow"));
}

#[test]
fn test_abyss_now_capped() {
    assert_eq!(
        LAYERS
            .iter()
            .find(|layer| layer.name == "Abyss")
            .and_then(|layer| layer.max_depth),
        Some(150)
    );
}

#[test]
fn test_hollow_is_unbounded() {
    assert_eq!(
        LAYERS
            .iter()
            .find(|layer| layer.name == "The Hollow")
            .and_then(|layer| layer.max_depth),
        None
    );
}

#[test]
fn test_get_layer_returns_new_layers() {
    assert_eq!(layer_at(160).name, "Fungal Depths");
    assert_eq!(layer_at(250).name, "Frozen Core");
    assert_eq!(layer_at(300).name, "The Hollow");
}

#[test]
fn test_new_bosses_exist() {
    assert_eq!(BOSS_BOUNDARIES.len(), 7);
    assert!(BOSS_BOUNDARIES.contains(&150));
    assert!(BOSS_BOUNDARIES.contains(&200));
    assert!(BOSS_BOUNDARIES.contains(&275));
}

#[test]
fn test_new_milestones() {
    let depths = MILESTONES
        .iter()
        .map(|(depth, _)| *depth)
        .collect::<BTreeSet<_>>();
    assert!(depths.is_superset(&BTreeSet::from([150, 200, 275])));
}

#[test]
fn test_luminosity_starts_at_100() {
    assert_eq!(TunnelState::default().luminosity, 100);
    assert_eq!(layer_at(0).luminosity_drain, 0);
}

#[test]
fn test_luminosity_drains_in_magma() {
    let before = 100;
    let drain = layer_at(80).luminosity_drain;
    assert_eq!(drain, 3);
    assert_eq!(before - drain, 97);
}

#[test]
fn test_luminosity_level_thresholds() {
    assert_eq!(luminosity_level(100), LuminosityLevel::Bright);
    assert_eq!(luminosity_level(76), LuminosityLevel::Bright);
    assert_eq!(luminosity_level(75), LuminosityLevel::Dim);
    assert_eq!(luminosity_level(26), LuminosityLevel::Dim);
    assert_eq!(luminosity_level(25), LuminosityLevel::Dark);
    assert_eq!(luminosity_level(1), LuminosityLevel::Dark);
    assert_eq!(luminosity_level(0), LuminosityLevel::PitchBlack);
}

#[test]
fn test_set_and_get_buff() {
    let buff = TempBuff {
        id: "test_buff",
        digs_remaining: 3,
        advance_bonus: 2,
        yield_percent: 100,
    };
    assert!(buff.active());
    assert_eq!(buff.id, "test_buff");
    assert_eq!(buff.digs_remaining, 3);
}

#[test]
fn test_buff_applies_advance_bonus() {
    let buff = TempBuff {
        id: "power",
        digs_remaining: 2,
        advance_bonus: 5,
        yield_percent: 100,
    };
    assert!(layer_at(10).advance_range.0 + buff.advance_bonus >= 6);
}

#[test]
fn test_buff_decrements() {
    let mut tunnel = started_tunnel(5);
    tunnel.buff = Some(TempBuff {
        id: "short",
        digs_remaining: 1,
        advance_bonus: 1,
        yield_percent: 100,
    });
    apply_dig_outcome(&mut tunnel, outcome(1, 0), 2_000_000);
    assert_eq!(tunnel.buff, None);
}

#[test]
fn test_fight_boss_error_has_no_won_key() {
    #[derive(Debug, Eq, PartialEq)]
    struct BossError {
        success: bool,
        error: &'static str,
    }
    let error = BossError {
        success: false,
        error: "You are not at a boss boundary.",
    };
    assert!(!error.success);
    assert!(!format!("{error:?}").contains("won:"));
}

#[test]
fn test_boss_boundary_preserves_last_dig_at() {
    let tunnel = started_tunnel(24);
    let before = tunnel.last_dig_at;
    let parked = apply_boss_gate(tunnel.depth, 0, &tunnel.defeated_bosses);
    assert_eq!(parked.boss_encounter, Some(25));
    assert_eq!(tunnel.last_dig_at, before);
}

#[test]
fn test_newly_reached_boss_marks_dig_consumed() {
    let result = apply_boss_gate(23, 3, &BTreeSet::new());
    assert_eq!(result.boss_encounter, Some(25));
    assert!(
        result.advance > 0,
        "a positive advance means the dig was consumed"
    );
}

#[test]
fn test_reaching_pinnacle_surfaces_boss_on_arrival() {
    let defeated = BOSS_BOUNDARIES
        .into_iter()
        .map(i64::from)
        .collect::<BTreeSet<_>>();
    let pinnacle = i64::from(PINNACLE_DEPTH);
    let result = apply_boss_gate(pinnacle - 2, 20, &defeated);
    assert_eq!(result.boss_encounter, Some(pinnacle));
    assert_eq!(result.depth_after, pinnacle - 1);
    assert!(result.advance > 0);
}

#[test]
fn test_boss_boundary_returns_full_info() {
    #[derive(Debug, Eq, PartialEq)]
    struct BossInfo {
        boundary: i64,
        name: &'static str,
        dialogue: &'static str,
        ascii_art: &'static str,
    }
    let info = BossInfo {
        boundary: 25,
        name: "Grothak",
        dialogue: "The tunnel trembles.",
        ascii_art: "<boss>",
    };
    assert_eq!(info.boundary, 25);
    assert!(!info.name.is_empty() && !info.dialogue.is_empty() && !info.ascii_art.is_empty());
}

#[test]
fn test_parked_dig_ignores_cooldown() {
    let balance = 200;
    for paid in [false, false, true] {
        let parked = apply_boss_gate(24, 0, &BTreeSet::new());
        assert_eq!(parked.boss_encounter, Some(25));
        assert_eq!(parked.advance, 0);
        assert!(!(paid && parked.advance > 0));
    }
    assert_eq!(balance, 200);
}

#[test]
fn test_parked_dig_ignores_cooldown_preconditions_path() {
    let terminal = apply_boss_gate(24, 0, &BTreeSet::new());
    assert_eq!(terminal.boss_encounter, Some(25));
    assert_eq!(terminal.advance, 0);
}

#[test]
fn test_direct_dig_caps_advance_after_all_bonuses() {
    let mut tunnel = started_tunnel(1);
    let result = apply_dig_outcome(&mut tunnel, outcome(51, 0), 2_000_000);
    assert_eq!(result.advance, 10);
    assert_eq!(result.depth_after, 11);
}

#[test]
fn test_advertised_advance_range_is_capped_after_all_bonuses() {
    let stats = MinerAllocation::default();
    let advertised = (
        capped_advance(51, stats, false, false),
        capped_advance(53, stats, false, false),
    );
    assert_eq!(advertised, (10, 10));
}

macro_rules! explosive_advertised_cap {
    ($name:ident, $dynamite:expr, $depth_charge:expr) => {
        #[test]
        fn $name() {
            let stats = MinerAllocation::default();
            assert_eq!(capped_advance(51, stats, $dynamite, $depth_charge), 20);
            assert_eq!(capped_advance(53, stats, $dynamite, $depth_charge), 20);
        }
    };
}

explosive_advertised_cap!(
    test_queued_explosive_advertises_twenty_block_cap_dynamite,
    true,
    false
);
explosive_advertised_cap!(
    test_queued_explosive_advertises_twenty_block_cap_depth_charge,
    false,
    true
);

#[test]
fn test_direct_dig_with_queued_dynamite_caps_at_twenty_blocks() {
    let mut tunnel = started_tunnel(1);
    let mut input = outcome(51, 0);
    input.dynamite = true;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.advance, 20);
    assert_eq!(result.depth_after, 21);
}

#[test]
fn test_direct_dig_with_strength_caps_at_twenty_blocks() {
    let mut tunnel = started_tunnel(1);
    tunnel.stats.strength = 5;
    let result = apply_dig_outcome(&mut tunnel, outcome(51, 0), 2_000_000);
    assert_eq!(result.advance, 20);
    assert_eq!(result.depth_after, 21);
}

#[test]
fn test_authored_event_depth_is_not_capped() {
    let mut tunnel = started_tunnel(101);
    tunnel.defeated_bosses = [25, 50, 75, 100].into_iter().collect();
    let mut input = outcome(18, 0);
    input.authored_event = true;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.advance, 18);
    assert_eq!(result.depth_after, 119);
}

#[test]
fn test_max_depth_advances_on_dm_dig() {
    let mut tunnel = started_tunnel(10);
    apply_dig_outcome(&mut tunnel, outcome(5, 0), 2_000_000);
    assert_eq!(tunnel.max_depth, 15);
}

#[test]
fn test_dm_dig_caps_main_advance_at_10() {
    let mut tunnel = started_tunnel(1);
    let result = apply_dig_outcome(&mut tunnel, outcome(50, 0), 2_000_000);
    assert_eq!(result.advance, 10);
    assert_eq!(result.depth_after, 11);
}

#[test]
fn test_dm_dig_with_strength_bonus_caps_advance_at_20() {
    let mut tunnel = started_tunnel(1);
    tunnel.stats.strength = 2;
    let result = apply_dig_outcome(&mut tunnel, outcome(50, 0), 2_000_000);
    assert_eq!(result.advance, 20);
    assert_eq!(result.depth_after, 21);
}

#[test]
fn test_max_depth_does_not_regress_on_dm_cave_in() {
    let mut tunnel = started_tunnel(20);
    let mut input = outcome(0, 0);
    input.cave_in_loss = 5;
    apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(tunnel.depth, 15);
    assert_eq!(tunnel.max_depth, 20);
}

#[test]
fn test_milestone_not_re_awarded_after_knockback() {
    let mut tunnel = started_tunnel(20);
    tunnel.max_depth = 30;
    let result = apply_dig_outcome(&mut tunnel, outcome(10, 0), 2_000_000);
    assert_eq!(result.milestone_bonus, 0);
}

#[test]
fn test_helltide_tax_applied_on_dm_path() {
    let mut tunnel = started_tunnel(10);
    let before = tunnel.balance;
    let mut input = outcome(1, 10);
    input.helltide_tax = 5;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(tunnel.balance - before, result.jc_earned);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(10) - 6);
}

#[test]
fn test_dm_dig_scales_generated_jc_before_mana_taxes() {
    let mut tunnel = started_tunnel(10);
    let result = apply_dig_outcome(&mut tunnel, outcome(1, 20), 2_000_000);
    assert_eq!(result.gross_jc, 20);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(20));
}

#[test]
fn test_base_dig_payout_cap_applies_on_dm_path() {
    let mut tunnel = started_tunnel(10);
    let before = tunnel.balance;
    let result = apply_dig_outcome(&mut tunnel, outcome(1, 25), 2_000_000);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(20));
    assert_eq!(tunnel.balance, before + result.jc_earned);
}

#[test]
fn test_dm_overgrowth_bonus_is_added_after_base_payout_scaling() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 25);
    input.overgrowth_bonus = 10;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.gross_jc, 20);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(20) + 10);
    assert_eq!(result.overgrowth_bonus, 10);
}

#[test]
fn test_base_dig_payout_cap_applies_after_dm_weather_combo() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 20);
    input.weather_yield_percent = 200;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.gross_jc, 20);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(20));
}

#[test]
fn test_weather_combo_applied_on_dm_path() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 10);
    input.weather_yield_percent = 200;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.gross_jc, 20);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(20));
}

#[test]
fn test_max_depth_advances_on_deterministic_success() {
    let mut tunnel = started_tunnel(10);
    apply_dig_outcome(&mut tunnel, outcome(1, 0), 2_000_000);
    assert!(tunnel.max_depth >= 10);
}

#[test]
fn test_deterministic_dig_caps_advance_after_all_bonuses() {
    let mut tunnel = started_tunnel(1);
    let result = apply_dig_outcome(&mut tunnel, outcome(51, 0), 2_000_000);
    assert_eq!(result.advance, 10);
    assert_eq!(result.depth_after, 11);
}

#[test]
fn test_deterministic_dig_with_strength_bonus_caps_at_twenty_blocks() {
    let mut tunnel = started_tunnel(1);
    tunnel.stats.strength = 2;
    let result = apply_dig_outcome(&mut tunnel, outcome(51, 0), 2_000_000);
    assert_eq!(result.advance, 20);
    assert_eq!(result.depth_after, 21);
}

#[test]
fn test_milestone_not_re_awarded_after_knockback_deterministic() {
    let mut tunnel = started_tunnel(20);
    tunnel.max_depth = 30;
    let result = apply_dig_outcome(&mut tunnel, outcome(10, 0), 2_000_000);
    assert_eq!(result.milestone_bonus, 0);
}

#[test]
fn test_helltide_tax_applied_on_deterministic_path() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 10);
    input.helltide_tax = 1;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert!(result.jc_earned < scale_positive_dig_jc(10));
}

#[test]
fn test_deterministic_overgrowth_bonus_is_added_after_base_payout_scaling() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 25);
    input.overgrowth_bonus = 10;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(20) + 10);
    assert_eq!(result.overgrowth_bonus, 10);
}

#[test]
fn test_live_dig_scales_full_positive_payout_before_sinks() {
    let mut tunnel = started_tunnel(10);
    let mut input = outcome(1, 15);
    input.overgrowth_bonus = 4;
    input.helltide_tax = 3;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert_eq!(result.gross_jc, 15);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(15) + 4 - 3);
    assert_eq!(tunnel.balance, 100 + result.jc_earned);
}

#[test]
fn test_cave_in_scales_combined_positive_reward_once() {
    let mut tunnel = started_tunnel(180);
    let result = apply_dig_outcome(
        &mut tunnel,
        DigOutcomeInput {
            advance: 0,
            gross_jc: 10,
            cave_in_loss: 12,
            ..DigOutcomeInput::default()
        },
        2_000_000,
    );
    assert!(result.cave_in);
    assert_eq!(result.gross_jc, 10);
    assert_eq!(result.jc_earned, scale_positive_dig_jc(10));
    assert_eq!(tunnel.balance, 100 + scale_positive_dig_jc(10));
}

#[test]
fn bankruptcy_share_scales_with_games_remaining_like_every_other_surface() {
    let policy = DigProfitPolicy {
        bankruptcy_penalty_bps_per_game: 500,
        bankruptcy_penalty_games_remaining: 1,
        ..DigProfitPolicy::default()
    };
    // Every outstanding game withholds a flat 5%, truncated from the f64
    // product exactly as bets, predictions, and match income do.
    assert_eq!(
        apply_jc_profit_policies(30_000, policy).bankruptcy_penalty,
        (30_000.0 * cama_domain::bankruptcy::withheld_rate(0.05, 1)) as i64
    );
    assert_eq!(apply_jc_profit_policies(120, policy).bankruptcy_penalty, 6);
    // 1 -> 5%, 3 -> 15%, 6 -> 30%, 20+ -> 100%; no games left withholds nothing.
    for (remaining, expected) in [(0, 0), (3, 18), (6, 36), (20, 120), (40, 120)] {
        assert_eq!(
            apply_jc_profit_policies(
                120,
                DigProfitPolicy {
                    bankruptcy_penalty_games_remaining: remaining,
                    ..policy
                }
            )
            .bankruptcy_penalty,
            expected,
            "{remaining} games remaining"
        );
    }
}
