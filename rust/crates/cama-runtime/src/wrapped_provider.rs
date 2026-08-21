//! Complete live `/wrapped` Discord command provider.
//!
//! The provider reads the Rust-migrated SQLite database, renders every story
//! page as a native PNG, preserves the Python command's public defer/follow-up
//! contract, and owns a ten-minute invoker-scoped component session. Sessions
//! are intentionally process-local like discord.py's non-persistent `View`;
//! after restart or expiry, stale buttons receive an explicit private error.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::drawing::{GambaInfo, GambaPoint, GambaStats as NativeGambaStats};
use cama_app::wrapped_media::{
    WrappedAvatar, WrappedGambaData, WrappedSlideData, render_wrapped_slide,
};
use cama_app::wrapped_story::{
    HeroRow, MatchDetails, MatchStatsRow, PairwiseEntry, PairwiseInput, PairwiseRaw,
    PersonalSummaryInput, PersonalSummaryWrapped, YearMatchRow, flavor_pool, get_flavor,
    hero_spotlight, package_deal_wrapped, pairwise_player_ids, pairwise_wrapped, personal_summary,
    role_breakdown, year_timestamps,
};
use cama_db::gambling_stats_repository::{
    GamblingOutcome, GamblingSource, GamblingStatsRepository, GamblingStatsService,
};
use cama_db::package_deal_repository::PackageDealRepository;
use cama_db::pairings_repository::PairingsRepository;
use cama_db::wrapped_live::{
    WrappedBankruptcy, WrappedBetsAgainst, WrappedBettingStats, WrappedHeroStats,
    WrappedLiveRepository, WrappedPlayerHeroStats, WrappedPlayerStats, WrappedRatingChange,
    WrappedRatingPoint, WrappedServerSummary, WrappedYearMatch,
};
use chrono::{Datelike, Utc};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tracing::warn;

use crate::application_config::ApplicationConfig;
use crate::registration::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute, InteractionActionRow,
    InteractionAttachment, InteractionButton, InteractionButtonStyle, InteractionHandler,
    InteractionHandlerError, InteractionMessageReceipt, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionValue, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};

const COMPONENT_PREFIX: &str = "wrapped:";
const STORY_TIMEOUT: Duration = Duration::from_secs(600);
const COMMAND_COOLDOWN: Duration = Duration::from_secs(60);
const PROFILE_CONCURRENCY: usize = 5;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WrappedDiscordProfile {
    pub display_name: String,
    /// The Python command reads only a member's custom avatar, not the default
    /// avatar. Production adapters return a bounded PNG or `None` fail-soft.
    pub avatar_png: Option<Vec<u8>>,
}

/// Narrow live Discord seam used for Wrapped's bounded name/avatar prefetch.
#[async_trait]
pub trait WrappedDiscordPort: Send + Sync {
    async fn wrapped_profile(
        &self,
        guild_id: u64,
        user_id: u64,
        include_avatar: bool,
    ) -> Result<Option<WrappedDiscordProfile>, String>;
}

#[derive(Clone)]
pub struct WrappedRegistrationProvider {
    handler: Arc<WrappedHandler>,
}

impl WrappedRegistrationProvider {
    /// Build the production provider from immutable configuration, migrated
    /// SQLite state, and the live Discord profile adapter.
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn WrappedDiscordPort>,
    ) -> Self {
        Self::with_timeout_and_year(database_path, config, discord, STORY_TIMEOUT, None)
    }

    fn with_timeout_and_year(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn WrappedDiscordPort>,
        timeout: Duration,
        fixed_year: Option<i32>,
    ) -> Self {
        let path = database_path.as_ref();
        Self {
            handler: Arc::new(WrappedHandler {
                sources: WrappedSources::new(path),
                discord,
                minimum_games: config.values.wrapped_min_games.max(0) as usize,
                minimum_bets: config.values.wrapped_min_bets.max(0),
                cooldowns: Mutex::new(HashMap::new()),
                sessions: Arc::new(Mutex::new(BTreeMap::new())),
                next_session: AtomicU64::new(fastrand::u64(1..u64::MAX)),
                timeout,
                fixed_year,
            }),
        }
    }
}

impl RegistrationProvider for WrappedRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "wrapped".to_owned(),
            description: "View your Cama Wrapped year in review".to_owned(),
            options: vec![CommandOptionSpec::new(
                "user",
                "View another user's wrapped",
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

struct WrappedHandler {
    sources: WrappedSources,
    discord: Arc<dyn WrappedDiscordPort>,
    minimum_games: usize,
    minimum_bets: i64,
    cooldowns: Mutex<HashMap<u64, Instant>>,
    sessions: Arc<Mutex<BTreeMap<u64, Arc<AsyncMutex<StorySession>>>>>,
    next_session: AtomicU64,
    timeout: Duration,
    fixed_year: Option<i32>,
}

struct StorySession {
    owner_id: u64,
    slides: Vec<WrappedSlideData>,
    rendered: BTreeMap<usize, Vec<u8>>,
    current: usize,
    expiry_generation: u64,
    receipt: Option<InteractionMessageReceipt>,
    active: bool,
}

#[derive(Clone)]
struct WrappedSources {
    wrapped: WrappedLiveRepository,
    pairings: PairingsRepository,
    packages: PackageDealRepository,
    gambling: GamblingStatsService<GamblingStatsRepository>,
}

impl WrappedSources {
    fn new(path: &Path) -> Self {
        Self {
            wrapped: WrappedLiveRepository::new(path),
            pairings: PairingsRepository::new(path),
            packages: PackageDealRepository::new(path),
            gambling: GamblingStatsService::new(GamblingStatsRepository::new(path)),
        }
    }

    fn load(
        &self,
        guild_id: i64,
        target_id: i64,
        year: i32,
    ) -> Result<Option<WrappedRawData>, String> {
        let (start_ts, end_ts) = year_timestamps(year);
        let summary = self
            .wrapped
            .server_summary(guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        if summary.total_matches == 0 {
            return Ok(None);
        }
        let player_stats = self
            .wrapped
            .player_stats(guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        let hero_stats = self
            .wrapped
            .hero_stats(guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        let player_heroes = self
            .wrapped
            .player_hero_stats(guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        let rating_changes = self
            .wrapped
            .rating_changes(guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        let betting = self
            .wrapped
            .betting_stats(guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        let bets_against = self
            .wrapped
            .bets_against(guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        let bankruptcies = self
            .wrapped
            .bankruptcies(guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        let player_name = self
            .wrapped
            .player_name(target_id, guild_id)
            .map_err(|error| error.to_string())?;
        let matches = self
            .wrapped
            .player_year_matches(target_id, guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;

        let pairwise = PairwiseInput {
            best_teammates: teammate_rows(
                self.pairings
                    .get_best_teammates(target_id, Some(guild_id), 3, 3)
                    .map_err(|error| error.to_string())?,
            ),
            most_played_with: teammate_rows(
                self.pairings
                    .get_most_played_with(target_id, Some(guild_id), 3, 3)
                    .map_err(|error| error.to_string())?,
            ),
            worst_matchups: opponent_rows(
                self.pairings
                    .get_worst_matchups(target_id, Some(guild_id), 3, 1)
                    .map_err(|error| error.to_string())?,
            ),
            best_matchups: opponent_rows(
                self.pairings
                    .get_best_matchups(target_id, Some(guild_id), 3, 1)
                    .map_err(|error| error.to_string())?,
            ),
            most_played_against: opponent_rows(
                self.pairings
                    .get_most_played_against(target_id, Some(guild_id), 3, 3)
                    .map_err(|error| error.to_string())?,
            ),
        };
        let pairwise_ids = pairwise_player_ids(&pairwise);
        let pairwise_names = pairwise_ids
            .iter()
            .map(|id| {
                self.wrapped
                    .player_name(*id, guild_id)
                    .map(|name| (*id, name.unwrap_or_else(|| id.to_string())))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let package_purchases = self
            .packages
            .get_purchases_involving_player(Some(guild_id), target_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        let rating_history = self
            .wrapped
            .rating_history(target_id, guild_id, 50)
            .unwrap_or_else(|error| {
                warn!(%error, target_id, guild_id, "failed to fetch Wrapped rating history");
                Vec::new()
            });
        let gamba_stats = self
            .gambling
            .get_player_stats(target_id, Some(guild_id))
            .map_err(|error| error.to_string())?;
        let gamba_points = self
            .gambling
            .cumulative_pnl_series(target_id, Some(guild_id))
            .map_err(|error| error.to_string())?;

        Ok(Some(WrappedRawData {
            summary,
            player_stats,
            hero_stats,
            player_heroes,
            rating_changes,
            betting,
            bets_against,
            bankruptcies,
            player_name,
            matches,
            pairwise,
            pairwise_names,
            package_purchases,
            rating_history,
            gamba_stats,
            gamba_points: gamba_points.into_iter().map(wrapped_gamba_point).collect(),
        }))
    }
}

fn wrapped_gamba_point(point: cama_db::gambling_stats_repository::PnlPoint) -> GambaPoint {
    let source = match point.event.source {
        GamblingSource::Bet => "bet",
        GamblingSource::Wheel => "wheel",
        GamblingSource::DoubleOrNothing => "double_or_nothing",
    };
    let outcome = match point.event.outcome {
        GamblingOutcome::Won => "won",
        GamblingOutcome::Lost => "lost",
        GamblingOutcome::Neutral => "neutral",
    };
    GambaPoint {
        event_number: i32::try_from(point.event_number).unwrap_or(i32::MAX),
        cumulative: point.cumulative_pnl,
        info: GambaInfo {
            source: source.to_owned(),
            outcome: Some(outcome.to_owned()),
            leverage: point.event.leverage,
            profit: point.event.profit,
        },
    }
}

fn wrapped_gamba_stats(stats: &cama_db::gambling_stats_repository::GambaStats) -> NativeGambaStats {
    NativeGambaStats {
        total_bets: usize::try_from(stats.total_bets.max(0)).unwrap_or(usize::MAX),
        win_rate: stats.win_rate,
        net_pnl: stats.net_pnl,
        roi: stats.roi,
    }
}

fn teammate_rows(rows: Vec<cama_db::pairings_repository::TeammatePairing>) -> Vec<PairwiseRaw> {
    rows.into_iter()
        .map(|row| PairwiseRaw {
            peer_id: row.teammate_id,
            games: row.games_together,
            wins: row.wins_together,
            win_rate: row.win_rate,
        })
        .collect()
}

fn opponent_rows(rows: Vec<cama_db::pairings_repository::OpponentPairing>) -> Vec<PairwiseRaw> {
    rows.into_iter()
        .map(|row| PairwiseRaw {
            peer_id: row.opponent_id,
            games: row.games_against,
            wins: row.wins_against,
            win_rate: row.win_rate,
        })
        .collect()
}

struct WrappedRawData {
    summary: WrappedServerSummary,
    player_stats: Vec<WrappedPlayerStats>,
    hero_stats: Vec<WrappedHeroStats>,
    player_heroes: Vec<WrappedPlayerHeroStats>,
    rating_changes: Vec<WrappedRatingChange>,
    betting: Vec<WrappedBettingStats>,
    bets_against: Vec<WrappedBetsAgainst>,
    bankruptcies: Vec<WrappedBankruptcy>,
    player_name: Option<String>,
    matches: Vec<WrappedYearMatch>,
    pairwise: PairwiseInput,
    pairwise_names: BTreeMap<i64, String>,
    package_purchases: Vec<cama_db::package_deal_repository::PackageDealPurchase>,
    rating_history: Vec<WrappedRatingPoint>,
    gamba_stats: Option<cama_db::gambling_stats_repository::GambaStats>,
    gamba_points: Vec<GambaPoint>,
}

#[async_trait]
impl InteractionHandler for WrappedHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command { ref name, .. } if name == "wrapped" => {
                self.command(request, responder).await
            }
            InteractionRequest::Component { .. } => self.component(request, responder).await,
            InteractionRequest::Command { name, .. } => {
                Err(format!("wrapped handler received command {name:?}").into())
            }
            InteractionRequest::Autocomplete { .. } => {
                Err("wrapped has no autocomplete route".into())
            }
            InteractionRequest::Modal { .. } => Err("wrapped has no modal route".into()),
        }
    }
}

impl WrappedHandler {
    async fn command(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            user_id,
            user_display_name,
            guild_id,
            options,
            ..
        } = request
        else {
            return Err("wrapped command received non-command request".into());
        };
        if self.on_cooldown(user_id)? {
            return responder
                .respond(
                    InteractionResponse::message(
                        "❌ An error occurred while processing your command. Please try again.",
                    )
                    .ephemeral()
                    .without_mentions(),
                )
                .await
                .map_err(response_error);
        }
        let Some(guild_id) = guild_id else {
            return responder
                .respond(
                    InteractionResponse::message("This command can only be used in a server.")
                        .ephemeral(),
                )
                .await
                .map_err(response_error);
        };
        if let Err(error) = responder.defer(false).await {
            warn!(%error, "unable to defer /wrapped interaction");
            return Ok(());
        }
        let (target_id, target_name) =
            user_option(&options).unwrap_or((user_id, user_display_name));
        let (signed_guild, signed_target) =
            (signed_id(guild_id, "guild")?, signed_id(target_id, "user")?);
        let year = self.fixed_year.unwrap_or_else(|| Utc::now().year());
        let sources = self.sources.clone();
        let raw = match tokio::task::spawn_blocking(move || {
            sources.load(signed_guild, signed_target, year)
        })
        .await
        {
            Ok(Ok(raw)) => raw,
            Ok(Err(error)) => {
                warn!(%error, "wrapped SQLite aggregation failed");
                return wrapped_failure(&responder).await;
            }
            Err(error) => {
                warn!(%error, "wrapped aggregation task failed");
                return wrapped_failure(&responder).await;
            }
        };
        let Some(raw) = raw else {
            return responder
                .followup(
                    InteractionResponse::message(format!("No match data found for {year}."))
                        .ephemeral(),
                )
                .await
                .map_err(response_error);
        };

        let (profile_ids, avatar_ids) = prefetch_ids(&raw, self.minimum_games, self.minimum_bets);
        let profiles = self
            .prefetch_profiles(guild_id, profile_ids, avatar_ids)
            .await;
        let minimum_games = self.minimum_games;
        let minimum_bets = self.minimum_bets;
        let slide_target_name = target_name.clone();
        let slides = match tokio::task::spawn_blocking(move || {
            build_slides(
                raw,
                &profiles,
                signed_target,
                &slide_target_name,
                year,
                minimum_games,
                minimum_bets,
            )
        })
        .await
        {
            Ok(slides) => slides,
            Err(error) => {
                warn!(%error, "wrapped slide assembly task failed");
                return wrapped_failure(&responder).await;
            }
        };
        if slides.is_empty() {
            return responder
                .followup(
                    InteractionResponse::message(format!("No wrapped data available for {year}."))
                        .ephemeral(),
                )
                .await
                .map_err(response_error);
        }
        let first_slide = slides[0].clone();
        let first =
            match tokio::task::spawn_blocking(move || render_wrapped_slide(&first_slide)).await {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(error)) => {
                    warn!(%error, "wrapped first slide render failed");
                    return wrapped_failure(&responder).await;
                }
                Err(error) => {
                    warn!(%error, "wrapped first slide task failed");
                    return wrapped_failure(&responder).await;
                }
            };
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let slide_count = slides.len();
        let session = Arc::new(AsyncMutex::new(StorySession {
            owner_id: user_id,
            slides,
            rendered: BTreeMap::from([(0, first.clone())]),
            current: 0,
            expiry_generation: 0,
            receipt: None,
            active: true,
        }));
        self.sessions
            .lock()
            .map_err(|_| InteractionHandlerError::from("wrapped session lock poisoned"))?
            .insert(session_id, Arc::clone(&session));
        let response = story_response(
            format!("**Cama Wrapped {year}** for {target_name}"),
            first,
            session_id,
            0,
            slide_count,
        );
        let receipt = match responder.followup_with_receipt(response).await {
            Ok(receipt) => receipt,
            Err(error) => {
                self.sessions
                    .lock()
                    .map_err(|_| InteractionHandlerError::from("wrapped session lock poisoned"))?
                    .remove(&session_id);
                return Err(response_error(error));
            }
        };
        session.lock().await.receipt = receipt;
        self.schedule_expiry(session_id, 0, responder);
        Ok(())
    }

    async fn component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Component {
            custom_id, user_id, ..
        } = request
        else {
            return Err("wrapped component received non-component request".into());
        };
        let (session_id, direction) = parse_component(&custom_id)?;
        let session = self
            .sessions
            .lock()
            .map_err(|_| InteractionHandlerError::from("wrapped session lock poisoned"))?
            .get(&session_id)
            .cloned();
        let Some(session) = session else {
            return responder
                .respond(
                    InteractionResponse::message("This wrapped has expired.")
                        .ephemeral()
                        .without_mentions(),
                )
                .await
                .map_err(response_error);
        };
        let mut session = session.lock().await;
        if !session.active {
            return responder
                .respond(
                    InteractionResponse::message("This wrapped has expired.")
                        .ephemeral()
                        .without_mentions(),
                )
                .await
                .map_err(response_error);
        }
        if session.owner_id != user_id {
            return responder
                .respond(
                    InteractionResponse::message(
                        "Only the command invoker can navigate this wrapped.",
                    )
                    .ephemeral(),
                )
                .await
                .map_err(response_error);
        }
        session.expiry_generation = session.expiry_generation.wrapping_add(1);
        let expiry_generation = session.expiry_generation;
        self.schedule_expiry(session_id, expiry_generation, Arc::clone(&responder));
        let target = match direction {
            StoryDirection::Previous => session.current.checked_sub(1),
            StoryDirection::Next => session
                .current
                .checked_add(1)
                .filter(|index| *index < session.slides.len()),
        };
        let Some(target) = target else {
            return responder.defer(false).await.map_err(response_error);
        };
        let bytes = if let Some(bytes) = session.rendered.get(&target) {
            bytes.clone()
        } else {
            let slide = session.slides[target].clone();
            match tokio::task::spawn_blocking(move || render_wrapped_slide(&slide)).await {
                Ok(Ok(bytes)) => {
                    session.rendered.insert(target, bytes.clone());
                    bytes
                }
                Ok(Err(error)) => {
                    warn!(%error, session_id, target, "wrapped slide render failed");
                    return responder
                        .respond(
                            InteractionResponse::message("Failed to render this slide.")
                                .ephemeral(),
                        )
                        .await
                        .map_err(response_error);
                }
                Err(error) => {
                    warn!(%error, session_id, target, "wrapped slide task failed");
                    return responder
                        .respond(
                            InteractionResponse::message("Failed to render this slide.")
                                .ephemeral(),
                        )
                        .await
                        .map_err(response_error);
                }
            }
        };
        session.current = target;
        let count = session.slides.len();
        drop(session);
        responder
            .update(story_response(
                String::new(),
                bytes,
                session_id,
                target,
                count,
            ))
            .await
            .map_err(response_error)
    }

    fn on_cooldown(&self, user_id: u64) -> Result<bool, InteractionHandlerError> {
        let now = Instant::now();
        let mut cooldowns = self
            .cooldowns
            .lock()
            .map_err(|_| InteractionHandlerError::from("wrapped cooldown lock poisoned"))?;
        cooldowns.retain(|_, started| now.duration_since(*started) < COMMAND_COOLDOWN);
        if cooldowns.contains_key(&user_id) {
            return Ok(true);
        }
        cooldowns.insert(user_id, now);
        Ok(false)
    }

    async fn prefetch_profiles(
        &self,
        guild_id: u64,
        profile_ids: BTreeSet<i64>,
        avatar_ids: BTreeSet<i64>,
    ) -> BTreeMap<i64, WrappedDiscordProfile> {
        let limiter = Arc::new(Semaphore::new(PROFILE_CONCURRENCY));
        let mut tasks = tokio::task::JoinSet::new();
        for signed_id in profile_ids {
            let Ok(user_id) = u64::try_from(signed_id) else {
                continue;
            };
            let discord = Arc::clone(&self.discord);
            let limiter = Arc::clone(&limiter);
            let include_avatar = avatar_ids.contains(&signed_id);
            tasks.spawn(async move {
                let permit = limiter.acquire_owned().await.ok()?;
                let result = discord
                    .wrapped_profile(guild_id, user_id, include_avatar)
                    .await;
                drop(permit);
                match result {
                    Ok(profile) => profile.map(|profile| (signed_id, profile)),
                    Err(error) => {
                        warn!(%error, user_id, "wrapped Discord profile prefetch failed");
                        None
                    }
                }
            });
        }
        let mut profiles = BTreeMap::new();
        while let Some(result) = tasks.join_next().await {
            if let Ok(Some((id, profile))) = result {
                profiles.insert(id, profile);
            }
        }
        profiles
    }

    fn schedule_expiry(
        &self,
        session_id: u64,
        expiry_generation: u64,
        responder: Arc<dyn InteractionResponder>,
    ) {
        let sessions = Arc::clone(&self.sessions);
        let timeout = self.timeout;
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            let session = sessions
                .lock()
                .ok()
                .and_then(|sessions| sessions.get(&session_id).cloned());
            let Some(session) = session else {
                return;
            };
            let mut locked = session.lock().await;
            if !locked.active || locked.expiry_generation != expiry_generation {
                return;
            }
            locked.active = false;
            let receipt = locked.receipt;
            if let Ok(mut sessions) = sessions.lock()
                && sessions
                    .get(&session_id)
                    .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                sessions.remove(&session_id);
            }
            drop(locked);
            if let Some(receipt) = receipt
                && let Err(error) = responder.delete_message(receipt).await
            {
                warn!(%error, session_id, "failed to delete expired wrapped story");
            }
        });
    }
}

#[derive(Clone, Copy)]
enum StoryDirection {
    Previous,
    Next,
}

fn parse_component(custom_id: &str) -> Result<(u64, StoryDirection), InteractionHandlerError> {
    let mut parts = custom_id.split(':');
    if parts.next() != Some("wrapped") {
        return Err(format!("invalid wrapped component {custom_id:?}").into());
    }
    let session_id = parts
        .next()
        .ok_or_else(|| InteractionHandlerError::from("missing wrapped session id"))?
        .parse::<u64>()
        .map_err(|_| InteractionHandlerError::from("invalid wrapped session id"))?;
    let direction = match parts.next() {
        Some("prev") => StoryDirection::Previous,
        Some("next") => StoryDirection::Next,
        _ => return Err(format!("invalid wrapped component {custom_id:?}").into()),
    };
    if parts.next().is_some() {
        return Err(format!("invalid wrapped component {custom_id:?}").into());
    }
    Ok((session_id, direction))
}

fn user_option(options: &[InteractionOption]) -> Option<(u64, String)> {
    options.iter().find_map(|option| {
        (option.name == "user")
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::User {
                    id, display_name, ..
                } => Some((*id, display_name.clone().unwrap_or_else(|| id.to_string()))),
                _ => None,
            })
    })
}

fn signed_id(id: u64, label: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(id).map_err(|_| format!("Discord {label} id exceeds SQLite INTEGER").into())
}

fn response_error(error: crate::registration::InteractionResponseError) -> InteractionHandlerError {
    error.to_string().into()
}

async fn wrapped_failure(
    responder: &Arc<dyn InteractionResponder>,
) -> Result<(), InteractionHandlerError> {
    responder
        .followup(
            InteractionResponse::message(
                "Something went wrong generating your wrapped. Please try again later.",
            )
            .ephemeral()
            .without_mentions(),
        )
        .await
        .map_err(response_error)
}

fn story_response(
    content: String,
    bytes: Vec<u8>,
    session_id: u64,
    index: usize,
    count: usize,
) -> InteractionResponse {
    InteractionResponse::message(content)
        .attachment(InteractionAttachment::bytes("wrapped_slide.png", bytes))
        .action_row(InteractionActionRow::buttons(vec![
            InteractionButton::new(format!("wrapped:{session_id}:prev"), "< Prev")
                .style(InteractionButtonStyle::Secondary)
                .disabled(index == 0),
            InteractionButton::new(format!("wrapped:{session_id}:next"), "Next >")
                .style(InteractionButtonStyle::Primary)
                .disabled(index + 1 >= count),
        ]))
}

fn prefetch_ids(
    raw: &WrappedRawData,
    minimum_games: usize,
    minimum_bets: i64,
) -> (BTreeSet<i64>, BTreeSet<i64>) {
    let award_ids = generate_awards(raw, minimum_games, minimum_bets)
        .into_iter()
        .map(|award| award.discord_id)
        .collect::<BTreeSet<_>>();
    let avatar_ids = pairwise_player_ids(&raw.pairwise)
        .into_iter()
        .collect::<BTreeSet<_>>();
    (award_ids.union(&avatar_ids).copied().collect(), avatar_ids)
}

#[derive(Clone, Debug)]
struct StoryAward {
    discord_id: i64,
    username: String,
    title: &'static str,
    stat: String,
    flavor: String,
}

fn build_slides(
    raw: WrappedRawData,
    profiles: &BTreeMap<i64, WrappedDiscordProfile>,
    target_id: i64,
    target_display_name: &str,
    year: i32,
    minimum_games: usize,
    minimum_bets: i64,
) -> Vec<WrappedSlideData> {
    let year_label = format!("Cama Wrapped {year}");
    let mut slides = Vec::new();
    let display_name = |id: i64, fallback: &str| {
        profiles
            .get(&id)
            .map(|profile| profile.display_name.clone())
            .unwrap_or_else(|| fallback.to_owned())
    };

    let mut server = base_slide(
        "server_summary",
        "Server Summary",
        target_display_name,
        &year_label,
    );
    server.headline = "THE SERVER YEAR IN REVIEW".to_owned();
    server.lines = vec![
        format!("{} MATCHES PLAYED", raw.summary.total_matches),
        format!("{} HEROES PICKED", raw.summary.unique_heroes),
        format!("{} JC WAGERED", raw.summary.total_wagered),
    ];
    if let Some(player) = raw
        .player_stats
        .iter()
        .take(10)
        .find(|player| player.games_played >= minimum_games as i64)
    {
        server.lines.push(format!(
            "TOP PERFORMER - {} - {}W {}L ({:.0}% WR)",
            player.discord_username,
            player.wins,
            player.games_played - player.wins,
            if player.games_played == 0 {
                0.0
            } else {
                player.wins as f64 / player.games_played as f64 * 100.0
            },
        ));
    }
    if let Some(hero) = raw.hero_stats.first() {
        server.lines.push(format!(
            "MOST PLAYED - {} - {} picks ({:.0}% WR)",
            hero_name(hero.hero_id),
            hero.picks,
            if hero.picks == 0 {
                0.0
            } else {
                hero.wins as f64 / hero.picks as f64 * 100.0
            },
        ));
    }
    if let Some(hero) = first_max_by(
        raw.hero_stats.iter().filter(|hero| hero.picks >= 5),
        |hero| hero.wins as f64 / hero.picks as f64,
    ) {
        server.lines.push(format!(
            "BEST WIN RATE - {} - {:.0}% ({} games)",
            hero_name(hero.hero_id),
            hero.wins as f64 / hero.picks as f64 * 100.0,
            hero.picks,
        ));
    }
    server.lines.push(format!(
        "{} players participated",
        raw.summary.total_players
    ));
    slides.push(server);

    let all_awards = generate_awards(&raw, minimum_games, minimum_bets);
    if !all_awards.is_empty() {
        let mut selected = all_awards
            .iter()
            .filter(|award| award.discord_id == target_id)
            .cloned()
            .collect::<Vec<_>>();
        let mut other_awards = all_awards
            .iter()
            .filter(|award| award.discord_id != target_id)
            .cloned()
            .collect::<Vec<_>>();
        fastrand::shuffle(&mut other_awards);
        selected.extend(
            other_awards
                .into_iter()
                .take(6_usize.saturating_sub(selected.len())),
        );
        selected.truncate(6);
        let mut awards = base_slide("awards", "Awards", target_display_name, &year_label);
        awards.headline = "SERVER SUPERLATIVES".to_owned();
        awards.lines = selected
            .into_iter()
            .map(|award| {
                format!(
                    "{} - {} - {} - {}",
                    display_name(award.discord_id, &award.username),
                    award.title,
                    award.stat,
                    award.flavor,
                )
            })
            .collect();
        slides.push(awards);
    }

    let app_stats = raw
        .player_stats
        .iter()
        .map(|row| MatchStatsRow {
            discord_id: row.discord_id,
            games_played: row.games_played,
            wins: row.wins,
            total_kills: row.total_kills,
            total_deaths: row.total_deaths,
            total_assists: row.total_assists,
        })
        .collect::<Vec<_>>();
    let app_heroes = raw
        .player_heroes
        .iter()
        .map(|row| HeroRow {
            discord_id: row.discord_id,
            hero_id: row.hero_id,
            picks: row.picks,
            wins: row.wins,
            total_kills: row.total_kills,
            total_deaths: row.total_deaths,
            total_assists: row.total_assists,
        })
        .collect::<Vec<_>>();
    let app_matches = raw
        .matches
        .iter()
        .map(|row| YearMatchRow {
            won: row.won,
            duration_seconds: row.duration_seconds,
            enrichment_present: row.enrichment_present,
            lane_role: row.lane_role,
        })
        .collect::<Vec<_>>();
    let summary = PersonalSummaryInput {
        discord_id: target_id,
        player_name: raw.player_name.clone(),
        match_details: (!raw.matches.is_empty()).then(|| MatchDetails {
            games_played: raw.matches.len() as i64,
            wins: raw
                .matches
                .iter()
                .filter(|row| row.won == Some(true))
                .count() as i64,
            losses: raw
                .matches
                .iter()
                .filter(|row| row.won == Some(false))
                .count() as i64,
        }),
        rating_change: raw
            .rating_changes
            .iter()
            .find(|row| row.discord_id == target_id)
            .map(|row| row.rating_change as i64),
        match_stats: app_stats,
        heroes: app_heroes.clone(),
        matches: app_matches.clone(),
    };
    let mut personal = personal_summary(&summary);
    if let Some(personal) = &mut personal {
        personal.flavor_text = random_flavor(if personal.games_played_percentile >= 75.0 {
            "games_played_high"
        } else if personal.games_played_percentile >= 30.0 {
            "games_played_mid"
        } else {
            "games_played_low"
        });
        push_personal_slides(
            &mut slides,
            personal,
            &personal.discord_username,
            &year_label,
        );
    }

    if raw.player_name.is_some() && raw.matches.len() >= minimum_games {
        for (kind, title, lines, accent) in record_slides(&raw.matches) {
            if lines.is_empty() {
                continue;
            }
            let mut slide = base_slide(
                kind,
                title,
                raw.player_name.as_deref().unwrap_or(target_display_name),
                &year_label,
            );
            slide.headline = format!("{title} RECORDS").to_ascii_uppercase();
            slide.lines = lines;
            slide.accent = accent;
            slides.push(slide);
        }
    }

    if let Some(hero) = hero_spotlight(target_id, &app_heroes, |id| Some(hero_name(id))) {
        let mut slide = base_slide(
            "story_hero",
            "Hero Spotlight",
            target_display_name,
            &year_label,
        );
        slide.headline = hero.top_hero_name.clone();
        slide.stat_value = format!("{} PICKS", hero.top_hero_picks);
        slide.lines = hero
            .top_3_heroes
            .iter()
            .map(|entry| {
                format!(
                    "{} - {} picks - {:.0}% win - {:.2} KDA",
                    entry.name,
                    entry.picks,
                    entry.win_rate * 100.0,
                    entry.kda
                )
            })
            .collect();
        slide.accent = [46, 204, 113];
        slides.push(slide);
    }

    if let Some(roles) = role_breakdown(&app_matches)
        && !roles.lane_freq.is_empty()
    {
        let mut slide = base_slide(
            "story_lanes",
            "Lane Breakdown",
            target_display_name,
            &year_label,
        );
        slide.headline = "WHERE YOU LIVED".to_owned();
        for role in [1, 2, 3] {
            if let Some(count) = roles.lane_freq.get(&role) {
                slide
                    .lines
                    .push(format!("{} - {} games", lane_name(role), count));
                slide.series.push(*count as f64);
            }
        }
        slide.accent = [155, 89, 182];
        slides.push(slide);
    }

    if let Some(pairwise) = pairwise_wrapped(Some(&raw.pairwise), &raw.pairwise_names) {
        push_pairwise_slides(&mut slides, &pairwise, profiles, target_display_name);
    }

    let app_purchases = raw
        .package_purchases
        .iter()
        .enumerate()
        .map(
            |(index, purchase)| cama_app::wrapped_story::PackageDealPurchase {
                id: index as i64 + 1,
                guild_id: 0,
                buyer_discord_id: purchase.buyer_discord_id,
                partner_discord_id: purchase.partner_discord_id,
                games_committed: purchase.games_committed,
                games_remaining: 0,
                jc_spent: purchase.jc_spent,
                year,
            },
        )
        .collect::<Vec<_>>();
    if let Some(package) = package_deal_wrapped(target_id, app_purchases.iter()) {
        let mut slide = base_slide(
            "story_packages",
            "Package Deals",
            target_display_name,
            "All-Time",
        );
        slide.headline = "FRIENDSHIP HAS A PRICE".to_owned();
        slide.stat_value = format!(
            "{} DEALS",
            package.times_bought + package.times_bought_on_you
        );
        slide.stat_label = "CONTRACT ENERGY".to_owned();
        slide.lines = vec![
            format!(
                "{} bought by you - {} JC",
                package.times_bought, package.jc_spent
            ),
            format!(
                "{} bought on you - {} JC",
                package.times_bought_on_you, package.jc_spent_on_you
            ),
            format!("{} unique buyers", package.unique_buyers),
            format!("{} games committed", package.total_games_committed),
            random_flavor("package_deal"),
        ];
        slide.accent = [241, 196, 15];
        slides.push(slide);
    }

    let rating_series = raw
        .rating_history
        .iter()
        .rev()
        .map(|point| point.rating.unwrap_or(f64::NAN))
        .collect::<Vec<_>>();
    let openskill_series = raw
        .rating_history
        .iter()
        .rev()
        .map(|point| {
            point
                .os_mu_after
                .map(|mu| ((mu - 25.0) * 50.0).max(0.0))
                .unwrap_or(f64::NAN)
        })
        .collect::<Vec<_>>();
    let rating_points = rating_series
        .iter()
        .chain(&openskill_series)
        .filter(|rating| rating.is_finite())
        .count();
    if raw.rating_history.len() >= 2 && rating_points >= 2 {
        let mut slide = base_slide(
            "chart_rating",
            "Rating Chart",
            target_display_name,
            &year_label,
        );
        slide.headline = "RATING HISTORY (ALL-TIME)".to_owned();
        let latest = rating_series
            .iter()
            .rev()
            .chain(openskill_series.iter().rev())
            .find(|rating| rating.is_finite())
            .copied()
            .unwrap_or_default();
        slide.lines = vec![format!(
            "{} points - latest {latest:.0}",
            raw.rating_history.len()
        )];
        slide.series = rating_series;
        slide.secondary_series = openskill_series;
        slide.outcomes = raw
            .rating_history
            .iter()
            .rev()
            .map(|point| point.won)
            .collect();
        slides.push(slide);
    }

    if let Some(gamba) = raw.gamba_stats
        && !raw.gamba_points.is_empty()
    {
        let mut slide = base_slide(
            "chart_gamba",
            "Gamba (All-Time)",
            target_display_name,
            &year_label,
        );
        slide.headline = "GAMBA (ALL-TIME)".to_owned();
        let pnl = raw.gamba_points.last().map_or(0, |point| point.cumulative);
        slide.lines = vec![format!(
            "{}{} JC · {} bets · Degen Score: {}",
            if pnl >= 0 { "+" } else { "" },
            pnl,
            gamba.total_bets,
            gamba.degen_score.total
        )];
        slide.gamba = Some(WrappedGambaData {
            degen_score: i32::try_from(gamba.degen_score.total).unwrap_or_default(),
            degen_title: gamba.degen_score.title.to_owned(),
            points: raw.gamba_points,
            stats: wrapped_gamba_stats(&gamba),
        });
        slide.accent = [237, 66, 69];
        slides.push(slide);
    }
    slides
}

fn base_slide(kind: &str, title: &str, username: &str, year_label: &str) -> WrappedSlideData {
    let mut slide = WrappedSlideData::new(kind, title);
    slide.username = username.to_owned();
    slide.year_label = year_label.to_owned();
    slide
}

fn push_personal_slides(
    slides: &mut Vec<WrappedSlideData>,
    personal: &PersonalSummaryWrapped,
    target_name: &str,
    year_label: &str,
) {
    let mut reveal = base_slide("story_games", "Your Year", target_name, year_label);
    reveal.headline = "YOUR YEAR IN REVIEW".to_owned();
    reveal.stat_value = personal.games_played.to_string();
    reveal.stat_label = "GAMES PLAYED".to_owned();
    reveal.lines = vec![
        personal.flavor_text.clone(),
        format!(
            "More than {:.0}% of players",
            personal.games_played_percentile
        ),
    ];
    reveal.accent = [241, 196, 15];
    slides.push(reveal);

    let kda = (personal.total_kills + personal.total_assists) as f64
        / personal.total_deaths.max(1) as f64;
    let mut stats = base_slide("story_summary", "Stats Grid", target_name, year_label);
    stats.headline = "THE NUMBERS".to_owned();
    stats.lines = vec![
        format!("{:.0}% WIN RATE", personal.win_rate * 100.0),
        format!("{kda:.1} AVG KDA"),
        format!("{}m AVG GAME", personal.avg_game_duration / 60),
        format!(
            "{}/{}/{} TOTAL K/D/A",
            personal.total_kills, personal.total_deaths, personal.total_assists
        ),
        format!("{} UNIQUE HEROES", personal.unique_heroes),
        format!("{:+} RATING", personal.rating_change),
    ];
    slides.push(stats);
}

fn push_pairwise_slides(
    slides: &mut Vec<WrappedSlideData>,
    pairwise: &cama_app::wrapped_story::PairwiseWrapped,
    profiles: &BTreeMap<i64, WrappedDiscordProfile>,
    target_name: &str,
) {
    let mut teammate_entries = pairwise
        .best_teammates
        .iter()
        .take(3)
        .enumerate()
        .map(|(index, entry)| PairwiseSlideEntry {
            entry,
            section: (index == 0).then_some("BEST TEAMMATE"),
            flavor: Some(random_flavor("teammate_best")),
        })
        .collect::<Vec<_>>();
    let seen_teammates = teammate_entries
        .iter()
        .map(|entry| entry.entry.discord_id)
        .collect::<BTreeSet<_>>();
    let teammate_slots = 6_usize.saturating_sub(teammate_entries.len());
    let most_played_start = teammate_entries.len();
    teammate_entries.extend(
        pairwise
            .most_played_with
            .iter()
            .filter(|entry| !seen_teammates.contains(&entry.discord_id))
            .take(teammate_slots)
            .enumerate()
            .map(|(index, entry)| PairwiseSlideEntry {
                entry,
                section: (index == 0 && most_played_start < 6).then_some("MOST PLAYED WITH"),
                flavor: None,
            }),
    );
    if !teammate_entries.is_empty() {
        let mut slide = base_slide("story_teammates", "Teammates", target_name, "All-Time");
        slide.headline = "YOUR TEAMMATES".to_owned();
        add_pairwise_entries(&mut slide, &teammate_entries, profiles);
        slide.accent = [46, 204, 113];
        slides.push(slide);
    }
    let mut rivals = Vec::<PairwiseSlideEntry<'_>>::new();
    if let Some(entry) = pairwise.nemesis.as_ref() {
        rivals.push(PairwiseSlideEntry {
            entry,
            section: Some("NEMESIS"),
            flavor: Some(random_flavor("rival_nemesis")),
        });
    }
    if let Some(entry) = pairwise.punching_bag.as_ref() {
        rivals.push(PairwiseSlideEntry {
            entry,
            section: Some("PUNCHING BAG"),
            flavor: Some(random_flavor("rival_punching_bag")),
        });
    }
    let seen_rivals = rivals
        .iter()
        .map(|entry| entry.entry.discord_id)
        .collect::<BTreeSet<_>>();
    let rival_slots = 6_usize.saturating_sub(rivals.len());
    rivals.extend(
        pairwise
            .most_played_against
            .iter()
            .filter(|entry| !seen_rivals.contains(&entry.discord_id))
            .take(rival_slots)
            .enumerate()
            .map(|(index, entry)| PairwiseSlideEntry {
                entry,
                section: (index == 0).then_some("MOST FACED"),
                flavor: None,
            }),
    );
    if !rivals.is_empty() {
        let mut slide = base_slide("story_rivals", "Rivals", target_name, "All-Time");
        slide.headline = "YOUR RIVALS".to_owned();
        add_pairwise_entries(&mut slide, &rivals, profiles);
        slide.accent = [237, 66, 69];
        slides.push(slide);
    }
}

struct PairwiseSlideEntry<'a> {
    entry: &'a PairwiseEntry,
    section: Option<&'static str>,
    flavor: Option<String>,
}

fn add_pairwise_entries(
    slide: &mut WrappedSlideData,
    entries: &[PairwiseSlideEntry<'_>],
    profiles: &BTreeMap<i64, WrappedDiscordProfile>,
) {
    for item in entries {
        let entry = item.entry;
        let losses = entry.games - entry.wins;
        let mut line = format!(
            "{}{} - {}W {}L ({:.0}% WR) - {} games",
            item.section
                .map_or(String::new(), |section| format!("{section} | ")),
            entry.username,
            entry.wins,
            losses,
            entry.win_rate * 100.0,
            entry.games,
        );
        if let Some(flavor) = &item.flavor {
            line.push_str(&format!(" - {flavor}"));
        }
        slide.lines.push(line);
        slide.avatars.push(WrappedAvatar {
            discord_id: entry.discord_id,
            png: profiles
                .get(&entry.discord_id)
                .and_then(|profile| profile.avatar_png.clone())
                .unwrap_or_default(),
        });
    }
}

fn record_slides(
    rows: &[WrappedYearMatch],
) -> Vec<(&'static str, &'static str, Vec<String>, [u8; 3])> {
    let best = |extract: fn(&WrappedYearMatch) -> Option<f64>| {
        first_max_by(
            rows.iter()
                .filter_map(|row| extract(row).map(|value| (value, row))),
            |(value, _)| *value,
        )
    };
    let worst = |extract: fn(&WrappedYearMatch) -> Option<f64>| {
        first_min_by(
            rows.iter()
                .filter_map(|row| extract(row).map(|value| (value, row))),
            |(value, _)| *value,
        )
    };
    let mut combat = Vec::new();
    push_record(
        &mut combat,
        "Most Kills",
        best(|row| row.kills.map(|value| value as f64)),
    );
    push_record(
        &mut combat,
        "Most Assists",
        best(|row| row.assists.map(|value| value as f64)),
    );
    push_record(&mut combat, "Best KDA", best(kda));
    push_record(
        &mut combat,
        "Feeding Frenzy",
        best(|row| row.deaths.map(|value| value as f64)),
    );
    push_record(&mut combat, "Worst KDA", worst(kda));
    push_record(&mut combat, "Kill Participation", best(kill_participation));

    let mut farming = Vec::new();
    for (label, value) in [
        ("Highest GPM", best(|row| row.gpm.map(|value| value as f64))),
        ("Highest XPM", best(|row| row.xpm.map(|value| value as f64))),
        (
            "Most Last Hits",
            best(|row| row.last_hits.map(|value| value as f64)),
        ),
        (
            "Most Denies",
            best(|row| row.denies.map(|value| value as f64)),
        ),
        ("Lowest GPM", worst(|row| row.gpm.map(|value| value as f64))),
        (
            "AFK Simulator",
            worst(|row| row.xpm.map(|value| value as f64)),
        ),
        (
            "Allergic to Creeps",
            worst(|row| row.last_hits.map(|value| value as f64)),
        ),
    ] {
        push_record(&mut farming, label, value);
    }

    let mut impact = Vec::new();
    for (label, value) in [
        (
            "Most Hero Damage",
            best(|row| row.hero_damage.map(|value| value as f64)),
        ),
        (
            "Most Tower Damage",
            best(|row| row.tower_damage.map(|value| value as f64)),
        ),
        (
            "Most Tower Kills",
            best(|row| row.towers_killed.map(|value| value as f64)),
        ),
        (
            "Most Hero Healing",
            best(|row| row.hero_healing.map(|value| value as f64)),
        ),
    ] {
        push_record(&mut impact, label, value);
    }
    push_record_or_na(
        &mut impact,
        "Biggest Comeback",
        best(|row| row.comeback.filter(|value| *value > 0.0)),
    );
    push_record_or_na(
        &mut impact,
        "Charity Case",
        best(|row| row.throw.filter(|value| *value > 0.0)),
    );

    let mut vision = Vec::new();
    for (label, value) in [
        (
            "Most Obs Placed",
            best(|row| row.obs_placed.map(|value| value as f64)),
        ),
        (
            "Most Sentries",
            best(|row| row.sen_placed.map(|value| value as f64)),
        ),
        ("Most Stuns", best(|row| row.stuns)),
    ] {
        push_record(&mut vision, label, value);
    }
    for (label, value) in [
        (
            "Most Courier Kills",
            best(|row| {
                row.courier_kills
                    .filter(|value| *value > 0)
                    .map(|value| value as f64)
            }),
        ),
        (
            "Signal Spammer",
            best(|row| row.pings.map(|value| value as f64)),
        ),
        (
            "Highest APM",
            best(|row| row.actions_per_min.filter(|value| *value > 0.0)),
        ),
    ] {
        push_record_or_na(&mut vision, label, value);
    }

    let mut endurance = Vec::new();
    push_duration_record(
        &mut endurance,
        "Longest Game",
        best(|row| {
            row.duration_seconds
                .filter(|value| *value > 0)
                .map(|value| value as f64)
        }),
    );
    push_duration_record(
        &mut endurance,
        "Shortest Game",
        worst(|row| {
            row.duration_seconds
                .filter(|value| *value > 0)
                .map(|value| value as f64)
        }),
    );
    let (wins, losses, win_breaker, loss_breaker) = longest_streaks(rows);
    if wins > 0 {
        endurance.push(format!(
            "Longest Win Streak - {wins} wins - {}",
            hero_name_or_na(win_breaker)
        ));
    }
    if losses > 0 {
        endurance.push(format!(
            "Tilt Master - {losses} losses - {}",
            hero_name_or_na(loss_breaker)
        ));
    }
    push_record_or_na(
        &mut endurance,
        "Most Rapiers",
        best(|row| (row.rapier_count > 0).then_some(row.rapier_count as f64)),
    );

    vec![
        ("records_combat", "Combat", combat, [237, 66, 69]),
        ("records_farming", "Farming", farming, [241, 196, 15]),
        ("records_impact", "Impact", impact, [155, 89, 182]),
        ("records_vision", "Vision & Utility", vision, [88, 101, 242]),
        (
            "records_endurance",
            "Endurance & Streaks",
            endurance,
            [46, 204, 113],
        ),
    ]
}

fn push_record(output: &mut Vec<String>, label: &str, value: Option<(f64, &WrappedYearMatch)>) {
    if let Some((value, row)) = value {
        output.push(format!(
            "{label} - {value:.1} - {}",
            hero_name_or_na(row.hero_id)
        ));
    }
}

fn push_duration_record(
    output: &mut Vec<String>,
    label: &str,
    value: Option<(f64, &WrappedYearMatch)>,
) {
    if let Some((seconds, row)) = value {
        let seconds = seconds as i64;
        output.push(format!(
            "{label} - {}:{:02} min - {}",
            seconds / 60,
            seconds % 60,
            hero_name_or_na(row.hero_id)
        ));
    }
}

fn push_record_or_na(
    output: &mut Vec<String>,
    label: &str,
    value: Option<(f64, &WrappedYearMatch)>,
) {
    if let Some(value) = value {
        push_record(output, label, Some(value));
    } else {
        output.push(format!("{label} - N/A"));
    }
}

fn kda(row: &WrappedYearMatch) -> Option<f64> {
    let (kills, deaths, assists) = (row.kills?, row.deaths?, row.assists?);
    (kills + deaths + assists > 0).then_some((kills + assists) as f64 / deaths.max(1) as f64)
}

fn kill_participation(row: &WrappedYearMatch) -> Option<f64> {
    let score = match row.side.as_deref() {
        Some("radiant") => row.radiant_score?,
        Some("dire") => row.dire_score?,
        _ => return None,
    };
    (score > 0).then_some((row.kills? + row.assists?) as f64 / score as f64 * 100.0)
}

fn longest_streaks(rows: &[WrappedYearMatch]) -> (usize, usize, Option<i64>, Option<i64>) {
    let (mut win, mut loss, mut best_win, mut best_loss) = (0, 0, 0, 0);
    let (mut best_win_end, mut best_loss_end) = (None, None);
    for (index, row) in rows.iter().enumerate() {
        match row.won {
            Some(true) => {
                win += 1;
                loss = 0;
                if win > best_win {
                    best_win = win;
                    best_win_end = Some(index);
                }
            }
            Some(false) => {
                loss += 1;
                win = 0;
                if loss > best_loss {
                    best_loss = loss;
                    best_loss_end = Some(index);
                }
            }
            None => {
                win = 0;
                loss = 0;
            }
        }
    }
    let breaker = |end: Option<usize>, expected: bool| {
        end.and_then(|index| rows.get(index + 1))
            .and_then(|row| (row.won == Some(expected)).then_some(row.hero_id).flatten())
    };
    (
        best_win,
        best_loss,
        breaker(best_win_end, false),
        breaker(best_loss_end, true),
    )
}

fn generate_awards(
    raw: &WrappedRawData,
    minimum_games: usize,
    minimum_bets: i64,
) -> Vec<StoryAward> {
    let eligible = raw
        .player_stats
        .iter()
        .filter(|row| row.games_played >= minimum_games as i64)
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return Vec::new();
    }
    let mut awards = Vec::new();
    award_max(
        &mut awards,
        &eligible,
        |row| row.avg_gpm,
        "Gold Goblin",
        |row| format!("{:.0} GPM", row.avg_gpm),
        "award_gold_goblin",
    );
    let best_kda = first_max_by(eligible.iter().copied(), |row| row.avg_kda);
    if let Some(row) = best_kda
        && row.avg_kda != 0.0
    {
        push_award(
            &mut awards,
            row,
            "Immortal Hands",
            format!("{:.2} KDA", row.avg_kda),
            "award_immortal_hands",
        );
    }
    if let Some(row) = first_min_by(
        eligible.iter().copied().filter(|row| row.avg_kda > 0.0),
        |row| row.avg_kda,
    ) && best_kda.is_some_and(|best| best.discord_id != row.discord_id)
    {
        push_award(
            &mut awards,
            row,
            "First Blood Enthusiast",
            format!("{:.2} KDA", row.avg_kda),
            "award_first_blood_enthusiast",
        );
    }
    award_max(
        &mut awards,
        &eligible,
        |row| row.total_wards as f64,
        "Ward Bot 9000",
        |row| format!("{} placed", row.total_wards),
        "award_ward_bot",
    );
    award_max(
        &mut awards,
        &eligible,
        |row| row.avg_xpm,
        "XP Vacuum",
        |row| format!("{:.0} XPM", row.avg_xpm),
        "award_xp_vacuum",
    );
    award_max(
        &mut awards,
        &eligible,
        |row| row.total_kills as f64,
        "Serial Killer",
        |row| format!("{} kills", row.total_kills),
        "award_serial_killer",
    );
    award_max(
        &mut awards,
        &eligible,
        |row| row.total_assists as f64,
        "Assist Machine",
        |row| format!("{} assists", row.total_assists),
        "award_assist_machine",
    );
    let best_win_rate = first_max_by(eligible.iter().copied(), |row| win_rate(row));
    if let Some(row) = best_win_rate
        && win_rate(row) > 0.5
    {
        push_award(
            &mut awards,
            row,
            "Win Merchant",
            format!("{:.0}%", win_rate(row) * 100.0),
            "award_win_merchant",
        );
    }
    if let Some(row) = first_min_by(eligible.iter().copied(), |row| win_rate(row))
        && win_rate(row) < 0.5
        && best_win_rate.is_some_and(|best| best.discord_id != row.discord_id)
    {
        push_award(
            &mut awards,
            row,
            "Charity Case",
            format!("{:.0}%", win_rate(row) * 100.0),
            "award_charity_case",
        );
    }
    award_max(
        &mut awards,
        &eligible,
        |row| row.total_fantasy,
        "Fantasy King",
        |row| format!("{:.0} pts", row.total_fantasy),
        "award_fantasy_king",
    );

    if let Some(row) = first_max_by(raw.rating_changes.iter(), |row| row.rating_change)
        && row.rating_change > 0.0
    {
        awards.push(rating_award(
            row,
            "Elo Inflation",
            format!("+{:.0} rating", row.rating_change),
            "award_elo_inflation",
        ));
    }
    if let Some(row) = first_min_by(raw.rating_changes.iter(), |row| row.rating_change)
        && row.rating_change < 0.0
    {
        awards.push(rating_award(
            row,
            "The Cliff",
            format!("{:.0} rating", row.rating_change),
            "award_the_cliff",
        ));
    }
    let variance = raw
        .rating_changes
        .iter()
        .filter(|row| row.rating_variance.is_some())
        .collect::<Vec<_>>();
    let consistent = first_min_by(variance.iter().copied(), |row| {
        row.rating_variance.unwrap_or_default()
    });
    if let Some(row) = consistent {
        awards.push(rating_award(
            row,
            "Steady Eddie",
            format!(
                "±{} rating std dev",
                row.rating_variance.unwrap_or_default().sqrt() as i64
            ),
            "award_steady_eddie",
        ));
    }
    if let Some(row) = first_max_by(variance.iter().copied(), |row| {
        row.rating_variance.unwrap_or_default()
    }) && consistent.is_some_and(|consistent| consistent.discord_id != row.discord_id)
    {
        awards.push(rating_award(
            row,
            "Coin Flip Player",
            format!(
                "±{} rating std dev",
                row.rating_variance.unwrap_or_default().sqrt() as i64
            ),
            "award_coin_flip",
        ));
    }

    let bettors = raw
        .betting
        .iter()
        .filter(|row| row.total_bets >= minimum_bets)
        .collect::<Vec<_>>();
    let best_roi = first_max_by(bettors.iter().copied(), |row| betting_roi(row));
    if let Some(row) = best_roi
        && betting_roi(row) > 0.0
    {
        awards.push(betting_award(
            row,
            "Diamond Hands",
            format!("+{:.1}%", betting_roi(row) * 100.0),
            "award_diamond_hands",
        ));
    }
    if let Some(row) = first_min_by(bettors.iter().copied(), |row| betting_roi(row))
        && betting_roi(row) < 0.0
        && best_roi.is_some_and(|best| best.discord_id != row.discord_id)
    {
        awards.push(betting_award(
            row,
            "House's Favorite",
            format!("{:.1}%", betting_roi(row) * 100.0),
            "award_house_favorite",
        ));
    }
    if let Some(row) = first_max_by(bettors.iter().copied(), |row| row.total_wagered as f64) {
        awards.push(betting_award(
            row,
            "Degen Supreme",
            format!("{} JC", row.total_wagered),
            "award_degen_supreme",
        ));
    }
    if let Some(row) = first_max_by(raw.bankruptcies.iter(), |row| row.bankruptcy_count as f64)
        && row.bankruptcy_count > 0
    {
        awards.push(bankruptcy_award(row));
    }

    #[derive(Clone)]
    struct HeroAwardCandidate {
        discord_id: i64,
        username: String,
        hero_id: i64,
        picks: i64,
        one_trick_percent: f64,
        unique_heroes: usize,
    }
    let mut heroes_by_player = BTreeMap::<i64, Vec<&WrappedPlayerHeroStats>>::new();
    for hero in &raw.player_heroes {
        heroes_by_player
            .entry(hero.discord_id)
            .or_default()
            .push(hero);
    }
    let mut hero_candidates = Vec::new();
    for (discord_id, heroes) in heroes_by_player {
        let Some(top_hero) = first_max_by(heroes.iter().copied(), |hero| hero.picks as f64) else {
            continue;
        };
        let total_games = heroes.iter().map(|hero| hero.picks).sum::<i64>();
        let Some(player) = raw
            .player_stats
            .iter()
            .find(|player| player.discord_id == discord_id)
        else {
            continue;
        };
        if total_games < minimum_games as i64 {
            continue;
        }
        hero_candidates.push(HeroAwardCandidate {
            discord_id,
            username: player.discord_username.clone(),
            hero_id: top_hero.hero_id,
            picks: top_hero.picks,
            one_trick_percent: if total_games > 0 {
                top_hero.picks as f64 / total_games as f64
            } else {
                0.0
            },
            unique_heroes: heroes.len(),
        });
    }
    if let Some(candidate) = first_max_by(hero_candidates.iter(), |entry| entry.one_trick_percent)
        && candidate.one_trick_percent >= 0.3
    {
        awards.push(StoryAward {
            discord_id: candidate.discord_id,
            username: candidate.username.clone(),
            title: "One-Trick Pony",
            stat: format!("{}g on {}", candidate.picks, hero_name(candidate.hero_id)),
            flavor: random_flavor("award_one_trick"),
        });
    }
    if let Some(candidate) =
        first_max_by(hero_candidates.iter(), |entry| entry.unique_heroes as f64)
    {
        awards.push(StoryAward {
            discord_id: candidate.discord_id,
            username: candidate.username.clone(),
            title: "Jack of All Trades",
            stat: format!("{} heroes", candidate.unique_heroes),
            flavor: random_flavor("award_jack_of_all_trades"),
        });
    }

    let no_life = first_max_by(eligible.iter().copied(), |row| row.games_played as f64);
    if let Some(row) = no_life {
        push_award(
            &mut awards,
            row,
            "No Life",
            format!("{} games", row.games_played),
            "award_no_life",
        );
    }
    if let Some(row) = first_min_by(eligible.iter().copied(), |row| row.games_played as f64)
        && no_life.is_some_and(|most| most.discord_id != row.discord_id)
    {
        push_award(
            &mut awards,
            row,
            "Touched Grass",
            format!("{} games", row.games_played),
            "award_touched_grass",
        );
    }
    if let Some(row) = first_max_by(raw.bets_against.iter(), |row| row.bets_against as f64)
        && row.bets_against >= 3
    {
        awards.push(StoryAward {
            discord_id: row.discord_id,
            username: row.discord_username.clone(),
            title: "Public Enemy #1",
            stat: format!("{} bets", row.bets_against),
            flavor: random_flavor("award_punching_bag"),
        });
    }
    awards
}

fn award_max<F, S>(
    awards: &mut Vec<StoryAward>,
    rows: &[&WrappedPlayerStats],
    metric: F,
    title: &'static str,
    stat: S,
    flavor_key: &str,
) where
    F: Fn(&WrappedPlayerStats) -> f64,
    S: Fn(&WrappedPlayerStats) -> String,
{
    if let Some(row) = first_max_by(rows.iter().copied(), |row| metric(row))
        && metric(row) > 0.0
    {
        push_award(awards, row, title, stat(row), flavor_key);
    }
}
fn push_award(
    awards: &mut Vec<StoryAward>,
    row: &WrappedPlayerStats,
    title: &'static str,
    stat: String,
    flavor_key: &str,
) {
    awards.push(StoryAward {
        discord_id: row.discord_id,
        username: row.discord_username.clone(),
        title,
        stat,
        flavor: random_flavor(flavor_key),
    });
}
fn rating_award(
    row: &WrappedRatingChange,
    title: &'static str,
    stat: String,
    flavor_key: &str,
) -> StoryAward {
    StoryAward {
        discord_id: row.discord_id,
        username: row.discord_username.clone(),
        title,
        stat,
        flavor: random_flavor(flavor_key),
    }
}
fn betting_award(
    row: &WrappedBettingStats,
    title: &'static str,
    stat: String,
    flavor_key: &str,
) -> StoryAward {
    StoryAward {
        discord_id: row.discord_id,
        username: row.discord_username.clone(),
        title,
        stat,
        flavor: random_flavor(flavor_key),
    }
}
fn bankruptcy_award(row: &WrappedBankruptcy) -> StoryAward {
    StoryAward {
        discord_id: row.discord_id,
        username: row.discord_username.clone(),
        title: "Bankruptcy Speedrunner",
        stat: format!("{}x", row.bankruptcy_count),
        flavor: random_flavor("award_bankruptcy_speedrunner"),
    }
}
fn win_rate(row: &WrappedPlayerStats) -> f64 {
    if row.games_played == 0 {
        0.0
    } else {
        row.wins as f64 / row.games_played as f64
    }
}
fn betting_roi(row: &WrappedBettingStats) -> f64 {
    if row.total_wagered == 0 {
        0.0
    } else {
        row.net_pnl as f64 / row.total_wagered as f64
    }
}

fn hero_name(hero_id: i64) -> String {
    static HERO_NAMES: std::sync::OnceLock<BTreeMap<i64, String>> = std::sync::OnceLock::new();
    HERO_NAMES
        .get_or_init(|| {
            serde_json::from_str::<BTreeMap<String, String>>(include_str!(
                "../../cama-app/data/heroes.json"
            ))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(id, name)| id.parse().ok().map(|id| (id, name)))
            .collect()
        })
        .get(&hero_id)
        .cloned()
        .unwrap_or_else(|| format!("Hero #{hero_id}"))
}
fn hero_name_or_na(hero_id: Option<i64>) -> String {
    hero_id.map(hero_name).unwrap_or_else(|| "N/A".to_owned())
}
fn lane_name(role: i32) -> &'static str {
    match role {
        1 => "Safe Lane",
        2 => "Mid Lane",
        3 => "Off Lane",
        _ => "Unknown",
    }
}
fn random_flavor(key: &str) -> String {
    flavor_pool(key).map_or_else(String::new, |pool| {
        get_flavor(key, fastrand::usize(..pool.len()), &[])
    })
}

fn first_max_by<I, T, F>(values: I, metric: F) -> Option<T>
where
    I: IntoIterator<Item = T>,
    F: Fn(&T) -> f64,
{
    let mut values = values.into_iter();
    let mut best = values.next()?;
    let mut best_metric = metric(&best);
    for value in values {
        let value_metric = metric(&value);
        if value_metric > best_metric {
            best = value;
            best_metric = value_metric;
        }
    }
    Some(best)
}

fn first_min_by<I, T, F>(values: I, metric: F) -> Option<T>
where
    I: IntoIterator<Item = T>,
    F: Fn(&T) -> f64,
{
    let mut values = values.into_iter();
    let mut best = values.next()?;
    let mut best_metric = metric(&best);
    for value in values {
        let value_metric = metric(&value);
        if value_metric < best_metric {
            best = value;
            best_metric = value_metric;
        }
    }
    Some(best)
}

#[cfg(all(test, feature = "runtime-test-core"))]
mod tests;
