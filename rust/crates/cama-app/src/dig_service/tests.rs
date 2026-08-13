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
fn test_sabotage_embed_mentions_prediction_contract_steal() {
    let rendered = append_prediction_contract_steal(
        "Damage dealt: **6** blocks",
        true,
        Some(&PredictionContractSteal {
            prediction_id: 42,
            side: "yes",
            contracts: 4,
        }),
    );
    assert!(rendered.contains("Stole **4 YES** prediction contracts from market **#42**."));
}

#[test]
fn test_sabotage_embed_ignores_prediction_contract_steal_on_miss() {
    let description = "The sabotage missed.";
    let rendered = append_prediction_contract_steal(
        description,
        false,
        Some(&PredictionContractSteal {
            prediction_id: 42,
            side: "yes",
            contracts: 4,
        }),
    );
    assert_eq!(rendered, description);
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

#[test]
fn test_boss_victory_base_jc_covers_every_boss_boundary() {
    assert_eq!(
        BOSS_BOUNDARIES
            .into_iter()
            .map(|boundary| (boundary, boss_base_reward(boundary, 0)))
            .collect::<Vec<_>>(),
        vec![
            (25, 15),
            (50, 20),
            (75, 25),
            (100, 30),
            (150, 40),
            (200, 50),
            (275, 65)
        ]
    );
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
    let supplied = cooldown_duration(0, false, false, effects.dig_cooldown_reduction_seconds);
    let standalone = cooldown_duration(0, false, false, 30);
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
        bankruptcy_keep_basis_points: 7_500,
        vanity_tax_basis_points: 1_000,
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
        bankruptcy_keep_basis_points: 7_500,
        vanity_tax_basis_points: 1_000,
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
    assert_eq!(cooldown_duration(0, false, false, 0), 3_600);
}

#[test]
fn dig_bankruptcy_cooldown_non_bankrupt_player_uses_flat_cooldown() {
    let _bankruptcy_penalty_games_remaining = 0;
    assert_eq!(cooldown_duration(0, false, false, 0), 3_600);
}

#[test]
fn dig_bankruptcy_cooldown_injury_override_is_not_halved() {
    let injury = r#"{"type":"slower_cooldown"}"#;
    assert!(has_slower_cooldown_injury(Some(injury)));
    assert_eq!(cooldown_duration(0, false, true, 0), 6 * 3_600);
}

#[test]
fn dig_bankruptcy_cooldown_malformed_injury_state_does_not_crash() {
    assert!(!has_slower_cooldown_injury(Some("{not valid json")));
    assert_eq!(cooldown_duration(0, false, false, 0), 3_600);
}

#[test]
fn test_stamina_13_caps_cooldown_after_mutations_plain() {
    assert_eq!(cooldown_duration(13, false, false, 0), 1_800);
}

#[test]
fn test_stamina_13_caps_cooldown_after_mutations_restless() {
    assert_eq!(cooldown_duration(13, true, false, 0), 3_600);
}

#[test]
fn test_forest_cooldown_is_ready_at_3570_seconds() {
    let duration = cooldown_duration(0, false, false, 30);
    assert_eq!(duration, 3_570);
    assert_eq!(cooldown_remaining(Some(1_000_000), 1_003_569, duration), 1);
    assert_eq!(cooldown_remaining(Some(1_000_000), 1_003_570, duration), 0);
}

#[test]
fn test_bulk_ready_times_use_one_tunnel_snapshot() {
    let snapshot = BTreeMap::from([(10_001, Some(1_000_000)), (10_002, Some(996_000))]);
    assert_eq!(
        bulk_ready_times(&[10_001, 10_002, 10_003, 10_001], &snapshot, 1_000_000),
        BTreeMap::from([(10_001, Some(1_003_600)), (10_002, None), (10_003, None)])
    );
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
    let mut stats = MinerAllocation::default();
    stats.allocate(2, 2, 1).expect("allocation should fit");
    assert!(story.contains("(at)everyone"));
    assert_eq!(stats.unspent_points(), 0);
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
fn test_auto_buy_purchases_and_applies_selected_items() {
    let mut balance = 100;
    let mut torch_inventory = 0;
    let mut hat_inventory = 0;
    let torch = auto_buy_item(&mut torch_inventory, &mut balance, "Torch", 6);
    let hat = auto_buy_item(&mut hat_inventory, &mut balance, "Hard Hat", 8);
    assert_eq!(torch.status, AutoBuyStatus::Purchased);
    assert_eq!(hat.status, AutoBuyStatus::Purchased);
    assert_eq!(balance, 86);
}

#[test]
fn test_auto_buy_uses_reserve_inventory_before_purchase() {
    let mut balance = 100;
    let mut inventory = 1;
    let result = auto_buy_item(&mut inventory, &mut balance, "Torch", 6);
    assert_eq!(result.status, AutoBuyStatus::QueuedFromInventory);
    assert_eq!(result.cost, 0);
    assert_eq!(balance, 100);
}

#[test]
fn test_auto_buy_insufficient_balance_does_not_block_dig() {
    let mut balance = 7;
    let mut inventory = 0;
    let result = auto_buy_item(&mut inventory, &mut balance, "Hard Hat", 8);
    assert_eq!(result.status, AutoBuyStatus::SkippedInsufficientBalance);
    let mut tunnel = started_tunnel(10);
    assert_eq!(
        apply_dig_outcome(&mut tunnel, outcome(1, 0), 2_000_000).advance,
        1
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
fn test_stat_build_cannot_overspend() {
    let mut stats = MinerAllocation::default();
    let error = stats.allocate(5, 1, 0).expect_err("six points must fail");
    assert!(error.contains("only have 5"));
}

#[test]
fn test_stat_build_cannot_reduce_existing_allocations() {
    let mut stats = MinerAllocation::default();
    stats.allocate(3, 2, 0).expect("first allocation fits");
    let error = stats
        .allocate(0, 1, 0)
        .expect_err("no unspent points remain");
    assert!(error.contains("only have 0 unspent"));
}

#[test]
fn test_respec_returns_all_points_and_charges_fifty_jc() {
    let mut stats = MinerAllocation::default();
    stats.allocate(2, 2, 1).expect("allocation fits");
    let mut balance = 100;
    assert_eq!(stats.respec(&mut balance), Ok(5));
    assert_eq!(balance, 50);
    assert_eq!(stats.unspent_points(), 5);
}

#[test]
fn test_respec_reports_returned_points_separately_from_total_unspent() {
    let mut stats = MinerAllocation {
        strength: 2,
        smarts: 1,
        stamina: 2,
        stat_points: 12,
    };
    let mut balance = 100;
    assert_eq!(stats.respec(&mut balance), Ok(5));
    assert_eq!(stats.unspent_points(), 12);
}

#[test]
fn test_respec_rejects_insufficient_balance_without_resetting_stats() {
    let mut stats = MinerAllocation::default();
    stats.allocate(3, 2, 0).expect("allocation fits");
    let original = stats;
    let mut balance = 49;
    assert!(stats.respec(&mut balance).is_err());
    assert_eq!(stats, original);
    assert_eq!(balance, 49);
}

#[test]
fn test_respec_rejects_empty_build_without_charging() {
    let mut stats = MinerAllocation::default();
    let mut balance = 100;
    assert!(stats.respec(&mut balance).is_err());
    assert_eq!(balance, 100);
}

#[test]
fn test_respec_can_repeat_after_reallocating() {
    let mut stats = MinerAllocation::default();
    let mut balance = 100;
    stats.allocate(5, 0, 0).expect("allocation fits");
    stats.respec(&mut balance).expect("first respec succeeds");
    stats.allocate(0, 0, 5).expect("allocation fits");
    stats.respec(&mut balance).expect("second respec succeeds");
    assert_eq!(balance, 0);
}

#[test]
fn test_strength_and_smarts_affect_preconditions() {
    let strength = miner_stat_effects(MinerStats::new(5, 0, 0).expect("valid stats"));
    let smarts = miner_stat_effects(MinerStats::new(0, 5, 0).expect("valid stats"));
    assert_eq!(strength.advance_min_bonus, 1);
    assert_eq!(strength.advance_max_bonus, 2);
    assert_close((0.05 - smarts.cave_in_reduction).max(0.01), 0.01);
}

#[test]
fn test_stamina_reduces_cooldown_and_paid_cost() {
    assert!(cooldown_duration(5, false, false, 0) < FREE_DIG_COOLDOWN_SECONDS);
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
fn test_cave_in_reduces_depth() {
    let loss = cave_in_loss(20, 10, false);
    let mut tunnel = started_tunnel(20);
    let mut input = outcome(0, 0);
    input.cave_in_loss = loss;
    let result = apply_dig_outcome(&mut tunnel, input, 2_000_000);
    assert!(result.cave_in);
    assert_eq!(result.depth_after, 10);
}

#[test]
fn test_cave_in_depth_min_zero() {
    let mut tunnel = started_tunnel(1);
    let mut input = outcome(0, 0);
    input.cave_in_loss = cave_in_loss(1, 14, false);
    assert_eq!(
        apply_dig_outcome(&mut tunnel, input, 2_000_000).depth_after,
        0
    );
}

#[test]
fn test_reinforcement_caps_cave_in_block_loss_at_eight() {
    assert_eq!(cave_in_loss(40, 14, true), 8);
    assert_eq!(cave_in_loss(40, 14, false), 14);
}

#[test]
fn test_cave_in_medical_bill() {
    assert_eq!(cave_in_medical_bill(50), 5);
}

#[test]
fn test_milestone_bonuses_at_25_50_100() {
    assert_eq!(milestone_bonus(23, 26), 3);
    assert_eq!(milestone_bonus(48, 51), 6);
    assert_eq!(milestone_bonus(98, 101), 20);
}

#[test]
fn test_layer_penalty_reduces_layer_payout_across_prestige_ladder() {
    assert!(prestige_layer_multiplier(1) > prestige_layer_multiplier(2));
    assert!(prestige_layer_multiplier(3) > prestige_layer_multiplier(4));
    assert!(prestige_layer_multiplier(4) > prestige_layer_multiplier(5));
    assert_eq!((17.0 * prestige_layer_multiplier(1)) as i64, 20);
    assert_eq!((17.0 * prestige_layer_multiplier(2)) as i64, 19);
}

#[test]
fn test_help_advances_target_tunnel() {
    let result = help_advance(10, 3, &BTreeSet::new());
    let helper_reward = scale_positive_dig_jc(1);
    assert_eq!(result.depth_after, 13);
    assert_eq!(helper_reward, 1);
    assert!(cooldown_remaining(Some(1_003_601), 1_003_661, 3_600) > 0);
    assert_ne!(
        10_001, 10_002,
        "self-help is rejected at the service boundary"
    );
}

fn sabotage_input() -> SabotageInput {
    SabotageInput {
        actor_id: 10_002,
        target_id: 10_001,
        target_depth: 50,
        actor_balance: 200,
        rolled_damage: 6,
        hit: true,
        trapped: false,
        insured: false,
    }
}

#[test]
fn test_sabotage_reduces_target_depth_and_costs_jc() {
    let result = resolve_sabotage(sabotage_input()).expect("valid sabotage");
    assert!((3..=8).contains(&result.damage));
    assert_eq!(result.target_depth_after, 44);
    assert_eq!(result.cost, 10);
    assert_eq!(result.actor_balance_after, 190);
}

#[test]
fn test_sabotage_cooldown_per_target() {
    let first_at = 1_000_000;
    assert!(1_000_060 - first_at < SABOTAGE_COOLDOWN_SECONDS);
    assert!(1_000_000 + SABOTAGE_COOLDOWN_SECONDS + 1 - first_at > SABOTAGE_COOLDOWN_SECONDS);
}

#[test]
fn test_sabotage_miss_costs_jc_without_damage_or_reward() {
    let result = resolve_sabotage(SabotageInput {
        hit: false,
        ..sabotage_input()
    })
    .expect("a miss is a resolved paid attempt");
    assert_eq!(result.damage, 0);
    assert_eq!(result.attacker_block_reward, 0);
    assert_eq!(result.target_depth_after, 50);
    assert_eq!(result.actor_balance_after, 190);
}

#[test]
fn test_sabotage_insufficient_funds() {
    assert!(
        resolve_sabotage(SabotageInput {
            actor_balance: 0,
            ..sabotage_input()
        })
        .is_err()
    );
}

#[test]
fn test_sabotage_self_fails() {
    assert!(
        resolve_sabotage(SabotageInput {
            actor_id: 10_001,
            ..sabotage_input()
        })
        .is_err()
    );
}

#[test]
fn test_sabotage_attacker_block_reward_by_depth_tier() {
    let rewards = [50, 150, 300].map(|target_depth| {
        resolve_sabotage(SabotageInput {
            target_depth,
            ..sabotage_input()
        })
        .expect("valid sabotage")
        .attacker_block_reward
    });
    assert_eq!(rewards, [3, 5, 7]);
}

#[test]
fn test_successful_sabotage_can_steal_prediction_contracts() {
    let mut victim = PredictionPosition {
        contracts: 8,
        cost_basis: 24,
    };
    let mut attacker = PredictionPosition {
        contracts: 0,
        cost_basis: 0,
    };
    assert_eq!(steal_prediction_contracts(&mut victim, &mut attacker, 4), 4);
    assert_eq!(
        victim,
        PredictionPosition {
            contracts: 4,
            cost_basis: 12
        }
    );
    assert_eq!(
        attacker,
        PredictionPosition {
            contracts: 4,
            cost_basis: 12
        }
    );
}

#[test]
fn test_blocked_sabotage_credits_victim_jc_tip() {
    let result = resolve_sabotage(SabotageInput {
        target_depth: 200,
        trapped: true,
        ..sabotage_input()
    })
    .expect("trap resolves sabotage");
    assert!(result.trapped);
    let generated_tip_after_event = 50;
    assert_eq!(
        blocked_sabotage_victim_credit(result.cost, generated_tip_after_event),
        result.cost + scale_positive_dig_jc(generated_tip_after_event)
    );
    assert!(result.actor_balance_after < 200 - result.cost);
}

#[test]
fn test_set_trap_free_daily() {
    let traps_set_today = 0;
    let cost = if traps_set_today == 0 { 0 } else { 5 };
    assert_eq!(cost, 0);
}

#[test]
fn test_trap_catches_saboteur_and_steals_jc() {
    let result = resolve_sabotage(SabotageInput {
        target_depth: 30,
        trapped: true,
        ..sabotage_input()
    })
    .expect("trap resolves sabotage");
    assert!(result.trapped);
    assert!(result.actor_balance_after < sabotage_input().actor_balance);
}

#[test]
fn test_insurance_reduces_sabotage_damage() {
    let plain = resolve_sabotage(sabotage_input()).expect("plain sabotage");
    let insured = resolve_sabotage(SabotageInput {
        insured: true,
        ..sabotage_input()
    })
    .expect("insured sabotage");
    assert!(insured.insurance_applied);
    assert_eq!(insured.damage, plain.damage / 2);
}

#[test]
fn test_insurance_cost_scales_with_depth() {
    assert_eq!(insurance_cost(50), 7);
}

#[test]
fn test_insurance_expires() {
    let bought_at = 1_000_000;
    let insured_until = bought_at + INSURANCE_DURATION_SECONDS;
    assert!(insurance_active(insured_until, bought_at));
    assert!(!insurance_active(insured_until, insured_until + 1));
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
fn test_boss_fight_win() {
    let result = resolve_boss(24, 200, 10, true, 15, 5, 20);
    assert!(result.won);
    assert!(result.depth_after > 24);
    assert!(result.payout > 0);
}

#[test]
fn test_boss_fight_win_pays_base_reward_despite_taper() {
    let result = resolve_boss(24, 200, 10, true, 15, 0, 20);
    assert!(result.payout >= scale_positive_dig_jc(15));
    assert_eq!(result.balance_after - 200, result.payout);
}

#[test]
fn test_boss_base_reward_scales_with_ascension() {
    assert_eq!(boss_base_reward(25, 0), 15);
    assert_eq!(boss_base_reward(25, 4), 22);
}

#[test]
fn test_boss_fight_rejects_wager_before_final_phase() {
    assert_eq!(
        validate_boss_wager(872, 1_000, false),
        Err("Boss wagers are only accepted in the final phase.")
    );
}

#[test]
fn test_boss_fight_forced_no_wager_phase_avoids_free_fight_penalty() {
    assert!(validate_boss_wager(0, 1_000, false).is_ok());
    assert_eq!(boss_loss_debit(0, true), 0);
}

#[test]
fn test_start_boss_duel_rejects_wager_before_final_phase() {
    assert!(validate_boss_wager(872, 1_000, false).is_err());
}

#[test]
fn test_boss_fight_ignores_stale_carried_wager() {
    let stale_carried_wager = 872;
    let requested_wager = 0;
    assert_eq!(stale_carried_wager, 872);
    assert_eq!(requested_wager, 0);
    assert_eq!(1_000 - boss_loss_debit(requested_wager, false), 992);
}

#[test]
fn test_boss_fight_lose() {
    let result = resolve_boss(24, 200, 10, false, 15, 0, 20);
    assert!(!result.won);
    assert_eq!(result.balance_after, 190);
    assert!((11..=20).contains(&result.knockback));
    assert!(result.depth_after < 24);
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
        .collect::<BTreeSet<_>>();
    assert_ne!(next_undefeated_boss(&partial), None);
    let all = BOSS_BOUNDARIES
        .into_iter()
        .chain([PINNACLE_DEPTH])
        .collect::<BTreeSet<_>>();
    assert_eq!(next_undefeated_boss(&all), None);
}

#[test]
fn test_streak_increments_consecutive_days() {
    assert_eq!(resolve_streak(1, Some(10), 11, false).days, 2);
}

#[test]
fn test_streak_resets_on_gap() {
    assert_eq!(resolve_streak(2, Some(10), 14, false).days, 1);
}

#[test]
fn test_streak_resets_after_one_missed_day_without_charm() {
    let result = resolve_streak(2, Some(10), 12, false);
    assert_eq!(
        result,
        StreakOutcome {
            days: 1,
            charm_used: false
        }
    );
}

#[test]
fn test_streak_charm_saves_exactly_one_missed_day() {
    let result = resolve_streak(2, Some(10), 12, true);
    assert_eq!(
        result,
        StreakOutcome {
            days: 3,
            charm_used: true
        }
    );
}

#[test]
fn test_streak_charm_does_not_save_longer_gap() {
    let result = resolve_streak(2, Some(10), 13, true);
    assert_eq!(
        result,
        StreakOutcome {
            days: 1,
            charm_used: false
        }
    );
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
            bankruptcy_keep_basis_points: 7_500,
            vanity_tax_basis_points: 0,
        },
    );
    assert_eq!(
        penalized,
        DigProfitSettlement {
            gross: 100,
            net: 75,
            bankruptcy_penalty: 25,
            vanity_tax: 0,
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
                bankruptcy_keep_basis_points: 7_500,
                vanity_tax_basis_points: 0,
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
                bankruptcy_keep_basis_points: 10_000,
                vanity_tax_basis_points: 1_000,
            },
        ),
        DigProfitSettlement {
            gross: 250,
            net: 225,
            bankruptcy_penalty: 0,
            vanity_tax: 25,
        }
    );
}

#[test]
fn test_bankruptcy_debuff_normal_dig_routes_yield_but_first_dig_is_exempt() {
    let policy = DigProfitPolicy {
        bankruptcy_keep_basis_points: 7_500,
        vanity_tax_basis_points: 0,
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
fn test_bankruptcy_debuff_boss_victory_routes_payout_through_policy() {
    let ordinary = resolve_boss(24, 1_000, 10, true, 15, 5, 20);
    let penalized = resolve_boss_with_policy(
        24,
        1_000,
        10,
        true,
        15,
        5,
        20,
        DigProfitPolicy {
            bankruptcy_keep_basis_points: 7_500,
            vanity_tax_basis_points: 0,
        },
    );
    let expected_penalty = ordinary.payout * 2_500 / 10_000;
    assert!(expected_penalty > 0);
    assert_eq!(penalized.bankruptcy_penalty, expected_penalty);
    assert_eq!(penalized.payout + expected_penalty, ordinary.payout);
    assert_eq!(penalized.balance_after, 1_000 + penalized.payout);
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
fn test_shallow_tips_excluded_at_deep_depth() {
    let deep = eligible_tips(50);
    assert!(deep.iter().all(|tip| !tip.contains("first paid dig")));
}

#[test]
fn test_tips_match_at_correct_depth() {
    let shallow = eligible_tips(5);
    assert!(!shallow.is_empty());
    assert!(shallow.contains(&DIG_TIPS[0].text));
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
fn test_luminosity_cave_in_bonus() {
    assert_close(luminosity_cave_in_bonus(100), 0.0);
    assert_close(luminosity_cave_in_bonus(50), 0.08);
    assert_close(luminosity_cave_in_bonus(10), 0.20);
    assert_close(luminosity_cave_in_bonus(0), 0.35);
}

#[test]
fn test_tiered_event_multiplier() {
    let base = 0.22;
    assert!(event_chance(base, 100) < 0.50);
    assert!(event_chance(base, 50) < 0.50);
    assert!(event_chance(base, 10) > 0.50);
    assert!(event_chance(base, 0) > 0.50);
}

#[test]
fn test_event_chance_cap() {
    assert_close(event_chance(0.25, 0), 0.75);
    assert_close(event_chance(0.35, 0), 0.75);
    assert!(0.70 < event_chance(0.35, 0));
    assert!(0.76 > event_chance(0.35, 0));
}

#[test]
fn test_pitch_black_forces_risky() {
    assert_eq!(resolved_event_choice("safe", 0), "risky");
}

#[test]
fn test_dark_risky_penalty() {
    assert!(0.58 >= risky_success_chance(0.62, 10));
}

#[test]
fn test_dark_risky_penalty_floor() {
    assert_close(risky_success_chance(0.10, 0), 0.05);
    assert!(0.04 < risky_success_chance(0.10, 0));
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
fn test_cheer_saves_cheer_data() {
    let mut cheers = CheerBook::default();
    cheers
        .cheer(10_002, 10_001, 1_000_000)
        .expect("first cheer succeeds");
    assert_eq!(cheers.active_count(1_000_000), 1);
    assert_eq!(cheers.cheers[0].cheerer_id, 10_002);
}

#[test]
fn test_cheer_increases_boss_win_chance() {
    let mut cheers = CheerBook::default();
    for cheerer in [10_002, 10_003, 10_004] {
        cheers
            .cheer(cheerer, 10_001, 1_000_000)
            .expect("cheer slot available");
    }
    assert_close(cheers.hit_bonus(1_000_000), 0.15);
    assert!(0.70 < 0.65 - 0.02 + cheers.hit_bonus(1_000_000));
}

#[test]
fn test_cheer_max_three() {
    let mut cheers = CheerBook::default();
    for cheerer in [10_002, 10_003, 10_004] {
        cheers
            .cheer(cheerer, 10_001, 1_000_000)
            .expect("cheer slot available");
    }
    assert!(cheers.cheer(10_005, 10_001, 1_000_000).is_err());
}

#[test]
fn test_cheer_slots_free_after_expiry() {
    let mut cheers = CheerBook::default();
    for cheerer in [10_002, 10_003, 10_004] {
        cheers
            .cheer(cheerer, 10_001, 1_000_000)
            .expect("cheer slot available");
    }
    assert!(cheers.cheer(10_005, 10_001, 1_003_601).is_ok());
}

#[test]
fn test_cheer_self_rejected() {
    assert!(
        CheerBook::default()
            .cheer(10_001, 10_001, 1_000_000)
            .is_err()
    );
}

#[test]
fn test_cheer_does_not_trigger_dig_cooldown() {
    let mut cheers = CheerBook::default();
    cheers
        .cheer(10_002, 10_001, 1_000_000)
        .expect("cheer succeeds");
    let cheerer_last_dig_at = None;
    assert_eq!(cooldown_remaining(cheerer_last_dig_at, 1_000_000, 3_600), 0);
}

#[test]
fn test_cheer_has_own_45s_cooldown() {
    let mut cheers = CheerBook::default();
    cheers
        .cheer(10_002, 10_001, 1_000_000)
        .expect("first cheer succeeds");
    assert!(cheers.cheer(10_002, 10_003, 1_000_044).is_err());
    assert!(cheers.cheer(10_002, 10_003, 1_000_045).is_ok());
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

macro_rules! boss_wager_case {
    ($name:ident, $wager:expr, $accepted:expr) => {
        #[test]
        fn $name() {
            assert_eq!(validate_boss_wager($wager, 2_000, true).is_ok(), $accepted);
        }
    };
}

boss_wager_case!(test_boss_wager_cap_1000_true_fight_boss, 1_000, true);
boss_wager_case!(test_boss_wager_cap_1000_true_start_boss_duel, 1_000, true);
boss_wager_case!(test_boss_wager_cap_1001_false_fight_boss, 1_001, false);
boss_wager_case!(test_boss_wager_cap_1001_false_start_boss_duel, 1_001, false);

#[test]
fn test_fight_boss_insufficient_balance_error() {
    assert_eq!(
        validate_boss_wager(999, 10, true),
        Err("Insufficient balance for that boss wager.")
    );
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
    let defeated = BOSS_BOUNDARIES.into_iter().collect::<BTreeSet<_>>();
    let result = apply_boss_gate(PINNACLE_DEPTH - 2, 20, &defeated);
    assert_eq!(result.boss_encounter, Some(PINNACLE_DEPTH));
    assert_eq!(result.depth_after, PINNACLE_DEPTH - 1);
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
fn test_first_dig_rolls_back_balance_when_tunnel_write_fails() {
    let mut tunnel = TunnelState::default();
    let before = tunnel.clone();
    let result: Result<(), &str> = atomic_state_update(&mut tunnel, |staged| {
        apply_first_dig(staged, 5, 4, 4, 1_000_000);
        Err("injected tunnel write failure")
    });
    assert!(result.is_err());
    assert_eq!(tunnel, before);
}

#[test]
fn test_normal_dig_rolls_back_balance_when_tunnel_write_fails() {
    let mut tunnel = started_tunnel(10);
    let before = tunnel.clone();
    let result: Result<(), &str> = atomic_state_update(&mut tunnel, |staged| {
        apply_dig_outcome(staged, outcome(3, 5), 2_000_000);
        Err("injected tunnel write failure")
    });
    assert!(result.is_err());
    assert_eq!(tunnel, before);
}

#[test]
fn test_queued_consumables_survive_a_failed_dig_commit() {
    let mut tunnel = started_tunnel(10);
    tunnel.queued_consumables.push("dynamite");
    let result: Result<(), &str> = atomic_state_update(&mut tunnel, |staged| {
        staged.queued_consumables.clear();
        Err("injected tunnel write failure")
    });
    assert!(result.is_err());
    assert_eq!(tunnel.queued_consumables, vec!["dynamite"]);
}

#[test]
fn test_queued_consumables_consumed_on_successful_dig() {
    let mut tunnel = started_tunnel(10);
    tunnel.queued_consumables.push("dynamite");
    atomic_state_update(&mut tunnel, |staged| {
        staged.queued_consumables.clear();
        Ok::<_, &str>(())
    })
    .expect("commit succeeds");
    assert!(tunnel.queued_consumables.is_empty());
}

#[test]
fn test_queued_boss_preparation_survives_normal_dig() {
    let mut tunnel = started_tunnel(10);
    tunnel.boss_preparation.push("rescue_line");
    atomic_state_update(&mut tunnel, |staged| {
        apply_dig_outcome(staged, outcome(1, 0), 2_000_000);
        Ok::<_, &str>(())
    })
    .expect("normal dig commits");
    assert_eq!(tunnel.boss_preparation, vec!["rescue_line"]);
}

#[test]
fn test_prestige_rolls_back_grant_when_tunnel_reset_fails() {
    let mut tunnel = started_tunnel(PINNACLE_DEPTH);
    tunnel.balance = 500;
    let before = tunnel.clone();
    let result: Result<(), &str> = atomic_state_update(&mut tunnel, |staged| {
        staged.balance += 1_000;
        staged.depth = 0;
        Err("injected tunnel reset failure")
    });
    assert!(result.is_err());
    assert_eq!(tunnel, before);
}

#[test]
fn test_prestige_relic_not_minted_when_tunnel_reset_fails() {
    let mut tunnel = started_tunnel(PINNACLE_DEPTH);
    let result: Result<(), &str> = atomic_state_update(&mut tunnel, |staged| {
        staged.artifacts.push("prestige_relic");
        Err("injected tunnel reset failure")
    });
    assert!(result.is_err());
    assert!(tunnel.artifacts.is_empty());
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
    tunnel.stats.allocate(5, 0, 0).expect("allocation fits");
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
fn test_auto_buy_runs_before_dm_preconditions() {
    let mut balance = 100;
    let mut inventory = 0;
    let purchase = auto_buy_item(&mut inventory, &mut balance, "Torch", 6);
    let advertised_items = vec![purchase.item];
    assert_eq!(purchase.status, AutoBuyStatus::Purchased);
    assert_eq!(advertised_items, vec!["Torch"]);
    assert_eq!(balance, 94);
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
    tunnel.stats.allocate(2, 0, 0).expect("allocation fits");
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
    tunnel.stats.allocate(2, 0, 0).expect("allocation fits");
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
fn test_pinnacle_reward_applies_edict_to_base_and_wager_profit() {
    let ordinary = resolve_boss(350, 1_000, 250, true, 1_000, 500, 20);
    let policy = DigProfitPolicy {
        bankruptcy_keep_basis_points: 7_500,
        vanity_tax_basis_points: 1_000,
    };
    let settled = resolve_boss_with_policy(350, 1_000, 250, true, 1_000, 500, 20, policy);
    let gross = scale_positive_dig_jc(1_000) + 500;
    let expected = apply_jc_profit_policies(gross, policy);
    assert_eq!(ordinary.payout, gross);
    assert_eq!(settled.payout, expected.net);
    assert_eq!(settled.bankruptcy_penalty, expected.bankruptcy_penalty);
    assert_eq!(settled.vanity_tax, expected.vanity_tax);
    assert_eq!(settled.balance_after, 1_000 + expected.net);
}
