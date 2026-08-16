//! OpenSkill Plackett-Luce ratings used by CAMA matchmaking.
//!
//! This module is a dependency-free port of the application's configured
//! `openskill` 6.2.0 path.  In particular, contribution weights are passed to
//! the native Weng-Lin posterior without min/max normalization, sigma is
//! inflated by `tau` before every update, and streak/positive-gain overlays
//! only alter the resulting `mu` delta.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;

/// A player's Gaussian OpenSkill belief.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rating {
    pub mu: f64,
    pub sigma: f64,
}

impl Rating {
    #[must_use]
    pub const fn new(mu: f64, sigma: f64) -> Self {
        Self { mu, sigma }
    }

    #[must_use]
    pub fn ordinal(self, z: f64) -> f64 {
        self.mu - z * self.sigma
    }
}

/// Input for an update that may use fantasy performance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeightedPlayer {
    pub id: u64,
    pub mu: Option<f64>,
    pub sigma: Option<f64>,
    pub fantasy_points: Option<f64>,
}

impl WeightedPlayer {
    #[must_use]
    pub const fn new(
        id: u64,
        mu: Option<f64>,
        sigma: Option<f64>,
        fantasy_points: Option<f64>,
    ) -> Self {
        Self {
            id,
            mu,
            sigma,
            fantasy_points,
        }
    }
}

/// Input for an equal-contribution update.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Player {
    pub id: u64,
    pub mu: Option<f64>,
    pub sigma: Option<f64>,
}

impl Player {
    #[must_use]
    pub const fn new(id: u64, mu: Option<f64>, sigma: Option<f64>) -> Self {
        Self { id, mu, sigma }
    }
}

/// The native posterior plus the contribution weight persisted by the app.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeightedRatingUpdate {
    pub rating: Rating,
    pub contribution_weight: Option<f64>,
}

/// The winner selected by match orchestration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WinningTeam {
    Team1,
    Team2,
}

impl TryFrom<u8> for WinningTeam {
    type Error = OpenSkillError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Team1),
            2 => Ok(Self::Team2),
            value => Err(OpenSkillError::InvalidWinningTeam(value)),
        }
    }
}

/// Settings that can alter stored OpenSkill results.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenSkillConfig {
    pub calibration_threshold: f64,
    pub performance_strength: f64,
    pub streak_threshold: i64,
    pub streak_multiplier_per_game: f64,
    pub low_priority_gain_multiplier: f64,
    pub sigma_decay_grace_period_days: i64,
    pub sigma_decay_per_week: f64,
    pub win_probability_temperature: f64,
    pub display_origin: f64,
    pub display_scale: f64,
}

impl Default for OpenSkillConfig {
    fn default() -> Self {
        Self {
            calibration_threshold: 4.0,
            performance_strength: 0.10,
            streak_threshold: 3,
            streak_multiplier_per_game: 0.30,
            low_priority_gain_multiplier: 1.10,
            sigma_decay_grace_period_days: 7,
            sigma_decay_per_week: 2.0,
            win_probability_temperature: 6.5,
            display_origin: 25.0,
            display_scale: 50.0,
        }
    }
}

/// Invalid inputs rejected before applying a rating update.
#[derive(Clone, Debug, PartialEq)]
pub enum OpenSkillError {
    InvalidPerformanceStrength(f64),
    NegativeSigmaDecay(f64),
    InvalidProbabilityTemperature(f64),
    InvalidWinningTeam(u8),
    EmptyTeam,
    DuplicatePlayerId(u64),
    WeightCountMismatch,
    InvalidWeight(f64),
}

impl fmt::Display for OpenSkillError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPerformanceStrength(value) => write!(
                formatter,
                "OpenSkill performance strength must be between 0 and 1, got {value}"
            ),
            Self::NegativeSigmaDecay(value) => {
                write!(
                    formatter,
                    "OpenSkill sigma decay cannot be negative, got {value}"
                )
            }
            Self::InvalidProbabilityTemperature(value) => write!(
                formatter,
                "OpenSkill probability temperature must be positive, got {value}"
            ),
            Self::InvalidWinningTeam(value) => {
                write!(formatter, "winning team must be 1 or 2, got {value}")
            }
            Self::EmptyTeam => formatter.write_str("both teams must contain at least one player"),
            Self::DuplicatePlayerId(id) => write!(formatter, "duplicate player id {id}"),
            Self::WeightCountMismatch => {
                formatter.write_str("weights must have one value for every player")
            }
            Self::InvalidWeight(value) => {
                write!(
                    formatter,
                    "contribution weights must be finite and positive, got {value}"
                )
            }
        }
    }
}

impl std::error::Error for OpenSkillError {}

/// The configured CAMA OpenSkill system.
#[derive(Clone, Debug, PartialEq)]
pub struct CamaOpenSkillSystem {
    config: OpenSkillConfig,
}

impl Default for CamaOpenSkillSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl CamaOpenSkillSystem {
    pub const DEFAULT_MU: f64 = 25.0;
    pub const DEFAULT_SIGMA: f64 = 25.0 / 3.0;
    pub const BETA: f64 = 25.0 / 6.0;
    pub const KAPPA: f64 = 0.0001;
    pub const TAU: f64 = 25.0 / 300.0;
    pub const FANTASY_MIN: f64 = 5.0;
    pub const FANTASY_MAX: f64 = 30.0;
    pub const WIN_PROBABILITY_EPSILON: f64 = 1e-12;
    pub const OPENSKILL_PACKAGE_VERSION: &'static str = "6.2.0";

    /// Construct the production-default model.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: OpenSkillConfig::default(),
        }
    }

    /// Construct a model from deployment-provided configuration.
    pub fn with_config(config: OpenSkillConfig) -> Result<Self, OpenSkillError> {
        if !config.performance_strength.is_finite()
            || !(0.0..=1.0).contains(&config.performance_strength)
        {
            return Err(OpenSkillError::InvalidPerformanceStrength(
                config.performance_strength,
            ));
        }
        if !config.sigma_decay_per_week.is_finite() || config.sigma_decay_per_week < 0.0 {
            return Err(OpenSkillError::NegativeSigmaDecay(
                config.sigma_decay_per_week,
            ));
        }
        Ok(Self { config })
    }

    #[must_use]
    pub const fn config(&self) -> &OpenSkillConfig {
        &self.config
    }

    /// OpenSkill's default `(1, 2)` weight expansion is intentionally disabled.
    #[must_use]
    pub const fn weight_bounds(&self) -> Option<(f64, f64)> {
        None
    }

    #[must_use]
    pub fn normalize_fantasy_score(&self, fantasy_points: f64) -> f64 {
        let clamped = fantasy_points.clamp(Self::FANTASY_MIN, Self::FANTASY_MAX);
        (clamped - Self::FANTASY_MIN) / (Self::FANTASY_MAX - Self::FANTASY_MIN)
    }

    /// Native team-centered contribution factors, deliberately close to 1.0.
    #[must_use]
    pub fn performance_factors(&self, fantasy_points: &[f64]) -> Vec<f64> {
        if fantasy_points.is_empty() {
            return Vec::new();
        }
        let normalized: Vec<_> = fantasy_points
            .iter()
            .map(|points| self.normalize_fantasy_score(*points))
            .collect();
        let mean = normalized.iter().sum::<f64>() / normalized.len() as f64;
        normalized
            .iter()
            .map(|score| 1.0 + self.config.performance_strength * (score - mean))
            .collect()
    }

    #[must_use]
    pub fn apply_sigma_decay(&self, sigma: f64, days_since_last_match: i64) -> f64 {
        if sigma >= Self::DEFAULT_SIGMA {
            return Self::DEFAULT_SIGMA;
        }
        if days_since_last_match <= self.config.sigma_decay_grace_period_days {
            return sigma;
        }
        let periods =
            (days_since_last_match - self.config.sigma_decay_grace_period_days) as f64 / 7.0;
        let decayed = (sigma * sigma + self.config.sigma_decay_per_week.powi(2) * periods).sqrt();
        decayed.min(Self::DEFAULT_SIGMA)
    }

    #[must_use]
    pub fn mmr_to_os_mu(&self, mmr: i32) -> f64 {
        Self::DEFAULT_MU + f64::from(mmr) / 200.0
    }

    #[must_use]
    pub fn create_rating(&self, mu: Option<f64>, sigma: Option<f64>) -> Rating {
        Rating::new(
            mu.unwrap_or(Self::DEFAULT_MU),
            sigma.unwrap_or(Self::DEFAULT_SIGMA),
        )
    }

    #[must_use]
    pub fn ordinal(&self, mu: f64, sigma: f64, z: f64) -> f64 {
        mu - z * sigma
    }

    #[must_use]
    pub fn is_calibrated(&self, sigma: f64) -> bool {
        sigma <= self.config.calibration_threshold
    }

    #[must_use]
    pub fn uncertainty_percentage(&self, sigma: f64) -> f64 {
        ((sigma / Self::DEFAULT_SIGMA) * 100.0).min(100.0)
    }

    #[must_use]
    pub fn certainty_percentage(&self, sigma: f64) -> f64 {
        100.0 - self.uncertainty_percentage(sigma)
    }

    #[must_use]
    pub fn mu_to_display(&self, mu: f64) -> i32 {
        let display = ((mu - self.config.display_origin) * self.config.display_scale).max(0.0);
        display.round_ties_even() as i32
    }

    /// Update both teams through the weighted native posterior.
    pub fn update_ratings_after_match(
        &self,
        team1: &[WeightedPlayer],
        team2: &[WeightedPlayer],
        winning_team: WinningTeam,
        streak_multipliers: &BTreeMap<u64, f64>,
        gain_multipliers: &BTreeMap<u64, f64>,
    ) -> Result<BTreeMap<u64, WeightedRatingUpdate>, OpenSkillError> {
        validate_player_ids(team1, team2)?;

        let team1_ratings: Vec<_> = team1
            .iter()
            .map(|player| self.create_rating(player.mu, player.sigma))
            .collect();
        let team2_ratings: Vec<_> = team2
            .iter()
            .map(|player| self.create_rating(player.mu, player.sigma))
            .collect();

        let complete_fantasy = team1
            .iter()
            .chain(team2)
            .all(|player| player.fantasy_points.is_some());
        let factors = complete_fantasy.then(|| {
            (
                self.performance_factors(
                    &team1
                        .iter()
                        .filter_map(|player| player.fantasy_points)
                        .collect::<Vec<_>>(),
                ),
                self.performance_factors(
                    &team2
                        .iter()
                        .filter_map(|player| player.fantasy_points)
                        .collect::<Vec<_>>(),
                ),
            )
        });

        let factor_slices = factors
            .as_ref()
            .map(|(team1, team2)| (team1.as_slice(), team2.as_slice()));
        let (updated1, updated2) =
            self.rate_native(&team1_ratings, &team2_ratings, winning_team, factor_slices)?;

        let mut results = BTreeMap::new();
        for (index, (player, native)) in team1.iter().zip(updated1).enumerate() {
            let old_mu = team1_ratings[index].mu;
            let mu = apply_overlays(
                old_mu,
                native.mu,
                streak_multipliers.get(&player.id).copied().unwrap_or(1.0),
                gain_multipliers.get(&player.id).copied().unwrap_or(1.0),
            );
            results.insert(
                player.id,
                WeightedRatingUpdate {
                    rating: Rating::new(mu, native.sigma),
                    contribution_weight: factors.as_ref().map(|value| value.0[index]),
                },
            );
        }
        for (index, (player, native)) in team2.iter().zip(updated2).enumerate() {
            let old_mu = team2_ratings[index].mu;
            let mu = apply_overlays(
                old_mu,
                native.mu,
                streak_multipliers.get(&player.id).copied().unwrap_or(1.0),
                gain_multipliers.get(&player.id).copied().unwrap_or(1.0),
            );
            results.insert(
                player.id,
                WeightedRatingUpdate {
                    rating: Rating::new(mu, native.sigma),
                    contribution_weight: factors.as_ref().map(|value| value.1[index]),
                },
            );
        }
        Ok(results)
    }

    /// Equal-contribution update used before fantasy enrichment completes.
    pub fn update_ratings_equal_weight(
        &self,
        team1: &[Player],
        team2: &[Player],
        winning_team: WinningTeam,
        streak_multipliers: &BTreeMap<u64, f64>,
        gain_multipliers: &BTreeMap<u64, f64>,
    ) -> Result<BTreeMap<u64, Rating>, OpenSkillError> {
        let team1_weighted: Vec<_> = team1
            .iter()
            .map(|player| WeightedPlayer::new(player.id, player.mu, player.sigma, None))
            .collect();
        let team2_weighted: Vec<_> = team2
            .iter()
            .map(|player| WeightedPlayer::new(player.id, player.mu, player.sigma, None))
            .collect();
        self.update_ratings_after_match(
            &team1_weighted,
            &team2_weighted,
            winning_team,
            streak_multipliers,
            gain_multipliers,
        )
        .map(|updates| {
            updates
                .into_iter()
                .map(|(id, update)| (id, update.rating))
                .collect()
        })
    }

    /// Apply the native `openskill` 6.2.0 two-team Plackett-Luce posterior.
    pub fn rate_native(
        &self,
        team1: &[Rating],
        team2: &[Rating],
        winning_team: WinningTeam,
        weights: Option<(&[f64], &[f64])>,
    ) -> Result<(Vec<Rating>, Vec<Rating>), OpenSkillError> {
        if team1.is_empty() || team2.is_empty() {
            return Err(OpenSkillError::EmptyTeam);
        }
        if let Some((weights1, weights2)) = weights {
            if weights1.len() != team1.len() || weights2.len() != team2.len() {
                return Err(OpenSkillError::WeightCountMismatch);
            }
            if let Some(weight) = weights1
                .iter()
                .chain(weights2)
                .find(|weight| !weight.is_finite() || **weight <= 0.0)
            {
                return Err(OpenSkillError::InvalidWeight(*weight));
            }
        }

        // Python sorts explicit ranks before `_compute`, then unwinds the
        // result.  Keeping winner first preserves the same floating-point
        // accumulation order for either winning-team value.
        let (winner, loser, winner_weights, loser_weights, winner_is_team1) = match winning_team {
            WinningTeam::Team1 => (
                team1,
                team2,
                weights.map(|value| value.0),
                weights.map(|value| value.1),
                true,
            ),
            WinningTeam::Team2 => (
                team2,
                team1,
                weights.map(|value| value.1),
                weights.map(|value| value.0),
                false,
            ),
        };

        let winner: Vec<_> = winner.iter().map(inflate_sigma).collect();
        let loser: Vec<_> = loser.iter().map(inflate_sigma).collect();
        let winner_stats = team_stats(&winner);
        let loser_stats = team_stats(&loser);
        let c = (winner_stats.sigma_squared
            + Self::BETA.powi(2)
            + loser_stats.sigma_squared
            + Self::BETA.powi(2))
        .sqrt();

        let winner_exp = (winner_stats.mu / c).exp();
        let loser_exp = (loser_stats.mu / c).exp();
        let denominator = winner_exp + loser_exp;
        let winner_share = winner_exp / denominator;
        let loser_share = loser_exp / denominator;

        let winner_omega = (1.0 - winner_share) * winner_stats.sigma_squared / c;
        let loser_omega = -loser_share * loser_stats.sigma_squared / c;
        let common_curvature = winner_share * (1.0 - winner_share);
        let winner_delta = common_curvature * winner_stats.sigma_squared / c.powi(2)
            * winner_stats.sigma_squared.sqrt()
            / c;
        let loser_delta = common_curvature * loser_stats.sigma_squared / c.powi(2)
            * loser_stats.sigma_squared.sqrt()
            / c;

        let updated_winner = update_team(
            &winner,
            winner_stats.sigma_squared,
            winner_omega,
            winner_delta,
            winner_weights,
        );
        let updated_loser = update_team(
            &loser,
            loser_stats.sigma_squared,
            loser_omega,
            loser_delta,
            loser_weights,
        );

        if winner_is_team1 {
            Ok((updated_winner, updated_loser))
        } else {
            Ok((updated_loser, updated_winner))
        }
    }

    /// Raw Bradley-Terry probability exposed by OpenSkill for two teams.
    #[must_use]
    pub fn os_predict_win_probability(&self, team1: &[Rating], team2: &[Rating]) -> f64 {
        if team1.is_empty() || team2.is_empty() {
            return 0.5;
        }
        let team1 = team_stats(team1);
        let team2 = team_stats(team2);
        let denominator =
            (2.0 * Self::BETA.powi(2) + team1.sigma_squared + team2.sigma_squared).sqrt();
        normal_cdf((team1.mu - team2.mu) / denominator)
    }

    pub fn calibrate_win_probability(
        &self,
        probability: f64,
        temperature: Option<f64>,
    ) -> Result<f64, OpenSkillError> {
        let temperature = temperature.unwrap_or(self.config.win_probability_temperature);
        if temperature <= 0.0 || !temperature.is_finite() {
            return Err(OpenSkillError::InvalidProbabilityTemperature(temperature));
        }
        let probability = probability.clamp(
            Self::WIN_PROBABILITY_EPSILON,
            1.0 - Self::WIN_PROBABILITY_EPSILON,
        );
        let scaled_log_odds = (probability / (1.0 - probability)).ln() / temperature;
        let calibrated = if scaled_log_odds >= 0.0 {
            1.0 / (1.0 + (-scaled_log_odds).exp())
        } else {
            let exp = scaled_log_odds.exp();
            exp / (1.0 + exp)
        };
        Ok(calibrated)
    }

    pub fn os_predict_calibrated_win_probability(
        &self,
        team1: &[Rating],
        team2: &[Rating],
    ) -> Result<f64, OpenSkillError> {
        self.calibrate_win_probability(self.os_predict_win_probability(team1, team2), None)
    }

    /// Canonical compact JSON for every setting that changes persisted results.
    #[must_use]
    pub fn algorithm_spec_json(&self) -> String {
        let mut json = String::new();
        write!(
            json,
            concat!(
                "{{\"balance\":false,",
                "\"beta\":{},",
                "\"display_origin\":{},",
                "\"display_scale\":{},",
                "\"family\":\"weng_lin.plackett_luce\",",
                "\"fantasy_max\":{},",
                "\"fantasy_min\":{},",
                "\"kappa\":{},",
                "\"limit_sigma\":false,",
                "\"low_priority_gain_multiplier\":{},",
                "\"mu\":{},",
                "\"openskill_package\":\"{}\",",
                "\"performance_strength\":{},",
                "\"positive_gain_policy\":\"posthoc_positive_mu_delta\",",
                "\"sigma\":{},",
                "\"sigma_decay_grace_days\":{},",
                "\"sigma_decay_per_week\":{},",
                "\"streak_multiplier_per_game\":{},",
                "\"streak_policy\":\"posthoc_mu_delta\",",
                "\"streak_threshold\":{},",
                "\"tau\":{},",
                "\"weight_bounds\":null,",
                "\"win_probability_temperature\":{}}}"
            ),
            python_json_number(Self::BETA),
            python_json_number(self.config.display_origin),
            python_json_number(self.config.display_scale),
            python_json_number(Self::FANTASY_MAX),
            python_json_number(Self::FANTASY_MIN),
            python_json_number(Self::KAPPA),
            python_json_number(self.config.low_priority_gain_multiplier),
            python_json_number(Self::DEFAULT_MU),
            Self::OPENSKILL_PACKAGE_VERSION,
            python_json_number(self.config.performance_strength),
            python_json_number(Self::DEFAULT_SIGMA),
            self.config.sigma_decay_grace_period_days,
            python_json_number(self.config.sigma_decay_per_week),
            python_json_number(self.config.streak_multiplier_per_game),
            self.config.streak_threshold,
            python_json_number(Self::TAU),
            python_json_number(self.config.win_probability_temperature),
        )
        .expect("writing to a String cannot fail");
        json
    }

    /// First 16 hexadecimal characters of SHA-256, matching Python exactly.
    #[must_use]
    pub fn algorithm_fingerprint(&self) -> String {
        sha256_hex(self.algorithm_spec_json().as_bytes())[..16].to_owned()
    }
}

#[derive(Clone, Copy)]
struct TeamStats {
    mu: f64,
    sigma_squared: f64,
}

fn team_stats(team: &[Rating]) -> TeamStats {
    // OpenSkill sorts players by conservative ordinal before summing.
    let mut sorted = team.to_vec();
    sorted.sort_by(|left, right| right.ordinal(3.0).total_cmp(&left.ordinal(3.0)));
    let mut mu = 0.0;
    let mut sigma_squared = 0.0;
    for player in sorted {
        mu += player.mu;
        sigma_squared += player.sigma.powi(2);
    }
    TeamStats { mu, sigma_squared }
}

fn inflate_sigma(rating: &Rating) -> Rating {
    Rating::new(
        rating.mu,
        (rating.sigma * rating.sigma + CamaOpenSkillSystem::TAU.powi(2)).sqrt(),
    )
}

fn update_team(
    team: &[Rating],
    team_sigma_squared: f64,
    omega: f64,
    delta: f64,
    weights: Option<&[f64]>,
) -> Vec<Rating> {
    team.iter()
        .enumerate()
        .map(|(index, player)| {
            let weight = weights.map_or(1.0, |values| values[index]);
            let variance_share = player.sigma.powi(2) / team_sigma_squared;
            let (mu_adjustment, sigma_adjustment) = if omega >= 0.0 {
                (omega * weight, delta * weight)
            } else {
                (omega / weight, delta / weight)
            };
            Rating::new(
                player.mu + variance_share * mu_adjustment,
                player.sigma
                    * (1.0 - variance_share * sigma_adjustment)
                        .max(CamaOpenSkillSystem::KAPPA)
                        .sqrt(),
            )
        })
        .collect()
}

fn apply_overlays(old_mu: f64, native_mu: f64, streak: f64, gain: f64) -> f64 {
    let streaked = if streak == 1.0 {
        native_mu
    } else {
        old_mu + (native_mu - old_mu) * streak
    };
    if gain == 1.0 || streaked <= old_mu {
        streaked
    } else {
        old_mu + (streaked - old_mu) * gain
    }
}

fn validate_player_ids(
    team1: &[WeightedPlayer],
    team2: &[WeightedPlayer],
) -> Result<(), OpenSkillError> {
    if team1.is_empty() || team2.is_empty() {
        return Err(OpenSkillError::EmptyTeam);
    }
    let mut ids = BTreeSet::new();
    for player in team1.iter().chain(team2) {
        if !ids.insert(player.id) {
            return Err(OpenSkillError::DuplicatePlayerId(player.id));
        }
    }
    Ok(())
}

// Numerical Recipes' erfc approximation.  Its maximum absolute error is
// about 1.2e-7; update posteriors do not use this approximation, only the
// user-facing Bradley-Terry prediction.
fn normal_cdf(value: f64) -> f64 {
    0.5 * erfc(-value / std::f64::consts::SQRT_2)
}

fn erfc(value: f64) -> f64 {
    let z = value.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let polynomial = t
        * (1.000_023_68
            + t * (0.374_091_96
                + t * (0.096_784_18
                    + t * (-0.186_288_06
                        + t * (0.278_868_07
                            + t * (-1.135_203_98
                                + t * (1.488_515_87 + t * (-0.822_152_23 + t * 0.170_872_77))))))));
    let answer = t * (-z * z - 1.265_512_23 + polynomial).exp();
    if value >= 0.0 { answer } else { 2.0 - answer }
}

fn python_json_number(value: f64) -> String {
    let mut result = value.to_string();
    if value.is_finite() && value.fract() == 0.0 && !result.contains('.') {
        result.push_str(".0");
    }
    result
}

fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];

    let bit_len = (input.len() as u64) * 8;
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = INITIAL;
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, bytes) in chunk.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let upper_e = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temporary1 = h
                .wrapping_add(upper_e)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let upper_a = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary2 = upper_a.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary1);
            d = c;
            c = b;
            b = a;
            a = temporary1.wrapping_add(temporary2);
        }
        for (current, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *current = current.wrapping_add(value);
        }
    }

    let mut output = String::with_capacity(64);
    for value in state {
        write!(output, "{value:08x}").expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_MULTIPLIERS: &BTreeMap<u64, f64> = &BTreeMap::new();

    fn weighted(id: u64, mu: f64, sigma: f64, fantasy: f64) -> WeightedPlayer {
        WeightedPlayer::new(id, Some(mu), Some(sigma), Some(fantasy))
    }

    fn equal(id: u64, mu: f64, sigma: f64) -> Player {
        Player::new(id, Some(mu), Some(sigma))
    }

    fn close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected:.15}, got {actual:.15}"
        );
    }

    #[test]
    fn test_weight_blending_reduces_fp_impact() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [weighted(1, 30.0, 8.0, 30.0), weighted(2, 30.0, 8.0, 5.0)];
        let team2 = [weighted(3, 30.0, 8.0, 15.0), weighted(4, 30.0, 8.0, 15.0)];
        let results = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        let high_gain = results[&1].rating.mu - 30.0;
        let low_gain = results[&2].rating.mu - 30.0;
        assert!(high_gain > 0.0 && low_gain > 0.0);
        assert!((1.0..1.25).contains(&(high_gain / low_gain)));
    }

    #[test]
    fn test_one_point_difference_does_not_expand_to_two_x() {
        let system = CamaOpenSkillSystem::new();
        let mut team1 = (1..=5)
            .map(|id| weighted(id, 45.0, 4.0, if id == 5 { 16.0 } else { 15.0 }))
            .collect::<Vec<_>>();
        let team2 = (6..=10)
            .map(|id| weighted(id, 45.0, 4.0, 15.0))
            .collect::<Vec<_>>();
        let weighted_result = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        let equal1 = team1
            .iter_mut()
            .map(|player| equal(player.id, player.mu.unwrap(), player.sigma.unwrap()))
            .collect::<Vec<_>>();
        let equal2 = team2
            .iter()
            .map(|player| equal(player.id, player.mu.unwrap(), player.sigma.unwrap()))
            .collect::<Vec<_>>();
        let equal_result = system
            .update_ratings_equal_weight(
                &equal1,
                &equal2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        let base = equal_result[&5].mu - 45.0;
        let weighted_delta = weighted_result[&5].rating.mu - 45.0;
        assert!(weighted_delta / base < 1.01);
    }

    #[test]
    fn test_performance_can_change_each_teams_total_movement() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [weighted(1, 45.0, 4.0, 30.0), weighted(2, 45.0, 8.0, 5.0)];
        let team2 = [weighted(3, 45.0, 4.0, 30.0), weighted(4, 45.0, 8.0, 5.0)];
        let weighted_result = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        let equal1 = [equal(1, 45.0, 4.0), equal(2, 45.0, 8.0)];
        let equal2 = [equal(3, 45.0, 4.0), equal(4, 45.0, 8.0)];
        let equal_result = system
            .update_ratings_equal_weight(
                &equal1,
                &equal2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        for ids in [[1, 2], [3, 4]] {
            let weighted_total = ids
                .iter()
                .map(|id| weighted_result[id].rating.mu - 45.0)
                .sum::<f64>();
            let equal_total = ids.iter().map(|id| equal_result[id].mu - 45.0).sum::<f64>();
            assert!((weighted_total - equal_total).abs() > 1e-12);
        }
    }

    #[test]
    fn test_model_does_not_expand_direct_weights() {
        assert!(CamaOpenSkillSystem::new().weight_bounds().is_none());
    }

    #[test]
    fn test_equal_fp_gives_equal_gains() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [weighted(1, 30.0, 8.0, 15.0), weighted(2, 30.0, 8.0, 15.0)];
        let team2 = [weighted(3, 30.0, 8.0, 15.0), weighted(4, 30.0, 8.0, 15.0)];
        let result = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        close(result[&1].rating.mu, result[&2].rating.mu, 0.01);
        close(result[&3].rating.mu, result[&4].rating.mu, 0.01);
    }

    #[test]
    fn test_loss_weight_inversion() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [weighted(1, 35.0, 8.0, 15.0), weighted(2, 35.0, 8.0, 15.0)];
        let team2 = [weighted(3, 35.0, 8.0, 30.0), weighted(4, 35.0, 8.0, 5.0)];
        let result = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        let high_loss = 35.0 - result[&3].rating.mu;
        let low_loss = 35.0 - result[&4].rating.mu;
        assert!(high_loss > 0.0 && low_loss > 0.0 && high_loss < low_loss);
    }

    #[test]
    fn test_native_weights_are_persistable_and_bounded() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [weighted(1, 35.0, 4.0, 30.0), weighted(2, 35.0, 4.0, 5.0)];
        let team2 = [weighted(3, 35.0, 4.0, 30.0), weighted(4, 35.0, 4.0, 5.0)];
        let result = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        for id in [1, 3] {
            assert!((1.0..1.15).contains(&result[&id].contribution_weight.unwrap()));
        }
        for id in [2, 4] {
            assert!((0.85..1.0).contains(&result[&id].contribution_weight.unwrap()));
        }
    }

    #[test]
    fn test_weighted_update_matches_installed_plackett_luce_exactly() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [weighted(1, 42.0, 5.0, 30.0), weighted(2, 38.0, 7.0, 5.0)];
        let team2 = [weighted(3, 45.0, 4.0, 20.0), weighted(4, 35.0, 6.0, 10.0)];
        let factors1 = system.performance_factors(&[30.0, 5.0]);
        let factors2 = system.performance_factors(&[20.0, 10.0]);
        let native = system
            .rate_native(
                &[Rating::new(42.0, 5.0), Rating::new(38.0, 7.0)],
                &[Rating::new(45.0, 4.0), Rating::new(35.0, 6.0)],
                WinningTeam::Team1,
                Some((&factors1, &factors2)),
            )
            .unwrap();
        let actual = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        for (id, expected) in [
            (1, native.0[0]),
            (2, native.0[1]),
            (3, native.1[0]),
            (4, native.1[1]),
        ] {
            close(actual[&id].rating.mu, expected.mu, 1e-12);
            close(actual[&id].rating.sigma, expected.sigma, 1e-12);
        }
    }

    #[test]
    fn test_uncertain_upset_is_not_capped_at_two_mu() {
        let system = CamaOpenSkillSystem::new();
        let result = system
            .update_ratings_after_match(
                &[weighted(1, 25.0, 8.333, 30.0)],
                &[weighted(2, 60.0, 4.0, 5.0)],
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        assert!(result[&1].rating.mu - 25.0 > 2.0);
    }

    #[test]
    fn test_native_mu_can_move_below_display_origin() {
        let system = CamaOpenSkillSystem::new();
        let result = system
            .update_ratings_after_match(
                &[weighted(1, 60.0, 4.0, 30.0)],
                &[weighted(2, 25.0, 8.0, 5.0)],
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        assert!(result[&2].rating.mu < 25.0);
        assert_eq!(system.mu_to_display(result[&2].rating.mu), 0);
    }

    #[test]
    fn test_weighted_update_scales_only_selected_players_mu_delta() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [weighted(1, 40.0, 5.0, 30.0), weighted(2, 40.0, 5.0, 5.0)];
        let team2 = [weighted(3, 40.0, 5.0, 30.0), weighted(4, 40.0, 5.0, 5.0)];
        let native = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        let streaks = BTreeMap::from([(1, 1.75), (4, 1.5)]);
        let streaked = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                &streaks,
                NO_MULTIPLIERS,
            )
            .unwrap();
        close(
            streaked[&1].rating.mu,
            40.0 + (native[&1].rating.mu - 40.0) * 1.75,
            1e-12,
        );
        close(
            streaked[&4].rating.mu,
            40.0 + (native[&4].rating.mu - 40.0) * 1.5,
            1e-12,
        );
        for id in [2, 3] {
            assert_eq!(streaked[&id], native[&id]);
        }
        for id in [1, 2, 3, 4] {
            assert_eq!(streaked[&id].rating.sigma, native[&id].rating.sigma);
            assert_eq!(
                streaked[&id].contribution_weight,
                native[&id].contribution_weight
            );
        }
    }

    #[test]
    fn test_explicit_one_x_preserves_native_values_exactly() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [weighted(1, 42.0, 5.0, 30.0), weighted(2, 38.0, 7.0, 5.0)];
        let team2 = [weighted(3, 45.0, 4.0, 20.0), weighted(4, 35.0, 6.0, 10.0)];
        let native = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        let ones = BTreeMap::from([(1, 1.0), (2, 1.0), (3, 1.0), (4, 1.0)]);
        let explicit = system
            .update_ratings_after_match(&team1, &team2, WinningTeam::Team1, &ones, NO_MULTIPLIERS)
            .unwrap();
        assert_eq!(explicit, native);
    }

    #[test]
    fn test_equal_weight_api_forwards_streak_multipliers() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [equal(1, 35.0, 8.0), equal(2, 35.0, 8.0)];
        let team2 = [equal(3, 35.0, 8.0), equal(4, 35.0, 8.0)];
        let native = system
            .update_ratings_equal_weight(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        let streaks = BTreeMap::from([(1, 1.75), (3, 1.5)]);
        let streaked = system
            .update_ratings_equal_weight(
                &team1,
                &team2,
                WinningTeam::Team1,
                &streaks,
                NO_MULTIPLIERS,
            )
            .unwrap();
        close(streaked[&1].mu, 35.0 + (native[&1].mu - 35.0) * 1.75, 1e-12);
        close(streaked[&3].mu, 35.0 + (native[&3].mu - 35.0) * 1.5, 1e-12);
        assert_eq!(streaked[&2], native[&2]);
        assert_eq!(streaked[&4], native[&4]);
        assert_eq!(streaked[&1].sigma, native[&1].sigma);
        assert_eq!(streaked[&3].sigma, native[&3].sigma);
    }

    #[test]
    fn test_gain_multiplier_stacks_with_streak_without_amplifying_loss() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [equal(1, 35.0, 8.0)];
        let team2 = [equal(2, 35.0, 8.0)];
        let streaks = BTreeMap::from([(1, 1.25), (2, 1.25)]);
        let gains = BTreeMap::from([(1, 1.10), (2, 1.10)]);
        let streaked = system
            .update_ratings_equal_weight(
                &team1,
                &team2,
                WinningTeam::Team1,
                &streaks,
                NO_MULTIPLIERS,
            )
            .unwrap();
        let boosted = system
            .update_ratings_equal_weight(&team1, &team2, WinningTeam::Team1, &streaks, &gains)
            .unwrap();
        close(
            boosted[&1].mu - 35.0,
            (streaked[&1].mu - 35.0) * 1.10,
            1e-12,
        );
        close(boosted[&2].mu, streaked[&2].mu, 1e-12);
        assert_eq!(boosted[&1].sigma, streaked[&1].sigma);
        assert_eq!(boosted[&2].sigma, streaked[&2].sigma);
    }

    #[test]
    fn test_equal_weight_uses_native_posterior() {
        let system = CamaOpenSkillSystem::new();
        let direct = system
            .rate_native(
                &[Rating::new(60.0, 4.0)],
                &[Rating::new(25.0, 8.0)],
                WinningTeam::Team1,
                None,
            )
            .unwrap();
        let actual = system
            .update_ratings_equal_weight(
                &[equal(1, 60.0, 4.0)],
                &[equal(2, 25.0, 8.0)],
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        assert_eq!(actual[&1], direct.0[0]);
        assert_eq!(actual[&2], direct.1[0]);
    }

    #[test]
    fn test_equal_weight_symmetry() {
        let system = CamaOpenSkillSystem::new();
        let actual = system
            .update_ratings_equal_weight(
                &[equal(1, 35.0, 8.0), equal(2, 35.0, 8.0)],
                &[equal(3, 35.0, 8.0), equal(4, 35.0, 8.0)],
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        close(actual[&1].mu, actual[&2].mu, 0.01);
        close(actual[&3].mu, actual[&4].mu, 0.01);
    }

    #[test]
    fn test_decay_respects_grace_period() {
        close(
            CamaOpenSkillSystem::new().apply_sigma_decay(4.0, 7),
            4.0,
            1e-15,
        );
    }

    #[test]
    fn test_decay_increases_uncertainty_by_variance() {
        close(
            CamaOpenSkillSystem::new().apply_sigma_decay(4.0, 14),
            (4.0_f64.powi(2) + 2.0_f64.powi(2)).sqrt(),
            1e-15,
        );
    }

    #[test]
    fn test_decay_caps_at_new_player_uncertainty() {
        close(
            CamaOpenSkillSystem::new().apply_sigma_decay(4.0, 1000),
            CamaOpenSkillSystem::DEFAULT_SIGMA,
            1e-15,
        );
    }

    #[test]
    fn test_calibration_preserves_coin_flip() {
        close(
            CamaOpenSkillSystem::new()
                .calibrate_win_probability(0.5, None)
                .unwrap(),
            0.5,
            1e-15,
        );
    }

    #[test]
    fn test_calibration_softens_extreme_favorite_without_flipping() {
        let value = CamaOpenSkillSystem::new()
            .calibrate_win_probability(0.9, None)
            .unwrap();
        assert!((0.5..0.9).contains(&value));
    }

    #[test]
    fn test_calibration_is_symmetric() {
        let system = CamaOpenSkillSystem::new();
        let favorite = system.calibrate_win_probability(0.85, None).unwrap();
        let underdog = system.calibrate_win_probability(0.15, None).unwrap();
        close(favorite, 1.0 - underdog, 1e-12);
    }

    #[test]
    fn test_calibrated_prediction_keeps_raw_probability_available() {
        let system = CamaOpenSkillSystem::new();
        let team1 = vec![Rating::new(60.0, 4.0); 5];
        let team2 = vec![Rating::new(35.0, 4.0); 5];
        let raw = system.os_predict_win_probability(&team1, &team2);
        let calibrated = system
            .os_predict_calibrated_win_probability(&team1, &team2)
            .unwrap();
        assert!(raw > 0.5 && calibrated > 0.5 && calibrated < raw);
    }

    #[test]
    fn test_mu_to_display_floor() {
        assert_eq!(CamaOpenSkillSystem::new().mu_to_display(25.0), 0);
    }

    #[test]
    fn test_mu_to_display_ceiling_at_max_mmr() {
        assert_eq!(CamaOpenSkillSystem::new().mu_to_display(85.0), 3000);
    }

    #[test]
    fn test_mu_to_display_mid() {
        let display = CamaOpenSkillSystem::new().mu_to_display(45.0);
        assert!((900..1100).contains(&display));
    }

    #[test]
    fn test_mu_to_display_matches_glicko_scale_at_max_mmr() {
        let system = CamaOpenSkillSystem::new();
        assert_eq!(system.mu_to_display(system.mmr_to_os_mu(12_000)), 3000);
    }

    #[test]
    fn test_algorithm_fingerprint_changes_with_rating_configuration() {
        let original = CamaOpenSkillSystem::new().algorithm_fingerprint();
        let mut config = OpenSkillConfig::default();
        config.performance_strength += 0.01;
        let changed = CamaOpenSkillSystem::with_config(config).unwrap();
        assert_ne!(changed.algorithm_fingerprint(), original);
    }

    #[test]
    fn test_algorithm_fingerprint_includes_streak_configuration() {
        let original = CamaOpenSkillSystem::new().algorithm_fingerprint();
        let mut config = OpenSkillConfig::default();
        config.streak_multiplier_per_game += 0.01;
        let changed = CamaOpenSkillSystem::with_config(config).unwrap();
        assert_ne!(changed.algorithm_fingerprint(), original);
    }

    #[test]
    fn test_algorithm_fingerprint_includes_positive_gain_configuration() {
        let original = CamaOpenSkillSystem::new();
        assert!(
            original
                .algorithm_spec_json()
                .contains("\"positive_gain_policy\":\"posthoc_positive_mu_delta\"")
        );
        let mut config = OpenSkillConfig::default();
        config.low_priority_gain_multiplier += 0.01;
        let changed = CamaOpenSkillSystem::with_config(config).unwrap();
        assert_ne!(
            changed.algorithm_fingerprint(),
            original.algorithm_fingerprint()
        );
    }

    #[test]
    fn native_posterior_matches_python_oracle() {
        let system = CamaOpenSkillSystem::new();
        let team1 = [weighted(1, 42.0, 5.0, 30.0), weighted(2, 38.0, 7.0, 5.0)];
        let team2 = [weighted(3, 45.0, 4.0, 20.0), weighted(4, 35.0, 6.0, 10.0)];
        let actual = system
            .update_ratings_after_match(
                &team1,
                &team2,
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            )
            .unwrap();
        close(actual[&1].rating.mu, 43.035_486_501_589_56, 2e-13);
        close(actual[&1].rating.sigma, 4.930_925_700_022_797, 2e-13);
        close(actual[&4].rating.mu, 33.551_045_638_509_905, 2e-13);
        close(actual[&4].rating.sigma, 5.902_252_445_582_854, 2e-13);
    }

    #[test]
    fn default_algorithm_fingerprint_matches_python() {
        let system = CamaOpenSkillSystem::new();
        assert_eq!(system.algorithm_fingerprint(), "ffdaf6752ef51115");
    }

    #[test]
    fn validation_rejects_unsafe_update_shapes() {
        let system = CamaOpenSkillSystem::new();
        assert_eq!(
            WinningTeam::try_from(3),
            Err(OpenSkillError::InvalidWinningTeam(3))
        );
        assert_eq!(
            system.rate_native(&[], &[Rating::new(25.0, 8.0)], WinningTeam::Team1, None),
            Err(OpenSkillError::EmptyTeam)
        );
        let duplicate = WeightedPlayer::new(1, None, None, None);
        assert_eq!(
            system.update_ratings_after_match(
                &[duplicate],
                &[duplicate],
                WinningTeam::Team1,
                NO_MULTIPLIERS,
                NO_MULTIPLIERS,
            ),
            Err(OpenSkillError::DuplicatePlayerId(1))
        );
    }
}
