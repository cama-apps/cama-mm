//! Live `/help`, `/leaderboard`, and `/calibration` Discord provider.
//!
//! The provider reads the migrated production SQLite schema directly through
//! the existing repositories, resolves current guild membership through the
//! live Discord transport before applying display limits, and keeps the
//! tabbed leaderboard state only for the same fourteen-minute lifetime as the
//! former Python view.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::unified_leaderboard::{
    BalanceLeaderboard, BankruptcyState, ButtonStyle, GamblingEntry, GamblingServerStats,
    GamblingSnapshot, GuildMemberDirectory, LeaderboardTab, NegativeBalanceEntry, RatingKind,
    ScopedTipLeaderboardEntry, TRIVIA_WINDOW_DAYS, TabData, TipsSnapshot, TriviaEntry,
    UnifiedLeaderboardRepositoryPort, UnifiedLeaderboardView,
};
use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::core_repositories::PlayerRepository;
use cama_db::gambling_stats_repository::{GamblingStatsRepository, GamblingStatsService};
use cama_db::tip_repository::TipRepository;
use cama_db::trivia_commands_repository::TriviaCommandsRepository;
use cama_domain::embed_safety::EmbedModel;
use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::tip_service::{TipLeaderboardEntry, TipVolume};
use rusqlite::{Connection, OpenFlags};
use tracing::warn;

/// Read-only counts returned by the production `/leaderboard` repository
/// adapters.  The snapshot smoke uses these normalized values so a copied
/// current-schema database can be exercised without a Discord transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InfoAnalyticsSnapshot {
    pub player_count: usize,
    pub balance_candidates: usize,
    pub glicko_candidates: usize,
    pub openskill_candidates: usize,
    pub gambling_entries: usize,
    pub tip_sender_entries: usize,
    pub tip_receiver_entries: usize,
    pub trivia_entries: usize,
}

/// Read the production Info/profile analytics surfaces from an already
/// admitted SQLite database.  This calls the same repository implementation
/// used by `/leaderboard`; it does not migrate or write the database.
pub fn read_info_analytics_snapshot(
    database_path: impl AsRef<Path>,
    guild_id: i64,
    limit: usize,
) -> Result<InfoAnalyticsSnapshot, String> {
    let mut repository = InfoLeaderboardRepository::new(database_path);
    let guild = Some(guild_id);
    let player_count = repository.player_count(guild)?;
    let balance_candidates = repository.balance_candidates(guild, limit)?.len();
    let glicko_candidates = repository
        .rating_candidates(guild, RatingKind::Glicko, limit)?
        .len();
    let openskill_candidates = repository
        .rating_candidates(guild, RatingKind::OpenSkill, limit)?
        .len();
    let gambling_entries = repository
        .gambling_snapshot(guild, limit)?
        .map_or(0, |snapshot| snapshot.entries.len());
    let (tip_sender_entries, tip_receiver_entries) = repository
        .tips_snapshot(guild, limit)?
        .map_or((0, 0), |snapshot| {
            (snapshot.top_senders.len(), snapshot.top_receivers.len())
        });
    let trivia_entries = repository
        .trivia_candidates(guild, TRIVIA_WINDOW_DAYS, limit)?
        .len();
    Ok(InfoAnalyticsSnapshot {
        player_count,
        balance_candidates,
        glicko_candidates,
        openskill_candidates,
        gambling_entries,
        tip_sender_entries,
        tip_receiver_entries,
        trivia_entries,
    })
}

use crate::application_config::ApplicationConfig;
use crate::discord_transport::{DiscordTransport, GuildPlayerNameDirectory};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionActionRow, InteractionAllowedMentions, InteractionButton, InteractionButtonStyle,
    InteractionEmbed, InteractionHandler, InteractionHandlerError, InteractionMessageDelivery,
    InteractionMessageReceipt, InteractionOption, InteractionRequest, InteractionResponder,
    InteractionResponse, InteractionValue, RegistrationError, RegistrationProvider,
    RegistryBuilder,
};

#[path = "info_provider/calibration.rs"]
mod calibration;

use calibration::{CalibrationDataSources, calibration_response};

const COMPONENT_PREFIX: &str = "info:";
const VIEW_TIMEOUT: Duration = Duration::from_secs(840);
const TAB_CLICK_WINDOW: Duration = Duration::from_millis(1_500);
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const DISCORD_BLUE: u32 = 0x34_98_DB;
const DISCORD_GOLD: u32 = 0xF1_C4_0F;
const TIPS_PINK: u32 = 0xE9_1E_63;
const TRIVIA_ORANGE: u32 = 0xFF_A0_00;

#[derive(Clone)]
pub struct InfoRegistrationProvider {
    handler: Arc<InfoHandler>,
}

impl InfoRegistrationProvider {
    /// Compose the live information surface from the migrated database, the
    /// immutable runtime configuration, and the production Discord transport.
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
    ) -> Self {
        Self::with_timeout(database_path, config, discord, VIEW_TIMEOUT)
    }

    fn with_timeout(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
        view_timeout: Duration,
    ) -> Self {
        let path = database_path.as_ref().to_path_buf();
        let leaderboard = InfoLeaderboardRepository::new(&path);
        Self {
            handler: Arc::new(InfoHandler {
                leaderboard,
                calibration: CalibrationDataSources::new(&path, config),
                discord,
                admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
                leverage_tiers: config.values.leverage_tiers.clone(),
                pingedash_cost: config.values.pingedash_cost,
                pingedkevin_cost: config.values.pingedkevin_cost,
                rate_limits: Mutex::new(CommandRateLimits::default()),
                views: Arc::new(Mutex::new(BTreeMap::new())),
                next_view_id: AtomicU64::new(1),
                view_timeout,
            }),
        }
    }
}

impl RegistrationProvider for InfoRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "help".to_owned(),
            description: "List all available commands".to_owned(),
            options: Vec::new(),
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "leaderboard".to_owned(),
            description: "View leaderboard (balance, gambling, ratings)".to_owned(),
            options: vec![
                CommandOptionSpec::new(
                    "type",
                    "Leaderboard type (default: balance)",
                    CommandOptionKind::String,
                )
                .choices(vec![
                    string_choice("Balance", "balance"),
                    string_choice("Glicko-2 Rating", "glicko"),
                    string_choice("OpenSkill Rating", "openskill"),
                    string_choice("Gambling", "gambling"),
                    string_choice("Tips", "tips"),
                    string_choice("Trivia", "trivia"),
                ]),
                CommandOptionSpec::new(
                    "limit",
                    "Number of entries to show (default: 100, max: 100)",
                    CommandOptionKind::Integer,
                ),
            ],
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "calibration".to_owned(),
            description: "View rating system stats and calibration progress".to_owned(),
            options: vec![CommandOptionSpec::new(
                "user",
                "Optional: View detailed stats for a specific player",
                CommandOptionKind::User,
            )],
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

fn string_choice(name: &str, value: &str) -> CommandOptionChoice {
    CommandOptionChoice::String {
        name: name.to_owned(),
        value: value.to_owned(),
    }
}

struct InfoHandler {
    leaderboard: InfoLeaderboardRepository,
    calibration: CalibrationDataSources,
    discord: Arc<dyn DiscordTransport>,
    admin_user_ids: BTreeSet<i64>,
    leverage_tiers: Vec<i64>,
    pingedash_cost: i64,
    pingedkevin_cost: i64,
    rate_limits: Mutex<CommandRateLimits>,
    views: Arc<Mutex<BTreeMap<u64, Arc<LeaderboardSession>>>>,
    next_view_id: AtomicU64,
    view_timeout: Duration,
}

struct LeaderboardSession {
    view: Arc<Mutex<UnifiedLeaderboardView<InfoLeaderboardRepository>>>,
    last_tab_click: Mutex<BTreeMap<u64, Instant>>,
    /// Bumped on every accepted interaction so the timer armed for an earlier
    /// one expires nothing. The legacy view measured its timeout from the last
    /// interaction, not from creation.
    expiry: Mutex<LeaderboardExpiry>,
}

#[derive(Default)]
struct LeaderboardExpiry {
    generation: u64,
    receipt: Option<InteractionMessageReceipt>,
    /// The responder that created the followup. Deleting an interaction
    /// followup goes through the token that produced it, so a later
    /// component's responder cannot stand in for it.
    owner: Option<Arc<dyn InteractionResponder>>,
}

#[derive(Debug, Default)]
struct CommandRateLimits {
    hits: BTreeMap<(&'static str, u64, u64), VecDeque<Instant>>,
    max_window_seen: Duration,
    next_purge_at: Option<Instant>,
}

impl CommandRateLimits {
    fn retry_after(
        &mut self,
        scope: &'static str,
        guild_id: Option<u64>,
        user_id: u64,
        limit: usize,
        window: Duration,
    ) -> Option<u64> {
        const PURGE_INTERVAL: Duration = Duration::from_secs(300);
        const PURGE_SIZE_THRESHOLD: usize = 1_024;

        let now = Instant::now();
        self.max_window_seen = self.max_window_seen.max(window);
        if self.next_purge_at.is_none_or(|next| now >= next) {
            self.next_purge_at = now.checked_add(PURGE_INTERVAL);
            if self.hits.len() >= PURGE_SIZE_THRESHOLD {
                let max_window = self.max_window_seen;
                self.hits.retain(|_, hits| {
                    hits.back()
                        .is_some_and(|started| now.duration_since(*started) <= max_window)
                });
            }
        }
        let hits = self
            .hits
            .entry((scope, guild_id.unwrap_or_default(), user_id))
            .or_default();
        while hits
            .front()
            .is_some_and(|started| now.duration_since(*started) > window)
        {
            hits.pop_front();
        }
        if hits.len() < limit {
            hits.push_back(now);
            return None;
        }
        hits.front().map(|started| {
            let remaining = window.saturating_sub(now.duration_since(*started));
            remaining
                .as_secs()
                .saturating_add(u64::from(remaining.subsec_nanos() != 0))
        })
    }
}

#[async_trait]
impl InteractionHandler for InfoHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command { ref name, .. } if name == "help" => {
                self.handle_help(request, responder).await
            }
            InteractionRequest::Command { ref name, .. } if name == "leaderboard" => {
                self.handle_leaderboard(request, responder).await
            }
            InteractionRequest::Command { ref name, .. } if name == "calibration" => {
                self.handle_calibration(request, responder).await
            }
            InteractionRequest::Component { .. } => {
                self.handle_leaderboard_component(request, responder).await
            }
            InteractionRequest::Command { name, .. } => {
                Err(format!("info handler received command {name:?}").into())
            }
            InteractionRequest::Autocomplete { .. } => {
                Err("info extension has no autocomplete routes".into())
            }
            InteractionRequest::Modal { .. } => Err("info extension has no modal routes".into()),
        }
    }
}

/// How a leaderboard's candidate ids were classified: from the guild's member
/// cache snapshot, or not at all because this transport keeps no cache.
enum MemberResolution {
    Snapshot(GuildMemberDirectory),
    Uncached(Vec<(i64, String)>),
}

impl InfoHandler {
    async fn handle_help(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            user_id,
            member_permissions,
            ..
        } = request
        else {
            return Err("help handler received a component".into());
        };
        if let Err(error) = responder.defer(true).await {
            warn!(%error, "unable to defer /help interaction");
            return Ok(());
        }
        let is_admin = i64::try_from(user_id)
            .ok()
            .is_some_and(|id| self.admin_user_ids.contains(&id))
            || member_permissions.is_some_and(|permissions| {
                permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            });
        responder
            .followup(
                InteractionResponse::message("")
                    .embed(help_embed(
                        &self.leverage_tiers,
                        self.pingedash_cost,
                        self.pingedkevin_cost,
                        is_admin,
                    ))
                    .ephemeral(),
            )
            .await
            .map_err(response_error)
    }

    async fn handle_leaderboard(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            user_id,
            guild_id,
            options,
            ..
        } = request
        else {
            return Err("leaderboard handler received a component".into());
        };
        let retry_after = {
            self.rate_limits
                .lock()
                .map_err(|_| InteractionHandlerError::from("info rate-limit lock was poisoned"))?
                .retry_after("leaderboard", guild_id, user_id, 3, Duration::from_secs(20))
        };
        if let Some(retry_after) = retry_after {
            return responder
                .respond(
                    InteractionResponse::message(format!(
                        "⏳ Please wait {retry_after}s before using `/leaderboard` again."
                    ))
                    .ephemeral(),
                )
                .await
                .map_err(response_error);
        }
        if let Err(error) = responder.defer(false).await {
            warn!(%error, "unable to defer /leaderboard interaction; attempting followup fallback");
        }

        let selected_type = string_option(&options, "type");
        let limit = integer_option(&options, "limit")
            .unwrap_or(100)
            .clamp(1, 100) as usize;
        let tab = LeaderboardTab::from_type_parameter(selected_type.as_deref());
        let signed_guild = signed_optional_id(guild_id, "guild")?;

        let members = match guild_id {
            Some(guild_id) => match self.member_directory(guild_id, signed_guild).await {
                Ok(members) => Some(members),
                Err(error) => {
                    warn!(%error, "unable to resolve /leaderboard guild members");
                    return responder
                        .followup(leaderboard_failure_response())
                        .await
                        .map_err(response_error);
                }
            },
            None => None,
        };
        let mut view = UnifiedLeaderboardView::with_options(
            self.leaderboard.clone(),
            signed_guild,
            members,
            tab,
            limit,
        );
        view = match tokio::task::spawn_blocking(move || {
            view.load_tab(tab)
                .map_err(|error| error.to_string())
                .map(|_| view)
        })
        .await
        .unwrap_or_else(|error| Err(format!("leaderboard load task failed: {error}")))
        {
            Ok(view) => view,
            Err(error) => {
                warn!(%error, "unable to load /leaderboard");
                return responder
                    .followup(leaderboard_failure_response())
                    .await
                    .map_err(response_error);
            }
        };

        let view_id = self.next_view_id.fetch_add(1, Ordering::Relaxed);
        let view = Arc::new(Mutex::new(view));
        let response = {
            let view = view
                .lock()
                .map_err(|_| InteractionHandlerError::from("leaderboard view lock poisoned"))?;
            leaderboard_response(view_id, &view)
        };
        let session = Arc::new(LeaderboardSession {
            view: Arc::clone(&view),
            last_tab_click: Mutex::new(BTreeMap::new()),
            expiry: Mutex::new(LeaderboardExpiry::default()),
        });
        self.views
            .lock()
            .map_err(|_| InteractionHandlerError::from("leaderboard registry lock poisoned"))?
            .insert(view_id, session);
        let receipt = match responder.followup_with_receipt(response).await {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Ok(mut views) = self.views.lock() {
                    views.remove(&view_id);
                }
                return Err(response_error(error));
            }
        };
        if let Ok(views) = self.views.lock()
            && let Some(session) = views.get(&view_id)
            && let Ok(mut expiry) = session.expiry.lock()
        {
            expiry.receipt = receipt;
            expiry.owner = Some(Arc::clone(&responder));
        }
        self.schedule_expiry(view_id, 0, responder);
        Ok(())
    }

    /// Classify every leaderboard candidate against the guild's member cache.
    ///
    /// This used to issue one live member request per unresolved candidate,
    /// sequentially, on every `/leaderboard`: the candidate set is the union of
    /// players, bettors, tip senders and recipients, and trivia participants,
    /// so every departed player cost a round trip. Python filters against a
    /// single `guild.members` snapshot, and so does this.
    async fn member_directory(
        &self,
        guild_id: u64,
        signed_guild_id: Option<i64>,
    ) -> Result<GuildMemberDirectory, InteractionHandlerError> {
        let repository = self.leaderboard.clone();
        let discord = Arc::clone(&self.discord);
        let resolution = tokio::task::spawn_blocking(move || {
            let ids = repository
                .candidate_discord_ids(signed_guild_id)
                .map_err(|error| error.to_string())?;
            let stored_names = repository.registered_usernames(signed_guild_id)?;
            let requested_ids = ids
                .iter()
                .filter_map(|discord_id| u64::try_from(*discord_id).ok())
                .collect::<Vec<_>>();
            Ok::<_, String>(
                match discord.cached_guild_member_server_nicknames(guild_id, &requested_ids)? {
                    // A candidate absent from the snapshot has left the guild
                    // and is simply not listed.
                    Some(nicknames) => {
                        let names = GuildPlayerNameDirectory::new(Some(nicknames));
                        MemberResolution::Snapshot(GuildMemberDirectory::new(
                            ids.into_iter()
                                .filter(|discord_id| names.contains(*discord_id))
                                .map(|discord_id| {
                                    let stored_name = stored_names
                                        .get(&discord_id)
                                        .cloned()
                                        .unwrap_or_else(|| format!("User {discord_id}"));
                                    let name = names.resolve(discord_id, &stored_name).to_owned();
                                    (discord_id, name)
                                }),
                        ))
                    }
                    None => MemberResolution::Uncached(
                        ids.into_iter()
                            .map(|discord_id| {
                                let stored_name = stored_names
                                    .get(&discord_id)
                                    .cloned()
                                    .unwrap_or_else(|| format!("User {discord_id}"));
                                (discord_id, stored_name)
                            })
                            .collect(),
                    ),
                },
            )
        })
        .await
        .map_err(|error| {
            InteractionHandlerError::from(format!("leaderboard member-id task failed: {error}"))
        })?
        .map_err(InteractionHandlerError::from)?;
        match resolution {
            MemberResolution::Snapshot(directory) => Ok(directory),
            MemberResolution::Uncached(ids) => {
                Ok(self.member_directory_by_lookup(guild_id, ids).await)
            }
        }
    }

    /// Fallback for transports without a member cache: resolve each candidate
    /// on its own, preserving the pre-snapshot behavior exactly.
    async fn member_directory_by_lookup(
        &self,
        guild_id: u64,
        candidates: Vec<(i64, String)>,
    ) -> GuildMemberDirectory {
        let mut members = Vec::with_capacity(candidates.len());
        for (discord_id, stored_name) in candidates {
            let Ok(user_id) = u64::try_from(discord_id) else {
                continue;
            };
            match self.discord.guild_member(guild_id, user_id).await {
                Ok(Some(_member)) => members.push((discord_id, stored_name)),
                Ok(None) => {}
                Err(error) => {
                    // A candidate that cannot be resolved is absent, not fatal
                    // to every other leaderboard entry.
                    warn!(%error, guild_id, user_id, "unable to resolve leaderboard member");
                }
            }
        }
        GuildMemberDirectory::new(members)
    }

    async fn handle_leaderboard_component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Component {
            custom_id, user_id, ..
        } = request
        else {
            return Err("leaderboard component handler received a command".into());
        };
        let (view_id, action) = parse_component_id(&custom_id)?;
        let session = self
            .views
            .lock()
            .map_err(|_| InteractionHandlerError::from("leaderboard registry lock poisoned"))?
            .get(&view_id)
            .cloned();
        let Some(session) = session else {
            return responder
                .respond(
                    InteractionResponse::message(
                        "This leaderboard has expired. Run `/leaderboard` again.",
                    )
                    .ephemeral(),
                )
                .await
                .map_err(response_error);
        };
        // The view is in use, so its deletion timer restarts from now -- before
        // any await, so a timer that would otherwise fire during this handler
        // does not delete a leaderboard the owner is actively using.
        self.extend_expiry(view_id);

        match action {
            LeaderboardAction::Tab(tab) => {
                let now = Instant::now();
                let throttled = {
                    let mut clicks = session.last_tab_click.lock().map_err(|_| {
                        InteractionHandlerError::from("leaderboard click lock poisoned")
                    })?;
                    let throttled = clicks
                        .get(&user_id)
                        .is_some_and(|last| now.duration_since(*last) < TAB_CLICK_WINDOW);
                    if !throttled {
                        clicks.insert(user_id, now);
                    }
                    throttled
                };
                if throttled {
                    return responder.defer(false).await.map_err(response_error);
                }
                let needs_load = session
                    .view
                    .lock()
                    .map_err(|_| InteractionHandlerError::from("leaderboard view lock poisoned"))?
                    .tab_state(tab)
                    .loaded;
                let needs_load = !needs_load;
                if needs_load && let Err(error) = responder.defer(false).await {
                    warn!(%error, "unable to defer leaderboard tab interaction");
                    return Ok(());
                }
                let view = Arc::clone(&session.view);
                let response = tokio::task::spawn_blocking(move || {
                    let mut view = view
                        .lock()
                        .map_err(|_| "leaderboard view lock poisoned".to_owned())?;
                    view.switch_tab(tab);
                    view.load_tab(tab).map_err(|error| error.to_string())?;
                    Ok::<_, String>(leaderboard_response(view_id, &view))
                })
                .await
                .map_err(|error| {
                    InteractionHandlerError::from(format!("leaderboard tab task failed: {error}"))
                })?
                .map_err(InteractionHandlerError::from)?;
                if needs_load {
                    responder.edit_original(response).await
                } else {
                    responder.update(response).await
                }
                .map_err(response_error)
            }
            LeaderboardAction::Previous | LeaderboardAction::Next => {
                let response = {
                    let mut view = session.view.lock().map_err(|_| {
                        InteractionHandlerError::from("leaderboard view lock poisoned")
                    })?;
                    let changed = match action {
                        LeaderboardAction::Previous => view.previous_page(),
                        LeaderboardAction::Next => view.next_page(),
                        LeaderboardAction::Tab(_) => unreachable!(),
                    };
                    changed.then(|| leaderboard_response(view_id, &view))
                };
                match response {
                    Some(response) => responder.update(response).await.map_err(response_error),
                    None => responder.defer(false).await.map_err(response_error),
                }
            }
        }
    }

    async fn handle_calibration(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            user_id,
            user_display_name: _,
            guild_id,
            options,
            ..
        } = request
        else {
            return Err("calibration handler received a component".into());
        };
        let retry_after = {
            self.rate_limits
                .lock()
                .map_err(|_| InteractionHandlerError::from("info rate-limit lock was poisoned"))?
                .retry_after("calibration", guild_id, user_id, 2, Duration::from_secs(30))
        };
        if let Some(retry_after) = retry_after {
            return responder
                .respond(
                    InteractionResponse::message(format!(
                        "⏳ Please wait {retry_after}s before using `/calibration` again."
                    ))
                    .ephemeral(),
                )
                .await
                .map_err(response_error);
        }
        if let Err(error) = responder.defer(true).await {
            warn!(%error, "unable to defer /calibration interaction");
            return Ok(());
        }
        let (target_id, selected_user) = user_option(&options)
            .map(|id| (id, true))
            .unwrap_or((user_id, false));
        let sources = self.calibration.clone();
        let signed_guild = signed_optional_id(guild_id, "guild")?;
        let target_name = self
            .player_render_name(guild_id, signed_guild, target_id)
            .await?;
        let target = (target_id, Some(target_name), selected_user);
        let response = tokio::task::spawn_blocking(move || {
            calibration_response(sources, target, signed_guild)
        })
        .await
        .unwrap_or_else(|error| Err(format!("calibration task failed: {error}")));
        match response {
            Ok(response) => responder.followup(response).await.map_err(response_error),
            Err(error) => {
                warn!(%error, "unable to render /calibration");
                responder
                    .followup(
                        InteractionResponse::message(
                            "❌ Couldn't render the calibration chart right now — try again in a moment.",
                        )
                        .ephemeral()
                        .without_mentions(),
                    )
                    .await
                    .map_err(response_error)
            }
        }
    }

    async fn player_render_name(
        &self,
        guild_id: Option<u64>,
        signed_guild_id: Option<i64>,
        user_id: u64,
    ) -> Result<String, InteractionHandlerError> {
        let repository = self.leaderboard.clone();
        let discord = Arc::clone(&self.discord);
        tokio::task::spawn_blocking(move || -> Result<String, String> {
            let signed_user_id = i64::try_from(user_id)
                .map_err(|_| "target user id exceeds SQLite range".to_owned())?;
            let stored_name = repository
                .registered_usernames(signed_guild_id)?
                .remove(&signed_user_id)
                .unwrap_or_else(|| format!("User {user_id}"));
            let Some(guild_id) = guild_id else {
                return Ok(stored_name);
            };
            let names = GuildPlayerNameDirectory::new(
                discord.cached_guild_member_server_nicknames(guild_id, &[user_id])?,
            );
            Ok(names.resolve(signed_user_id, &stored_name).to_owned())
        })
        .await
        .map_err(|error| {
            InteractionHandlerError::from(format!("player-name task failed: {error}"))
        })?
        .map_err(InteractionHandlerError::from)
    }

    /// Delete the leaderboard `view_timeout` after the last interaction.
    fn schedule_expiry(
        &self,
        view_id: u64,
        expiry_generation: u64,
        responder: Arc<dyn InteractionResponder>,
    ) {
        let views = Arc::clone(&self.views);
        let timeout = self.view_timeout;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let session = views
                .lock()
                .ok()
                .and_then(|views| views.get(&view_id).cloned());
            let Some(session) = session else {
                return;
            };
            let receipt = match session.expiry.lock() {
                Ok(expiry) if expiry.generation == expiry_generation => expiry.receipt,
                // A newer interaction re-armed the timer, or the lock is
                // poisoned; either way this timer no longer owns the view.
                _ => return,
            };
            if let Ok(mut views) = views.lock()
                && views
                    .get(&view_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                views.remove(&view_id);
            }
            // Delete through the channel rather than the interaction followup:
            // the timer restarts on every interaction, so it can fire after the
            // interaction token's fifteen-minute life, and a token-scoped
            // delete would 404 and leave the message behind.
            if let Some(receipt) = receipt
                && let Err(error) = responder
                    .delete_message(InteractionMessageReceipt {
                        delivery: InteractionMessageDelivery::ChannelFallback,
                        ..receipt
                    })
                    .await
            {
                warn!(%error, view_id, "failed to delete expired leaderboard message");
            }
        });
    }

    /// Re-arm the expiry after an accepted component interaction.
    fn extend_expiry(&self, view_id: u64) {
        let armed = self
            .views
            .lock()
            .ok()
            .and_then(|views| views.get(&view_id).cloned())
            .and_then(|session| {
                session.expiry.lock().ok().and_then(|mut expiry| {
                    expiry.generation = expiry.generation.wrapping_add(1);
                    expiry.owner.clone().map(|owner| (expiry.generation, owner))
                })
            });
        if let Some((generation, owner)) = armed {
            self.schedule_expiry(view_id, generation, owner);
        }
    }
}

fn leaderboard_failure_response() -> InteractionResponse {
    InteractionResponse::message(
        "❌ Couldn't load the leaderboard right now — try again in a moment.",
    )
    .ephemeral()
    .without_mentions()
}

fn string_option(options: &[InteractionOption], name: &str) -> Option<String> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::String(value) => Some(value.clone()),
                _ => None,
            })
    })
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

fn user_option(options: &[InteractionOption]) -> Option<u64> {
    options.iter().find_map(|option| {
        if option.name != "user" {
            return None;
        }
        match &option.value {
            InteractionValue::User { id, .. } => Some(*id),
            _ => None,
        }
    })
}

fn signed_optional_id(
    value: Option<u64>,
    label: &str,
) -> Result<Option<i64>, InteractionHandlerError> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| {
                InteractionHandlerError::from(format!(
                    "Discord {label} ID exceeds SQLite's signed range"
                ))
            })
        })
        .transpose()
}

fn response_error(error: impl ToString) -> InteractionHandlerError {
    InteractionHandlerError::from(error.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LeaderboardAction {
    Tab(LeaderboardTab),
    Previous,
    Next,
}

fn parse_component_id(
    custom_id: &str,
) -> Result<(u64, LeaderboardAction), InteractionHandlerError> {
    let mut parts = custom_id.split(':');
    if parts.next() != Some("info") {
        return Err(format!("invalid info component ID {custom_id:?}").into());
    }
    let view_id = parts
        .next()
        .ok_or_else(|| InteractionHandlerError::from("missing leaderboard view ID"))?
        .parse::<u64>()
        .map_err(|_| InteractionHandlerError::from("invalid leaderboard view ID"))?;
    let action = match parts.next() {
        Some("tab") => LeaderboardAction::Tab(match parts.next() {
            Some("balance") => LeaderboardTab::Balance,
            Some("gambling") => LeaderboardTab::Gambling,
            Some("glicko") => LeaderboardTab::Glicko,
            Some("openskill") => LeaderboardTab::OpenSkill,
            Some("tips") => LeaderboardTab::Tips,
            Some("trivia") => LeaderboardTab::Trivia,
            _ => return Err("invalid leaderboard tab".into()),
        }),
        Some("previous") => LeaderboardAction::Previous,
        Some("next") => LeaderboardAction::Next,
        _ => return Err(format!("invalid leaderboard action in {custom_id:?}").into()),
    };
    if parts.next().is_some() {
        return Err(format!("invalid info component ID {custom_id:?}").into());
    }
    Ok((view_id, action))
}

fn leaderboard_response(
    view_id: u64,
    view: &UnifiedLeaderboardView<InfoLeaderboardRepository>,
) -> InteractionResponse {
    let embed = interaction_embed(view.build_embed(), view.current_tab());
    let mut response = InteractionResponse::message("")
        .embed(embed)
        .action_rows(leaderboard_action_rows(view_id, view));
    let mentions = leaderboard_mentions(view);
    response.allowed_mentions = if mentions.is_empty() {
        InteractionAllowedMentions::None
    } else {
        InteractionAllowedMentions::Users(mentions)
    };
    response
}

fn interaction_embed(embed: EmbedModel, tab: LeaderboardTab) -> InteractionEmbed {
    let color = match tab {
        LeaderboardTab::Balance | LeaderboardTab::Glicko | LeaderboardTab::OpenSkill => {
            DISCORD_GOLD
        }
        LeaderboardTab::Gambling => 0xFF_D7_00,
        LeaderboardTab::Tips => TIPS_PINK,
        LeaderboardTab::Trivia => TRIVIA_ORANGE,
    };
    let mut converted = embed
        .title
        .map_or_else(InteractionEmbed::default, InteractionEmbed::titled)
        .color(color);
    if let Some(description) = embed.description {
        converted = converted.description(description);
    }
    for field in embed.fields {
        converted = converted.field(field.name, field.value, field.inline);
    }
    if let Some(footer) = embed.footer {
        converted = converted.footer(footer);
    }
    converted
}

fn leaderboard_action_rows(
    view_id: u64,
    view: &UnifiedLeaderboardView<InfoLeaderboardRepository>,
) -> Vec<InteractionActionRow> {
    let style = |tab| match view.button_style(tab) {
        ButtonStyle::Primary => InteractionButtonStyle::Primary,
        ButtonStyle::Secondary => InteractionButtonStyle::Secondary,
    };
    let page = view.pagination_buttons();
    vec![
        InteractionActionRow::buttons(vec![
            tab_button(view_id, LeaderboardTab::Balance, "Balance", style),
            tab_button(view_id, LeaderboardTab::Gambling, "Gambling", style),
            tab_button(view_id, LeaderboardTab::Glicko, "Glicko", style),
            tab_button(view_id, LeaderboardTab::OpenSkill, "OpenSkill", style),
        ]),
        InteractionActionRow::buttons(vec![
            tab_button(view_id, LeaderboardTab::Tips, "Tips", style),
            tab_button(view_id, LeaderboardTab::Trivia, "Trivia", style),
            InteractionButton::new(format!("info:{view_id}:previous"), " Previous")
                .style(InteractionButtonStyle::Secondary)
                .disabled(page.previous_disabled),
            InteractionButton::new(format!("info:{view_id}:next"), "Next ")
                .style(InteractionButtonStyle::Secondary)
                .disabled(page.next_disabled),
        ]),
    ]
}

fn tab_button(
    view_id: u64,
    tab: LeaderboardTab,
    label: &str,
    style: impl Fn(LeaderboardTab) -> InteractionButtonStyle,
) -> InteractionButton {
    InteractionButton::new(format!("info:{view_id}:tab:{}", tab.as_str()), label).style(style(tab))
}

fn leaderboard_mentions(view: &UnifiedLeaderboardView<InfoLeaderboardRepository>) -> Vec<u64> {
    let state = view.tab_state(view.current_tab());
    let page_size = view.current_tab().page_size();
    let start = state.current_page.saturating_mul(page_size);
    let mut ids = BTreeSet::new();
    match state.data.as_ref() {
        Some(TabData::Balance(BalanceLeaderboard {
            players, debtors, ..
        })) => {
            ids.extend(
                players
                    .iter()
                    .skip(start)
                    .take(page_size)
                    .filter_map(|entry| u64::try_from(entry.discord_id).ok()),
            );
            if state.current_page == 0 {
                ids.extend(
                    debtors
                        .iter()
                        .take(10)
                        .filter_map(|entry| u64::try_from(entry.discord_id).ok()),
                );
            }
        }
        Some(TabData::Glicko(data) | TabData::OpenSkill(data)) => ids.extend(
            data.players
                .iter()
                .skip(start)
                .take(page_size)
                .filter_map(|entry| u64::try_from(entry.discord_id).ok()),
        ),
        _ => {}
    }
    ids.into_iter().collect()
}

fn help_embed(
    leverage_tiers: &[i64],
    pingedash_cost: i64,
    pingedkevin_cost: i64,
    is_admin: bool,
) -> InteractionEmbed {
    let leverage = leverage_tiers
        .iter()
        .map(|tier| format!("{tier}x"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut embed = InteractionEmbed::titled("📚 Cama Shuffle Bot Commands")
        .description("All available commands for the matchmaking bot")
        .color(DISCORD_BLUE)
        .field(
            "👤 Registration & Profile (`/player`)",
            "`/player register` - Register yourself as a player\n\
             `/player roles` - Set your preferred roles (1-5)\n\
             `/player link` / `unlink` / `steamids` - Manage Steam accounts\n\
             `/player lobby autonotify enabled:<true|false>` - Persistent public 8/9-player alerts\n\
             `/player lobby autonotify playername:<player>` - One DM on their next lobby join\n\
             `/player exclusion` - Check your exclusion factor\n\
             `/profile` - View unified profile\n\
             `/matchup` - Head-to-head comparison",
            false,
        )
        .field(
            "📊 Dota 2 Stats (OpenDota)",
            "`/matches history` - Recent matches with heroes and stats\n\
             `/matches view` - View detailed match embed\n\
             `/matches recent` - Recent matches as image table\n\
             *Use `/profile` Dota tab for role & lane graphs*",
            false,
        )
        .field(
            "📖 Dota 2 Reference (`/dota`)",
            "`/dota hero` - Look up hero stats, abilities, talents\n\
             `/dota ability` - Look up ability details",
            false,
        )
        .field(
            "🎮 Lobby Management",
            "`/lobby` - Create or view the matchmaking lobby\n\
             `/kick` - Kick a player (Admin or lobby creator only)\n\
             `/resetlobby` - Reset the current lobby (Admin or lobby creator only)\n\
             Use buttons in the thread to join/leave",
            false,
        )
        .field(
            "⚔️ Match Management",
            "`/shuffle` - Create balanced teams from lobby (pool betting)\n\
             `/record` - Record a match result\n\
             `/draft start` - Start Immortal Draft (captain mode)\n\
             `/draft restart` - Restart active draft",
            false,
        )
        .field(
            format!("🎰 Betting ({JOPACOIN_EMOTE} Jopacoin)"),
            format!(
                "`/bet` - Bet on Radiant or Dire (leverage: {leverage})\n  • Can place multiple bets on the same team\n  • Leverage can push you into debt\n  • Cannot bet while in debt\n`/mybets` - View your active bets and potential payout\n`/balance` - Check your jopacoin balance and debt\n`/economy tip` - Give jopacoin to another player\n`/economy paydebt` - Help another player pay off their debt\n`/economy loan` / `/economy bankruptcy` - Manage debt\n`/economy reserve` / `/economy disburse` - View or allocate Reserve funds\n`/shop buy` / `/shop mana` - Browse the jopacoin and mana shops\n`/shop avoids` / `/shop deals` - View active matchmaking purchases\n`/shop cancel-deal` - Cancel an inactive package deal after 30 days\n`/shop pingedash` - Spend {pingedash_cost} jopacoin to send the Pingedash\n`/shop pingedkevin` - Spend {pingedkevin_cost} jopacoin to send the PingedKevin"
            ),
            false,
        )
        .field(
            "🔮 Prediction Markets (`/predict`)",
            "`/predict create` - Create a new prediction market\n\
             `/predict list` - List active predictions\n\
             `/predict mine` - View your prediction positions\n\
             `/predict resolve` - Vote to resolve a prediction\n\
             `/predict close` / `cancel` - Admin: close or cancel",
            false,
        )
        .field(
            "🏆 Leaderboard",
            "`/leaderboard` - View leaderboard (default: balance)\n\
             `/leaderboard type:glicko` - Glicko-2 rating rankings\n\
             `/leaderboard type:openskill` - OpenSkill (fantasy-weighted) rankings\n\
             `/leaderboard type:gambling` - Gambling rankings & Hall of Degen\n\
             `/leaderboard type:tips` - Tipping rankings (generous/popular)\n\
             `/leaderboard type:trivia` - Trivia best streaks (7-day)\n\
             `/calibration` - Rating system health & calibration stats",
            false,
        );
    if is_admin {
        embed = embed.field(
            "🔧 Admin Commands",
            "`/admin addfake` - Add fake users to lobby for testing\n\
             `/admin sync` - Force sync commands\n\
             `/admin givecoin` - Give jopacoin to a user\n\
             `/admin setrating` - Set initial rating for a player\n\
             `/enrich match` - Enrich match with OpenDota data\n\
             `/enrich discover` - Auto-discover Dota match IDs\n\
             `/enrich config` - View server configuration\n\
             `/enrich rebuildpairings` - Rebuild pairwise stats",
            false,
        );
    }
    embed.footer("Tip: Type / and use Discord's autocomplete to see command details!")
}

#[derive(Clone)]
struct InfoLeaderboardRepository {
    path: PathBuf,
    clock: Arc<dyn InfoClock>,
}

trait InfoClock: Send + Sync {
    fn unix_timestamp(&self) -> i64;
}

struct SystemInfoClock;

impl InfoClock for SystemInfoClock {
    fn unix_timestamp(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs() as i64)
    }
}

impl InfoLeaderboardRepository {
    fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            clock: Arc::new(SystemInfoClock),
        }
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

    fn candidate_discord_ids(&self, guild_id: Option<i64>) -> Result<Vec<i64>, String> {
        let guild_id = PlayerRepository::normalize_guild_id(guild_id);
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT discord_id FROM players WHERE guild_id = ?1
                 UNION SELECT discord_id FROM bets WHERE guild_id = ?1
                 UNION SELECT sender_id FROM tip_transactions WHERE guild_id = ?1
                 UNION SELECT recipient_id FROM tip_transactions WHERE guild_id = ?1
                 UNION SELECT discord_id FROM trivia_sessions WHERE guild_id = ?1
                 ORDER BY 1",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map([guild_id], |row| row.get(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    fn registered_usernames(&self, guild_id: Option<i64>) -> Result<BTreeMap<i64, String>, String> {
        PlayerRepository::new(&self.path)
            .get_all(guild_id)
            .map_err(|error| error.to_string())
            .map(|players| {
                players
                    .into_iter()
                    .filter_map(|player| Some((player.discord_id?, player.name)))
                    .collect()
            })
    }
}

impl UnifiedLeaderboardRepositoryPort for InfoLeaderboardRepository {
    type Error = String;

    fn balance_candidates(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<cama_domain::player::Player>, Self::Error> {
        PlayerRepository::new(&self.path)
            .get_leaderboard(guild_id, sqlite_limit(limit), 0)
            .map_err(|error| error.to_string())
    }

    fn player_count(&mut self, guild_id: Option<i64>) -> Result<usize, Self::Error> {
        PlayerRepository::new(&self.path)
            .get_all(guild_id)
            .map(|players| players.len())
            .map_err(|error| error.to_string())
    }

    fn negative_balances(
        &mut self,
        guild_id: Option<i64>,
    ) -> Result<Vec<NegativeBalanceEntry>, Self::Error> {
        let mut entries = PlayerRepository::new(&self.path)
            .get_all(guild_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|player| player.jopacoin_balance < 0)
            .filter_map(|player| {
                Some(NegativeBalanceEntry {
                    discord_id: player.discord_id?,
                    guild_id: player.guild_id,
                    username: player.name,
                    balance: player.jopacoin_balance,
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| (entry.balance, entry.discord_id));
        Ok(entries)
    }

    fn gambling_snapshot(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Option<GamblingSnapshot>, Self::Error> {
        let leaderboard = GamblingStatsService::new(GamblingStatsRepository::new(&self.path))
            .get_leaderboard(guild_id, limit, 3)
            .map_err(|error| error.to_string())?;
        let mut entries = BTreeMap::new();
        for entry in leaderboard
            .top_earners
            .iter()
            .chain(&leaderboard.down_bad)
            .chain(&leaderboard.hall_of_degen)
            .chain(&leaderboard.biggest_gamblers)
        {
            entries
                .entry(entry.discord_id)
                .or_insert_with(|| GamblingEntry {
                    discord_id: entry.discord_id,
                    guild_id,
                    total_bets: entry.total_bets,
                    wins: entry.wins,
                    losses: entry.losses,
                    win_rate: entry.win_rate,
                    net_pnl: entry.net_pnl,
                    total_wagered: entry.total_wagered,
                    avg_leverage: entry.avg_leverage,
                    degen_score: Some(entry.degen_score),
                    degen_title: Some(entry.degen_title.to_owned()),
                    degen_emoji: Some(entry.degen_emoji.to_owned()),
                });
        }
        Ok(Some(GamblingSnapshot {
            entries: entries.into_values().collect(),
            server_stats: Some(GamblingServerStats {
                total_bets: leaderboard.server_stats.total_bets,
                total_wagered: leaderboard.server_stats.total_wagered,
                unique_gamblers: i64::try_from(leaderboard.server_stats.unique_gamblers)
                    .unwrap_or(i64::MAX),
                total_bankruptcies: leaderboard.server_stats.total_bankruptcies,
            }),
        }))
    }

    fn bankruptcy_states(
        &mut self,
        guild_id: Option<i64>,
        discord_ids: &[i64],
    ) -> Result<BTreeMap<i64, BankruptcyState>, Self::Error> {
        BankruptcyRepository::new(&self.path)
            .get_bulk_states(discord_ids, guild_id)
            .map(|states| {
                states
                    .into_iter()
                    .map(|(id, state)| {
                        (
                            id,
                            BankruptcyState {
                                penalty_games_remaining: state.penalty_games_remaining,
                            },
                        )
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())
    }

    fn rating_candidates(
        &mut self,
        guild_id: Option<i64>,
        kind: RatingKind,
        limit: usize,
    ) -> Result<Vec<cama_domain::player::Player>, Self::Error> {
        let players = PlayerRepository::new(&self.path);
        match kind {
            RatingKind::Glicko => {
                players.get_leaderboard_by_glicko(guild_id, sqlite_limit(limit), 0, 0)
            }
            RatingKind::OpenSkill => {
                players.get_leaderboard_by_openskill(guild_id, sqlite_limit(limit), 0, 0)
            }
        }
        .map_err(|error| error.to_string())
    }

    fn rated_player_count(
        &mut self,
        guild_id: Option<i64>,
        kind: RatingKind,
    ) -> Result<usize, Self::Error> {
        PlayerRepository::new(&self.path)
            .get_all(guild_id)
            .map(|players| {
                players
                    .iter()
                    .filter(|player| match kind {
                        RatingKind::Glicko => player.glicko_rating.is_some(),
                        RatingKind::OpenSkill => player.os_mu.is_some(),
                    })
                    .count()
            })
            .map_err(|error| error.to_string())
    }

    fn tips_snapshot(
        &mut self,
        guild_id: Option<i64>,
        limit: usize,
    ) -> Result<Option<TipsSnapshot>, Self::Error> {
        let tips = TipRepository::new(&self.path);
        let top_senders = tips
            .get_top_senders(guild_id, sqlite_limit(limit))
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|entry| ScopedTipLeaderboardEntry {
                guild_id,
                entry: TipLeaderboardEntry {
                    discord_id: entry.discord_id,
                    total_amount: entry.total_amount,
                    tip_count: entry.tip_count,
                },
            })
            .collect();
        let top_receivers = tips
            .get_top_receivers(guild_id, sqlite_limit(limit))
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|entry| ScopedTipLeaderboardEntry {
                guild_id,
                entry: TipLeaderboardEntry {
                    discord_id: entry.discord_id,
                    total_amount: entry.total_amount,
                    tip_count: entry.tip_count,
                },
            })
            .collect();
        let total_volume = tips
            .get_total_tip_volume(guild_id)
            .map_err(|error| error.to_string())?;
        Ok(Some(TipsSnapshot {
            top_senders,
            top_receivers,
            total_volume: TipVolume {
                total_amount: total_volume.total_amount,
                total_fees: total_volume.total_fees,
                total_transactions: total_volume.total_transactions,
            },
        }))
    }

    fn trivia_candidates(
        &mut self,
        guild_id: Option<i64>,
        days: i64,
        limit: usize,
    ) -> Result<Vec<TriviaEntry>, Self::Error> {
        let since = self
            .clock
            .unix_timestamp()
            .saturating_sub(days.saturating_mul(86_400));
        TriviaCommandsRepository::new(&self.path)
            .get_trivia_leaderboard(guild_id, since, limit)
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| TriviaEntry {
                        discord_id: entry.discord_id,
                        guild_id,
                        best_streak: entry.best_streak,
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())
    }
}

fn sqlite_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

trait CommandOptionChoices {
    fn choices(self, choices: Vec<CommandOptionChoice>) -> Self;
}

impl CommandOptionChoices for CommandOptionSpec {
    fn choices(mut self, choices: Vec<CommandOptionChoice>) -> Self {
        self.choices = choices;
        self
    }
}

#[cfg(all(test, feature = "runtime-test-match"))]
#[path = "info_provider/tests.rs"]
mod tests;
