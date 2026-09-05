//! Production Discord provider for the daily guild `/playertrivia` game.
//!
//! Generated questions are frozen in migrated SQLite before Discord displays
//! them. Component callbacks reload that snapshot instead of trusting process
//! memory, so a Rust restart can continue a still-live question and stale or
//! duplicate clicks cannot issue a second reward. Runtime construction does
//! not create schema; startup migration authority remains outside this module.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::economy_event_sqlite::SqliteEconomyEventService;
use cama_app::player_trivia_service as app;
use cama_app::service_container::PersistentVanityTaxService;
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::player_trivia as db;
use cama_db::predictions_repository::PredictionRepository;
use cama_domain::economy_scaling::scale_minigame_jc_delta;
use cama_domain::pet_evolution::PetActivity;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::ApplicationConfig;
use crate::discord_transport::{DiscordIdPlayerNameResolver, GuildPlayerNameResolver};
use crate::economy_events_worker::EconomyEventsWorkerConfig;
use crate::ids::signed_id;
use crate::profit_deductions::ProfitDeductionSource;
use crate::registration::{
    CommandSpec, ComponentRoute, InteractionActionRow, InteractionButton, InteractionEmbed,
    InteractionHandler, InteractionHandlerError, InteractionMessageReceipt, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionResponseError, RegistrationError,
    RegistrationProvider, RegistryBuilder,
};

const COMPONENT_PREFIX: &str = "player_trivia:";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const WRONG_CHANNEL_MESSAGE: &str = "The ancient spirits reject your offering... this ground is not consecrated. A single jopacoin dissolves into the ether as penance.";
const JOPACOIN_EMOTE: &str = "<:jopacoin:954159801049440297>";
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const COMMAND_RATE_LIMIT: Duration = Duration::from_secs(5);
const OPTION_LABELS: [&str; 4] = ["A", "B", "C", "D"];

#[async_trait]
pub trait PlayerTriviaDiscordPort: Send + Sync {
    /// True when the channel itself, or its thread parent, contains `gamba`.
    async fn player_trivia_channel_is_gamba(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<bool, String>;

    /// The current cached non-bot membership, or `None` when unavailable.
    fn player_trivia_non_bot_member_ids(
        &self,
        guild_id: i64,
    ) -> Result<Option<BTreeSet<i64>>, String>;

    /// Equivalent to discord.py's guild-aware `display_avatar.url`.
    async fn player_trivia_user_avatar_url(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<String>, String>;

    /// Edit a follow-up/channel-fallback receipt after its answer timer fires.
    async fn edit_player_trivia_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError>;
}

#[derive(Clone)]
pub struct PlayerTriviaRegistrationProvider {
    handler: Arc<PlayerTriviaInteractionHandler>,
}

impl PlayerTriviaRegistrationProvider {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        discord: Arc<dyn PlayerTriviaDiscordPort>,
    ) -> Self {
        let repository =
            RuntimePlayerTriviaRepository::new(database_path.as_ref(), config, vanity_tax);
        let service = Arc::new(app::PlayerTriviaService::new(Arc::new(repository.clone())));
        let event_config = EconomyEventsWorkerConfig::from_application_config(config).event;
        Self {
            handler: Arc::new(PlayerTriviaInteractionHandler {
                state: Arc::new(PlayerTriviaRuntimeState {
                    player_names: Arc::new(DiscordIdPlayerNameResolver),
                    repository,
                    service,
                    penalties: PredictionRepository::new(database_path.as_ref()),
                    pets: PetEvolutionRepository::new(database_path.as_ref()),
                    economy_events: Arc::new(SqliteEconomyEventService::new(
                        database_path.as_ref(),
                        event_config,
                    )),
                    discord,
                    config: PlayerTriviaRuntimeConfig::from_application_config(config),
                    command_cooldowns: Mutex::new(BTreeMap::new()),
                    timeouts: Mutex::new(BTreeMap::new()),
                }),
            }),
        }
    }

    #[must_use]
    pub fn with_player_names(mut self, player_names: Arc<dyn GuildPlayerNameResolver>) -> Self {
        let handler =
            Arc::get_mut(&mut self.handler).expect("new player-trivia provider owns its handler");
        Arc::get_mut(&mut handler.state)
            .expect("new player-trivia provider owns its runtime state")
            .player_names = player_names;
        self
    }
}

impl RegistrationProvider for PlayerTriviaRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "playertrivia".to_owned(),
            description: "Play today's trivia set about this server's player stats.".to_owned(),
            options: Vec::new(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

#[derive(Clone, Debug)]
struct PlayerTriviaRuntimeConfig {
    admin_user_ids: BTreeSet<i64>,
    answer_timeout: Duration,
    cooldown_seconds: i64,
    include_spicy: bool,
    minigame_scale: f64,
    question_count: usize,
    recent_days: i64,
    reward_per_correct: i64,
}

impl PlayerTriviaRuntimeConfig {
    fn from_application_config(config: &ApplicationConfig) -> Self {
        Self {
            admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
            answer_timeout: Duration::from_secs(
                u64::try_from(config.values.player_trivia_answer_timeout_seconds.max(0))
                    .unwrap_or(u64::MAX),
            ),
            cooldown_seconds: config.values.player_trivia_cooldown_seconds,
            include_spicy: config.values.player_trivia_include_spicy,
            minigame_scale: config.values.minigame_jc_delta_scale,
            question_count: usize::try_from(config.values.player_trivia_question_count.max(0))
                .unwrap_or(usize::MAX),
            recent_days: config.values.player_trivia_recent_days,
            reward_per_correct: config.values.player_trivia_reward_per_correct,
        }
    }
}

struct PlayerTriviaInteractionHandler {
    state: Arc<PlayerTriviaRuntimeState>,
}

struct PlayerTriviaRuntimeState {
    player_names: Arc<dyn GuildPlayerNameResolver>,
    repository: RuntimePlayerTriviaRepository,
    service: Arc<app::PlayerTriviaService<RuntimePlayerTriviaRepository>>,
    penalties: PredictionRepository,
    pets: PetEvolutionRepository,
    economy_events: Arc<SqliteEconomyEventService>,
    discord: Arc<dyn PlayerTriviaDiscordPort>,
    config: PlayerTriviaRuntimeConfig,
    command_cooldowns: Mutex<BTreeMap<i64, Instant>>,
    timeouts: Mutex<BTreeMap<(i64, i64), JoinHandle<()>>>,
}

#[async_trait]
impl InteractionHandler for PlayerTriviaInteractionHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let result = match request {
            InteractionRequest::Command { ref name, .. } if name == "playertrivia" => {
                self.state.command(request, responder).await
            }
            InteractionRequest::Component { .. } => self.state.component(request, responder).await,
            InteractionRequest::Command { name, .. } => {
                Err(format!("player-trivia handler received command {name:?}"))
            }
            InteractionRequest::Autocomplete { .. } | InteractionRequest::Modal { .. } => {
                Err("player-trivia extension received an unsupported interaction".to_owned())
            }
        };
        result.map_err(Into::into)
    }
}

impl PlayerTriviaRuntimeState {
    async fn render_player_name(&self, user_id: i64, guild_id: i64) -> String {
        self.player_names
            .names_for_guild(guild_id, &[user_id])
            .unwrap_or_default()
            .resolve(user_id)
    }

    async fn command(
        self: &Arc<Self>,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Command {
            user_id,
            user_display_name: _,
            guild_id,
            channel_id,
            member_permissions,
            ..
        } = request
        else {
            return Err("invalid player-trivia command payload".to_owned());
        };
        let user_id = signed_id(user_id, "user")?;
        // discord.py applies the command cooldown check before invoking the
        // guild-only callback wrapper, so a DM attempt consumes the same
        // per-user five-second bucket as a guild attempt.
        if let Some(retry_after) = self.take_command_rate_limit(user_id)? {
            return respond_ephemeral(
                &responder,
                format!("Slow down! Try again in {retry_after:.0}s."),
            )
            .await;
        }
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(&responder, GUILD_ONLY_MESSAGE).await;
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let channel_id = channel_id
            .map(|value| signed_id(value, "channel"))
            .transpose()?;

        if !self
            .require_gamba(user_id, guild_id, channel_id, &responder)
            .await?
        {
            return Ok(());
        }

        let repository = self.repository.clone();
        let registered = tokio::task::spawn_blocking(move || {
            repository
                .player_exists(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia registration task failed: {error}"))??;
        if !registered {
            return respond_ephemeral(
                &responder,
                "You must be registered to play. Use `/player register` first.",
            )
            .await;
        }

        let now = unix_timestamp()?;
        let repository = self.repository.clone();
        let active = tokio::task::spawn_blocking(move || {
            repository
                .active_session_for_player(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia active-session task failed: {error}"))??;
        if let Some(active) = active {
            let deadline = active
                .last_activity_at()
                .saturating_add(duration_seconds(self.config.answer_timeout));
            if now < deadline {
                return respond_ephemeral(
                    &responder,
                    "You already have an active player-trivia session!",
                )
                .await;
            }
            let repository = self.repository.clone();
            let session_id = active.session_id;
            if let Some(question) = active.next_question() {
                let question_number = question.question_number;
                let changed = tokio::task::spawn_blocking(move || {
                    repository
                        .timeout_session_if_question_unanswered(session_id, question_number, now)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("player-trivia stale-session task failed: {error}"))??;
                if !changed {
                    return respond_ephemeral(
                        &responder,
                        "You already have an active player-trivia session!",
                    )
                    .await;
                }
            }
        }

        let is_admin = self.is_admin(user_id, member_permissions);
        let repository = self.repository.clone();
        let last_started = tokio::task::spawn_blocking(move || {
            repository
                .get_last_session_started(user_id, guild_id)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia cooldown task failed: {error}"))??;
        if !is_admin
            && last_started
                .is_some_and(|last| now.saturating_sub(last) < self.config.cooldown_seconds)
        {
            let unlock = last_started
                .unwrap_or(now)
                .saturating_add(self.config.cooldown_seconds);
            return respond_ephemeral(
                &responder,
                format!("Player trivia is on cooldown! Your next set unlocks <t:{unlock}:R>."),
            )
            .await;
        }

        if responder.defer_thinking(false).await.is_err() {
            return Ok(());
        }
        let user_display_name = self.render_player_name(user_id, guild_id).await;
        let member_ids = self
            .discord
            .player_trivia_non_bot_member_ids(guild_id)
            .unwrap_or_else(|error| {
                debug!(guild_id, %error, "player-trivia member cache unavailable");
                None
            });
        let service = Arc::clone(&self.service);
        let count = self.config.question_count;
        let include_spicy = self.config.include_spicy;
        let recent_days = self.config.recent_days;
        let questions = match tokio::task::spawn_blocking(move || {
            service
                .generate_questions(
                    user_id,
                    guild_id,
                    member_ids.as_ref(),
                    count,
                    include_spicy,
                    recent_days,
                )
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia generation task failed: {error}"))?
        {
            Ok(questions) => questions,
            Err(error) => {
                warn!(guild_id, %error, "failed to generate player trivia");
                return followup_ephemeral(
                    &responder,
                    "❌ Couldn't build today's player-trivia set right now — try again in a moment.",
                )
                .await;
            }
        };
        let valid = questions.iter().all(valid_question);
        if questions.len() < self.config.question_count || !valid {
            return followup_ephemeral(
                &responder,
                format!(
                    "There isn't enough eligible server history to build today's {}-question set yet. No cooldown was used.",
                    self.config.question_count
                ),
            )
            .await;
        }
        let questions = questions
            .into_iter()
            .take(self.config.question_count)
            .collect::<Vec<_>>();
        let service = Arc::clone(&self.service);
        let frozen = questions.clone();
        let cooldown = self.config.cooldown_seconds;
        let session_id = match tokio::task::spawn_blocking(move || {
            service
                .try_start_session(user_id, guild_id, &frozen, now, cooldown, is_admin)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia session-start task failed: {error}"))?
        {
            Ok(Some(session_id)) => session_id,
            Ok(None) => {
                let repository = self.repository.clone();
                let last = tokio::task::spawn_blocking(move || {
                    repository
                        .get_last_session_started(user_id, guild_id)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("player-trivia race cooldown task failed: {error}"))??;
                let unlock = last
                    .unwrap_or(now)
                    .saturating_add(self.config.cooldown_seconds);
                return followup_ephemeral(
                    &responder,
                    format!(
                        "Another player-trivia run already claimed today's set. Your next set unlocks <t:{unlock}:R>."
                    ),
                )
                .await;
            }
            Err(error) => {
                warn!(%error, "failed to persist player-trivia session");
                return followup_ephemeral(
                    &responder,
                    "❌ Couldn't start today's player-trivia set right now — try again in a moment.",
                )
                .await;
            }
        };

        let avatar = self
            .discord
            .player_trivia_user_avatar_url(user_id, guild_id)
            .await
            .unwrap_or_else(|error| {
                debug!(user_id, guild_id, %error, "player-trivia avatar lookup failed");
                None
            });
        let first = &questions[0];
        let response = question_response(
            session_id,
            first,
            1,
            questions.len(),
            0,
            0,
            &user_display_name,
            avatar.as_deref(),
            None,
            self.config.answer_timeout,
        );
        let receipt = match responder.followup_with_receipt(response).await {
            Ok(Some(receipt)) => receipt,
            Ok(None) | Err(_) => {
                let repository = self.repository.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    repository.cancel_session_if_unanswered(session_id)
                })
                .await;
                return Ok(());
            }
        };
        self.arm_timeout(
            session_id,
            1,
            user_display_name,
            avatar,
            TimeoutTarget::Receipt(receipt),
        )
    }

    async fn component(
        self: &Arc<Self>,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let InteractionRequest::Component {
            custom_id,
            user_id,
            user_display_name: _,
            guild_id,
            ..
        } = request
        else {
            return Err("invalid player-trivia component payload".to_owned());
        };
        let parsed = parse_component(&custom_id)?;
        let user_id = signed_id(user_id, "user")?;
        let Some(guild_id) = guild_id else {
            return respond_ephemeral(&responder, GUILD_ONLY_MESSAGE).await;
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let repository = self.repository.clone();
        let session = tokio::task::spawn_blocking(move || {
            repository
                .get_session(parsed.session_id)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia component session task failed: {error}"))??;
        let Some(session) = session else {
            return respond_ephemeral(&responder, "That player-trivia session no longer exists.")
                .await;
        };
        if session.guild_id != guild_id || session.discord_id != user_id {
            return respond_ephemeral(&responder, "This isn't your player-trivia session!").await;
        }
        let next = session.next_question();
        if session.status != "active"
            || next.is_none_or(|question| question.question_number != parsed.question_number)
        {
            if responder.response_is_done() {
                return Ok(());
            }
            return responder
                .defer(false)
                .await
                .map_err(|error| error.to_string());
        }
        let question = next.expect("checked player-trivia next question");
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let user_display_name = self.render_player_name(user_id, guild_id).await;
        let question_for_display = convert_stored_question(&question.question);
        let now = unix_timestamp()?;
        let deadline = session
            .last_activity_at()
            .saturating_add(duration_seconds(self.config.answer_timeout));
        self.abort_timeout(session.session_id, question.question_number)?;
        let avatar = self
            .discord
            .player_trivia_user_avatar_url(user_id, guild_id)
            .await
            .unwrap_or(None);
        if now >= deadline {
            let repository = self.repository.clone();
            let session_id = session.session_id;
            let changed = tokio::task::spawn_blocking(move || {
                repository
                    .timeout_session_if_question_unanswered(session_id, parsed.question_number, now)
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| format!("player-trivia timeout task failed: {error}"))??;
            if !changed {
                return Ok(());
            }
            return responder
                .update(summary_response(
                    "Player Trivia — Time's up!",
                    &session,
                    &user_display_name,
                    avatar.as_deref(),
                    Some(answer_result(&question_for_display, false, 0, 0)),
                ))
                .await
                .map_err(|error| error.to_string());
        }

        let scaled = scale_minigame_jc_delta(
            self.config.reward_per_correct.max(0) as f64,
            self.config.minigame_scale,
        )
        .max(0);
        // adjust_reward_at reads SQLite, so it belongs on a blocking thread.
        let events = Arc::clone(&self.economy_events);
        let reward =
            tokio::task::spawn_blocking(move || events.adjust_reward_at(guild_id, scaled, now))
                .await
                .map_err(|error| error.to_string())?
                .map_or_else(
                    |error| {
                        warn!(guild_id, %error, "failed to apply daily player-trivia reward event");
                        scaled
                    },
                    |reward| reward.max(0),
                );
        let repository = self.repository.clone();
        let settlement = tokio::task::spawn_blocking(move || {
            repository.settle_answer(
                guild_id,
                parsed.session_id,
                parsed.question_number,
                parsed.selected_index,
                reward,
                now,
            )
        })
        .await
        .map_err(|error| format!("player-trivia settlement task failed: {error}"));
        let settlement = match settlement {
            Ok(Ok(Some(settlement))) => settlement,
            Ok(Ok(None)) => {
                // None also covers the benign duplicate-click race: two rapid
                // clicks both pass the pre-check and the loser lands here while
                // the winner's settlement stands. Killing a still-active
                // session there would end a healthy run and burn the daily
                // cooldown, so only a session that really is finished reports
                // the interruption.
                let repository = self.repository.clone();
                let session_id = session.session_id;
                let current = tokio::task::spawn_blocking(move || {
                    repository
                        .get_session(session_id)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| format!("player-trivia session reload failed: {error}"))??;
                if current.is_some_and(|current| current.status == "active") {
                    return Ok(());
                }
                self.finish_interrupted(session.session_id, now).await;
                return responder
                    .update(summary_response(
                        "Player Trivia — Interrupted",
                        &session,
                        &user_display_name,
                        avatar.as_deref(),
                        Some(
                            "That round was already settled or was no longer active. No duplicate reward was issued."
                                .to_owned(),
                        ),
                    ))
                    .await
                    .map_err(|error| error.to_string());
            }
            Ok(Err(error)) | Err(error) => {
                warn!(session_id = session.session_id, %error, "failed to settle player-trivia answer");
                self.finish_interrupted(session.session_id, now).await;
                return responder
                    .update(summary_response(
                        "Player Trivia — Interrupted",
                        &session,
                        &user_display_name,
                        avatar.as_deref(),
                        Some(
                            "❌ Couldn't save that answer right now — try again in a moment."
                                .to_owned(),
                        ),
                    ))
                    .await
                    .map_err(|error| error.to_string());
            }
        };

        let pets = self.pets.clone();
        let source_key = format!(
            "player-trivia:{}:{}",
            session.session_id, question.question_number
        );
        if let Err(error) = tokio::task::spawn_blocking(move || {
            pets.record_activity(
                user_id,
                Some(guild_id),
                PetActivity::TriviaCompleted,
                &source_key,
                now,
            )
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result.map_err(|error| error.to_string()))
        {
            warn!(user_id, guild_id, %error, "failed to record player-trivia pet activity");
        }

        let previous = answer_result(
            &question_for_display,
            settlement.is_correct,
            settlement.reward,
            settlement.withheld,
        );
        if settlement.completed {
            let mut completed = session;
            completed.score = settlement.score;
            completed.jc_earned = settlement.jc_earned;
            completed.status = "completed".to_owned();
            return responder
                .update(summary_response(
                    "Player Trivia — Complete!",
                    &completed,
                    &user_display_name,
                    avatar.as_deref(),
                    Some(previous),
                ))
                .await
                .map_err(|error| error.to_string());
        }

        let repository = self.repository.clone();
        let refreshed = tokio::task::spawn_blocking(move || {
            repository
                .get_session(parsed.session_id)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia refresh task failed: {error}"))??
        .ok_or_else(|| "player-trivia session disappeared after settlement".to_owned())?;
        let next = refreshed
            .next_question()
            .ok_or_else(|| "player-trivia session has no next frozen question".to_owned())?;
        let next_question = convert_stored_question(&next.question);
        let response = question_response(
            refreshed.session_id,
            &next_question,
            next.question_number,
            refreshed.questions.len(),
            refreshed.score,
            refreshed.jc_earned,
            &user_display_name,
            avatar.as_deref(),
            Some(previous),
            self.config.answer_timeout,
        );
        responder
            .update(response)
            .await
            .map_err(|error| error.to_string())?;
        self.arm_timeout(
            refreshed.session_id,
            next.question_number,
            user_display_name,
            avatar,
            TimeoutTarget::Responder(Arc::clone(&responder)),
        )
    }

    async fn require_gamba(
        &self,
        user_id: i64,
        guild_id: i64,
        channel_id: Option<i64>,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<bool, String> {
        let valid = match channel_id {
            Some(channel_id) => self
                .discord
                .player_trivia_channel_is_gamba(guild_id, channel_id)
                .await
                .unwrap_or(false),
            None => false,
        };
        if valid {
            return Ok(true);
        }
        let penalties = self.penalties.clone();
        tokio::task::spawn_blocking(move || {
            penalties
                .debit_gamba_channel_penalty(user_id, Some(guild_id))
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia gamba penalty task failed: {error}"))??;
        respond_ephemeral(responder, WRONG_CHANNEL_MESSAGE).await?;
        Ok(false)
    }

    fn is_admin(&self, user_id: i64, permissions: Option<u64>) -> bool {
        self.config.admin_user_ids.contains(&user_id)
            || permissions.is_some_and(|value| {
                value & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            })
    }

    fn take_command_rate_limit(&self, user_id: i64) -> Result<Option<f64>, String> {
        let now = Instant::now();
        let mut cooldowns = self
            .command_cooldowns
            .lock()
            .map_err(|_| "player-trivia command cooldown lock was poisoned".to_owned())?;
        cooldowns.retain(|_, claimed| now.saturating_duration_since(*claimed) < COMMAND_RATE_LIMIT);
        if let Some(claimed) = cooldowns.get(&user_id) {
            return Ok(Some(
                (COMMAND_RATE_LIMIT - now.saturating_duration_since(*claimed)).as_secs_f64(),
            ));
        }
        cooldowns.insert(user_id, now);
        Ok(None)
    }

    fn abort_timeout(&self, session_id: i64, question_number: i64) -> Result<(), String> {
        if let Some(timeout) = self
            .timeouts
            .lock()
            .map_err(|_| "player-trivia timeout lock was poisoned".to_owned())?
            .remove(&(session_id, question_number))
        {
            timeout.abort();
        }
        Ok(())
    }

    fn arm_timeout(
        self: &Arc<Self>,
        session_id: i64,
        question_number: i64,
        user_display_name: String,
        avatar: Option<String>,
        target: TimeoutTarget,
    ) -> Result<(), String> {
        self.abort_timeout(session_id, question_number)?;
        let state = Arc::clone(self);
        let timeout = tokio::spawn(async move {
            tokio::time::sleep(state.config.answer_timeout).await;
            if let Err(error) = state
                .timeout_session(
                    session_id,
                    question_number,
                    &user_display_name,
                    avatar.as_deref(),
                    target,
                )
                .await
            {
                debug!(session_id, question_number, %error, "player-trivia timeout failed");
            }
            if let Ok(mut timeouts) = state.timeouts.lock() {
                timeouts.remove(&(session_id, question_number));
            }
        });
        self.timeouts
            .lock()
            .map_err(|_| "player-trivia timeout lock was poisoned".to_owned())?
            .insert((session_id, question_number), timeout);
        Ok(())
    }

    async fn timeout_session(
        &self,
        session_id: i64,
        question_number: i64,
        user_display_name: &str,
        avatar: Option<&str>,
        target: TimeoutTarget,
    ) -> Result<(), String> {
        let repository = self.repository.clone();
        let now = unix_timestamp()?;
        let session = tokio::task::spawn_blocking(move || {
            repository
                .get_session(session_id)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia timeout read task failed: {error}"))??;
        let Some(session) = session else {
            return Ok(());
        };
        let Some(question) = session.next_question() else {
            return Ok(());
        };
        if session.status != "active" || question.question_number != question_number {
            return Ok(());
        }
        let repository = self.repository.clone();
        let changed = tokio::task::spawn_blocking(move || {
            repository
                .timeout_session_if_question_unanswered(session_id, question_number, now)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("player-trivia timeout write task failed: {error}"))??;
        if !changed {
            return Ok(());
        }
        let question = convert_stored_question(&question.question);
        let response = summary_response(
            "Player Trivia — Time's up!",
            &session,
            user_display_name,
            avatar,
            Some(answer_result(&question, false, 0, 0)),
        );
        match target {
            TimeoutTarget::Receipt(receipt) => self
                .discord
                .edit_player_trivia_message(receipt, response)
                .await
                .map_err(|error| error.to_string()),
            TimeoutTarget::Responder(responder) => responder
                .update(response)
                .await
                .map_err(|error| error.to_string()),
        }
    }

    async fn finish_interrupted(&self, session_id: i64, now: i64) {
        let repository = self.repository.clone();
        let _ = tokio::task::spawn_blocking(move || {
            repository.finish_session(session_id, "error", now)
        })
        .await;
    }
}

enum TimeoutTarget {
    Receipt(InteractionMessageReceipt),
    Responder(Arc<dyn InteractionResponder>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParsedComponent {
    session_id: i64,
    question_number: i64,
    selected_index: usize,
}

fn parse_component(custom_id: &str) -> Result<ParsedComponent, String> {
    let parts = custom_id.split(':').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "player_trivia" {
        return Err("invalid player-trivia component ID".to_owned());
    }
    let session_id = parts[1]
        .parse::<i64>()
        .map_err(|_| "invalid player-trivia session ID".to_owned())?;
    let question_number = parts[2]
        .parse::<i64>()
        .map_err(|_| "invalid player-trivia question number".to_owned())?;
    let selected_index = parts[3]
        .parse::<usize>()
        .map_err(|_| "invalid player-trivia answer index".to_owned())?;
    if session_id <= 0 || question_number <= 0 || selected_index >= 4 {
        return Err("player-trivia component values are outside the supported range".to_owned());
    }
    Ok(ParsedComponent {
        session_id,
        question_number,
        selected_index,
    })
}

fn valid_question(question: &app::PlayerTriviaQuestion) -> bool {
    question.correct_index < 4
        && question
            .options
            .iter()
            .map(|value| value.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            == 4
}

#[allow(clippy::too_many_arguments)]
fn question_response(
    session_id: i64,
    question: &app::PlayerTriviaQuestion,
    question_number: i64,
    question_count: usize,
    score: i64,
    jc_earned: i64,
    user_display_name: &str,
    avatar: Option<&str>,
    previous_result: Option<String>,
    answer_timeout: Duration,
) -> InteractionResponse {
    let choices = question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| format!("**{}.** {option}", OPTION_LABELS[index]))
        .collect::<Vec<_>>()
        .join("\n");
    let mut embed = InteractionEmbed::titled(format!(
        "Player Trivia — Question {question_number}/{question_count}"
    ))
    .description(question.text.clone())
    .color(0x58_65_F2)
    .author(user_display_name, avatar.map(str::to_owned))
    .field("Choose one", choices, false);
    if let Some(previous_result) = previous_result {
        embed = embed.field("Previous question result", previous_result, false);
    } else if question_number == 1 {
        embed = embed.field(
            "Your daily set",
            "Each player gets an independently generated set; some questions may overlap.",
            false,
        );
    }
    embed = embed.footer(format!(
        "Score: {score}/{} | JC earned: {jc_earned} | {}s to answer",
        question_number - 1,
        duration_seconds(answer_timeout)
    ));
    let buttons = OPTION_LABELS
        .iter()
        .enumerate()
        .map(|(index, label)| {
            InteractionButton::new(
                format!("player_trivia:{session_id}:{question_number}:{index}"),
                *label,
            )
        })
        .collect::<Vec<_>>();
    InteractionResponse::message("")
        .without_mentions()
        .embed(embed)
        .action_rows(vec![
            InteractionActionRow::buttons(buttons[..2].to_vec()),
            InteractionActionRow::buttons(buttons[2..].to_vec()),
        ])
}

fn answer_result(
    question: &app::PlayerTriviaQuestion,
    is_correct: bool,
    reward: i64,
    withheld: i64,
) -> String {
    let correct = format!(
        "**{}. {}**",
        OPTION_LABELS[question.correct_index], question.options[question.correct_index]
    );
    let mut result = if is_correct {
        format!("✅ Correct! **+{reward}** {JOPACOIN_EMOTE} The answer was {correct}.")
    } else {
        format!("❌ Not this time. The answer was {correct}.")
    };
    if withheld > 0 {
        result.push_str(&format!(" ({withheld} JC withheld.)"));
    }
    if !question.explanation.is_empty() {
        result.push('\n');
        result.push_str(&question.explanation);
    }
    result
}

fn summary_response(
    title: &str,
    session: &db::StoredPlayerTriviaSession,
    user_display_name: &str,
    avatar: Option<&str>,
    last_result: Option<String>,
) -> InteractionResponse {
    let mut embed = InteractionEmbed::titled(title)
        .color(if session.score > 0 {
            0x43_A0_47
        } else {
            0x60_7D_8B
        })
        .author(user_display_name, avatar.map(str::to_owned));
    if let Some(last_result) = last_result {
        embed = embed.description(last_result);
    }
    embed = embed.field(
        "Daily result",
        format!(
            "Score: **{}/{}**\nEarned: **{}** {JOPACOIN_EMOTE}\nCome back in 24 hours for another stat set.",
            session.score,
            session.questions.len(),
            session.jc_earned
        ),
        false,
    );
    InteractionResponse::message("")
        .without_mentions()
        .embed(embed)
}

#[derive(Clone, Debug)]
struct RuntimePlayerTriviaRepository {
    inner: db::PlayerTriviaRepository,
    profit_deductions: ProfitDeductionSource,
}

impl RuntimePlayerTriviaRepository {
    fn new(
        path: impl AsRef<Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
    ) -> Self {
        Self {
            inner: db::PlayerTriviaRepository::new(path.as_ref()),
            profit_deductions: ProfitDeductionSource::from_config(
                config,
                path.as_ref().to_path_buf(),
                Some(vanity_tax),
            ),
        }
    }

    fn player_exists(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<bool, db::PlayerTriviaRepositoryError> {
        self.inner.player_exists(user_id, guild_id)
    }

    fn get_last_session_started(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<i64>, db::PlayerTriviaRepositoryError> {
        self.inner.get_last_session_started(user_id, guild_id)
    }

    fn active_session_for_player(
        &self,
        user_id: i64,
        guild_id: i64,
    ) -> Result<Option<db::StoredPlayerTriviaSession>, db::PlayerTriviaRepositoryError> {
        self.inner.active_session_for_player(user_id, guild_id)
    }

    fn get_session(
        &self,
        session_id: i64,
    ) -> Result<Option<db::StoredPlayerTriviaSession>, db::PlayerTriviaRepositoryError> {
        self.inner.get_session(session_id)
    }

    fn finish_session(
        &self,
        session_id: i64,
        status: &str,
        now: i64,
    ) -> Result<bool, db::PlayerTriviaRepositoryError> {
        self.inner.finish_session(session_id, status, now)
    }

    fn timeout_session_if_question_unanswered(
        &self,
        session_id: i64,
        question_number: i64,
        now: i64,
    ) -> Result<bool, db::PlayerTriviaRepositoryError> {
        self.inner
            .timeout_session_if_question_unanswered(session_id, question_number, now)
    }

    /// Settle inside a blocking task: both taxable rosters are SQLite reads.
    fn settle_answer(
        &self,
        guild_id: i64,
        session_id: i64,
        question_number: i64,
        selected_index: usize,
        reward: i64,
        now: i64,
    ) -> Result<Option<db::PlayerTriviaSettlement>, String> {
        let policy = self.profit_deductions.resolve(guild_id)?;
        self.inner
            .settle_answer(
                session_id,
                question_number,
                selected_index,
                reward,
                now,
                &policy.as_policy(),
            )
            .map_err(|error| error.to_string())
    }

    fn cancel_session_if_unanswered(
        &self,
        session_id: i64,
    ) -> Result<bool, db::PlayerTriviaRepositoryError> {
        self.inner.cancel_session_if_unanswered(session_id)
    }
}

impl app::PlayerTriviaSnapshotRepository for RuntimePlayerTriviaRepository {
    fn load_snapshot(&self, guild_id: i64) -> Result<app::PlayerTriviaSnapshot, String> {
        self.inner
            .load_snapshot(guild_id)
            .map(convert_snapshot)
            .map_err(|error| error.to_string())
    }

    fn recent_question_keys(
        &self,
        discord_id: i64,
        guild_id: i64,
        since: i64,
    ) -> Result<Vec<String>, String> {
        self.inner
            .recent_question_keys(discord_id, guild_id, since)
            .map_err(|error| error.to_string())
    }
}

impl app::PlayerTriviaSessionRepository for RuntimePlayerTriviaRepository {
    type StartResult = Option<i64>;
    type AnswerResult = Option<db::PlayerTriviaSettlement>;
    type FinishResult = bool;
    type CancelResult = bool;

    fn try_start_session(
        &self,
        discord_id: i64,
        guild_id: i64,
        questions: &[app::PlayerTriviaQuestionRecord],
        now: i64,
        cooldown_seconds: i64,
        bypass: bool,
    ) -> Result<Self::StartResult, String> {
        let questions = questions
            .iter()
            .map(convert_question_record)
            .collect::<Vec<_>>();
        self.inner
            .try_start_session(
                discord_id,
                guild_id,
                &questions,
                now,
                cooldown_seconds,
                bypass,
            )
            .map_err(|error| error.to_string())
    }

    fn settle_answer(
        &self,
        guild_id: i64,
        session_id: i64,
        question_number: i64,
        selected_index: usize,
        reward: i64,
        answered_at: i64,
    ) -> Result<Self::AnswerResult, String> {
        Self::settle_answer(
            self,
            guild_id,
            session_id,
            question_number,
            selected_index,
            reward,
            answered_at,
        )
    }

    fn finish_session(
        &self,
        session_id: i64,
        status: &str,
        completed_at: i64,
    ) -> Result<Self::FinishResult, String> {
        self.inner
            .finish_session(session_id, status, completed_at)
            .map_err(|error| error.to_string())
    }

    fn cancel_session_if_unanswered(&self, session_id: i64) -> Result<Self::CancelResult, String> {
        self.inner
            .cancel_session_if_unanswered(session_id)
            .map_err(|error| error.to_string())
    }
}

fn convert_question_record(
    value: &app::PlayerTriviaQuestionRecord,
) -> db::PlayerTriviaQuestionRecord {
    db::PlayerTriviaQuestionRecord {
        question_key: value.question_key.clone(),
        category: value.category.clone(),
        question_text: value.question_text.clone(),
        options: value.options.clone(),
        correct_index: value.correct_index,
        explanation: value.explanation.clone(),
        spicy: value.spicy,
    }
}

fn convert_stored_question(value: &db::PlayerTriviaQuestion) -> app::PlayerTriviaQuestion {
    app::PlayerTriviaQuestion::new(
        value.key.clone(),
        value.category.clone(),
        value.text.clone(),
        value.options.clone(),
        value.correct_index,
        value.explanation.clone(),
        value.spicy,
    )
    .expect("stored player-trivia questions were validated on persistence")
}

fn convert_snapshot(value: db::PlayerTriviaSnapshot) -> app::PlayerTriviaSnapshot {
    app::PlayerTriviaSnapshot {
        players: value
            .players
            .into_iter()
            .map(|row| app::PlayerTriviaPlayer {
                discord_id: row.discord_id,
                username: row.username,
                wins: row.wins,
                losses: row.losses,
                balance: row.balance,
                lowest_balance_ever: row.lowest_balance_ever,
                glicko_rating: row.glicko_rating,
                glicko_rd: row.glicko_rd,
                openskill_mu: row.openskill_mu,
                openskill_sigma: row.openskill_sigma,
                last_match_unix: row.last_match_unix,
            })
            .collect(),
        participants: value
            .participants
            .into_iter()
            .map(|row| app::ParticipantRow {
                discord_id: row.discord_id,
                match_id: row.match_id,
                match_time: row.match_time,
                hero_id: row.hero_id,
                won: row.won,
                kills: row.kills,
                assists: row.assists,
                gpm: row.gpm,
                hero_damage: row.hero_damage,
                tower_damage: row.tower_damage,
                fantasy_points: row.fantasy_points,
                lane_role: row.lane_role,
            })
            .collect(),
        ratings: value
            .ratings
            .into_iter()
            .map(|row| app::RatingRow {
                discord_id: row.discord_id,
                rating: row.rating,
                rating_before: row.rating_before,
            })
            .collect(),
        pairings: value
            .pairings
            .into_iter()
            .map(|row| app::PairingRow {
                player1_id: row.player1_id,
                player2_id: row.player2_id,
                games_together: row.games_together,
                wins_together: row.wins_together,
                games_against: row.games_against,
                player1_wins_against: row.player1_wins_against,
            })
            .collect(),
        tips: value
            .tips
            .into_iter()
            .map(|row| app::TipRow {
                sender_id: row.sender_id,
                recipient_id: row.recipient_id,
                amount: row.amount,
            })
            .collect(),
        wheel_spins: value
            .wheel_spins
            .into_iter()
            .map(|row| app::WheelSpinRow {
                spin_id: row.spin_id,
                discord_id: row.discord_id,
                spin_time: row.spin_time,
                outcome_code: row.outcome_code,
            })
            .collect(),
        double_spins: value
            .double_spins
            .into_iter()
            .map(|row| app::DoubleSpinRow {
                discord_id: row.discord_id,
                won: row.won,
            })
            .collect(),
        trivia_sessions: value
            .trivia_sessions
            .into_iter()
            .map(|row| app::TriviaHistoryRow {
                discord_id: row.discord_id,
                streak: row.streak,
                jc_earned: row.jc_earned,
            })
            .collect(),
        tunnels: value
            .tunnels
            .into_iter()
            .map(|row| app::TunnelRow {
                discord_id: row.discord_id,
                total_digs: row.total_digs,
                max_depth: row.max_depth,
                total_jc_earned: row.total_jc_earned,
                prestige_level: row.prestige_level,
                best_run_score: row.best_run_score,
                pickaxe_tier: row.pickaxe_tier,
                stat_strength: row.stat_strength,
                stat_smarts: row.stat_smarts,
                stat_stamina: row.stat_stamina,
            })
            .collect(),
        bankruptcies: value
            .bankruptcies
            .into_iter()
            .map(|row| app::BankruptcyRow {
                discord_id: row.discord_id,
                bankruptcy_count: row.bankruptcy_count,
            })
            .collect(),
        disbursement_votes: value
            .disbursement_votes
            .into_iter()
            .map(|row| app::DisbursementVoteRow {
                proposal_id: row.proposal_id,
                discord_id: row.discord_id,
                vote_method: row.vote_method,
                proposal_outcome: row.proposal_outcome,
                finalized_at: row.finalized_at,
            })
            .collect(),
        prediction_positions: value
            .prediction_positions
            .into_iter()
            .map(|row| app::PredictionPositionRow {
                prediction_id: row.prediction_id,
                discord_id: row.discord_id,
                status: row.status,
                outcome: row.outcome,
                yes_contracts: row.yes_contracts,
                yes_cost_basis_total: row.yes_cost_basis_total,
                no_contracts: row.no_contracts,
                no_cost_basis_total: row.no_cost_basis_total,
                bankruptcy_penalty: row.bankruptcy_penalty,
                vanity_tax: row.vanity_tax,
                low_priority_tax: row.low_priority_tax,
                question: row.question,
            })
            .collect(),
        prediction_trades: value
            .prediction_trades
            .into_iter()
            .map(|row| app::PredictionTradeRow {
                prediction_id: row.prediction_id,
                discord_id: row.discord_id,
                contracts: row.contracts,
                status: row.status,
            })
            .collect(),
        bets: value
            .bets
            .into_iter()
            .map(|row| app::SettledBetRow {
                bet_id: row.bet_id,
                discord_id: row.discord_id,
                match_id: row.match_id,
                team_bet_on: row.team_bet_on,
                winning_team: row.winning_team,
                amount: row.amount,
                leverage: row.leverage,
                effective_bet: row.effective_bet,
                payout: row.payout,
                settlement_vanity_tax: row.settlement_vanity_tax,
                settlement_low_priority_tax: row.settlement_low_priority_tax,
            })
            .collect(),
        dig_artifacts: value
            .dig_artifacts
            .into_iter()
            .map(|row| app::DigArtifactRow {
                discord_id: row.discord_id,
                is_relic: row.is_relic,
            })
            .collect(),
        dig_achievements: value
            .dig_achievements
            .into_iter()
            .map(|row| app::DigAchievementRow {
                discord_id: row.discord_id,
            })
            .collect(),
        dig_actions: value
            .dig_actions
            .into_iter()
            .map(|row| app::DigActionRow {
                actor_id: row.actor_id,
                action_type: row.action_type,
                depth_before: row.depth_before,
                depth_after: row.depth_after,
            })
            .collect(),
        mafia_games: value
            .mafia_games
            .into_iter()
            .map(|row| app::MafiaGameRow {
                game_id: row.game_id,
                phase: row.phase,
                status: row.status,
                winner: row.winner,
                mvp_id: row.mvp_id,
            })
            .collect(),
        mafia_players: value
            .mafia_players
            .into_iter()
            .map(|row| app::MafiaPlayerRow {
                game_id: row.game_id,
                discord_id: row.discord_id,
                role: row.role,
            })
            .collect(),
        mafia_actions: value
            .mafia_actions
            .into_iter()
            .map(|row| app::MafiaActionRow {
                game_id: row.game_id,
                actor_id: row.actor_id,
                action_type: row.action_type,
            })
            .collect(),
        recalibrations: value
            .recalibrations
            .into_iter()
            .map(|row| app::RecalibrationRow {
                discord_id: row.discord_id,
                total_recalibrations: row.total_recalibrations,
            })
            .collect(),
    }
}

fn unix_timestamp() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())
}

fn duration_seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), String> {
    responder
        .respond(
            InteractionResponse::message(content)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string())
}

async fn followup_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), String> {
    responder
        .followup(
            InteractionResponse::message(content)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string())
}

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "player_trivia_provider/tests.rs"]
mod tests;
