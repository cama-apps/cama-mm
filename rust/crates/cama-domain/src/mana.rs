//! Pure mana-color effects and cross-system modifier policies.
//!
//! Persistence and Discord service lookup stay outside this module.  The
//! string identities are intentional: the Python value object preserves an
//! unknown color or land rather than rejecting it, while known values activate
//! the corresponding modifiers.

/// The complete immutable effect snapshot for one player's active mana.
#[derive(Clone, Debug, PartialEq)]
pub struct ManaEffects {
    /// Color label supplied by the mana assignment.
    pub color: Option<String>,
    /// Land label supplied by the mana assignment.
    pub land: Option<String>,

    // Red (Mountain).
    pub red_10x_leverage: bool,
    pub red_bomb_pot_ante: i64,
    pub red_roll_cost: i64,
    pub red_roll_jackpot: i64,

    // Blue (Island).
    pub blue_gamba_scrying: bool,
    pub blue_gamba_reduction: f64,
    pub blue_cashback_rate: f64,
    pub blue_tax_rate: f64,

    // Green (Forest).
    pub green_steady_bonus: i64,
    pub green_gain_cap: Option<i64>,
    pub green_bankrupt_penalty: i64,
    pub green_max_wheel_win: i64,

    // White (Plains).
    pub plains_guardian_aura: bool,
    pub plains_guardian_cooldown_key: String,
    pub plains_max_wheel_win: Option<i64>,
    pub plains_tip_fee_rate: Option<f64>,
    pub plains_tithe_rate: f64,

    // Black (Swamp).
    pub swamp_siphon: bool,
    pub swamp_self_tax: i64,
    pub swamp_bankruptcy_games: i64,

    // Dig-specific effects.
    pub dig_yield_variance: f64,
    pub dig_hazard_modifier: f64,
    pub dig_durability_bonus_ticks: i64,
    pub dig_dynamite_overyield_chance: f64,
    pub dig_paid_cost_modifier_pct: f64,
    pub dig_cooldown_reduction_seconds: i64,
    pub dig_paid_refund_on_caveins: f64,

    // Boss-combat effects shared with the older mana mechanics.
    pub boss_damage_variance_modifier: f64,
    pub boss_durability_refund_rate: f64,
    pub boss_durability_prevention_rate: f64,

    // Match betting, predictions, shop, and trivia.
    pub match_bet_steady_bonus: i64,
    pub pred_fee_discount_rate: f64,
    pub pred_steady_bonus: i64,
    pub shop_info_discount_rate: f64,
    pub shop_consumable_discount_rate: f64,
    pub trivia_streak_bonus: i64,
    pub trivia_payout_multiplier: f64,
    pub trivia_hint_bonus: i64,

    // Loan and bankruptcy mechanics.
    pub loan_fee_mult: f64,
    pub loan_limit_mult: f64,
    pub bankruptcy_games_reduction: i64,

    // Sabotage and social PvP.
    pub sabotage_cost_mult: f64,
    pub sabotage_steal_depth_pct: f64,
    pub sabotage_first_aegis_today: bool,
    pub sabotage_reveal_attacker: bool,
    pub sabotage_passive_recovery: bool,

    // Boss combat.
    pub boss_damage_mult: f64,
    pub boss_dynamite_damage_mult: f64,
    pub boss_hp_mult: f64,
    pub boss_loot_mult: f64,
    pub boss_reveal_hp: bool,
    pub boss_no_crit_against: bool,

    // Roll, wheel, and prediction extras.
    pub roll_loss_refund_rate: f64,
    pub roll_critfail_halved: bool,
    pub pred_profit_cap_mult: f64,
    pub pred_leverage_allowed: bool,

    // Layer-weather combinations.
    pub weather_combo_storm_cooldown_mult: f64,
    pub weather_combo_sunny_yield_mult: f64,
    pub weather_combo_fog_hazard_mult: f64,
    pub weather_combo_heat_dynamite_extra_chains: i64,
    pub weather_combo_rain_recovery_mult: f64,
}

impl Default for ManaEffects {
    fn default() -> Self {
        Self {
            color: None,
            land: None,
            red_10x_leverage: false,
            red_bomb_pot_ante: 10,
            red_roll_cost: 1,
            red_roll_jackpot: 20,
            blue_gamba_scrying: false,
            blue_gamba_reduction: 0.0,
            blue_cashback_rate: 0.0,
            blue_tax_rate: 0.0,
            green_steady_bonus: 0,
            green_gain_cap: None,
            green_bankrupt_penalty: -100,
            green_max_wheel_win: 100,
            plains_guardian_aura: false,
            plains_guardian_cooldown_key: "plains_guardian".to_owned(),
            plains_max_wheel_win: None,
            plains_tip_fee_rate: None,
            plains_tithe_rate: 0.0,
            swamp_siphon: false,
            swamp_self_tax: 0,
            swamp_bankruptcy_games: 5,
            dig_yield_variance: 0.0,
            dig_hazard_modifier: 0.0,
            dig_durability_bonus_ticks: 0,
            dig_dynamite_overyield_chance: 0.0,
            dig_paid_cost_modifier_pct: 0.0,
            dig_cooldown_reduction_seconds: 0,
            dig_paid_refund_on_caveins: 0.0,
            boss_damage_variance_modifier: 0.0,
            boss_durability_refund_rate: 0.0,
            boss_durability_prevention_rate: 0.0,
            match_bet_steady_bonus: 0,
            pred_fee_discount_rate: 0.0,
            pred_steady_bonus: 0,
            shop_info_discount_rate: 0.0,
            shop_consumable_discount_rate: 0.0,
            trivia_streak_bonus: 0,
            trivia_payout_multiplier: 1.0,
            trivia_hint_bonus: 0,
            loan_fee_mult: 1.0,
            loan_limit_mult: 1.0,
            bankruptcy_games_reduction: 0,
            sabotage_cost_mult: 1.0,
            sabotage_steal_depth_pct: 0.0,
            sabotage_first_aegis_today: false,
            sabotage_reveal_attacker: false,
            sabotage_passive_recovery: false,
            boss_damage_mult: 1.0,
            boss_dynamite_damage_mult: 1.0,
            boss_hp_mult: 1.0,
            boss_loot_mult: 1.0,
            boss_reveal_hp: false,
            boss_no_crit_against: false,
            roll_loss_refund_rate: 0.0,
            roll_critfail_halved: false,
            pred_profit_cap_mult: 1.0,
            pred_leverage_allowed: false,
            weather_combo_storm_cooldown_mult: 1.0,
            weather_combo_sunny_yield_mult: 1.0,
            weather_combo_fog_hazard_mult: 1.0,
            weather_combo_heat_dynamite_extra_chains: 0,
            weather_combo_rain_recovery_mult: 1.0,
        }
    }
}

impl ManaEffects {
    /// Build the effect snapshot for a color and land assignment.
    ///
    /// Matching is case-sensitive, just like the Python factory.  A missing
    /// color means no assignment and therefore also discards `land`.  An
    /// unknown color retains both identity strings but activates no effects.
    #[must_use]
    pub fn for_color(color: Option<&str>, land: Option<&str>) -> Self {
        let Some(color) = color else {
            return Self::default();
        };
        let identity = || Self {
            color: Some(color.to_owned()),
            land: land.map(str::to_owned),
            ..Self::default()
        };

        match color {
            "Red" => Self {
                red_10x_leverage: true,
                red_bomb_pot_ante: 30,
                red_roll_cost: 2,
                red_roll_jackpot: 40,
                dig_yield_variance: 0.15,
                dig_hazard_modifier: 0.01,
                dig_dynamite_overyield_chance: 0.10,
                dig_paid_cost_modifier_pct: -0.05,
                boss_damage_variance_modifier: 0.15,
                boss_dynamite_damage_mult: 1.5,
                trivia_payout_multiplier: 1.5,
                sabotage_cost_mult: 0.5,
                pred_leverage_allowed: true,
                weather_combo_heat_dynamite_extra_chains: 2,
                ..identity()
            },
            "Blue" => Self {
                blue_gamba_scrying: true,
                blue_gamba_reduction: 0.25,
                blue_cashback_rate: 0.05,
                blue_tax_rate: 0.055,
                dig_paid_refund_on_caveins: 0.5,
                boss_durability_refund_rate: 0.25,
                boss_reveal_hp: true,
                pred_fee_discount_rate: 0.05,
                shop_info_discount_rate: 0.10,
                trivia_hint_bonus: 1,
                sabotage_reveal_attacker: true,
                roll_loss_refund_rate: 0.25,
                weather_combo_storm_cooldown_mult: 0.5,
                ..identity()
            },
            "Green" => Self {
                green_steady_bonus: 1,
                green_gain_cap: Some(50),
                green_bankrupt_penalty: -50,
                green_max_wheel_win: 60,
                dig_hazard_modifier: -0.01,
                dig_durability_bonus_ticks: 1,
                dig_cooldown_reduction_seconds: 30,
                boss_damage_variance_modifier: -0.10,
                boss_no_crit_against: true,
                match_bet_steady_bonus: 1,
                pred_steady_bonus: 1,
                pred_profit_cap_mult: 2.0,
                shop_consumable_discount_rate: 0.05,
                trivia_streak_bonus: 1,
                sabotage_passive_recovery: true,
                weather_combo_rain_recovery_mult: 2.0,
                ..identity()
            },
            "White" => Self {
                plains_guardian_aura: true,
                plains_max_wheel_win: Some(50),
                plains_tip_fee_rate: Some(0.0),
                plains_tithe_rate: 0.05,
                boss_durability_prevention_rate: 0.25,
                boss_damage_mult: 1.20,
                loan_fee_mult: 0.5,
                bankruptcy_games_reduction: 1,
                weather_combo_sunny_yield_mult: 1.10,
                ..identity()
            },
            "Black" => Self {
                swamp_siphon: true,
                swamp_self_tax: 2,
                swamp_bankruptcy_games: 3,
                dig_hazard_modifier: 0.01,
                loan_fee_mult: 1.5,
                loan_limit_mult: 2.0,
                sabotage_steal_depth_pct: 0.25,
                boss_hp_mult: 1.30,
                boss_loot_mult: 1.25,
                roll_critfail_halved: true,
                weather_combo_fog_hazard_mult: 0.5,
                ..identity()
            },
            _ => identity(),
        }
    }
}

/// Current assignment data already validated for the active mana day.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManaAssignment {
    pub color: Option<String>,
    pub land: Option<String>,
    pub consumed: bool,
}

impl ManaAssignment {
    /// Construct an active-day assignment snapshot.
    #[must_use]
    pub fn new(color: Option<&str>, land: Option<&str>, consumed: bool) -> Self {
        Self {
            color: color.map(str::to_owned),
            land: land.map(str::to_owned),
            consumed,
        }
    }
}

/// Resolve effects from an optional active-day assignment.
///
/// A missing or consumed assignment has no active effects.  The date check is
/// deliberately a persistence/service responsibility so this policy remains
/// clock-independent.
#[must_use]
pub fn effects_from_assignment(assignment: Option<&ManaAssignment>) -> ManaEffects {
    let Some(assignment) = assignment else {
        return ManaEffects::default();
    };
    if assignment.consumed {
        return ManaEffects::default();
    }
    ManaEffects::for_color(assignment.color.as_deref(), assignment.land.as_deref())
}

/// Return the emoji-only badge for an effect snapshot.
#[must_use]
pub fn format_mana_badge(effects: Option<&ManaEffects>) -> &'static str {
    match effects.and_then(|effects| effects.land.as_deref()) {
        Some("Forest") => "🌲",
        Some("Mountain") => "⛰️",
        Some("Island") => "🏝️",
        Some("Plains") => "🌾",
        Some("Swamp") => "🌿",
        _ => "",
    }
}

/// Runtime lookup boundary used by [`resolve_mana_badge`].
pub trait ManaEffectsSource {
    type Error;

    /// Fetch the player's current active effects.
    fn get_effects(
        &self,
        discord_id: u64,
        guild_id: Option<u64>,
    ) -> Result<ManaEffects, Self::Error>;
}

/// Fetch and format a badge, returning an empty string when the service is
/// absent or fails.  The failure swallowing is part of the display contract.
#[must_use]
pub fn resolve_mana_badge<S: ManaEffectsSource + ?Sized>(
    source: Option<&S>,
    discord_id: u64,
    guild_id: Option<u64>,
) -> &'static str {
    source
        .and_then(|source| source.get_effects(discord_id, guild_id).ok())
        .as_ref()
        .map_or("", |effects| format_mana_badge(Some(effects)))
}

/// Mana-adjusted loan terms.
#[derive(Clone, Debug, PartialEq)]
pub struct LoanModifiers {
    pub fee_rate: f64,
    pub limit: i64,
    pub color: Option<String>,
}

/// Apply active mana to the base loan fee and limit.
#[must_use]
pub fn apply_loan_modifiers(
    effects: &ManaEffects,
    base_fee_rate: f64,
    base_limit: i64,
) -> LoanModifiers {
    if effects.color.is_none() {
        return LoanModifiers {
            fee_rate: base_fee_rate,
            limit: base_limit,
            color: None,
        };
    }

    LoanModifiers {
        fee_rate: (base_fee_rate * effects.loan_fee_mult).max(0.0),
        limit: ((base_limit as f64 * effects.loan_limit_mult) as i64).max(1),
        color: effects.color.clone(),
    }
}

/// Mana-adjusted sabotage inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct SabotageModifiers {
    pub cost: i64,
    pub steal_depth_pct: f64,
    pub color: Option<String>,
}

/// Apply active mana to an attacker's sabotage cost and steal depth.
#[must_use]
pub fn apply_sabotage_modifiers(effects: &ManaEffects, base_cost: i64) -> SabotageModifiers {
    if effects.color.is_none() {
        return SabotageModifiers {
            cost: base_cost,
            steal_depth_pct: 0.0,
            color: None,
        };
    }

    SabotageModifiers {
        cost: ((base_cost as f64 * effects.sabotage_cost_mult) as i64).max(1),
        steal_depth_pct: effects.sabotage_steal_depth_pct,
        color: effects.color.clone(),
    }
}

/// Player-specific boss combat modifiers.
#[derive(Clone, Debug, PartialEq)]
pub struct BossCombatModifiers {
    pub damage_mult: f64,
    pub dynamite_damage_mult: f64,
    pub hp_mult: f64,
    pub loot_mult: f64,
    pub reveal_hp: bool,
    pub no_crit_against: bool,
    pub color: Option<String>,
}

/// Extract the boss-combat fields from an effect snapshot.
#[must_use]
pub fn boss_combat_modifiers(effects: &ManaEffects) -> BossCombatModifiers {
    BossCombatModifiers {
        damage_mult: effects.boss_damage_mult,
        dynamite_damage_mult: effects.boss_dynamite_damage_mult,
        hp_mult: effects.boss_hp_mult,
        loot_mult: effects.boss_loot_mult,
        reveal_hp: effects.boss_reveal_hp,
        no_crit_against: effects.boss_no_crit_against,
        color: effects.color.clone(),
    }
}

/// One weather-layer interaction result.  Defaults are identity modifiers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeatherComboModifiers {
    pub cooldown_mult: f64,
    pub yield_mult: f64,
    pub hazard_mult: f64,
    pub dynamite_extra_chains: i64,
    pub recovery_mult: f64,
    pub applied: bool,
}

impl Default for WeatherComboModifiers {
    fn default() -> Self {
        Self {
            cooldown_mult: 1.0,
            yield_mult: 1.0,
            hazard_mult: 1.0,
            dynamite_extra_chains: 0,
            recovery_mult: 1.0,
            applied: false,
        }
    }
}

/// Return the active weather/color combination, if any.
#[must_use]
pub fn weather_combo_modifiers(
    effects: &ManaEffects,
    weather: Option<&str>,
) -> WeatherComboModifiers {
    let Some(color) = effects.color.as_deref() else {
        return WeatherComboModifiers::default();
    };
    let Some(weather) = weather else {
        return WeatherComboModifiers::default();
    };

    let mut result = WeatherComboModifiers::default();
    match (weather.to_lowercase().as_str(), color) {
        ("storm", "Blue") => {
            result.cooldown_mult = effects.weather_combo_storm_cooldown_mult;
            result.applied = true;
        }
        ("sunny", "White") => {
            result.yield_mult = effects.weather_combo_sunny_yield_mult;
            result.applied = true;
        }
        ("fog", "Black") => {
            result.hazard_mult = effects.weather_combo_fog_hazard_mult;
            result.applied = true;
        }
        ("heat", "Red") => {
            result.dynamite_extra_chains = effects.weather_combo_heat_dynamite_extra_chains;
            result.applied = true;
        }
        ("rain", "Green") => {
            result.recovery_mult = effects.weather_combo_rain_recovery_mult;
            result.applied = true;
        }
        _ => {}
    }
    result
}

/// Resolve weather modifiers using a supplied effect snapshot when available.
///
/// `load_effects` is called only when `effects` is `None`, allowing command
/// handlers to reuse one service snapshot without a second lookup.
#[must_use]
pub fn weather_combo_modifiers_with_effects(
    weather: Option<&str>,
    effects: Option<&ManaEffects>,
    load_effects: impl FnOnce() -> ManaEffects,
) -> WeatherComboModifiers {
    match effects {
        Some(effects) => weather_combo_modifiers(effects, weather),
        None => weather_combo_modifiers(&load_effects(), weather),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ManaAssignment, ManaEffects, ManaEffectsSource, apply_loan_modifiers,
        apply_sabotage_modifiers, boss_combat_modifiers, effects_from_assignment,
        format_mana_badge, resolve_mana_badge, weather_combo_modifiers,
        weather_combo_modifiers_with_effects,
    };

    fn active_effects(color: Option<&str>, land: Option<&str>, consumed: bool) -> ManaEffects {
        let assignment = color.map(|color| ManaAssignment::new(Some(color), land, consumed));
        effects_from_assignment(assignment.as_ref())
    }

    #[derive(Clone)]
    struct FakeManaSource {
        response: Result<ManaEffects, &'static str>,
    }

    impl ManaEffectsSource for FakeManaSource {
        type Error = &'static str;

        fn get_effects(
            &self,
            _discord_id: u64,
            _guild_id: Option<u64>,
        ) -> Result<ManaEffects, Self::Error> {
            self.response.clone()
        }
    }

    #[test]
    fn test_format_mana_badge_returns_emoji_for_each_land() {
        let cases = [
            ("Forest", "Green", "🌲"),
            ("Mountain", "Red", "⛰️"),
            ("Island", "Blue", "🏝️"),
            ("Plains", "White", "🌾"),
            ("Swamp", "Black", "🌿"),
        ];

        for (land, color, expected) in cases {
            let effects = ManaEffects::for_color(Some(color), Some(land));
            assert_eq!(format_mana_badge(Some(&effects)), expected);
        }
    }

    #[test]
    fn test_format_mana_badge_unassigned_returns_empty() {
        assert_eq!(format_mana_badge(None), "");
        assert_eq!(format_mana_badge(Some(&ManaEffects::default())), "");
    }

    #[test]
    fn test_format_mana_badge_unknown_land_returns_empty() {
        let effects = ManaEffects::for_color(Some("???"), Some("Atlantis"));

        assert_eq!(format_mana_badge(Some(&effects)), "");
    }

    #[test]
    fn test_resolve_mana_badge_no_service_returns_empty() {
        assert_eq!(
            resolve_mana_badge(Option::<&FakeManaSource>::None, 1, Some(1)),
            ""
        );
    }

    #[test]
    fn test_resolve_mana_badge_returns_emoji() {
        let source = FakeManaSource {
            response: Ok(ManaEffects::for_color(Some("Green"), Some("Forest"))),
        };

        assert_eq!(resolve_mana_badge(Some(&source), 1, Some(1)), "🌲");
    }

    #[test]
    fn test_resolve_mana_badge_swallows_exceptions() {
        let source = FakeManaSource {
            response: Err("boom"),
        };

        assert_eq!(resolve_mana_badge(Some(&source), 1, Some(1)), "");
    }

    #[test]
    fn test_loan_modifiers_per_color_white_plains_0_5_1_0() {
        let effects = ManaEffects::for_color(Some("White"), Some("Plains"));

        assert_eq!(effects.loan_fee_mult, 0.5);
        assert_eq!(effects.loan_limit_mult, 1.0);
    }

    #[test]
    fn test_loan_modifiers_per_color_black_swamp_1_5_2_0() {
        let effects = ManaEffects::for_color(Some("Black"), Some("Swamp"));

        assert_eq!(effects.loan_fee_mult, 1.5);
        assert_eq!(effects.loan_limit_mult, 2.0);
    }

    #[test]
    fn test_loan_modifiers_per_color_red_mountain_1_0_1_0() {
        let effects = ManaEffects::for_color(Some("Red"), Some("Mountain"));

        assert_eq!(effects.loan_fee_mult, 1.0);
        assert_eq!(effects.loan_limit_mult, 1.0);
    }

    #[test]
    fn test_loan_modifiers_per_color_blue_island_1_0_1_0() {
        let effects = ManaEffects::for_color(Some("Blue"), Some("Island"));

        assert_eq!(effects.loan_fee_mult, 1.0);
        assert_eq!(effects.loan_limit_mult, 1.0);
    }

    #[test]
    fn test_loan_modifiers_per_color_green_forest_1_0_1_0() {
        let effects = ManaEffects::for_color(Some("Green"), Some("Forest"));

        assert_eq!(effects.loan_fee_mult, 1.0);
        assert_eq!(effects.loan_limit_mult, 1.0);
    }

    #[test]
    fn test_sabotage_modifiers_per_color() {
        let red = ManaEffects::for_color(Some("Red"), Some("Mountain"));
        let black = ManaEffects::for_color(Some("Black"), Some("Swamp"));
        let white = ManaEffects::for_color(Some("White"), Some("Plains"));
        let blue = ManaEffects::for_color(Some("Blue"), Some("Island"));
        let green = ManaEffects::for_color(Some("Green"), Some("Forest"));

        assert_eq!(red.sabotage_cost_mult, 0.5);
        assert_eq!(black.sabotage_steal_depth_pct, 0.25);
        assert!(!white.sabotage_first_aegis_today);
        assert!(white.plains_guardian_aura);
        assert!(blue.sabotage_reveal_attacker);
        assert!(green.sabotage_passive_recovery);
    }

    #[test]
    fn test_boss_combat_modifiers_per_color() {
        let white = ManaEffects::for_color(Some("White"), Some("Plains"));
        let black = ManaEffects::for_color(Some("Black"), Some("Swamp"));
        let red = ManaEffects::for_color(Some("Red"), Some("Mountain"));
        let blue = ManaEffects::for_color(Some("Blue"), Some("Island"));
        let green = ManaEffects::for_color(Some("Green"), Some("Forest"));

        assert_eq!(white.boss_damage_mult, 1.20);
        assert_eq!(black.boss_hp_mult, 1.30);
        assert_eq!(black.boss_loot_mult, 1.25);
        assert_eq!(red.boss_dynamite_damage_mult, 1.5);
        assert!(blue.boss_reveal_hp);
        assert!(green.boss_no_crit_against);
    }

    #[test]
    fn test_weather_combo_per_color() {
        let blue = ManaEffects::for_color(Some("Blue"), Some("Island"));
        let white = ManaEffects::for_color(Some("White"), Some("Plains"));
        let black = ManaEffects::for_color(Some("Black"), Some("Swamp"));
        let red = ManaEffects::for_color(Some("Red"), Some("Mountain"));
        let green = ManaEffects::for_color(Some("Green"), Some("Forest"));

        assert_eq!(blue.weather_combo_storm_cooldown_mult, 0.5);
        assert_eq!(white.weather_combo_sunny_yield_mult, 1.10);
        assert_eq!(black.weather_combo_fog_hazard_mult, 0.5);
        assert_eq!(red.weather_combo_heat_dynamite_extra_chains, 2);
        assert_eq!(green.weather_combo_rain_recovery_mult, 2.0);
    }

    #[test]
    fn test_no_color_is_all_defaults() {
        let effects = ManaEffects::default();

        assert_eq!(effects.loan_fee_mult, 1.0);
        assert_eq!(effects.sabotage_cost_mult, 1.0);
        assert_eq!(effects.boss_damage_mult, 1.0);
        assert_eq!(effects.weather_combo_storm_cooldown_mult, 1.0);
    }

    #[test]
    fn test_apply_loan_modifiers_white() {
        let effects = active_effects(Some("White"), Some("Plains"), false);
        let result = apply_loan_modifiers(&effects, 0.20, 100);

        assert!((result.fee_rate - 0.10).abs() < f64::EPSILON);
        assert_eq!(result.limit, 100);
        assert_eq!(result.color.as_deref(), Some("White"));
    }

    #[test]
    fn test_apply_loan_modifiers_black_doubles_limit() {
        let effects = active_effects(Some("Black"), Some("Swamp"), false);
        let result = apply_loan_modifiers(&effects, 0.20, 100);

        assert!((result.fee_rate - 0.30).abs() < 1e-12);
        assert_eq!(result.limit, 200);
        assert_eq!(result.color.as_deref(), Some("Black"));
    }

    #[test]
    fn test_apply_loan_modifiers_no_color_passthrough() {
        let effects = active_effects(None, None, false);
        let result = apply_loan_modifiers(&effects, 0.20, 100);

        assert_eq!(result.fee_rate, 0.20);
        assert_eq!(result.limit, 100);
        assert_eq!(result.color, None);
    }

    #[test]
    fn test_apply_loan_modifiers_when_tapped_passes_through() {
        let effects = active_effects(Some("White"), Some("Plains"), true);
        let result = apply_loan_modifiers(&effects, 0.20, 100);

        assert_eq!(result.color, None);
        assert_eq!(result.fee_rate, 0.20);
        assert_eq!(result.limit, 100);
    }

    #[test]
    fn test_apply_sabotage_modifiers_red_halves_cost() {
        let effects = active_effects(Some("Red"), Some("Mountain"), false);
        let result = apply_sabotage_modifiers(&effects, 10);

        assert_eq!(result.cost, 5);
        assert_eq!(result.steal_depth_pct, 0.0);
    }

    #[test]
    fn test_apply_sabotage_modifiers_black_skim() {
        let effects = active_effects(Some("Black"), Some("Swamp"), false);
        let result = apply_sabotage_modifiers(&effects, 10);

        assert_eq!(result.cost, 10);
        assert_eq!(result.steal_depth_pct, 0.25);
    }

    #[test]
    fn test_get_boss_combat_modifiers_white() {
        let effects = active_effects(Some("White"), Some("Plains"), false);
        let result = boss_combat_modifiers(&effects);

        assert_eq!(result.damage_mult, 1.20);
        assert_eq!(result.hp_mult, 1.0);
        assert_eq!(result.loot_mult, 1.0);
    }

    #[test]
    fn test_get_boss_combat_modifiers_black_tankier_droppier() {
        let effects = active_effects(Some("Black"), Some("Swamp"), false);
        let result = boss_combat_modifiers(&effects);

        assert_eq!(result.damage_mult, 1.0);
        assert_eq!(result.hp_mult, 1.30);
        assert_eq!(result.loot_mult, 1.25);
    }

    #[test]
    fn test_get_boss_combat_modifiers_blue_reveals_hp() {
        let effects = active_effects(Some("Blue"), Some("Island"), false);
        let result = boss_combat_modifiers(&effects);

        assert!(result.reveal_hp);
    }

    #[test]
    fn test_get_weather_combo_modifiers_storm_blue_cooldown() {
        let effects = active_effects(Some("Blue"), Some("Island"), false);
        let result = weather_combo_modifiers(&effects, Some("storm"));

        assert!(result.applied);
        assert_eq!(result.cooldown_mult, 0.5);
    }

    #[test]
    fn test_get_weather_combo_modifiers_sunny_white_yield() {
        let effects = active_effects(Some("White"), Some("Plains"), false);
        let result = weather_combo_modifiers(&effects, Some("sunny"));

        assert!(result.applied);
        assert_eq!(result.yield_mult, 1.10);
    }

    #[test]
    fn test_get_weather_combo_modifiers_reuses_supplied_snapshot() {
        let effects = active_effects(Some("White"), Some("Plains"), false);
        let expected = weather_combo_modifiers(&effects, Some("sunny"));
        let result = weather_combo_modifiers_with_effects(Some("sunny"), Some(&effects), || {
            panic!("snapshot path reloaded effects")
        });

        assert_eq!(result, expected);
    }

    #[test]
    fn test_get_weather_combo_modifiers_mismatch_no_op() {
        let effects = active_effects(Some("White"), Some("Plains"), false);
        let result = weather_combo_modifiers(&effects, Some("storm"));

        assert!(!result.applied);
        assert_eq!(result.cooldown_mult, 1.0);
        assert_eq!(result.yield_mult, 1.0);
    }

    #[test]
    fn test_get_weather_combo_modifiers_no_weather() {
        let effects = active_effects(Some("Blue"), Some("Island"), false);
        let result = weather_combo_modifiers(&effects, None);

        assert!(!result.applied);
    }
}
