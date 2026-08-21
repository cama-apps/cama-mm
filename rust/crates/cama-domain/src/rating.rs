//! Pure Glicko-2 and Cama rating policy.
//!
//! The Python application uses the third-party `glicko2.Player` both directly
//! and through a `FixedVolatilityPlayer` subclass.  This module keeps those two
//! behaviours in one typed player: [`Player::new`] uses the canonical dynamic
//! volatility update, while [`Player::new_fixed`] preserves volatility exactly.
//! Cama-created players and all synthetic team representatives are fixed.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;

pub const CALIBRATION_RD_THRESHOLD: f64 = 100.0;
pub const MAX_GLICKO_RD: f64 = 250.0;
pub const INITIAL_GLICKO_RD: f64 = MAX_GLICKO_RD;
pub const MAX_RATING_SWING_PER_GAME: f64 = 400.0;
pub const BASE_RATING_DELTA_MULTIPLIER: f64 = 0.98;
pub const MAX_RD_CONTRACTION_PER_GAME: f64 = 0.065;
pub const NEW_PLAYER_MMR_DISCOUNT: i32 = 500;
pub const RD_DECAY_CONSTANT: f64 = 100.0;
pub const RD_DECAY_GRACE_PERIOD_DAYS: i32 = 7;
pub const STREAK_THRESHOLD: i64 = 3;
pub const STREAK_MULTIPLIER_PER_GAME: f64 = 0.30;

pub const LEGACY_STREAK_MULTIPLIER_PER_GAME: f64 = 0.20;
pub const LEGACY_STREAK_THRESHOLD: i64 = 3;

pub const GLICKO2_SCALE: f64 = 173.7178;
pub const GLICKO2_TAU: f64 = 0.5;
const VOLATILITY_EPSILON: f64 = 0.000_001;

#[must_use]
pub const fn cap_glicko_rd(rd: f64) -> f64 {
    if rd > MAX_GLICKO_RD {
        MAX_GLICKO_RD
    } else {
        rd
    }
}

/// SQLite-like values accepted by the historical streak parsers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RecordedValue<'a> {
    Null,
    Integer(i64),
    Real(f64),
    Text(&'a str),
    Boolean(bool),
    Invalid,
}

/// Parse a persisted per-match streak rate, falling back to the legacy curve.
#[must_use]
pub fn recorded_streak_rate(value: RecordedValue<'_>) -> f64 {
    match value {
        RecordedValue::Null | RecordedValue::Invalid => LEGACY_STREAK_MULTIPLIER_PER_GAME,
        RecordedValue::Integer(value) => value as f64,
        RecordedValue::Real(value) => value,
        RecordedValue::Text(value) => value
            .trim()
            .parse()
            .unwrap_or(LEGACY_STREAK_MULTIPLIER_PER_GAME),
        RecordedValue::Boolean(value) => f64::from(u8::from(value)),
    }
}

/// Parse a persisted per-match streak threshold, falling back to the legacy value.
#[must_use]
pub fn recorded_streak_threshold(value: RecordedValue<'_>) -> i64 {
    match value {
        RecordedValue::Null | RecordedValue::Invalid => LEGACY_STREAK_THRESHOLD,
        RecordedValue::Integer(value) => value,
        RecordedValue::Real(value) if value.is_finite() => value.trunc() as i64,
        RecordedValue::Real(_) => LEGACY_STREAK_THRESHOLD,
        RecordedValue::Text(value) => value.trim().parse().unwrap_or(LEGACY_STREAK_THRESHOLD),
        RecordedValue::Boolean(value) => i64::from(value),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VolatilityPolicy {
    Dynamic,
    Fixed,
}

/// A Glicko-2 player snapshot on the external rating/RD scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Player {
    pub rating: f64,
    pub rd: f64,
    pub volatility: f64,
    volatility_policy: VolatilityPolicy,
}

impl Player {
    /// Construct a canonical Glicko-2 player with inferred volatility.
    #[must_use]
    pub const fn new(rating: f64, rd: f64, volatility: f64) -> Self {
        Self {
            rating,
            rd,
            volatility,
            volatility_policy: VolatilityPolicy::Dynamic,
        }
    }

    /// Construct a player whose volatility remains an input, not inferred state.
    #[must_use]
    pub const fn new_fixed(rating: f64, rd: f64, volatility: f64) -> Self {
        Self {
            rating,
            rd,
            volatility,
            volatility_policy: VolatilityPolicy::Fixed,
        }
    }

    /// Apply one Glicko-2 rating period containing the supplied opponents.
    ///
    /// Cama match updates contain one synthetic opponent; accepting a slice
    /// also pins parity with the underlying Python library's public operation.
    pub fn update(&mut self, opponents: &[Opponent]) -> Result<(), RatingError> {
        if opponents.is_empty() {
            return Err(RatingError::NoOpponents);
        }

        let mu = (self.rating - 1500.0) / GLICKO2_SCALE;
        let phi = self.rd / GLICKO2_SCALE;

        let variance_inverse = opponents
            .iter()
            .map(|opponent| {
                let opponent_mu = (opponent.rating - 1500.0) / GLICKO2_SCALE;
                let opponent_phi = opponent.rd / GLICKO2_SCALE;
                let g = g(opponent_phi);
                let expectation = expectation(mu, opponent_mu, opponent_phi);
                g * g * expectation * (1.0 - expectation)
            })
            .sum::<f64>();
        let variance = 1.0 / variance_inverse;

        let score_sum = opponents
            .iter()
            .map(|opponent| {
                let opponent_mu = (opponent.rating - 1500.0) / GLICKO2_SCALE;
                let opponent_phi = opponent.rd / GLICKO2_SCALE;
                g(opponent_phi) * (opponent.score - expectation(mu, opponent_mu, opponent_phi))
            })
            .sum::<f64>();

        let new_volatility = match self.volatility_policy {
            VolatilityPolicy::Fixed => self.volatility,
            VolatilityPolicy::Dynamic => {
                new_volatility(phi, self.volatility, variance, variance * score_sum)
            }
        };

        let pre_rating_phi = (phi * phi + new_volatility * new_volatility).sqrt();
        let new_phi = 1.0 / (1.0 / (pre_rating_phi * pre_rating_phi) + 1.0 / variance).sqrt();
        let new_mu = mu + new_phi * new_phi * score_sum;

        self.rating = new_mu * GLICKO2_SCALE + 1500.0;
        self.rd = new_phi * GLICKO2_SCALE;
        self.volatility = new_volatility;
        Ok(())
    }

    /// Increase RD for an inactive rating period, matching Glicko-2 step 6.
    pub fn did_not_compete(&mut self) {
        let phi = self.rd / GLICKO2_SCALE;
        self.rd = (phi * phi + self.volatility * self.volatility).sqrt() * GLICKO2_SCALE;
    }
}

/// One opponent observation in a Glicko-2 rating period.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opponent {
    pub rating: f64,
    pub rd: f64,
    pub score: f64,
}

impl Opponent {
    #[must_use]
    pub const fn new(rating: f64, rd: f64, score: f64) -> Self {
        Self { rating, rd, score }
    }
}

fn g(phi: f64) -> f64 {
    1.0 / (1.0 + 3.0 * phi * phi / std::f64::consts::PI.powi(2)).sqrt()
}

fn expectation(mu: f64, opponent_mu: f64, opponent_phi: f64) -> f64 {
    1.0 / (1.0 + (-g(opponent_phi) * (mu - opponent_mu)).exp())
}

fn volatility_objective(x: f64, delta: f64, phi: f64, variance: f64, a: f64) -> f64 {
    let exponent = x.exp();
    let numerator = exponent * (delta * delta - phi * phi - variance - exponent);
    let denominator = 2.0 * (phi * phi + variance + exponent).powi(2);
    numerator / denominator - (x - a) / GLICKO2_TAU.powi(2)
}

fn new_volatility(phi: f64, volatility: f64, variance: f64, delta: f64) -> f64 {
    let a = (volatility * volatility).ln();
    let mut left = a;
    let mut right = if delta * delta > phi * phi + variance {
        (delta * delta - phi * phi - variance).ln()
    } else {
        let mut k = 1.0;
        while volatility_objective(a - k * GLICKO2_TAU, delta, phi, variance, a) < 0.0 {
            k += 1.0;
        }
        a - k * GLICKO2_TAU
    };

    let mut f_left = volatility_objective(left, delta, phi, variance, a);
    let mut f_right = volatility_objective(right, delta, phi, variance, a);
    while (right - left).abs() > VOLATILITY_EPSILON {
        let candidate = left + (left - right) * f_left / (f_right - f_left);
        let f_candidate = volatility_objective(candidate, delta, phi, variance, a);
        if f_candidate * f_right <= 0.0 {
            left = right;
            f_left = f_right;
        } else {
            f_left /= 2.0;
        }
        right = candidate;
        f_right = f_candidate;
    }
    (left / 2.0).exp()
}

/// Deployment-configurable values consumed by the pure rating policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RatingConfig {
    pub calibration_rd_threshold: f64,
    pub initial_rd: f64,
    pub max_rating_swing_per_game: f64,
    pub base_rating_delta_multiplier: f64,
    pub max_rd_contraction_per_game: f64,
    pub new_player_mmr_discount: i32,
    pub rd_decay_constant: f64,
    pub rd_decay_grace_period_days: i32,
    pub streak_threshold: i64,
    pub streak_multiplier_per_game: f64,
}

impl Default for RatingConfig {
    fn default() -> Self {
        Self {
            calibration_rd_threshold: CALIBRATION_RD_THRESHOLD,
            initial_rd: INITIAL_GLICKO_RD,
            max_rating_swing_per_game: MAX_RATING_SWING_PER_GAME,
            base_rating_delta_multiplier: BASE_RATING_DELTA_MULTIPLIER,
            max_rd_contraction_per_game: MAX_RD_CONTRACTION_PER_GAME,
            new_player_mmr_discount: NEW_PLAYER_MMR_DISCOUNT,
            rd_decay_constant: RD_DECAY_CONSTANT,
            rd_decay_grace_period_days: RD_DECAY_GRACE_PERIOD_DAYS,
            streak_threshold: STREAK_THRESHOLD,
            streak_multiplier_per_game: STREAK_MULTIPLIER_PER_GAME,
        }
    }
}

/// Pure Cama rating service.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CamaRatingSystem {
    pub initial_rd: f64,
    pub initial_volatility: f64,
    pub config: RatingConfig,
}

impl Default for CamaRatingSystem {
    fn default() -> Self {
        Self::new(INITIAL_GLICKO_RD, 0.06)
    }
}

impl CamaRatingSystem {
    pub const MMR_MIN: i32 = 0;
    pub const MMR_MAX: i32 = 12_000;
    pub const RATING_MIN: f64 = 0.0;
    pub const RATING_MAX: f64 = 3000.0;

    #[must_use]
    pub fn new(initial_rd: f64, initial_volatility: f64) -> Self {
        Self {
            initial_rd: cap_glicko_rd(initial_rd),
            initial_volatility,
            config: RatingConfig::default(),
        }
    }

    #[must_use]
    pub const fn with_config(
        initial_rd: f64,
        initial_volatility: f64,
        config: RatingConfig,
    ) -> Self {
        Self {
            initial_rd: cap_glicko_rd(initial_rd),
            initial_volatility,
            config,
        }
    }

    #[must_use]
    pub const fn mmr_to_rating_scale() -> f64 {
        (Self::RATING_MAX - Self::RATING_MIN) / (Self::MMR_MAX - Self::MMR_MIN) as f64
    }

    #[must_use]
    pub fn aggregate_team_stats(&self, players: &[Player]) -> TeamStats {
        if players.is_empty() {
            return TeamStats {
                rating: 0.0,
                rd: INITIAL_GLICKO_RD,
                volatility: self.initial_volatility,
            };
        }
        let count = players.len() as f64;
        TeamStats {
            rating: players.iter().map(|player| player.rating).sum::<f64>() / count,
            rd: (players.iter().map(|player| player.rd.powi(2)).sum::<f64>() / count).sqrt(),
            volatility: players.iter().map(|player| player.volatility).sum::<f64>() / count,
        }
    }

    #[must_use]
    pub fn is_calibrated(&self, rd: f64) -> bool {
        rd <= self.config.calibration_rd_threshold
    }

    #[must_use]
    pub const fn streak_multiplier_per_game(&self) -> f64 {
        self.config.streak_multiplier_per_game
    }

    #[must_use]
    pub const fn base_rating_delta_multiplier(&self) -> f64 {
        self.config.base_rating_delta_multiplier
    }

    #[must_use]
    pub const fn streak_threshold(&self) -> i64 {
        self.config.streak_threshold
    }

    #[must_use]
    pub fn apply_rd_decay(&self, rd: f64, days_since_last_match: i32) -> f64 {
        let rd_cap = cap_glicko_rd(self.config.initial_rd);
        if rd >= rd_cap {
            return rd_cap;
        }
        if days_since_last_match <= self.config.rd_decay_grace_period_days {
            return rd;
        }
        let decay_days = days_since_last_match - self.config.rd_decay_grace_period_days;
        let periods = f64::from(decay_days) / 7.0;
        let decayed = (rd * rd + self.config.rd_decay_constant.powi(2) * periods).sqrt();
        rd_cap.min(decayed)
    }

    #[must_use]
    pub fn predict_win_probability(
        rating: f64,
        rd: f64,
        opponent_rating: f64,
        opponent_rd: f64,
    ) -> f64 {
        let combined_rd = rd.hypot(opponent_rd);
        let g = 1.0
            / (1.0 + 3.0 * (combined_rd / GLICKO2_SCALE).powi(2) / std::f64::consts::PI.powi(2))
                .sqrt();
        let probability = 1.0 / (1.0 + (-g * (rating - opponent_rating) / GLICKO2_SCALE).exp());
        probability.clamp(0.0, 1.0)
    }

    #[must_use]
    pub fn mmr_to_rating(&self, mmr: i32) -> f64 {
        let clamped = mmr.clamp(Self::MMR_MIN, Self::MMR_MAX);
        f64::from(clamped - Self::MMR_MIN) * Self::mmr_to_rating_scale() + Self::RATING_MIN
    }

    #[must_use]
    pub fn new_player_seed_mmr(&self, mmr: i32) -> i32 {
        let capped = mmr.clamp(Self::MMR_MIN, Self::MMR_MAX);
        (capped - self.config.new_player_mmr_discount).max(Self::MMR_MIN)
    }

    #[must_use]
    pub fn rating_to_display(&self, rating: f64) -> i64 {
        rating.round_ties_even() as i64
    }

    #[must_use]
    pub fn create_player_from_mmr(&self, mmr: Option<i32>) -> Player {
        let source_mmr = mmr.unwrap_or(4000);
        let rating = self.mmr_to_rating(self.new_player_seed_mmr(source_mmr));
        Player::new_fixed(rating, self.initial_rd, self.initial_volatility)
    }

    #[must_use]
    pub const fn create_player_from_rating(&self, rating: f64, rd: f64, volatility: f64) -> Player {
        Player::new_fixed(rating, cap_glicko_rd(rd), volatility)
    }

    #[must_use]
    pub fn calculate_streak_multiplier(
        &self,
        recent_outcomes: &[bool],
        won: bool,
        streak_multiplier_per_game: Option<f64>,
        streak_threshold: Option<i64>,
    ) -> StreakAdjustment {
        let Some(most_recent) = recent_outcomes.first() else {
            return StreakAdjustment::default();
        };
        if *most_recent != won {
            return StreakAdjustment::default();
        }

        let streak_length = 1 + recent_outcomes
            .iter()
            .take_while(|outcome| **outcome == won)
            .count();
        let threshold = streak_threshold.unwrap_or(self.config.streak_threshold);
        if (streak_length as i64) < threshold {
            return StreakAdjustment {
                streak_length,
                multiplier: 1.0,
            };
        }
        let rate = streak_multiplier_per_game.unwrap_or(self.config.streak_multiplier_per_game);
        StreakAdjustment {
            streak_length,
            multiplier: 1.0 + rate * (streak_length - 2) as f64,
        }
    }

    #[must_use]
    pub fn update_player_rating(
        &self,
        player: Player,
        team_rating: f64,
        opponent_rating: f64,
        opponent_rd: f64,
        result: f64,
    ) -> UpdatedPlayer {
        self.update_player_rating_with_modifiers(
            player,
            team_rating,
            opponent_rating,
            opponent_rd,
            result,
            PlayerUpdateModifiers::default(),
        )
    }

    #[must_use]
    pub fn update_player_rating_with_modifiers(
        &self,
        player: Player,
        team_rating: f64,
        opponent_rating: f64,
        opponent_rd: f64,
        result: f64,
        modifiers: PlayerUpdateModifiers,
    ) -> UpdatedPlayer {
        let mut synthetic = Player::new_fixed(team_rating, player.rd, player.volatility);
        synthetic
            .update(&[Opponent::new(opponent_rating, opponent_rd, result)])
            .expect("a single synthetic opponent is always valid");

        let base_multiplier = modifiers
            .base_rating_delta_multiplier
            .unwrap_or(self.config.base_rating_delta_multiplier);
        let mut delta =
            (synthetic.rating - team_rating) * base_multiplier * modifiers.streak_multiplier;
        if delta > 0.0 {
            delta *= modifiers.gain_multiplier;
        }
        delta = delta.clamp(
            -self.config.max_rating_swing_per_game,
            self.config.max_rating_swing_per_game,
        );

        let contraction_floor = player.rd * (1.0 - self.config.max_rd_contraction_per_game);
        UpdatedPlayer {
            rating: (player.rating + delta).max(0.0),
            rd: player.rd.min(synthetic.rd.max(contraction_floor)),
            volatility: synthetic.volatility,
        }
    }

    pub fn update_ratings_after_match<Id>(
        &self,
        team1: &[TeamPlayer<Id>],
        team2: &[TeamPlayer<Id>],
        winning_team: i32,
    ) -> Result<TeamUpdates<Id>, RatingError>
    where
        Id: Clone + Eq + Hash,
    {
        self.update_ratings_after_match_with_options(
            team1,
            team2,
            winning_team,
            &MatchUpdateOptions::default(),
        )
    }

    pub fn update_ratings_after_match_with_options<Id>(
        &self,
        team1: &[TeamPlayer<Id>],
        team2: &[TeamPlayer<Id>],
        winning_team: i32,
        options: &MatchUpdateOptions<Id>,
    ) -> Result<TeamUpdates<Id>, RatingError>
    where
        Id: Clone + Eq + Hash,
    {
        if team1.is_empty() {
            return Err(RatingError::EmptyTeam1);
        }
        if team2.is_empty() {
            return Err(RatingError::EmptyTeam2);
        }
        if !matches!(winning_team, 1 | 2) {
            return Err(RatingError::InvalidWinningTeam(winning_team));
        }

        let team1_stats = self
            .aggregate_team_stats(&team1.iter().map(|member| member.player).collect::<Vec<_>>());
        let team2_stats = self
            .aggregate_team_stats(&team2.iter().map(|member| member.player).collect::<Vec<_>>());
        let team1_result = if winning_team == 1 { 1.0 } else { 0.0 };
        let team2_result = if winning_team == 2 { 1.0 } else { 0.0 };

        let update_team = |team: &[TeamPlayer<Id>],
                           team_rating: f64,
                           opponent: TeamStats,
                           result: f64|
         -> Vec<UpdatedRating<Id>> {
            team.iter()
                .map(|member| {
                    let updated = self.update_player_rating_with_modifiers(
                        member.player,
                        team_rating,
                        opponent.rating,
                        opponent.rd,
                        result,
                        PlayerUpdateModifiers {
                            streak_multiplier: options
                                .streak_multipliers
                                .get(&member.id)
                                .copied()
                                .unwrap_or(1.0),
                            base_rating_delta_multiplier: options.base_rating_delta_multiplier,
                            gain_multiplier: options
                                .gain_multipliers
                                .get(&member.id)
                                .copied()
                                .unwrap_or(1.0),
                        },
                    );
                    UpdatedRating {
                        rating: updated.rating,
                        rd: updated.rd,
                        volatility: updated.volatility,
                        id: member.id.clone(),
                    }
                })
                .collect()
        };

        Ok(TeamUpdates {
            team1: update_team(team1, team1_stats.rating, team2_stats, team1_result),
            team2: update_team(team2, team2_stats.rating, team1_stats, team2_result),
        })
    }

    #[must_use]
    pub fn rating_uncertainty_percentage(&self, rd: f64) -> f64 {
        crate::formatting::python_round_decimals((rd / MAX_GLICKO_RD * 100.0).min(100.0), 1)
    }

    #[must_use]
    pub fn get_rating_uncertainty_percentage(&self, rd: f64) -> f64 {
        self.rating_uncertainty_percentage(rd)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TeamStats {
    pub rating: f64,
    pub rd: f64,
    pub volatility: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreakAdjustment {
    pub streak_length: usize,
    pub multiplier: f64,
}

impl Default for StreakAdjustment {
    fn default() -> Self {
        Self {
            streak_length: 1,
            multiplier: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UpdatedPlayer {
    pub rating: f64,
    pub rd: f64,
    pub volatility: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerUpdateModifiers {
    pub streak_multiplier: f64,
    pub base_rating_delta_multiplier: Option<f64>,
    pub gain_multiplier: f64,
}

impl Default for PlayerUpdateModifiers {
    fn default() -> Self {
        Self {
            streak_multiplier: 1.0,
            base_rating_delta_multiplier: None,
            gain_multiplier: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TeamPlayer<Id> {
    pub player: Player,
    pub id: Id,
}

impl<Id> TeamPlayer<Id> {
    #[must_use]
    pub const fn new(player: Player, id: Id) -> Self {
        Self { player, id }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpdatedRating<Id> {
    pub rating: f64,
    pub rd: f64,
    pub volatility: f64,
    pub id: Id,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TeamUpdates<Id> {
    pub team1: Vec<UpdatedRating<Id>>,
    pub team2: Vec<UpdatedRating<Id>>,
}

#[derive(Clone, Debug)]
pub struct MatchUpdateOptions<Id> {
    pub streak_multipliers: HashMap<Id, f64>,
    pub base_rating_delta_multiplier: Option<f64>,
    pub gain_multipliers: HashMap<Id, f64>,
}

impl<Id> Default for MatchUpdateOptions<Id> {
    fn default() -> Self {
        Self {
            streak_multipliers: HashMap::new(),
            base_rating_delta_multiplier: None,
            gain_multipliers: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RatingError {
    NoOpponents,
    EmptyTeam1,
    EmptyTeam2,
    InvalidWinningTeam(i32),
}

impl fmt::Display for RatingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOpponents => formatter.write_str("at least one opponent is required"),
            Self::EmptyTeam1 => formatter.write_str("team1_players cannot be empty"),
            Self::EmptyTeam2 => formatter.write_str("team2_players cannot be empty"),
            Self::InvalidWinningTeam(team) => {
                write!(formatter, "winning_team must be 1 or 2, got {team}")
            }
        }
    }
}

impl std::error::Error for RatingError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {actual} to be within {tolerance} of {expected}"
        );
    }

    fn member<Id>(rating: f64, rd: f64, id: Id) -> TeamPlayer<Id> {
        TeamPlayer::new(Player::new(rating, rd, 0.06), id)
    }

    fn repeated_team<Id: Clone>(count: usize, rating: f64, rd: f64, id: Id) -> Vec<TeamPlayer<Id>> {
        (0..count).map(|_| member(rating, rd, id.clone())).collect()
    }

    fn high_rd_on_strong_team() -> (Vec<TeamPlayer<&'static str>>, Vec<TeamPlayer<&'static str>>) {
        (
            vec![
                member(413.0, 334.0, "high_rd_low_rating"),
                member(1400.0, 80.0, "calibrated_1"),
                member(1350.0, 75.0, "calibrated_2"),
                member(1300.0, 85.0, "calibrated_3"),
                member(1250.0, 90.0, "calibrated_4"),
            ],
            vec![
                member(1350.0, 80.0, "enemy_1"),
                member(1300.0, 85.0, "enemy_2"),
                member(1250.0, 75.0, "enemy_3"),
                member(1200.0, 90.0, "enemy_4"),
                member(1180.0, 80.0, "enemy_5"),
            ],
        )
    }

    fn balanced_calibrated_teams() -> (Vec<TeamPlayer<&'static str>>, Vec<TeamPlayer<&'static str>>)
    {
        (
            vec![
                member(1250.0, 75.0, "player_1"),
                member(1220.0, 80.0, "player_2"),
                member(1200.0, 85.0, "player_3"),
                member(1180.0, 70.0, "player_4"),
                member(1150.0, 90.0, "player_5"),
            ],
            vec![
                member(1240.0, 80.0, "enemy_1"),
                member(1210.0, 75.0, "enemy_2"),
                member(1190.0, 85.0, "enemy_3"),
                member(1170.0, 70.0, "enemy_4"),
                member(1160.0, 90.0, "enemy_5"),
            ],
        )
    }

    #[test]
    fn test_mmr_to_rating_extreme_values() {
        let system = CamaRatingSystem::default();
        assert!(system.mmr_to_rating(0) >= 0.0);
        assert!(system.mmr_to_rating(12_000) <= 3000.0);
        assert!(system.mmr_to_rating(-100) >= 0.0);
        assert!(system.mmr_to_rating(20_000) <= 3000.0);
    }

    #[test]
    fn test_mmr_to_rating_linear_mapping() {
        let system = CamaRatingSystem::default();
        assert!(system.mmr_to_rating(6000) > 1000.0);
        assert!(system.mmr_to_rating(6000) < 2000.0);
        assert!(system.mmr_to_rating(9000) > system.mmr_to_rating(3000));
    }

    macro_rules! seed_discount_test {
        ($name:ident, $mmr:expr, $expected:expr) => {
            #[test]
            fn $name() {
                let player = CamaRatingSystem::default().create_player_from_mmr(Some($mmr));
                assert_close(player.rating, $expected, 1e-12);
            }
        };
    }

    seed_discount_test!(
        test_new_player_seed_applies_discount_after_mmr_cap_8000_1875_0,
        8000,
        1875.0
    );
    seed_discount_test!(
        test_new_player_seed_applies_discount_after_mmr_cap_12000_2875_0,
        12_000,
        2875.0
    );
    seed_discount_test!(
        test_new_player_seed_applies_discount_after_mmr_cap_20000_2875_0,
        20_000,
        2875.0
    );
    seed_discount_test!(
        test_new_player_seed_applies_discount_after_mmr_cap_250_0_0,
        250,
        0.0
    );

    #[test]
    fn test_new_player_seed_confidence_limits_even_match_movement() {
        let system = CamaRatingSystem::default();
        let seed = system.create_player_from_mmr(Some(8_000));
        let team1 = (1..=5)
            .map(|id| TeamPlayer::new(seed, id))
            .collect::<Vec<_>>();
        let team2 = (6..=10)
            .map(|id| TeamPlayer::new(seed, id))
            .collect::<Vec<_>>();

        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();

        assert_close(seed.rd, 250.0, 1e-12);
        assert_close(updates.team1[0].rating - seed.rating, 104.940_754, 1e-6);
        assert_close(updates.team2[0].rating - seed.rating, -104.940_754, 1e-6);
    }

    #[test]
    fn test_create_player_from_none_mmr() {
        let player = CamaRatingSystem::default().create_player_from_mmr(None);
        assert!(player.rating > 0.0);
        assert!(player.rd > 0.0);
        assert!(player.volatility > 0.0);
    }

    #[test]
    fn test_rating_update_extreme_ratings() {
        let system = CamaRatingSystem::default();
        let mut high = system.create_player_from_rating(2800.0, 50.0, 0.06);
        let mut low = system.create_player_from_rating(200.0, 50.0, 0.06);
        let high_initial = high.rating;
        let low_initial = low.rating;
        high.update(&[Opponent::new(low.rating, low.rd, 1.0)])
            .unwrap();
        assert!(high.rating > high_initial && high.rating > 0.0);
        low.update(&[Opponent::new(high.rating, high.rd, 0.0)])
            .unwrap();
        assert!(low.rating < low_initial && low.rating >= 0.0);
    }

    #[test]
    fn test_rating_update_new_player_vs_experienced() {
        let system = CamaRatingSystem::default();
        let mut new_player = system.create_player_from_rating(1500.0, 350.0, 0.06);
        let mut experienced = system.create_player_from_rating(1500.0, 50.0, 0.06);
        new_player
            .update(&[Opponent::new(experienced.rating, experienced.rd, 1.0)])
            .unwrap();
        experienced
            .update(&[Opponent::new(new_player.rating, new_player.rd, 0.0)])
            .unwrap();
        assert!((new_player.rating - 1500.0).abs() >= (experienced.rating - 1500.0).abs());
    }

    #[test]
    fn test_rapid_rating_changes() {
        let system = CamaRatingSystem::default();
        let mut player1 = system.create_player_from_rating(1500.0, 350.0, 0.06);
        let mut player2 = system.create_player_from_rating(1500.0, 350.0, 0.06);
        for _ in 0..10 {
            player1
                .update(&[Opponent::new(player2.rating, player2.rd, 1.0)])
                .unwrap();
            player2
                .update(&[Opponent::new(player1.rating, player1.rd, 0.0)])
                .unwrap();
        }
        assert!(player1.rating > 1500.0);
        assert!(player2.rating < 1500.0);
    }

    #[test]
    fn test_rating_uncertainty_percentage() {
        let system = CamaRatingSystem::default();
        let low = system.rating_uncertainty_percentage(30.0);
        let high = system.rating_uncertainty_percentage(350.0);
        assert!((0.0..=100.0).contains(&low) && low < 50.0);
        assert!((0.0..=100.0).contains(&high) && high > 50.0);
        assert!(high > low);
    }

    #[test]
    fn test_team_rating_update() {
        let system = CamaRatingSystem::default();
        let team1 = (0..5)
            .map(|index| member(1500.0 + f64::from(index) * 10.0, 80.0, 1000 + index))
            .collect::<Vec<_>>();
        let team2 = (0..5)
            .map(|index| member(1500.0 + f64::from(index) * 10.0, 80.0, 2000 + index))
            .collect::<Vec<_>>();
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert_eq!(updates.team1.len(), 5);
        assert_eq!(updates.team2.len(), 5);
        let winners = updates
            .team1
            .iter()
            .zip(&team1)
            .map(|(updated, old)| updated.rating - old.player.rating)
            .collect::<Vec<_>>();
        let losers = updates
            .team2
            .iter()
            .zip(&team2)
            .map(|(updated, old)| updated.rating - old.player.rating)
            .collect::<Vec<_>>();
        assert!(winners.iter().all(|delta| *delta > 0.0 && *delta < 100.0));
        assert!(losers.iter().all(|delta| *delta < 0.0 && *delta > -100.0));
        assert!(
            winners.iter().copied().reduce(f64::max).unwrap()
                - winners.iter().copied().reduce(f64::min).unwrap()
                < 5.0
        );
        assert!(
            updates
                .team1
                .iter()
                .chain(&updates.team2)
                .all(|p| p.rd > 0.0)
        );
    }

    #[test]
    fn test_single_even_team_match_has_moderate_change() {
        let system = CamaRatingSystem::default();
        let team1 = repeated_team(5, 1500.0, 350.0, 1);
        let team2 = repeated_team(5, 1500.0, 350.0, 6);
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert!(
            updates
                .team1
                .iter()
                .all(|p| p.rating > 1500.0 && p.rating < 1800.0)
        );
        assert!(
            updates
                .team2
                .iter()
                .all(|p| p.rating < 1500.0 && p.rating > 1200.0)
        );
    }

    #[test]
    fn test_individual_deltas_depend_on_rd() {
        let system = CamaRatingSystem::default();
        let high = repeated_team(5, 1500.0, 250.0, 1);
        let low = repeated_team(5, 1500.0, 80.0, 2);
        let first = system.update_ratings_after_match(&high, &low, 1).unwrap();
        assert!((first.team1[0].rating - 1500.0).abs() > (first.team2[0].rating - 1500.0).abs());
        let second = system.update_ratings_after_match(&low, &high, 1).unwrap();
        assert!((second.team2[0].rating - 1500.0).abs() > (second.team1[0].rating - 1500.0).abs());
    }

    #[test]
    fn test_upset_rewards_underdog() {
        let system = CamaRatingSystem::default();
        let favorite = repeated_team(5, 1800.0, 80.0, 1);
        let underdog = repeated_team(5, 1200.0, 80.0, 2);
        let upset = system
            .update_ratings_after_match(&underdog, &favorite, 1)
            .unwrap();
        let expected = system
            .update_ratings_after_match(&favorite, &underdog, 1)
            .unwrap();
        assert!(upset.team1[0].rating - 1200.0 > expected.team1[0].rating - 1800.0);
        assert!((upset.team2[0].rating - 1800.0).abs() > (expected.team2[0].rating - 1200.0).abs());
    }

    #[test]
    fn test_hybrid_delta_guardrails_winner() {
        let system = CamaRatingSystem::default();
        let mixed = vec![
            member(1500.0, 80.0, 1),
            member(1500.0, 80.0, 2),
            member(1500.0, 80.0, 3),
            member(1500.0, 150.0, 4),
            member(1500.0, 150.0, 5),
        ];
        let opponent = repeated_team(5, 1500.0, 80.0, 10);
        let updates = system
            .update_ratings_after_match(&mixed, &opponent, 1)
            .unwrap();
        let team_delta = updates.team1[0].rating - 1500.0;
        assert!(team_delta > 0.0);
        assert!((updates.team1[2].rating - updates.team1[0].rating).abs() < 0.01);
        assert!(
            updates.team1[3..]
                .iter()
                .all(|p| p.rating - 1500.0 >= team_delta - 0.01)
        );
    }

    #[test]
    fn test_hybrid_delta_guardrails_loser() {
        let system = CamaRatingSystem::default();
        let mixed = vec![
            member(1500.0, 80.0, 1),
            member(1500.0, 80.0, 2),
            member(1500.0, 80.0, 3),
            member(1500.0, 150.0, 4),
            member(1500.0, 150.0, 5),
        ];
        let opponent = repeated_team(5, 1500.0, 80.0, 10);
        let updates = system
            .update_ratings_after_match(&mixed, &opponent, 2)
            .unwrap();
        let team_delta = updates.team1[0].rating - 1500.0;
        assert!(team_delta < 0.0);
        assert!((updates.team1[2].rating - updates.team1[0].rating).abs() < 0.01);
        assert!(
            updates.team1[3..]
                .iter()
                .all(|p| p.rating - 1500.0 <= team_delta + 0.01)
        );
    }

    #[test]
    fn test_mixed_team_rd_weighted_behavior() {
        let system = CamaRatingSystem::default();
        let team1 = vec![
            member(1600.0, 80.0, 1),
            member(1400.0, 250.0, 2),
            member(1500.0, 120.0, 3),
            member(1550.0, 90.0, 4),
            member(1450.0, 180.0, 5),
        ];
        let team2 = repeated_team(5, 1500.0, 80.0, 10);
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        let originals = [1600.0, 1400.0, 1500.0, 1550.0, 1450.0];
        let deltas = updates
            .team1
            .iter()
            .zip(originals)
            .map(|(p, old)| p.rating - old)
            .collect::<Vec<_>>();
        assert!(deltas[1] > deltas[0].max(deltas[3]));
        assert!(deltas.iter().all(|delta| *delta > 0.0));
        assert!(updates.team2.iter().all(|p| p.rating < 1500.0));
        assert!(
            updates
                .team2
                .iter()
                .all(|p| (p.rating - updates.team2[0].rating).abs() < 0.01)
        );
    }

    #[test]
    fn test_all_calibrating_team_uses_threshold_rd() {
        let system = CamaRatingSystem::default();
        let team1 = repeated_team(5, 1500.0, 150.0, 1);
        let team2 = repeated_team(5, 1500.0, 150.0, 2);
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert!(updates.team1.iter().all(|p| p.rating > 1500.0));
        assert!(updates.team2.iter().all(|p| p.rating < 1500.0));
        assert!(
            updates
                .team1
                .iter()
                .all(|p| (p.rating - updates.team1[0].rating).abs() < 1.0)
        );
    }

    #[test]
    fn test_zero_rating() {
        let system = CamaRatingSystem::default();
        let mut player = system.create_player_from_rating(0.0, 350.0, 0.06);
        let opponent = system.create_player_from_rating(1500.0, 350.0, 0.06);
        assert_eq!(player.rating, 0.0);
        player
            .update(&[Opponent::new(opponent.rating, opponent.rd, 1.0)])
            .unwrap();
        assert!(player.rating > 0.0);
    }

    #[test]
    fn test_maximum_rating() {
        let system = CamaRatingSystem::default();
        let mut player = system.create_player_from_rating(3000.0, 50.0, 0.06);
        let opponent = system.create_player_from_rating(1500.0, 350.0, 0.06);
        player
            .update(&[Opponent::new(opponent.rating, opponent.rd, 0.0)])
            .unwrap();
        assert!(player.rating < 3000.0 && player.rating > 0.0);
    }

    #[test]
    fn test_very_small_rd() {
        let system = CamaRatingSystem::default();
        let mut player = system.create_player_from_rating(1500.0, 10.0, 0.06);
        let opponent = system.create_player_from_rating(1500.0, 350.0, 0.06);
        player
            .update(&[Opponent::new(opponent.rating, opponent.rd, 1.0)])
            .unwrap();
        let change = (player.rating - 1500.0).abs();
        assert!(change > 0.0 && change < 100.0);
    }

    #[test]
    fn test_is_calibrated_threshold() {
        let system = CamaRatingSystem::default();
        assert!(system.is_calibrated(CALIBRATION_RD_THRESHOLD));
        assert!(!system.is_calibrated(CALIBRATION_RD_THRESHOLD + 0.1));
    }

    #[test]
    fn test_rd_decay_starts_continuously_after_seven_day_grace() {
        let system = CamaRatingSystem::default();
        assert_eq!(system.apply_rd_decay(150.0, 7), 150.0);
        assert!(system.apply_rd_decay(150.0, 8) > 150.0);
        assert_close(
            system.apply_rd_decay(150.0, 14),
            (150.0_f64.powi(2) + 100.0_f64.powi(2)).sqrt(),
            1e-10,
        );
        let periods = (30.0 - 7.0) / 7.0;
        assert_close(
            system.apply_rd_decay(150.0, 30),
            (150.0_f64.powi(2) + 100.0_f64.powi(2) * periods).sqrt(),
            1e-10,
        );
    }

    #[test]
    fn test_rd_decay_never_exceeds_new_player_rd() {
        let system = CamaRatingSystem::default();
        assert_eq!(system.apply_rd_decay(350.0, 100), 250.0);
        assert_eq!(system.apply_rd_decay(240.0, 70), 250.0);
    }

    #[test]
    fn test_existing_rating_rd_is_limited_by_shared_cap() {
        let system = CamaRatingSystem::default();
        assert_eq!(
            system.create_player_from_rating(1_500.0, 350.0, 0.06).rd,
            250.0
        );
    }

    #[test]
    fn test_probability_uses_both_teams_uncertainty_and_is_symmetric() {
        let stronger = CamaRatingSystem::predict_win_probability(1700.0, 350.0, 1500.0, 50.0);
        let weaker = CamaRatingSystem::predict_win_probability(1500.0, 50.0, 1700.0, 350.0);
        assert_close(stronger, 0.682_652_690_3, 1e-10);
        assert_close(stronger + weaker, 1.0, 1e-12);
    }

    macro_rules! fixed_volatility_test {
        ($name:ident, $volatility:expr) => {
            #[test]
            fn $name() {
                let system = CamaRatingSystem::default();
                let player = system.create_player_from_rating(1500.0, 200.0, $volatility);
                let updated = system.update_player_rating(player, 1500.0, 1500.0, 100.0, 1.0);
                assert_eq!(updated.volatility, $volatility);
            }
        };
    }

    fixed_volatility_test!(
        test_match_update_preserves_each_players_volatility_0_04,
        0.04
    );
    fixed_volatility_test!(
        test_match_update_preserves_each_players_volatility_0_06,
        0.06
    );
    fixed_volatility_test!(
        test_match_update_preserves_each_players_volatility_0_08,
        0.08
    );

    #[test]
    fn test_high_rd_player_win_bounded_by_cap() {
        let system = CamaRatingSystem::default();
        let (team1, team2) = high_rd_on_strong_team();
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        let delta = updates.team1[0].rating - team1[0].player.rating;
        assert!(delta > 100.0 && delta <= MAX_RATING_SWING_PER_GAME);
    }

    #[test]
    fn test_high_rd_player_loss_bounded_by_cap() {
        let system = CamaRatingSystem::default();
        let (team1, team2) = high_rd_on_strong_team();
        let updates = system
            .update_ratings_after_match(&team1, &team2, 2)
            .unwrap();
        let delta = updates.team1[0].rating - team1[0].player.rating;
        assert!((-MAX_RATING_SWING_PER_GAME..0.0).contains(&delta));
    }

    #[test]
    fn test_win_and_loss_both_bounded() {
        let system = CamaRatingSystem::default();
        let (team1, team2) = high_rd_on_strong_team();
        let win = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap()
            .team1[0]
            .rating
            - 413.0;
        let loss = system
            .update_ratings_after_match(&team1, &team2, 2)
            .unwrap()
            .team1[0]
            .rating
            - 413.0;
        assert!(win > 0.0 && win <= MAX_RATING_SWING_PER_GAME);
        assert!((-MAX_RATING_SWING_PER_GAME..0.0).contains(&loss));
    }

    #[test]
    fn test_calibrated_players_small_deltas() {
        let system = CamaRatingSystem::default();
        let (team1, team2) = balanced_calibrated_teams();
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert!(updates.team1.iter().zip(&team1).all(|(new, old)| {
            let delta = new.rating - old.player.rating;
            delta > 5.0 && delta < 60.0
        }));
    }

    #[test]
    fn test_calibrated_players_independent_of_teammates() {
        let system = CamaRatingSystem::default();
        let with_high = vec![
            member(500.0, 350.0, "new"),
            member(1200.0, 80.0, "test"),
            member(1200.0, 75.0, "c2"),
            member(1200.0, 85.0, "c3"),
            member(1200.0, 90.0, "c4"),
        ];
        let all_calibrated = vec![
            member(1200.0, 80.0, "replacement"),
            member(1200.0, 80.0, "test"),
            member(1200.0, 75.0, "c2"),
            member(1200.0, 85.0, "c3"),
            member(1200.0, 90.0, "c4"),
        ];
        let opponents = repeated_team(5, 1200.0, 80.0, "enemy");
        let first = system
            .update_ratings_after_match(&with_high, &opponents, 1)
            .unwrap()
            .team1[1]
            .rating
            - 1200.0;
        let second = system
            .update_ratings_after_match(&all_calibrated, &opponents, 1)
            .unwrap()
            .team1[1]
            .rating
            - 1200.0;
        let difference = (first - second).abs();
        assert!(difference < 20.0 || difference < first.abs().max(second.abs()) * 0.5);
    }

    #[test]
    fn test_higher_rd_gets_larger_delta() {
        let system = CamaRatingSystem::default();
        let (team1, team2) = high_rd_on_strong_team();
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert!(
            (updates.team1[0].rating - team1[0].player.rating).abs()
                > (updates.team1[1].rating - team1[1].player.rating).abs()
        );
    }

    #[test]
    fn test_cap_applied_for_extreme_underdog_win() {
        let system = CamaRatingSystem::default();
        let team1 = (0..5)
            .map(|i| member(200.0 + f64::from(i) * 100.0, 350.0, i))
            .collect::<Vec<_>>();
        let team2 = repeated_team(5, 2500.0, 50.0, 10);
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert!(
            updates
                .team1
                .iter()
                .zip(&team1)
                .all(|(new, old)| new.rating - old.player.rating <= MAX_RATING_SWING_PER_GAME)
        );
    }

    #[test]
    fn test_cap_applied_for_extreme_favorite_loss() {
        let system = CamaRatingSystem::default();
        let team1 = repeated_team(5, 2500.0, 80.0, 1);
        let team2 = repeated_team(5, 500.0, 350.0, 2);
        let updates = system
            .update_ratings_after_match(&team1, &team2, 2)
            .unwrap();
        assert!(
            updates
                .team1
                .iter()
                .all(|p| p.rating - 2500.0 >= -MAX_RATING_SWING_PER_GAME)
        );
    }

    #[test]
    fn test_rd_never_increases_after_match() {
        let system = CamaRatingSystem::default();
        let (team1, team2) = high_rd_on_strong_team();
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert!(
            updates
                .team1
                .iter()
                .zip(&team1)
                .all(|(new, old)| new.rd <= old.player.rd)
        );
        assert!(
            updates
                .team2
                .iter()
                .zip(&team2)
                .all(|(new, old)| new.rd <= old.player.rd)
        );
    }

    #[test]
    fn test_rating_never_negative() {
        let system = CamaRatingSystem::default();
        let team1 = vec![
            member(50.0, 350.0, 1),
            member(100.0, 300.0, 2),
            member(150.0, 250.0, 3),
            member(200.0, 200.0, 4),
            member(250.0, 150.0, 5),
        ];
        let team2 = repeated_team(5, 2000.0, 80.0, 10);
        let updates = system
            .update_ratings_after_match(&team1, &team2, 2)
            .unwrap();
        assert!(updates.team1.iter().all(|p| p.rating >= 0.0));
    }

    #[test]
    fn test_identical_rd_similar_distribution() {
        let system = CamaRatingSystem::default();
        let team1 = repeated_team(5, 1200.0, 100.0, 1);
        let team2 = repeated_team(5, 1200.0, 100.0, 2);
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert!(
            updates
                .team1
                .iter()
                .all(|p| (p.rating - updates.team1[0].rating).abs() < 0.01)
        );
    }

    #[test]
    fn test_new_player_rd_contraction_is_limited_per_match() {
        let system = CamaRatingSystem::default();
        let initial_rd = system.initial_rd;
        let team1 = vec![
            member(1000.0, initial_rd, "new"),
            member(1200.0, 80.0, "c1"),
            member(1200.0, 80.0, "c2"),
            member(1200.0, 80.0, "c3"),
            member(1200.0, 80.0, "c4"),
        ];
        let team2 = repeated_team(5, 1200.0, 80.0, "enemy");
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert_close(updates.team1[0].rd, initial_rd * 0.935, 1e-10);
    }

    #[test]
    fn test_new_player_reaches_calibration_within_twenty_matches() {
        let system = CamaRatingSystem::default();
        let mut player = system.create_player_from_mmr(Some(6_500));
        let mut games = 0;
        while player.rd > CALIBRATION_RD_THRESHOLD && games < 20 {
            let updated =
                system.update_player_rating(player, 1500.0, 1500.0, 100.0, f64::from(games % 2));
            player =
                system.create_player_from_rating(updated.rating, updated.rd, updated.volatility);
            games += 1;
        }
        assert!(player.rd <= CALIBRATION_RD_THRESHOLD);
        assert!((10..=20).contains(&games));
    }

    #[test]
    fn test_rd_sixty_player_moves_about_ten_in_even_match() {
        let system = CamaRatingSystem::default();
        let team1 = repeated_team(5, 1500.0, 60.0, 1);
        let team2 = repeated_team(5, 1500.0, 60.0, 2);
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert_close(updates.team1[0].rating - 1500.0, 10.0, 0.5);
        assert_close(updates.team2[0].rating - 1500.0, -10.0, 0.5);
    }

    #[test]
    fn test_low_rated_player_on_losing_favorite_loses_appropriately() {
        let system = CamaRatingSystem::default();
        let team1 = vec![
            member(500.0, 300.0, "low"),
            member(1600.0, 80.0, "h1"),
            member(1500.0, 80.0, "h2"),
            member(1500.0, 80.0, "h3"),
            member(1500.0, 80.0, "h4"),
        ];
        let team2 = repeated_team(5, 1000.0, 80.0, "underdog");
        let updates = system
            .update_ratings_after_match(&team1, &team2, 2)
            .unwrap();
        assert!(updates.team1[0].rating - 500.0 < -50.0);
    }

    #[test]
    fn test_high_rated_player_on_winning_underdog_gains_appropriately() {
        let system = CamaRatingSystem::default();
        let team1 = vec![
            member(1800.0, 150.0, "high"),
            member(800.0, 80.0, "l1"),
            member(800.0, 80.0, "l2"),
            member(800.0, 80.0, "l3"),
            member(800.0, 80.0, "l4"),
        ];
        let team2 = repeated_team(5, 1600.0, 80.0, "favorite");
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert!(updates.team1[0].rating - 1800.0 > 50.0);
    }

    #[test]
    fn test_same_rd_players_get_same_delta_regardless_of_personal_rating() {
        let system = CamaRatingSystem::default();
        let team1 = vec![
            member(500.0, 150.0, "low"),
            member(1500.0, 150.0, "high"),
            member(1000.0, 80.0, "f1"),
            member(1000.0, 80.0, "f2"),
            member(1000.0, 80.0, "f3"),
        ];
        let team2 = repeated_team(5, 1000.0, 80.0, "enemy");
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        let low_delta = updates.team1[0].rating - 500.0;
        let high_delta = updates.team1[1].rating - 1500.0;
        assert!((low_delta - high_delta).abs() < 1.0);
    }

    #[test]
    fn test_empty_team1_raises_error() {
        let system = CamaRatingSystem::default();
        let team2 = repeated_team(5, 1200.0, 80.0, 2);
        assert_eq!(
            system
                .update_ratings_after_match(&[], &team2, 1)
                .unwrap_err()
                .to_string(),
            "team1_players cannot be empty"
        );
    }

    #[test]
    fn test_empty_team2_raises_error() {
        let system = CamaRatingSystem::default();
        let team1 = repeated_team(5, 1200.0, 80.0, 1);
        assert_eq!(
            system
                .update_ratings_after_match(&team1, &[], 1)
                .unwrap_err()
                .to_string(),
            "team2_players cannot be empty"
        );
    }

    #[test]
    fn test_invalid_winning_team_raises_error() {
        let system = CamaRatingSystem::default();
        let team1 = repeated_team(5, 1200.0, 80.0, 1);
        let team2 = repeated_team(5, 1200.0, 80.0, 2);
        assert!(
            system
                .update_ratings_after_match(&team1, &team2, 0)
                .unwrap_err()
                .to_string()
                .contains("winning_team must be 1 or 2")
        );
        assert!(
            system
                .update_ratings_after_match(&team1, &team2, 3)
                .unwrap_err()
                .to_string()
                .contains("winning_team must be 1 or 2")
        );
    }

    #[test]
    fn test_v3_caps_extreme_wins() {
        let system = CamaRatingSystem::default();
        let (team1, team2) = high_rd_on_strong_team();
        let win = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap()
            .team1[0]
            .rating
            - 413.0;
        let loss = system
            .update_ratings_after_match(&team1, &team2, 2)
            .unwrap()
            .team1[0]
            .rating
            - 413.0;
        assert!(win > 100.0 && win <= MAX_RATING_SWING_PER_GAME);
        assert!((-MAX_RATING_SWING_PER_GAME..0.0).contains(&loss));
    }

    #[test]
    fn test_v3_no_teammate_rd_inflation() {
        let system = CamaRatingSystem::default();
        let team1 = vec![
            member(413.0, 325.0, "michael"),
            member(1400.0, 80.0, "c1"),
            member(1350.0, 75.0, "c2"),
            member(1300.0, 85.0, "c3"),
            member(1250.0, 90.0, "c4"),
        ];
        let team2 = repeated_team(5, 1256.0, 82.0, "enemy");
        let updates = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        assert!(updates.team1[0].rating - 413.0 <= MAX_RATING_SWING_PER_GAME);
        assert!(
            updates.team1[1..]
                .iter()
                .zip(&team1[1..])
                .all(|(new, old)| {
                    let delta = new.rating - old.player.rating;
                    delta > 5.0 && delta < 60.0
                })
        );
    }

    #[test]
    fn test_glickman_paper_worked_example() {
        let mut player = Player::new(1500.0, 200.0, 0.06);
        player
            .update(&[
                Opponent::new(1400.0, 30.0, 1.0),
                Opponent::new(1550.0, 100.0, 0.0),
                Opponent::new(1700.0, 300.0, 0.0),
            ])
            .unwrap();
        assert_close(player.rating, 1464.06, 0.5);
        assert_close(player.rd, 151.52, 0.5);
        assert_close(player.volatility, 0.05999, 0.0001);
    }

    fn streak(outcomes: &[bool], won: bool) -> StreakAdjustment {
        CamaRatingSystem::default().calculate_streak_multiplier(outcomes, won, None, None)
    }

    #[test]
    fn test_no_streak_returns_multiplier_1() {
        assert_eq!(streak(&[], true), StreakAdjustment::default());
    }

    #[test]
    fn test_two_game_streak_returns_multiplier_1() {
        assert_eq!(
            streak(&[true], true),
            StreakAdjustment {
                streak_length: 2,
                multiplier: 1.0,
            }
        );
    }

    #[test]
    fn test_three_game_streak_returns_multiplier_1_30() {
        let adjustment = streak(&[true, true], true);
        assert_eq!(adjustment.streak_length, 3);
        assert_close(adjustment.multiplier, 1.30, 1e-12);
    }

    #[test]
    fn test_four_game_streak_returns_multiplier_1_60() {
        let adjustment = streak(&[true, true, true], true);
        assert_eq!(adjustment.streak_length, 4);
        assert_close(adjustment.multiplier, 1.60, 1e-12);
    }

    #[test]
    fn test_five_game_streak_returns_multiplier_1_90() {
        let adjustment = streak(&[true, true, true, true], true);
        assert_eq!(adjustment.streak_length, 5);
        assert_close(adjustment.multiplier, 1.90, 1e-12);
    }

    #[test]
    fn test_ten_game_streak_returns_multiplier_3_40() {
        let adjustment = streak(&[true; 9], true);
        assert_eq!(adjustment.streak_length, 10);
        assert_close(adjustment.multiplier, 3.40, 1e-12);
    }

    #[test]
    fn test_loss_streak_works_same_as_win_streak() {
        let adjustment = streak(&[false, false, false], false);
        assert_eq!(adjustment.streak_length, 4);
        assert_close(adjustment.multiplier, 1.60, 1e-12);
    }

    #[test]
    fn test_streak_broken_returns_multiplier_1() {
        assert_eq!(
            streak(&[true, true, true, true], false),
            StreakAdjustment::default()
        );
    }

    #[test]
    fn test_win_breaks_loss_streak_returns_multiplier_1() {
        assert_eq!(
            streak(&[false, false, false], true),
            StreakAdjustment::default()
        );
    }

    #[test]
    fn test_mixed_history_finds_current_streak() {
        assert_eq!(
            streak(&[false, true, true, true, false, false], false),
            StreakAdjustment {
                streak_length: 2,
                multiplier: 1.0,
            }
        );
    }

    #[test]
    fn test_continuing_streak_from_history() {
        let adjustment = streak(&[true, true, true, false, false], true);
        assert_eq!(adjustment.streak_length, 4);
        assert_close(adjustment.multiplier, 1.60, 1e-12);
    }

    #[test]
    fn test_empty_history_returns_streak_of_1() {
        assert_eq!(streak(&[], true), StreakAdjustment::default());
    }

    #[test]
    fn test_config_constants_have_expected_values() {
        assert_eq!(STREAK_THRESHOLD, 3);
        assert_close(STREAK_MULTIPLIER_PER_GAME, 0.30, 1e-12);
    }

    #[test]
    fn test_threshold_override_suppresses_boost_below_recorded_gate() {
        let adjustment = CamaRatingSystem::default().calculate_streak_multiplier(
            &[true, true],
            true,
            None,
            Some(4),
        );
        assert_eq!(adjustment.streak_length, 3);
        assert_eq!(adjustment.multiplier, 1.0);
    }

    #[test]
    fn test_threshold_override_matching_default_keeps_boost() {
        let adjustment = CamaRatingSystem::default().calculate_streak_multiplier(
            &[true, true],
            true,
            None,
            Some(3),
        );
        assert_eq!(adjustment.streak_length, 3);
        assert_close(adjustment.multiplier, 1.30, 1e-12);
    }

    #[test]
    fn test_recorded_streak_helpers_fall_back_to_legacy_values() {
        assert_close(recorded_streak_rate(RecordedValue::Null), 0.20, 1e-12);
        assert_close(
            recorded_streak_rate(RecordedValue::Text("not-a-number")),
            0.20,
            1e-12,
        );
        assert_close(recorded_streak_rate(RecordedValue::Real(0.30)), 0.30, 1e-12);
        assert_eq!(recorded_streak_threshold(RecordedValue::Null), 3);
        assert_eq!(
            recorded_streak_threshold(RecordedValue::Text("not-a-number")),
            3
        );
        assert_eq!(recorded_streak_threshold(RecordedValue::Integer(4)), 4);
    }

    #[test]
    fn test_update_player_rating_applies_streak_multiplier() {
        let system = CamaRatingSystem::default();
        let player = Player::new(1500.0, 100.0, 0.06);
        let base = system.update_player_rating(player, 1500.0, 1500.0, 100.0, 1.0);
        let boosted = system.update_player_rating_with_modifiers(
            player,
            1500.0,
            1500.0,
            100.0,
            1.0,
            PlayerUpdateModifiers {
                streak_multiplier: 1.30,
                ..PlayerUpdateModifiers::default()
            },
        );
        assert_close(
            boosted.rating - player.rating,
            (base.rating - player.rating) * 1.30,
            (base.rating - player.rating).abs() * 0.01,
        );
    }

    #[test]
    fn test_streak_multiplier_1_has_no_effect() {
        let system = CamaRatingSystem::default();
        let player = Player::new(1500.0, 100.0, 0.06);
        let default = system.update_player_rating(player, 1500.0, 1500.0, 100.0, 1.0);
        let explicit = system.update_player_rating_with_modifiers(
            player,
            1500.0,
            1500.0,
            100.0,
            1.0,
            PlayerUpdateModifiers {
                streak_multiplier: 1.0,
                ..PlayerUpdateModifiers::default()
            },
        );
        assert_close(default.rating, explicit.rating, 1e-12);
    }

    #[test]
    fn test_win_streak_amplifies_rating_gain() {
        let system = CamaRatingSystem::default();
        let team1 = vec![member(1500.0, 100.0, 1)];
        let team2 = vec![member(1500.0, 100.0, 2)];
        let base = system
            .update_ratings_after_match(&team1, &team2, 1)
            .unwrap();
        let options = MatchUpdateOptions {
            streak_multipliers: HashMap::from([(1, 1.90)]),
            ..MatchUpdateOptions::default()
        };
        let boosted = system
            .update_ratings_after_match_with_options(&team1, &team2, 1, &options)
            .unwrap();
        assert_close(
            boosted.team1[0].rating - 1500.0,
            (base.team1[0].rating - 1500.0) * 1.90,
            (base.team1[0].rating - 1500.0).abs() * 0.01,
        );
    }

    #[test]
    fn test_loss_streak_amplifies_rating_loss() {
        let system = CamaRatingSystem::default();
        let team1 = vec![member(1500.0, 100.0, 1)];
        let team2 = vec![member(1500.0, 100.0, 2)];
        let base = system
            .update_ratings_after_match(&team1, &team2, 2)
            .unwrap();
        let options = MatchUpdateOptions {
            streak_multipliers: HashMap::from([(1, 1.60)]),
            ..MatchUpdateOptions::default()
        };
        let boosted = system
            .update_ratings_after_match_with_options(&team1, &team2, 2, &options)
            .unwrap();
        assert_close(
            boosted.team1[0].rating - 1500.0,
            (base.team1[0].rating - 1500.0) * 1.60,
            (base.team1[0].rating - 1500.0).abs() * 0.01,
        );
    }

    #[test]
    fn test_gain_multiplier_stacks_with_streak_without_amplifying_loss() {
        let system = CamaRatingSystem::default();
        let team1 = vec![member(1500.0, 100.0, 1)];
        let team2 = vec![member(1500.0, 100.0, 2)];
        let base_options = MatchUpdateOptions {
            streak_multipliers: HashMap::from([(1, 1.25), (2, 1.25)]),
            ..MatchUpdateOptions::default()
        };
        let base = system
            .update_ratings_after_match_with_options(&team1, &team2, 1, &base_options)
            .unwrap();
        let boosted_options = MatchUpdateOptions {
            streak_multipliers: HashMap::from([(1, 1.25), (2, 1.25)]),
            gain_multipliers: HashMap::from([(1, 1.10), (2, 1.10)]),
            base_rating_delta_multiplier: None,
        };
        let boosted = system
            .update_ratings_after_match_with_options(&team1, &team2, 1, &boosted_options)
            .unwrap();
        assert_close(
            boosted.team1[0].rating - 1500.0,
            (base.team1[0].rating - 1500.0) * 1.10,
            1e-10,
        );
        assert_close(boosted.team2[0].rating, base.team2[0].rating, 1e-12);
        assert_close(boosted.team1[0].rd, base.team1[0].rd, 1e-12);
        assert_close(boosted.team1[0].volatility, base.team1[0].volatility, 1e-12);
        assert_eq!(boosted.team1[0].id, base.team1[0].id);
        assert_close(boosted.team2[0].rd, base.team2[0].rd, 1e-12);
        assert_close(boosted.team2[0].volatility, base.team2[0].volatility, 1e-12);
        assert_eq!(boosted.team2[0].id, base.team2[0].id);
    }

    #[test]
    fn test_gain_multiplier_is_applied_before_the_rating_cap() {
        let system = CamaRatingSystem::default();
        let player = Player::new(1000.0, 350.0, 0.06);
        let updated = system.update_player_rating_with_modifiers(
            player,
            1000.0,
            3000.0,
            30.0,
            1.0,
            PlayerUpdateModifiers {
                streak_multiplier: 3.0,
                base_rating_delta_multiplier: None,
                gain_multiplier: 1.10,
            },
        );
        assert_close(
            updated.rating - player.rating,
            MAX_RATING_SWING_PER_GAME,
            1e-12,
        );
    }
}
