//! Production Discord provider for Python's classic `/trivia` extension.
//!
//! The in-memory question/view session intentionally remains process-local,
//! while cooldown claims, balance writes, pet activity, and leaderboard rows
//! use the admitted production SQLite schema. Component routes are registered
//! for every generated Python-compatible `trivia_...` custom ID.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::bankruptcy::{BankruptcyPolicy, BankruptcyService};
use cama_app::dotabase_sqlite::{DotabaseSqliteSource, PRODUCTION_DOTABASE_PATH};
use cama_app::economy_event_sqlite::SqliteEconomyEventService;
use cama_app::mana_effects_service::{ManaRecord, effects_from_record};
use cama_app::service_container::PersistentVanityTaxService;
use cama_app::trivia_data::TriviaDataRegistry;
use cama_app::trivia_image_cache::{
    CachedTriviaImage, PRODUCTION_TRIVIA_CACHE_PATH, TriviaImageCache,
};
use cama_app::trivia_questions::{TriviaCatalog, TriviaQuestion, TriviaRandom, generate_question};
use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::loan_repository::{LedgerContext, LoanRepository};
use cama_db::mana_service_repository::ManaRepository;
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::trivia_commands_repository::TriviaCommandsRepository;
use cama_domain::economy_scaling::scale_minigame_jc_delta;
use cama_domain::mana::ManaEffects;
use cama_domain::pet_evolution::PetActivity;
use chrono::{Datelike, Duration as ChronoDuration, NaiveDate, Timelike, Weekday};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::economy_events_worker::EconomyEventsWorkerConfig;
use crate::gateway_events::{GatewayEventObserver, ReadyRecoveryContext, ReadyRecoveryReport};
use crate::ids::signed_id;
use crate::registration::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute, InteractionActionRow,
    InteractionAttachment, InteractionButton, InteractionEmbed, InteractionHandler,
    InteractionHandlerError, InteractionMessageReceipt, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionResponseError, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};
use crate::{ApplicationConfig, ReminderHooks};

const COMPONENT_PREFIX: &str = "trivia_";
const CACHE_OBSERVER_NAME: &str = "trivia-image-cache";
const WRONG_CHANNEL_MESSAGE: &str = "The ancient spirits reject your offering... this ground is not consecrated. A single jopacoin dissolves into the ether as penance.";
const JOPACOIN_EMOTE: &str = "<:jopacoin:954159801049440297>";
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const QUESTION_GENERATION_RETRIES: usize = 15;
const COMMAND_RATE_LIMIT: Duration = Duration::from_secs(5);

#[async_trait]
pub trait TriviaDiscordPort: Send + Sync {
    /// True when this channel, or its thread parent, contains `gamba`.
    async fn trivia_channel_is_gamba(&self, guild_id: i64, channel_id: i64)
    -> Result<bool, String>;

    /// Equivalent to discord.py's `display_avatar.url` for embed authors.
    async fn trivia_user_avatar_url(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<String>, String>;

    async fn edit_trivia_message(
        &self,
        channel_id: u64,
        message_id: u64,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError>;

    async fn delete_trivia_message(&self, channel_id: u64, message_id: u64) -> Result<(), String>;
}

#[derive(Clone)]
pub struct TriviaRegistrationProvider {
    handler: Arc<TriviaInteractionHandler>,
    cache_observer: Arc<TriviaImageCacheWarmObserver>,
}

impl TriviaRegistrationProvider {
    /// Compose against the immutable Dotabase artifact and writable cache in
    /// the Rust production image.
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        discord: Arc<dyn TriviaDiscordPort>,
        reminder_hooks: Option<ReminderHooks>,
    ) -> Result<Self, String> {
        Self::from_paths(
            database_path,
            PRODUCTION_DOTABASE_PATH,
            PRODUCTION_TRIVIA_CACHE_PATH,
            config,
            vanity_tax,
            discord,
            reminder_hooks,
        )
    }

    pub fn from_paths(
        database_path: impl AsRef<Path>,
        dotabase_path: impl AsRef<Path>,
        cache_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        discord: Arc<dyn TriviaDiscordPort>,
        reminder_hooks: Option<ReminderHooks>,
    ) -> Result<Self, String> {
        let data = TriviaDataRegistry::new(DotabaseSqliteSource::new(dotabase_path));
        Self::from_catalog(
            database_path,
            data.question_catalog().map_err(|error| error.to_string())?,
            TriviaImageCache::new(cache_path).map_err(|error| error.to_string())?,
            config,
            vanity_tax,
            discord,
            reminder_hooks,
        )
    }

    fn from_catalog(
        database_path: impl AsRef<Path>,
        catalog: TriviaCatalog,
        image_cache: TriviaImageCache,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        discord: Arc<dyn TriviaDiscordPort>,
        reminder_hooks: Option<ReminderHooks>,
    ) -> Result<Self, String> {
        let database_path = database_path.as_ref().to_path_buf();
        let catalog = Arc::new(catalog);
        let image_cache = Arc::new(image_cache);
        let bankruptcy = BankruptcyService::new(
            BankruptcyRepository::new(&database_path),
            BankruptcyPolicy {
                fresh_start_balance: config.values.bankruptcy_fresh_start_balance,
                cooldown_seconds: config.values.bankruptcy_cooldown_seconds,
                penalty_games: config.values.bankruptcy_penalty_games,
                penalty_kept_rate: config.values.bankruptcy_penalty_rate,
            },
        )
        .map_err(|error| error.to_string())?;
        let economy_config = EconomyEventsWorkerConfig::from_application_config(config).event;
        let state = Arc::new(TriviaRuntimeState {
            trivia: TriviaCommandsRepository::new(&database_path),
            mana: ManaRepository::new(&database_path),
            bankruptcy,
            loans: LoanRepository::new(&database_path),
            pets: PetEvolutionRepository::new(&database_path),
            economy_events: SqliteEconomyEventService::new(&database_path, economy_config),
            vanity_tax,
            discord,
            reminder_hooks,
            catalog: Arc::clone(&catalog),
            image_cache: Arc::clone(&image_cache),
            random: Mutex::new(FastrandTriviaRandom::new()),
            sessions: Mutex::new(BTreeMap::new()),
            command_cooldowns: Mutex::new(BTreeMap::new()),
            config: TriviaRuntimeConfig::from_application_config(config),
        });
        Ok(Self {
            handler: Arc::new(TriviaInteractionHandler { state }),
            cache_observer: Arc::new(TriviaImageCacheWarmObserver {
                catalog,
                image_cache,
                task: Mutex::new(None),
            }),
        })
    }

    #[must_use]
    pub fn gateway_observer(&self) -> Arc<dyn GatewayEventObserver> {
        self.cache_observer.clone()
    }

    /// Share the already-loaded Dotabase-backed catalog with the live Dig
    /// bonus dispatcher.  Keeping one immutable catalog avoids a second
    /// SQLite load and guarantees both `/trivia` and Dig use the same question
    /// generators and authored records.
    #[must_use]
    pub fn catalog(&self) -> Arc<TriviaCatalog> {
        Arc::clone(&self.handler.state.catalog)
    }
}

impl RegistrationProvider for TriviaRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "trivia".to_owned(),
            description: "Test Dota 2 knowledge for 1 JC per correct answer plus streak bonuses."
                .to_owned(),
            options: Vec::new(),
            handler: self.handler.clone(),
        })?;
        registry.command(CommandSpec {
            name: "trivia-reset-cooldown".to_owned(),
            description: "(Admin) Reset a user's trivia cooldown.".to_owned(),
            options: vec![
                CommandOptionSpec::new(
                    "user",
                    "The user whose trivia cooldown to reset",
                    CommandOptionKind::User,
                )
                .required(true),
            ],
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

#[derive(Clone, Debug)]
struct TriviaRuntimeConfig {
    admin_user_ids: BTreeSet<i64>,
    answer_timeout: Duration,
    bankrupt_multiplier: f64,
    cooldown_seconds: i64,
    minigame_scale: f64,
    reward_per_question: i64,
}

impl TriviaRuntimeConfig {
    fn from_application_config(config: &ApplicationConfig) -> Self {
        Self {
            admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
            answer_timeout: Duration::from_secs(
                u64::try_from(config.values.trivia_answer_timeout_seconds.max(0))
                    .unwrap_or(u64::MAX),
            ),
            bankrupt_multiplier: config.values.trivia_bankrupt_multiplier,
            cooldown_seconds: config.values.trivia_cooldown_seconds,
            minigame_scale: config.values.minigame_jc_delta_scale,
            reward_per_question: config.values.trivia_reward_per_question,
        }
    }
}

struct TriviaRuntimeState {
    trivia: TriviaCommandsRepository,
    mana: ManaRepository,
    bankruptcy: BankruptcyService<BankruptcyRepository>,
    loans: LoanRepository,
    pets: PetEvolutionRepository,
    economy_events: SqliteEconomyEventService,
    vanity_tax: Arc<PersistentVanityTaxService>,
    discord: Arc<dyn TriviaDiscordPort>,
    reminder_hooks: Option<ReminderHooks>,
    catalog: Arc<TriviaCatalog>,
    image_cache: Arc<TriviaImageCache>,
    random: Mutex<FastrandTriviaRandom>,
    sessions: Mutex<TriviaSessions>,
    command_cooldowns: Mutex<BTreeMap<i64, Instant>>,
    config: TriviaRuntimeConfig,
}

type TriviaSessions = BTreeMap<(i64, i64), LiveTriviaSession>;
type TriviaSessionsGuard<'a> = std::sync::MutexGuard<'a, TriviaSessions>;

struct LiveTriviaSession {
    user_id: i64,
    guild_id: i64,
    user_display_name: String,
    user_avatar_url: Option<String>,
    streak: i64,
    total_jc: i64,
    recent_categories: Vec<&'static str>,
    current: LiveQuestion,
    previous_message: Option<InteractionMessageReceipt>,
    status: QuestionStatus,
    timeout: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct LiveQuestion {
    question: TriviaQuestion,
    number: i64,
    message: Option<InteractionMessageReceipt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuestionStatus {
    Preparing,
    Active,
    Settling,
}

#[derive(Clone)]
struct SessionSnapshot {
    user_id: i64,
    guild_id: i64,
    user_display_name: String,
    user_avatar_url: Option<String>,
    streak: i64,
    total_jc: i64,
    recent_categories: Vec<&'static str>,
    current: LiveQuestion,
    previous_message: Option<InteractionMessageReceipt>,
}

impl From<&LiveTriviaSession> for SessionSnapshot {
    fn from(session: &LiveTriviaSession) -> Self {
        Self {
            user_id: session.user_id,
            guild_id: session.guild_id,
            user_display_name: session.user_display_name.clone(),
            user_avatar_url: session.user_avatar_url.clone(),
            streak: session.streak,
            total_jc: session.total_jc,
            recent_categories: session.recent_categories.clone(),
            current: session.current.clone(),
            previous_message: session.previous_message,
        }
    }
}

struct TriviaInteractionHandler {
    state: Arc<TriviaRuntimeState>,
}

#[async_trait]
impl InteractionHandler for TriviaInteractionHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let result = match request {
            InteractionRequest::Command { ref name, .. } if name == "trivia" => {
                self.state.start_command(request, responder).await
            }
            InteractionRequest::Command { ref name, .. } if name == "trivia-reset-cooldown" => {
                self.state.reset_command(request, responder).await
            }
            InteractionRequest::Component { .. } => {
                self.state.answer_component(request, responder).await
            }
            InteractionRequest::Command { name, .. } => {
                Err(format!("trivia handler received command {name:?}"))
            }
            InteractionRequest::Autocomplete { .. } | InteractionRequest::Modal { .. } => {
                Err("trivia extension received an unsupported interaction".to_owned())
            }
        };
        result.map_err(Into::into)
    }
}

impl TriviaRuntimeState {
    async fn start_command(
        self: &Arc<Self>,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Command {
            user_id,
            user_display_name,
            guild_id,
            channel_id,
            member_permissions,
            ..
        } = request
        else {
            return Err("invalid trivia command payload".to_owned());
        };
        let user_id = signed_id(user_id, "user")?;
        let guild_id = guild_id.map_or(Ok(0), |value| signed_id(value, "guild"))?;
        let channel_id = channel_id
            .map(|value| signed_id(value, "channel"))
            .transpose()?;

        if let Some(retry_after) = self.take_command_rate_limit(user_id)? {
            return respond_ephemeral(
                &responder,
                format!("Slow down! Try again in {retry_after:.0}s."),
            )
            .await;
        }
        if !self
            .require_gamba(user_id, guild_id, channel_id, &responder)
            .await?
        {
            return Ok(());
        }
        if self.session_is_active(user_id, guild_id)? {
            return respond_ephemeral(
                &responder,
                "You already have an active trivia session! Finish it first.",
            )
            .await;
        }

        let trivia = self.trivia.clone();
        let registered = tokio::task::spawn_blocking(move || {
            trivia
                .get_balance(user_id, Some(guild_id))
                .map(|balance| balance.is_some())
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("trivia registration lookup task failed: {error}"))??;
        if !registered {
            return respond_ephemeral(
                &responder,
                "You must be registered to play trivia. Use `/player register` first.",
            )
            .await;
        }

        let is_admin = self.is_admin(user_id, member_permissions);
        let now = unix_timestamp()?;
        let mut claimed = false;
        if !is_admin {
            let trivia = self.trivia.clone();
            let cooldown = self.config.cooldown_seconds;
            claimed = tokio::task::spawn_blocking(move || {
                trivia
                    .try_claim_trivia_session(user_id, Some(guild_id), now, cooldown)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| format!("trivia cooldown claim task failed: {error}"))??;
            if !claimed {
                let trivia = self.trivia.clone();
                let last = tokio::task::spawn_blocking(move || {
                    trivia
                        .get_last_trivia_session(user_id, Some(guild_id))
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("trivia cooldown read task failed: {error}"))??;
                let message = last.map_or_else(
                    || "Trivia is on cooldown.".to_owned(),
                    |last| {
                        let remaining = last + self.config.cooldown_seconds - now;
                        let hours = remaining / 3_600;
                        let minutes = (remaining % 3_600) / 60;
                        format!(
                            "Trivia is on cooldown! Next session available in **{hours}h {minutes}m**."
                        )
                    },
                );
                return respond_ephemeral(&responder, message).await;
            }
        } else if let Some(reminders) = &self.reminder_hooks {
            reminders
                .schedule_trivia(user_id, guild_id, now + self.config.cooldown_seconds)
                .await?;
        }

        if responder.defer(false).await.is_err() {
            if claimed {
                let trivia = self.trivia.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    trivia.reset_trivia_cooldown(user_id, Some(guild_id))
                })
                .await;
            }
            return Ok(());
        }

        let placeholder = TriviaQuestion {
            text: String::new(),
            options: Vec::new(),
            correct_index: 0,
            difficulty: cama_app::trivia_questions::Difficulty::Easy,
            image_url: None,
            category: "",
            explanation: None,
        };
        self.sessions_lock()?.insert(
            (user_id, guild_id),
            LiveTriviaSession {
                user_id,
                guild_id,
                user_display_name: user_display_name.clone(),
                user_avatar_url: None,
                streak: 0,
                total_jc: 0,
                recent_categories: Vec::new(),
                current: LiveQuestion {
                    question: placeholder,
                    number: 1,
                    message: None,
                },
                previous_message: None,
                status: QuestionStatus::Preparing,
                timeout: None,
            },
        );

        let preparation = self.prepare_question(0, Vec::new());
        let avatar = self.discord.trivia_user_avatar_url(user_id, guild_id);
        let (prepared, avatar) = tokio::join!(preparation, avatar);
        let avatar = avatar.unwrap_or_else(|error| {
            debug!(user_id, %error, "trivia author avatar lookup failed");
            None
        });
        let Some(prepared) = prepared? else {
            responder
                .followup(
                    InteractionResponse::message(
                        "Failed to generate a trivia question. Try again later.",
                    )
                    .ephemeral(),
                )
                .await
                .map_err(|error| error.to_string())?;
            self.end_session(user_id, guild_id).await;
            return Ok(());
        };
        let response = question_response(
            &prepared,
            1,
            0,
            0,
            &user_display_name,
            avatar.as_deref(),
            self.config.answer_timeout,
            user_id,
        );
        let receipt = responder
            .followup_with_receipt(response)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "trivia follow-up did not return a message receipt".to_owned())?;
        {
            let mut sessions = self.sessions_lock()?;
            let Some(session) = sessions.get_mut(&(user_id, guild_id)) else {
                return Ok(());
            };
            session.user_avatar_url = avatar;
            session.current = LiveQuestion {
                question: prepared.question,
                number: 1,
                message: Some(receipt),
            };
            session.status = QuestionStatus::Active;
        }
        self.arm_timeout(user_id, guild_id, 1)?;
        Ok(())
    }

    async fn reset_command(
        self: &Arc<Self>,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Command {
            user_id,
            guild_id,
            member_permissions,
            options,
            ..
        } = request
        else {
            return Err("invalid trivia reset payload".to_owned());
        };
        let actor_id = signed_id(user_id, "user")?;
        if !self.is_admin(actor_id, member_permissions) {
            return respond_ephemeral(
                &responder,
                "You need admin permissions to use this command.",
            )
            .await;
        }
        let (target_id, target_name) = user_option(&options)?;
        let guild_id = guild_id.map_or(Ok(0), |value| signed_id(value, "guild"))?;
        let trivia = self.trivia.clone();
        let reset = tokio::task::spawn_blocking(move || {
            trivia
                .reset_trivia_cooldown(target_id, Some(guild_id))
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("trivia cooldown reset task failed: {error}"))??;
        let message = if reset {
            format!("Trivia cooldown reset for **{target_name}**.")
        } else {
            format!("No player found for **{target_name}** (not registered or no cooldown).")
        };
        respond_ephemeral(&responder, message).await
    }

    async fn answer_component(
        self: &Arc<Self>,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Component {
            interaction_id,
            custom_id,
            user_id,
            guild_id,
            ..
        } = request
        else {
            return Err("invalid trivia component payload".to_owned());
        };
        let parsed = parse_component_id(&custom_id)?;
        let actor_id = signed_id(user_id, "user")?;
        if actor_id != parsed.user_id {
            return respond_ephemeral(&responder, "This isn't your trivia session!").await;
        }
        let guild_id = guild_id.map_or(Ok(0), |value| signed_id(value, "guild"))?;
        let snapshot = {
            let mut sessions = self.sessions_lock()?;
            match sessions.get_mut(&(parsed.user_id, guild_id)) {
                None => None,
                Some(session)
                    if session.current.number != parsed.question_number
                        || session.status != QuestionStatus::Active =>
                {
                    Some(None)
                }
                Some(session) => {
                    session.status = QuestionStatus::Settling;
                    if let Some(timeout) = session.timeout.take() {
                        timeout.abort();
                    }
                    Some(Some(SessionSnapshot::from(&*session)))
                }
            }
        };
        let Some(snapshot) = snapshot else {
            return respond_ephemeral(&responder, "This trivia question is no longer active.")
                .await;
        };
        let Some(snapshot) = snapshot else {
            responder
                .defer(false)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(());
        };

        let pets = self.pets.clone();
        let source_key = format!("trivia-answer:{interaction_id}");
        let occurred_at = unix_timestamp()?;
        tokio::task::spawn_blocking(move || {
            pets.record_activity(
                parsed.user_id,
                Some(guild_id),
                PetActivity::TriviaCompleted,
                &source_key,
                occurred_at,
            )
            .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("trivia pet-activity task failed: {error}"))??;

        if parsed.choice == snapshot.current.question.correct_index {
            self.settle_correct(snapshot, responder).await
        } else {
            self.settle_wrong(snapshot, responder).await
        }
    }

    async fn settle_correct(
        self: &Arc<Self>,
        mut snapshot: SessionSnapshot,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let state = Arc::clone(self);
        let user_id = snapshot.user_id;
        let guild_id = snapshot.guild_id;
        let question_number = snapshot.current.number;
        let category = snapshot.current.question.category;
        let prior_streak = snapshot.streak;
        let award = tokio::task::spawn_blocking(move || {
            state.calculate_and_credit_award(user_id, guild_id, prior_streak + 1, question_number)
        })
        .await
        .map_err(|error| format!("trivia reward task failed: {error}"))?;
        snapshot.streak += 1;
        snapshot.total_jc += award;
        snapshot.recent_categories.push(category);
        {
            let mut sessions = self.sessions_lock()?;
            let Some(session) = sessions.get_mut(&(user_id, guild_id)) else {
                return Ok(());
            };
            session.streak = snapshot.streak;
            session.total_jc = snapshot.total_jc;
            session.recent_categories = snapshot.recent_categories.clone();
        }

        let delete = self.delete_previous(snapshot.previous_message);
        let edit = responder.update(correct_response(&snapshot, award));
        let prepare = self.prepare_question(snapshot.streak, snapshot.recent_categories.clone());
        let ((), edit_result, prepared_result) = tokio::join!(delete, edit, prepare);
        if let Err(error) = edit_result
            && !error.is_not_found()
        {
            return Err(error.to_string());
        }
        {
            let mut sessions = self.sessions_lock()?;
            let Some(session) = sessions.get_mut(&(user_id, guild_id)) else {
                return Ok(());
            };
            session.previous_message = snapshot.current.message;
        }
        let prepared = prepared_result?;
        let Some(prepared) = prepared else {
            let _ = responder.followup(no_more_response(&snapshot)).await;
            self.end_session(user_id, guild_id).await;
            return Ok(());
        };

        let next_number = question_number + 1;
        let response = question_response(
            &prepared,
            next_number,
            snapshot.streak,
            snapshot.total_jc,
            &snapshot.user_display_name,
            snapshot.user_avatar_url.as_deref(),
            self.config.answer_timeout,
            user_id,
        );
        let receipt = match responder.followup_with_receipt(response).await {
            Ok(Some(receipt)) => receipt,
            Ok(None) => {
                warn!(
                    user_id,
                    guild_id, "next trivia follow-up returned no receipt"
                );
                self.end_session(user_id, guild_id).await;
                return Ok(());
            }
            Err(error) => {
                warn!(user_id, guild_id, %error, "failed to send next trivia question");
                self.end_session(user_id, guild_id).await;
                return Ok(());
            }
        };
        {
            let mut sessions = self.sessions_lock()?;
            let Some(session) = sessions.get_mut(&(user_id, guild_id)) else {
                return Ok(());
            };
            session.current = LiveQuestion {
                question: prepared.question,
                number: next_number,
                message: Some(receipt),
            };
            session.status = QuestionStatus::Active;
        }
        self.arm_timeout(user_id, guild_id, next_number)
    }

    async fn settle_wrong(
        self: &Arc<Self>,
        snapshot: SessionSnapshot,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let delete = self.delete_previous(snapshot.previous_message);
        let edit = responder.update(game_over_response(&snapshot, false));
        let ((), edit_result) = tokio::join!(delete, edit);
        if let Err(error) = edit_result
            && !error.is_not_found()
        {
            return Err(error.to_string());
        }
        self.end_session(snapshot.user_id, snapshot.guild_id).await;
        Ok(())
    }

    fn calculate_and_credit_award(
        &self,
        user_id: i64,
        guild_id: i64,
        new_streak: i64,
        question_number: i64,
    ) -> i64 {
        let mut streak_bonus = streak_bonus_for_streak(new_streak);
        let mut effects = None;
        if streak_bonus > 0 {
            match self.active_mana_effects(user_id, guild_id) {
                Ok(active) => {
                    if active.trivia_payout_multiplier != 1.0 {
                        streak_bonus =
                            ((streak_bonus as f64 * active.trivia_payout_multiplier) as i64).max(1);
                    }
                    if active.trivia_streak_bonus > 0 {
                        streak_bonus += active.trivia_streak_bonus;
                    }
                    effects = Some(active);
                }
                Err(error) => {
                    warn!(user_id, guild_id, %error, "failed to apply trivia mana effects")
                }
            }
            match self.trivia.get_balance(user_id, Some(guild_id)) {
                Ok(Some(balance)) if balance <= 0 && self.config.bankrupt_multiplier != 1.0 => {
                    streak_bonus =
                        ((streak_bonus as f64 * self.config.bankrupt_multiplier) as i64).max(1);
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(user_id, guild_id, %error, "failed to apply trivia bankrupt multiplier")
                }
            }
            let gross_streak_bonus = streak_bonus;
            match self
                .bankruptcy
                .apply_penalty_to_winnings(user_id, streak_bonus, Some(guild_id))
            {
                Ok(penalty) => streak_bonus = penalty.penalized,
                Err(error) => {
                    warn!(user_id, guild_id, %error, "failed to apply trivia bankruptcy debuff")
                }
            }
            streak_bonus -= self
                .vanity_tax
                .calculate_tax(user_id, guild_id, gross_streak_bonus);
        }

        let raw = self.config.reward_per_question + streak_bonus;
        let scaled = scale_minigame_jc_delta(raw as f64, self.config.minigame_scale);
        let now = unix_timestamp().unwrap_or_default();
        let mut reward = match self.economy_events.adjust_reward_at(guild_id, scaled, now) {
            Ok(adjusted) => adjusted,
            Err(error) => {
                warn!(user_id, guild_id, %error, "failed to apply daily reward event; using scaled trivia reward");
                scaled
            }
        };

        if let Some(effects) = effects
            && effects.plains_tithe_rate > 0.0
            && reward > 1
        {
            let tithe = ((reward as f64 * effects.plains_tithe_rate) as i64).max(1);
            if tithe < reward {
                let context = LedgerContext {
                    source: Some("trivia".to_owned()),
                    related_type: Some("plains_tithe".to_owned()),
                    reason: Some("trivia plains tithe reserve credit".to_owned()),
                    metadata: Some(format!(
                        r#"{{"tithe": {tithe}, "net_jc_before_tithe": {reward}}}"#
                    )),
                    ..LedgerContext::default()
                };
                match self
                    .loans
                    .add_to_nonprofit_fund(Some(guild_id), tithe, Some(&context))
                {
                    Ok(_) => reward -= tithe,
                    Err(error) => {
                        warn!(user_id, guild_id, %error, "trivia tithe transfer failed; tithe skipped")
                    }
                }
            }
        }
        if reward <= 0 {
            return reward;
        }
        match self
            .trivia
            .credit_trivia_reward(user_id, Some(guild_id), reward, question_number)
        {
            Ok(_) => reward,
            Err(error) => {
                warn!(user_id, guild_id, %error, "failed to award trivia JC");
                0
            }
        }
    }

    fn active_mana_effects(&self, user_id: i64, guild_id: i64) -> Result<ManaEffects, String> {
        let row = self
            .mana
            .get_mana(user_id, Some(guild_id))
            .map_err(|error| error.to_string())?;
        let record = row.map(|row| ManaRecord {
            discord_id: user_id,
            guild_id,
            land: Some(row.current_land.clone()),
            color: cama_app::mana_effects_service::color_for_land(&row.current_land)
                .map(str::to_owned),
            assigned_date: row.assigned_date,
            consumed: row.consumed_today,
        });
        Ok(
            effects_from_record(record.as_ref(), &pacific_mana_day(unix_timestamp()?)?)
                .into_inner(),
        )
    }

    async fn prepare_question(
        self: &Arc<Self>,
        streak: i64,
        recent_categories: Vec<&'static str>,
    ) -> Result<Option<PreparedRuntimeQuestion>, String> {
        let state = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let question = {
                let mut random = state
                    .random
                    .lock()
                    .map_err(|_| "trivia random lock was poisoned".to_owned())?;
                generate_question(
                    &state.catalog,
                    streak,
                    &recent_categories,
                    QUESTION_GENERATION_RETRIES,
                    &mut *random,
                )
            };
            Ok(question.map(|question| {
                let image = question.image_url.as_deref().and_then(|url| {
                    state
                        .image_cache
                        .get_or_download(url)
                        .map_err(|error| {
                            debug!(%error, url, "failed to cache trivia image");
                            error
                        })
                        .ok()
                });
                PreparedRuntimeQuestion { question, image }
            }))
        })
        .await
        .map_err(|error| format!("trivia question preparation task failed: {error}"))?
    }

    fn arm_timeout(
        self: &Arc<Self>,
        user_id: i64,
        guild_id: i64,
        question: i64,
    ) -> Result<(), String> {
        let state = Arc::clone(self);
        let delay = self.config.answer_timeout;
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            state.timeout_question(user_id, guild_id, question).await;
        });
        let mut sessions = self.sessions_lock()?;
        if let Some(session) = sessions.get_mut(&(user_id, guild_id))
            && session.current.number == question
            && session.status == QuestionStatus::Active
        {
            session.timeout = Some(handle);
        } else {
            handle.abort();
        }
        Ok(())
    }

    async fn timeout_question(self: Arc<Self>, user_id: i64, guild_id: i64, question: i64) {
        let snapshot = {
            let Ok(mut sessions) = self.sessions_lock() else {
                return;
            };
            let Some(session) = sessions.get_mut(&(user_id, guild_id)) else {
                return;
            };
            if session.current.number != question || session.status != QuestionStatus::Active {
                return;
            }
            session.status = QuestionStatus::Settling;
            SessionSnapshot::from(&*session)
        };
        let delete = self.delete_previous(snapshot.previous_message);
        let edit = async {
            let Some(message) = snapshot.current.message else {
                return Ok(());
            };
            self.discord
                .edit_trivia_message(
                    message.channel_id,
                    message.message_id,
                    game_over_response(&snapshot, true),
                )
                .await
        };
        let ((), edit_result) = tokio::join!(delete, edit);
        if let Err(error) = edit_result
            && !error.is_not_found()
        {
            warn!(user_id, guild_id, %error, "trivia timeout result edit failed");
            return;
        }
        self.end_session(user_id, guild_id).await;
    }

    async fn delete_previous(&self, message: Option<InteractionMessageReceipt>) {
        let Some(message) = message else {
            return;
        };
        let _ = self
            .discord
            .delete_trivia_message(message.channel_id, message.message_id)
            .await;
    }

    async fn end_session(&self, user_id: i64, guild_id: i64) {
        let session = match self.sessions_lock() {
            Ok(mut sessions) => sessions.remove(&(user_id, guild_id)),
            Err(error) => {
                warn!(user_id, guild_id, %error, "could not remove trivia session");
                None
            }
        };
        let Some(mut session) = session else {
            return;
        };
        if let Some(timeout) = session.timeout.take() {
            timeout.abort();
        }
        if session.streak <= 0 {
            return;
        }
        let trivia = self.trivia.clone();
        let streak = session.streak;
        let total_jc = session.total_jc;
        let played_at = unix_timestamp().unwrap_or_default();
        match tokio::task::spawn_blocking(move || {
            trivia.record_trivia_session(user_id, Some(guild_id), streak, total_jc, played_at)
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => warn!(user_id, guild_id, %error, "failed to record trivia session"),
            Err(error) => warn!(user_id, guild_id, %error, "trivia session recording task failed"),
        }
    }

    async fn require_gamba(
        &self,
        user_id: i64,
        guild_id: i64,
        channel_id: Option<i64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<bool, String> {
        let allowed = match channel_id {
            Some(channel_id) => self
                .discord
                .trivia_channel_is_gamba(guild_id, channel_id)
                .await
                .unwrap_or(false),
            None => false,
        };
        if allowed {
            return Ok(true);
        }
        let trivia = self.trivia.clone();
        let _ =
            tokio::task::spawn_blocking(move || trivia.adjust_balance(user_id, Some(guild_id), -1))
                .await;
        respond_ephemeral(responder, WRONG_CHANNEL_MESSAGE).await?;
        Ok(false)
    }

    fn is_admin(&self, user_id: i64, permissions: Option<u64>) -> bool {
        self.config.admin_user_ids.contains(&user_id)
            || permissions.is_some_and(|permissions| {
                permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            })
    }

    fn session_is_active(&self, user_id: i64, guild_id: i64) -> Result<bool, String> {
        Ok(self.sessions_lock()?.get(&(user_id, guild_id)).is_some())
    }

    fn take_command_rate_limit(&self, user_id: i64) -> Result<Option<f64>, String> {
        let now = Instant::now();
        let mut cooldowns = self
            .command_cooldowns
            .lock()
            .map_err(|_| "trivia command cooldown lock was poisoned".to_owned())?;
        if let Some(started) = cooldowns.get(&user_id) {
            let elapsed = now.saturating_duration_since(*started);
            if elapsed < COMMAND_RATE_LIMIT {
                return Ok(Some((COMMAND_RATE_LIMIT - elapsed).as_secs_f64()));
            }
        }
        cooldowns.insert(user_id, now);
        cooldowns.retain(|_, started| now.saturating_duration_since(*started) < COMMAND_RATE_LIMIT);
        Ok(None)
    }

    fn sessions_lock(&self) -> Result<TriviaSessionsGuard<'_>, String> {
        self.sessions
            .lock()
            .map_err(|_| "trivia session lock was poisoned".to_owned())
    }
}

struct PreparedRuntimeQuestion {
    question: TriviaQuestion,
    image: Option<CachedTriviaImage>,
}

#[derive(Debug)]
struct FastrandTriviaRandom {
    random: fastrand::Rng,
}

impl FastrandTriviaRandom {
    fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64);
        Self {
            random: fastrand::Rng::with_seed(seed),
        }
    }
}

impl TriviaRandom for FastrandTriviaRandom {
    fn next_index(&mut self, upper: usize) -> usize {
        self.random.usize(0..upper)
    }
}

struct ParsedComponentId {
    user_id: i64,
    question_number: i64,
    choice: usize,
}

fn parse_component_id(custom_id: &str) -> Result<ParsedComponentId, String> {
    let mut parts = custom_id.split('_');
    if parts.next() != Some("trivia") {
        return Err(format!("invalid trivia component ID {custom_id:?}"));
    }
    let user_id = parts
        .next()
        .ok_or_else(|| format!("invalid trivia component ID {custom_id:?}"))?
        .parse()
        .map_err(|_| format!("invalid trivia component user in {custom_id:?}"))?;
    let question_number = parts
        .next()
        .ok_or_else(|| format!("invalid trivia component ID {custom_id:?}"))?
        .parse()
        .map_err(|_| format!("invalid trivia component question in {custom_id:?}"))?;
    let choice = parts
        .next()
        .ok_or_else(|| format!("invalid trivia component ID {custom_id:?}"))?
        .parse::<usize>()
        .map_err(|_| format!("invalid trivia component choice in {custom_id:?}"))?;
    if parts.next().is_some() || choice >= 4 || question_number <= 0 {
        return Err(format!("invalid trivia component ID {custom_id:?}"));
    }
    Ok(ParsedComponentId {
        user_id,
        question_number,
        choice,
    })
}

fn user_option(options: &[InteractionOption]) -> Result<(i64, String), String> {
    options
        .iter()
        .find_map(|option| match &option.value {
            InteractionValue::User {
                id, display_name, ..
            } if option.name == "user" => signed_id(*id, "target user")
                .ok()
                .map(|id| (id, display_name.clone().unwrap_or_else(|| id.to_string()))),
            _ => None,
        })
        .ok_or_else(|| "trivia reset requires a user".to_owned())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the Python embed contract keeps session counters, identity, timeout, and owner explicit"
)]
fn question_response(
    prepared: &PreparedRuntimeQuestion,
    question_number: i64,
    streak: i64,
    total_jc: i64,
    user_display_name: &str,
    user_avatar_url: Option<&str>,
    timeout: Duration,
    user_id: i64,
) -> InteractionResponse {
    let question = &prepared.question;
    let options = question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| format!("**{}.** {option}", option_label(index)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut embed = InteractionEmbed::titled(format!("Dota 2 Trivia — Question {question_number}"))
        .description(&question.text)
        .color(difficulty_color(question.difficulty.as_str()))
        .author(user_display_name, user_avatar_url.map(str::to_owned))
        .field("Options", options, false)
        .footer(format!(
            "Streak: {streak} | Difficulty: {} | JC earned: {total_jc} | {}s to answer",
            capitalize(question.difficulty.as_str()),
            timeout.as_secs()
        ));
    let mut response = InteractionResponse::message("");
    if let Some(image) = &prepared.image {
        embed = embed.thumbnail(format!("attachment://{}", image.filename));
        response = response.attachment(InteractionAttachment::bytes(
            &image.filename,
            image.bytes.clone(),
        ));
    } else if let Some(image_url) = &question.image_url {
        embed = embed.thumbnail(image_url);
    }
    let buttons = question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            InteractionButton::new(
                format!("trivia_{user_id}_{question_number}_{index}"),
                format!(
                    "{}. {}",
                    option_label(index),
                    option.chars().take(72).collect::<String>()
                ),
            )
        })
        .collect::<Vec<_>>();
    response.embed(embed).action_rows(vec![
        InteractionActionRow::buttons(buttons[..buttons.len().min(2)].to_vec()),
        InteractionActionRow::buttons(buttons[buttons.len().min(2)..].to_vec()),
    ])
}

fn correct_response(snapshot: &SessionSnapshot, awarded: i64) -> InteractionResponse {
    let question = &snapshot.current.question;
    let answer = &question.options[question.correct_index];
    InteractionResponse::message("").embed(
        InteractionEmbed::titled(format!(
            "Question {} — Correct! +{awarded} {JOPACOIN_EMOTE}",
            snapshot.current.number
        ))
        .description(format!(
            "**{}.** {answer}",
            option_label(question.correct_index)
        ))
        .color(0x43_A0_47)
        .author(
            &snapshot.user_display_name,
            snapshot.user_avatar_url.clone(),
        )
        .footer(format!(
            "Streak: {} | JC earned: {}",
            snapshot.streak, snapshot.total_jc
        )),
    )
}

fn game_over_response(snapshot: &SessionSnapshot, timed_out: bool) -> InteractionResponse {
    let question = &snapshot.current.question;
    let mut description = format!(
        "The correct answer was: **{}.** {}",
        option_label(question.correct_index),
        question.options[question.correct_index]
    );
    if let Some(explanation) = &question.explanation {
        description.push_str(&format!("\n\n*{explanation}*"));
    }
    let title = if timed_out {
        format!("Question {} — Time's up!", snapshot.current.number)
    } else {
        format!("Question {} — Wrong!", snapshot.current.number)
    };
    InteractionResponse::message("").embed(
        InteractionEmbed::titled(title)
            .description(description)
            .color(0xE5_39_35)
            .author(
                &snapshot.user_display_name,
                snapshot.user_avatar_url.clone(),
            )
            .field(
                "Game Over",
                game_over_summary(snapshot.streak, snapshot.total_jc),
                false,
            ),
    )
}

fn no_more_response(snapshot: &SessionSnapshot) -> InteractionResponse {
    InteractionResponse::message("").embed(
        InteractionEmbed::titled("Trivia — No more questions!")
            .color(0xE5_39_35)
            .author(
                &snapshot.user_display_name,
                snapshot.user_avatar_url.clone(),
            )
            .field(
                "Game Over",
                game_over_summary(snapshot.streak, snapshot.total_jc),
                false,
            ),
    )
}

fn game_over_summary(streak: i64, total_jc: i64) -> String {
    let mut summary =
        format!("Final streak: **{streak}**\nTotal earned: **{total_jc}** {JOPACOIN_EMOTE}");
    if streak >= 15 {
        summary.push_str("\nDota encyclopedia! Incredible run!");
    } else if streak >= 10 {
        summary.push_str("\nYou're a beast!");
    } else if streak >= 5 {
        summary.push_str("\nImpressive knowledge!");
    }
    summary
}

const fn streak_bonus_for_streak(streak: i64) -> i64 {
    if streak == 10 {
        2
    } else if streak == 3 || streak == 6 || (streak > 10 && (streak - 10) % 4 == 0) {
        1
    } else {
        0
    }
}

const fn difficulty_color(difficulty: &str) -> u32 {
    match difficulty.as_bytes() {
        b"easy" => 0x43_A0_47,
        b"medium" => 0xFF_A0_00,
        b"hard" => 0xE5_39_35,
        b"challenging" => 0x7B_1F_A2,
        _ => 0x9E_9E_9E,
    }
}

fn option_label(index: usize) -> char {
    ['A', 'B', 'C', 'D'].get(index).copied().unwrap_or('?')
}

fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), String> {
    responder
        .respond(InteractionResponse::message(content).ephemeral())
        .await
        .map_err(|error| error.to_string())
}

fn unix_timestamp() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())
        .and_then(|duration| {
            i64::try_from(duration.as_secs())
                .map_err(|_| "system clock exceeds SQLite INTEGER".to_owned())
        })
}

fn pacific_mana_day(timestamp: i64) -> Result<String, String> {
    let utc = chrono::DateTime::from_timestamp(timestamp, 0)
        .ok_or_else(|| format!("invalid Unix timestamp {timestamp}"))?;
    let year = utc.year();
    let start = nth_weekday(year, 3, Weekday::Sun, 2)
        .and_hms_opt(10, 0, 0)
        .expect("valid DST transition")
        .and_utc()
        .timestamp();
    let end = nth_weekday(year, 11, Weekday::Sun, 1)
        .and_hms_opt(9, 0, 0)
        .expect("valid DST transition")
        .and_utc()
        .timestamp();
    let offset = if (start..end).contains(&timestamp) {
        -7 * 3_600
    } else {
        -8 * 3_600
    };
    let local = chrono::DateTime::from_timestamp(timestamp + offset, 0)
        .ok_or_else(|| format!("invalid Pacific timestamp {timestamp}"))?
        .naive_utc();
    let effective = if local.hour() < 4 {
        local - ChronoDuration::days(1)
    } else {
        local
    };
    Ok(effective.format("%Y-%m-%d").to_string())
}

fn nth_weekday(year: i32, month: u32, weekday: Weekday, occurrence: u32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid fixed month");
    let delta = (7 + i64::from(weekday.num_days_from_sunday())
        - i64::from(first.weekday().num_days_from_sunday()))
        % 7;
    first + ChronoDuration::days(delta + 7 * i64::from(occurrence - 1))
}

struct TriviaImageCacheWarmObserver {
    catalog: Arc<TriviaCatalog>,
    image_cache: Arc<TriviaImageCache>,
    task: Mutex<Option<JoinHandle<()>>>,
}

#[async_trait]
impl GatewayEventObserver for TriviaImageCacheWarmObserver {
    fn name(&self) -> &'static str {
        CACHE_OBSERVER_NAME
    }

    async fn ready_recovery(&self, context: ReadyRecoveryContext) -> ReadyRecoveryReport {
        let mut task = match self.task.lock() {
            Ok(task) => task,
            Err(_) => {
                return ReadyRecoveryReport::empty(CACHE_OBSERVER_NAME, context.guild_ids().len());
            }
        };
        if task.as_ref().is_some_and(|task| !task.is_finished()) {
            return ReadyRecoveryReport::empty(CACHE_OBSERVER_NAME, context.guild_ids().len());
        }
        let catalog = Arc::clone(&self.catalog);
        let image_cache = Arc::clone(&self.image_cache);
        *task = Some(tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || image_cache.warm_catalog(&catalog)).await {
                Ok(Ok(report)) => info!(
                    newly_cached = report.newly_cached,
                    total_urls = report.total_urls,
                    "trivia image cache warm completed"
                ),
                Ok(Err(error)) => warn!(%error, "trivia image cache warm failed"),
                Err(error) => warn!(%error, "trivia image cache warm task failed"),
            }
        }));
        ReadyRecoveryReport::empty(CACHE_OBSERVER_NAME, context.guild_ids().len())
    }
}

#[cfg(test)]
#[path = "trivia_provider/tests.rs"]
mod tests;
