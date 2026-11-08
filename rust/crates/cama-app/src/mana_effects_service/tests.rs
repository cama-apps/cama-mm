use cama_domain::mana::ManaEffects;

use super::*;

const GUILD_ID: GuildId = 42;
const TODAY: &str = "2026-08-11";

type TestService = ManaEffectsService<InMemoryManaEffectsRepository>;

fn fixture() -> TestService {
    ManaEffectsService::new(InMemoryManaEffectsRepository::default(), TODAY)
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
    let mut record = ManaRecord::new(10_007, Some(GUILD_ID), "Mountain", TODAY);
    record.consumed = true;
    service.repository_mut().assign_mana(record);

    let effects = service
        .get_effects(10_007, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color, None);
    assert!(!effects.red_10x_leverage);
    assert_eq!(service.repository().current_mana_reads(), 1);
}

#[test]
fn test_get_effects_stale_mana() {
    let mut service = fixture();
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
    assign(&mut service, 10_006, "Swamp");
    let effects = service
        .get_effects(10_006, Some(GUILD_ID))
        .expect("memory lookup succeeds");
    assert_eq!(effects.color.as_deref(), Some("Black"));
    assert!(effects.swamp_siphon);
    assert_eq!(effects.swamp_bankruptcy_games, 3);
}

#[test]
fn test_red_roll_cost_and_jackpot() {
    let effects = ManaEffects::for_color(Some("Red"), Some("Mountain"));
    assert_eq!(effects.red_roll_cost, 2);
    assert_eq!(effects.red_roll_jackpot, 40);
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
