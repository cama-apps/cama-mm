//! Complete live `/wrapped` Discord command provider.
//!
//! The provider reads the Rust-migrated SQLite database, renders every story
//! page as native PNG/GIF media, preserves the public defer/follow-up
//! contract, and owns a ten-minute invoker-scoped component session. Sessions
//! are intentionally process-local like discord.py's non-persistent `View`;
//! after restart or expiry, stale buttons receive an explicit private error.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::wrapped_media::{
    WrappedArtwork, WrappedLayout, WrappedRenderedMedia, WrappedRow, WrappedSlideData,
    WrappedTheme, render_wrapped_media, render_wrapped_slide, vary_beat_palettes,
};
use cama_app::wrapped_story::chapters::{months_card, story_seed};
use cama_app::wrapped_story::{
    HeroRow, MatchDetails, MatchStatsRow, PersonalMoment, PersonalMomentMatch,
    PersonalSummaryInput, PersonalSummaryWrapped, YearMatchRow, flavor_pool, get_flavor,
    hero_spotlight, personal_moments, personal_summary, personal_superlatives, role_breakdown,
    year_timestamps,
};
use cama_db::gambling_stats_repository::{
    GamblingOutcome, GamblingStatsRepository, GamblingStatsService,
};
use cama_db::pet_repository::PetRepository;
use cama_db::wrapped_activity::{WrappedActivityRepository, YearWindow};
use cama_db::wrapped_live::{
    WrappedBankruptcy, WrappedBetsAgainst, WrappedBettingStats, WrappedHeroStats,
    WrappedLiveRepository, WrappedPlayerHeroStats, WrappedPlayerStats, WrappedRatingChange,
    WrappedRatingPoint, WrappedServerSummary, WrappedYearMatch,
};
use cama_domain::discord_content::{UNKNOWN_PLAYER, readable_player_name};
use cama_domain::hero_catalog::hero_name;
use cama_domain::wrapped_distinct::{
    DistinctMatch, DistinctPlayer, PeopleYear, people_year, personal_records,
};
use chrono::{Datelike, Utc};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tracing::warn;

use crate::application_config::ApplicationConfig;
use crate::ids::signed_id;
use crate::option_ext::user_option;
use crate::registration::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute, InteractionActionRow,
    InteractionAttachment, InteractionButton, InteractionButtonStyle, InteractionHandler,
    InteractionHandlerError, InteractionMessageReceipt, InteractionRequest, InteractionResponder,
    InteractionResponse, RegistrationError, RegistrationProvider, RegistryBuilder,
};

mod activity;
mod deck;
mod distinct;
mod personal;

use activity::{
    ActivityData, ArtSources, activity_cards, card_slide, persona_signals, persona_slide,
    recap_numbers,
};
use distinct::{MIN_STORIES, cave_in_slide, distinct_slides, people_slides, stonks};
use personal::{hero_story_slides, record_count, record_story_slides};

const COMPONENT_PREFIX: &str = "wrapped:";
const STORY_TIMEOUT: Duration = Duration::from_secs(600);
const COMMAND_COOLDOWN: Duration = Duration::from_secs(60);
const PROFILE_CONCURRENCY: usize = 5;
const MAX_CACHED_MEDIA_BYTES: usize = 12 * 1024 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WrappedDiscordProfile {
    pub display_name: String,
}

/// Narrow live Discord seam used for Wrapped's bounded name prefetch.
#[async_trait]
pub trait WrappedDiscordPort: Send + Sync {
    async fn wrapped_profile(
        &self,
        guild_id: u64,
        user_id: u64,
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
                sources: WrappedSources::with_art(
                    path,
                    ArtSources::production(config.values.pet_hunger_decay_per_day),
                ),
                discord,
                minimum_games: config.values.wrapped_min_games.max(0) as usize,
                minimum_bets: config.values.wrapped_min_bets.max(0),
                cooldowns: Mutex::new(HashMap::new()),
                sessions: Arc::new(Mutex::new(BTreeMap::new())),
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
    timeout: Duration,
    fixed_year: Option<i32>,
}

struct StorySession {
    owner_id: u64,
    guild_id: u64,
    slides: Vec<WrappedSlideData>,
    details: Vec<WrappedSlideData>,
    rendered: BTreeMap<(bool, usize, bool), WrappedRenderedMedia>,
    current: usize,
    story_position: usize,
    showing_details: bool,
    animated: bool,
    expiry_generation: u64,
    receipt: Option<InteractionMessageReceipt>,
    active: bool,
    /// The recap was posted to the channel; it can be shared once per story.
    shared: bool,
}

impl StorySession {
    fn pages(&self) -> &[WrappedSlideData] {
        if self.showing_details {
            &self.details
        } else {
            &self.slides
        }
    }
}

#[derive(Clone)]
struct WrappedSources {
    wrapped: WrappedLiveRepository,
    gambling: GamblingStatsService<GamblingStatsRepository>,
    activity: WrappedActivityRepository,
    pets: PetRepository,
    art: ArtSources,
}

impl WrappedSources {
    #[cfg(all(test, feature = "runtime-test-core"))]
    fn new(path: &Path) -> Self {
        Self::with_art(path, ArtSources::default_production())
    }

    fn with_art(path: &Path, art: ArtSources) -> Self {
        Self {
            wrapped: WrappedLiveRepository::new(path),
            gambling: GamblingStatsService::new(GamblingStatsRepository::new(path)),
            activity: WrappedActivityRepository::new(path),
            pets: PetRepository::new(path),
            art,
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
        let hero_art = self.top_hero_portraits(&player_heroes, target_id);
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
        let matches = self
            .wrapped
            .player_year_matches(target_id, guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;

        let (year_rows, year_players) = self
            .wrapped
            .guild_year_rows(guild_id, start_ts, end_ts)
            .map_err(|error| error.to_string())?;
        let people = people_year(&year_rows, target_id);
        let rating_history = self
            .wrapped
            .rating_history(target_id, guild_id, start_ts, end_ts)
            .unwrap_or_else(|error| {
                warn!(%error, target_id, guild_id, "failed to fetch Wrapped rating history");
                Vec::new()
            });
        let gamba_stats = self
            .gambling
            .get_player_stats(target_id, Some(guild_id))
            .map_err(|error| error.to_string())?;
        let activity = self.load_activity(
            guild_id,
            target_id,
            YearWindow {
                start: start_ts,
                end: end_ts,
            },
        )?;
        // Gold graphs are read only for the story that draws one.
        let repository = self.wrapped.clone();
        let gold: GoldLookup = Box::new(move |match_id| {
            repository
                .gold_advantage(guild_id, match_id)
                .unwrap_or_else(|error| {
                    warn!(%error, match_id, "failed to fetch Wrapped gold advantage");
                    None
                })
        });

        Ok(Some(WrappedRawData {
            summary,
            player_stats,
            hero_stats,
            player_heroes,
            rating_changes,
            betting,
            bets_against,
            bankruptcies,
            matches,
            year_rows,
            year_players,
            people,
            rating_history,
            gamba_stats,
            activity,
            hero_art,
            art: self.art.clone(),
            gold,
        }))
    }

    /// Portraits for every hero the top-heroes card could rank. Ties at the
    /// cut all get one, so the card's own ordering never misses a portrait.
    fn top_hero_portraits(
        &self,
        heroes: &[WrappedPlayerHeroStats],
        target_id: i64,
    ) -> BTreeMap<i64, Vec<u8>> {
        let own = heroes
            .iter()
            .filter(|row| row.discord_id == target_id)
            .collect::<Vec<_>>();
        let mut picks = own.iter().map(|row| row.picks).collect::<Vec<_>>();
        picks.sort_unstable_by(|left, right| right.cmp(left));
        let Some(&cut) = picks.get(TOP_HEROES - 1).or(picks.last()) else {
            return BTreeMap::new();
        };
        own.into_iter()
            .filter(|row| row.picks >= cut)
            .filter_map(|row| Some((row.hero_id, self.art.hero_portrait(row.hero_id)?)))
            .collect()
    }
}

/// A match's radiant gold advantage per minute, read on demand.
type GoldLookup = Box<dyn Fn(i64) -> Option<Vec<f64>> + Send>;

struct WrappedRawData {
    summary: WrappedServerSummary,
    player_stats: Vec<WrappedPlayerStats>,
    hero_stats: Vec<WrappedHeroStats>,
    player_heroes: Vec<WrappedPlayerHeroStats>,
    rating_changes: Vec<WrappedRatingChange>,
    betting: Vec<WrappedBettingStats>,
    bets_against: Vec<WrappedBetsAgainst>,
    bankruptcies: Vec<WrappedBankruptcy>,
    matches: Vec<WrappedYearMatch>,
    /// Every registered player's games in the guild this year.
    year_rows: Vec<DistinctMatch>,
    year_players: Vec<DistinctPlayer>,
    /// Who the player shared the year with.
    people: PeopleYear,
    rating_history: Vec<WrappedRatingPoint>,
    gamba_stats: Option<cama_db::gambling_stats_repository::GambaStats>,
    activity: ActivityData,
    /// Cached Steam portraits for the player's top heroes, by hero id.
    hero_art: BTreeMap<i64, Vec<u8>>,
    /// Hero art for the distinct stories, which are chosen later.
    art: ArtSources,
    gold: GoldLookup,
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
            interaction_id: session_id,
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
        let (target_id, _target_name) = user_option(&options, "user")?
            .map(|user| (user.raw_id, user.readable_display_name()))
            .unwrap_or((user_id, user_display_name));
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

        let profile_ids = prefetch_ids(&raw, signed_target, self.minimum_games, self.minimum_bets);
        let profiles = self.prefetch_profiles(guild_id, profile_ids).await;
        let minimum_games = self.minimum_games;
        let minimum_bets = self.minimum_bets;
        let slide_target_name = profiles
            .get(&signed_target)
            .map(|profile| profile.display_name.as_str())
            .and_then(readable_player_name)
            .unwrap_or(UNKNOWN_PLAYER)
            .to_owned();
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
        let (mut details, mut slides): (Vec<_>, Vec<_>) = slides
            .into_iter()
            .partition(|slide| is_detail_page(&slide.kind));
        number_chapters(&mut slides);
        number_chapters(&mut details);
        let first_slide = slides[0].clone();
        let first =
            match tokio::task::spawn_blocking(move || render_wrapped_media(&first_slide, true))
                .await
            {
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
        let session = Arc::new(AsyncMutex::new(StorySession {
            owner_id: user_id,
            guild_id,
            slides,
            details,
            rendered: BTreeMap::from([((false, 0, true), first.clone())]),
            current: 0,
            story_position: 0,
            showing_details: false,
            animated: true,
            expiry_generation: 0,
            receipt: None,
            active: true,
            shared: false,
        }));
        self.sessions
            .lock()
            .map_err(|_| InteractionHandlerError::from("wrapped session lock poisoned"))?
            .insert(session_id, Arc::clone(&session));
        let response = story_response(first, session_id, &*session.lock().await, 0, false, true);
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
            custom_id,
            user_id,
            guild_id,
            ..
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
        // Acknowledge before waiting behind another click's render or delivery.
        responder.defer(false).await.map_err(response_error)?;
        let mut session = session.lock().await;
        if !session.active || Some(session.guild_id) != guild_id {
            return responder
                .followup(
                    InteractionResponse::message("This wrapped has expired.")
                        .ephemeral()
                        .without_mentions(),
                )
                .await
                .map_err(response_error);
        }
        if session.owner_id != user_id {
            return responder
                .followup(
                    InteractionResponse::message(
                        "Only the person who started this Wrapped can use its buttons.",
                    )
                    .ephemeral(),
                )
                .await
                .map_err(response_error);
        }
        session.expiry_generation = session.expiry_generation.wrapping_add(1);
        let expiry_generation = session.expiry_generation;
        self.schedule_expiry(session_id, expiry_generation, Arc::clone(&responder));
        let mut showing_details = session.showing_details;
        let mut animated = session.animated;
        let mut story_position = session.story_position;
        let target = match direction {
            StoryDirection::Previous => session.current.saturating_sub(1),
            StoryDirection::Next => (session.current + 1).min(session.pages().len() - 1),
            StoryDirection::Replay => {
                showing_details = false;
                0
            }
            StoryDirection::Motion => {
                animated = !animated;
                session.current
            }
            StoryDirection::Recap => {
                showing_details = false;
                session.slides.len() - 1
            }
            StoryDirection::Details if !session.details.is_empty() => {
                if !showing_details {
                    story_position = session.current;
                }
                showing_details = true;
                0
            }
            StoryDirection::Back => {
                showing_details = false;
                story_position
            }
            StoryDirection::Details => session.current,
            StoryDirection::Share => {
                if let Some(reply) = self.share_recap(&mut session, &responder).await? {
                    return responder.followup(reply).await.map_err(response_error);
                }
                session.current
            }
        };
        let cache_key = (showing_details, target, animated);
        let media = if let Some(media) = session.rendered.get(&cache_key) {
            media.clone()
        } else {
            let slide = if showing_details {
                &session.details[target]
            } else {
                &session.slides[target]
            }
            .clone();
            match tokio::task::spawn_blocking(move || render_wrapped_media(&slide, animated)).await
            {
                Ok(Ok(media)) => {
                    let cached_bytes: usize =
                        session.rendered.values().map(|item| item.bytes.len()).sum();
                    if cached_bytes + media.bytes.len() > MAX_CACHED_MEDIA_BYTES {
                        session.rendered.clear();
                    }
                    session.rendered.insert(cache_key, media.clone());
                    media
                }
                Ok(Err(error)) => {
                    warn!(%error, session_id, target, "wrapped slide render failed");
                    return responder
                        .followup(
                            InteractionResponse::message("Could not create this card.").ephemeral(),
                        )
                        .await
                        .map_err(response_error);
                }
                Err(error) => {
                    warn!(%error, session_id, target, "wrapped slide task failed");
                    return responder
                        .followup(
                            InteractionResponse::message("Could not create this card.").ephemeral(),
                        )
                        .await
                        .map_err(response_error);
                }
            }
        };
        let response = story_response(
            media,
            session_id,
            &session,
            target,
            showing_details,
            animated,
        );
        // Keep delivery ordered with navigation and only advance after Discord
        // accepts the page. A failed edit leaves the displayed state intact.
        responder.update(response).await.map_err(response_error)?;
        session.current = target;
        session.showing_details = showing_details;
        session.animated = animated;
        session.story_position = story_position;
        Ok(())
    }

    /// Post the recap as its own message, which outlives the story. Returns a
    /// private reply instead when the recap cannot be shared.
    async fn share_recap(
        &self,
        session: &mut StorySession,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<Option<InteractionResponse>, InteractionHandlerError> {
        if session.shared {
            return Ok(Some(
                InteractionResponse::message("Your recap is already in the channel.").ephemeral(),
            ));
        }
        let Some(recap) = session.slides.last().cloned() else {
            return Ok(Some(
                InteractionResponse::message("There is no recap to share.").ephemeral(),
            ));
        };
        let content = format!("**{}** · {}", recap.year_label, recap.username);
        let png = match tokio::task::spawn_blocking(move || render_wrapped_slide(&recap)).await {
            Ok(Ok(png)) => png,
            Ok(Err(error)) => {
                warn!(%error, "wrapped recap render failed");
                return Ok(Some(
                    InteractionResponse::message("Could not create your recap.").ephemeral(),
                ));
            }
            Err(error) => {
                warn!(%error, "wrapped recap task failed");
                return Ok(Some(
                    InteractionResponse::message("Could not create your recap.").ephemeral(),
                ));
            }
        };
        responder
            .followup(
                InteractionResponse::message(content)
                    .without_mentions()
                    .attachment(InteractionAttachment::bytes("cama_wrapped.png", png)),
            )
            .await
            .map_err(response_error)?;
        session.shared = true;
        Ok(None)
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
    ) -> BTreeMap<i64, WrappedDiscordProfile> {
        let limiter = Arc::new(Semaphore::new(PROFILE_CONCURRENCY));
        let mut tasks = tokio::task::JoinSet::new();
        for signed_id in profile_ids {
            let Ok(user_id) = u64::try_from(signed_id) else {
                continue;
            };
            let discord = Arc::clone(&self.discord);
            let limiter = Arc::clone(&limiter);
            tasks.spawn(async move {
                let permit = limiter.acquire_owned().await.ok()?;
                let result = discord.wrapped_profile(guild_id, user_id).await;
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
                && let Err(error) = responder
                    .delete_message(InteractionMessageReceipt {
                        delivery: crate::registration::InteractionMessageDelivery::ChannelFallback,
                        ..receipt
                    })
                    .await
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
    Replay,
    Motion,
    Recap,
    Details,
    Back,
    Share,
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
        Some("replay") => StoryDirection::Replay,
        Some("motion") => StoryDirection::Motion,
        Some("recap") => StoryDirection::Recap,
        Some("details") => StoryDirection::Details,
        Some("back") => StoryDirection::Back,
        Some("share") => StoryDirection::Share,
        _ => return Err(format!("invalid wrapped component {custom_id:?}").into()),
    };
    if parts.next().is_some() {
        return Err(format!("invalid wrapped component {custom_id:?}").into());
    }
    Ok((session_id, direction))
}

fn response_error(error: crate::registration::InteractionResponseError) -> InteractionHandlerError {
    error.to_string().into()
}

async fn wrapped_failure(
    responder: &Arc<dyn InteractionResponder>,
) -> Result<(), InteractionHandlerError> {
    responder
        .followup(
            InteractionResponse::message("Could not create your Wrapped. Try again later.")
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(response_error)
}

fn story_response(
    media: WrappedRenderedMedia,
    session_id: u64,
    session: &StorySession,
    index: usize,
    showing_details: bool,
    animated: bool,
) -> InteractionResponse {
    let pages = if showing_details {
        &session.details
    } else {
        &session.slides
    };
    let count = pages.len();
    let slide = &pages[index];
    let last = index + 1 == count;
    let mut buttons = vec![
        InteractionButton::new(format!("wrapped:{session_id}:prev"), "Previous")
            .style(InteractionButtonStyle::Secondary)
            .disabled(index == 0),
        InteractionButton::new(
            format!(
                "wrapped:{session_id}:{}",
                if last && !showing_details {
                    "replay"
                } else {
                    "next"
                }
            ),
            if last && !showing_details {
                "Replay"
            } else {
                "Next"
            },
        )
        .style(InteractionButtonStyle::Primary)
        .disabled(last && showing_details),
        InteractionButton::new(
            format!("wrapped:{session_id}:motion"),
            if animated {
                "Still images"
            } else {
                "Animations"
            },
        ),
        if last && !showing_details {
            InteractionButton::new(
                format!("wrapped:{session_id}:share"),
                if session.shared {
                    "Shared"
                } else {
                    "Share recap"
                },
            )
            .style(InteractionButtonStyle::Success)
            .disabled(session.shared)
        } else {
            InteractionButton::new(
                format!(
                    "wrapped:{session_id}:{}",
                    if showing_details { "back" } else { "recap" }
                ),
                if showing_details {
                    "Back to story"
                } else {
                    "My recap"
                },
            )
        },
    ];
    if !showing_details && !session.details.is_empty() {
        buttons.push(InteractionButton::new(
            format!("wrapped:{session_id}:details"),
            "More stats",
        ));
    }
    InteractionResponse::message(format!(
        "**{}** for {}\n{} {}/{} · {}",
        slide.year_label,
        slide.username,
        if showing_details {
            "More stats"
        } else {
            "Chapter"
        },
        index + 1,
        count,
        slide.title,
    ))
    .without_mentions()
    .attachment(InteractionAttachment::bytes(media.filename, media.bytes))
    .action_row(InteractionActionRow::buttons(buttons))
}

fn number_chapters(slides: &mut [WrappedSlideData]) {
    let count = slides.len();
    for (index, slide) in slides.iter_mut().enumerate() {
        slide.chapter_index = index;
        slide.chapter_count = count;
    }
}

/// Everyone the story might name: the player, award winners, this year's
/// people, and the side games' rivals.
fn prefetch_ids(
    raw: &WrappedRawData,
    target_id: i64,
    minimum_games: usize,
    minimum_bets: i64,
) -> BTreeSet<i64> {
    let mut profile_ids = generate_awards(raw, minimum_games, minimum_bets)
        .into_iter()
        .map(|award| award.discord_id)
        .collect::<BTreeSet<_>>();
    profile_ids.insert(target_id);
    let people = &raw.people;
    profile_ids.extend(
        [
            &people.duo,
            &people.cursed_duo,
            &people.nemesis,
            &people.punching_bag,
        ]
        .into_iter()
        .flatten()
        .map(|pair| pair.discord_id),
    );
    profile_ids.extend(raw.activity.person_ids());
    profile_ids
}

#[derive(Clone, Debug)]
struct StoryAward {
    discord_id: i64,
    username: String,
    title: &'static str,
    stat: String,
    flavor: String,
}

/// Personal moments a thin deck borrows; the rest of the evidence lives in
/// the detailed stats.
const STORY_MOMENTS: usize = 2;
const MOMENT_THEMES: [WrappedTheme; 4] = [
    WrappedTheme::QueueAlt,
    WrappedTheme::Heroes,
    WrappedTheme::Queue,
    WrappedTheme::Months,
];
/// The story's running order. The recap always closes it.
const STORY_ORDER: [&str; 19] = [
    "story_intro",
    "story_games",
    "story_playtime",
    "story_hero",
    "story_distinct",
    "story_record",
    "story_hero_year",
    "story_people",
    "chart_gamba",
    "story_gamba",
    "story_dig",
    "story_dig_boss",
    "story_dig_feud",
    "story_dig_cave_in",
    "story_pet",
    "story_pet_grave",
    "story_mafia",
    "story_mafia_victim",
    "story_persona",
];

/// Pages that live under "More stats" rather than in the story itself.
fn is_detail_page(kind: &str) -> bool {
    kind.starts_with("records_")
        || kind.starts_with("more_")
        || matches!(
            kind,
            "awards"
                | "story_summary"
                | "server_summary"
                | "story_lanes"
                | "story_months"
                | "chart_rating"
        )
}

fn story_position(kind: &str) -> usize {
    match kind {
        "story_mafia_accuser" => story_position("story_mafia_victim"),
        // A thin deck's borrowed cards stand in for the distinct stories.
        "story_moment" | "story_superlative" => story_position("story_distinct"),
        _ => STORY_ORDER
            .iter()
            .position(|candidate| *candidate == kind)
            .unwrap_or(STORY_ORDER.len()),
    }
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
    let target_display_name = readable_player_name(target_display_name).unwrap_or(UNKNOWN_PLAYER);
    let year_label = format!("Cama Wrapped {year}");
    let mut slides = Vec::new();
    let display_name = |id: i64, _fallback: &str| {
        profiles
            .get(&id)
            .map(|profile| profile.display_name.as_str())
            .and_then(readable_player_name)
            .map(str::to_owned)
            .unwrap_or_else(|| cama_domain::discord_content::UNKNOWN_PLAYER.to_owned())
    };
    // Chapters name people only when a readable name is known.
    let readable_name = |id: i64| {
        profiles
            .get(&id)
            .map(|profile| profile.display_name.as_str())
            .and_then(readable_player_name)
            .filter(|name| *name != UNKNOWN_PLAYER)
            .map(str::to_owned)
    };

    let mut intro = base_slide("story_intro", "Your year", target_display_name, &year_label);
    intro.headline = format!("Your\n{year}\nWrapped");
    intro.stat_value = target_display_name.to_owned();
    intro.stat_label = "Your Cama Wrapped".to_owned();
    if raw.matches.is_empty() {
        intro
            .lines
            .push("No games recorded for you this year. Here are the server stats.".to_owned());
    }
    slides.push(intro);
    let mut server = base_slide(
        "server_summary",
        "Server stats",
        target_display_name,
        &year_label,
    );
    server.headline = "The server this year".to_owned();
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
            "MOST GAMES - {} - {}W {}L ({:.0}% WR)",
            display_name(player.discord_id, &player.discord_username),
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
        let lines = selected
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
            .collect::<Vec<_>>();
        for chunk in lines.chunks(2) {
            let mut awards =
                base_slide("awards", "Server records", target_display_name, &year_label);
            awards.headline = "Leaders in the recorded stats".to_owned();
            awards.lines = chunk.to_vec();
            slides.push(awards);
        }
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
        player_name: Some(target_display_name.to_owned()),
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
    let has_cohort = raw.player_stats.len() > 1
        && raw
            .player_stats
            .iter()
            .any(|row| row.discord_id == target_id);
    let personal = personal_summary(&summary);
    if let Some(personal) = &personal {
        push_personal_slides(
            &mut slides,
            personal,
            target_display_name,
            &year_label,
            has_cohort,
            &raw,
        );
    }

    let moment_rows = raw
        .matches
        .iter()
        .map(|row| PersonalMomentMatch {
            hero_id: row.hero_id,
            won: row.won,
            duration_seconds: row.duration_seconds,
            kills: row.kills,
            deaths: row.deaths,
            assists: row.assists,
            wards: row
                .obs_placed
                .zip(row.sen_placed)
                .map(|(obs, sen)| obs.saturating_add(sen)),
            pings: row.pings,
            courier_kills: row.courier_kills,
            hero_healing: row.hero_healing,
            tower_damage: row.tower_damage,
            comeback: row.comeback,
            apm: row.actions_per_min,
            rapiers: row.enrichment_present.then_some(row.rapier_count),
            throw: row.throw,
        })
        .collect::<Vec<_>>();
    let stories = personal_moments(&moment_rows);
    let superlatives = personal_superlatives(&moment_rows);
    let seed = story_seed(target_id, year);
    if let Some(time) = &stories.playtime {
        slides.push(moment_slide(
            time,
            "story_playtime",
            target_display_name,
            &year_label,
        ));
    }

    if raw.matches.len() >= minimum_games {
        for (kind, title, lines, accent) in record_slides(&raw.matches) {
            if lines.is_empty() {
                continue;
            }
            for chunk in lines.chunks(2) {
                let mut slide = base_slide(kind, title, target_display_name, &year_label);
                slide.headline = format!("Your {title} highlights");
                slide.lines = chunk.to_vec();
                slide.accent = accent;
                slides.push(slide);
            }
        }
    }

    let hero = hero_spotlight(target_id, &app_heroes, |id| Some(hero_name(id)));
    if let Some(hero) = &hero {
        let mut slide = hero_slide(
            hero,
            &app_heroes,
            &raw.hero_art,
            &raw.matches,
            target_id,
            target_display_name,
            &year_label,
        );
        // The main hero as a full render cutout, when startup cached one.
        slide.art_png = top_heroes(&app_heroes, target_id)
            .first()
            .and_then(|row| raw.art.hero_render(row.hero_id));
        slides.push(slide);
    }

    let match_dates = raw
        .matches
        .iter()
        .filter_map(|row| row.match_date.as_deref())
        .collect::<Vec<_>>();
    if let Some(card) = months_card(&match_dates, seed) {
        slides.push(card_slide(
            card,
            &raw.activity,
            target_display_name,
            &year_label,
        ));
    }

    if let Some(roles) = role_breakdown(&app_matches)
        && !roles.lane_freq.is_empty()
    {
        let mut slide = base_slide(
            "story_lanes",
            "Your lanes",
            target_display_name,
            &year_label,
        );
        slide.headline = "Where you played".to_owned();
        slide.stat_label = format!(
            "Lane data recorded in {} of {} games",
            roles.total_games,
            raw.matches.len()
        );
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
            "Your rating",
            target_display_name,
            &year_label,
        );
        let latest = rating_series
            .iter()
            .rev()
            .chain(openskill_series.iter().rev())
            .find(|rating| rating.is_finite())
            .copied()
            .unwrap_or_default();
        let change = personal
            .as_ref()
            .filter(|_| {
                raw.rating_changes
                    .iter()
                    .any(|row| row.discord_id == target_id)
            })
            .map(|personal| personal.rating_change);
        slide.headline = match change {
            Some(change) if change > 0 => "Your rating rose this year.",
            Some(change) if change < 0 => "Your rating fell this year.",
            _ => "Your rating history",
        }
        .to_owned();
        slide.stat_value = format!("{latest:.0}");
        slide.stat_label = match change {
            Some(change) => format!("rating now, {change:+} this year"),
            None => "rating now".to_owned(),
        };
        slide.lines = vec![format!(
            "{} rated games on the chart",
            raw.rating_history.len()
        )];
        slide.series = rating_series;
        slide.secondary_series = openskill_series;
        slides.push(slide);
    }

    // Side games: each played game's main card first, at most four in all.
    let bankruptcies = raw
        .bankruptcies
        .iter()
        .find(|row| row.discord_id == target_id)
        .map_or(0, |row| row.bankruptcy_count);
    let degen = raw
        .gamba_stats
        .as_ref()
        .map(|stats| (stats.degen_score.total, stats.degen_score.title));
    let mut side = activity_cards(
        &raw.activity,
        degen,
        bankruptcies,
        &readable_name,
        target_id,
        year,
    )
    .into_iter()
    .map(|card| {
        let mut slide = card_slide(card, &raw.activity, target_display_name, &year_label);
        if slide.kind == "chart_gamba" {
            stonks(&mut slide, &raw.activity.gamba, seed);
        }
        slide
    })
    .collect::<Vec<_>>();
    side.extend(cave_in_slide(
        &raw.activity.dig,
        seed,
        target_display_name,
        &year_label,
    ));
    let side_cards = deck::budget_side_cards(&mut side, &raw.activity);

    let mut people = people_slides(
        &raw.people,
        &readable_name,
        seed,
        target_display_name,
        &year_label,
    );
    people.truncate(deck::MOST_PEOPLE);

    // The player against their own games, once they have a year to measure.
    let (record_candidates, hero_stories) = if raw.matches.len() >= minimum_games {
        (
            personal_records(&raw.year_rows, target_id),
            hero_story_slides(
                &raw.year_rows,
                target_id,
                &raw.art,
                seed,
                target_display_name,
                &year_label,
            ),
        )
    } else {
        (Vec::new(), Vec::new())
    };

    // Distinct stories take what the other sections leave, up to eight.
    let fixed = slides
        .iter()
        .filter(|slide| deck::FIXED_KINDS.contains(&slide.kind.as_str()))
        .count();
    let slots = deck::distinct_slots(
        fixed
            + record_count(&record_candidates)
            + hero_stories.len()
            + people.len()
            + side_cards
            + deck::PERSONA_SLOTS,
    );
    let distinct = distinct_slides(
        &raw.year_rows,
        &raw.year_players,
        target_id,
        minimum_games,
        slots,
        &raw.art,
        &*raw.gold,
        seed,
        target_display_name,
        &year_label,
    );
    let records = record_story_slides(
        &record_candidates,
        &distinct.games,
        &raw.year_players,
        target_id,
        &raw.art,
        seed,
        target_display_name,
        &year_label,
    );
    // Too few distinct stories, as for a player below the minimum: the
    // personal moments and awards fill the section so the deck still says
    // something.
    let thin = distinct.stories.len() < MIN_STORIES;
    let mut open = slots.saturating_sub(distinct.stories.len());
    slides.extend(distinct.stories);
    for (index, moment) in stories.moments.iter().enumerate() {
        let kind = if thin && index < STORY_MOMENTS && open > 0 {
            open -= 1;
            "story_moment"
        } else {
            "records_moments"
        };
        let mut slide = moment_slide(moment, kind, target_display_name, &year_label);
        slide.layout = WrappedLayout::BigNumber;
        slide.theme = MOMENT_THEMES[index % MOMENT_THEMES.len()];
        slides.push(slide);
    }
    if thin {
        for award in superlatives.iter().take(open) {
            slides.push(moment_slide(
                award,
                "story_superlative",
                target_display_name,
                &year_label,
            ));
        }
    }
    slides.extend(records);
    slides.extend(hero_stories);
    slides.extend(people);
    slides.extend(side);

    let signals = persona_signals(
        personal.as_ref(),
        has_cohort,
        stories
            .identity
            .as_ref()
            .map(|identity| identity.value.as_str()),
        &raw.activity,
        minimum_games,
    );
    let persona = persona_slide(
        &signals,
        distinct.persona.as_ref(),
        &raw.activity,
        target_display_name,
        &year_label,
    );

    let mut finale = base_slide(
        "story_finale",
        &format!("{year} Wrapped"),
        target_display_name,
        &year_label,
    );
    finale.headline = persona
        .as_ref()
        .map(|persona| persona.stat_value.clone())
        .or_else(|| {
            stories
                .identity
                .as_ref()
                .map(|identity| identity.value.clone())
        })
        .unwrap_or_else(|| "Your year in Cama".to_owned());
    // The badge names something the deck showed: a thin deck's first award,
    // or the best popoff's fact line.
    finale.badge = if thin {
        superlatives
            .first()
            .map_or_else(String::new, |award| award.title.to_owned())
    } else {
        distinct.badge.unwrap_or_default()
    };
    finale.stat_value = target_display_name.to_owned();
    finale.rows = top_heroes(&app_heroes, target_id)
        .into_iter()
        .map(|row| {
            let mut ranked = WrappedRow::new(hero_name(row.hero_id), "");
            ranked.image_png = raw.hero_art.get(&row.hero_id).cloned();
            ranked
        })
        .collect();
    let highlights = [distinct.best_popoff, distinct.best_roast]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    finale.lines = recap_numbers(
        personal.as_ref(),
        stories.playtime.as_ref().map(|time| time.value.as_str()),
        &highlights,
        &raw.activity,
    );
    if finale.lines.is_empty() {
        finale.lines.push("0|recorded games this year".to_owned());
    }
    if let Some(persona) = persona {
        slides.push(persona);
    }
    slides.sort_by_key(|slide| story_position(&slide.kind));
    // Detail pages first, then the story, which the recap closes. Neighbors
    // in the story never share a background color.
    let (mut pages, mut story): (Vec<_>, Vec<_>) = slides
        .into_iter()
        .partition(|slide| is_detail_page(&slide.kind));
    story.push(finale);
    deck::space_fixed_backgrounds(&mut story);
    deck::separate_neighbors(&mut story);
    vary_beat_palettes(&mut story);
    pages.extend(story);
    pages
}

/// The player's heroes by picks, most played first, as the spotlight orders them.
/// How many heroes the top-heroes card ranks.
const TOP_HEROES: usize = 5;

fn top_heroes(heroes: &[HeroRow], target_id: i64) -> Vec<&HeroRow> {
    let mut own = heroes
        .iter()
        .filter(|row| row.discord_id == target_id)
        .collect::<Vec<_>>();
    own.sort_by_key(|row| std::cmp::Reverse(row.picks));
    own.truncate(TOP_HEROES);
    own
}

fn hero_slide(
    hero: &cama_app::wrapped_story::HeroSpotlightWrapped,
    heroes: &[HeroRow],
    portraits: &BTreeMap<i64, Vec<u8>>,
    matches: &[WrappedYearMatch],
    target_id: i64,
    username: &str,
    year_label: &str,
) -> WrappedSlideData {
    let mut slide = base_slide("story_hero", "Your heroes", username, year_label);
    slide.headline = "Your top heroes".to_owned();
    let recorded_picks: i64 = heroes
        .iter()
        .filter(|row| row.discord_id == target_id)
        .map(|row| row.picks)
        .sum();
    slide.stat_label = format!(
        "{} across {recorded_picks} recorded picks",
        match hero.unique_heroes {
            1 => "1 hero".to_owned(),
            count => format!("{count} heroes"),
        }
    );
    let own = top_heroes(heroes, target_id);
    slide.rows = own
        .iter()
        .map(|row| {
            let name = hero_name(row.hero_id);
            let mut detail = format!(
                "{} games · {:.0}% wins",
                row.picks,
                row.wins as f64 / row.picks.max(1) as f64 * 100.0
            );
            let samples = matches
                .iter()
                .filter(|game| game.hero_id == Some(row.hero_id))
                .filter_map(|game| game.kills.zip(game.deaths).zip(game.assists))
                .collect::<Vec<_>>();
            if !samples.is_empty() {
                let (kills, deaths, assists) = samples
                    .iter()
                    .fold((0, 0, 0), |(k, d, a), ((kills, deaths), assists)| {
                        (k + kills, d + deaths, a + assists)
                    });
                let kda = (kills + assists) as f64 / deaths.max(1) as f64;
                detail.push_str(&format!(" · {kda:.1} KDA"));
            }
            let mut ranked = WrappedRow::new(name, detail);
            ranked.image_png = portraits.get(&row.hero_id).cloned();
            ranked
        })
        .collect();
    // Spotify-style bragging rights: where this player ranks on their top hero.
    if let Some(top) = own.first() {
        let mut rivals = heroes
            .iter()
            .filter(|row| row.hero_id == top.hero_id && row.picks > 0)
            .map(|row| (row.discord_id, row.picks))
            .collect::<Vec<_>>();
        rivals.sort_by_key(|(_, picks)| std::cmp::Reverse(*picks));
        let ahead = rivals
            .iter()
            .filter(|(_, picks)| *picks > top.picks)
            .count();
        if rivals.len() >= 2 && ahead == 0 {
            slide.badge = format!("#1 by {} games", hero.top_hero_name);
        } else if rivals.len() >= 4 && ahead < 3 {
            slide.badge = format!(
                "#{} by {} games ({} players)",
                ahead + 1,
                hero.top_hero_name,
                rivals.len()
            );
        }
    }
    slide
}

fn moment_slide(
    moment: &PersonalMoment,
    kind: &str,
    username: &str,
    year: &str,
) -> WrappedSlideData {
    let mut slide = base_slide(kind, moment.title, username, year);
    slide.headline = moment.headline.to_owned();
    slide.stat_value = moment.value.clone();
    slide.stat_label = moment.label.to_owned();
    slide.lines = vec![moment.detail.clone()];
    // Coverage fine print stays on the detail pages.
    if is_detail_page(kind) {
        slide.lines.push(moment.coverage.clone());
    }
    slide.artwork = match moment.key {
        "wards" | "pings" => WrappedArtwork::Ward,
        "couriers" | "playtime" => WrappedArtwork::Courier,
        _ => WrappedArtwork::None,
    };
    slide
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
    has_cohort: bool,
    raw: &WrappedRawData,
) {
    let mut reveal = base_slide("story_games", "Your Year", target_name, year_label);
    reveal.headline = "Your games this year".to_owned();
    reveal.stat_value = personal.games_played.to_string();
    reveal.stat_label = "games played".to_owned();
    reveal.artwork = WrappedArtwork::Courier;
    if has_cohort && personal.games_played_percentile >= 50.0 {
        reveal.badge = format!(
            "More games than {:.0}% of the server",
            personal.games_played_percentile
        );
    }
    reveal.accent = [241, 196, 15];
    slides.push(reveal);

    let mut stats = base_slide("story_summary", "Your stats", target_name, year_label);
    stats.headline = "Your yearly totals".to_owned();
    stats.lines.push(format!(
        "{} games · {} wins · {} losses",
        personal.games_played, personal.wins, personal.losses
    ));
    let decided = personal.wins + personal.losses;
    if decided > 0 {
        stats.lines.push(format!(
            "{:.0}% win rate in {decided} decided games",
            personal.wins as f64 / decided as f64 * 100.0
        ));
    }
    let combat = raw
        .matches
        .iter()
        .filter_map(|row| row.kills.zip(row.deaths).zip(row.assists))
        .collect::<Vec<_>>();
    if combat.is_empty() {
        stats.lines.push("No combat stats recorded.".to_owned());
    } else {
        let (kills, deaths, assists) = combat
            .iter()
            .fold((0, 0, 0), |(k, d, a), ((kills, deaths), assists)| {
                (k + kills, d + deaths, a + assists)
            });
        let kda = (kills + assists) as f64 / deaths.max(1) as f64;
        stats.lines.push(format!(
            "{kills}/{deaths}/{assists} K/D/A · {kda:.1} aggregate KDA across {} recorded games",
            combat.len()
        ));
    }
    if personal.avg_game_duration > 0 {
        stats.lines.push(format!(
            "{}m average recorded game",
            personal.avg_game_duration / 60
        ));
    }
    if personal.unique_heroes > 0 {
        stats.lines.push(format!(
            "{} heroes in your recorded rotation",
            personal.unique_heroes
        ));
    }
    if raw
        .rating_changes
        .iter()
        .any(|row| row.discord_id == personal.discord_id)
    {
        stats.lines.push(format!(
            "{:+} rating change this year",
            personal.rating_change
        ));
    }
    slides.push(stats);
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
        "Most deaths",
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
        ("Lowest XPM", worst(|row| row.xpm.map(|value| value as f64))),
        (
            "Fewest last hits",
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
        "Largest lead lost",
        best(|row| row.throw.filter(|value| *value > 0.0)),
    );

    let mut vision = Vec::new();
    for (label, value) in [
        (
            "Most observer wards",
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
            "Most pings",
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
            "Longest loss streak - {losses} losses - {}",
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
        "Highest average GPM",
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
            "Highest average KDA",
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
            "Lowest average KDA",
            format!("{:.2} KDA", row.avg_kda),
            "award_first_blood_enthusiast",
        );
    }
    award_max(
        &mut awards,
        &eligible,
        |row| row.total_wards as f64,
        "Most wards placed",
        |row| format!("{} placed", row.total_wards),
        "award_ward_bot",
    );
    award_max(
        &mut awards,
        &eligible,
        |row| row.avg_xpm,
        "Highest average XPM",
        |row| format!("{:.0} XPM", row.avg_xpm),
        "award_xp_vacuum",
    );
    award_max(
        &mut awards,
        &eligible,
        |row| row.total_kills as f64,
        "Most kills",
        |row| format!("{} kills", row.total_kills),
        "award_serial_killer",
    );
    award_max(
        &mut awards,
        &eligible,
        |row| row.total_assists as f64,
        "Most assists",
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
            "Highest win rate",
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
            "Lowest win rate",
            format!("{:.0}%", win_rate(row) * 100.0),
            "award_charity_case",
        );
    }
    award_max(
        &mut awards,
        &eligible,
        |row| row.total_fantasy,
        "Most fantasy points",
        |row| format!("{:.0} pts", row.total_fantasy),
        "award_fantasy_king",
    );

    if let Some(row) = first_max_by(raw.rating_changes.iter(), |row| row.rating_change)
        && row.rating_change > 0.0
    {
        awards.push(rating_award(
            row,
            "Largest rating gain",
            format!("+{:.0} rating", row.rating_change),
            "award_elo_inflation",
        ));
    }
    if let Some(row) = first_min_by(raw.rating_changes.iter(), |row| row.rating_change)
        && row.rating_change < 0.0
    {
        awards.push(rating_award(
            row,
            "Largest rating loss",
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
            "Smallest rating swings",
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
            "Largest rating swings",
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
            "Best betting return",
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
            "Lowest betting return",
            format!("{:.1}%", betting_roi(row) * 100.0),
            "award_house_favorite",
        ));
    }
    if let Some(row) = first_max_by(bettors.iter().copied(), |row| row.total_wagered as f64) {
        awards.push(betting_award(
            row,
            "Most JC wagered",
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
            title: "Most focused hero pool",
            stat: format!(
                "{} games on {}",
                candidate.picks,
                hero_name(candidate.hero_id)
            ),
            flavor: random_flavor("award_one_trick"),
        });
    }
    if let Some(candidate) =
        first_max_by(hero_candidates.iter(), |entry| entry.unique_heroes as f64)
    {
        awards.push(StoryAward {
            discord_id: candidate.discord_id,
            username: candidate.username.clone(),
            title: "Most heroes played",
            stat: format!("{} heroes", candidate.unique_heroes),
            flavor: random_flavor("award_jack_of_all_trades"),
        });
    }

    let no_life = first_max_by(eligible.iter().copied(), |row| row.games_played as f64);
    if let Some(row) = no_life {
        push_award(
            &mut awards,
            row,
            "Most games played",
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
            "Fewest games played",
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
            title: "Most bets against",
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
        title: "Most bankruptcies",
        stat: format!(
            "{} bankruptcies all-time; active this year",
            row.bankruptcy_count
        ),
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
#[path = "wrapped_provider/tests.rs"]
mod tests;
