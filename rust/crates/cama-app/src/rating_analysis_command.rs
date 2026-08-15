//! Discord-independent `/ratinganalysis` command orchestration.
//!
//! This module preserves Python's command gating, service call order, report
//! formatting, chart attachment decisions, and per-player OpenSkill
//! calculations through typed ports. Persistence and Discord remain adapter
//! responsibilities.

use cama_domain::embed_safety::EmbedModel;
use cama_domain::openskill::CamaOpenSkillSystem;

use crate::rating_comparison_service::RatingComparisonResult;

pub type DiscordId = i64;
pub type GuildId = i64;

pub const TREND_MINIMUM_MATCHES: usize = 20;
pub const PLAYER_HISTORY_LIMIT: usize = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberTarget {
    pub discord_id: DiscordId,
    pub display_name: String,
}

impl MemberTarget {
    #[must_use]
    pub fn new(discord_id: DiscordId, display_name: impl Into<String>) -> Self {
        Self {
            discord_id,
            display_name: display_name.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RatingAnalysisInput {
    pub admin_allowed: bool,
    pub action: String,
    pub guild_id: Option<GuildId>,
    pub invoker: MemberTarget,
    pub selected_user: Option<MemberTarget>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryRoute {
    InitialResponse,
    Followup,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentPlan {
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivery {
    pub route: DeliveryRoute,
    pub content: Option<String>,
    pub embed: Option<EmbedModel>,
    pub attachment: Option<AttachmentPlan>,
    /// `None` preserves an omitted `ephemeral` keyword on the Python followup.
    pub ephemeral: Option<bool>,
}

impl Delivery {
    fn initial(content: impl Into<String>, ephemeral: bool) -> Self {
        Self {
            route: DeliveryRoute::InitialResponse,
            content: Some(content.into()),
            embed: None,
            attachment: None,
            ephemeral: Some(ephemeral),
        }
    }

    fn followup_content(content: impl Into<String>, ephemeral: Option<bool>) -> Self {
        Self {
            route: DeliveryRoute::Followup,
            content: Some(content.into()),
            embed: None,
            attachment: None,
            ephemeral,
        }
    }

    fn followup_embed(
        embed: EmbedModel,
        attachment: Option<AttachmentPlan>,
        ephemeral: Option<bool>,
    ) -> Self {
        Self {
            route: DeliveryRoute::Followup,
            content: None,
            embed: Some(embed),
            attachment,
            ephemeral,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommandPlan {
    /// `Some(true)` means the live adapter must defer ephemerally before work.
    pub deferred_ephemeral: Option<bool>,
    pub deliveries: Vec<Delivery>,
}

impl CommandPlan {
    fn deferred() -> Self {
        Self {
            deferred_ephemeral: Some(true),
            deliveries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackfillResult {
    pub matches_processed: usize,
    pub total_matches: Option<usize>,
    pub players_updated: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerRatingRecord {
    pub glicko_rating: Option<f64>,
    pub glicko_rd: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerOpenSkillHistory {
    pub os_mu_before: f64,
    pub os_mu_after: f64,
    pub os_sigma_before: f64,
    pub os_sigma_after: f64,
    pub won: Option<bool>,
    pub fantasy_weight: Option<f64>,
    pub streak_length: Option<i64>,
    pub streak_multiplier: Option<f64>,
}

pub trait RatingAnalysisMatchPort {
    fn backfill_openskill_ratings(
        &mut self,
        guild_id: Option<GuildId>,
        reset_first: bool,
    ) -> Result<BackfillResult, String>;

    fn player_openskill_history(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
        limit: usize,
    ) -> Vec<PlayerOpenSkillHistory>;
}

pub trait RatingAnalysisPlayerPort {
    fn player(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
    ) -> Option<PlayerRatingRecord>;

    fn openskill_rating(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
    ) -> Option<(f64, f64)>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum ComparisonSummaryPayload {
    Report(RatingComparisonResult),
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalibrationPoint {
    pub predicted: f64,
    pub actual_rate: f64,
    pub count: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalibrationCurveData {
    pub glicko: Vec<CalibrationPoint>,
    pub openskill: Vec<CalibrationPoint>,
    pub perfect_line: Vec<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CalibrationCurvePayload {
    Curves(CalibrationCurveData),
    Error(String),
}

pub trait RatingAnalysisComparisonPort {
    fn comparison_summary(
        &mut self,
        guild_id: Option<GuildId>,
    ) -> Result<ComparisonSummaryPayload, String>;

    fn calibration_curve_data(
        &mut self,
        guild_id: Option<GuildId>,
    ) -> Result<CalibrationCurvePayload, String>;
}

/// Bitmap generation remains an adapter responsibility. The command policy
/// owns the exact input and attachment filename, while tests can use a tiny
/// deterministic PNG stand-in just like the canonical Python suite.
pub trait RatingAnalysisDrawingPort {
    fn rating_comparison_chart(
        &mut self,
        comparison: &RatingComparisonResult,
    ) -> Result<Vec<u8>, String>;

    fn calibration_curve_chart(&mut self, curves: &CalibrationCurveData)
    -> Result<Vec<u8>, String>;

    fn prediction_over_time_chart(
        &mut self,
        comparison: &RatingComparisonResult,
        window: usize,
    ) -> Result<Vec<u8>, String>;
}

/// The canonical OpenSkill helper is explicit so a test can prove command
/// calibration is delegated instead of reimplementing the threshold locally.
pub trait RatingAnalysisMath {
    fn ordinal(&self, mu: f64, sigma: f64) -> f64;
    fn mu_to_display(&self, mu: f64) -> i32;
    fn is_calibrated(&self, sigma: f64) -> bool;
    fn calibration_threshold(&self) -> f64;
}

impl RatingAnalysisMath for CamaOpenSkillSystem {
    fn ordinal(&self, mu: f64, sigma: f64) -> f64 {
        self.ordinal(mu, sigma, 3.0)
    }

    fn mu_to_display(&self, mu: f64) -> i32 {
        self.mu_to_display(mu)
    }

    fn is_calibrated(&self, sigma: f64) -> bool {
        self.is_calibrated(sigma)
    }

    fn calibration_threshold(&self) -> f64 {
        self.config().calibration_threshold
    }
}

pub struct RatingAnalysisCommand<Matches, Players, Comparisons, Drawing, Math> {
    matches: Matches,
    players: Players,
    comparisons: Option<Comparisons>,
    drawing: Drawing,
    math: Math,
}

impl<Matches, Players, Comparisons, Drawing, Math>
    RatingAnalysisCommand<Matches, Players, Comparisons, Drawing, Math>
where
    Matches: RatingAnalysisMatchPort,
    Players: RatingAnalysisPlayerPort,
    Comparisons: RatingAnalysisComparisonPort,
    Drawing: RatingAnalysisDrawingPort,
    Math: RatingAnalysisMath,
{
    #[must_use]
    pub const fn new(
        matches: Matches,
        players: Players,
        comparisons: Option<Comparisons>,
        drawing: Drawing,
        math: Math,
    ) -> Self {
        Self {
            matches,
            players,
            comparisons,
            drawing,
            math,
        }
    }

    #[must_use]
    pub const fn match_port(&self) -> &Matches {
        &self.matches
    }

    #[must_use]
    pub const fn player_port(&self) -> &Players {
        &self.players
    }

    #[must_use]
    pub const fn comparison_port(&self) -> Option<&Comparisons> {
        self.comparisons.as_ref()
    }

    #[must_use]
    pub const fn drawing_port(&self) -> &Drawing {
        &self.drawing
    }

    #[must_use]
    pub const fn math_port(&self) -> &Math {
        &self.math
    }

    /// Produce an ordered Discord delivery plan for one callback invocation.
    pub fn execute(&mut self, input: &RatingAnalysisInput) -> CommandPlan {
        if !input.admin_allowed {
            return CommandPlan {
                deferred_ephemeral: None,
                deliveries: vec![Delivery::initial("This command is admin-only.", true)],
            };
        }

        match input.action.as_str() {
            "backfill" => self.handle_backfill(input.guild_id),
            "compare" => self.handle_compare(input.guild_id),
            "calibration" => self.handle_calibration(input.guild_id),
            "trend" => self.handle_trend(input.guild_id),
            "player" => {
                let target = input.selected_user.as_ref().unwrap_or(&input.invoker);
                self.handle_player(input.guild_id, target)
            }
            action => CommandPlan {
                deferred_ephemeral: None,
                deliveries: vec![Delivery::initial(format!("Unknown action: {action}"), true)],
            },
        }
    }

    fn handle_backfill(&mut self, guild_id: Option<GuildId>) -> CommandPlan {
        let mut plan = CommandPlan::deferred();
        plan.deliveries.push(Delivery::followup_content(
            "Starting OpenSkill backfill... This may take a while.",
            Some(true),
        ));

        let result = match self.matches.backfill_openskill_ratings(guild_id, true) {
            Ok(result) => result,
            Err(error) => {
                plan.deliveries.push(Delivery::followup_content(
                    format!("Backfill failed: {error}"),
                    Some(true),
                ));
                return plan;
            }
        };

        let mut embed = EmbedModel {
            title: Some("OpenSkill Backfill Complete".to_owned()),
            ..EmbedModel::default()
        };
        let total = result
            .total_matches
            .map_or_else(|| "?".to_owned(), |total| total.to_string());
        embed.add_field(
            "Matches Processed",
            format!("{}/{total}", result.matches_processed),
            true,
        );
        embed.add_field("Players Updated", result.players_updated.to_string(), true);
        if !result.errors.is_empty() {
            let mut error_text = result.errors.iter().take(5).cloned().collect::<Vec<_>>();
            if result.errors.len() > 5 {
                error_text.push(format!("...and {} more", result.errors.len() - 5));
            }
            embed.add_field("Errors", error_text.join("\n"), false);
        }
        plan.deliveries
            .push(Delivery::followup_embed(embed, None, Some(true)));
        plan
    }

    fn handle_compare(&mut self, guild_id: Option<GuildId>) -> CommandPlan {
        let mut plan = CommandPlan::deferred();
        let Some(comparisons) = self.comparisons.as_mut() else {
            plan.deliveries.push(Delivery::followup_content(
                "Rating comparison service not available.",
                None,
            ));
            return plan;
        };

        let payload = match comparisons.comparison_summary(guild_id) {
            Ok(payload) => payload,
            Err(error) => {
                plan.deliveries.push(Delivery::followup_content(
                    format!("Comparison failed: {error}"),
                    None,
                ));
                return plan;
            }
        };
        let comparison = match payload {
            ComparisonSummaryPayload::Report(comparison) => comparison,
            ComparisonSummaryPayload::Error(error) => {
                plan.deliveries.push(Delivery::followup_content(
                    format!("Comparison unavailable: {error}"),
                    None,
                ));
                return plan;
            }
        };

        let embed = comparison_embed(&comparison);
        let attachment = self
            .drawing
            .rating_comparison_chart(&comparison)
            .ok()
            .map(|bytes| AttachmentPlan {
                filename: "comparison.png".to_owned(),
                bytes,
            });
        plan.deliveries
            .push(Delivery::followup_embed(embed, attachment, None));
        plan
    }

    fn handle_calibration(&mut self, guild_id: Option<GuildId>) -> CommandPlan {
        let mut plan = CommandPlan::deferred();
        let Some(comparisons) = self.comparisons.as_mut() else {
            plan.deliveries.push(Delivery::followup_content(
                "Rating comparison service not available.",
                None,
            ));
            return plan;
        };

        let payload = match comparisons.calibration_curve_data(guild_id) {
            Ok(payload) => payload,
            Err(_) => {
                plan.deliveries.push(Delivery::followup_content(
                    friendly_error("generate the calibration data"),
                    None,
                ));
                return plan;
            }
        };
        let curves = match payload {
            CalibrationCurvePayload::Curves(curves) => curves,
            CalibrationCurvePayload::Error(error) => {
                plan.deliveries.push(Delivery::followup_content(
                    format!("Calibration unavailable: {error}"),
                    None,
                ));
                return plan;
            }
        };

        let embed = EmbedModel {
            title: Some("Rating System Calibration".to_owned()),
            description: Some(
                "Calibration curves show how well predicted probabilities match actual outcomes.\n\
                 A perfectly calibrated system would have all points on the diagonal line."
                    .to_owned(),
            ),
            ..EmbedModel::default()
        };
        let attachment = match self.drawing.calibration_curve_chart(&curves) {
            Ok(bytes) => Some(AttachmentPlan {
                filename: "calibration.png".to_owned(),
                bytes,
            }),
            Err(_) => {
                plan.deliveries.push(Delivery::followup_content(
                    friendly_error("generate the calibration chart"),
                    None,
                ));
                return plan;
            }
        };
        plan.deliveries
            .push(Delivery::followup_embed(embed, attachment, None));
        plan
    }

    fn handle_trend(&mut self, guild_id: Option<GuildId>) -> CommandPlan {
        let mut plan = CommandPlan::deferred();
        let Some(comparisons) = self.comparisons.as_mut() else {
            plan.deliveries.push(Delivery::followup_content(
                "Rating comparison service not available.",
                None,
            ));
            return plan;
        };

        let payload = match comparisons.comparison_summary(guild_id) {
            Ok(payload) => payload,
            Err(_) => {
                plan.deliveries.push(Delivery::followup_content(
                    friendly_error("analyze rating trends"),
                    None,
                ));
                return plan;
            }
        };
        let comparison = match payload {
            ComparisonSummaryPayload::Report(comparison) => comparison,
            ComparisonSummaryPayload::Error(error) => {
                plan.deliveries.push(Delivery::followup_content(
                    format!("Trend analysis unavailable: {error}"),
                    None,
                ));
                return plan;
            }
        };
        if comparison.match_data.len() < TREND_MINIMUM_MATCHES {
            plan.deliveries.push(Delivery::followup_content(
                "Need at least 20 matches for trend analysis.",
                None,
            ));
            return plan;
        }

        let embed = EmbedModel {
            title: Some("Prediction Accuracy Over Time".to_owned()),
            description: Some(format!(
                "Rolling accuracy of predictions across {} matches.\n\
                 Higher values = better predictions.",
                comparison.match_data.len()
            )),
            ..EmbedModel::default()
        };
        let attachment = match self
            .drawing
            .prediction_over_time_chart(&comparison, TREND_MINIMUM_MATCHES)
        {
            Ok(bytes) => Some(AttachmentPlan {
                filename: "trend.png".to_owned(),
                bytes,
            }),
            Err(_) => {
                plan.deliveries.push(Delivery::followup_content(
                    friendly_error("generate the trend chart"),
                    None,
                ));
                return plan;
            }
        };
        plan.deliveries
            .push(Delivery::followup_embed(embed, attachment, None));
        plan
    }

    fn handle_player(&mut self, guild_id: Option<GuildId>, target: &MemberTarget) -> CommandPlan {
        let mut plan = CommandPlan::deferred();
        let Some(player) = self.players.player(target.discord_id, guild_id) else {
            plan.deliveries.push(Delivery::followup_content(
                format!("{} is not registered.", target.display_name),
                None,
            ));
            return plan;
        };

        let os_data = self.players.openskill_rating(target.discord_id, guild_id);
        let mut embed = EmbedModel {
            title: Some(format!("OpenSkill Rating: {}", target.display_name)),
            ..EmbedModel::default()
        };
        if let Some((mu, sigma)) = os_data {
            let ordinal = self.math.ordinal(mu, sigma);
            let normalized_rating = self.math.mu_to_display(mu);
            let is_calibrated = self.math.is_calibrated(sigma);

            embed.add_field(
                "Normalized Rating",
                format!("**{normalized_rating}**"),
                true,
            );
            if let (Some(glicko_rating), Some(glicko_rd)) = (player.glicko_rating, player.glicko_rd)
            {
                embed.add_field(
                    "Glicko-2 Rating",
                    format!("**{glicko_rating:.0}** ± {glicko_rd:.0}"),
                    true,
                );
            }
            embed.add_field(
                "Calibrated",
                if is_calibrated {
                    "✓ Yes".to_owned()
                } else {
                    format!("No (need σ ≤ {:.1})", self.math.calibration_threshold())
                },
                true,
            );
            embed.add_field("Skill (μ)", format!("**{mu:.2}**"), true);
            embed.add_field("Uncertainty (σ)", format!("**{sigma:.3}**"), true);
            embed.add_field("Ordinal (μ-3σ)", format!("**{ordinal:.2}**"), true);

            let history = self.matches.player_openskill_history(
                target.discord_id,
                guild_id,
                PLAYER_HISTORY_LIMIT,
            );
            if !history.is_empty() {
                embed.add_field(
                    "Recent OpenSkill Changes",
                    history
                        .iter()
                        .map(|entry| format_history_line(&self.math, entry))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    false,
                );
            }
        } else {
            embed.description = Some(
                "No OpenSkill rating data yet.\n\n\
                 OpenSkill ratings are calculated from enriched matches with fantasy data. \
                 Use `/ratinganalysis backfill` to calculate historical ratings."
                    .to_owned(),
            );
        }

        plan.deliveries
            .push(Delivery::followup_embed(embed, None, None));
        plan
    }
}

fn comparison_embed(comparison: &RatingComparisonResult) -> EmbedModel {
    let glicko = &comparison.glicko;
    let openskill = &comparison.openskill;
    let brier_winner = if glicko.brier_score < openskill.brier_score {
        "Glicko-2"
    } else {
        "OpenSkill"
    };
    let accuracy_winner = if glicko.accuracy > openskill.accuracy {
        "Glicko-2"
    } else {
        "OpenSkill"
    };

    let mut embed = EmbedModel {
        title: Some("Rating System Comparison".to_owned()),
        description: Some(format!(
            "Analysis of {} matches",
            comparison.matches_analyzed
        )),
        ..EmbedModel::default()
    };
    embed.add_field(
        "Brier Score (Lower = Better)",
        format!(
            "Glicko-2: **{:.4}** \nOpenSkill: **{:.4}** ",
            glicko.brier_score, openskill.brier_score
        ),
        true,
    );
    embed.add_field(
        "Prediction Accuracy",
        format!(
            "Glicko-2: **{:.1}%**\nOpenSkill: **{:.1}%**",
            glicko.accuracy * 100.0,
            openskill.accuracy * 100.0
        ),
        true,
    );
    embed.add_field(
        "Log Loss (Lower = Better)",
        format!(
            "Glicko-2: **{:.4}**\nOpenSkill: **{:.4}**",
            glicko.log_loss, openskill.log_loss
        ),
        true,
    );

    let overall = if brier_winner == accuracy_winner {
        format!("**{brier_winner}** performs better overall")
    } else {
        "Mixed results - neither system clearly dominates".to_owned()
    };
    let brier_difference_percent =
        (glicko.brier_score - openskill.brier_score).abs() / glicko.brier_score.max(0.001) * 100.0;
    let difference = if brier_difference_percent < 5.0 {
        "Difference is marginal (<5%)".to_owned()
    } else if brier_difference_percent < 15.0 {
        format!("Moderate difference ({brier_difference_percent:.1}%)")
    } else {
        format!("Significant difference ({brier_difference_percent:.1}%)")
    };
    embed.add_field("Summary", format!("{overall}\n{difference}"), false);
    embed
}

fn format_history_line<Math: RatingAnalysisMath>(
    math: &Math,
    entry: &PlayerOpenSkillHistory,
) -> String {
    let display_change =
        math.mu_to_display(entry.os_mu_after) - math.mu_to_display(entry.os_mu_before);
    let sigma_change = entry.os_sigma_after - entry.os_sigma_before;
    let result = match entry.won {
        Some(true) => "🏆",
        Some(false) => "❌",
        None => "•",
    };
    let weight = entry
        .fantasy_weight
        .map_or_else(String::new, |weight| format!(" (perf×{weight:.3})"));
    let streak = match (entry.streak_length, entry.streak_multiplier) {
        (Some(length), Some(multiplier)) if multiplier > 1.0 => {
            let outcome = match entry.won {
                Some(true) => "W",
                Some(false) => "L",
                None => "",
            };
            format!(" (streak {outcome}{length}×{multiplier:.2})")
        }
        _ => String::new(),
    };
    format!("{result} Rating: {display_change:+}, σ: {sigma_change:+.3}{weight}{streak}")
}

fn friendly_error(action: &str) -> String {
    format!("❌ Couldn't {action} right now — try again in a moment.")
}

#[cfg(test)]
mod tests;
