//! Production Discord provider for Python's `/ratinganalysis` command.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cama_app::rating_analysis_command::{
    BackfillResult, CalibrationCurveData, CalibrationCurvePayload, CalibrationPoint, CommandPlan,
    ComparisonSummaryPayload, Delivery, DeliveryRoute, MemberTarget, PlayerOpenSkillHistory,
    PlayerRatingRecord, RatingAnalysisCommand, RatingAnalysisComparisonPort, RatingAnalysisInput,
    RatingAnalysisMatchPort, RatingAnalysisPlayerPort,
};
use cama_app::rating_analysis_media::NativeRatingAnalysisDrawing;
use cama_app::rating_comparison_service::{
    CalibrationBucket, CurrentOpenSkillPredictionPort, HistoricalOpenSkillRatings,
    HistoricalRatingMatch, RatingComparisonMatchPort, RatingComparisonService,
};
use cama_db::rating_analysis::{
    RatingAnalysisHistoricalMatch, RatingAnalysisHistoricalRatings, RatingAnalysisPlayer,
    RatingAnalysisRepository,
};
use cama_domain::openskill::{CamaOpenSkillSystem, OpenSkillError, Rating};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use thiserror::Error;
use tracing::{error, warn};

use crate::application_config::ApplicationConfig;
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionAttachment,
    InteractionEmbed, InteractionHandler, InteractionHandlerError, InteractionOption,
    InteractionRequest, InteractionResponder, InteractionResponse, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};

const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const GUILD_ONLY: &str = "This command can only be used in a server.";
const ADMIN_ONLY: &str = "This command is admin-only.";
const STARTING_BACKFILL: &str = "Starting OpenSkill backfill... This may take a while.";
const BLUE: u32 = 0x34_98_db;
const GREEN: u32 = 0x57_f2_87;

#[derive(Debug, Error)]
pub enum RatingAnalysisProviderBuildError {
    #[error("invalid OpenSkill runtime configuration: {0}")]
    OpenSkill(#[from] OpenSkillError),
    #[error("invalid Glicko runtime configuration: {0}")]
    InvalidRatingConfig(&'static str),
}

#[derive(Clone)]
pub struct RatingAnalysisRegistrationProvider {
    handler: Arc<RatingAnalysisHandler>,
}

impl RatingAnalysisRegistrationProvider {
    /// Compose the complete Python-off provider over the admitted SQLite file
    /// and the exact OpenSkill settings captured in `ApplicationConfig`.
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
    ) -> Result<Self, RatingAnalysisProviderBuildError> {
        let openskill = CamaOpenSkillSystem::with_config(config.migration.openskill.clone())?;
        // `with_config` validates update parameters; force validation of the
        // probability temperature before accepting live traffic as well.
        openskill.calibrate_win_probability(0.5, None)?;
        let rating_system = config
            .glicko_rating_system()
            .map_err(RatingAnalysisProviderBuildError::InvalidRatingConfig)?;
        let repository = RatingAnalysisRepository::new(
            database_path,
            openskill.clone(),
            rating_system,
            config.migration.new_player_mmr_discount,
        );
        Ok(Self::with_worker(
            config.identities.admin_user_ids.iter().copied().collect(),
            Arc::new(ProductionRatingAnalysisWorker {
                repository,
                openskill,
            }),
        ))
    }

    fn with_worker(admin_user_ids: BTreeSet<i64>, worker: Arc<dyn RatingAnalysisWorkPort>) -> Self {
        Self {
            handler: Arc::new(RatingAnalysisHandler {
                admin_user_ids,
                worker,
            }),
        }
    }
}

impl RegistrationProvider for RatingAnalysisRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "ratinganalysis".to_owned(),
            description: "Analyze and compare rating systems (Admin only)".to_owned(),
            options: vec![
                action_option(),
                CommandOptionSpec::new(
                    "user",
                    "Optional: Analyze specific player's rating data",
                    CommandOptionKind::User,
                ),
            ],
            handler: self.handler.clone(),
        })
    }
}

fn action_option() -> CommandOptionSpec {
    let mut option =
        CommandOptionSpec::new("action", "Action to perform", CommandOptionKind::String)
            .required(true);
    option.choices = [
        ("compare - Show Glicko vs OpenSkill comparison", "compare"),
        ("calibration - Show calibration curves", "calibration"),
        ("trend - Show accuracy over time", "trend"),
        ("backfill - Recalculate OpenSkill from history", "backfill"),
        ("player - Show player's OpenSkill rating details", "player"),
    ]
    .into_iter()
    .map(|(name, value)| CommandOptionChoice::String {
        name: name.to_owned(),
        value: value.to_owned(),
    })
    .collect();
    option
}

struct RatingAnalysisHandler {
    admin_user_ids: BTreeSet<i64>,
    worker: Arc<dyn RatingAnalysisWorkPort>,
}

#[async_trait]
impl InteractionHandler for RatingAnalysisHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            name,
            user_id,
            user_display_name,
            guild_id,
            member_permissions,
            options,
            ..
        } = request
        else {
            return Err("rating-analysis provider only accepts slash commands".into());
        };
        if name != "ratinganalysis" {
            return Err(format!("rating-analysis handler received command {name:?}").into());
        }
        let Some(guild_id) = guild_id else {
            return respond_initial(&responder, GUILD_ONLY).await;
        };
        let user_id = signed_id(user_id, "user")?;
        let guild_id = signed_id(guild_id, "guild")?;
        if !self.is_admin(user_id, member_permissions) {
            return respond_initial(&responder, ADMIN_ONLY).await;
        }

        let action = string_option(&options, "action").unwrap_or_default();
        if !matches!(
            action.as_str(),
            "backfill" | "compare" | "calibration" | "trend" | "player"
        ) {
            return respond_initial(&responder, &format!("Unknown action: {action}")).await;
        }
        let selected_user = user_option(&options, "user")
            .map(|(id, display_name)| {
                signed_id(id, "selected user")
                    .map(|id| MemberTarget::new(id, display_name.unwrap_or_else(|| id.to_string())))
            })
            .transpose()?;
        let input = RatingAnalysisInput {
            admin_allowed: true,
            action: action.clone(),
            guild_id: Some(guild_id),
            invoker: MemberTarget::new(user_id, user_display_name),
            selected_user,
        };

        if let Err(response_error) = responder.defer(true).await {
            warn!(%response_error, action, "unable to defer /ratinganalysis interaction");
            return Ok(());
        }
        if action == "backfill" {
            responder
                .followup(
                    InteractionResponse::message(STARTING_BACKFILL)
                        .without_mentions()
                        .ephemeral(),
                )
                .await
                .map_err(response_error)?;
        }

        let worker = Arc::clone(&self.worker);
        let plan = tokio::task::spawn_blocking(move || worker.execute(input)).await;
        let plan = match plan {
            Ok(Ok(plan)) => plan,
            Ok(Err(message)) => {
                error!(action, %message, "rating-analysis worker failed");
                return responder
                    .followup(worker_failure(&action, &message))
                    .await
                    .map_err(response_error);
            }
            Err(join_error) => {
                error!(action, %join_error, "rating-analysis blocking task failed");
                return responder
                    .followup(worker_failure(&action, "worker task failed"))
                    .await
                    .map_err(response_error);
            }
        };
        for delivery in plan.deliveries {
            if action == "backfill" && delivery.content.as_deref() == Some(STARTING_BACKFILL) {
                continue;
            }
            deliver(&responder, delivery).await?;
        }
        Ok(())
    }
}

impl RatingAnalysisHandler {
    fn is_admin(&self, user_id: i64, permissions: Option<u64>) -> bool {
        self.admin_user_ids.contains(&user_id)
            || permissions.is_some_and(|permissions| {
                permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            })
    }
}

trait RatingAnalysisWorkPort: Send + Sync {
    fn execute(&self, input: RatingAnalysisInput) -> Result<CommandPlan, String>;
}

struct ProductionRatingAnalysisWorker {
    repository: RatingAnalysisRepository,
    openskill: CamaOpenSkillSystem,
}

impl RatingAnalysisWorkPort for ProductionRatingAnalysisWorker {
    fn execute(&self, input: RatingAnalysisInput) -> Result<CommandPlan, String> {
        let (player, history) = if input.action == "player" {
            let target = input.selected_user.as_ref().unwrap_or(&input.invoker);
            let player = self
                .repository
                .player(target.discord_id, input.guild_id)
                .map_err(|error| error.to_string())?;
            let history = if player
                .is_some_and(|player| player.os_mu.is_some() && player.os_sigma.is_some())
            {
                self.repository
                    .player_openskill_history(target.discord_id, input.guild_id, 5)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|row| PlayerOpenSkillHistory {
                        os_mu_before: row.os_mu_before,
                        os_mu_after: row.os_mu_after,
                        os_sigma_before: row.os_sigma_before,
                        os_sigma_after: row.os_sigma_after,
                        won: row.won,
                        fantasy_weight: row.fantasy_weight,
                        streak_length: row.streak_length,
                        streak_multiplier: row.streak_multiplier,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (player, history)
        } else {
            (None, Vec::new())
        };
        let matches = ProductionMatchPort {
            repository: self.repository.clone(),
            history,
        };
        let players = PreloadedPlayerPort { player };
        let comparisons = ProductionComparisonPort {
            repository: self.repository.clone(),
            openskill: self.openskill.clone(),
        };
        let mut command = RatingAnalysisCommand::new(
            matches,
            players,
            Some(comparisons),
            NativeRatingAnalysisDrawing,
            self.openskill.clone(),
        );
        Ok(command.execute(&input))
    }
}

struct ProductionMatchPort {
    repository: RatingAnalysisRepository,
    history: Vec<PlayerOpenSkillHistory>,
}

impl RatingAnalysisMatchPort for ProductionMatchPort {
    fn backfill_openskill_ratings(
        &mut self,
        guild_id: Option<i64>,
        reset_first: bool,
    ) -> Result<BackfillResult, String> {
        if !reset_first {
            return Err(
                "reset_first=False is unsupported because it reapplies complete history on top of already-final ratings"
                    .to_owned(),
            );
        }
        self.repository
            .backfill_openskill(guild_id)
            .map(|result| BackfillResult {
                matches_processed: result.matches_processed,
                total_matches: Some(result.total_matches),
                players_updated: result.players_updated,
                errors: result.errors,
            })
            .map_err(|error| error.to_string())
    }

    fn player_openskill_history(
        &mut self,
        _discord_id: i64,
        _guild_id: Option<i64>,
        limit: usize,
    ) -> Vec<PlayerOpenSkillHistory> {
        self.history.iter().copied().take(limit).collect()
    }
}

struct PreloadedPlayerPort {
    player: Option<RatingAnalysisPlayer>,
}

impl RatingAnalysisPlayerPort for PreloadedPlayerPort {
    fn player(&mut self, _discord_id: i64, _guild_id: Option<i64>) -> Option<PlayerRatingRecord> {
        self.player.map(|player| PlayerRatingRecord {
            glicko_rating: player.glicko_rating,
            glicko_rd: player.glicko_rd,
        })
    }

    fn openskill_rating(&mut self, _discord_id: i64, _guild_id: Option<i64>) -> Option<(f64, f64)> {
        self.player
            .and_then(|player| player.os_mu.zip(player.os_sigma))
    }
}

struct ProductionComparisonPort {
    repository: RatingAnalysisRepository,
    openskill: CamaOpenSkillSystem,
}

impl ProductionComparisonPort {
    fn analyze(
        &self,
        guild_id: Option<i64>,
    ) -> Result<Option<cama_app::rating_comparison_service::RatingComparisonResult>, String> {
        let matches = self
            .repository
            .all_matches_with_predictions(guild_id)
            .map_err(|error| error.to_string())?;
        let ids = matches.iter().map(|row| row.match_id).collect::<Vec<_>>();
        let ratings = self
            .repository
            .openskill_ratings_for_matches(&ids, guild_id)
            .map_err(|error| error.to_string())?;
        let mut service = RatingComparisonService::with_openskill(
            LoadedComparisonRows { matches, ratings },
            NeverCurrentPredictions,
            self.openskill.clone(),
        );
        Ok(service.analyze_rating_systems(guild_id))
    }
}

impl RatingAnalysisComparisonPort for ProductionComparisonPort {
    fn comparison_summary(
        &mut self,
        guild_id: Option<i64>,
    ) -> Result<ComparisonSummaryPayload, String> {
        Ok(self.analyze(guild_id)?.map_or_else(
            || ComparisonSummaryPayload::Error("Insufficient data for comparison".to_owned()),
            ComparisonSummaryPayload::Report,
        ))
    }

    fn calibration_curve_data(
        &mut self,
        guild_id: Option<i64>,
    ) -> Result<CalibrationCurvePayload, String> {
        let Some(comparison) = self.analyze(guild_id)? else {
            return Ok(CalibrationCurvePayload::Error(
                "Insufficient data".to_owned(),
            ));
        };
        let extract = |buckets: &std::collections::BTreeMap<&'static str, CalibrationBucket>| {
            let mut points = buckets
                .values()
                .filter(|bucket| bucket.count != 0)
                .map(|bucket| CalibrationPoint {
                    predicted: bucket.avg_predicted,
                    actual_rate: bucket.actual_rate,
                    count: bucket.count,
                })
                .collect::<Vec<_>>();
            points.sort_by(|left, right| left.predicted.total_cmp(&right.predicted));
            points
        };
        Ok(CalibrationCurvePayload::Curves(CalibrationCurveData {
            glicko: extract(&comparison.glicko.calibration_buckets),
            openskill: extract(&comparison.openskill.calibration_buckets),
            perfect_line: vec![(0.0, 0.0), (1.0, 1.0)],
        }))
    }
}

struct LoadedComparisonRows {
    matches: Vec<RatingAnalysisHistoricalMatch>,
    ratings: HashMap<i64, RatingAnalysisHistoricalRatings>,
}

impl RatingComparisonMatchPort for LoadedComparisonRows {
    fn all_matches_with_predictions(
        &mut self,
        _guild_id: Option<i64>,
    ) -> Vec<HistoricalRatingMatch> {
        self.matches.iter().map(historical_match).collect()
    }

    fn openskill_ratings_for_matches(
        &mut self,
        match_ids: &[i64],
        _guild_id: Option<i64>,
    ) -> HashMap<i64, HistoricalOpenSkillRatings> {
        match_ids
            .iter()
            .filter_map(|match_id| {
                self.ratings.get(match_id).map(|ratings| {
                    (
                        *match_id,
                        HistoricalOpenSkillRatings {
                            team1: ratings
                                .team1
                                .iter()
                                .map(|(mu, sigma)| Rating::new(*mu, *sigma))
                                .collect(),
                            team2: ratings
                                .team2
                                .iter()
                                .map(|(mu, sigma)| Rating::new(*mu, *sigma))
                                .collect(),
                        },
                    )
                })
            })
            .collect()
    }
}

struct NeverCurrentPredictions;

impl CurrentOpenSkillPredictionPort for NeverCurrentPredictions {
    fn predict_from_current_ratings(
        &mut self,
        _team1_players: &[i64],
        _team2_players: &[i64],
        _guild_id: Option<i64>,
    ) -> f64 {
        unreachable!("historical comparison must never read current OpenSkill ratings")
    }
}

fn historical_match(row: &RatingAnalysisHistoricalMatch) -> HistoricalRatingMatch {
    HistoricalRatingMatch {
        match_id: row.match_id,
        match_date: parse_match_date(&row.match_date).unwrap_or(row.match_id),
        winning_team: row.winning_team,
        expected_radiant_win_prob: row.expected_radiant_win_prob,
        team1_players: row.team1_players.clone(),
        team2_players: row.team2_players.clone(),
        openskill_radiant_win_prob: row.openskill_radiant_win_prob,
        openskill_raw_radiant_win_prob: row.openskill_raw_radiant_win_prob,
    }
}

fn parse_match_date(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.timestamp())
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .ok()
                .or_else(|| {
                    NaiveDate::parse_from_str(value, "%Y-%m-%d")
                        .ok()
                        .and_then(|date| date.and_hms_opt(0, 0, 0))
                })
                .map(|date| DateTime::<Utc>::from_naive_utc_and_offset(date, Utc).timestamp())
        })
}

async fn deliver(
    responder: &Arc<dyn InteractionResponder>,
    delivery: Delivery,
) -> Result<(), InteractionHandlerError> {
    let mut response =
        InteractionResponse::message(delivery.content.unwrap_or_default()).without_mentions();
    if let Some(attachment) = delivery.attachment {
        if let Some(embed) = delivery.embed.as_ref() {
            response = response.embed(interaction_embed(embed, Some(&attachment.filename)));
        }
        response = response.attachment(InteractionAttachment::bytes(
            attachment.filename,
            attachment.bytes,
        ));
    } else if let Some(embed) = delivery.embed.as_ref() {
        response = response.embed(interaction_embed(embed, None));
    }
    if delivery.ephemeral.unwrap_or(false) {
        response = response.ephemeral();
    }
    match delivery.route {
        DeliveryRoute::InitialResponse => responder.respond(response).await,
        DeliveryRoute::Followup => responder.followup(response).await,
    }
    .map_err(response_error)
}

fn interaction_embed(
    embed: &cama_domain::embed_safety::EmbedModel,
    attachment_filename: Option<&str>,
) -> InteractionEmbed {
    InteractionEmbed {
        title: embed.title.clone(),
        description: embed.description.clone(),
        color: Some(
            if embed.title.as_deref() == Some("OpenSkill Backfill Complete") {
                GREEN
            } else {
                BLUE
            },
        ),
        image_url: attachment_filename.map(|filename| format!("attachment://{filename}")),
        fields: embed
            .fields
            .iter()
            .map(|field| crate::registration::InteractionEmbedField {
                name: field.name.clone(),
                value: field.value.clone(),
                inline: field.inline,
            })
            .collect(),
        ..InteractionEmbed::default()
    }
}

fn worker_failure(action: &str, message: &str) -> InteractionResponse {
    let (content, ephemeral) = match action {
        "backfill" => (format!("Backfill failed: {message}"), true),
        "compare" => (format!("Comparison failed: {message}"), false),
        "calibration" => (
            "❌ Couldn't generate the calibration data right now — try again in a moment."
                .to_owned(),
            false,
        ),
        "trend" => (
            "❌ Couldn't analyze rating trends right now — try again in a moment.".to_owned(),
            false,
        ),
        _ => (
            "❌ An error occurred while processing your command. Please try again.".to_owned(),
            true,
        ),
    };
    let response = InteractionResponse::message(content).without_mentions();
    if ephemeral {
        response.ephemeral()
    } else {
        response
    }
}

async fn respond_initial(
    responder: &Arc<dyn InteractionResponder>,
    content: &str,
) -> Result<(), InteractionHandlerError> {
    responder
        .respond(
            InteractionResponse::message(content)
                .without_mentions()
                .ephemeral(),
        )
        .await
        .map_err(response_error)
}

fn string_option(options: &[InteractionOption], name: &str) -> Option<String> {
    options
        .iter()
        .find_map(|option| match (&*option.name, &option.value) {
            (candidate, InteractionValue::String(value)) if candidate == name => {
                Some(value.clone())
            }
            _ => None,
        })
}

fn user_option(options: &[InteractionOption], name: &str) -> Option<(u64, Option<String>)> {
    options
        .iter()
        .find_map(|option| match (&*option.name, &option.value) {
            (
                candidate,
                InteractionValue::User {
                    id, display_name, ..
                },
            ) if candidate == name => Some((*id, display_name.clone())),
            _ => None,
        })
}

fn signed_id(value: u64, kind: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(value)
        .map_err(|_| format!("Discord {kind} ID exceeds SQLite's signed integer range").into())
}

fn response_error(error: crate::registration::InteractionResponseError) -> InteractionHandlerError {
    error.to_string().into()
}

#[cfg(all(test, feature = "runtime-test-draft"))]
#[path = "rating_analysis_provider/tests.rs"]
mod tests;
