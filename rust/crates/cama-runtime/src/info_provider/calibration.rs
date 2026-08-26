use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cama_app::drawing::draw_rating_distribution_with_median;
use cama_db::core_repositories::{LobbyTypeStats, MatchRepository, PlayerRepository};
use cama_domain::openskill::{CamaOpenSkillSystem, Rating as OpenSkillRating};
use cama_domain::player::Player;
use cama_domain::rating::{CamaRatingSystem, MAX_GLICKO_RD, RatingConfig, cap_glicko_rd};
use cama_domain::rating_insights::{
    CalibrationStats, MatchPrediction, OpenSkillMatchRatings, PlayerCalibration, PredictionQuality,
    RatingHistoryEntry, RatingMovement, RatingSeedCalculator, RatingStability,
    compute_calibration_stats, compute_player_calibration, compute_prediction_quality,
    rd_to_certainty,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::ApplicationConfig;
use crate::registration::{
    InteractionAllowedMentions, InteractionAttachment, InteractionEmbed, InteractionResponse,
};

const DISCORD_BLUE: u32 = 0x34_98_DB;
const PRODUCTION_DOTABASE_PATH: &str = "/app/assets/dotabase.db";

#[derive(Clone)]
pub(super) struct CalibrationDataSources {
    path: PathBuf,
    heroes: Arc<dyn CalibrationHeroCatalog>,
    rating_system: CamaRatingSystem,
    openskill_system: Result<CamaOpenSkillSystem, String>,
}

impl CalibrationDataSources {
    pub(super) fn new(path: impl AsRef<Path>, config: &ApplicationConfig) -> Self {
        let values = &config.values;
        let migration = config.migration_settings();
        let rating_system = CamaRatingSystem::with_config(
            values.initial_glicko_rd,
            0.06,
            RatingConfig {
                calibration_rd_threshold: values.calibration_rd_threshold,
                initial_rd: values.initial_glicko_rd,
                max_rating_swing_per_game: values.max_rating_swing_per_game,
                base_rating_delta_multiplier: values.base_rating_delta_multiplier,
                max_rd_contraction_per_game: values.max_rd_contraction_per_game,
                new_player_mmr_discount: migration.new_player_mmr_discount,
                rd_decay_constant: values.rd_decay_constant,
                rd_decay_grace_period_days: saturating_i32(values.rd_decay_grace_period_days),
                streak_threshold: values.streak_threshold,
                streak_multiplier_per_game: values.streak_multiplier_per_game,
            },
        );
        Self {
            path: path.as_ref().to_path_buf(),
            heroes: Arc::new(DotabaseHeroCatalog::new(PRODUCTION_DOTABASE_PATH)),
            rating_system,
            openskill_system: CamaOpenSkillSystem::with_config(migration.openskill)
                .map_err(|error| error.to_string()),
        }
    }

    #[cfg(all(test, feature = "runtime-test-match"))]
    const fn rating_system(&self) -> &CamaRatingSystem {
        &self.rating_system
    }

    pub(super) fn openskill_system(&self) -> Result<&CamaOpenSkillSystem, String> {
        self.openskill_system.as_ref().map_err(Clone::clone)
    }

    fn connection(&self) -> Result<Connection, String> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|error| error.to_string())?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        connection
            .pragma_update(None, "foreign_keys", false)
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }

    fn player_repository(&self) -> PlayerRepository {
        PlayerRepository::new(&self.path)
    }

    fn match_repository(&self) -> MatchRepository {
        MatchRepository::new(&self.path)
    }
}

pub(super) fn calibration_response(
    sources: CalibrationDataSources,
    target: (u64, Option<String>, bool),
    guild_id: Option<i64>,
) -> Result<InteractionResponse, String> {
    if target.2 {
        let user_id = i64::try_from(target.0)
            .map_err(|_| "Discord user ID exceeds SQLite's signed range".to_owned())?;
        sources.individual_response(
            user_id,
            target.1.unwrap_or_else(|| target.0.to_string()),
            guild_id,
        )
    } else {
        sources.server_response(guild_id)
    }
}

#[derive(Clone, Copy, Debug)]
struct PredictionRow {
    expected_radiant_win_prob: Option<f64>,
    winning_team: Option<i32>,
}

#[derive(Clone, Copy, Debug)]
struct UpsetRow {
    match_id: i64,
    underdog_win_prob: f64,
    winning_team: i32,
}

#[derive(Clone, Copy, Debug)]
struct PerformanceRow {
    discord_id: i64,
    total_matches: i64,
    overperformance: f64,
}

impl CalibrationDataSources {
    fn server_response(&self, guild_id: Option<i64>) -> Result<InteractionResponse, String> {
        let players = self
            .player_repository()
            .get_all(guild_id)
            .map_err(|error| error.to_string())?;
        let match_count = self.match_count(guild_id)?;
        let prediction_rows = self.recent_predictions(guild_id, 200)?;
        let predictions = prediction_rows
            .iter()
            .map(|row| MatchPrediction {
                expected_radiant_win_prob: row.expected_radiant_win_prob,
                winning_team: row.winning_team,
            })
            .collect::<Vec<_>>();
        let history = self.recent_rating_history(guild_id, 2_000)?;
        let biggest_upsets = self.biggest_upsets(guild_id, 5)?;
        let performance = self.player_performance(guild_id)?;
        let mut stats = compute_calibration_stats(&players, match_count, &predictions, &history);
        // Python constructs its OpenSkill model only when rating history is
        // non-empty. Preserve that error boundary for invalid deployment
        // settings: an empty-history server report still renders, while any
        // history exercises (and validates) the configured model.
        let openskill_system = if history.is_empty() {
            None
        } else {
            Some(self.openskill_system()?)
        };
        apply_configured_calibration_stats(
            &mut stats,
            &players,
            &history,
            &self.rating_system,
            openskill_system,
        )?;
        let rating_system = &self.rating_system;

        let bucket = |name| stats.rating_bucket_count(name).unwrap_or_default();
        let avg_rating = optional_rounded(stats.avg_rating);
        let median_rating = optional_rounded(stats.median_rating);
        let rating_distribution = format!(
            "Immortal (1355+): {} | Divine (1155-1354): {}\nAncient (962-1154): {} | Legend (770-961): {}\nArchon (578-769): {} | Crusader (385-577): {}\nGuardian (192-384): {} | Herald (0-191): {}\nAvg: {avg_rating} | Median: {median_rating}",
            bucket("Immortal"),
            bucket("Divine"),
            bucket("Ancient"),
            bucket("Legend"),
            bucket("Archon"),
            bucket("Crusader"),
            bucket("Guardian"),
            bucket("Herald"),
        );
        let tier = |name| stats.rd_tier_count(name).unwrap_or_default();
        let calibration_progress = format!(
            "**Locked In** (RD ≤75, 70-100% certain): {}\n**Settling** (RD 76-150, 40-70% certain): {}\n**Developing** (RD 151-249, 1-40% certain): {}\n**Fresh** (RD 250+, 0% certain): {}\n\nAvg: RD {} ({} certain)",
            tier("Locked In"),
            tier("Settling"),
            tier("Developing"),
            tier("Fresh"),
            optional_rounded(stats.avg_rd),
            stats
                .avg_certainty
                .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.0}%")),
        );
        let side_text = side_balance_text(&stats.side_balance);
        let movement_text = format!(
            "{}\n{}\n*Display points per game; higher = more volatile*",
            format_movement("Glicko", stats.glicko_rating_movement),
            format_movement("OpenSkill", stats.openskill_rating_movement),
        );
        let drift_text = drift_text(&stats);
        let stability_text = format!(
            "{}\n{}\n*Ratio <1 means calibrated ratings move less*",
            format_stability("Glicko", stats.glicko_rating_stability),
            format_stability("OpenSkill", stats.openskill_rating_stability),
        );
        let mut embed = InteractionEmbed::titled("Rating System Health")
            .color(DISCORD_BLUE)
            .field(
                "System Overview",
                format!(
                    "Total Players: {} | Matches Recorded: {}\nPlayers with Ratings: {} | Avg Games/Player: {}",
                    stats.total_players,
                    stats.match_count,
                    stats.rated_players,
                    stats
                        .avg_games
                        .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.1}")),
                ),
                false,
            )
            .field("Rating Distribution", rating_distribution, false)
            .field("📈 Calibration Progress", calibration_progress, false)
            .field("⚔️ Side Balance", side_text, true)
            .field(
                "🎯 Glicko Prediction Quality",
                format_prediction_quality(stats.glicko_prediction_quality),
                true,
            )
            .field(
                "🎯 OpenSkill Prediction Quality",
                format_prediction_quality(stats.openskill_prediction_quality),
                true,
            )
            .field("📊 Rating Movement", movement_text, false)
            .field("🔄 Rating Drift (Seed vs Current)", drift_text, false)
            .field("⚖️ Rating Stability", stability_text, false);

        if !stats.team_composition.halves.is_empty() {
            let mut lines = Vec::new();
            if let Some(correlation) = stats.team_composition.gini_correlation {
                let verdict = if correlation.abs() < 0.1 {
                    "Rating spread has no meaningful effect on match outcomes"
                } else if correlation > 0.0 {
                    "Teams with mixed ratings slightly overperform"
                } else {
                    "Teams with similar ratings slightly overperform"
                };
                lines.push(format!("*{verdict}*"));
            }
            for half in &stats.team_composition.halves {
                let over = half.overperformance * 100.0;
                let indicator = if over > 3.0 {
                    "+"
                } else if over < -3.0 {
                    "-"
                } else {
                    "~"
                };
                lines.push(format!(
                    "{indicator} **{}**: {:.0}% WR vs {:.0}% exp ({over:+.0}%, {}W/{}G)",
                    half.name,
                    half.winrate * 100.0,
                    half.avg_expected * 100.0,
                    half.wins,
                    half.total,
                ));
            }
            embed = embed.field("🧪 Rating Spread & Winrate", lines.join("\n"), false);
        }

        let lobby_stats = self
            .match_repository()
            .get_lobby_type_stats(guild_id)
            .map_err(|error| error.to_string())?;
        let lobby_lines = lobby_stats_lines(&lobby_stats, false);
        if !lobby_lines.is_empty() {
            embed = embed.field("🎲 Lobby Type Impact", lobby_lines.join("\n"), false);
        }
        embed = embed
            .field(
                "Highest Rated",
                format_ranked(&stats.top_rated, |player| {
                    format!(
                        "{:.0}",
                        rating_system.rating_to_display(player.glicko_rating.unwrap_or_default())
                    )
                }),
                true,
            )
            .field(
                "Most Calibrated",
                format_calibrated(&stats.most_calibrated),
                true,
            )
            .field(
                "Most Volatile",
                format_ranked(&stats.highest_volatility, |player| {
                    format!("{:.3}", player.glicko_volatility.unwrap_or_default())
                }),
                true,
            )
            .field(
                "Lowest Rated",
                format_ranked(&stats.lowest_rated, |player| {
                    format!(
                        "{:.0}",
                        rating_system.rating_to_display(player.glicko_rating.unwrap_or_default())
                    )
                }),
                true,
            )
            .field(
                "Least Calibrated",
                format_calibrated(&stats.least_calibrated),
                true,
            )
            .field(
                "Most Experienced",
                format_ranked(&stats.most_experienced, |player| {
                    format!("{} games", player.wins.saturating_add(player.losses))
                }),
                true,
            );

        if let Some(last) = prediction_rows.first() {
            let (result, outcome) = match last.winning_team {
                Some(1) => {
                    let probability = last.expected_radiant_win_prob.ok_or_else(|| {
                        "completed match prediction has no Radiant probability".to_owned()
                    })?;
                    (
                        format!(
                            "Radiant won (had {probability:.0}% chance)",
                            probability = probability * 100.0
                        ),
                        if probability >= 0.5 {
                            "expected"
                        } else {
                            "upset"
                        },
                    )
                }
                Some(2) => {
                    let probability = last.expected_radiant_win_prob.ok_or_else(|| {
                        "completed match prediction has no Radiant probability".to_owned()
                    })?;
                    (
                        format!("Dire won (had {:.0}% chance)", (1.0 - probability) * 100.0),
                        if probability <= 0.5 {
                            "expected"
                        } else {
                            "upset"
                        },
                    )
                }
                _ => ("Pending...".to_owned(), ""),
            };
            let emoji = match outcome {
                "expected" => "✅",
                "upset" => "🔥",
                _ => "",
            };
            embed = embed.field("Last Match", format!("{emoji} {result}"), false);
        }
        if !biggest_upsets.is_empty() {
            embed = embed.field(
                "🔥 Biggest Upsets",
                biggest_upsets
                    .iter()
                    .take(5)
                    .map(|upset| {
                        format!(
                            "Match #{}: {} won ({:.0}% chance)",
                            upset.match_id,
                            if upset.winning_team == 1 {
                                "Radiant"
                            } else {
                                "Dire"
                            },
                            upset.underdog_win_prob * 100.0,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
            );
        }
        if !performance.is_empty() {
            let registered = players
                .iter()
                .filter_map(|player| player.discord_id)
                .collect::<std::collections::BTreeSet<_>>();
            embed = embed.field(
                "🎯 Top Outperformers",
                performance
                    .iter()
                    .take(3)
                    .map(|entry| {
                        let name = if registered.contains(&entry.discord_id) {
                            format!("<@{}>", entry.discord_id)
                        } else {
                            format!("ID:{}", entry.discord_id)
                        };
                        format!(
                            "{name}: +{:.1} wins over expected ({} games)",
                            entry.overperformance, entry.total_matches
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
            );
        }
        embed = embed.footer(
            "RD = Rating Deviation | Drift = Current - Seed | Brier: 0=perfect, 0.25=coin flip | ECE: lower=better calibrated",
        );

        let ratings = players
            .iter()
            .filter_map(|player| player.glicko_rating)
            .collect::<Vec<_>>();
        let mut response = InteractionResponse::message("").embed(if ratings.is_empty() {
            embed
        } else {
            embed.image("attachment://rating_distribution.png")
        });
        if !ratings.is_empty() {
            response = response.attachment(InteractionAttachment::bytes(
                "rating_distribution.png",
                draw_rating_distribution_with_median(&ratings, stats.median_rating).into_inner(),
            ));
        }
        response.ephemeral = true;
        response.allowed_mentions = user_mentions_from_embed(&response.embeds);
        Ok(response)
    }

    fn match_count(&self, guild_id: Option<i64>) -> Result<usize, String> {
        let connection = self.connection()?;
        let count = connection
            .query_row(
                "SELECT COUNT(*) FROM matches WHERE guild_id = ?1",
                [PlayerRepository::normalize_guild_id(guild_id)],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?;
        usize::try_from(count).map_err(|_| "negative match count in SQLite".to_owned())
    }

    fn recent_predictions(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<PredictionRow>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT mp.expected_radiant_win_prob,m.winning_team
                   FROM match_predictions AS mp
                   JOIN matches AS m ON m.match_id=mp.match_id
                  WHERE m.guild_id=?1 ORDER BY m.match_date DESC LIMIT ?2",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(
                params![PlayerRepository::normalize_guild_id(guild_id), limit],
                |row| {
                    Ok(PredictionRow {
                        expected_radiant_win_prob: row.get(0)?,
                        winning_team: row
                            .get::<_, Option<i64>>(1)?
                            .and_then(|value| i32::try_from(value).ok()),
                    })
                },
            )
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn recent_rating_history(
        &self,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<RatingHistoryEntry>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT match_id,team_number,won,rating_before,rating,rd_before,
                        expected_team_win_prob,os_mu_before,os_mu_after,os_sigma_before
                   FROM rating_history WHERE guild_id=?1
                  ORDER BY timestamp DESC LIMIT ?2",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(
                params![PlayerRepository::normalize_guild_id(guild_id), limit],
                rating_history_from_row,
            )
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn biggest_upsets(&self, guild_id: Option<i64>, limit: i64) -> Result<Vec<UpsetRow>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT mp.match_id,mp.expected_radiant_win_prob,m.winning_team
                   FROM match_predictions AS mp
                   JOIN matches AS m ON m.match_id=mp.match_id
                  WHERE m.guild_id=?1 AND m.winning_team IS NOT NULL
                    AND ((m.winning_team=1 AND mp.expected_radiant_win_prob<0.5)
                      OR (m.winning_team=2 AND mp.expected_radiant_win_prob>0.5))
                  ORDER BY CASE WHEN m.winning_team=1
                                THEN mp.expected_radiant_win_prob
                                ELSE 1.0-mp.expected_radiant_win_prob END ASC
                  LIMIT ?2",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(
                params![PlayerRepository::normalize_guild_id(guild_id), limit],
                |row| {
                    let probability = row.get::<_, f64>(1)?;
                    let winner = row.get::<_, i32>(2)?;
                    Ok(UpsetRow {
                        match_id: row.get(0)?,
                        underdog_win_prob: if winner == 1 {
                            probability
                        } else {
                            1.0 - probability
                        },
                        winning_team: winner,
                    })
                },
            )
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn player_performance(&self, guild_id: Option<i64>) -> Result<Vec<PerformanceRow>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT discord_id,COUNT(*),
                        SUM(CASE WHEN won=1 THEN 1 ELSE 0 END)-SUM(expected_team_win_prob)
                   FROM rating_history
                  WHERE guild_id=?1 AND expected_team_win_prob IS NOT NULL
                  GROUP BY discord_id HAVING COUNT(*)>=5 ORDER BY 3 DESC",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map([PlayerRepository::normalize_guild_id(guild_id)], |row| {
                Ok(PerformanceRow {
                    discord_id: row.get(0)?,
                    total_matches: row.get(1)?,
                    overperformance: row.get(2)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }
}

fn rating_history_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RatingHistoryEntry> {
    Ok(RatingHistoryEntry {
        match_id: row.get(0)?,
        team_number: row
            .get::<_, Option<i64>>(1)?
            .and_then(|value| i32::try_from(value).ok()),
        won: row.get::<_, Option<i64>>(2)?.map(|value| value != 0),
        rating_before: row.get(3)?,
        rating: row.get(4)?,
        rd_before: row.get(5)?,
        expected_team_win_prob: row.get(6)?,
        os_mu_before: row.get(7)?,
        os_mu_after: row.get(8)?,
        os_sigma_before: row.get(9)?,
    })
}

const fn saturating_i32(value: i64) -> i32 {
    if value > i32::MAX as i64 {
        i32::MAX
    } else if value < i32::MIN as i64 {
        i32::MIN
    } else {
        value as i32
    }
}

struct ConfiguredRatingSeedCalculator<'a>(&'a CamaRatingSystem);

impl RatingSeedCalculator for ConfiguredRatingSeedCalculator<'_> {
    fn seed_rating(&self, source_mmr: i64) -> f64 {
        let source_mmr = saturating_i32(source_mmr.clamp(
            i64::from(CamaRatingSystem::MMR_MIN),
            i64::from(CamaRatingSystem::MMR_MAX),
        ));
        self.0.mmr_to_rating(self.0.new_player_seed_mmr(source_mmr))
    }
}

fn apply_configured_calibration_stats(
    stats: &mut CalibrationStats,
    players: &[Player],
    history: &[RatingHistoryEntry],
    rating_system: &CamaRatingSystem,
    openskill_system: Option<&CamaOpenSkillSystem>,
) -> Result<(), String> {
    let seed = ConfiguredRatingSeedCalculator(rating_system);
    let mut drifts = players
        .iter()
        .filter_map(|player| {
            Some((
                player.clone(),
                player.glicko_rating? - seed.seed_rating(player.initial_mmr?),
            ))
        })
        .collect::<Vec<_>>();
    let drift_values = drifts.iter().map(|(_, drift)| *drift).collect::<Vec<_>>();
    stats.avg_drift = mean_value(&drift_values);
    stats.median_drift = median_value(&drift_values);
    drifts.sort_by(|left, right| right.1.total_cmp(&left.1));
    stats.biggest_gainers = drifts.iter().take(3).cloned().collect();
    drifts.sort_by(|left, right| left.1.total_cmp(&right.1));
    stats.biggest_drops = drifts.into_iter().take(3).collect();
    if let Some(openskill_system) = openskill_system {
        stats.openskill_prediction_quality =
            configured_openskill_prediction_quality(history, openskill_system)?;
        stats.openskill_rating_stability =
            configured_openskill_stability(history, openskill_system);
    }
    Ok(())
}

fn configured_openskill_prediction_quality(
    history: &[RatingHistoryEntry],
    system: &CamaOpenSkillSystem,
) -> Result<PredictionQuality, String> {
    let mut matches = BTreeMap::<i64, (Vec<&RatingHistoryEntry>, Vec<&RatingHistoryEntry>)>::new();
    for entry in history {
        let (Some(match_id), Some(team @ (1 | 2))) = (entry.match_id, entry.team_number) else {
            continue;
        };
        let teams = matches.entry(match_id).or_default();
        if team == 1 {
            teams.0.push(entry);
        } else {
            teams.1.push(entry);
        }
    }

    let mut predictions = Vec::new();
    for (team1, team2) in matches.into_values() {
        if team1.len() != 5 || team2.len() != 5 {
            continue;
        }
        let team1_ratings = team1
            .iter()
            .map(|entry| Some((entry.os_mu_before?, entry.os_sigma_before?)))
            .collect::<Option<Vec<_>>>();
        let team2_ratings = team2
            .iter()
            .map(|entry| Some((entry.os_mu_before?, entry.os_sigma_before?)))
            .collect::<Option<Vec<_>>>();
        let (Some(team1_ratings), Some(team2_ratings)) = (team1_ratings, team2_ratings) else {
            continue;
        };
        let team1_won = team1[0].won.unwrap_or(false);
        let team2_won = team2[0].won.unwrap_or(false);
        if team1_won == team2_won {
            continue;
        }
        predictions.push(MatchPrediction {
            expected_radiant_win_prob: Some(configured_openskill_probability(
                system,
                &team1_ratings,
                &team2_ratings,
            )?),
            winning_team: Some(if team1_won { 1 } else { 2 }),
        });
    }
    Ok(compute_prediction_quality(&predictions))
}

fn configured_openskill_probability(
    system: &CamaOpenSkillSystem,
    team: &[(f64, f64)],
    opponent: &[(f64, f64)],
) -> Result<f64, String> {
    let team = team
        .iter()
        .map(|(mu, sigma)| OpenSkillRating::new(*mu, *sigma))
        .collect::<Vec<_>>();
    let opponent = opponent
        .iter()
        .map(|(mu, sigma)| OpenSkillRating::new(*mu, *sigma))
        .collect::<Vec<_>>();
    system
        .os_predict_calibrated_win_probability(&team, &opponent)
        .map_err(|error| error.to_string())
}

fn configured_openskill_stability(
    history: &[RatingHistoryEntry],
    system: &CamaOpenSkillSystem,
) -> RatingStability {
    let mut calibrated = Vec::new();
    let mut uncalibrated = Vec::new();
    for entry in history {
        let (Some(before), Some(after), Some(sigma)) =
            (entry.os_mu_before, entry.os_mu_after, entry.os_sigma_before)
        else {
            continue;
        };
        let target = if sigma <= system.config().calibration_threshold {
            &mut calibrated
        } else {
            &mut uncalibrated
        };
        target.push((after - before).abs() * system.config().display_scale);
    }
    let calibrated_average = mean_value(&calibrated);
    let uncalibrated_average = mean_value(&uncalibrated);
    RatingStability {
        calibrated_avg_delta: calibrated_average,
        calibrated_count: calibrated.len(),
        uncalibrated_avg_delta: uncalibrated_average,
        uncalibrated_count: uncalibrated.len(),
        stability_ratio: calibrated_average.zip(uncalibrated_average).and_then(
            |(calibrated, uncalibrated)| (uncalibrated > 0.0).then_some(calibrated / uncalibrated),
        ),
    }
}

fn mean_value(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn median_value(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    })
}

fn optional_rounded(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |value| format!("{value:.0}"))
}

fn format_prediction_quality(quality: PredictionQuality) -> String {
    if quality.count == 0 {
        return "No prediction data yet.".to_owned();
    }
    let brier = quality.brier.unwrap_or_default();
    let brier_quality = if brier < 0.15 {
        "excellent"
    } else if brier < 0.20 {
        "good"
    } else if brier < 0.25 {
        "fair"
    } else {
        "poor"
    };
    let balance_rate = quality.balance_rate.unwrap_or_default();
    let balance_desc = if balance_rate >= 0.8 {
        "very balanced"
    } else if balance_rate >= 0.5 {
        "balanced"
    } else {
        "unbalanced"
    };
    format!(
        "**{}** matches analyzed\nBrier Score: **{brier:.3}** ({brier_quality})\nECE: **{}**\nPick Accuracy: **{:.0}%** of favorites won\nBalance Rate: **{:.0}%** were close games ({balance_desc})\nUpset Rate: **{}** underdogs won",
        quality.count,
        quality
            .ece
            .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.3}")),
        quality.accuracy.unwrap_or_default() * 100.0,
        balance_rate * 100.0,
        quality.upset_rate.map_or_else(
            || "n/a".to_owned(),
            |value| format!("{:.0}%", value * 100.0)
        ),
    )
}

fn format_movement(label: &str, movement: RatingMovement) -> String {
    if movement.count == 0 {
        return format!("**{label}:** No rating history");
    }
    format!(
        "**{label}:** ±{:.1} avg | ±{:.1} median ({} changes)",
        movement.avg_delta.unwrap_or_default(),
        movement.median_delta.unwrap_or_default(),
        movement.count,
    )
}

fn format_stability(label: &str, stability: RatingStability) -> String {
    let mut parts = Vec::new();
    if stability.calibrated_count != 0 {
        parts.push(format!(
            "cal ±{:.1} (n={})",
            stability.calibrated_avg_delta.unwrap_or_default(),
            stability.calibrated_count,
        ));
    }
    if stability.uncalibrated_count != 0 {
        parts.push(format!(
            "uncal ±{:.1} (n={})",
            stability.uncalibrated_avg_delta.unwrap_or_default(),
            stability.uncalibrated_count,
        ));
    }
    if let Some(ratio) = stability.stability_ratio {
        parts.push(format!("ratio {ratio:.2}x"));
    }
    format!(
        "**{label}:** {}",
        if parts.is_empty() {
            "No stability history".to_owned()
        } else {
            parts.join(" | ")
        }
    )
}

fn side_balance_text(balance: &cama_domain::rating_insights::SideBalance) -> String {
    if balance.total == 0 {
        return "No match data yet.".to_owned();
    }
    let radiant = balance.radiant_rate.unwrap_or_default();
    let dire = balance.dire_rate.unwrap_or_default();
    let status = if (radiant - 0.5).abs() < 0.05 {
        "perfectly balanced"
    } else if (radiant - 0.5).abs() < 0.1 {
        "slightly favored"
    } else {
        "noticeably favored"
    };
    let favored = if radiant > 0.5 {
        "Radiant"
    } else if radiant < 0.5 {
        "Dire"
    } else {
        "Neither"
    };
    if favored == "Neither" {
        // Preserve the Python conditional-expression precedence: at an exact
        // 50/50 split only the verdict line is rendered.
        format!("*{status}*")
    } else {
        format!(
            "**Radiant**: {}W ({:.0}%)\n**Dire**: {}W ({:.0}%)\n*{favored} {status}*",
            balance.radiant_wins,
            radiant * 100.0,
            balance.dire_wins,
            dire * 100.0,
        )
    }
}

fn drift_text(stats: &cama_domain::rating_insights::CalibrationStats) -> String {
    let (Some(average), Some(median)) = (stats.avg_drift, stats.median_drift) else {
        return "No seed MMR data yet.".to_owned();
    };
    let direction = if average > 0.0 {
        "outperforming"
    } else if average < 0.0 {
        "underperforming"
    } else {
        "matching"
    };
    format!(
        "*Current rating vs initial MMR seed*\nAvg: **{average:+.0}** | Median: **{median:+.0}**\nPlayers are {direction} their pub MMR\n📈 Gainers: {}\n📉 Drops: {}",
        format_drift_players(&stats.biggest_gainers),
        format_drift_players(&stats.biggest_drops),
    )
}

fn format_drift_players(entries: &[(Player, f64)]) -> String {
    if entries.is_empty() {
        return "n/a".to_owned();
    }
    entries
        .iter()
        .take(3)
        .map(|(player, drift)| format!("{} ({drift:+.0})", player_name(player)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_ranked(players: &[Player], value: impl Fn(&Player) -> String) -> String {
    if players.is_empty() {
        return "n/a".to_owned();
    }
    players
        .iter()
        .take(3)
        .enumerate()
        .map(|(index, player)| {
            format!("{}. {} ({})", index + 1, player_name(player), value(player))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_calibrated(players: &[Player]) -> String {
    format_ranked(players, |player| {
        let rd = cap_glicko_rd(player.glicko_rd.unwrap_or(MAX_GLICKO_RD));
        format!("RD {rd:.0}, {:.0}%", rd_to_certainty(rd))
    })
}

fn player_name(player: &Player) -> String {
    player.discord_id.filter(|id| *id > 0).map_or_else(
        || player.name.clone(),
        |discord_id| format!("<@{discord_id}>"),
    )
}

fn lobby_stats_lines(stats: &[LobbyTypeStats], player: bool) -> Vec<String> {
    let shuffle = stats.iter().find(|stats| stats.lobby_type == "shuffle");
    let draft = stats.iter().find(|stats| stats.lobby_type == "draft");
    let mut lines = Vec::new();
    if let Some(stats) = shuffle {
        lines.push(format_lobby_stats_line(stats, "🎲", "Shuffle", player));
    }
    if let Some(stats) = draft {
        lines.push(format_lobby_stats_line(stats, "👑", "Draft", player));
    }
    lines.extend(format_lobby_comparisons(shuffle, draft, player));
    lines
}

fn format_lobby_stats_line(
    stats: &LobbyTypeStats,
    emoji: &str,
    label: &str,
    player: bool,
) -> String {
    let actual = stats.actual_win_rate.map_or_else(
        || "n/a".to_owned(),
        |value| format!("{:.0}%", value * 100.0),
    );
    let expected = stats.expected_win_rate.map_or_else(
        || "n/a".to_owned(),
        |value| format!("{:.0}%", value * 100.0),
    );
    let os_expected = stats.openskill_expected_win_rate.map_or_else(
        || "n/a".to_owned(),
        |value| format!("{:.0}%", value * 100.0),
    );
    format!(
        "{emoji} **{label}**: {} | {} games | {} {actual} | G exp {expected} | O exp {os_expected}",
        format_lobby_swing_summary(stats),
        stats.games,
        if player { "W" } else { "Radiant actual" },
    )
}

fn format_lobby_swing_summary(stats: &LobbyTypeStats) -> String {
    let mut parts = Vec::new();
    if let Some(swing) = stats.glicko_avg_swing.or(stats.avg_swing) {
        parts.push(format!(
            "G ±{swing:.1} (n={})",
            if stats.glicko_samples == 0 {
                stats.games
            } else {
                stats.glicko_samples
            }
        ));
    }
    if let Some(swing) = stats.openskill_avg_swing {
        parts.push(format!("O ±{swing:.1} (n={})", stats.openskill_samples));
    }
    if parts.is_empty() {
        "No rating changes".to_owned()
    } else {
        parts.join(" | ")
    }
}

fn format_lobby_comparisons(
    shuffle: Option<&LobbyTypeStats>,
    draft: Option<&LobbyTypeStats>,
    player: bool,
) -> Vec<String> {
    let (Some(shuffle), Some(draft)) = (shuffle, draft) else {
        return Vec::new();
    };
    [
        (
            "G",
            shuffle.glicko_avg_swing.or(shuffle.avg_swing),
            draft.glicko_avg_swing.or(draft.avg_swing),
        ),
        ("O", shuffle.openskill_avg_swing, draft.openskill_avg_swing),
    ]
    .into_iter()
    .filter_map(|(label, shuffle, draft)| {
        let (Some(shuffle), Some(draft)) = (shuffle, draft) else {
            return None;
        };
        if shuffle <= 0.0 {
            return None;
        }
        let difference = (draft - shuffle) / shuffle * 100.0;
        let comparison = if difference > 0.0 {
            if player {
                format!("You swing {difference:.0}% more in Draft")
            } else {
                format!("Draft swings are {difference:.0}% larger than Shuffle")
            }
        } else if difference < 0.0 {
            if player {
                format!("You swing {:.0}% more in Shuffle", difference.abs())
            } else {
                format!(
                    "Shuffle swings are {:.0}% larger than Draft",
                    difference.abs()
                )
            }
        } else {
            "Draft and Shuffle swings are even".to_owned()
        };
        Some(format!("*{label}: {comparison}*"))
    })
    .collect()
}

#[derive(Clone, Debug)]
struct DetailedHistory {
    insight: RatingHistoryEntry,
    rd_after: Option<f64>,
    lobby_type: String,
}

#[derive(Clone, Copy, Debug)]
struct HeroStats {
    hero_id: i64,
    wins: i64,
    losses: i64,
    avg_kills: f64,
    avg_deaths: f64,
    avg_assists: f64,
    avg_gpm: f64,
    avg_damage: f64,
}

#[derive(Clone, Copy, Debug)]
struct HeroRoleCount {
    hero_id: i64,
    games: i64,
}

#[derive(Clone, Copy, Debug)]
struct FantasyGame {
    match_id: i64,
    fantasy_points: f64,
    won: bool,
    hero_id: Option<i64>,
}

#[derive(Clone, Debug)]
struct FantasyStats {
    total_games: i64,
    total_fp: f64,
    avg_fp: f64,
    best_fp: f64,
    best_match_id: Option<i64>,
    recent_games: Vec<FantasyGame>,
}

impl CalibrationDataSources {
    fn individual_response(
        &self,
        user_id: i64,
        display_name: String,
        guild_id: Option<i64>,
    ) -> Result<InteractionResponse, String> {
        let Some(player) = self
            .player_repository()
            .get_by_id(user_id, guild_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(
                InteractionResponse::message(format!("❌ <@{user_id}> is not registered."))
                    .ephemeral()
                    .with_user_mentions(vec![u64::try_from(user_id).unwrap_or_default()]),
            );
        };
        let detailed_history = self.player_history(user_id, guild_id, 50)?;
        let history = detailed_history
            .iter()
            .map(|entry| entry.insight.clone())
            .collect::<Vec<_>>();
        let all_players = self
            .player_repository()
            .get_all(guild_id)
            .map_err(|error| error.to_string())?;
        let rated_players = all_players
            .into_iter()
            .filter(|player| player.glicko_rating.is_some())
            .collect::<Vec<_>>();
        let calibration = compute_player_calibration(
            &player,
            &history,
            &rated_players,
            &ConfiguredRatingSeedCalculator(&self.rating_system),
        );
        let rating_system = &self.rating_system;
        let os_system = self.openskill_system()?;
        let rd = cap_glicko_rd(player.glicko_rd.unwrap_or(MAX_GLICKO_RD));
        let calibration_tier = if rd <= 75.0 {
            "Locked In"
        } else if rd <= 150.0 {
            "Settling"
        } else if rd < MAX_GLICKO_RD {
            "Developing"
        } else {
            "Fresh"
        };
        let os_data = player.os_mu.map(|mu| {
            (
                mu,
                player
                    .os_sigma
                    .unwrap_or(CamaOpenSkillSystem::DEFAULT_SIGMA),
            )
        });
        let recent = detailed_history.iter().take(5).collect::<Vec<_>>();
        let os_probabilities = self.os_probabilities(&recent, guild_id)?;
        let mut recent_lines = Vec::new();
        for (history, os_expected) in recent.iter().zip(os_probabilities) {
            let row = &history.insight;
            let delta = recent_rating_delta_text(row, rating_system, os_system);
            let mut predictions = Vec::new();
            if let Some(expected) = row.expected_team_win_prob {
                predictions.push(format!("G:{:.0}%", expected * 100.0));
            }
            if let Some(expected) = os_expected {
                predictions.push(format!("O:{:.0}%", expected * 100.0));
            }
            let prediction = if predictions.is_empty() {
                "no pred".to_owned()
            } else {
                predictions.join(" ")
            };
            let won = row.won.unwrap_or(false);
            recent_lines.push(format!(
                "{}#{}: {}{} ({prediction}) → **Δ {delta}**",
                if history.lobby_type == "draft" {
                    "👑"
                } else {
                    "🎲"
                },
                row.match_id.unwrap_or_default(),
                if won { "✅" } else { "❌" },
                if won { "W" } else { "L" },
            ));
        }

        let percentile = calibration
            .percentile
            .filter(|value| *value != 0.0)
            .map_or_else(
                || "N/A".to_owned(),
                |value| format!("Top {:.0}%", 100.0 - value),
            );
        let profile_lines = rating_profile_lines(
            &player,
            rating_system,
            os_system,
            calibration_tier,
            &percentile,
            os_data,
        );
        let mut embed = InteractionEmbed::titled(format!("Calibration Stats: {display_name}"))
            .color(DISCORD_BLUE)
            .field("📊 Rating Profile", profile_lines.join("\n"), false);

        if let Some(drift) = calibration.drift {
            embed = embed.field(
                "🎯 Rating Drift",
                format!(
                    "{} **{drift:+.0}** rating vs initial seed ({} MMR)",
                    if drift > 0.0 {
                        "📈"
                    } else if drift < 0.0 {
                        "📉"
                    } else {
                        "➡️"
                    },
                    player.initial_mmr.unwrap_or_default(),
                ),
                false,
            );
        }
        embed = add_performance_fields(embed, &calibration);
        if calibration.last_5_delta.is_some() || calibration.openskill_last_5_delta.is_some() {
            embed = embed.field("📉 Trend", trend_text(&calibration, history.len()), true);
        }
        if !recent_lines.is_empty() {
            embed = embed.field(
                format!(
                    "📊 Last {} Matches (G=Glicko O=OpenSkill)",
                    recent_lines.len()
                ),
                recent_lines.join("\n"),
                false,
            );
        }
        let player_lobby_stats = self
            .match_repository()
            .get_player_lobby_type_stats(user_id, guild_id)
            .map_err(|error| error.to_string())?;
        let lobby_lines = lobby_stats_lines(&player_lobby_stats, true);
        if !lobby_lines.is_empty() {
            embed = embed.field(
                "🎲 Rating Swings by Lobby Type",
                lobby_lines.join("\n"),
                false,
            );
        }
        if let Some(convergence) = convergence_text(&detailed_history) {
            embed = embed.field("🔄 Convergence Trend", convergence, true);
        }
        let highlights = highlight_lines(&calibration);
        if !highlights.is_empty() {
            embed = embed.field("⚡ Highlights", highlights.join("\n"), false);
        }
        let hero_stats = self.hero_stats(user_id, guild_id, 8)?;
        if !hero_stats.is_empty() {
            let role_counts = self.hero_role_counts(user_id, guild_id)?;
            let role_mismatch = self.role_mismatch(&player, &role_counts)?;
            let mut hero_text = "```\nHero     W-L   KDA       GPM  Dmg\n".to_owned();
            hero_text.push_str(
                &hero_stats
                    .iter()
                    .take(6)
                    .map(|hero| {
                        format!(
                            "`{:<8}` {:<5} {:<9} {:<4} {}",
                            hero_short_name(hero.hero_id),
                            format!("{}-{}", hero.wins, hero.losses),
                            format!(
                                "{:.0}/{:.0}/{:.0}",
                                hero.avg_kills, hero.avg_deaths, hero.avg_assists
                            ),
                            format!("{:.0}", hero.avg_gpm),
                            if hero.avg_damage != 0.0 {
                                format!("{:.1}k", hero.avg_damage / 1_000.0)
                            } else {
                                "-".to_owned()
                            },
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            hero_text.push_str("\n```");
            if let Some(role_mismatch) = role_mismatch {
                hero_text.push('\n');
                hero_text.push_str(&role_mismatch);
            }
            embed = embed.field("🦸 Recent Heroes", hero_text, false);
        }
        if let Some(fantasy) = self.fantasy_stats(user_id, guild_id)?
            && fantasy.total_games > 0
        {
            let mut text = format!(
                "**Avg FP:** {:.1} | **Best:** {:.1} (Match #{})\n**Total:** {:.1} FP over {} enriched games",
                fantasy.avg_fp,
                fantasy.best_fp,
                fantasy.best_match_id.unwrap_or_default(),
                fantasy.total_fp,
                fantasy.total_games,
            );
            if !fantasy.recent_games.is_empty() {
                text.push('\n');
                text.push_str(
                    &fantasy
                        .recent_games
                        .iter()
                        .take(5)
                        .map(|game| {
                            format!(
                                "#{}: {} {} **{:.1}**",
                                game.match_id,
                                if game.won { "W" } else { "L" },
                                game.hero_id
                                    .map_or_else(|| "?".to_owned(), hero_short_name,),
                                game.fantasy_points,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" | "),
                );
            }
            embed = embed.field("⭐ Fantasy Points", text, false);
        }
        if let Some((mu, sigma)) = os_data {
            embed = embed.field(
                "🎲 OpenSkill Rating (Fantasy-Weighted)",
                format!(
                    "**Skill (μ):** {mu:.2} → **{}** display\n**Uncertainty (σ):** {sigma:.3} ({:.0}% certain)\n**Ordinal** (μ-3σ): {:.2}\n**Calibrated:** {}",
                    os_system.mu_to_display(mu),
                    os_system.certainty_percentage(sigma),
                    os_system.ordinal(mu, sigma, 3.0),
                    if os_system.is_calibrated(sigma) { "Yes" } else { "No" },
                ),
                false,
            );
        }
        let games = player.wins.saturating_add(player.losses);
        let record = if games > 0 {
            format!(
                "**W-L:** {}-{} ({:.0}%)",
                player.wins,
                player.losses,
                player.wins as f64 / games as f64 * 100.0,
            )
        } else {
            format!("**W-L:** {}-{}", player.wins, player.losses)
        };
        embed = embed
            .field("📋 Record", record, true)
            .footer("Rating delta shown per game | RD decrease = more stable rating");
        let mut response = InteractionResponse::message("").embed(embed).ephemeral();
        response.allowed_mentions =
            InteractionAllowedMentions::Users(vec![u64::try_from(user_id).unwrap_or_default()]);
        Ok(response)
    }

    fn player_history(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<DetailedHistory>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT rh.match_id,rh.team_number,rh.won,rh.rating_before,rh.rating,
                        rh.rd_before,rh.expected_team_win_prob,rh.os_mu_before,
                        rh.os_mu_after,rh.os_sigma_before,rh.rd_after,
                        COALESCE(m.lobby_type,'shuffle')
                   FROM rating_history AS rh
                   LEFT JOIN matches AS m
                     ON rh.match_id=m.match_id AND m.guild_id=rh.guild_id
                  WHERE rh.discord_id=?1 AND rh.guild_id=?2
                  ORDER BY rh.timestamp DESC LIMIT ?3",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(
                params![
                    user_id,
                    PlayerRepository::normalize_guild_id(guild_id),
                    limit
                ],
                |row| {
                    Ok(DetailedHistory {
                        insight: RatingHistoryEntry {
                            match_id: row.get(0)?,
                            team_number: row
                                .get::<_, Option<i64>>(1)?
                                .and_then(|value| i32::try_from(value).ok()),
                            won: row.get::<_, Option<i64>>(2)?.map(|value| value != 0),
                            rating_before: row.get(3)?,
                            rating: row.get(4)?,
                            rd_before: row.get(5)?,
                            expected_team_win_prob: row.get(6)?,
                            os_mu_before: row.get(7)?,
                            os_mu_after: row.get(8)?,
                            os_sigma_before: row.get(9)?,
                        },
                        rd_after: row.get(10)?,
                        lobby_type: row.get(11)?,
                    })
                },
            )
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn os_probabilities(
        &self,
        history: &[&DetailedHistory],
        guild_id: Option<i64>,
    ) -> Result<Vec<Option<f64>>, String> {
        let ids = history
            .iter()
            .filter_map(|entry| entry.insight.match_id.filter(|id| *id != 0))
            .collect::<std::collections::BTreeSet<_>>();
        if ids.is_empty() {
            return Ok(vec![None; history.len()]);
        }
        let ids = ids.into_iter().collect::<Vec<_>>();
        let connection = self.connection()?;
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT match_id,team_number,os_mu_before,os_sigma_before
               FROM rating_history WHERE guild_id=? AND match_id IN ({placeholders})
                AND os_mu_before IS NOT NULL AND os_sigma_before IS NOT NULL"
        );
        let mut parameters = vec![PlayerRepository::normalize_guild_id(guild_id)];
        parameters.extend(&ids);
        let mut statement = connection
            .prepare(&sql)
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(parameters), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut ratings = HashMap::<i64, OpenSkillMatchRatings>::new();
        for row in rows {
            let (match_id, team, mu, sigma) = row.map_err(|error| error.to_string())?;
            let snapshot = ratings.entry(match_id).or_default();
            if team == 1 {
                snapshot.team1.push((mu, sigma));
            } else if team == 2 {
                snapshot.team2.push((mu, sigma));
            }
        }
        let system = self.openskill_system()?;
        let mut probabilities = Vec::with_capacity(history.len());
        for entry in history {
            let row = &entry.insight;
            let Some(snapshot) = row.match_id.and_then(|match_id| ratings.get(&match_id)) else {
                probabilities.push(None);
                continue;
            };
            if snapshot.team1.is_empty() || snapshot.team2.is_empty() {
                probabilities.push(None);
                continue;
            }
            let probability = match row.team_number {
                Some(1) => {
                    configured_openskill_probability(system, &snapshot.team1, &snapshot.team2)?
                }
                Some(2) => {
                    configured_openskill_probability(system, &snapshot.team2, &snapshot.team1)?
                }
                _ => {
                    probabilities.push(None);
                    continue;
                }
            };
            probabilities.push(Some(probability));
        }
        Ok(probabilities)
    }

    fn hero_stats(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<HeroStats>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT hero_id,COUNT(*),
                        SUM(CASE WHEN won=1 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN won=0 THEN 1 ELSE 0 END),
                        AVG(kills),AVG(deaths),AVG(assists),AVG(gpm),AVG(hero_damage),
                        MAX(match_id)
                   FROM match_participants
                  WHERE discord_id=?1 AND guild_id=?2 AND hero_id IS NOT NULL
                  GROUP BY hero_id ORDER BY 10 DESC LIMIT ?3",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(
                params![
                    user_id,
                    PlayerRepository::normalize_guild_id(guild_id),
                    limit
                ],
                |row| {
                    Ok(HeroStats {
                        hero_id: row.get(0)?,
                        wins: row.get(2)?,
                        losses: row.get(3)?,
                        avg_kills: row.get::<_, Option<f64>>(4)?.unwrap_or_default(),
                        avg_deaths: row.get::<_, Option<f64>>(5)?.unwrap_or_default(),
                        avg_assists: row.get::<_, Option<f64>>(6)?.unwrap_or_default(),
                        avg_gpm: row.get::<_, Option<f64>>(7)?.unwrap_or_default(),
                        avg_damage: row.get::<_, Option<f64>>(8)?.unwrap_or_default(),
                    })
                },
            )
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn hero_role_counts(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<HeroRoleCount>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT hero_id,COUNT(*) FROM match_participants
                  WHERE discord_id=?1 AND guild_id=?2 AND hero_id IS NOT NULL
                  GROUP BY hero_id",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(
                params![user_id, PlayerRepository::normalize_guild_id(guild_id)],
                |row| {
                    Ok(HeroRoleCount {
                        hero_id: row.get(0)?,
                        games: row.get(1)?,
                    })
                },
            )
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn role_mismatch(
        &self,
        player: &Player,
        counts: &[HeroRoleCount],
    ) -> Result<Option<String>, String> {
        let total = counts.iter().map(|entry| entry.games).sum::<i64>();
        if total < 5 {
            return Ok(None);
        }
        // Python deliberately treats a missing/invalid optional Dotabase
        // catalog as an unknown (therefore non-support) hero and still renders
        // the calibration response. Keep that production recovery behavior:
        // the bundled catalog enriches role diagnostics, but can never take
        // the command down.
        let core = counts.iter().fold(0_i64, |total, entry| {
            total
                + if self.heroes.is_support(entry.hero_id).unwrap_or(false) {
                    0
                } else {
                    entry.games
                }
        });
        let roles = player.preferred_roles.as_deref().unwrap_or_default();
        let prefers_support = roles.iter().any(|role| role == "4" || role == "5")
            && !roles
                .iter()
                .any(|role| role == "1" || role == "2" || role == "3");
        let prefers_core = roles
            .iter()
            .any(|role| role == "1" || role == "2" || role == "3")
            && !roles.iter().any(|role| role == "4" || role == "5");
        let core_rate = core as f64 / total as f64;
        Ok(if prefers_support && core_rate > 0.6 {
            Some(format!(
                "⚠️ Prefers Support but plays {:.0}% Core heroes",
                core_rate * 100.0
            ))
        } else if prefers_core && core_rate < 0.4 {
            Some(format!(
                "⚠️ Prefers Core but plays {:.0}% Support heroes",
                (1.0 - core_rate) * 100.0
            ))
        } else {
            None
        })
    }

    fn fantasy_stats(
        &self,
        user_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<FantasyStats>, String> {
        let connection = self.connection()?;
        let guild_id = PlayerRepository::normalize_guild_id(guild_id);
        let aggregate = connection
            .query_row(
                "SELECT COUNT(*),COALESCE(SUM(fantasy_points),0),
                        COALESCE(AVG(fantasy_points),0),COALESCE(MAX(fantasy_points),0)
                   FROM match_participants
                  WHERE discord_id=?1 AND guild_id=?2 AND fantasy_points IS NOT NULL",
                params![user_id, guild_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        if aggregate.0 == 0 {
            return Ok(None);
        }
        let best_match_id = connection
            .query_row(
                "SELECT match_id FROM match_participants
                  WHERE discord_id=?1 AND guild_id=?2 AND fantasy_points=?3 LIMIT 1",
                params![user_id, guild_id, aggregate.3],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT mp.match_id,mp.fantasy_points,mp.won,mp.hero_id
                   FROM match_participants AS mp
                   JOIN matches AS m ON mp.match_id=m.match_id
                  WHERE mp.discord_id=?1 AND mp.guild_id=?2
                    AND mp.fantasy_points IS NOT NULL
                  ORDER BY m.match_date DESC LIMIT 10",
            )
            .map_err(|error| error.to_string())?;
        let recent_games = statement
            .query_map(params![user_id, guild_id], |row| {
                Ok(FantasyGame {
                    match_id: row.get(0)?,
                    fantasy_points: row.get(1)?,
                    won: row.get::<_, Option<i64>>(2)?.unwrap_or_default() != 0,
                    hero_id: row.get(3)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(Some(FantasyStats {
            total_games: aggregate.0,
            total_fp: aggregate.1,
            avg_fp: aggregate.2,
            best_fp: aggregate.3,
            best_match_id,
            recent_games,
        }))
    }
}

fn recent_rating_delta_text(
    row: &RatingHistoryEntry,
    rating_system: &CamaRatingSystem,
    openskill_system: &CamaOpenSkillSystem,
) -> String {
    let mut deltas = Vec::new();
    if let (Some(before), Some(after)) = (row.rating_before, row.rating) {
        deltas.push(format!(
            "G:{:+}",
            rating_system.rating_to_display(after) - rating_system.rating_to_display(before)
        ));
    }
    if let (Some(before), Some(after)) = (row.os_mu_before, row.os_mu_after) {
        deltas.push(format!(
            "O:{:+}",
            openskill_system.mu_to_display(after) - openskill_system.mu_to_display(before)
        ));
    }
    if deltas.is_empty() {
        "?".to_owned()
    } else {
        deltas.join(" ")
    }
}

fn rating_profile_lines(
    player: &Player,
    rating_system: &CamaRatingSystem,
    openskill_system: &CamaOpenSkillSystem,
    calibration_tier: &str,
    percentile: &str,
    openskill: Option<(f64, f64)>,
) -> Vec<String> {
    let rating_display = player.glicko_rating.map_or_else(
        || "N/A".to_owned(),
        |rating| rating_system.rating_to_display(rating).to_string(),
    );
    let rd = cap_glicko_rd(player.glicko_rd.unwrap_or(MAX_GLICKO_RD));
    let certainty = 100.0 - rating_system.get_rating_uncertainty_percentage(rd);
    let mut lines = vec![
        format!("**Rating:** {rating_display} | **RD:** {rd:.0} ({certainty:.0}% certain)"),
        format!("**Tier:** {calibration_tier} | **Percentile:** {percentile}"),
    ];
    if let Some((mu, sigma)) = openskill {
        lines.insert(
            1,
            format!(
                "**OpenSkill:** {} | **σ:** {sigma:.3} ({:.0}% certain)",
                openskill_system.mu_to_display(mu),
                openskill_system.certainty_percentage(sigma),
            ),
        );
    }
    if let Some(volatility) = player.glicko_volatility.filter(|value| *value != 0.0) {
        lines.push(format!("**Volatility:** {volatility:.3}"));
    }
    lines
}

fn add_performance_fields(
    mut embed: InteractionEmbed,
    calibration: &PlayerCalibration,
) -> InteractionEmbed {
    if calibration.matches_with_predictions.is_empty() {
        return embed;
    }
    let mut performance = format!(
        "**Actual Wins:** {} | **Expected:** {:.1}\n",
        calibration.actual_wins, calibration.expected_wins
    );
    if let Some(over) = calibration.overperformance {
        performance.push_str(&format!(
            "**Over/Under:** {} {over:+.1} wins",
            if over > 0.0 {
                "🔥"
            } else if over < 0.0 {
                "💀"
            } else {
                "➡️"
            }
        ));
    }
    embed = embed.field("📈 Performance", performance, true);
    let mut situational = Vec::new();
    if !calibration.favored_matches.is_empty() {
        situational.push(format!(
            "**When Favored (55%+):** {}/{} ({:.0}%)",
            calibration.favored_wins,
            calibration.favored_matches.len(),
            calibration.favored_wins as f64 / calibration.favored_matches.len() as f64 * 100.0,
        ));
    }
    if !calibration.underdog_matches.is_empty() {
        situational.push(format!(
            "**As Underdog (45%-):** {}/{} ({:.0}%)",
            calibration.underdog_wins,
            calibration.underdog_matches.len(),
            calibration.underdog_wins as f64 / calibration.underdog_matches.len() as f64 * 100.0,
        ));
    }
    if !situational.is_empty() {
        embed = embed.field("🎲 Situational", situational.join("\n"), true);
    }
    embed
}

fn trend_text(calibration: &PlayerCalibration, history_len: usize) -> String {
    let value = calibration
        .last_5_delta
        .or(calibration.openskill_last_5_delta)
        .unwrap_or_default();
    let mut parts = Vec::new();
    if let Some(delta) = calibration.last_5_delta {
        parts.push(format!("**G:** {delta:+.0}"));
    }
    if let Some(delta) = calibration.openskill_last_5_delta {
        parts.push(format!("**O:** {delta:+.0}"));
    }
    let mut text = format!(
        "{} {} over last {} games",
        if value > 0.0 {
            "📈"
        } else if value < 0.0 {
            "📉"
        } else {
            "➡️"
        },
        parts.join(" | "),
        history_len.min(5),
    );
    if calibration.streak != 0
        && let Some(kind) = calibration.streak_type
    {
        text.push_str(&format!(
            "\n{} Current: **{kind}{}** streak",
            if kind == "W" { "🔥" } else { "💀" },
            calibration.streak,
        ));
    }
    text
}

fn convergence_text(history: &[DetailedHistory]) -> Option<String> {
    if history.len() < 2 {
        return None;
    }
    let swings = history
        .iter()
        .take(5)
        .filter_map(|entry| {
            entry
                .insight
                .rating_before
                .zip(entry.insight.rating)
                .map(|(before, after)| (after - before).abs())
        })
        .collect::<Vec<_>>();
    let rd_changes = history
        .iter()
        .take(5)
        .filter_map(|entry| {
            entry
                .insight
                .rd_before
                .zip(entry.rd_after)
                .map(|(before, after)| after - before)
        })
        .collect::<Vec<_>>();
    if swings.is_empty() || rd_changes.is_empty() {
        return None;
    }
    let average = swings.iter().sum::<f64>() / swings.len() as f64;
    let total_rd = rd_changes.iter().sum::<f64>();
    let (trend, expectation) = if total_rd < -5.0 {
        (
            "📉 RD decreasing (converging)",
            "Expect smaller rating swings",
        )
    } else if total_rd > 5.0 {
        (
            "📈 RD increasing (uncertain)",
            "Expect larger rating swings",
        )
    } else {
        ("➡️ RD stable", "Rating swings should be consistent")
    };
    Some(format!(
        "{trend}\nAvg swing: **±{average:.0}** per game\n*{expectation}*"
    ))
}

fn highlight_lines(calibration: &PlayerCalibration) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some((row, probability)) = calibration.upsets.first() {
        lines.push(format!(
            "🔥 **Best Upset:** Won with {:.0}% chance (Match #{})",
            probability * 100.0,
            row.match_id.unwrap_or_default(),
        ));
    }
    if let Some((row, probability)) = calibration.chokes.first() {
        lines.push(format!(
            "💀 **Worst Choke:** Lost with {:.0}% chance (Match #{})",
            probability * 100.0,
            row.match_id.unwrap_or_default(),
        ));
    }
    lines
}

fn user_mentions_from_embed(embeds: &[InteractionEmbed]) -> InteractionAllowedMentions {
    let mut users = std::collections::BTreeSet::new();
    for embed in embeds {
        for text in embed
            .title
            .iter()
            .chain(embed.description.iter())
            .chain(embed.fields.iter().map(|field| &field.value))
        {
            let mut rest = text.as_str();
            while let Some(start) = rest.find("<@") {
                rest = &rest[start + 2..];
                let digits = rest
                    .trim_start_matches('!')
                    .chars()
                    .take_while(|character| character.is_ascii_digit())
                    .collect::<String>();
                if let Ok(id) = digits.parse::<u64>() {
                    users.insert(id);
                }
                if let Some(end) = rest.find('>') {
                    rest = &rest[end + 1..];
                } else {
                    break;
                }
            }
        }
    }
    if users.is_empty() {
        InteractionAllowedMentions::None
    } else {
        InteractionAllowedMentions::Users(users.into_iter().take(100).collect())
    }
}

trait CalibrationHeroCatalog: Send + Sync {
    fn is_support(&self, hero_id: i64) -> Result<bool, String>;
}

#[derive(Clone, Debug)]
struct HeroCatalogEntry {
    roles: String,
}

struct DotabaseHeroCatalog {
    path: PathBuf,
    cache: Mutex<Option<Result<BTreeMap<i64, HeroCatalogEntry>, String>>>,
}

impl DotabaseHeroCatalog {
    fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            cache: Mutex::new(None),
        }
    }

    fn load(&self) -> Result<BTreeMap<i64, HeroCatalogEntry>, String> {
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("could not open bundled Dotabase catalog: {error}"))?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(|error| error.to_string())?;
        let mut statement = connection
            .prepare("SELECT id,COALESCE(roles,'') FROM heroes")
            .map_err(|error| error.to_string())?;
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, HeroCatalogEntry { roles: row.get(1)? }))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(|error| error.to_string())
    }
}

impl CalibrationHeroCatalog for DotabaseHeroCatalog {
    fn is_support(&self, hero_id: i64) -> Result<bool, String> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| "Dotabase hero cache lock poisoned".to_owned())?;
        if cache.is_none() {
            *cache = Some(self.load());
        }
        cache
            .as_ref()
            .expect("cache initialized")
            .as_ref()
            .map_err(Clone::clone)
            .map(|heroes| {
                heroes
                    .get(&hero_id)
                    .is_some_and(|hero| hero.roles.split('|').any(|role| role == "Support"))
            })
    }
}

fn hero_short_name(hero_id: i64) -> String {
    cama_app::hero_lookup::hero_short_name(hero_id)
}

#[cfg(all(test, feature = "runtime-test-match"))]
mod configured_model_tests {
    use super::*;

    fn non_default_config() -> ApplicationConfig {
        ApplicationConfig::from_lookup(|name| {
            Some(
                match name {
                    "DISCORD_BOT_TOKEN" => "test-token",
                    "CALIBRATION_RD_THRESHOLD" => "111",
                    "INITIAL_GLICKO_RD" => "222",
                    "MAX_RATING_SWING_PER_GAME" => "333",
                    "BASE_RATING_DELTA_MULTIPLIER" => "0.77",
                    "MAX_RD_CONTRACTION_PER_GAME" => "0.044",
                    "NEW_PLAYER_MMR_DISCOUNT" => "1000",
                    "RD_DECAY_CONSTANT" => "88",
                    "RD_DECAY_GRACE_PERIOD_DAYS" => "13",
                    "STREAK_THRESHOLD" => "9",
                    "STREAK_MULTIPLIER_PER_GAME" => "0.42",
                    "OPENSKILL_CALIBRATION_SIGMA_THRESHOLD" => "3",
                    "OPENSKILL_PERFORMANCE_STRENGTH" => "0.25",
                    "OPENSKILL_SIGMA_DECAY_GRACE_PERIOD_DAYS" => "11",
                    "OPENSKILL_SIGMA_DECAY_PER_WEEK" => "1.25",
                    "OPENSKILL_WIN_PROBABILITY_TEMPERATURE" => "2",
                    _ => return None,
                }
                .to_owned(),
            )
        })
        .expect("non-default calibration configuration")
    }

    #[test]
    fn non_default_application_config_drives_calibration_models_and_analytics() {
        let sources = CalibrationDataSources::new("unused.db", &non_default_config());
        let rating = sources.rating_system();
        assert_eq!(rating.initial_rd, 222.0);
        assert_eq!(rating.initial_volatility, 0.06);
        assert_eq!(rating.config.calibration_rd_threshold, 111.0);
        assert_eq!(rating.config.initial_rd, 222.0);
        assert_eq!(rating.config.max_rating_swing_per_game, 333.0);
        assert_eq!(rating.config.base_rating_delta_multiplier, 0.77);
        assert_eq!(rating.config.max_rd_contraction_per_game, 0.044);
        assert_eq!(rating.config.new_player_mmr_discount, 1_000);
        assert_eq!(rating.config.rd_decay_constant, 88.0);
        assert_eq!(rating.config.rd_decay_grace_period_days, 13);
        assert_eq!(rating.config.streak_threshold, 9);
        assert_eq!(rating.config.streak_multiplier_per_game, 0.42);

        let openskill = sources.openskill_system().expect("configured OpenSkill");
        let os_config = openskill.config();
        assert_eq!(os_config.calibration_threshold, 3.0);
        assert_eq!(os_config.performance_strength, 0.25);
        assert_eq!(os_config.streak_threshold, 9);
        assert_eq!(os_config.streak_multiplier_per_game, 0.42);
        assert_eq!(os_config.sigma_decay_grace_period_days, 11);
        assert_eq!(os_config.sigma_decay_per_week, 1.25);
        assert_eq!(os_config.win_probability_temperature, 2.0);

        let player = Player {
            name: "Configured".to_owned(),
            initial_mmr: Some(4_000),
            glicko_rating: Some(1_700.0),
            ..Player::default()
        };
        let history = [RatingHistoryEntry {
            os_mu_before: Some(40.0),
            os_mu_after: Some(41.0),
            os_sigma_before: Some(3.5),
            ..RatingHistoryEntry::default()
        }];
        let mut stats = compute_calibration_stats(std::slice::from_ref(&player), 0, &[], &history);
        apply_configured_calibration_stats(
            &mut stats,
            &[player],
            &history,
            rating,
            Some(openskill),
        )
        .expect("configured analytics");
        // 4,000 MMR - 1,000 discount => 3,000 seed MMR => 750 rating.
        assert_eq!(stats.avg_drift, Some(950.0));
        assert_eq!(stats.median_drift, Some(950.0));
        assert_eq!(stats.openskill_rating_stability.calibrated_count, 0);
        assert_eq!(stats.openskill_rating_stability.uncalibrated_count, 1);

        let stronger = [(45.0, 4.0); 5];
        let weaker = [(25.0, 4.0); 5];
        let configured_probability =
            configured_openskill_probability(openskill, &stronger, &weaker)
                .expect("configured probability");
        let default_probability =
            configured_openskill_probability(&CamaOpenSkillSystem::new(), &stronger, &weaker)
                .expect("default probability");
        assert!(configured_probability > default_probability);
    }

    struct BrokenHeroCatalog;

    impl CalibrationHeroCatalog for BrokenHeroCatalog {
        fn is_support(&self, _hero_id: i64) -> Result<bool, String> {
            Err("catalog unavailable".to_owned())
        }
    }

    #[test]
    fn optional_hero_catalog_failure_never_takes_calibration_down() {
        let mut sources = CalibrationDataSources::new("unused.db", &non_default_config());
        sources.heroes = Arc::new(BrokenHeroCatalog);
        let player = Player {
            preferred_roles: Some(vec!["4".to_owned(), "5".to_owned()]),
            ..Player::default()
        };
        let mismatch = sources
            .role_mismatch(
                &player,
                &[HeroRoleCount {
                    hero_id: 999,
                    games: 5,
                }],
            )
            .expect("catalog error is fail-soft");
        assert_eq!(
            mismatch.as_deref(),
            Some("⚠️ Prefers Support but plays 100% Core heroes")
        );
    }

    #[test]
    fn test_calibration_stats_with_empty_data() {
        let stats = compute_calibration_stats(&[], 0, &[], &[]);

        assert!(!stats.rating_buckets.is_empty());
        assert!(!stats.rd_tiers.is_empty());
        assert_eq!(stats.prediction_quality.count, 0);
        assert_eq!(stats.rating_movement.count, 0);
        assert_eq!(stats.avg_rating, None);
        assert_eq!(stats.avg_rd, None);
    }

    #[test]
    fn test_calibration_stats_with_single_player() {
        let player = Player {
            name: "TestPlayer".to_owned(),
            glicko_rating: Some(1_200.0),
            glicko_rd: Some(100.0),
            glicko_volatility: Some(0.06),
            wins: 5,
            losses: 3,
            initial_mmr: Some(3_500),
            discord_id: Some(12_345),
            ..Player::default()
        };
        let stats = compute_calibration_stats(&[player], 8, &[], &[]);

        assert_eq!(stats.rating_bucket_count("Divine"), Some(1));
        assert_eq!(stats.rd_tier_count("Settling"), Some(1));
        assert_eq!(stats.avg_rating, Some(1_200.0));
        assert_eq!(stats.avg_rd, Some(100.0));
    }

    #[test]
    fn test_unrated_player_rating_profile_not_duplicated() {
        let player = Player {
            name: "Unrated".to_owned(),
            glicko_rating: Some(1_500.0),
            glicko_rd: Some(350.0),
            glicko_volatility: None,
            os_mu: Some(30.0),
            os_sigma: Some(3.0),
            initial_mmr: Some(3_000),
            discord_id: Some(777),
            ..Player::default()
        };
        let rating = CamaRatingSystem::default();
        let openskill = CamaOpenSkillSystem::new();
        let value = rating_profile_lines(
            &player,
            &rating,
            &openskill,
            "Fresh",
            "N/A",
            Some((30.0, 3.0)),
        )
        .join("\n");

        assert_eq!(value.matches("**Rating:**").count(), 1);
        assert_eq!(value.matches("**Tier:**").count(), 1);
        assert!(value.contains("**RD:** 250"));
        assert!(value.contains("**OpenSkill:** 250"));
        assert!(value.contains("64% certain"));
    }

    #[test]
    fn test_recent_match_shows_glicko_and_openskill_display_deltas() {
        let row = RatingHistoryEntry {
            rating_before: Some(1_500.0),
            rating: Some(1_510.0),
            os_mu_before: Some(25.0),
            os_mu_after: Some(26.0),
            ..RatingHistoryEntry::default()
        };

        assert_eq!(
            recent_rating_delta_text(
                &row,
                &CamaRatingSystem::default(),
                &CamaOpenSkillSystem::new(),
            ),
            "G:+10 O:+50"
        );
    }

    #[test]
    fn test_formats_both_swings_samples_and_expectations() {
        let stats = LobbyTypeStats {
            lobby_type: "shuffle".to_owned(),
            avg_swing: None,
            glicko_avg_swing: Some(10.0),
            openskill_avg_swing: Some(25.0),
            glicko_samples: 40,
            openskill_samples: 30,
            games: 12,
            actual_win_rate: Some(0.6),
            expected_win_rate: Some(0.55),
            openskill_expected_win_rate: Some(0.52),
        };
        let line = format_lobby_stats_line(&stats, "🎲", "Shuffle", false);

        assert!(line.contains("G ±10.0 (n=40)"));
        assert!(line.contains("O ±25.0 (n=30)"));
        assert!(line.contains("Radiant actual 60%"));
        assert!(line.contains("G exp 55%"));
        assert!(line.contains("O exp 52%"));
    }

    #[test]
    fn test_legacy_avg_swing_falls_back_to_glicko() {
        let stats = LobbyTypeStats {
            lobby_type: "shuffle".to_owned(),
            avg_swing: Some(12.5),
            glicko_avg_swing: None,
            openskill_avg_swing: None,
            glicko_samples: 0,
            openskill_samples: 0,
            games: 8,
            actual_win_rate: None,
            expected_win_rate: None,
            openskill_expected_win_rate: None,
        };

        assert_eq!(format_lobby_swing_summary(&stats), "G ±12.5 (n=8)");
        let line = format_lobby_stats_line(&stats, "🎲", "Shuffle", false);
        assert!(line.contains("G exp n/a"));
        assert!(line.contains("O exp n/a"));
    }

    #[test]
    fn test_compares_draft_and_shuffle_for_each_system() {
        let stats = |lobby_type: &str, glicko: f64, openskill: f64| LobbyTypeStats {
            lobby_type: lobby_type.to_owned(),
            avg_swing: None,
            glicko_avg_swing: Some(glicko),
            openskill_avg_swing: Some(openskill),
            glicko_samples: 1,
            openskill_samples: 1,
            games: 1,
            actual_win_rate: None,
            expected_win_rate: None,
            openskill_expected_win_rate: None,
        };
        let shuffle = stats("shuffle", 10.0, 20.0);
        let draft = stats("draft", 12.5, 15.0);

        assert_eq!(
            format_lobby_comparisons(Some(&shuffle), Some(&draft), false),
            [
                "*G: Draft swings are 25% larger than Shuffle*",
                "*O: Shuffle swings are 25% larger than Draft*",
            ]
        );
    }
}
