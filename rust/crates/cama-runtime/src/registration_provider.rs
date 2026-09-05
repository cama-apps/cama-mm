//! Production Discord composition for player registration and preferences.
//!
//! This is the Rust lift of `commands.registration`: it owns the complete
//! slash-command tree plus the persistent button/modal callback lifecycle.
//! Database adapters open only the schema admitted by runtime startup.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::admin_low_priority::player_visible_reason;
use cama_app::curfew_service::{CurfewService, CurfewServiceError};
use cama_app::moderation::ModerationService;
use cama_app::neon_degen::{EventKey, NeonDegenService, NeonEventPort, StoreError};
use cama_app::opendota_http::OpenDotaRuntimeServices;
use cama_app::player_mmr_fallback::{
    RegisterPlayerError, RegisterPlayerInput, RegisterPlayerResult, register_player,
};
use cama_app::playtime_service::{PlaytimeService, PlaytimeServiceError};
use cama_app::registration::{RoleInputError, parse_roles};
use cama_app::timezone_service::{TimezoneService, TimezoneServiceError};
use cama_db::core_repositories::PlayerRepository;
use cama_db::curfew::CurfewRepository;
use cama_db::low_priority_repository::{LowPriorityRepository, LowPriorityState};
use cama_db::moderation::{
    LobbySuspension, ModerationRepository, SuspensionCompletion, SuspensionScope,
};
use cama_db::neon_events::NeonEventRepository;
use cama_db::notifications::{NotificationKind, NotificationRepository};
use cama_db::opendota_player::{OpenDotaPlayerRepository, OpenDotaPlayerRepositoryError};
use cama_db::referrals::ReferralRepository;
use cama_db::registration_repository::{
    MmrPromptState, RegistrationRepository, STEAM_ACCOUNT_ID_UPPER_BOUND, SetRolesOutcome,
};
use cama_domain::formatting::{escape_discord_text, format_role_display};
use cama_domain::openskill::CamaOpenSkillSystem;
use cama_domain::rating::CamaRatingSystem;
use cama_domain::region::{
    CountsPayload, GameCountValue, RegionCountEntry, infer_region_from_counts,
};
use serde_json::Value as JsonValue;
use tracing::{debug, error, warn};

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{
    DiscordDirectMessageErrorKind, DiscordMessage, DiscordTransport, GuildPlayerNameDirectory,
};
use crate::gateway_events::{
    GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryFailure, ReadyRecoveryReport,
};
use crate::option_ext::{boolean_option, integer_option, string_option, user_option};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionAcknowledgementPolicy, InteractionActionRow, InteractionButton, InteractionHandler,
    InteractionHandlerError, InteractionModal, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionTextInput, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};
use crate::runtime_ports::{
    ConfirmedLobbyJoin, DraftNeonObserver, DraftNeonResult, LobbyGambaSpectator, LobbyJoinObserver,
};

const MMR_COMPONENT_PREFIX: &str = "registration:mmr:";
const REFER_COOLDOWN: Duration = Duration::from_secs(5);
const NEON_DELETE_DELAY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerRegistrationConfig {
    pub mmr_modal_retry_limit: i64,
    pub mmr_modal_timeout: Duration,
    pub new_player_exclusion_boost: i64,
    pub neon_degen_enabled: bool,
    pub lobby_rally_cooldown: Duration,
    pub lobby_ready_cooldown: Duration,
    pub lobby_channel_id: Option<i64>,
    pub low_skill_lobby_channel_id: Option<i64>,
    pub rating_system: CamaRatingSystem,
    pub openskill: CamaOpenSkillSystem,
}

impl PlayerRegistrationConfig {
    #[must_use]
    pub fn from_application_config(config: &ApplicationConfig) -> Self {
        Self {
            mmr_modal_retry_limit: config.values.mmr_modal_retry_limit.max(1),
            mmr_modal_timeout: Duration::from_secs(
                config
                    .values
                    .mmr_modal_timeout_minutes
                    .max(1)
                    .saturating_mul(60) as u64,
            ),
            new_player_exclusion_boost: config.values.new_player_exclusion_boost,
            neon_degen_enabled: config.values.neon_degen_enabled,
            lobby_rally_cooldown: Duration::from_secs(
                config.values.lobby_rally_cooldown_seconds.max(0) as u64,
            ),
            lobby_ready_cooldown: Duration::from_secs(
                config.values.lobby_ready_cooldown_seconds.max(0) as u64,
            ),
            lobby_channel_id: config.channels.lobby,
            // Registration seeds must use the env-configured systems; the
            // config surface is fail-soft like the rest of `Values`, so the
            // single pathological overflow error falls back to defaults.
            rating_system: config
                .glicko_rating_system()
                .unwrap_or_else(|_| CamaRatingSystem::default()),
            openskill: CamaOpenSkillSystem::with_config(config.migration.openskill.clone())
                .unwrap_or_default(),
            low_skill_lobby_channel_id: config.channels.low_skill_lobby,
        }
    }
}

#[derive(Clone)]
pub struct PlayerRegistrationProvider {
    handler: Arc<PlayerRegistrationHandler>,
}

impl PlayerRegistrationProvider {
    /// Compose the production provider from the admitted SQLite path, shared
    /// OpenDota services, typed application config, and live Discord transport.
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        opendota: Arc<OpenDotaRuntimeServices>,
        discord: Arc<dyn DiscordTransport>,
        application_config: &ApplicationConfig,
    ) -> Self {
        Self::with_config(
            database_path,
            opendota,
            discord,
            PlayerRegistrationConfig::from_application_config(application_config),
        )
    }

    #[must_use]
    pub fn with_config(
        database_path: impl AsRef<Path>,
        opendota: Arc<OpenDotaRuntimeServices>,
        discord: Arc<dyn DiscordTransport>,
        config: PlayerRegistrationConfig,
    ) -> Self {
        let path = database_path.as_ref();
        let neon_port: Arc<dyn NeonEventPort> = Arc::new(SqliteNeonEventPort {
            repository: NeonEventRepository::new(path),
        });
        let mut neon = NeonDegenService::with_event_port(now_nanos(), neon_port);
        neon.set_enabled(config.neon_degen_enabled);
        Self {
            handler: Arc::new(PlayerRegistrationHandler {
                registration: RegistrationRepository::new(path),
                steam: OpenDotaPlayerRepository::new(path),
                referrals: ReferralRepository::new(path),
                notifications: NotificationRepository::new(path),
                moderation: ModerationRepository::new(path),
                low_priority: LowPriorityRepository::new(path),
                players: PlayerRepository::new(path),
                curfew: CurfewService::new(CurfewRepository::new(path)),
                opendota,
                discord,
                config,
                refer_cooldowns: Mutex::new(BTreeMap::new()),
                neon: Mutex::new(neon),
                rally_cooldowns: Mutex::new(BTreeMap::new()),
                ready_cooldowns: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    /// Side-effect hook that must be attached to `LobbyRegistrationProvider`
    /// before command registration is considered a complete extension.
    #[must_use]
    pub fn lobby_join_observer(&self) -> Arc<dyn LobbyJoinObserver> {
        self.handler.clone()
    }

    /// Re-run the idempotent inferred-region backfill on READY/resume.
    #[must_use]
    pub fn region_backfill_observer(&self) -> Arc<dyn GatewayEventObserver> {
        Arc::new(RegionBackfillObserver {
            registration: self.handler.registration.clone(),
            opendota: Arc::clone(&self.handler.opendota),
        })
    }

    /// Reuse this provider's SQLite-backed Neon service for Draft's typed
    /// event hooks. Keeping the adapter here preserves the existing Neon
    /// event store and enablement policy instead of constructing a second
    /// process-local Neon service during runtime composition.
    #[must_use]
    pub fn draft_neon_observer(&self) -> Arc<dyn DraftNeonObserver> {
        Arc::new(RegistrationDraftNeonObserver {
            handler: Arc::clone(&self.handler),
        })
    }
}

struct RegionBackfillObserver {
    registration: RegistrationRepository,
    opendota: Arc<OpenDotaRuntimeServices>,
}

#[async_trait]
impl GatewayEventObserver for RegionBackfillObserver {
    fn name(&self) -> &'static str {
        "player-region-backfill"
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let ready_guild_ids = context
            .guild_ids()
            .iter()
            .filter_map(|guild_id| i64::try_from(*guild_id).ok())
            .collect::<BTreeSet<_>>();
        let repository = self.registration.clone();
        let candidates = match tokio::task::spawn_blocking(move || {
            repository
                .region_backfill_candidates_for_guilds(&ready_guild_ids)
                .map_err(|error| error.to_string())
        })
        .await
        {
            Ok(Ok(candidates)) => candidates,
            Ok(Err(message)) => {
                return region_backfill_failure(context.guild_ids().len(), message);
            }
            Err(error) => {
                return region_backfill_failure(
                    context.guild_ids().len(),
                    format!("region candidate task failed: {error}"),
                );
            }
        };
        let attempted_guilds = candidates
            .iter()
            .filter_map(|candidate| u64::try_from(candidate.guild_id).ok())
            .collect::<BTreeSet<_>>();
        let mut by_steam = BTreeMap::<i64, Vec<_>>::new();
        let mut steam_order = Vec::new();
        for candidate in candidates {
            if !by_steam.contains_key(&candidate.steam_id) {
                steam_order.push(candidate.steam_id);
            }
            by_steam
                .entry(candidate.steam_id)
                .or_default()
                .push(candidate);
        }
        let mut updates = Vec::new();
        for steam_id in steam_order {
            let candidates = by_steam
                .remove(&steam_id)
                .expect("first-seen Steam ID was inserted");
            let Some(raw_counts) = self
                .opendota
                .shared_client()
                .get_player_counts(steam_id)
                .await
            else {
                continue;
            };
            let counts = project_counts_payload(&raw_counts);
            let Some(region) = infer_region_from_counts(Some(&counts)) else {
                continue;
            };
            updates.extend(
                candidates
                    .into_iter()
                    .map(|candidate| (candidate.discord_id, candidate.guild_id, region.to_owned())),
            );
        }
        let repository = self.registration.clone();
        let updated = match tokio::task::spawn_blocking(move || {
            repository
                .set_inferred_regions_bulk(&updates)
                .map_err(|error| error.to_string())
        })
        .await
        {
            Ok(Ok(updated)) => updated,
            Ok(Err(message)) => {
                return region_backfill_failure(attempted_guilds.len(), message);
            }
            Err(error) => {
                return region_backfill_failure(
                    attempted_guilds.len(),
                    format!("region bulk-write task failed: {error}"),
                );
            }
        };
        ReadyRecoveryReport {
            observer: self.name(),
            guilds_attempted: attempted_guilds.len(),
            guilds_refreshed: attempted_guilds.len(),
            guilds_superseded: 0,
            members_refreshed: updated,
            failures: Vec::new(),
        }
    }
}

fn region_backfill_failure(guilds_attempted: usize, message: String) -> ReadyRecoveryReport {
    ReadyRecoveryReport {
        observer: "player-region-backfill",
        guilds_attempted,
        guilds_refreshed: 0,
        guilds_superseded: 0,
        members_refreshed: 0,
        failures: vec![ReadyRecoveryFailure {
            guild_id: 0,
            message,
        }],
    }
}

impl RegistrationProvider for PlayerRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "refer".to_owned(),
            description: "Refer a player before their first game".to_owned(),
            options: vec![required_option("player", "…", CommandOptionKind::User)],
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "player".to_owned(),
            description: "Player registration and profile management".to_owned(),
            options: player_command_options(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: MMR_COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

pub(crate) fn player_command_options() -> Vec<CommandOptionSpec> {
    let mut options = vec![
        subcommand(
            "register",
            "Register yourself as a player",
            vec![required_option(
                "steam_id",
                "Steam32 ID (found in your Dotabuff URL)",
                CommandOptionKind::Integer,
            )],
        ),
        subcommand(
            "link",
            "Link an additional Steam account",
            vec![
                required_option(
                    "steam_id",
                    "Steam32 ID (found in your Dotabuff URL)",
                    CommandOptionKind::Integer,
                ),
                CommandOptionSpec::new(
                    "set_primary",
                    "Set as your primary Steam account (default: False)",
                    CommandOptionKind::Boolean,
                ),
            ],
        ),
        subcommand(
            "unlink",
            "Remove a linked Steam account",
            vec![required_option(
                "steam_id",
                "Steam32 ID to remove",
                CommandOptionKind::Integer,
            )],
        ),
        subcommand("steamids", "View your linked Steam accounts", Vec::new()),
        subcommand(
            "roles",
            "Set your preferred roles",
            vec![required_option(
                "roles",
                "Roles (1-5, e.g., '123' or '1,2,3' for carry, mid, offlane)",
                CommandOptionKind::String,
            )],
        ),
        subcommand(
            "region",
            "Set your preferred Dota server (US East / US West)",
            vec![CommandOptionSpec {
                choices: vec![
                    CommandOptionChoice::String {
                        name: "US East".to_owned(),
                        value: "USE".to_owned(),
                    },
                    CommandOptionChoice::String {
                        name: "US West".to_owned(),
                        value: "USW".to_owned(),
                    },
                ],
                ..CommandOptionSpec::new(
                    "region",
                    "Your preferred server — leave blank to see your current setting",
                    CommandOptionKind::String,
                )
            }],
        ),
        subcommand("exclusion", "Check your exclusion factor", Vec::new()),
        CommandOptionSpec::new(
            "lobby",
            "Lobby notification preferences",
            CommandOptionKind::SubcommandGroup,
        )
        .options(vec![
            subcommand(
                "autonotify",
                "Manage public lobby alerts or watch a player's next signup",
                vec![
                    CommandOptionSpec::new(
                        "enabled",
                        "Enable or disable persistent lobby notifications",
                        CommandOptionKind::Boolean,
                    ),
                    CommandOptionSpec::new(
                        "playername",
                        "DM you once when this player next joins a lobby",
                        CommandOptionKind::User,
                    ),
                ],
            ),
            subcommand(
                "status",
                "Privately view your active lobby suspension and low-priority progress",
                Vec::new(),
            ),
        ]),
        CommandOptionSpec::new(
            "curfew",
            "Named windows during which you're auto-locked and auto-removed from lobbies",
            CommandOptionKind::SubcommandGroup,
        )
        .options(vec![
            subcommand(
                "add",
                "Add (or update) a named curfew window",
                vec![
                    required_option(
                        "name",
                        "A name for this window (your choice) — reuse a name to update that window",
                        CommandOptionKind::String,
                    ),
                    required_option(
                        "start",
                        "Start time, 24-hour HH:MM",
                        CommandOptionKind::String,
                    ),
                    required_option(
                        "end",
                        "End time, 24-hour HH:MM — queueing unlocks then",
                        CommandOptionKind::String,
                    ),
                    CommandOptionSpec::new(
                        "timezone",
                        "IANA timezone name — leave blank to use your /player timezone setting",
                        CommandOptionKind::String,
                    ),
                    CommandOptionSpec::new(
                        "days",
                        "Nights it starts, e.g. 'Sa,Su' — overnight spans run into the next morning; blank = every day",
                        CommandOptionKind::String,
                    ),
                    curfew_mode_choices(CommandOptionSpec::new(
                        "mode",
                        "default (block), strict (shrinking edits wait until next morning), informational (asks first)",
                        CommandOptionKind::String,
                    )),
                ],
            ),
            subcommand(
                "remove",
                "Remove one of your named curfew windows",
                vec![required_option(
                    "name",
                    "The window's name",
                    CommandOptionKind::String,
                )],
            ),
            subcommand("list", "View your named curfew windows", Vec::new()),
        ]),
        CommandOptionSpec::new(
            "timezone",
            "Your timezone, used by curfew windows and other time-based features",
            CommandOptionKind::SubcommandGroup,
        )
        .options(vec![
            subcommand(
                "set",
                "Set your timezone, used by curfew windows and other time-based features",
                vec![required_option(
                    "timezone",
                    "IANA timezone name, e.g. America/New_York",
                    CommandOptionKind::String,
                )],
            ),
            subcommand("status", "View your timezone setting", Vec::new()),
            subcommand(
                "list",
                "See common timezone names to use with /player timezone set",
                Vec::new(),
            ),
        ]),
        CommandOptionSpec::new(
            "playtime",
            "Your preferred dota hours (informational) and the group's most popular times",
            CommandOptionKind::SubcommandGroup,
        )
        .options(vec![
            subcommand(
                "set",
                "Set the hours you like to play dota — informational only, doesn't restrict queueing",
                vec![required_option(
                    "hours",
                    "Hours you're usually free, 24-hour, your own timezone — e.g. '18-23' or '14,20,21'",
                    CommandOptionKind::String,
                )],
            ),
            subcommand("clear", "Clear your dota play-time hours", Vec::new()),
            subcommand("status", "View your dota play-time hours", Vec::new()),
            subcommand(
                "popular",
                "See the group's most popular hours to play dota",
                Vec::new(),
            ),
        ]),
    ];
    // `player_lobby`, `player_curfew`, `player_timezone`, and
    // `player_playtime` are all constructed with `parent=player` before
    // Python's decorated leaf commands are evaluated, so Discord receives
    // them first, in declaration order.
    let playtime = options.pop().expect("player playtime option is present");
    let timezone = options.pop().expect("player timezone option is present");
    let curfew = options.pop().expect("player curfew option is present");
    let lobby = options.pop().expect("player lobby option is present");
    options.insert(0, lobby);
    options.insert(1, curfew);
    options.insert(2, timezone);
    options.insert(3, playtime);
    options
}

fn subcommand(
    name: &'static str,
    description: &'static str,
    options: Vec<CommandOptionSpec>,
) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
}

fn curfew_mode_choices(mut option: CommandOptionSpec) -> CommandOptionSpec {
    option.choices = vec![
        CommandOptionChoice::String {
            name: "default".to_owned(),
            value: "default".to_owned(),
        },
        CommandOptionChoice::String {
            name: "strict".to_owned(),
            value: "strict".to_owned(),
        },
        CommandOptionChoice::String {
            name: "informational".to_owned(),
            value: "informational".to_owned(),
        },
    ];
    option
}

fn required_option(
    name: &'static str,
    description: &'static str,
    kind: CommandOptionKind,
) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, kind).required(true)
}

struct PlayerRegistrationHandler {
    registration: RegistrationRepository,
    steam: OpenDotaPlayerRepository,
    referrals: ReferralRepository,
    notifications: NotificationRepository,
    moderation: ModerationRepository,
    low_priority: LowPriorityRepository,
    players: PlayerRepository,
    curfew: CurfewService,
    opendota: Arc<OpenDotaRuntimeServices>,
    discord: Arc<dyn DiscordTransport>,
    config: PlayerRegistrationConfig,
    refer_cooldowns: Mutex<BTreeMap<u64, Instant>>,
    neon: Mutex<NeonDegenService>,
    rally_cooldowns: Mutex<BTreeMap<(u64, LobbyKindKey, usize, u64), Instant>>,
    ready_cooldowns: Mutex<BTreeMap<(u64, LobbyKindKey), Instant>>,
}

struct RegistrationDraftNeonObserver {
    handler: Arc<PlayerRegistrationHandler>,
}

fn draft_neon_result(result: Option<cama_app::neon_degen::NeonResult>) -> Option<DraftNeonResult> {
    result.map(|result| DraftNeonResult {
        text_block: result.text_block.unwrap_or_default(),
        attachment: result.gif_file.map(|gif| {
            crate::registration::InteractionAttachment::bytes(
                format!("{}.gif", gif.kind),
                gif.bytes,
            )
        }),
    })
}

fn draft_neon_id(value: i64, label: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{label} ID {value} is negative"))
}

#[async_trait]
impl DraftNeonObserver for RegistrationDraftNeonObserver {
    async fn on_draft_coinflip(
        &self,
        guild_id: i64,
        winner_id: i64,
        loser_id: i64,
    ) -> Result<Option<DraftNeonResult>, String> {
        let guild_id = draft_neon_id(guild_id, "guild")?;
        let winner_id = draft_neon_id(winner_id, "winner")?;
        let loser_id = draft_neon_id(loser_id, "loser")?;
        let result = self
            .handler
            .neon
            .lock()
            .map_err(|_| "registration Neon service lock was poisoned".to_owned())?
            .on_draft_coinflip(Some(guild_id), winner_id, loser_id);
        Ok(draft_neon_result(result))
    }

    async fn on_captain_symmetry(
        &self,
        guild_id: i64,
        radiant_captain_id: i64,
        dire_captain_id: i64,
        rating_diff: i64,
    ) -> Result<Option<DraftNeonResult>, String> {
        let guild_id = draft_neon_id(guild_id, "guild")?;
        let radiant_captain_id = draft_neon_id(radiant_captain_id, "radiant captain")?;
        let dire_captain_id = draft_neon_id(dire_captain_id, "dire captain")?;
        let result = self
            .handler
            .neon
            .lock()
            .map_err(|_| "registration Neon service lock was poisoned".to_owned())?
            .on_captain_symmetry(
                Some(guild_id),
                radiant_captain_id,
                dire_captain_id,
                rating_diff,
            );
        Ok(draft_neon_result(result))
    }

    async fn on_bomb_pot(
        &self,
        guild_id: i64,
        pool_amount: i64,
        contributor_count: i64,
    ) -> Result<Option<DraftNeonResult>, String> {
        let guild_id = draft_neon_id(guild_id, "guild")?;
        let result = self
            .handler
            .neon
            .lock()
            .map_err(|_| "registration Neon service lock was poisoned".to_owned())?
            .on_bomb_pot(Some(guild_id), pool_amount, contributor_count);
        Ok(draft_neon_result(result))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LobbyKindKey {
    Open,
    LowSkill,
}

#[async_trait]
impl InteractionHandler for PlayerRegistrationHandler {
    fn acknowledgement_policy(
        &self,
        request: &InteractionRequest,
    ) -> InteractionAcknowledgementPolicy {
        match request {
            InteractionRequest::Component { custom_id, .. }
                if custom_id.starts_with(MMR_COMPONENT_PREFIX)
                    && custom_id.contains(":button:") =>
            {
                InteractionAcknowledgementPolicy::Modal
            }
            _ => InteractionAcknowledgementPolicy::Automatic,
        }
    }

    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command { .. } => self.handle_command(request, responder).await,
            InteractionRequest::Component { .. } => {
                self.handle_mmr_button(request, responder).await
            }
            InteractionRequest::Modal { .. } => self.handle_mmr_modal(request, responder).await,
            InteractionRequest::Autocomplete { .. } => {
                Err("player handler received an autocomplete interaction".to_owned())
            }
        }
        .map_err(Into::into)
    }
}

#[async_trait]
impl LobbyJoinObserver for PlayerRegistrationHandler {
    async fn confirmed_lobby_join(&self, event: ConfirmedLobbyJoin) -> Result<(), String> {
        if let Err(error) = self.deliver_one_shot_lobby_alerts(&event).await {
            warn!(%error, "one-shot lobby alert delivery failed independently");
        }
        if event.player_ids.len() == event.ready_threshold {
            self.deliver_lobby_ready(&event).await
        } else {
            self.deliver_lobby_rally(&event).await
        }
    }

    async fn explicit_lobby_join_neon(&self, event: ConfirmedLobbyJoin) -> Result<(), String> {
        let result = self
            .neon
            .lock()
            .map_err(|_| "lobby Neon service lock was poisoned".to_owned())?
            .on_lobby_join(
                event.player_id,
                Some(event.guild_id),
                &event.player_display_name,
                event.player_ids.len(),
            );
        let (Some(channel_id), Some(text)) = (
            event.lobby_channel_id,
            result.and_then(|result| result.text_block),
        ) else {
            return Ok(());
        };
        self.discord
            .send_message(
                channel_id,
                DiscordMessage::silent(InteractionResponse::message(text).without_mentions()),
            )
            .await
            .map(|_| ())
    }

    async fn gamba_spectator(&self, event: LobbyGambaSpectator) -> Result<(), String> {
        let result = self
            .neon
            .lock()
            .map_err(|_| "gamba Neon service lock was poisoned".to_owned())?
            .on_gamba_spectator(
                event.player_id,
                Some(event.guild_id),
                &event.player_display_name,
            );
        let Some(text) = result.and_then(|result| result.text_block) else {
            return Ok(());
        };
        let receipt = self
            .discord
            .send_message(
                event.channel_id,
                DiscordMessage::silent(InteractionResponse::message(text).without_mentions()),
            )
            .await?;
        let discord = self.discord.clone();
        tokio::spawn(async move {
            tokio::time::sleep(NEON_DELETE_DELAY).await;
            if let Err(error) = discord
                .delete_message(receipt.channel_id, receipt.message_id)
                .await
            {
                debug!(%error, "failed to delete gamba spectator Neon message");
            }
        });
        Ok(())
    }

    async fn lobby_reset(
        &self,
        guild_id: u64,
        lobby_kind: cama_app::embeds::LobbyKind,
    ) -> Result<(), String> {
        let kind = match lobby_kind {
            cama_app::embeds::LobbyKind::Open => LobbyKindKey::Open,
            cama_app::embeds::LobbyKind::LowSkill => LobbyKindKey::LowSkill,
        };
        self.ready_cooldowns
            .lock()
            .map_err(|_| "lobby ready cooldown lock was poisoned".to_owned())?
            .remove(&(guild_id, kind));
        self.rally_cooldowns
            .lock()
            .map_err(|_| "lobby rally cooldown lock was poisoned".to_owned())?
            .retain(|(candidate_guild, candidate_kind, _, _), _| {
                *candidate_guild != guild_id || *candidate_kind != kind
            });
        Ok(())
    }
}

impl PlayerRegistrationHandler {
    /// Best-effort push-notification fan-out for a lobby that just reached
    /// its ready threshold.
    fn render_player_name(&self, guild_id: u64, user_id: u64) -> String {
        let names = self
            .discord
            .cached_guild_member_render_names(guild_id, &[user_id])
            .map(GuildPlayerNameDirectory::new)
            .unwrap_or_default();
        i64::try_from(user_id)
            .map_or_else(|_| user_id.to_string(), |user_id| names.resolve(user_id))
    }

    async fn deliver_lobby_ready(&self, event: &ConfirmedLobbyJoin) -> Result<(), String> {
        let kind = match event.lobby_kind {
            cama_app::embeds::LobbyKind::Open => LobbyKindKey::Open,
            cama_app::embeds::LobbyKind::LowSkill => LobbyKindKey::LowSkill,
        };
        let key = (event.guild_id, kind);
        {
            let now = Instant::now();
            let mut cooldowns = self
                .ready_cooldowns
                .lock()
                .map_err(|_| "lobby ready cooldown lock was poisoned".to_owned())?;
            if cooldowns
                .get(&key)
                .is_some_and(|last| now.duration_since(*last) < self.config.lobby_ready_cooldown)
            {
                return Ok(());
            }
            cooldowns.insert(key, now);
        }
        let result = self.send_lobby_ready(event).await;
        if result.is_err()
            && let Ok(mut cooldowns) = self.ready_cooldowns.lock()
        {
            cooldowns.remove(&key);
        }
        result
    }

    async fn send_lobby_ready(&self, event: &ConfirmedLobbyJoin) -> Result<(), String> {
        let target_channel = event
            .origin_channel_id
            .or(event.lobby_channel_id)
            .ok_or_else(|| "lobby ready announcement has no destination channel".to_owned())?;
        let mut embed = crate::registration::InteractionEmbed::titled(format!(
            "🎮 {} Ready!",
            event.lobby_kind.label()
        ))
        .description(format!(
            "{} now has 10 players!",
            event.lobby_kind.display_name()
        ))
        .color(0x2e_cc_71)
        .field("Next Step", "Run `/readycheck` before `/shuffle`.", false);
        if let (Some(channel), Some(message)) = (event.lobby_channel_id, event.lobby_message_id) {
            embed = embed.field(
                "",
                format!(
                    "[Jump to Lobby](https://discord.com/channels/{}/{channel}/{message})",
                    event.guild_id
                ),
                false,
            );
        }
        self.discord
            .send_message(
                target_channel,
                DiscordMessage::silent(
                    InteractionResponse::message("")
                        .embed(embed)
                        .without_mentions(),
                ),
            )
            .await
            .map(|_| ())
    }

    async fn deliver_one_shot_lobby_alerts(
        &self,
        event: &ConfirmedLobbyJoin,
    ) -> Result<(), String> {
        let notifications = self.notifications.clone();
        let target_id = signed_id(event.player_id, "lobby target")?;
        let guild_id = signed_id(event.guild_id, "guild")?;
        let cutoff = event.joined_at_ns;
        let subscribers = tokio::task::spawn_blocking(move || {
            notifications.claim_lobby_target_subscribers(target_id, Some(guild_id), cutoff)
        })
        .await
        .map_err(|error| format!("one-shot lobby claim task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let subscribers = subscribers
            .into_iter()
            .filter_map(|subscriber| u64::try_from(subscriber).ok())
            .collect::<BTreeSet<_>>();
        if subscribers.is_empty() {
            return Ok(());
        }
        let target_name = escape_discord_text(&event.player_display_name);
        let message = format!(
            "🔔 **{target_name}** just joined {}! This one-time lobby notification has now been used.",
            event.lobby_kind.label()
        );
        for subscriber in subscribers {
            if let Err(error) = self
                .discord
                .send_direct_message(
                    subscriber,
                    DiscordMessage::silent(
                        InteractionResponse::message(&message).without_mentions(),
                    ),
                )
                .await
            {
                warn!(subscriber, %error, "one-shot lobby DM delivery failed");
            }
        }
        Ok(())
    }

    async fn deliver_lobby_rally(&self, event: &ConfirmedLobbyJoin) -> Result<(), String> {
        let total = event.player_ids.len();
        let Some(needed) = event.ready_threshold.checked_sub(total) else {
            return Ok(());
        };
        if !(1..=2).contains(&needed) {
            return Ok(());
        }
        let kind = match event.lobby_kind {
            cama_app::embeds::LobbyKind::Open => LobbyKindKey::Open,
            cama_app::embeds::LobbyKind::LowSkill => LobbyKindKey::LowSkill,
        };
        let key = (
            event.guild_id,
            kind,
            needed,
            event.lobby_message_id.unwrap_or_default(),
        );
        {
            let now = Instant::now();
            let mut cooldowns = self
                .rally_cooldowns
                .lock()
                .map_err(|_| "lobby rally cooldown lock was poisoned".to_owned())?;
            if cooldowns
                .get(&key)
                .is_some_and(|last| now.duration_since(*last) < self.config.lobby_rally_cooldown)
            {
                return Ok(());
            }
            cooldowns.insert(key, now);
        }

        let result = self.send_lobby_rally(event, total, needed).await;
        if result.is_err()
            && let Ok(mut cooldowns) = self.rally_cooldowns.lock()
        {
            cooldowns.remove(&key);
        }
        result
    }

    async fn send_lobby_rally(
        &self,
        event: &ConfirmedLobbyJoin,
        total: usize,
        needed: usize,
    ) -> Result<(), String> {
        let target_channel = event
            .origin_channel_id
            .or(event.lobby_channel_id)
            .ok_or_else(|| "lobby rally has no destination channel".to_owned())?;
        let reaction_subscribers = if let (Some(channel), Some(message)) =
            (event.lobby_channel_id, event.lobby_message_id)
        {
            self.discord
                .reaction_users(
                    channel,
                    message,
                    &crate::discord_transport::DiscordEmoji::unicode("📋"),
                )
                .await
                .unwrap_or_else(|error| {
                    warn!(%error, "could not load clipboard lobby subscribers");
                    Vec::new()
                })
        } else {
            Vec::new()
        };
        let notifications = self.notifications.clone();
        let guild_id = signed_id(event.guild_id, "guild")?;
        let mut persistent = tokio::task::spawn_blocking(move || {
            notifications.enabled_users(Some(guild_id), NotificationKind::Lobby)
        })
        .await
        .map_err(|error| format!("persistent lobby subscriber task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        if event.lobby_kind == cama_app::embeds::LobbyKind::LowSkill && !persistent.is_empty() {
            let players = self.players.clone();
            persistent = tokio::task::spawn_blocking(move || {
                let players = players.get_by_ids(&persistent, Some(guild_id))?;
                Ok::<_, cama_db::core_repositories::CoreRepositoryError>(
                    players
                        .into_iter()
                        .filter(|player| {
                            player.glicko_rating.is_some_and(|rating| rating < 1_400.0)
                        })
                        .filter_map(|player| player.discord_id)
                        .collect::<Vec<_>>(),
                )
            })
            .await
            .map_err(|error| format!("low-skill subscriber filter task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        }

        let mut excluded = event.player_ids.clone();
        if let Ok(Some(bot_user_id)) = self.discord.bot_user_id().await {
            excluded.insert(bot_user_id);
        }
        let mut subscriber_ids = Vec::new();
        let mut seen = excluded;
        for user in reaction_subscribers {
            if !user.is_bot && seen.insert(user.user_id) {
                subscriber_ids.push(user.user_id);
            }
        }
        persistent.sort_unstable();
        persistent.dedup();
        for subscriber in persistent
            .into_iter()
            .filter_map(|subscriber| u64::try_from(subscriber).ok())
        {
            if subscriber > 0 && seen.insert(subscriber) {
                subscriber_ids.push(subscriber);
            }
        }
        let mut selected = Vec::new();
        let mut content_length = 0_usize;
        for subscriber in subscriber_ids {
            let mention = format!("<@{subscriber}>");
            let added = mention.len() + usize::from(!selected.is_empty());
            if selected.len() >= 100 || content_length.saturating_add(added) > 2_000 {
                break;
            }
            content_length += added;
            selected.push(subscriber);
        }
        let content = selected
            .iter()
            .map(|subscriber| format!("<@{subscriber}>"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut embed = crate::registration::InteractionEmbed::titled(format!(
            "📢 {} Almost Ready!",
            event.lobby_kind.label()
        ))
        .description(format!(
            "{} has **{total}** players — just **+{needed}** more needed!",
            event.lobby_kind.display_name()
        ))
        .color(0xe6_7e_22);
        if let (Some(channel), Some(message)) = (event.lobby_channel_id, event.lobby_message_id) {
            embed = embed.field(
                "",
                format!(
                    "[Jump to Lobby](https://discord.com/channels/{}/{channel}/{message})",
                    event.guild_id
                ),
                false,
            );
        }
        self.discord
            .send_message(
                target_channel,
                DiscordMessage::mentioning(
                    InteractionResponse::message(content)
                        .embed(embed)
                        .with_user_mentions(selected.clone()),
                    selected.iter().copied().collect(),
                ),
            )
            .await?;
        if let Some(thread_id) = event.thread_id
            && let Err(error) = self
                .discord
                .send_message(
                    thread_id,
                    DiscordMessage::silent(
                        InteractionResponse::message(format!(
                            "📢 {}: **+{needed}** more player{} needed!",
                            event.lobby_kind.label(),
                            if needed > 1 { "s" } else { "" }
                        ))
                        .without_mentions(),
                    ),
                )
                .await
        {
            warn!(%error, "could not send lobby rally to thread");
        }
        Ok(())
    }
}

#[derive(Debug)]
struct CommandContext {
    interaction_id: u64,
    name: String,
    user_id: u64,
    user_display_name: String,
    guild_id: Option<u64>,
    channel_id: Option<u64>,
    options: Vec<InteractionOption>,
}

impl CommandContext {
    fn from_request(request: InteractionRequest) -> Result<Self, String> {
        let InteractionRequest::Command {
            interaction_id,
            name,
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            options,
            ..
        } = request
        else {
            return Err("registration command handler received a callback".to_owned());
        };
        Ok(Self {
            interaction_id,
            name,
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            options,
        })
    }

    fn guild_id(&self) -> Result<u64, String> {
        self.guild_id
            .ok_or_else(|| "This command can only be used in a server.".to_owned())
    }
}

impl PlayerRegistrationHandler {
    async fn handle_command(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let context = CommandContext::from_request(request)?;
        let guild_id = match context.guild_id() {
            Ok(guild_id) => guild_id,
            Err(message) => {
                return respond_ephemeral(&responder, format!("❌ {message}")).await;
            }
        };
        if context.name == "refer" && self.refer_retry_after(context.user_id)?.is_some() {
            return respond_ephemeral(
                &responder,
                "❌ An error occurred while processing your command. Please try again.",
            )
            .await;
        }
        if !defer_ephemeral(&responder).await {
            return Ok(());
        }
        match context.name.as_str() {
            "refer" => self.refer(context, guild_id, responder).await,
            "player" => {
                let command_options = context.options.clone();
                let (path, options) = leaf_command(&command_options)?;
                match path.as_str() {
                    "register" => self.register(context, guild_id, options, responder).await,
                    "link" => self.link(context, guild_id, options, responder).await,
                    "unlink" => self.unlink(context, guild_id, options, responder).await,
                    "steamids" => self.steamids(context, guild_id, responder).await,
                    "roles" => self.roles(context, guild_id, options, responder).await,
                    "region" => self.region(context, guild_id, options, responder).await,
                    "exclusion" => self.exclusion(context, guild_id, responder).await,
                    "lobby autonotify" => {
                        self.autonotify(context, guild_id, options, responder).await
                    }
                    "lobby status" => self.lobby_status(context, guild_id, responder).await,
                    "curfew add" => self.curfew_add(context, guild_id, options, responder).await,
                    "curfew remove" => {
                        self.curfew_remove(context, guild_id, options, responder)
                            .await
                    }
                    "curfew list" => self.curfew_list(context, guild_id, responder).await,
                    "timezone set" => {
                        self.timezone_set(context, guild_id, options, responder)
                            .await
                    }
                    "timezone status" => self.timezone_status(context, guild_id, responder).await,
                    "timezone list" => self.timezone_list(responder).await,
                    "playtime set" => {
                        self.playtime_set(context, guild_id, options, responder)
                            .await
                    }
                    "playtime clear" => self.playtime_clear(context, guild_id, responder).await,
                    "playtime status" => self.playtime_status(context, guild_id, responder).await,
                    "playtime popular" => self.playtime_popular(guild_id, responder).await,
                    _ => Err(format!("unknown player command {path:?}")),
                }
            }
            name => Err(format!("registration handler received command {name:?}")),
        }
    }

    async fn refer(
        &self,
        context: CommandContext,
        guild_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let target = user_option(&context.options, "player")?
            .ok_or_else(|| "refer requires player".to_owned())?;
        if target.is_bot == Some(true) {
            return followup_ephemeral(&responder, "❌ You can only refer human players.").await;
        }
        if target.raw_id == context.user_id {
            return followup_ephemeral(&responder, "❌ You can't refer yourself.").await;
        }
        let repository = self.referrals.clone();
        let referrer = signed_id(context.user_id, "referrer")?;
        let target_id = target.raw_id;
        let target = target.id;
        let guild = signed_id(guild_id, "guild")?;
        let result = tokio::task::spawn_blocking(move || {
            repository.create_referral(referrer, target, Some(guild))
        })
        .await
        .map_err(|error| format!("referral task failed: {error}"))?;
        if let Err(error) = result {
            return followup_ephemeral(&responder, format!("❌ {error}")).await;
        }
        followup_ephemeral(&responder, "✅ Referral created.").await?;
        let mut lobby_channels = Vec::new();
        for channel_id in [
            self.config.lobby_channel_id,
            self.config.low_skill_lobby_channel_id,
        ]
        .into_iter()
        .flatten()
        {
            let Ok(channel_id_u64) = u64::try_from(channel_id) else {
                continue;
            };
            match self
                .discord
                .guild_channel_exists(guild_id, channel_id_u64)
                .await
            {
                Ok(true) => {
                    if !lobby_channels.contains(&channel_id) {
                        lobby_channels.push(channel_id);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    debug!(%error, guild_id, channel_id, "could not resolve configured referral lobby channel")
                }
            }
        }
        let lobby_destination = if lobby_channels.is_empty() {
            "a lobby channel".to_owned()
        } else {
            lobby_channels
                .into_iter()
                .map(|channel_id| format!("<#{channel_id}>"))
                .collect::<Vec<_>>()
                .join(" or ")
        };
        responder
            .followup(
                InteractionResponse::message(format!(
                    "**Referral: <@{target_id}>**\n\
                     1. If needed, run `/player register` with your Steam32 ID (the number in your Dotabuff URL).\n\
                     2. Run `/player roles` with your Dota positions (1-5).\n\
                     3. React in {lobby_destination} to join an active lobby, or run `/lobby` to start one."
                ))
                .with_user_mentions(vec![target_id]),
            )
            .await
            .map_err(|error| error.to_string())
    }

    fn refer_retry_after(&self, user_id: u64) -> Result<Option<u64>, String> {
        let now = Instant::now();
        let mut cooldowns = self
            .refer_cooldowns
            .lock()
            .map_err(|_| "referral cooldown lock was poisoned".to_owned())?;
        if let Some(previous) = cooldowns.get(&user_id) {
            let elapsed = now.duration_since(*previous);
            if elapsed < REFER_COOLDOWN {
                let remaining = REFER_COOLDOWN - elapsed;
                return Ok(Some(
                    remaining
                        .as_secs()
                        .saturating_add(u64::from(remaining.subsec_nanos() != 0)),
                ));
            }
        }
        cooldowns.insert(user_id, now);
        Ok(None)
    }

    async fn register(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let steam_id = integer_option(options, "steam_id")
            .ok_or_else(|| "register requires steam_id".to_owned())?;
        match self
            .run_registration(&context, guild_id, steam_id, None)
            .await
        {
            Ok(result) => {
                self.best_effort_infer_region(&context, guild_id, steam_id)
                    .await;
                self.registration_success(&context, guild_id, result, responder)
                    .await
            }
            Err(RegisterPlayerError::MmrUnavailable) => {
                let nonce = context.interaction_id;
                let state = MmrPromptState {
                    nonce,
                    discord_id: signed_id(context.user_id, "user")?,
                    guild_id: signed_id(guild_id, "guild")?,
                    steam_id,
                    attempts: 0,
                    expires_at: now_seconds().saturating_add(
                        i64::try_from(self.config.mmr_modal_timeout.as_secs()).unwrap_or(i64::MAX),
                    ),
                    completed: false,
                };
                let repository = self.registration.clone();
                tokio::task::spawn_blocking(move || repository.save_mmr_prompt(&state))
                    .await
                    .map_err(|error| format!("MMR prompt persistence task failed: {error}"))?
                    .map_err(|error| error.to_string())?;
                responder
                    .followup(
                        InteractionResponse::message(
                            "⚠️ OpenDota could not find your MMR. Click **Enter MMR** to finish registering.",
                        )
                        .ephemeral()
                        .without_mentions()
                        .action_row(InteractionActionRow::buttons(vec![InteractionButton::new(
                            format!("{MMR_COMPONENT_PREFIX}button:{guild_id}:{nonce}"),
                            "Enter MMR",
                        )])),
                    )
                    .await
                    .map_err(|error| error.to_string())
            }
            Err(error) if is_user_registration_error(&error) => {
                followup_ephemeral(&responder, format!("❌ {error}")).await
            }
            Err(error) => {
                error!(user_id = context.user_id, %error, "registration failed");
                followup_ephemeral(
                    &responder,
                    "❌ Unexpected error registering you. Try again later.",
                )
                .await
            }
        }
    }

    async fn run_registration(
        &self,
        context: &CommandContext,
        guild_id: u64,
        steam_id: i64,
        mmr_override: Option<i64>,
    ) -> Result<RegisterPlayerResult, RegisterPlayerError> {
        let mut repository = self.registration.clone();
        let mut api = self.opendota.registration_api();
        let input = RegisterPlayerInput {
            discord_id: signed_id(context.user_id, "user")
                .map_err(RegisterPlayerError::Repository)?,
            discord_username: context.user_display_name.clone(),
            guild_id: signed_id(guild_id, "guild").map_err(RegisterPlayerError::Repository)?,
            steam_id,
            mmr_override,
            exclusion_count: self.config.new_player_exclusion_boost,
            added_at: now_seconds(),
        };
        let rating_system = self.config.rating_system;
        let openskill = self.config.openskill.clone();
        tokio::task::spawn_blocking(move || {
            register_player(&mut repository, &mut api, input, &rating_system, &openskill)
        })
        .await
        .map_err(|error| {
            RegisterPlayerError::Repository(format!("registration task failed: {error}"))
        })?
    }

    async fn best_effort_infer_region(
        &self,
        context: &CommandContext,
        guild_id: u64,
        steam_id: i64,
    ) {
        let Some(raw_counts) = self
            .opendota
            .shared_client()
            .get_player_counts(steam_id)
            .await
        else {
            return;
        };
        let counts = project_counts_payload(&raw_counts);
        let Some(region) = infer_region_from_counts(Some(&counts)) else {
            return;
        };
        let (Ok(discord_id), Ok(guild_id)) = (
            signed_id(context.user_id, "user"),
            signed_id(guild_id, "guild"),
        ) else {
            return;
        };
        let repository = self.registration.clone();
        let region = region.to_owned();
        match tokio::task::spawn_blocking(move || {
            repository.set_inferred_region(discord_id, Some(guild_id), &region)
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => warn!(%error, "registration region inference write failed"),
            Err(error) => warn!(%error, "registration region inference task failed"),
        }
    }

    async fn registration_success(
        &self,
        context: &CommandContext,
        guild_id: u64,
        result: RegisterPlayerResult,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .followup(
                InteractionResponse::message(format!(
                    "✅ Registered <@{}>!\n\
                     Cama Rating: {} ({:.0}% uncertainty)\n\
                     Use `/player roles` to set your preferred roles.\n\
                     Use `/player region` to set your server (US East / US West).",
                    context.user_id, result.cama_rating, result.uncertainty
                ))
                .with_user_mentions(vec![context.user_id]),
            )
            .await
            .map_err(|error| error.to_string())?;
        self.best_effort_neon(context, guild_id).await;
        Ok(())
    }

    async fn best_effort_neon(&self, context: &CommandContext, guild_id: u64) {
        let rendered_name = self.render_player_name(guild_id, context.user_id);
        let result = match self.neon.lock() {
            Ok(mut neon) => neon.on_registration(context.user_id, Some(guild_id), &rendered_name),
            Err(_) => {
                warn!("registration Neon service lock was poisoned");
                None
            }
        };
        let (Some(channel_id), Some(text)) = (
            context.channel_id,
            result.and_then(|result| result.text_block),
        ) else {
            return;
        };
        match self
            .discord
            .send_message(
                channel_id,
                DiscordMessage::silent(InteractionResponse::message(text).without_mentions()),
            )
            .await
        {
            Ok(receipt) => {
                let discord = self.discord.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(NEON_DELETE_DELAY).await;
                    if let Err(error) = discord
                        .delete_message(receipt.channel_id, receipt.message_id)
                        .await
                    {
                        debug!(%error, "failed to delete registration Neon message");
                    }
                });
            }
            Err(error) => debug!(%error, "failed to send registration Neon message"),
        }
    }

    async fn handle_mmr_button(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Component {
            custom_id,
            guild_id,
            ..
        } = request
        else {
            return Err("MMR button handler received another interaction".to_owned());
        };
        let (kind, encoded_guild, nonce) = parse_mmr_custom_id(&custom_id)?;
        if kind != "button" || guild_id != Some(encoded_guild) {
            return respond_ephemeral(&responder, "❌ This MMR prompt is no longer valid.").await;
        }
        let mut input = InteractionTextInput::short("mmr", "Enter your MMR");
        input.required = false;
        responder
            .show_modal(InteractionModal {
                custom_id: format!("{MMR_COMPONENT_PREFIX}modal:{encoded_guild}:{nonce}"),
                title: "Enter MMR".to_owned(),
                inputs: vec![input],
            })
            .await
            .map_err(|error| error.to_string())
    }

    async fn handle_mmr_modal(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Modal {
            custom_id,
            user_id,
            guild_id,
            channel_id,
            fields,
            ..
        } = request
        else {
            return Err("MMR modal handler received another interaction".to_owned());
        };
        let (kind, encoded_guild, nonce) = parse_mmr_custom_id(&custom_id)?;
        if kind != "modal" || guild_id != Some(encoded_guild) {
            return respond_ephemeral(&responder, "❌ This MMR prompt is no longer valid.").await;
        }
        let Some(state) = self.load_prompt(encoded_guild, nonce).await? else {
            return respond_ephemeral(&responder, "❌ This MMR prompt is no longer valid.").await;
        };
        if !prompt_is_usable(&state, user_id, self.config.mmr_modal_retry_limit) {
            return respond_ephemeral(&responder, "❌ Invalid MMR").await;
        }
        let mmr = fields
            .get("mmr")
            .map(|raw| raw.trim())
            .filter(|raw| !raw.is_empty())
            .and_then(|raw| raw.parse::<i64>().ok())
            .filter(|mmr| (1..=12_000).contains(mmr));
        let Some(mmr) = mmr else {
            let state = self.record_prompt_failure(encoded_guild, nonce).await?;
            respond_ephemeral(&responder, "❌ Invalid MMR").await?;
            if state.is_some_and(|state| state.attempts >= self.config.mmr_modal_retry_limit) {
                followup_ephemeral(&responder, "❌ Invalid MMR").await?;
            }
            return Ok(());
        };
        respond_ephemeral(&responder, "✅ MMR received").await?;
        let account_username = self
            .discord
            .user(user_id)
            .await
            .ok()
            .flatten()
            .map_or_else(|| user_id.to_string(), |user| user.account_username);
        let context = CommandContext {
            interaction_id: nonce,
            name: "player".to_owned(),
            user_id,
            user_display_name: account_username,
            guild_id: Some(encoded_guild),
            channel_id,
            options: Vec::new(),
        };
        match self
            .run_registration(&context, encoded_guild, state.steam_id, Some(mmr))
            .await
        {
            Ok(result) => {
                let signed_guild = signed_id(encoded_guild, "guild")?;
                let repository = self.registration.clone();
                match tokio::task::spawn_blocking(move || {
                    repository.complete_mmr_prompt(signed_guild, nonce)
                })
                .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => warn!(%error, "MMR prompt completion write failed"),
                    Err(error) => warn!(%error, "MMR prompt completion task failed"),
                }
                self.registration_success(&context, encoded_guild, result, responder)
                    .await
            }
            Err(error) => {
                error!(%error, user_id, "failed to finalize modal registration");
                followup_ephemeral(
                    &responder,
                    "❌ Error finalizing registration. Try again later.",
                )
                .await
            }
        }
    }

    async fn load_prompt(
        &self,
        guild_id: u64,
        nonce: u64,
    ) -> Result<Option<MmrPromptState>, String> {
        let guild_id = signed_id(guild_id, "guild")?;
        let repository = self.registration.clone();
        tokio::task::spawn_blocking(move || repository.mmr_prompt(guild_id, nonce))
            .await
            .map_err(|error| format!("MMR prompt lookup task failed: {error}"))?
            .map_err(|error| error.to_string())
    }

    async fn record_prompt_failure(
        &self,
        guild_id: u64,
        nonce: u64,
    ) -> Result<Option<MmrPromptState>, String> {
        let guild_id = signed_id(guild_id, "guild")?;
        let repository = self.registration.clone();
        tokio::task::spawn_blocking(move || repository.record_mmr_prompt_failure(guild_id, nonce))
            .await
            .map_err(|error| format!("MMR prompt update task failed: {error}"))?
            .map_err(|error| error.to_string())
    }

    async fn link(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let steam_id = integer_option(options, "steam_id")
            .ok_or_else(|| "link requires steam_id".to_owned())?;
        let set_primary = boolean_option(options, "set_primary").unwrap_or(false);
        if !valid_steam_id(steam_id) {
            return followup_ephemeral(
                &responder,
                "❌ Invalid Steam ID. Please use the 32-bit Steam ID from your Dotabuff URL.",
            )
            .await;
        }
        let registration = self.registration.clone();
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let exists =
            tokio::task::spawn_blocking(move || registration.player_exists(user_id, Some(guild)))
                .await
                .map_err(|error| format!("player lookup task failed: {error}"))?
                .map_err(|error| error.to_string())?;
        if !exists {
            return followup_ephemeral(
                &responder,
                "❌ You are not registered. Use `/player register` first.",
            )
            .await;
        }
        let steam = self.steam.clone();
        let ids = tokio::task::spawn_blocking(move || steam.get_steam_ids(user_id))
            .await
            .map_err(|error| format!("Steam account lookup task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if ids.contains(&steam_id) {
            if set_primary {
                let steam = self.steam.clone();
                let updated = tokio::task::spawn_blocking(move || {
                    steam.set_primary_steam_id(user_id, steam_id)
                })
                .await
                .map_err(|error| format!("primary Steam update task failed: {error}"))?
                .map_err(|error| error.to_string())?;
                if updated {
                    return followup_ephemeral(
                        &responder,
                        format!("✅ Steam ID `{steam_id}` is now your primary account."),
                    )
                    .await;
                }
            }
            return followup_ephemeral(
                &responder,
                format!("ℹ️ Steam ID `{steam_id}` is already linked to your account."),
            )
            .await;
        }
        let is_primary = set_primary || ids.is_empty();
        let steam = self.steam.clone();
        let added = tokio::task::spawn_blocking(move || {
            steam.add_steam_id(user_id, steam_id, is_primary, now_seconds())
        })
        .await
        .map_err(|error| format!("Steam account link task failed: {error}"))?;
        if let Err(error) = added {
            return match error {
                OpenDotaPlayerRepositoryError::SteamIdAlreadyLinked { steam_id, .. } => {
                    followup_ephemeral(
                        &responder,
                        format!("❌ Steam ID {steam_id} is already linked to another player"),
                    )
                    .await
                }
                error => {
                    error!(%error, "Steam account link persistence failed");
                    followup_ephemeral(
                        &responder,
                        "❌ An error occurred while processing your command. Please try again.",
                    )
                    .await
                }
            };
        }
        let steam = self.steam.clone();
        let new_ids = tokio::task::spawn_blocking(move || steam.get_steam_ids(user_id))
            .await
            .map_err(|error| format!("Steam account lookup task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if new_ids.len() == 1 {
            followup_ephemeral(
                &responder,
                format!(
                    "✅ Steam ID `{steam_id}` linked to your account!\n\
                     You can now use `/rolesgraph`, `/lanegraph`, and the Dota tab in `/profile`."
                ),
            )
            .await
        } else {
            let primary_note = if set_primary { " (set as primary)" } else { "" };
            followup_ephemeral(
                &responder,
                format!(
                    "✅ Steam ID `{steam_id}` added to your account{primary_note}!\n\
                     You now have {} linked accounts. Use `/player steamids` to view all linked accounts.",
                    new_ids.len()
                ),
            )
            .await
        }
    }

    async fn unlink(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let steam_id = integer_option(options, "steam_id")
            .ok_or_else(|| "unlink requires steam_id".to_owned())?;
        let user_id = signed_id(context.user_id, "user")?;
        if !self
            .player_exists(user_id, signed_id(guild_id, "guild")?)
            .await?
        {
            return followup_ephemeral(
                &responder,
                "❌ You are not registered. Use `/player register` first.",
            )
            .await;
        }
        let ids = self.steam_ids(user_id).await?;
        if !ids.contains(&steam_id) {
            return followup_ephemeral(
                &responder,
                format!("❌ Steam ID `{steam_id}` is not linked to your account."),
            )
            .await;
        }
        if ids.len() == 1 {
            followup_ephemeral(
                &responder,
                format!(
                    "⚠️ Steam ID `{steam_id}` is your only linked account.\n\
                     Unlinking it will disable match discovery and Dota stats.\n\
                     Are you sure? Run the command again to confirm."
                ),
            )
            .await?;
        }
        let steam = self.steam.clone();
        let removed = tokio::task::spawn_blocking(move || steam.remove_steam_id(user_id, steam_id))
            .await
            .map_err(|error| format!("Steam unlink task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if !removed {
            return followup_ephemeral(
                &responder,
                format!("❌ Failed to unlink Steam ID `{steam_id}`."),
            )
            .await;
        }
        let remaining = self.steam_ids(user_id).await?;
        match remaining.first() {
            Some(primary) => {
                followup_ephemeral(
                    &responder,
                    format!(
                        "✅ Steam ID `{steam_id}` has been unlinked.\nYour primary account is now `{primary}`."
                    ),
                )
                .await
            }
            None => {
                followup_ephemeral(
                    &responder,
                    format!(
                        "✅ Steam ID `{steam_id}` has been unlinked.\nYou no longer have any linked Steam accounts."
                    ),
                )
                .await
            }
        }
    }

    async fn steamids(
        &self,
        context: CommandContext,
        guild_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let user_id = signed_id(context.user_id, "user")?;
        if !self
            .player_exists(user_id, signed_id(guild_id, "guild")?)
            .await?
        {
            return followup_ephemeral(
                &responder,
                "❌ You are not registered. Use `/player register` first.",
            )
            .await;
        }
        let ids = self.steam_ids(user_id).await?;
        if ids.is_empty() {
            return followup_ephemeral(
                &responder,
                "ℹ️ You don't have any Steam accounts linked.\nUse `/player link` to link your Steam account.",
            )
            .await;
        }
        let mut lines = vec!["**Your Linked Steam Accounts:**\n".to_owned()];
        for (index, steam_id) in ids.into_iter().enumerate() {
            let url = format!("https://www.dotabuff.com/players/{steam_id}");
            if index == 0 {
                lines.push(format!("⭐ `{steam_id}` (Primary) - [Dotabuff]({url})"));
            } else {
                lines.push(format!("• `{steam_id}` - [Dotabuff]({url})"));
            }
        }
        lines.push(
            "\n*Use `/player link` to add more accounts or `/player unlink` to remove one.*"
                .to_owned(),
        );
        followup_ephemeral(&responder, lines.join("\n")).await
    }

    async fn roles(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let raw = string_option(options, "roles")
            .ok_or_else(|| "roles requires a roles value".to_owned())?;
        let roles = match parse_roles(raw) {
            Ok(roles) => roles,
            Err(RoleInputError::Invalid(role)) => {
                let valid = (1..=5)
                    .map(|role| format_role_display(&role.to_string()))
                    .collect::<Vec<_>>()
                    .join(", ");
                return followup_ephemeral(
                    &responder,
                    format!("❌ Invalid role: {role}. Roles must be 1-5:\n{valid}"),
                )
                .await;
            }
            Err(RoleInputError::Empty) => {
                return followup_ephemeral(&responder, "❌ Please provide at least one role.")
                    .await;
            }
        };
        let repository = self.registration.clone();
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let saved_roles = roles.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            repository.set_roles_atomic(user_id, Some(guild), &saved_roles)
        })
        .await
        .map_err(|error| format!("roles update task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        match outcome {
            SetRolesOutcome::MissingPlayer => {
                followup_ephemeral(&responder, "❌ Player not registered.").await
            }
            SetRolesOutcome::Updated(_) => responder
                .followup(
                    InteractionResponse::message(format!(
                        "✅ Set your preferred roles to: {}",
                        roles
                            .iter()
                            .map(|role| format_role_display(role))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .without_mentions(),
                )
                .await
                .map_err(|error| error.to_string()),
        }
    }

    async fn region(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        if let Some(region) = string_option(options, "region") {
            let name = match region {
                "USE" => "US East",
                "USW" => "US West",
                _ => return followup_ephemeral(&responder, "❌ Invalid region").await,
            };
            let repository = self.registration.clone();
            let region = region.to_owned();
            let updated = tokio::task::spawn_blocking(move || {
                repository.set_preferred_region(user_id, Some(guild), &region)
            })
            .await
            .map_err(|error| format!("region update task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            if !updated {
                return followup_ephemeral(&responder, "❌ Player not registered.").await;
            }
            return followup_ephemeral(&responder, format!("✅ Set your server to **{name}**."))
                .await;
        }
        let repository = self.registration.clone();
        let preferences = tokio::task::spawn_blocking(move || {
            repository.player_preferences(user_id, Some(guild))
        })
        .await
        .map_err(|error| format!("region lookup task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let Some(preferences) = preferences else {
            return followup_ephemeral(&responder, "❌ Player not registered.").await;
        };
        let preferred = preferences
            .preferred_region
            .filter(|region| matches!(region.as_str(), "USE" | "USW"));
        let inferred = preferences
            .inferred_region
            .filter(|region| matches!(region.as_str(), "USE" | "USW"));
        let message = if let Some(region) = preferred {
            format!(
                "Your server is set to **{}**. Pick again to change it.",
                region_name(&region)
            )
        } else if let Some(region) = inferred {
            format!(
                "Your server is **{}** (inferred from your Dota history). Use `/player region` and pick one to lock it in.",
                region_name(&region)
            )
        } else {
            "You haven't set a server yet. Use `/player region` and pick US East or US West."
                .to_owned()
        };
        followup_ephemeral(&responder, message).await
    }

    async fn curfew_add(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let name = string_option(options, "name")
            .ok_or_else(|| "curfew add requires a name".to_owned())?
            .to_owned();
        let start = string_option(options, "start")
            .ok_or_else(|| "curfew add requires a start time".to_owned())?;
        let end = string_option(options, "end")
            .ok_or_else(|| "curfew add requires an end time".to_owned())?;
        let timezone = string_option(options, "timezone").map(str::to_owned);
        let days = string_option(options, "days").map(str::to_owned);
        let mode = string_option(options, "mode").map(str::to_owned);

        let (start_hour, start_minute) = match cama_domain::curfew::parse_clock(start) {
            Ok(parsed) => parsed,
            Err(error) => return followup_ephemeral(&responder, format!("❌ {error}")).await,
        };
        let (end_hour, end_minute) = match cama_domain::curfew::parse_clock(end) {
            Ok(parsed) => parsed,
            Err(error) => return followup_ephemeral(&responder, format!("❌ {error}")).await,
        };

        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let curfew = self.curfew.clone();
        let now = chrono::Utc::now();
        let result = tokio::task::spawn_blocking(move || {
            curfew.add_window(
                user_id,
                guild,
                &name,
                start_hour,
                start_minute,
                end_hour,
                end_minute,
                timezone.as_deref(),
                days.as_deref(),
                mode.as_deref(),
                now,
            )
        })
        .await
        .map_err(|error| format!("curfew add task failed: {error}"))?;

        let change = match result {
            Ok(change) => change,
            Err(CurfewServiceError::PlayerNotRegistered) => {
                return followup_ephemeral(&responder, "❌ Player not registered.").await;
            }
            Err(error) => {
                return followup_ephemeral(&responder, format!("❌ {error}")).await;
            }
        };

        // general_timezone reads SQLite, so it runs on a blocking thread like
        // the window query beside it.
        let curfew = self.curfew.clone();
        let general_timezone =
            tokio::task::spawn_blocking(move || curfew.general_timezone(user_id, guild))
                .await
                .map_err(|error| format!("curfew timezone task failed: {error}"))?
                .unwrap_or(None);
        let content = match change {
            cama_app::curfew_service::CurfewWindowChange::Applied(window) => {
                let enforcement = match window.mode {
                    cama_domain::curfew::CurfewMode::Default => {
                        "You'll be blocked from joining a lobby and auto-removed from any lobby \
                         you're already in while inside this window."
                            .to_owned()
                    }
                    cama_domain::curfew::CurfewMode::Strict => {
                        "You'll be blocked from joining a lobby and auto-removed from any lobby \
                         you're already in while inside this window. Shrinking or removing it \
                         only takes effect the next morning."
                            .to_owned()
                    }
                    cama_domain::curfew::CurfewMode::Informational => {
                        "Joining during this window asks you to confirm first; a yes covers you \
                         until your next completed match or the day rolls over."
                            .to_owned()
                    }
                };
                format!(
                    "✅ Window added: {}. {enforcement}",
                    cama_domain::curfew::format_window(&window, general_timezone.as_deref())
                )
            }
            cama_app::curfew_service::CurfewWindowChange::Staged {
                window,
                effective_at,
            } => format!(
                "🔒 **{}** is in {} mode and this edit shrinks it, so it won't take effect today — it's \
                 scheduled for {} (server time), and today's version keeps enforcing until then. New settings: {}",
                window.name,
                window.mode.as_str(),
                effective_at.format("%Y-%m-%d %H:%M UTC"),
                cama_domain::curfew::format_window(&window, general_timezone.as_deref())
            ),
        };
        followup_ephemeral(&responder, content).await
    }

    async fn curfew_remove(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let name = string_option(options, "name")
            .ok_or_else(|| "curfew remove requires a name".to_owned())?
            .to_owned();
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let curfew = self.curfew.clone();
        let remove_name = name.clone();
        let now = chrono::Utc::now();
        let outcome = tokio::task::spawn_blocking(move || {
            curfew.remove_window(user_id, guild, &remove_name, now)
        })
        .await
        .map_err(|error| format!("curfew remove task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        match outcome {
            cama_app::curfew_service::CurfewRemoveOutcome::Removed => {
                followup_ephemeral(&responder, format!("✅ Removed **{name}**.")).await
            }
            cama_app::curfew_service::CurfewRemoveOutcome::Staged { effective_at } => {
                followup_ephemeral(
                    &responder,
                    format!(
                        "🔒 **{name}** is in a mode that delays removals, so it stays live for the rest of \
                         today — the removal takes effect {} (server time).",
                        effective_at.format("%Y-%m-%d %H:%M UTC")
                    ),
                )
                .await
            }
            cama_app::curfew_service::CurfewRemoveOutcome::NotFound => {
                followup_ephemeral(
                    &responder,
                    format!(
                        "❌ You don't have a window named **{name}**. Use `/player curfew list` to see yours."
                    ),
                )
                .await
            }
        }
    }

    async fn curfew_list(
        &self,
        context: CommandContext,
        guild_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let curfew = self.curfew.clone();
        let windows = tokio::task::spawn_blocking(move || curfew.list_windows(user_id, guild))
            .await
            .map_err(|error| format!("curfew list task failed: {error}"))?
            .map_err(|error| error.to_string())?;
        if windows.is_empty() {
            return followup_ephemeral(
                &responder,
                "You don't have any curfew windows set. Use `/player curfew add`.",
            )
            .await;
        }
        // general_timezone and pending both read SQLite, so they run on a
        // blocking thread like the window query beside them.
        let curfew = self.curfew.clone();
        let (general_timezone, pending) = tokio::task::spawn_blocking(move || {
            let general_timezone = curfew.general_timezone(user_id, guild).unwrap_or(None);
            let pending = curfew.pending_changes(user_id, guild).unwrap_or_default();
            (general_timezone, pending)
        })
        .await
        .map_err(|error| format!("curfew timezone task failed: {error}"))?;
        let lines: Vec<String> = windows
            .iter()
            .map(|window| {
                let mut line =
                    cama_domain::curfew::format_window(window, general_timezone.as_deref());
                if let Some(change) = pending.get(&window.name) {
                    let effective_at = change.effective_at.format("%Y-%m-%d %H:%M UTC");
                    line.push_str(&match &change.window {
                        Some(_) => format!(" (edit scheduled for {effective_at})"),
                        None => format!(" (removal scheduled for {effective_at})"),
                    });
                }
                line
            })
            .collect();
        followup_ephemeral(
            &responder,
            format!(
                "Your curfew windows:\n{}",
                lines
                    .iter()
                    .map(|line| format!("- {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .await
    }

    async fn timezone_set(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let timezone = string_option(options, "timezone")
            .ok_or_else(|| "timezone set requires a timezone".to_owned())?
            .to_owned();
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let service = TimezoneService::new(self.registration.clone());
        let result = tokio::task::spawn_blocking(move || {
            service.set_timezone(user_id, Some(guild), &timezone)
        })
        .await
        .map_err(|error| format!("timezone set task failed: {error}"))?;
        match result {
            Ok(()) => {
                followup_ephemeral(
                    &responder,
                    "✅ Timezone set. Curfew windows will use this unless a window was given \
                     its own timezone with `/player curfew add`.",
                )
                .await
            }
            Err(TimezoneServiceError::PlayerNotRegistered) => {
                followup_ephemeral(&responder, "❌ Player not registered.").await
            }
            Err(error) => followup_ephemeral(&responder, format!("❌ {error}")).await,
        }
    }

    async fn timezone_status(
        &self,
        context: CommandContext,
        guild_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let service = TimezoneService::new(self.registration.clone());
        let result =
            tokio::task::spawn_blocking(move || service.timezone_info(user_id, Some(guild)))
                .await
                .map_err(|error| format!("timezone status task failed: {error}"))?;
        match result {
            Ok(Some(timezone)) => {
                followup_ephemeral(&responder, format!("Your timezone is **{timezone}**.")).await
            }
            Ok(None) => {
                followup_ephemeral(
                    &responder,
                    format!(
                        "You haven't set a timezone yet (defaults to {} where needed). \
                         Use `/player timezone set`.",
                        cama_domain::timezone::DEFAULT_TIMEZONE
                    ),
                )
                .await
            }
            Err(TimezoneServiceError::PlayerNotRegistered) => {
                followup_ephemeral(&responder, "❌ Player not registered.").await
            }
            Err(error) => followup_ephemeral(&responder, format!("❌ {error}")).await,
        }
    }

    async fn timezone_list(&self, responder: Arc<dyn InteractionResponder>) -> Result<(), String> {
        followup_ephemeral(&responder, cama_domain::timezone::format_common_timezones()).await
    }

    async fn playtime_set(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let raw_hours = string_option(options, "hours")
            .ok_or_else(|| "playtime set requires hours".to_owned())?;
        let hours = match cama_domain::playtime::parse_hour_set(raw_hours) {
            Ok(hours) => hours,
            Err(error) => return followup_ephemeral(&responder, format!("❌ {error}")).await,
        };
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let service = PlaytimeService::new(self.registration.clone());
        let saved_hours: Vec<i64> = hours.iter().map(|&hour| i64::from(hour)).collect();
        let result = tokio::task::spawn_blocking(move || {
            service.set_play_hours(user_id, Some(guild), &saved_hours)
        })
        .await
        .map_err(|error| format!("playtime set task failed: {error}"))?;
        match result {
            Ok(()) => {
                followup_ephemeral(
                    &responder,
                    format!(
                        "✅ Play-time hours set: **{}**. This is informational only — it \
                         doesn't affect queueing.",
                        cama_domain::playtime::format_hour_ranges(&hours)
                    ),
                )
                .await
            }
            Err(PlaytimeServiceError::PlayerNotRegistered) => {
                followup_ephemeral(&responder, "❌ Player not registered.").await
            }
            Err(error) => followup_ephemeral(&responder, format!("❌ {error}")).await,
        }
    }

    async fn playtime_clear(
        &self,
        context: CommandContext,
        guild_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let service = PlaytimeService::new(self.registration.clone());
        let result =
            tokio::task::spawn_blocking(move || service.clear_play_hours(user_id, Some(guild)))
                .await
                .map_err(|error| format!("playtime clear task failed: {error}"))?;
        match result {
            Ok(()) => followup_ephemeral(&responder, "✅ Play-time hours cleared.").await,
            Err(PlaytimeServiceError::PlayerNotRegistered) => {
                followup_ephemeral(&responder, "❌ Player not registered.").await
            }
            Err(error) => followup_ephemeral(&responder, format!("❌ {error}")).await,
        }
    }

    async fn playtime_status(
        &self,
        context: CommandContext,
        guild_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let service = PlaytimeService::new(self.registration.clone());
        let result =
            tokio::task::spawn_blocking(move || service.play_hours_info(user_id, Some(guild)))
                .await
                .map_err(|error| format!("playtime status task failed: {error}"))?;
        match result {
            Ok(Some(hours)) => {
                let hour_set: std::collections::BTreeSet<u32> = hours
                    .into_iter()
                    .filter_map(|hour| u32::try_from(hour).ok())
                    .collect();
                followup_ephemeral(
                    &responder,
                    format!(
                        "Your play-time hours: **{}**.",
                        cama_domain::playtime::format_hour_ranges(&hour_set)
                    ),
                )
                .await
            }
            Ok(None) => {
                followup_ephemeral(
                    &responder,
                    "You haven't set any play-time hours yet. Use `/player playtime set`.",
                )
                .await
            }
            Err(PlaytimeServiceError::PlayerNotRegistered) => {
                followup_ephemeral(&responder, "❌ Player not registered.").await
            }
            Err(error) => followup_ephemeral(&responder, format!("❌ {error}")).await,
        }
    }

    async fn playtime_popular(
        &self,
        guild_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let guild = signed_id(guild_id, "guild")?;
        let service = PlaytimeService::new(self.registration.clone());
        let counts =
            tokio::task::spawn_blocking(move || service.popular_play_hours(Some(guild), None))
                .await
                .map_err(|error| format!("playtime popular task failed: {error}"))?
                .map_err(|error| error.to_string())?;
        if counts.iter().all(|&count| count == 0) {
            return followup_ephemeral(
                &responder,
                "No one has set their play-time hours yet. Try `/player playtime set`.",
            )
            .await;
        }
        let lines: Vec<String> = counts
            .iter()
            .enumerate()
            .map(|(hour, &count)| {
                let bar = "█".repeat(count as usize);
                let spacer = if count > 0 { " " } else { "" };
                format!("{hour:02}:00 | {bar}{spacer}{count}")
            })
            .collect();
        followup_ephemeral(
            &responder,
            format!(
                "**🎮 Most Popular Dota Hours** (times in {})\n```\n{}\n```",
                cama_domain::timezone::DEFAULT_TIMEZONE,
                lines.join("\n")
            ),
        )
        .await
    }

    async fn exclusion(
        &self,
        context: CommandContext,
        guild_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let repository = self.registration.clone();
        let user_id = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let preferences = tokio::task::spawn_blocking(move || {
            repository.player_preferences(user_id, Some(guild))
        })
        .await
        .map_err(|error| format!("exclusion lookup task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let Some(preferences) = preferences else {
            return followup_ephemeral(
                &responder,
                "You are not registered. Use `/player register` first.",
            )
            .await;
        };
        followup_ephemeral(
            &responder,
            format!(
                "Your exclusion factor is **{}**.\nHigher = more priority to play next game when there are extra players.",
                preferences.exclusion_count
            ),
        )
        .await
    }

    async fn autonotify(
        &self,
        context: CommandContext,
        guild_id: u64,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let enabled = boolean_option(options, "enabled");
        let target = user_option(options, "playername")?;
        if enabled.is_some() && target.is_some() {
            return followup_ephemeral(
                &responder,
                "❌ Choose either `playername` for a one-time DM or `enabled` for public 8/9-player alerts, not both.",
            )
            .await;
        }
        let subscriber_id = signed_id(context.user_id, "subscriber")?;
        let guild = signed_id(guild_id, "guild")?;
        let Some(target) = target else {
            let enabled = enabled.unwrap_or(true);
            let notifications = self.notifications.clone();
            let saved = tokio::task::spawn_blocking(move || {
                notifications.set_preference(
                    subscriber_id,
                    Some(guild),
                    NotificationKind::Lobby,
                    enabled,
                    now_seconds(),
                )
            })
            .await
            .map_err(|error| format!("notification preference task failed: {error}"))?;
            if let Err(error) = saved {
                error!(%error, "failed to save lobby notification preference");
                return followup_ephemeral(
                    &responder,
                    "❌ Couldn't update your lobby notification preference. Try again later.",
                )
                .await;
            }
            let message = if enabled {
                "✅ Lobby auto-notify is now **ON**. You'll be pinged with 📋 when a lobby is filling up.\nThis stays enabled across lobbies; reacting 📋 still subscribes only to the current lobby."
            } else {
                "🔕 Lobby auto-notify is now **OFF**."
            };
            return followup_ephemeral(&responder, message).await;
        };
        if target.raw_id == context.user_id {
            return followup_ephemeral(
                &responder,
                "❌ You can't subscribe to your own lobby signup.",
            )
            .await;
        }
        if target.is_bot == Some(true) {
            return followup_ephemeral(
                &responder,
                "❌ Bot accounts can't sign up for player lobbies.",
            )
            .await;
        }
        let target_name = escape_discord_text(&self.render_player_name(guild_id, target.raw_id));
        let target = target.id;
        let notifications = self.notifications.clone();
        let duplicate = tokio::task::spawn_blocking(move || {
            notifications.has_lobby_target_subscription(subscriber_id, target, Some(guild))
        })
        .await
        .map_err(|error| format!("notification duplicate task failed: {error}"));
        if matches!(duplicate, Ok(Ok(true))) {
            return followup_ephemeral(
                &responder,
                format!("🔔 You already have a one-time lobby alert set for **{target_name}**."),
            )
            .await;
        }
        let preflight = format!(
            "✅ DM check passed for your one-time lobby alert request about **{target_name}**."
        );
        let receipt = match self
            .discord
            .send_direct_message_with_receipt(
                context.user_id,
                DiscordMessage::silent(InteractionResponse::message(preflight).without_mentions()),
            )
            .await
        {
            Ok(receipt) => receipt,
            Err(error) if error.kind == DiscordDirectMessageErrorKind::Forbidden => {
                return followup_ephemeral(
                    &responder,
                    "❌ I can't DM you. Enable direct messages from server members, then try again.",
                )
                .await;
            }
            Err(error) => {
                warn!(%error.message, "Discord rejected lobby target DM preflight");
                return followup_ephemeral(
                    &responder,
                    "❌ I couldn't verify that I can DM you. Try again in a moment.",
                )
                .await;
            }
        };
        let notifications = self.notifications.clone();
        let created = tokio::task::spawn_blocking(move || {
            notifications.add_lobby_target_subscription(
                subscriber_id,
                target,
                Some(guild),
                now_nanos_i64(),
            )
        })
        .await
        .map_err(|error| format!("notification save task failed: {error}"))?;
        let message = match created {
            Ok(true) => format!(
                "🔔 One-time lobby alert set for **{target_name}**. I'll DM you the next time they join a lobby in this server."
            ),
            Ok(false) => {
                format!("🔔 You already have a one-time lobby alert set for **{target_name}**.")
            }
            Err(error) => {
                error!(%error, "failed to save one-shot lobby alert");
                let message =
                    "❌ Your DM check passed, but I couldn't save the alert. Try again later."
                        .to_owned();
                let _ = self
                    .discord
                    .edit_direct_message(
                        &receipt,
                        DiscordMessage::silent(
                            InteractionResponse::message(&message).without_mentions(),
                        ),
                    )
                    .await;
                return followup_ephemeral(&responder, message).await;
            }
        };
        if let Err(error) = self
            .discord
            .edit_direct_message(
                &receipt,
                DiscordMessage::silent(InteractionResponse::message(&message).without_mentions()),
            )
            .await
        {
            debug!(%error, "could not update lobby-alert preflight DM");
        }
        followup_ephemeral(&responder, message).await
    }

    async fn lobby_status(
        &self,
        context: CommandContext,
        guild_id: u64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let moderation = self.moderation.clone();
        let low_priority = self.low_priority.clone();
        let user = signed_id(context.user_id, "user")?;
        let guild = signed_id(guild_id, "guild")?;
        let now = now_seconds();
        // Both lookups are for the caller's own row in their own guild, so the
        // command can never surface another player's restrictions.
        let (suspension, low_priority) = tokio::task::spawn_blocking(move || {
            let suspension = ModerationService::new(moderation)
                .get_active_suspension(user, Some(guild), None, now)
                .map_err(|error| format!("{error:?}"))?;
            let low_priority = low_priority
                .get_state(user, Some(guild))
                .map_err(|error| error.to_string())?
                .filter(|state| state.active && state.wins_remaining > 0);
            Ok::<_, String>((suspension, low_priority))
        })
        .await
        .map_err(|error| format!("lobby status lookup task failed: {error}"))??;
        let mut sections = Vec::new();
        if let Some(suspension) = suspension.as_ref() {
            sections.push(format_suspension(suspension));
        }
        if let Some(state) = low_priority.as_ref() {
            sections.push(format_low_priority(state));
        }
        let message = if sections.is_empty() {
            "✅ All clear — you have no active lobby suspension or low priority.".to_owned()
        } else {
            sections.join("\n\n")
        };
        followup_ephemeral(&responder, message).await
    }

    async fn player_exists(&self, user_id: i64, guild_id: i64) -> Result<bool, String> {
        let repository = self.registration.clone();
        tokio::task::spawn_blocking(move || repository.player_exists(user_id, Some(guild_id)))
            .await
            .map_err(|error| format!("player lookup task failed: {error}"))?
            .map_err(|error| error.to_string())
    }

    async fn steam_ids(&self, user_id: i64) -> Result<Vec<i64>, String> {
        let repository = self.steam.clone();
        tokio::task::spawn_blocking(move || repository.get_steam_ids(user_id))
            .await
            .map_err(|error| format!("Steam account lookup task failed: {error}"))?
            .map_err(|error| error.to_string())
    }
}

fn leaf_command(options: &[InteractionOption]) -> Result<(String, &[InteractionOption]), String> {
    let Some(option) = options.iter().find(|option| {
        matches!(
            option.value,
            InteractionValue::Subcommand(_) | InteractionValue::SubcommandGroup(_)
        )
    }) else {
        return Err("player command is missing a subcommand".to_owned());
    };
    match &option.value {
        InteractionValue::Subcommand(children) => Ok((option.name.clone(), children)),
        InteractionValue::SubcommandGroup(children) => {
            let (leaf, options) = leaf_command(children)?;
            Ok((format!("{} {leaf}", option.name), options))
        }
        _ => unreachable!("filtered to a nested command"),
    }
}

async fn defer_ephemeral(responder: &Arc<dyn InteractionResponder>) -> bool {
    if let Err(error) = responder.defer(true).await {
        warn!(%error, "unable to defer registration interaction");
        false
    } else {
        true
    }
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    message: impl Into<String>,
) -> Result<(), String> {
    responder
        .respond(
            InteractionResponse::message(message)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string())
}

async fn followup_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    message: impl Into<String>,
) -> Result<(), String> {
    responder
        .followup(
            InteractionResponse::message(message)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string())
}

fn parse_mmr_custom_id(custom_id: &str) -> Result<(&str, u64, u64), String> {
    let suffix = custom_id
        .strip_prefix(MMR_COMPONENT_PREFIX)
        .ok_or_else(|| "unknown MMR callback".to_owned())?;
    let mut pieces = suffix.split(':');
    let kind = pieces
        .next()
        .ok_or_else(|| "missing callback kind".to_owned())?;
    let guild_id = pieces
        .next()
        .ok_or_else(|| "missing callback guild".to_owned())?
        .parse()
        .map_err(|_| "invalid callback guild".to_owned())?;
    let nonce = pieces
        .next()
        .ok_or_else(|| "missing callback nonce".to_owned())?
        .parse()
        .map_err(|_| "invalid callback nonce".to_owned())?;
    if pieces.next().is_some() {
        return Err("invalid MMR callback".to_owned());
    }
    Ok((kind, guild_id, nonce))
}

fn prompt_is_usable(state: &MmrPromptState, user_id: u64, retry_limit: i64) -> bool {
    !state.completed
        && state.discord_id == i64::try_from(user_id).unwrap_or(-1)
        && state.expires_at >= now_seconds()
        && state.attempts < retry_limit
}

fn is_user_registration_error(error: &RegisterPlayerError) -> bool {
    matches!(
        error,
        RegisterPlayerError::InvalidSteamId
            | RegisterPlayerError::PlayerAlreadyRegistered
            | RegisterPlayerError::SteamAlreadyOwned { .. }
            | RegisterPlayerError::PlayerDataUnavailable
            | RegisterPlayerError::OpenDota(_)
    )
}

fn valid_steam_id(steam_id: i64) -> bool {
    steam_id > 0 && steam_id < STEAM_ACCOUNT_ID_UPPER_BOUND
}

fn signed_id(id: u64, kind: &str) -> Result<i64, String> {
    i64::try_from(id).map_err(|_| format!("{kind} ID {id} exceeds SQLite's signed range"))
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}

fn now_nanos_i64() -> i64 {
    i64::try_from(now_nanos()).unwrap_or(i64::MAX)
}

fn region_name(region: &str) -> &str {
    match region {
        "USE" => "US East",
        "USW" => "US West",
        other => other,
    }
}

fn project_counts_payload(value: &JsonValue) -> CountsPayload {
    let region = value
        .get("region")
        .and_then(JsonValue::as_object)
        .map(|regions| {
            regions
                .iter()
                .map(|(key, entry)| {
                    let projected = if let Some(object) = entry.as_object() {
                        object
                            .get("games")
                            .map_or(RegionCountEntry::GamesMissing, |games| {
                                RegionCountEntry::Games(project_game_count(games))
                            })
                    } else {
                        RegionCountEntry::Bare(project_game_count(entry))
                    };
                    (key.clone(), projected)
                })
                .collect()
        });
    CountsPayload { region }
}

fn project_game_count(value: &JsonValue) -> GameCountValue {
    match value {
        JsonValue::Null => GameCountValue::Null,
        JsonValue::Bool(value) => GameCountValue::Boolean(*value),
        JsonValue::Number(value) => value.as_i64().map_or_else(
            || {
                value.as_u64().map_or_else(
                    || {
                        value
                            .as_f64()
                            .map_or(GameCountValue::Malformed, GameCountValue::Float)
                    },
                    GameCountValue::UnsignedInteger,
                )
            },
            GameCountValue::Integer,
        ),
        JsonValue::String(value) => GameCountValue::String(value.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => GameCountValue::Malformed,
    }
}

fn format_suspension(state: &LobbySuspension) -> String {
    let scope = match state.scope {
        SuspensionScope::All => "all matchmaking lobbies",
        SuspensionScope::Open => "🍽️ All You Can Feed",
        SuspensionScope::Lowskill => "🧀 Whine & Cheese",
    };
    let mut terms = Vec::new();
    if let Some(expires_at) = state.expires_at {
        terms.push(format!("until <t:{expires_at}:F> (<t:{expires_at}:R>)"));
    }
    if let Some(matches) = state.matches_remaining {
        let noun = if matches == 1 { "match" } else { "matches" };
        terms.push(format!("{matches} completed {noun} remaining"));
    }
    let joiner = if state.completion == SuspensionCompletion::Either {
        " or "
    } else {
        " and "
    };
    let terms = if terms.is_empty() {
        "no remaining term".to_owned()
    } else {
        terms.join(joiner)
    };
    format!(
        "**Lobby suspension** — {scope}\nTerm: {terms}\nReason: {}",
        state.reason
    )
}

/// Render the caller's own low-priority progress.
///
/// This deliberately omits `set_by`: the player is shown what they are serving
/// and why, never which admin issued it, matching the assignment DM. The reason
/// goes through the same `player_visible_reason` normaliser as that DM so both
/// player-facing surfaces clip and defuse it identically.
fn format_low_priority(state: &LowPriorityState) -> String {
    let completed = state.wins_required.saturating_sub(state.wins_remaining);
    let required_noun = if state.wins_required == 1 {
        "win"
    } else {
        "wins"
    };
    let remaining_noun = if state.wins_remaining == 1 {
        "win"
    } else {
        "wins"
    };
    let mut rendered = format!(
        "**Low priority** — {completed}/{} {required_noun} completed ({} {remaining_noun} remaining)",
        state.wins_required, state.wins_remaining
    );
    // Rows written before the option was documented as player-facing carry
    // `reason_player_visible = 0`; their reason may name a third party, so the
    // player gets their progress without it. "Placed for" rather than "Reason"
    // keeps this attributable when a suspension is rendered alongside it.
    if state.reason_player_visible
        && let Some(reason) = player_visible_reason(state.reason.as_deref())
    {
        rendered.push_str(&format!("\nPlaced for: {reason}"));
    }
    rendered
}

struct SqliteNeonEventPort {
    repository: NeonEventRepository,
}

impl NeonEventPort for SqliteNeonEventPort {
    fn load_one_time_events(&self) -> Result<Vec<EventKey>, StoreError> {
        self.repository
            .load_one_time_events()
            .map(|events| {
                events
                    .into_iter()
                    .filter_map(|event| {
                        Some(EventKey::new(
                            u64::try_from(event.discord_id).ok()?,
                            u64::try_from(event.guild_id).ok(),
                            event.event_type,
                        ))
                    })
                    .collect()
            })
            .map_err(|error| StoreError(error.to_string()))
    }

    fn check_one_time_event(&self, key: &EventKey) -> Result<bool, StoreError> {
        let discord_id = i64::try_from(key.discord_id)
            .map_err(|_| StoreError("Discord ID exceeds SQLite range".to_owned()))?;
        let guild_id = i64::try_from(key.guild_id)
            .map_err(|_| StoreError("guild ID exceeds SQLite range".to_owned()))?;
        self.repository
            .check_one_time_event(discord_id, guild_id, &key.event_type)
            .map_err(|error| StoreError(error.to_string()))
    }

    fn persist_one_time_event(&self, key: EventKey, layer: u8) -> Result<(), StoreError> {
        let discord_id = i64::try_from(key.discord_id)
            .map_err(|_| StoreError("Discord ID exceeds SQLite range".to_owned()))?;
        let guild_id = i64::try_from(key.guild_id)
            .map_err(|_| StoreError("guild ID exceeds SQLite range".to_owned()))?;
        self.repository
            .persist_one_time_event(
                discord_id,
                guild_id,
                &key.event_type,
                i64::from(layer),
                now_seconds(),
            )
            .map_err(|error| StoreError(error.to_string()))
    }
}

#[cfg(all(test, feature = "runtime-test-draft"))]
#[path = "registration_provider/tests.rs"]
mod tests;
