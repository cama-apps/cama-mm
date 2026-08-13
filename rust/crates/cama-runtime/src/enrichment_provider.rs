//! Complete live Discord composition for Python's enrichment extension.
//!
//! Both slash-command groups execute against the admitted production SQLite
//! file and the process-wide OpenDota transport. Discovery and manual
//! enrichment share the same atomic write/replay semantics; match graph
//! controls carry all durable identity in their custom IDs, so a process
//! restart cannot redirect a click to another guild or match.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::dotabase_sqlite::{DotabaseSqliteSource, PRODUCTION_DOTABASE_PATH};
use cama_app::drawing::{AdvantageData, MatchRow, draw_advantage_graph, draw_matches_table};
use cama_app::embeds::{
    BankruptcyPenaltySource, EnrichedMatchRequest, MatchParticipant as EmbedParticipant,
    create_enriched_match_embed, create_match_summary_embed_with_context,
};
use cama_app::match_discovery::{
    BatchDiscoveryResult, DiscoveryConfig, DiscoveryResult, DiscoveryStatus, EnrichMatchRequest,
    EnrichmentSource, InternalMatchId, MatchDiscoveryService, MatchEnrichmentService,
    OpenSkillEnrichmentPort, OpenSkillEnrichmentResult, SteamId, SystemDiscoveryClock,
};
use cama_app::opendota_http::{OpenDotaHttpClient, OpenDotaRuntimeServices};
use cama_app::opendota_player_service::{OpenDotaPlayerApiPort, did_win};
use cama_app::trivia_data::TriviaDataRegistry;
use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::core_repositories::{
    EnrichmentJsonValue, MatchParticipant, MatchRepository, MatchSummary, PlayerMatch,
    PlayerMatchParticipant, PlayerRepository,
};
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_db::match_correction_repository::MatchCorrectionRepository;
use cama_db::match_discovery_repository::MatchDiscoveryRepository;
use cama_db::match_recording_repository::MatchRecordingRepository;
use cama_db::opendota_player::OpenDotaPlayerRepository;
use cama_db::pairings_repository::{PairingsRepository, PairingsService};
use cama_domain::embed_safety::EmbedModel;
use cama_domain::guild_config::GuildConfigStore;
use cama_domain::openskill::{CamaOpenSkillSystem, OpenSkillError};
use rusqlite::types::Value;
use thiserror::Error;
use tracing::{debug, error, info, warn};

use crate::application_config::ApplicationConfig;
use crate::registration::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute, InteractionActionRow,
    InteractionAttachment, InteractionButton, InteractionButtonStyle, InteractionEmbed,
    InteractionHandler, InteractionHandlerError, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionValue, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};

const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const GUILD_ONLY: &str = "This command can only be used in a server.";
const ADMIN_ONLY: &str = "This command is admin-only.";
const COMPONENT_PREFIX: &str = "enrichment:";
const MATCH_VIEW_TIMEOUT: Duration = Duration::from_secs(120);
const DISCORD_BLUE: u32 = 0x34_98_db;
const DISCORD_GREEN: u32 = 0x2e_cc_71;
const DISCORD_RED: u32 = 0xe7_4c_3c;
const DISCORD_GREYPLE: u32 = 0x99_aa_b5;
const DISCORD_DARKER: u32 = 0x2f_31_36;
const MATCH_CORRECTION_REPLAY_PREFIX: &str = "match_correction:";

type LiveEnrichment = MatchEnrichmentService<
    MatchRepository,
    OpenDotaPlayerRepository,
    OpenDotaHttpClient,
    MatchRecordingRepository,
    ConfiguredOpenSkillReplay,
>;

type LiveDiscovery = MatchDiscoveryService<
    MatchRepository,
    OpenDotaPlayerRepository,
    OpenDotaHttpClient,
    LiveEnrichment,
    SystemDiscoveryClock,
>;

type HeroCatalog = TriviaDataRegistry<DotabaseSqliteSource>;

#[derive(Debug, Error)]
pub enum EnrichmentProviderBuildError {
    #[error("invalid OpenSkill runtime configuration: {0}")]
    OpenSkill(#[from] OpenSkillError),
}

/// Result of Python-compatible post-record OpenDota discovery.
#[derive(Clone, Debug, PartialEq)]
pub enum RecordedMatchDiscoveryOutcome {
    Disabled,
    Discovered {
        result: DiscoveryResult,
        response: InteractionResponse,
    },
    Exhausted {
        last_result: Option<DiscoveryResult>,
    },
    Stopped(DiscoveryResult),
}

/// Narrow handle shared with the match runtime. It deliberately owns no
/// service state: the provider's existing handler supplies the one live
/// discovery cache, enrichment pipeline, and OpenSkill replay policy.
#[async_trait]
pub trait RecordedMatchDiscovery: Send + Sync {
    async fn discover_recorded_match(
        &self,
        guild_id: i64,
        match_id: i64,
    ) -> Result<RecordedMatchDiscoveryOutcome, String>;
}

#[derive(Clone)]
pub struct EnrichmentRegistrationProvider {
    handler: Arc<EnrichmentHandler>,
}

impl EnrichmentRegistrationProvider {
    /// Compose every enrichment command over the migrated database.
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        opendota: Arc<OpenDotaRuntimeServices>,
    ) -> Result<Self, EnrichmentProviderBuildError> {
        Self::with_dotabase_path(database_path, config, opendota, PRODUCTION_DOTABASE_PATH)
    }

    /// Testable production composition with an explicit immutable Dotabase
    /// catalog path. The ordinary constructor uses the image's pinned asset.
    pub fn with_dotabase_path(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        opendota: Arc<OpenDotaRuntimeServices>,
        dotabase_path: impl AsRef<Path>,
    ) -> Result<Self, EnrichmentProviderBuildError> {
        Self::compose(
            database_path,
            config,
            opendota,
            dotabase_path,
            MATCH_VIEW_TIMEOUT,
        )
    }

    fn compose(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        opendota: Arc<OpenDotaRuntimeServices>,
        dotabase_path: impl AsRef<Path>,
        match_view_timeout: Duration,
    ) -> Result<Self, EnrichmentProviderBuildError> {
        let path = database_path.as_ref();
        let system = CamaOpenSkillSystem::with_config(config.migration.openskill.clone())?;
        system.calibrate_win_probability(0.5, None)?;
        let correction = MatchCorrectionRepository::new(path);
        match correction.list_pending_openskill_replays() {
            Ok(jobs) => {
                for job in jobs {
                    // A result correction can still have an exact-once bet
                    // leg and an owned `core_applied` claim to finish.  Its
                    // replay job is therefore the Admin correction runtime's
                    // recovery marker; clearing it here would strand those
                    // steps after a restart.
                    if job.reason.starts_with(MATCH_CORRECTION_REPLAY_PREFIX) {
                        debug!(
                            guild_id = job.guild_id,
                            reason = %job.reason,
                            "leaving match-correction replay to admin recovery"
                        );
                        continue;
                    }
                    match correction.replay_openskill_atomic_configured(
                        Some(job.guild_id),
                        &system,
                        config.migration.new_player_mmr_discount,
                    ) {
                        Ok((summary, _)) if summary.errors.is_empty() => {
                            info!(
                                guild_id = job.guild_id,
                                reason = %job.reason,
                                "recovered pending OpenSkill replay"
                            );
                        }
                        Ok((summary, _)) => {
                            error!(
                                guild_id = job.guild_id,
                                reason = %job.reason,
                                errors = ?summary.errors,
                                "pending OpenSkill replay remains blocked"
                            );
                        }
                        Err(replay_error) => {
                            error!(
                                guild_id = job.guild_id,
                                reason = %job.reason,
                                %replay_error,
                                "pending OpenSkill replay recovery failed"
                            );
                        }
                    }
                }
            }
            Err(recovery_error) => {
                error!(%recovery_error, "unable to enumerate pending OpenSkill replays");
            }
        }
        let replay = ConfiguredOpenSkillReplay {
            completion: MatchDiscoveryRepository::new(path),
            correction,
            system,
            new_player_mmr_discount: config.migration.new_player_mmr_discount,
        };
        let make_enrichment = || {
            MatchEnrichmentService::new(
                MatchRepository::new(path),
                OpenDotaPlayerRepository::new(path),
                opendota.enrichment_api(),
                MatchRecordingRepository::new(path),
                Some(replay.clone()),
            )
            .with_minimum_roster_matches(nonnegative_usize(
                config.values.enrichment_min_player_match,
            ))
        };
        let discovery = MatchDiscoveryService::new(
            MatchRepository::new(path),
            OpenDotaPlayerRepository::new(path),
            opendota.discovery_api(),
            make_enrichment(),
            SystemDiscoveryClock,
        )
        .with_config(DiscoveryConfig {
            minimum_players_with_steam_id: 5,
            minimum_roster_matches: nonnegative_usize(config.values.enrichment_min_player_match),
            time_window_seconds: config.values.enrichment_discovery_time_window,
            history_limit: 100,
            batch_limit: 1_000,
            match_details_cache_size: 32,
            transient_retry_delay: Duration::ZERO,
        });
        Ok(Self {
            handler: Arc::new(EnrichmentHandler {
                matches: MatchRepository::new(path),
                players: PlayerRepository::new(path),
                steam: OpenDotaPlayerRepository::new(path),
                guild_config: GuildConfigRepository::new(path, config.values.ai_features_enabled),
                administrative: MatchDiscoveryRepository::new(path),
                pairings: PairingsService::new(PairingsRepository::new(path)),
                bankruptcy: BankruptcySource(BankruptcyRepository::new(path)),
                enrichment: Arc::new(make_enrichment()),
                discovery: Arc::new(discovery),
                opendota,
                hero_catalog: Arc::new(TriviaDataRegistry::new(DotabaseSqliteSource::new(
                    dotabase_path,
                ))),
                match_view_timeout,
                enrichment_retry_delays: config
                    .values
                    .enrichment_retry_delays
                    .iter()
                    .copied()
                    .map(|seconds| Duration::from_secs(seconds.max(0) as u64))
                    .collect(),
                admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
            }),
        })
    }

    /// Clone the existing provider state for post-record discovery.
    #[must_use]
    pub fn recorded_match_discovery(&self) -> Arc<dyn RecordedMatchDiscovery> {
        self.handler.clone()
    }
}

impl RegistrationProvider for EnrichmentRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "enrich".to_owned(),
            description: "Match enrichment and discovery (Admin)".to_owned(),
            options: enrich_subcommands(),
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "matches".to_owned(),
            description: "Recent and detailed match statistics".to_owned(),
            options: matches_subcommands(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

fn enrich_subcommands() -> Vec<CommandOptionSpec> {
    vec![
        subcommand(
            "setleague",
            "Set the Dota 2 league ID used by OpenDota discovery (Admin)",
            vec![
                CommandOptionSpec::new(
                    "league_id",
                    "The Dota 2 league ID used by OpenDota discovery",
                    CommandOptionKind::Integer,
                )
                .required(true),
            ],
        ),
        subcommand(
            "match",
            "Enrich a match with OpenDota data (Admin)",
            vec![
                CommandOptionSpec::new(
                    "valve_match_id",
                    "The Dota 2 match ID (optional - defaults to enriching most recent match)",
                    CommandOptionKind::Integer,
                ),
                CommandOptionSpec::new(
                    "internal_match_id",
                    "Our internal match ID (optional - defaults to most recent)",
                    CommandOptionKind::Integer,
                ),
            ],
        ),
        subcommand(
            "backfill",
            "Backfill steam_id from dotabuff URLs for all players (Admin)",
            vec![],
        ),
        subcommand("config", "Show current server configuration", vec![]),
        subcommand(
            "discover",
            "[Admin] Auto-discover Dota match IDs for unenriched matches",
            vec![
                CommandOptionSpec::new(
                    "dry_run",
                    "Preview only, don't apply enrichments (default: False)",
                    CommandOptionKind::Boolean,
                ),
                CommandOptionSpec::new(
                    "refill_fantasy",
                    "Re-enrich matches that have enrichment but no fantasy data",
                    CommandOptionKind::Boolean,
                ),
            ],
        ),
        subcommand("wipeall", "[Admin] Wipe all match enrichments", vec![]),
        subcommand(
            "wipematch",
            "[Admin] Wipe enrichment data for a specific match",
            vec![
                CommandOptionSpec::new(
                    "match_id",
                    "Internal match ID to wipe enrichment from",
                    CommandOptionKind::Integer,
                )
                .required(true),
            ],
        ),
        subcommand(
            "rebuildpairings",
            "Rebuild pairwise stats from match history (Admin only)",
            vec![],
        ),
    ]
}

fn matches_subcommands() -> Vec<CommandOptionSpec> {
    vec![
        subcommand(
            "history",
            "View recent matches with detailed stats",
            vec![
                CommandOptionSpec::new(
                    "user",
                    "Player to view history for (defaults to yourself)",
                    CommandOptionKind::User,
                ),
                CommandOptionSpec::new(
                    "limit",
                    "Number of matches to show (default: 5, max: 10)",
                    CommandOptionKind::Integer,
                ),
            ],
        ),
        subcommand(
            "view",
            "View detailed stats for a specific match",
            vec![
                CommandOptionSpec::new(
                    "match_id",
                    "Internal match ID to view (defaults to most recent)",
                    CommandOptionKind::Integer,
                ),
                CommandOptionSpec::new(
                    "user",
                    "Player whose most recent match to view (if no match_id)",
                    CommandOptionKind::User,
                ),
            ],
        ),
        subcommand(
            "recent",
            "View your recent Dota 2 matches as an image",
            vec![
                CommandOptionSpec::new(
                    "user",
                    "User to show matches for (defaults to yourself)",
                    CommandOptionKind::User,
                ),
                CommandOptionSpec::new(
                    "limit",
                    "Number of matches to show (default 10, max 20)",
                    CommandOptionKind::Integer,
                ),
            ],
        ),
    ]
}

fn subcommand(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
}

fn nonnegative_usize(value: i64) -> usize {
    usize::try_from(value.max(0)).unwrap_or(usize::MAX)
}

#[derive(Clone)]
struct ConfiguredOpenSkillReplay {
    completion: MatchDiscoveryRepository,
    correction: MatchCorrectionRepository,
    system: CamaOpenSkillSystem,
    new_player_mmr_discount: i32,
}

impl OpenSkillEnrichmentPort for ConfiguredOpenSkillReplay {
    fn update_openskill_ratings_for_match(
        &self,
        match_id: cama_app::match_discovery::InternalMatchId,
        guild_id: Option<cama_app::dedicated_lobby_channel::GuildId>,
    ) -> Result<OpenSkillEnrichmentResult, cama_app::match_discovery::DiscoveryPortError> {
        let guild_id = guild_id.map(|guild_id| guild_id.0);
        let (participants, with_fantasy) = self
            .completion
            .fantasy_completion(match_id.0, guild_id)
            .map_err(|error| {
                cama_app::match_discovery::DiscoveryPortError::new(error.to_string())
            })?;
        if participants != 10 || with_fantasy != participants {
            return Ok(OpenSkillEnrichmentResult {
                success: true,
                players_updated: 0,
                error: None,
            });
        }
        let (summary, _) = self
            .correction
            .replay_openskill_atomic_configured(
                guild_id,
                &self.system,
                self.new_player_mmr_discount,
            )
            .map_err(|error| {
                cama_app::match_discovery::DiscoveryPortError::new(error.to_string())
            })?;
        Ok(OpenSkillEnrichmentResult {
            success: summary.errors.is_empty(),
            players_updated: if summary.errors.is_empty() {
                participants
            } else {
                0
            },
            error: (!summary.errors.is_empty())
                .then(|| format!("OpenSkill replay failed: {}", summary.errors.join("; "))),
        })
    }
}

#[derive(Clone)]
struct BankruptcySource(BankruptcyRepository);

impl BankruptcyPenaltySource for BankruptcySource {
    fn get_bulk_states(
        &self,
        discord_ids: &[i64],
        guild_id: Option<i64>,
    ) -> Result<BTreeMap<i64, i64>, String> {
        self.0
            .get_bulk_states(discord_ids, guild_id)
            .map(|states| {
                states
                    .into_iter()
                    .map(|(id, state)| (id, state.penalty_games_remaining))
                    .collect()
            })
            .map_err(|error| error.to_string())
    }

    fn get_penalty_games(&self, discord_id: i64, guild_id: Option<i64>) -> Result<i64, String> {
        self.0
            .get_penalty_games(discord_id, guild_id)
            .map_err(|error| error.to_string())
    }
}

struct EnrichmentHandler {
    matches: MatchRepository,
    players: PlayerRepository,
    steam: OpenDotaPlayerRepository,
    guild_config: GuildConfigRepository,
    administrative: MatchDiscoveryRepository,
    pairings: PairingsService<PairingsRepository>,
    bankruptcy: BankruptcySource,
    enrichment: Arc<LiveEnrichment>,
    discovery: Arc<LiveDiscovery>,
    opendota: Arc<OpenDotaRuntimeServices>,
    hero_catalog: Arc<HeroCatalog>,
    match_view_timeout: Duration,
    enrichment_retry_delays: Vec<Duration>,
    admin_user_ids: BTreeSet<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordedDiscoveryDisposition {
    Publish,
    Retry,
    Stop,
}

fn recorded_discovery_disposition(status: DiscoveryStatus) -> RecordedDiscoveryDisposition {
    match status {
        DiscoveryStatus::Discovered => RecordedDiscoveryDisposition::Publish,
        DiscoveryStatus::LowConfidence
        | DiscoveryStatus::NoCandidates
        | DiscoveryStatus::InProgress => RecordedDiscoveryDisposition::Retry,
        DiscoveryStatus::NoSteamIds
        | DiscoveryStatus::NoTimestamp
        | DiscoveryStatus::NotFound
        | DiscoveryStatus::ValidationFailed
        | DiscoveryStatus::Error => RecordedDiscoveryDisposition::Stop,
    }
}

#[async_trait]
impl RecordedMatchDiscovery for EnrichmentHandler {
    async fn discover_recorded_match(
        &self,
        guild_id: i64,
        match_id: i64,
    ) -> Result<RecordedMatchDiscoveryOutcome, String> {
        let guild_config = self.guild_config.clone();
        let enabled = run_blocking(move || guild_config.get_auto_enrich(guild_id)).await?;
        if !enabled {
            return Ok(RecordedMatchDiscoveryOutcome::Disabled);
        }

        let mut last_result = None;
        for delay in &self.enrichment_retry_delays {
            tokio::time::sleep(*delay).await;
            let discovery = Arc::clone(&self.discovery);
            let result = run_blocking(move || {
                Ok::<_, String>(discovery.discover_match(
                    InternalMatchId(match_id),
                    Some(cama_app::dedicated_lobby_channel::GuildId(guild_id)),
                    false,
                ))
            })
            .await?;
            match recorded_discovery_disposition(result.status) {
                RecordedDiscoveryDisposition::Publish => {
                    let response = self
                        .recorded_match_discovery_response(guild_id, &result)
                        .await?;
                    return Ok(RecordedMatchDiscoveryOutcome::Discovered { result, response });
                }
                RecordedDiscoveryDisposition::Retry => last_result = Some(result),
                RecordedDiscoveryDisposition::Stop => {
                    return Ok(RecordedMatchDiscoveryOutcome::Stopped(result));
                }
            }
        }
        Ok(RecordedMatchDiscoveryOutcome::Exhausted { last_result })
    }
}

#[async_trait]
impl InteractionHandler for EnrichmentHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command {
                name,
                user_id,
                user_display_name,
                guild_id,
                member_permissions,
                options,
                ..
            } => {
                let (subcommand, options) = selected_subcommand(&options)?;
                let context = CommandContext {
                    user_id: signed_id(user_id, "user")?,
                    user_display_name,
                    guild_id: guild_id
                        .map(|guild_id| signed_id(guild_id, "guild"))
                        .transpose()?,
                    member_permissions,
                };
                match (name.as_str(), subcommand) {
                    ("enrich", "setleague") => {
                        self.handle_set_league(context, options, responder).await
                    }
                    ("enrich", "match") => {
                        self.handle_enrich_match(context, options, responder).await
                    }
                    ("enrich", "backfill") => self.handle_backfill(context, responder).await,
                    ("enrich", "config") => self.handle_config(context, responder).await,
                    ("enrich", "discover") => {
                        self.handle_discover(context, options, responder).await
                    }
                    ("enrich", "wipeall") => self.handle_wipe_all(context, responder).await,
                    ("enrich", "wipematch") => {
                        self.handle_wipe_match(context, options, responder).await
                    }
                    ("enrich", "rebuildpairings") => {
                        self.handle_rebuild_pairings(context, responder).await
                    }
                    ("matches", "history") => {
                        self.handle_history(context, options, responder).await
                    }
                    ("matches", "view") => self.handle_view(context, options, responder).await,
                    ("matches", "recent") => self.handle_recent(context, options, responder).await,
                    _ => Err(format!(
                        "enrichment handler received unknown path {name} {subcommand}"
                    )
                    .into()),
                }
            }
            InteractionRequest::Component {
                custom_id,
                guild_id,
                ..
            } => self.handle_component(&custom_id, guild_id, responder).await,
            InteractionRequest::Autocomplete { .. } => {
                Err("enrichment extension has no autocomplete routes".into())
            }
            InteractionRequest::Modal { .. } => {
                Err("enrichment extension has no modal routes".into())
            }
        }
    }
}

#[derive(Clone)]
struct CommandContext {
    user_id: i64,
    user_display_name: String,
    guild_id: Option<i64>,
    member_permissions: Option<u64>,
}

impl EnrichmentHandler {
    async fn recorded_match_discovery_response(
        &self,
        guild_id: i64,
        result: &DiscoveryResult,
    ) -> Result<InteractionResponse, String> {
        let matches = self.matches.clone();
        let bankruptcy = self.bankruptcy.clone();
        let opendota = Arc::clone(&self.opendota);
        let hero_catalog = Arc::clone(&self.hero_catalog);
        let match_view_timeout = self.match_view_timeout;
        let match_id = result.match_id.0;
        let confidence_percent = result.confidence.unwrap_or_default() * 100.0;
        let valve_match_id = result.valve_match_id.map(|id| id.0);
        run_blocking(move || {
            let match_data = matches
                .get_match(match_id, Some(guild_id))
                .map_err(|error| error.to_string())?;
            let participants = matches
                .get_match_participants(match_id, Some(guild_id))
                .map_err(|error| error.to_string())?;
            let Some(match_data) = match_data else {
                return Ok::<_, String>(InteractionResponse::message(format!(
                    "📊 Match #{match_id} auto-enriched! (Dota Match ID: {}, {confidence_percent:.0}% confidence)",
                    valve_match_id.map_or_else(|| "unknown".to_owned(), |id| id.to_string())
                )));
            };
            if participants.is_empty() {
                return Ok(InteractionResponse::message(format!(
                    "📊 Match #{match_id} auto-enriched! (Dota Match ID: {}, {confidence_percent:.0}% confidence)",
                    valve_match_id.map_or_else(|| "unknown".to_owned(), |id| id.to_string())
                )));
            }
            let mut response = InteractionResponse::message(format!(
                "📊 Match #{match_id} auto-enriched ({confidence_percent:.0}% confidence)"
            ))
            .embed(build_match_embed(
                &match_data,
                &participants,
                &bankruptcy,
                &opendota,
                &hero_catalog,
            ));
            let advantage = matches
                .get_enrichment_data(match_id, Some(guild_id))
                .ok()
                .flatten()
                .as_ref()
                .map(advantage_data)
                .unwrap_or_default();
            if !advantage.radiant_gold.is_empty() || !advantage.radiant_xp.is_empty() {
                let timeout_seconds =
                    i64::try_from(match_view_timeout.as_secs()).unwrap_or(i64::MAX);
                response = response.action_row(match_view_buttons(
                    guild_id,
                    match_id,
                    unix_now().saturating_add(timeout_seconds),
                    0,
                ));
            }
            Ok(response)
        })
        .await
    }

    fn is_admin(&self, context: &CommandContext) -> bool {
        self.admin_user_ids.contains(&context.user_id)
            || context.member_permissions.is_some_and(|permissions| {
                permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            })
    }

    async fn require_guild(
        &self,
        context: &CommandContext,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<Option<i64>, InteractionHandlerError> {
        let Some(guild_id) = context.guild_id else {
            respond_initial(
                responder,
                InteractionResponse::message(GUILD_ONLY).ephemeral(),
            )
            .await?;
            return Ok(None);
        };
        Ok(Some(guild_id))
    }

    async fn require_admin(
        &self,
        context: &CommandContext,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<bool, InteractionHandlerError> {
        if self.is_admin(context) {
            Ok(true)
        } else {
            respond_initial(
                responder,
                InteractionResponse::message(ADMIN_ONLY).ephemeral(),
            )
            .await?;
            Ok(false)
        }
    }

    async fn handle_set_league(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = self.require_guild(&context, &responder).await? else {
            return Ok(());
        };
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let league_id = integer_option(options, "league_id")
            .ok_or_else(|| InteractionHandlerError::from("missing league_id"))?;
        if !defer(&responder, true).await {
            return Ok(());
        }
        let repository = self.guild_config.clone();
        run_blocking(move || repository.set_league_id(guild_id, league_id))
            .await
            .map_err(InteractionHandlerError::from)?;
        responder
            .followup(
                InteractionResponse::message(format!(
                    "League ID set to **{league_id}** for this server."
                ))
                .ephemeral(),
            )
            .await
            .map_err(response_error)
    }

    async fn handle_config(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = self.require_guild(&context, &responder).await? else {
            return Ok(());
        };
        if !defer(&responder, true).await {
            return Ok(());
        }
        let repository = self.guild_config.clone();
        let config = run_blocking(move || repository.get_config(guild_id))
            .await
            .map_err(InteractionHandlerError::from)?;
        let response = if let Some(config) = config {
            let league_id = config
                .league_id
                .map_or_else(|| "Not set".to_owned(), |value| value.to_string());
            let auto = if config.auto_enrich_matches {
                "Enabled"
            } else {
                "Disabled"
            };
            format!(
                "**Server Configuration**\n\n**League ID:** {league_id}\n\
                 **Auto-enrich matches:** {auto}"
            )
        } else {
            "No configuration set for this server.\nUse `/setleague` to configure.".to_owned()
        };
        responder
            .followup(InteractionResponse::message(response).ephemeral())
            .await
            .map_err(response_error)
    }

    async fn handle_backfill(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        if !defer(&responder, true).await {
            return Ok(());
        }
        let enrichment = Arc::clone(&self.enrichment);
        let result = run_blocking(move || enrichment.backfill_steam_ids())
            .await
            .map_err(InteractionHandlerError::from)?;
        let mut content = format!(
            "Steam ID backfill complete!\n\n**Players updated:** {}",
            result.players_updated
        );
        if !result.players_failed.is_empty() {
            content.push_str(&format!(
                "\n**Failed:** {} players (invalid dotabuff URLs)",
                result.players_failed.len()
            ));
        }
        responder
            .followup(InteractionResponse::message(content).ephemeral())
            .await
            .map_err(response_error)
    }

    async fn handle_wipe_all(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = self.require_guild(&context, &responder).await? else {
            return Ok(());
        };
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        if !defer(&responder, true).await {
            return Ok(());
        }
        let repository = self.administrative.clone();
        let count = run_blocking(move || repository.get_enriched_count(Some(guild_id)))
            .await
            .map_err(InteractionHandlerError::from)?;
        let content = if count == 0 {
            "No enrichments to wipe.".to_owned()
        } else {
            let repository = self.administrative.clone();
            let wiped = run_blocking(move || repository.wipe_all_enrichments(Some(guild_id)))
                .await
                .map_err(InteractionHandlerError::from)?;
            format!("Wiped {wiped} enrichments. Run `/enrich discover` to re-enrich.")
        };
        responder
            .followup(InteractionResponse::message(content).ephemeral())
            .await
            .map_err(response_error)
    }

    async fn handle_wipe_match(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let match_id = integer_option(options, "match_id")
            .ok_or_else(|| InteractionHandlerError::from("missing match_id"))?;
        if !defer(&responder, true).await {
            return Ok(());
        }
        let repository = self.administrative.clone();
        let guild_id = context.guild_id;
        let wiped = run_blocking(move || repository.wipe_match_enrichment(match_id, guild_id))
            .await
            .map_err(InteractionHandlerError::from)?;
        let content = if wiped {
            format!("Wiped enrichment data for match #{match_id}.")
        } else {
            format!("Match #{match_id} not found.")
        };
        responder
            .followup(InteractionResponse::message(content).ephemeral())
            .await
            .map_err(response_error)
    }

    async fn handle_rebuild_pairings(
        &self,
        context: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        if !defer(&responder, true).await {
            return Ok(());
        }
        let pairings = self.pairings.clone();
        let guild_id = context.guild_id;
        match run_blocking(move || pairings.rebuild_all_pairings(guild_id)).await {
            Ok(count) => responder
                .followup(
                    InteractionResponse::message(format!(
                        "Rebuilt pairwise statistics. {count} pairings calculated from match history."
                    ))
                    .ephemeral(),
                )
                .await
                .map_err(response_error),
            Err(message) => {
                error!(%message, "error rebuilding pairings");
                responder
                    .followup(
                        InteractionResponse::message(format!(
                            "Error rebuilding pairings: {message}"
                        ))
                        .ephemeral(),
                    )
                    .await
                    .map_err(response_error)
            }
        }
    }
}

impl EnrichmentHandler {
    async fn handle_enrich_match(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = self.require_guild(&context, &responder).await? else {
            return Ok(());
        };
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        if !defer(&responder, true).await {
            return Ok(());
        }

        // discord.py treats optional integer zero as absent in the original
        // command (`if internal_match_id` / `if valve_match_id`). Preserve
        // that edge case so a zero value still selects the normal fallback.
        let requested_internal = integer_option(options, "internal_match_id").filter(|id| *id != 0);
        let requested_valve = integer_option(options, "valve_match_id").filter(|id| *id != 0);
        let matches = self.matches.clone();
        let enrichment = Arc::clone(&self.enrichment);
        let bankruptcy = self.bankruptcy.clone();
        let opendota = Arc::clone(&self.opendota);
        let hero_catalog = Arc::clone(&self.hero_catalog);
        let response = run_blocking(move || -> Result<InteractionResponse, String> {
            let selected = if let Some(match_id) = requested_internal {
                matches
                    .get_match(match_id, Some(guild_id))
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("Match #{match_id} not found."))
            } else {
                let recent = matches
                    .get_most_recent_match(Some(guild_id))
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "No matches found in the database.".to_owned())?;
                matches
                    .get_match(recent.match_id, Some(guild_id))
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "No matches found in the database.".to_owned())
            };
            let selected = match selected {
                Ok(selected) => selected,
                Err(message) => return Ok(InteractionResponse::message(message).ephemeral()),
            };
            let internal_match_id = selected.match_id;
            let valve_match_id = requested_valve.or(selected.valve_match_id);
            let Some(valve_match_id) = valve_match_id else {
                return Ok(InteractionResponse::message(format!(
                    "Match #{internal_match_id} needs a Dota 2 match ID to enrich.\n\
                         Use: `/enrich match valve_match_id:<DOTA_MATCH_ID>`\n\n\
                         You can find the match ID on Dotabuff or in the Dota 2 client."
                ))
                .ephemeral());
            };

            let result = enrichment.enrich_match(EnrichMatchRequest {
                internal_match_id: cama_app::match_discovery::InternalMatchId(internal_match_id),
                valve_match_id: cama_app::match_discovery::ValveMatchId(valve_match_id),
                guild_id: Some(cama_app::dedicated_lobby_channel::GuildId(guild_id)),
                source: EnrichmentSource::Manual,
                confidence: None,
                skip_validation: true,
                opendota_match_data: None,
            });
            if !result.success {
                return Ok(InteractionResponse::message(format!(
                    "Enrichment failed: {}",
                    result.error.as_deref().unwrap_or("Unknown error")
                ))
                .ephemeral());
            }

            let match_data = matches
                .get_match(internal_match_id, Some(guild_id))
                .map_err(|error| error.to_string())?;
            let participants = matches
                .get_match_participants(internal_match_id, Some(guild_id))
                .map_err(|error| error.to_string())?;
            if let Some(match_data) = match_data
                && !participants.is_empty()
            {
                let embed = build_match_embed(
                    &match_data,
                    &participants,
                    &bankruptcy,
                    &opendota,
                    &hero_catalog,
                );
                let mut note = format!("Enriched {}/10 players", result.players_enriched);
                if !result.players_not_found.is_empty() {
                    note.push_str(&format!(
                        " ({} missing steam_id)",
                        result.players_not_found.len()
                    ));
                }
                return Ok(InteractionResponse::message(note).embed(embed).ephemeral());
            }

            let duration_minutes = result.duration_seconds / 60;
            let duration_seconds = result.duration_seconds % 60;
            let winner = if result.radiant_win {
                "Radiant"
            } else {
                "Dire"
            };
            let mut content = format!(
                "Match #{internal_match_id} enriched successfully!\n\n\
                 **Dota 2 Match ID:** {valve_match_id}\n\
                 **Duration:** {duration_minutes}:{duration_seconds:02}\n\
                 **Score:** Radiant {} - {} Dire\n\
                 **Winner:** {winner}\n\
                 **Players enriched:** {}/10",
                result.radiant_score, result.dire_score, result.players_enriched
            );
            if !result.players_not_found.is_empty() {
                content.push_str(&format!(
                    "\n\n*{} players not matched (missing steam_id)*",
                    result.players_not_found.len()
                ));
            }
            Ok(InteractionResponse::message(content).ephemeral())
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        responder.followup(response).await.map_err(response_error)
    }

    async fn handle_discover(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = self.require_guild(&context, &responder).await? else {
            return Ok(());
        };
        if !self.require_admin(&context, &responder).await? {
            return Ok(());
        }
        let dry_run = boolean_option(options, "dry_run").unwrap_or(false);
        let refill = boolean_option(options, "refill_fantasy").unwrap_or(false);
        if !defer(&responder, true).await {
            return Ok(());
        }
        if refill {
            return self
                .handle_refill_fantasy(guild_id, dry_run, responder)
                .await;
        }

        responder
            .followup(
                InteractionResponse::message(format!(
                    "Starting match discovery (dry_run={dry_run})... This may take a while."
                ))
                .ephemeral(),
            )
            .await
            .map_err(response_error)?;
        let discovery = Arc::clone(&self.discovery);
        let result = run_blocking(move || {
            Ok::<_, String>(discovery.discover_all_matches(
                Some(cama_app::dedicated_lobby_channel::GuildId(guild_id)),
                dry_run,
            ))
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        responder
            .edit_original(
                InteractionResponse::message(format_discovery(&result, dry_run)).ephemeral(),
            )
            .await
            .map_err(response_error)
    }

    async fn handle_refill_fantasy(
        &self,
        guild_id: i64,
        dry_run: bool,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let matches = self.matches.clone();
        let candidates =
            run_blocking(move || matches.get_matches_without_fantasy_data(Some(guild_id), 100))
                .await
                .map_err(InteractionHandlerError::from)?;
        if candidates.is_empty() {
            return responder
                .followup(
                    InteractionResponse::message(
                        "No matches need fantasy data refill. All enriched matches have fantasy points.",
                    )
                    .ephemeral(),
                )
                .await
                .map_err(response_error);
        }

        responder
            .followup(
                InteractionResponse::message(format!(
                    "Found {} matches without fantasy data. {}",
                    candidates.len(),
                    if dry_run {
                        "Previewing..."
                    } else {
                        "Re-enriching..."
                    }
                ))
                .ephemeral(),
            )
            .await
            .map_err(response_error)?;

        let enrichment = Arc::clone(&self.enrichment);
        let processed = candidates.len();
        let (refilled, errors) = run_blocking(move || {
            let mut refilled = Vec::new();
            let mut errors = Vec::new();
            for candidate in candidates {
                if dry_run {
                    refilled.push((candidate.match_id, candidate.valve_match_id));
                    continue;
                }
                let result = enrichment.enrich_match(EnrichMatchRequest {
                    internal_match_id: cama_app::match_discovery::InternalMatchId(
                        candidate.match_id,
                    ),
                    valve_match_id: cama_app::match_discovery::ValveMatchId(
                        candidate.valve_match_id,
                    ),
                    guild_id: Some(cama_app::dedicated_lobby_channel::GuildId(guild_id)),
                    source: EnrichmentSource::Manual,
                    confidence: None,
                    skip_validation: true,
                    opendota_match_data: None,
                });
                if result.success {
                    refilled.push((candidate.match_id, candidate.valve_match_id));
                } else {
                    errors.push((
                        candidate.match_id,
                        result.error.unwrap_or_else(|| "Unknown".to_owned()),
                    ));
                }
            }
            Ok::<_, String>((refilled, errors))
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        let mut lines = vec![
            format!(
                "**Fantasy Data Refill {}**",
                if dry_run { "(DRY RUN)" } else { "Complete" }
            ),
            String::new(),
            format!("Matches processed: {processed}"),
            format!("Successfully refilled: {}", refilled.len()),
            format!("Errors: {}", errors.len()),
        ];
        if !refilled.is_empty() && !dry_run {
            lines.extend([String::new(), "**Refilled:**".to_owned()]);
            lines.extend(
                refilled
                    .iter()
                    .take(10)
                    .map(|(match_id, valve_id)| format!("  #{match_id} → {valve_id}")),
            );
            if refilled.len() > 10 {
                lines.push(format!("  ... and {} more", refilled.len() - 10));
            }
        }
        if !errors.is_empty() {
            lines.extend([String::new(), "**Errors:**".to_owned()]);
            lines.extend(
                errors
                    .iter()
                    .take(5)
                    .map(|(match_id, message)| format!("  #{match_id}: {message}")),
            );
        }
        responder
            .edit_original(InteractionResponse::message(lines.join("\n")).ephemeral())
            .await
            .map_err(response_error)
    }
}

impl EnrichmentHandler {
    async fn handle_history(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = self.require_guild(&context, &responder).await? else {
            return Ok(());
        };
        if !defer(&responder, true).await {
            return Ok(());
        }
        let selected_user = user_option(options, "user");
        let (target_id, target_name, selected) = selected_user.map_or_else(
            || (context.user_id, context.user_display_name.clone(), false),
            |(id, name)| (id, name, true),
        );
        let limit = integer_option(options, "limit").unwrap_or(5).clamp(1, 10);
        let players = self.players.clone();
        let matches = self.matches.clone();
        let opendota = Arc::clone(&self.opendota);
        let hero_catalog = Arc::clone(&self.hero_catalog);
        let response = run_blocking(move || {
            if !players
                .exists(target_id, Some(guild_id))
                .map_err(|error| error.to_string())?
            {
                return Ok::<_, String>(
                    InteractionResponse::message(if selected {
                        "That user is not registered.".to_owned()
                    } else {
                        "You are not registered.".to_owned()
                    })
                    .ephemeral(),
                );
            }
            let history = matches
                .get_player_matches(target_id, Some(guild_id), limit)
                .map_err(|error| error.to_string())?;
            if history.is_empty() {
                return Ok(InteractionResponse::message(format!(
                    "No matches found for {target_name}."
                ))
                .ephemeral());
            }
            let wins = history.iter().filter(|item| item.player_won).count();
            let losses = history.len() - wins;
            let color = match wins.cmp(&losses) {
                std::cmp::Ordering::Greater => DISCORD_GREEN,
                std::cmp::Ordering::Less => DISCORD_RED,
                std::cmp::Ordering::Equal => DISCORD_GREYPLE,
            };
            let mut embed =
                InteractionEmbed::titled(format!("📜 Recent Matches for {target_name}"))
                    .description(format!(
                        "Last {} matches • **{wins}W - {losses}L**",
                        history.len()
                    ))
                    .color(color);
            let mut first_hero_id = None;
            for match_row in &history {
                let hero_id = value_i64(&match_row.hero_id).filter(|hero_id| *hero_id != 0);
                if first_hero_id.is_none() {
                    first_hero_id = hero_id;
                }
                let hero = hero_id.map_or_else(
                    || "???".to_owned(),
                    |hero_id| {
                        opendota
                            .hero_name(hero_id)
                            .map_or_else(|| format!("Hero {hero_id}"), ToOwned::to_owned)
                    },
                );
                let side = value_text(&match_row.side).unwrap_or("?").to_owned();
                let capital_side = capitalize(&side);
                let result_emoji = if match_row.player_won { "✅" } else { "❌" };
                let lobby_emoji = if value_text(&match_row.lobby_type) == Some("draft") {
                    "👑"
                } else {
                    "🎲"
                };
                let field = if hero_id.is_some() {
                    let kills = value_i64(&match_row.kills).unwrap_or(0);
                    let deaths = value_i64(&match_row.deaths).unwrap_or(0);
                    let assists = value_i64(&match_row.assists).unwrap_or(0);
                    let gpm = value_i64(&match_row.gpm).unwrap_or(0);
                    let damage = compact_number(value_i64(&match_row.hero_damage));
                    let net_worth = compact_number(value_i64(&match_row.net_worth));
                    let tower_damage = compact_number(value_i64(&match_row.tower_damage));
                    let healing = compact_number(value_i64(&match_row.hero_healing));
                    let lane = history_lane_label(match_row, target_id);
                    let mut block = format!(
                        "```\nKDA: {kills}/{deaths}/{assists}  GPM: {gpm}\n\
                         DMG: {damage}  TD: {tower_damage}\n\
                         NW:  {net_worth}  Heal: {healing}\n\
                         Side: {capital_side}"
                    );
                    if !lane.is_empty() {
                        block.push_str(&format!("  Lane: {lane}"));
                    }
                    block.push_str("\n```");
                    if let Some(valve_id) = value_i64(&match_row.valve_match_id) {
                        block.push_str(&format!(
                            "[DB](https://www.dotabuff.com/matches/{valve_id})"
                        ));
                    }
                    block
                } else {
                    format!(
                        "```\nKDA: -/-/-  GPM: -\nDMG: -  TD: -\n\
                         NW: -  Side: {capital_side}\n```"
                    )
                };
                embed = embed.field(
                    format!(
                        "{result_emoji}{lobby_emoji} #{} • {hero}",
                        match_row.match_id
                    ),
                    field,
                    true,
                );
            }
            if let Some(thumbnail) =
                first_hero_id.and_then(|hero_id| hero_thumbnail_url(&hero_catalog, hero_id))
            {
                embed = embed.thumbnail(thumbnail);
            }
            embed = embed.footer("Use /matches view <id> for detailed match stats");
            Ok(InteractionResponse::message(String::new())
                .embed(embed)
                .ephemeral())
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        responder.followup(response).await.map_err(response_error)
    }

    async fn handle_recent(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if !defer(&responder, false).await {
            return Ok(());
        }
        let (target_id, target_name) =
            user_option(options, "user").unwrap_or((context.user_id, context.user_display_name));
        let limit = integer_option(options, "limit").unwrap_or(10).clamp(1, 20);
        let limit = usize::try_from(limit).unwrap_or(10);
        let steam = self.steam.clone();
        let api = self.opendota.player_api();
        let opendota = Arc::clone(&self.opendota);
        let response = run_blocking(move || {
            let steam_id = steam
                .get_steam_id(target_id)
                .map_err(|error| error.to_string())?;
            let Some(steam_id) = steam_id else {
                return Ok::<_, String>(InteractionResponse::message(format!(
                    "{target_name} hasn't linked their Steam account. Use `/player link` first."
                )));
            };
            let matches = api
                .projected_matches(SteamId(steam_id), limit)
                .unwrap_or_default();
            if matches.is_empty() {
                return Ok(InteractionResponse::message(format!(
                    "{target_name} hasn't linked their Steam account. Use `/player link` first."
                )));
            }
            let rows = matches
                .iter()
                .map(|item| MatchRow {
                    hero_id: item.hero_id,
                    hero_name: item.hero_id.map(|hero_id| {
                        opendota
                            .hero_name(hero_id)
                            .map_or_else(|| format!("Hero {hero_id}"), ToOwned::to_owned)
                    }),
                    kills: item.kills,
                    deaths: item.deaths,
                    assists: item.assists,
                    won: Some(did_win(item)),
                    duration_seconds: u64::try_from(item.duration_seconds).ok(),
                })
                .collect::<Vec<_>>();
            let hero_names = rows
                .iter()
                .filter_map(|row| Some((row.hero_id?, row.hero_name.clone()?)))
                .collect();
            let image = draw_matches_table(&rows, &hero_names).into_inner();
            Ok(InteractionResponse::message(String::new())
                .embed(
                    InteractionEmbed::titled(format!("Recent Matches: {target_name}"))
                        .color(DISCORD_BLUE)
                        .image("attachment://recent_matches.png")
                        .footer(format!("Showing {} most recent matches", matches.len())),
                )
                .attachment(InteractionAttachment::bytes("recent_matches.png", image)))
        })
        .await;
        let response = match response {
            Ok(response) => response,
            Err(message) => {
                error!(%message, "error generating recent matches image");
                InteractionResponse::message(
                    "❌ Couldn't generate the match image right now — try again in a moment.",
                )
            }
        };
        responder.followup(response).await.map_err(response_error)
    }
}

impl EnrichmentHandler {
    async fn handle_view(
        &self,
        context: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(guild_id) = self.require_guild(&context, &responder).await? else {
            return Ok(());
        };
        if !defer(&responder, false).await {
            return Ok(());
        }
        // Match the Python truthiness check: an explicit zero means "use the
        // latest match" rather than attempting to load internal match #0.
        let explicit_match_id = integer_option(options, "match_id").filter(|id| *id != 0);
        let selected_user = user_option(options, "user");
        let invoking_user_id = context.user_id;
        let (target_id, target_name, user_was_selected) = selected_user.map_or_else(
            || (context.user_id, context.user_display_name.clone(), false),
            |(id, name)| (id, name, true),
        );
        let matches = self.matches.clone();
        let bankruptcy = self.bankruptcy.clone();
        let opendota = Arc::clone(&self.opendota);
        let hero_catalog = Arc::clone(&self.hero_catalog);
        let timeout_seconds = i64::try_from(self.match_view_timeout.as_secs()).unwrap_or(i64::MAX);
        let response = run_blocking(move || {
            let match_data = if let Some(match_id) = explicit_match_id {
                let selected = matches
                    .get_match(match_id, Some(guild_id))
                    .map_err(|error| error.to_string())?;
                let Some(selected) = selected else {
                    return Ok::<_, String>(InteractionResponse::message(format!(
                        "Match #{match_id} not found."
                    )));
                };
                selected
            } else {
                let target_history = matches
                    .get_player_matches(target_id, Some(guild_id), 1)
                    .map_err(|error| error.to_string())?;
                let selected = if let Some(recent) = target_history.first() {
                    matches
                        .get_match(recent.match_id, Some(guild_id))
                        .map_err(|error| error.to_string())?
                } else {
                    let recent = matches
                        .get_most_recent_match(Some(guild_id))
                        .map_err(|error| error.to_string())?;
                    match recent {
                        Some(recent) => matches
                            .get_match(recent.match_id, Some(guild_id))
                            .map_err(|error| error.to_string())?,
                        None => None,
                    }
                };
                let Some(selected) = selected else {
                    return Ok(InteractionResponse::message(if user_was_selected {
                        format!("No matches found for {target_name}.")
                    } else {
                        "No matches found.".to_owned()
                    }));
                };
                selected
            };
            let participants = matches
                .get_match_participants(match_data.match_id, Some(guild_id))
                .map_err(|error| error.to_string())?;
            if participants.is_empty() {
                return Ok(InteractionResponse::message(format!(
                    "No participant data for match #{}.",
                    match_data.match_id
                )));
            }
            let is_enriched = participants
                .iter()
                .any(|participant| value_i64(&participant.hero_id).is_some_and(|id| id != 0));
            if !is_enriched {
                let (radiant, dire) =
                    embed_participants(&participants, &opendota, Some(guild_id));
                let summary = create_match_summary_embed_with_context(
                    match_data.match_id,
                    winning_team(&match_data.winning_team),
                    &radiant,
                    &dire,
                    match_data.valve_match_id,
                    Some(&bankruptcy),
                    match_data.lobby_type.as_deref().unwrap_or("shuffle"),
                    Some(guild_id),
                );
                let mut summary = embed_model(summary);
                summary.color = Some(match_color(&match_data.winning_team));
                return Ok(
                    InteractionResponse::message(
                        "ℹ️ This match has not been enriched yet. Use `/enrich match` to add detailed stats.",
                    )
                    .embed(summary),
                );
            }

            let mut embed = build_match_embed(
                &match_data,
                &participants,
                &bankruptcy,
                &opendota,
                &hero_catalog,
            );
            if let Some(thumbnail) = participants
                .iter()
                .find(|participant| participant.discord_id == invoking_user_id)
                .and_then(|participant| value_i64(&participant.hero_id))
                .filter(|hero_id| *hero_id != 0)
                .and_then(|hero_id| hero_thumbnail_url(&hero_catalog, hero_id))
            {
                embed.thumbnail_url = Some(thumbnail);
            }
            let advantage = matches
                .get_enrichment_data(match_data.match_id, Some(guild_id))
                .ok()
                .flatten()
                .as_ref()
                .map(advantage_data)
                .unwrap_or_default();
            let mut response = InteractionResponse::message(String::new()).embed(embed);
            if !advantage.radiant_gold.is_empty() || !advantage.radiant_xp.is_empty() {
                let expires_at = unix_now().saturating_add(timeout_seconds);
                response = response.action_row(match_view_buttons(
                    guild_id,
                    match_data.match_id,
                    expires_at,
                    0,
                ));
            }
            Ok(response)
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        let timeout_response = disabled_component_response(&response);
        let receipt = responder
            .followup_with_receipt(response)
            .await
            .map_err(response_error)?;
        if let (Some(receipt), Some(timeout_response)) = (receipt, timeout_response) {
            let responder = Arc::clone(&responder);
            let timeout = self.match_view_timeout;
            tokio::spawn(async move {
                tokio::time::sleep(timeout).await;
                if let Err(response_error) = responder.edit_message(receipt, timeout_response).await
                {
                    warn!(%response_error, "unable to disable expired match view controls");
                }
            });
        }
        Ok(())
    }

    async fn handle_component(
        &self,
        custom_id: &str,
        interaction_guild_id: Option<u64>,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let Some(route) = MatchViewRoute::parse(custom_id) else {
            return Err("invalid enrichment component route".into());
        };
        let interaction_guild_id = interaction_guild_id
            .map(|id| signed_id(id, "guild"))
            .transpose()?;
        if interaction_guild_id != Some(route.guild_id) {
            return respond_initial(
                &responder,
                InteractionResponse::message("This match view belongs to another server.")
                    .ephemeral(),
            )
            .await;
        }
        if unix_now() >= route.expires_at {
            return respond_initial(
                &responder,
                InteractionResponse::message("This match view has expired.").ephemeral(),
            )
            .await;
        }
        if route.page == 1 {
            let matches = self.matches.clone();
            let data = run_blocking(move || {
                matches.get_enrichment_data(route.match_id, Some(route.guild_id))
            })
            .await
            .map_err(InteractionHandlerError::from)?;
            let advantage = data.as_ref().map(advantage_data).unwrap_or_default();
            let Some(image) = draw_advantage_graph(&advantage, Some(route.match_id))
                .map(|image| image.into_inner())
            else {
                if let Err(response_error) = responder.defer(false).await {
                    warn!(%response_error, "unable to acknowledge empty match graph");
                }
                return Ok(());
            };
            return responder
                .update(
                    InteractionResponse::message(String::new())
                        .embed(
                            InteractionEmbed::default()
                                .color(DISCORD_DARKER)
                                .image("attachment://advantage.png")
                                .footer(format!(
                                    "Match #{} — Team Advantages Per Minute",
                                    route.match_id
                                )),
                        )
                        .attachment(InteractionAttachment::bytes("advantage.png", image))
                        .action_row(match_view_buttons(
                            route.guild_id,
                            route.match_id,
                            route.expires_at,
                            1,
                        )),
                )
                .await
                .map_err(response_error);
        }

        let matches = self.matches.clone();
        let bankruptcy = self.bankruptcy.clone();
        let opendota = Arc::clone(&self.opendota);
        let hero_catalog = Arc::clone(&self.hero_catalog);
        let response = run_blocking(move || {
            let match_data = matches
                .get_match(route.match_id, Some(route.guild_id))
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("Match #{} not found.", route.match_id))?;
            let participants = matches
                .get_match_participants(route.match_id, Some(route.guild_id))
                .map_err(|error| error.to_string())?;
            let embed = build_match_embed(
                &match_data,
                &participants,
                &bankruptcy,
                &opendota,
                &hero_catalog,
            );
            Ok::<_, String>(
                InteractionResponse::message(String::new())
                    .embed(embed)
                    .action_row(match_view_buttons(
                        route.guild_id,
                        route.match_id,
                        route.expires_at,
                        0,
                    )),
            )
        })
        .await
        .map_err(InteractionHandlerError::from)?;
        responder.update(response).await.map_err(response_error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MatchViewRoute {
    guild_id: i64,
    match_id: i64,
    expires_at: i64,
    page: u8,
}

impl MatchViewRoute {
    fn parse(custom_id: &str) -> Option<Self> {
        let mut parts = custom_id.strip_prefix(COMPONENT_PREFIX)?.split(':');
        if parts.next()? != "match" {
            return None;
        }
        let route = Self {
            guild_id: parts.next()?.parse().ok()?,
            match_id: parts.next()?.parse().ok()?,
            expires_at: parts.next()?.parse().ok()?,
            page: parts.next()?.parse().ok()?,
        };
        (parts.next().is_none() && route.page <= 1).then_some(route)
    }
}

fn match_view_buttons(
    guild_id: i64,
    match_id: i64,
    expires_at: i64,
    page: u8,
) -> InteractionActionRow {
    InteractionActionRow::buttons(vec![
        InteractionButton::new(
            format!("{COMPONENT_PREFIX}match:{guild_id}:{match_id}:{expires_at}:0"),
            "< Prev",
        )
        .style(InteractionButtonStyle::Secondary)
        .disabled(page == 0),
        InteractionButton::new(
            format!("{COMPONENT_PREFIX}match:{guild_id}:{match_id}:{expires_at}:1"),
            "Next >",
        )
        .style(InteractionButtonStyle::Primary)
        .disabled(page == 1),
    ])
}

fn disabled_component_response(response: &InteractionResponse) -> Option<InteractionResponse> {
    if response.components.is_empty() {
        return None;
    }
    let mut rows = response.components.clone();
    for row in &mut rows {
        for button in &mut row.buttons {
            button.disabled = true;
        }
    }
    Some(InteractionResponse::message(String::new()).action_rows(rows))
}

fn selected_subcommand(
    options: &[InteractionOption],
) -> Result<(&str, &[InteractionOption]), InteractionHandlerError> {
    options
        .iter()
        .find_map(|option| match &option.value {
            InteractionValue::Subcommand(children) => {
                Some((option.name.as_str(), children.as_slice()))
            }
            _ => None,
        })
        .ok_or_else(|| "missing enrichment subcommand".into())
}

fn integer_option(options: &[InteractionOption], name: &str) -> Option<i64> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::Integer(value) => Some(*value),
                _ => None,
            })
    })
}

fn boolean_option(options: &[InteractionOption], name: &str) -> Option<bool> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::Boolean(value) => Some(*value),
                _ => None,
            })
    })
}

fn user_option(options: &[InteractionOption], name: &str) -> Option<(i64, String)> {
    options.iter().find_map(|option| {
        if option.name != name {
            return None;
        }
        let InteractionValue::User {
            id, display_name, ..
        } = &option.value
        else {
            return None;
        };
        let id = i64::try_from(*id).ok()?;
        Some((id, display_name.clone().unwrap_or_else(|| id.to_string())))
    })
}

fn signed_id(id: u64, label: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(id).map_err(|_| format!("Discord {label} id exceeds SQLite range").into())
}

async fn defer(responder: &Arc<dyn InteractionResponder>, ephemeral: bool) -> bool {
    match responder.defer(ephemeral).await {
        Ok(()) => true,
        Err(response_error) => {
            warn!(%response_error, "unable to defer enrichment interaction");
            false
        }
    }
}

async fn respond_initial(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
) -> Result<(), InteractionHandlerError> {
    responder.respond(response).await.map_err(response_error)
}

fn response_error(error: crate::registration::InteractionResponseError) -> InteractionHandlerError {
    error.to_string().into()
}

async fn run_blocking<T, E, F>(operation: F) -> Result<T, String>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: FnOnce() -> Result<T, E> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| format!("enrichment blocking task failed: {error}"))?
        .map_err(|error| error.to_string())
}

fn format_discovery(result: &BatchDiscoveryResult, dry_run: bool) -> String {
    let mut lines = vec![
        format!(
            "**Match Discovery {}**",
            if dry_run { "(DRY RUN)" } else { "Complete" }
        ),
        String::new(),
        format!("Total unenriched: {}", result.total_unenriched),
        format!("Discovered: {}", result.discovered),
        format!(
            "Skipped (low confidence): {}",
            result.skipped_low_confidence
        ),
        format!(
            "Skipped (validation failed): {}",
            result.skipped_validation_failed
        ),
        format!("Skipped (no steam IDs): {}", result.skipped_no_steam_ids),
        format!("Errors: {}", result.errors),
    ];
    let discovered = result
        .details
        .iter()
        .filter(|detail| detail.status == DiscoveryStatus::Discovered)
        .collect::<Vec<_>>();
    if !discovered.is_empty() {
        lines.extend([String::new(), "**Discovered Matches:**".to_owned()]);
        lines.extend(discovered.iter().take(10).filter_map(|detail| {
            Some(format!(
                "  #{} → {} ({:.0}% - {}/{} players)",
                detail.match_id.0,
                detail.valve_match_id?.0,
                detail.confidence.unwrap_or(0.0) * 100.0,
                detail.player_count,
                detail.total_players
            ))
        }));
        if discovered.len() > 10 {
            lines.push(format!("  ... and {} more", discovered.len() - 10));
        }
    }
    let low_confidence = result
        .details
        .iter()
        .filter(|detail| {
            detail.status == DiscoveryStatus::LowConfidence && detail.valve_match_id.is_some()
        })
        .collect::<Vec<_>>();
    if !low_confidence.is_empty() {
        lines.extend([
            String::new(),
            "**⚠️ Low Confidence (needs manual review):**".to_owned(),
        ]);
        lines.extend(low_confidence.iter().take(5).filter_map(|detail| {
            Some(format!(
                "  #{} → {} ({:.0}% - {}/{} players)",
                detail.match_id.0,
                detail.valve_match_id?.0,
                detail.confidence.unwrap_or(0.0) * 100.0,
                detail.player_count,
                detail.total_players
            ))
        }));
        if low_confidence.len() > 5 {
            lines.push(format!("  ... and {} more", low_confidence.len() - 5));
        }
        lines.push("*Use `/enrich match` to manually enrich these matches*".to_owned());
    }
    let validation_failed = result
        .details
        .iter()
        .filter(|detail| detail.status == DiscoveryStatus::ValidationFailed)
        .collect::<Vec<_>>();
    if !validation_failed.is_empty() {
        lines.extend([String::new(), "**❌ Validation Failed:**".to_owned()]);
        lines.extend(validation_failed.iter().take(5).map(|detail| {
            format!(
                "  #{} → {}: {}",
                detail.match_id.0,
                detail
                    .valve_match_id
                    .map_or_else(|| "?".to_owned(), |id| id.0.to_string()),
                detail.validation_error.as_deref().unwrap_or("Unknown")
            )
        }));
        if validation_failed.len() > 5 {
            lines.push(format!("  ... and {} more", validation_failed.len() - 5));
        }
    }
    lines.join("\n")
}

fn build_match_embed(
    match_data: &MatchSummary,
    participants: &[MatchParticipant],
    bankruptcy: &BankruptcySource,
    opendota: &OpenDotaRuntimeServices,
    hero_catalog: &HeroCatalog,
) -> InteractionEmbed {
    let guild_id = participants.first().map(|participant| participant.guild_id);
    let (radiant, dire) = embed_participants(participants, opendota, guild_id);
    let model = create_enriched_match_embed(
        &EnrichedMatchRequest {
            match_id: match_data.match_id,
            valve_match_id: match_data.valve_match_id,
            duration_seconds: match_data.duration_seconds,
            radiant_score: match_data.radiant_score,
            dire_score: match_data.dire_score,
            winning_team: winning_team(&match_data.winning_team),
            show_mvp: true,
            draft: match_data.lobby_type.as_deref() == Some("draft"),
            guild_id,
        },
        &radiant,
        &dire,
        Some(bankruptcy),
    );
    let mut embed = embed_model(model);
    embed.color = Some(match_color(&match_data.winning_team));
    let winning_side = if winning_team(&match_data.winning_team) == 1 {
        "radiant"
    } else {
        "dire"
    };
    let mut mvp: Option<&MatchParticipant> = None;
    for participant in participants
        .iter()
        .filter(|participant| value_text(&participant.side) == Some(winning_side))
    {
        let damage = value_i64(&participant.hero_damage).unwrap_or_default();
        if mvp.is_none_or(|current| damage > value_i64(&current.hero_damage).unwrap_or_default()) {
            mvp = Some(participant);
        }
    }
    if let Some(thumbnail) = mvp
        .and_then(|participant| value_i64(&participant.hero_id))
        .filter(|hero_id| *hero_id != 0)
        .and_then(|hero_id| hero_thumbnail_url(hero_catalog, hero_id))
    {
        embed.thumbnail_url = Some(thumbnail);
    }
    embed
}

fn match_color(winning_team_value: &Value) -> u32 {
    if winning_team(winning_team_value) == 1 {
        DISCORD_GREEN
    } else {
        DISCORD_RED
    }
}

fn hero_thumbnail_url(catalog: &HeroCatalog, hero_id: i64) -> Option<String> {
    catalog
        .get_hero_by_id(hero_id)
        .ok()
        .flatten()
        .and_then(|hero| hero.image_url.clone())
}

fn embed_participants(
    participants: &[MatchParticipant],
    opendota: &OpenDotaRuntimeServices,
    guild_id: Option<i64>,
) -> (Vec<EmbedParticipant>, Vec<EmbedParticipant>) {
    let mut radiant = Vec::new();
    let mut dire = Vec::new();
    for participant in participants {
        let hero_id = value_i64(&participant.hero_id);
        let projected = EmbedParticipant {
            discord_id: Some(participant.discord_id),
            guild_id: Some(participant.guild_id).or(guild_id),
            hero_id,
            hero_name: hero_id
                .and_then(|hero_id| opendota.hero_name(hero_id))
                .map(ToOwned::to_owned),
            kills: value_i64(&participant.kills),
            deaths: value_i64(&participant.deaths),
            assists: value_i64(&participant.assists),
            hero_damage: value_i64(&participant.hero_damage),
            net_worth: value_i64(&participant.net_worth),
            lane_role: value_i64(&participant.lane_role),
            lane_efficiency: value_i64(&participant.lane_efficiency),
        };
        if value_text(&participant.side) == Some("radiant") {
            radiant.push(projected);
        } else {
            dire.push(projected);
        }
    }
    (radiant, dire)
}

fn embed_model(model: EmbedModel) -> InteractionEmbed {
    InteractionEmbed {
        title: model.title,
        description: model.description,
        color: None,
        author_name: model.author,
        author_icon_url: None,
        image_url: None,
        thumbnail_url: None,
        footer: model.footer,
        fields: model
            .fields
            .into_iter()
            .map(|field| crate::registration::InteractionEmbedField {
                name: field.name,
                value: field.value,
                inline: field.inline,
            })
            .collect(),
    }
}

fn winning_team(value: &Value) -> i64 {
    value_i64(value).unwrap_or(2)
}

fn value_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) => Some(*value),
        Value::Real(value) if value.is_finite() => Some(*value as i64),
        Value::Text(value) => value.parse().ok(),
        Value::Null | Value::Blob(_) | Value::Real(_) => None,
    }
}

fn value_text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(value) => Some(value.as_str()),
        _ => None,
    }
}

fn compact_number(value: Option<i64>) -> String {
    match value.unwrap_or(0) {
        value if value >= 1_000 => format!("{:.1}k", value as f64 / 1_000.0),
        value => value.to_string(),
    }
}

fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    first
        .to_uppercase()
        .chain(characters.flat_map(char::to_lowercase))
        .collect()
}

fn history_lane_label(match_row: &PlayerMatch, target_id: i64) -> String {
    let Some(lane_role) = value_i64(&match_row.lane_role) else {
        return String::new();
    };
    let lane_name = match lane_role {
        0 => "Roam",
        1 => "Safe",
        2 => "Mid",
        3 => "Off",
        4 => "Jgl",
        _ => "",
    };
    if lane_name.is_empty() || value_i64(&match_row.lane_efficiency).is_none() {
        return lane_name.to_owned();
    }
    let radiant = match_row
        .participants
        .iter()
        .filter(|participant| value_text(&participant.side) == Some("radiant"))
        .collect::<Vec<_>>();
    let dire = match_row
        .participants
        .iter()
        .filter(|participant| value_text(&participant.side) == Some("dire"))
        .collect::<Vec<_>>();
    let target_side = value_text(&match_row.side).unwrap_or("dire");
    let team = if target_side == "radiant" {
        &radiant
    } else {
        &dire
    };
    let Some(team_index) = team
        .iter()
        .position(|participant| participant.discord_id == target_id)
    else {
        return lane_name.to_owned();
    };
    let outcomes = history_lane_outcomes(&radiant, &dire);
    outcomes
        .get(&(target_side == "radiant", team_index))
        .map_or_else(
            || lane_name.to_owned(),
            |outcome| format!("{lane_name} {outcome}"),
        )
}

fn history_lane_outcomes(
    radiant: &[&PlayerMatchParticipant],
    dire: &[&PlayerMatchParticipant],
) -> BTreeMap<(bool, usize), char> {
    fn lanes(participants: &[&PlayerMatchParticipant]) -> BTreeMap<i64, Vec<(usize, i64)>> {
        let mut lanes: BTreeMap<i64, Vec<(usize, i64)>> = BTreeMap::new();
        for (index, participant) in participants.iter().enumerate() {
            if let (Some(lane @ 1..=3), Some(efficiency)) = (
                value_i64(&participant.lane_role),
                value_i64(&participant.lane_efficiency),
            ) {
                lanes.entry(lane).or_default().push((index, efficiency));
            }
        }
        lanes
    }
    fn average(values: &[(usize, i64)]) -> f64 {
        values.iter().map(|(_, value)| *value as f64).sum::<f64>() / values.len() as f64
    }
    let radiant_lanes = lanes(radiant);
    let dire_lanes = lanes(dire);
    let mut outcomes = BTreeMap::new();
    for (radiant_lane, dire_lane) in [(1, 3), (2, 2), (3, 1)] {
        let (Some(radiant_players), Some(dire_players)) =
            (radiant_lanes.get(&radiant_lane), dire_lanes.get(&dire_lane))
        else {
            continue;
        };
        let (radiant_outcome, dire_outcome) =
            if average(radiant_players) > average(dire_players) + 5.0 {
                ('W', 'L')
            } else if average(dire_players) > average(radiant_players) + 5.0 {
                ('L', 'W')
            } else {
                ('D', 'D')
            };
        outcomes.extend(
            radiant_players
                .iter()
                .map(|(index, _)| ((true, *index), radiant_outcome)),
        );
        outcomes.extend(
            dire_players
                .iter()
                .map(|(index, _)| ((false, *index), dire_outcome)),
        );
    }
    outcomes
}

fn advantage_data(value: &EnrichmentJsonValue) -> AdvantageData {
    let EnrichmentJsonValue::Object(object) = value else {
        return AdvantageData::default();
    };
    AdvantageData {
        radiant_gold: json_number_array(object.get("radiant_gold_adv")),
        radiant_xp: json_number_array(object.get("radiant_xp_adv")),
    }
}

fn json_number_array(value: Option<&EnrichmentJsonValue>) -> Vec<f64> {
    let Some(EnrichmentJsonValue::Array(values)) = value else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            EnrichmentJsonValue::Integer(value) => Some(*value as f64),
            EnrichmentJsonValue::Float(value) if value.is_finite() => Some(*value),
            _ => None,
        })
        .collect()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

#[cfg(test)]
#[path = "enrichment_provider/tests.rs"]
mod tests;
