//! Player data and rating-selection policy used by team balancing.

use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

use crate::role_performance::{RoleRecord, role_factor};

/// OpenSkill's display origin. Model `mu` values are allowed below this value.
pub const OPENSKILL_MIN_MU: f64 = 25.0;

/// Convert an OpenSkill `mu` delta from the display origin to the 0-3000 scale.
///
/// The canonical seed conversion maps 12,000 MMR to `mu = 85`, so a factor of
/// 50 maps that value to the same 3,000-point ceiling used by Glicko seeding.
pub const OPENSKILL_DISPLAY_SCALE: f64 = 50.0;

/// A player who can participate in matchmaking.
///
/// The fields mirror the Python domain dataclass. They remain public because
/// persistence and orchestration layers build this pure domain value from
/// stored snapshots.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Player {
    pub name: String,
    pub mmr: Option<i64>,
    pub initial_mmr: Option<i64>,
    pub wins: i64,
    pub losses: i64,
    pub preferred_roles: Option<Vec<String>>,
    pub main_role: Option<String>,
    pub glicko_rating: Option<f64>,
    pub glicko_rd: Option<f64>,
    pub glicko_volatility: Option<f64>,
    pub os_mu: Option<f64>,
    pub os_sigma: Option<f64>,
    pub discord_id: Option<i64>,
    pub guild_id: Option<i64>,
    pub jopacoin_balance: i64,
    pub steam_id: Option<i64>,
    pub personal_best_win_streak: i64,
    pub total_bets_placed: i64,
    pub first_leverage_used: bool,
    pub is_solo_grinder: bool,
    pub solo_grinder_checked_at: Option<String>,
    pub preferred_region: Option<String>,
    pub inferred_region: Option<String>,
    /// Win/loss record per role this player has been assigned, keyed by role
    /// ("1".."5"). Absent roles mean no recorded sample, which the role factor
    /// treats as unproven. Empty for players loaded outside team balancing.
    pub role_records: BTreeMap<String, RoleRecord>,
}

impl Player {
    /// Construct an unrated player with the same defaults as the Python model.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Select the player's effective rating for team balancing.
    ///
    /// The priority order matches Python exactly:
    ///
    /// 1. Jopacoin whenever `use_jopacoin` is enabled.
    /// 2. OpenSkill whenever `use_openskill` is enabled, falling back to a
    ///    Glicko rating and then quarter-scaled MMR when `os_mu` is absent.
    /// 3. Glicko when requested and available.
    /// 4. Raw MMR, or zero when the player has no rating data.
    #[must_use]
    pub fn get_value(&self, use_glicko: bool, use_openskill: bool, use_jopacoin: bool) -> f64 {
        if use_jopacoin {
            return self.jopacoin_balance as f64;
        }

        if use_openskill {
            if let Some(os_mu) = self.os_mu {
                return ((os_mu - OPENSKILL_MIN_MU) * OPENSKILL_DISPLAY_SCALE).max(0.0);
            }
            if let Some(glicko_rating) = self.glicko_rating {
                return glicko_rating;
            }
            return self.mmr.map_or(0.0, |mmr| mmr as f64 * 0.25);
        }

        if use_glicko && let Some(glicko_rating) = self.glicko_rating {
            return glicko_rating;
        }

        self.mmr.map_or(0.0, |mmr| mmr as f64)
    }

    /// The rating multiplier for being assigned `role`, from this player's win
    /// rate in that role.
    ///
    /// Replaces the flat off-role multiplier: a player assigned a role they
    /// rarely play has no recorded sample and lands on the floor by itself,
    /// while a proven specialist is scaled up.
    #[must_use]
    pub fn role_factor_for(&self, role: &str) -> f64 {
        role_factor(self.role_records.get(role).copied().unwrap_or_default())
    }

    /// This player's effective value when assigned `role`, after the
    /// role-performance multiplier.
    #[must_use]
    pub fn role_adjusted_value(
        &self,
        role: &str,
        use_glicko: bool,
        use_openskill: bool,
        use_jopacoin: bool,
    ) -> f64 {
        (self.get_value(use_glicko, use_openskill, use_jopacoin) * self.role_factor_for(role))
            .max(0.0)
    }
}

impl Display for Player {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let mmr = self
            .mmr
            .filter(|mmr| *mmr != 0)
            .map_or_else(|| "No MMR".to_owned(), |mmr| mmr.to_string());
        write!(
            formatter,
            "{} (MMR: {mmr}, W-L: {}-{})",
            self.name, self.wins, self.losses
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{OPENSKILL_DISPLAY_SCALE, OPENSKILL_MIN_MU, Player};

    #[test]
    fn test_player_value_rating_sources() {
        let player = Player {
            mmr: Some(2_000),
            glicko_rating: Some(1_800.0),
            wins: 5,
            losses: 3,
            ..Player::new("TestPlayer")
        };

        assert_eq!(player.get_value(true, false, false), 1_800.0);
        assert_eq!(player.get_value(false, false, false), 2_000.0);
        assert_eq!(Player::new("Unrated").get_value(true, false, false), 0.0);
    }

    #[test]
    fn test_glicko_and_openskill_values_agree_at_same_mmr() {
        for mmr in [0_i64, 3_000, 6_000, 9_000, 12_000] {
            let glicko_player = Player {
                mmr: Some(mmr),
                glicko_rating: Some(mmr as f64 * 0.25),
                ..Player::new(format!("G{mmr}"))
            };
            let openskill_player = Player {
                mmr: Some(mmr),
                os_mu: Some(25.0 + mmr as f64 / 200.0),
                ..Player::new(format!("O{mmr}"))
            };

            let glicko_value = glicko_player.get_value(true, false, false);
            let openskill_value = openskill_player.get_value(true, true, false);
            assert!((glicko_value - openskill_value).abs() <= 1.0);
        }

        let all_sources = Player {
            mmr: Some(8_000),
            glicko_rating: Some(1_900.0),
            os_mu: Some(50.0),
            ..Player::new("AllSources")
        };
        assert_eq!(
            all_sources.get_value(true, true, false),
            (50.0 - OPENSKILL_MIN_MU) * OPENSKILL_DISPLAY_SCALE
        );

        let glicko_fallback = Player {
            mmr: Some(8_000),
            glicko_rating: Some(1_900.0),
            ..Player::new("GlickoFallback")
        };
        assert_eq!(glicko_fallback.get_value(false, true, false), 1_900.0);

        let mmr_fallback = Player {
            mmr: Some(8_000),
            ..Player::new("MmrFallback")
        };
        assert_eq!(mmr_fallback.get_value(false, true, false), 2_000.0);

        let below_origin = Player {
            os_mu: Some(20.0),
            ..Player::new("BelowOrigin")
        };
        assert_eq!(below_origin.get_value(true, true, false), 0.0);
    }

    #[test]
    fn test_player_value_jopacoin() {
        let rich = Player {
            mmr: Some(2_000),
            glicko_rating: Some(1_800.0),
            jopacoin_balance: 500,
            ..Player::new("Rich")
        };
        assert_eq!(rich.get_value(true, false, true), 500.0);

        let broke = Player {
            mmr: Some(5_000),
            glicko_rating: Some(2_500.0),
            jopacoin_balance: -200,
            ..Player::new("Broke")
        };
        assert_eq!(broke.get_value(true, false, true), -200.0);

        let zero = Player {
            mmr: Some(3_000),
            jopacoin_balance: 0,
            ..Player::new("Zero")
        };
        assert_eq!(zero.get_value(true, false, true), 0.0);

        let all_sources = Player {
            mmr: Some(3_000),
            glicko_rating: Some(2_000.0),
            os_mu: Some(50.0),
            os_sigma: Some(3.0),
            jopacoin_balance: 7,
            ..Player::new("AllSources")
        };
        assert_eq!(all_sources.get_value(true, true, true), 7.0);
    }
}
