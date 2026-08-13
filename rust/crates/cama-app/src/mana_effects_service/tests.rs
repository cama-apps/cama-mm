use std::cell::RefCell;
use std::rc::Rc;

use cama_domain::economy_scaling::DailyRewardPolicy;
use cama_domain::mana::ManaEffects;

use super::*;

const GUILD_ID: GuildId = 42;
const TODAY: &str = "2026-08-11";

#[derive(Clone, Copy, Debug)]
struct FixedEntropy {
    roll: SiphonRoll,
}

impl Default for FixedEntropy {
    fn default() -> Self {
        Self {
            roll: SiphonRoll::new(0, 2, false, 0x1234),
        }
    }
}

impl SiphonEntropy for FixedEntropy {
    fn next_roll(&mut self) -> SiphonRoll {
        self.roll
    }
}

type TestService = ManaEffectsService<InMemoryManaEffectsRepository>;

fn fixture() -> TestService {
    ManaEffectsService::new(
        InMemoryManaEffectsRepository::default(),
        TODAY,
        FixedEntropy::default(),
    )
}

fn register(service: &mut TestService, discord_id: DiscordId, balance: i64) {
    service
        .repository_mut()
        .register_player(discord_id, Some(GUILD_ID), balance);
}

fn assign_on(service: &mut TestService, discord_id: DiscordId, land: &str, assigned_date: &str) {
    service.repository_mut().assign_mana(ManaRecord::new(
        discord_id,
        Some(GUILD_ID),
        land,
        assigned_date,
    ));
}

fn assign(service: &mut TestService, discord_id: DiscordId, land: &str) {
    assign_on(service, discord_id, land, TODAY);
}

#[test]
fn test_default_effects() {
    let effects = ManaEffects::default();
    assert_eq!(effects.color, None);
    assert_eq!(effects.land, None);
    assert!(!effects.red_10x_leverage);
    assert!(!effects.blue_gamba_scrying);
    assert_eq!(effects.green_steady_bonus, 0);
    assert_eq!(effects.green_gain_cap, None);
    assert!(!effects.plains_guardian_aura);
    assert_eq!(effects.plains_max_wheel_win, None);
    assert_eq!(effects.plains_tip_fee_rate, None);
    assert!(!effects.swamp_siphon);
    assert_eq!(effects.swamp_self_tax, 0);
    assert_eq!(effects.swamp_bankruptcy_games, 5);
}

#[test]
fn test_red_effects() {
    let effects = ManaEffects::for_color(Some("Red"), Some("Mountain"));
    assert_eq!(effects.color.as_deref(), Some("Red"));
    assert_eq!(effects.land.as_deref(), Some("Mountain"));
    assert!(effects.red_10x_leverage);
    assert_eq!(effects.red_bomb_pot_ante, 30);
    assert_eq!(effects.red_roll_cost, 2);
    assert_eq!(effects.red_roll_jackpot, 40);
}

#[test]
fn test_blue_effects() {
    let effects = ManaEffects::for_color(Some("Blue"), Some("Island"));
    assert_eq!(effects.color.as_deref(), Some("Blue"));
    assert_eq!(effects.land.as_deref(), Some("Island"));
    assert!(effects.blue_gamba_scrying);
    assert_eq!(effects.blue_gamba_reduction, 0.25);
    assert_eq!(effects.blue_cashback_rate, 0.05);
    assert_eq!(effects.blue_tax_rate, 0.055);
}

#[test]
fn test_green_effects() {
    let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));
    assert_eq!(effects.color.as_deref(), Some("Green"));
    assert_eq!(effects.land.as_deref(), Some("Forest"));
    assert_eq!(effects.green_steady_bonus, 1);
    assert_eq!(effects.green_gain_cap, Some(50));
    assert_eq!(effects.green_bankrupt_penalty, -50);
    assert_eq!(effects.green_max_wheel_win, 60);
}

#[test]
fn test_white_effects() {
    let effects = ManaEffects::for_color(Some("White"), Some("Plains"));
    assert_eq!(effects.color.as_deref(), Some("White"));
    assert_eq!(effects.land.as_deref(), Some("Plains"));
    assert!(effects.plains_guardian_aura);
    assert_eq!(effects.plains_max_wheel_win, Some(50));
    assert_eq!(effects.plains_tip_fee_rate, Some(0.0));
    assert_eq!(effects.plains_tithe_rate, 0.05);
}

#[test]
fn test_black_effects() {
    let effects = ManaEffects::for_color(Some("Black"), Some("Swamp"));
    assert_eq!(effects.color.as_deref(), Some("Black"));
    assert_eq!(effects.land.as_deref(), Some("Swamp"));
    assert!(effects.swamp_siphon);
    assert_eq!(effects.swamp_self_tax, 2);
    assert_eq!(effects.swamp_bankruptcy_games, 3);
}

#[test]
fn test_none_color() {
    let effects = ManaEffects::for_color(None, None);
    assert_eq!(effects.color, None);
    assert_eq!(effects.land, None);
    assert!(!effects.red_10x_leverage);
    assert!(!effects.blue_gamba_scrying);
    assert_eq!(effects.green_steady_bonus, 0);
    assert!(!effects.plains_guardian_aura);
    assert!(!effects.swamp_siphon);
}

#[test]
fn test_red_new_fields() {
    let effects = ManaEffects::for_color(Some("Red"), Some("Mountain"));
    assert!(effects.dig_yield_variance > 0.0);
    assert!(effects.dig_dynamite_overyield_chance > 0.0);
    assert!(effects.dig_paid_cost_modifier_pct < 0.0);
    assert!(effects.boss_damage_variance_modifier > 0.0);
    assert!(effects.trivia_payout_multiplier > 1.0);
}

#[test]
fn test_blue_new_fields() {
    let effects = ManaEffects::for_color(Some("Blue"), Some("Island"));
    assert!(effects.dig_paid_refund_on_caveins > 0.0);
    assert!(effects.boss_durability_refund_rate > 0.0);
    assert!(effects.pred_fee_discount_rate > 0.0);
    assert!(effects.shop_info_discount_rate > 0.0);
    assert!(effects.trivia_hint_bonus > 0);
}

#[test]
fn test_green_new_fields() {
    let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));
    assert!(effects.dig_hazard_modifier < 0.0);
    assert!(effects.dig_durability_bonus_ticks > 0);
    assert!(effects.dig_cooldown_reduction_seconds > 0);
    assert!(effects.boss_damage_variance_modifier < 0.0);
    assert!(effects.match_bet_steady_bonus > 0);
    assert!(effects.pred_steady_bonus > 0);
    assert!(effects.shop_consumable_discount_rate > 0.0);
    assert!(effects.trivia_streak_bonus > 0);
}

#[test]
fn test_white_new_fields() {
    let effects = ManaEffects::for_color(Some("White"), Some("Plains"));
    assert!(effects.boss_durability_prevention_rate > 0.0);
    assert_eq!(effects.plains_tithe_rate, 0.05);
}

#[test]
fn test_black_new_fields() {
    let effects = ManaEffects::for_color(Some("Black"), Some("Swamp"));
    assert!(effects.dig_hazard_modifier > 0.0);
}

#[test]
fn test_red_does_not_activate_other_colors() {
    let effects = ManaEffects::for_color(Some("Red"), Some("Mountain"));
    assert!(!effects.blue_gamba_scrying);
    assert_eq!(effects.green_steady_bonus, 0);
    assert!(!effects.plains_guardian_aura);
    assert!(!effects.swamp_siphon);
}

#[test]
fn test_default_non_color_fields_unchanged() {
    let effects = ManaEffects::for_color(Some("Blue"), Some("Island"));
    assert!(!effects.red_10x_leverage);
    assert_eq!(effects.red_roll_cost, 1);
    assert_eq!(effects.green_gain_cap, None);
    assert_eq!(effects.swamp_bankruptcy_games, 5);
}

#[test]
fn test_mana_effects_is_frozen() {
    let mut source = ManaEffects::for_color(Some("Red"), Some("Mountain"));
    let snapshot = ManaEffectsSnapshot::new(source.clone());
    source.red_10x_leverage = false;
    source.color = Some("Blue".to_owned());
    assert!(snapshot.red_10x_leverage);
    assert_eq!(snapshot.color.as_deref(), Some("Red"));

    let default_snapshot = ManaEffectsSnapshot::default();
    assert_eq!(default_snapshot.swamp_bankruptcy_games, 5);
}

#[test]
fn test_get_effects_no_mana() {
    let mut service = fixture();
    register(&mut service, 99_999, 100);
    let effects = service
        .get_effects(99_999, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color, None);
    assert!(!effects.red_10x_leverage);
    assert!(!effects.blue_gamba_scrying);
}

#[test]
fn test_get_effects_with_mana() {
    let mut service = fixture();
    register(&mut service, 10_001, 100);
    assign(&mut service, 10_001, "Mountain");
    let effects = service
        .get_effects(10_001, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color.as_deref(), Some("Red"));
    assert!(effects.red_10x_leverage);
    assert_eq!(effects.red_roll_cost, 2);
    assert_eq!(effects.red_roll_jackpot, 40);
    assert_eq!(effects.red_bomb_pot_ante, 30);
}

#[test]
fn test_get_effects_reuses_consumed_state_from_mana_read() {
    let mut service = fixture();
    register(&mut service, 10_007, 100);
    let mut record = ManaRecord::new(10_007, Some(GUILD_ID), "Mountain", TODAY);
    record.consumed = true;
    service.repository_mut().assign_mana(record);

    let effects = service
        .get_effects(10_007, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color, None);
    assert!(!effects.red_10x_leverage);
    assert_eq!(service.repository().current_mana_reads(), 1);
    assert_eq!(service.repository().all_mana_reads(), 0);
}

#[test]
fn test_get_effects_bulk_uses_one_snapshot_and_preserves_suppression() {
    let mut service = fixture();
    for discord_id in [10_011, 10_012, 10_013] {
        register(&mut service, discord_id, 100);
    }
    assign(&mut service, 10_011, "Forest");
    assign_on(&mut service, 10_012, "Mountain", "2020-01-01");
    let mut consumed = ManaRecord::new(10_013, Some(GUILD_ID), "Mountain", TODAY);
    consumed.consumed = true;
    service.repository_mut().assign_mana(consumed);

    let effects = service
        .get_effects_bulk(&[10_011, 10_012, 10_013, 10_014, 10_011], Some(GUILD_ID))
        .expect("snapshot lookup succeeds");
    assert_eq!(effects.ids(), vec![10_011, 10_012, 10_013, 10_014]);
    assert_eq!(
        effects.get(10_011).and_then(|e| e.color.as_deref()),
        Some("Green")
    );
    assert_eq!(effects.get(10_012).and_then(|e| e.color.as_deref()), None);
    assert_eq!(effects.get(10_013).and_then(|e| e.color.as_deref()), None);
    assert_eq!(effects.get(10_014).and_then(|e| e.color.as_deref()), None);
    assert_eq!(service.repository().all_mana_reads(), 1);
    assert_eq!(service.repository().current_mana_reads(), 0);
}

#[test]
fn test_get_effects_stale_mana() {
    let mut service = fixture();
    register(&mut service, 10_002, 100);
    assign_on(&mut service, 10_002, "Mountain", "2020-01-01");
    let effects = service
        .get_effects(10_002, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color, None);
    assert!(!effects.red_10x_leverage);
}

#[test]
fn test_get_effects_blue() {
    let mut service = fixture();
    register(&mut service, 10_003, 100);
    assign(&mut service, 10_003, "Island");
    let effects = service
        .get_effects(10_003, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color.as_deref(), Some("Blue"));
    assert!(effects.blue_gamba_scrying);
    assert_eq!(effects.blue_tax_rate, 0.055);
}

#[test]
fn test_get_effects_green() {
    let mut service = fixture();
    register(&mut service, 10_004, 100);
    assign(&mut service, 10_004, "Forest");
    let effects = service
        .get_effects(10_004, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color.as_deref(), Some("Green"));
    assert_eq!(effects.green_steady_bonus, 1);
    assert_eq!(effects.green_gain_cap, Some(50));
}

#[test]
fn test_get_effects_white() {
    let mut service = fixture();
    register(&mut service, 10_005, 100);
    assign(&mut service, 10_005, "Plains");
    let effects = service
        .get_effects(10_005, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color.as_deref(), Some("White"));
    assert!(effects.plains_guardian_aura);
}

#[test]
fn test_get_effects_black() {
    let mut service = fixture();
    register(&mut service, 10_006, 100);
    assign(&mut service, 10_006, "Swamp");
    let effects = service
        .get_effects(10_006, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color.as_deref(), Some("Black"));
    assert!(effects.swamp_siphon);
    assert_eq!(effects.swamp_bankruptcy_games, 3);
}

#[test]
fn test_execute_siphon() {
    let mut service = fixture();
    register(&mut service, 20_001, 50);
    register(&mut service, 20_002, 100);
    let result = service
        .execute_siphon(20_001, Some(GUILD_ID))
        .expect("eligible target exists");
    assert_eq!(result.victim_id, 20_002);
    assert!((1..=3).contains(&result.amount));
    assert!(result.event_key.starts_with("swamp_siphon:"));
}

#[test]
fn test_execute_siphon_transfers_balance() {
    let mut service = fixture();
    register(&mut service, 21_001, 50);
    register(&mut service, 21_002, 100);
    let result = service
        .execute_siphon(21_001, Some(GUILD_ID))
        .expect("eligible target exists");
    assert_eq!(
        service.repository().balance(21_001, Some(GUILD_ID)),
        Some(50 + result.amount)
    );
    assert_eq!(
        service.repository().balance(21_002, Some(GUILD_ID)),
        Some(100 - result.amount)
    );
    assert_eq!(service.repository().ledger().len(), 2);
}

#[test]
fn test_execute_siphon_no_targets() {
    let mut service = fixture();
    register(&mut service, 30_001, 50);
    assert_eq!(service.execute_siphon(30_001, Some(GUILD_ID)), None);
}

#[test]
fn test_execute_siphon_skips_zero_balance() {
    let mut service = fixture();
    register(&mut service, 31_001, 50);
    register(&mut service, 31_002, 0);
    assert_eq!(service.execute_siphon(31_001, Some(GUILD_ID)), None);
}

#[test]
fn test_apply_blue_tax() {
    let mut service = fixture();
    register(&mut service, 40_001, 100);
    assign(&mut service, 40_001, "Island");
    let tax = service
        .apply_blue_tax(40_001, Some(GUILD_ID), 200)
        .expect("tax write succeeds");
    assert_eq!(tax, 11);
    assert_eq!(
        service.repository().balance(40_001, Some(GUILD_ID)),
        Some(89)
    );
    assert_eq!(service.repository().ledger()[0].metadata["gain"], 200);
}

#[test]
fn test_apply_blue_tax_minimum_one() {
    let mut service = fixture();
    register(&mut service, 40_003, 100);
    assign(&mut service, 40_003, "Island");
    let tax = service
        .apply_blue_tax(40_003, Some(GUILD_ID), 5)
        .expect("tax write succeeds");
    assert_eq!(tax, 1);
    assert_eq!(
        service.repository().balance(40_003, Some(GUILD_ID)),
        Some(99)
    );
}

#[test]
fn test_apply_blue_tax_zero_gain() {
    let mut service = fixture();
    register(&mut service, 40_004, 100);
    assign(&mut service, 40_004, "Island");
    assert_eq!(service.apply_blue_tax(40_004, Some(GUILD_ID), 0), Ok(0));
    assert_eq!(
        service.repository().balance(40_004, Some(GUILD_ID)),
        Some(100)
    );
    assert!(service.repository().ledger().is_empty());
}

#[test]
fn test_apply_blue_tax_no_blue_mana() {
    let mut service = fixture();
    register(&mut service, 40_005, 100);
    assign(&mut service, 40_005, "Mountain");
    assert_eq!(service.apply_blue_tax(40_005, Some(GUILD_ID), 100), Ok(0));
    assert_eq!(
        service.repository().balance(40_005, Some(GUILD_ID)),
        Some(100)
    );
}

#[test]
fn test_apply_blue_cashback() {
    let mut service = fixture();
    register(&mut service, 40_002, 100);
    assign(&mut service, 40_002, "Island");
    let cashback = service
        .apply_blue_cashback(40_002, Some(GUILD_ID), 20)
        .expect("cashback write succeeds");
    assert_eq!(cashback, 1);
    assert_eq!(
        service.repository().balance(40_002, Some(GUILD_ID)),
        Some(101)
    );
}

#[test]
fn test_apply_blue_cashback_larger_loss() {
    let mut service = fixture();
    register(&mut service, 40_006, 100);
    assign(&mut service, 40_006, "Island");
    let cashback = service
        .apply_blue_cashback(40_006, Some(GUILD_ID), 100)
        .expect("cashback write succeeds");
    assert_eq!(cashback, 5);
    assert_eq!(
        service.repository().balance(40_006, Some(GUILD_ID)),
        Some(105)
    );
}

type RewardCalls = Rc<RefCell<Vec<(Option<GuildId>, i64)>>>;

#[derive(Clone)]
struct RecordingRewardPolicy {
    calls: RewardCalls,
    result: i64,
}

impl DailyRewardPolicy for RecordingRewardPolicy {
    fn adjust_reward(&mut self, guild_id: Option<GuildId>, amount: i64) -> i64 {
        self.calls.borrow_mut().push((guild_id, amount));
        self.result
    }
}

#[test]
fn test_apply_blue_cashback_runs_daily_event_after_central_scale() {
    let mut service = fixture();
    register(&mut service, 40_009, 100);
    assign(&mut service, 40_009, "Island");
    let calls = Rc::new(RefCell::new(Vec::new()));
    service.set_reward_policy(RecordingRewardPolicy {
        calls: Rc::clone(&calls),
        result: 2,
    });

    let cashback = service
        .apply_blue_cashback(40_009, Some(GUILD_ID), 100)
        .expect("cashback write succeeds");
    assert_eq!(&*calls.borrow(), &[(Some(GUILD_ID), 5)]);
    assert_eq!(cashback, 2);
    assert_eq!(
        service.repository().balance(40_009, Some(GUILD_ID)),
        Some(102)
    );
}

#[test]
fn test_apply_blue_cashback_zero_loss() {
    let mut service = fixture();
    register(&mut service, 40_007, 100);
    assign(&mut service, 40_007, "Island");
    assert_eq!(
        service.apply_blue_cashback(40_007, Some(GUILD_ID), 0),
        Ok(0)
    );
    assert_eq!(
        service.repository().balance(40_007, Some(GUILD_ID)),
        Some(100)
    );
}

#[test]
fn test_apply_blue_cashback_no_blue_mana() {
    let mut service = fixture();
    register(&mut service, 40_008, 100);
    assign(&mut service, 40_008, "Forest");
    assert_eq!(
        service.apply_blue_cashback(40_008, Some(GUILD_ID), 100),
        Ok(0)
    );
    assert_eq!(
        service.repository().balance(40_008, Some(GUILD_ID)),
        Some(100)
    );
}

#[test]
fn test_apply_green_cap() {
    let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));
    assert_eq!(TestService::apply_green_cap(&effects, 100), 50);
}

#[test]
fn test_apply_green_cap_below_cap() {
    let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));
    assert_eq!(TestService::apply_green_cap(&effects, 30), 30);
}

#[test]
fn test_apply_green_cap_exactly_at_cap() {
    let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));
    assert_eq!(TestService::apply_green_cap(&effects, 50), 50);
}

#[test]
fn test_apply_green_cap_no_green() {
    let effects = ManaEffects::for_color(Some("Red"), Some("Mountain"));
    assert_eq!(TestService::apply_green_cap(&effects, 200), 200);
}

#[test]
fn test_apply_green_cap_default_no_cap() {
    assert_eq!(
        TestService::apply_green_cap(&ManaEffects::default(), 999),
        999
    );
}

#[test]
fn test_apply_plains_tithe() {
    let mut service = fixture();
    register(&mut service, 50_001, 200);
    assign(&mut service, 50_001, "Plains");
    let tithe = service
        .apply_plains_tithe(50_001, Some(GUILD_ID), 100)
        .expect("atomic tithe succeeds");
    assert_eq!(tithe, 5);
    assert_eq!(
        service.repository().balance(50_001, Some(GUILD_ID)),
        Some(195)
    );
    assert_eq!(service.repository().nonprofit_fund(Some(GUILD_ID)), 5);
    assert_eq!(
        service.repository().ledger()[0].related_type,
        "plains_tithe"
    );
}

#[test]
fn test_apply_plains_tithe_zero_gain() {
    let mut service = fixture();
    register(&mut service, 50_002, 200);
    assign(&mut service, 50_002, "Plains");
    assert_eq!(service.apply_plains_tithe(50_002, Some(GUILD_ID), 0), Ok(0));
    assert_eq!(
        service.repository().balance(50_002, Some(GUILD_ID)),
        Some(200)
    );
    assert_eq!(service.repository().nonprofit_fund(Some(GUILD_ID)), 0);
}

#[test]
fn test_apply_plains_tithe_no_plains() {
    let mut service = fixture();
    register(&mut service, 50_003, 200);
    assign(&mut service, 50_003, "Mountain");
    assert_eq!(
        service.apply_plains_tithe(50_003, Some(GUILD_ID), 100),
        Ok(0)
    );
    assert_eq!(
        service.repository().balance(50_003, Some(GUILD_ID)),
        Some(200)
    );
    assert_eq!(service.repository().nonprofit_fund(Some(GUILD_ID)), 0);
}

#[test]
fn test_apply_shop_discount_blue_info() {
    let mut service = fixture();
    register(&mut service, 60_001, 200);
    assign(&mut service, 60_001, "Island");
    assert_eq!(
        service.apply_shop_discount(60_001, Some(GUILD_ID), 100, ShopItemKind::Info),
        Ok(90)
    );
}

#[test]
fn test_apply_shop_discount_green_consumable() {
    let mut service = fixture();
    register(&mut service, 60_002, 200);
    assign(&mut service, 60_002, "Forest");
    assert_eq!(
        service.apply_shop_discount(60_002, Some(GUILD_ID), 100, ShopItemKind::Consumable),
        Ok(95)
    );
}

#[test]
fn test_apply_shop_discount_no_match() {
    let mut service = fixture();
    register(&mut service, 60_003, 200);
    assign(&mut service, 60_003, "Mountain");
    assert_eq!(
        service.apply_shop_discount(60_003, Some(GUILD_ID), 100, ShopItemKind::Info),
        Ok(100)
    );
}

#[test]
fn test_apply_shop_discount_zero_cost_passes_through() {
    let mut service = fixture();
    register(&mut service, 60_004, 200);
    assign(&mut service, 60_004, "Island");
    assert_eq!(
        service.apply_shop_discount(60_004, Some(GUILD_ID), 0, ShopItemKind::Info),
        Ok(0)
    );
}

#[test]
fn test_apply_shop_discount_floors_at_one() {
    let mut service = fixture();
    register(&mut service, 60_005, 200);
    assign(&mut service, 60_005, "Island");
    assert_eq!(
        service.apply_shop_discount(60_005, Some(GUILD_ID), 1, ShopItemKind::Info),
        Ok(1)
    );
}

#[test]
fn test_red_roll_cost_and_jackpot() {
    let effects = ManaEffects::for_color(Some("Red"), Some("Mountain"));
    assert_eq!(effects.red_roll_cost, 2);
    assert_eq!(effects.red_roll_jackpot, 40);
}

#[test]
fn test_green_gain_cap_applied() {
    let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));
    assert_eq!(TestService::apply_green_cap(&effects, 120), 50);
}

#[test]
fn test_swamp_bankruptcy_games() {
    let effects = ManaEffects::for_color(Some("Black"), Some("Swamp"));
    let default = ManaEffects::default();
    assert_eq!(effects.swamp_bankruptcy_games, 3);
    assert_eq!(default.swamp_bankruptcy_games, 5);
    assert!(effects.swamp_bankruptcy_games < default.swamp_bankruptcy_games);
}

#[test]
fn test_red_bomb_pot_ante_elevated() {
    let effects = ManaEffects::for_color(Some("Red"), Some("Mountain"));
    let default = ManaEffects::default();
    assert_eq!(effects.red_bomb_pot_ante, 30);
    assert_eq!(default.red_bomb_pot_ante, 10);
    assert!(effects.red_bomb_pot_ante > default.red_bomb_pot_ante);
}

#[test]
fn test_green_compressed_variance() {
    let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));
    let default = ManaEffects::default();
    assert_eq!(effects.green_bankrupt_penalty, -50);
    assert_eq!(default.green_bankrupt_penalty, -100);
}

#[test]
fn test_green_max_wheel_win_reduced() {
    let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));
    let default = ManaEffects::default();
    assert_eq!(effects.green_max_wheel_win, 60);
    assert_eq!(default.green_max_wheel_win, 100);
}

#[test]
fn test_all_colors_set_identity_fields() {
    for (color, land) in [
        ("Red", "Mountain"),
        ("Blue", "Island"),
        ("Green", "Forest"),
        ("White", "Plains"),
        ("Black", "Swamp"),
    ] {
        let effects = ManaEffects::for_color(Some(color), Some(land));
        assert_eq!(effects.color.as_deref(), Some(color));
        assert_eq!(effects.land.as_deref(), Some(land));
    }
}
