//! Production Discord adapter for the Python-compatible `/ask` command.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cama_app::ai_query_sqlite::SqliteAIQueryRepository;
use cama_app::ai_services::{AIQueryRepository, AIService, GuildAiConfigPort, SQLQueryService};
use cama_app::ask::{
    AskConfig, AskQueryPort, AskRateLimiter, EMBED_ANSWER_LENGTH, EMBED_QUESTION_LENGTH,
    EmbedColor, MAX_QUESTION_LENGTH, MIN_QUESTION_LENGTH, RateLimitDecision, RateLimitRequest,
    format_query_result,
};
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_domain::guild_config::GuildConfigStore;
use tracing::warn;

use crate::embed_colors::{DISCORD_BLUE, DISCORD_RED};
use crate::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, InteractionEmbed, InteractionHandler,
    InteractionHandlerError, InteractionRequest, InteractionResponder, InteractionResponse,
    InteractionValue, RegistrationError, RegistrationProvider, RegistryBuilder,
};

#[derive(Clone)]
pub struct AskRegistrationProvider {
    handler: Arc<AskHandler>,
}

impl AskRegistrationProvider {
    pub fn new(
        database_path: impl AsRef<Path>,
        ai: Arc<AIService>,
        ai_features_default: bool,
        rate_limit_requests: i64,
        rate_limit_window_seconds: i64,
    ) -> Self {
        // Python's fail-soft integer parser does not clamp these settings.
        // A nonpositive request limit denies every call; a negative window
        // expires prior hits immediately. Saturating very large positives
        // preserves their practical unlimited behavior.
        let config = normalize_rate_config(rate_limit_requests, rate_limit_window_seconds);
        let repository: Arc<dyn AIQueryRepository> =
            Arc::new(SqliteAIQueryRepository::new(database_path.as_ref()));
        let query = Arc::new(ProductionAskQuery {
            service: SQLQueryService::new(repository),
            ai,
            guild_config: GuildAiConfig {
                repository: GuildConfigRepository::new(database_path, ai_features_default),
            },
        });
        Self::with_ports(config, Arc::new(WindowRateLimiter::default()), query)
    }

    fn with_ports(
        config: AskConfig,
        limiter: Arc<dyn AskRateLimiter + Send + Sync>,
        query: Arc<dyn AskQueryPort + Send + Sync>,
    ) -> Self {
        Self {
            handler: Arc::new(AskHandler {
                config,
                limiter,
                query,
            }),
        }
    }
}

fn normalize_rate_config(requests: i64, window_seconds: i64) -> AskConfig {
    AskConfig {
        rate_limit_requests: u32::try_from(requests.max(0)).unwrap_or(u32::MAX),
        rate_limit_window_seconds: u64::try_from(window_seconds.max(0)).unwrap_or(u64::MAX),
    }
}

impl RegistrationProvider for AskRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        let mut question = CommandOptionSpec::new(
            "question",
            "Your question (e.g., 'who has the highest win rate?', 'how many matches have been played?')",
            CommandOptionKind::String,
        );
        question.required = true;
        registry.command(CommandSpec {
            name: "ask".to_owned(),
            description: "Ask a question about league data in natural language".to_owned(),
            options: vec![question],
            handler: self.handler.clone(),
        })
    }
}

struct AskHandler {
    config: AskConfig,
    limiter: Arc<dyn AskRateLimiter + Send + Sync>,
    query: Arc<dyn AskQueryPort + Send + Sync>,
}

#[async_trait]
impl InteractionHandler for AskHandler {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        let InteractionRequest::Command {
            name,
            user_id,
            guild_id,
            options,
            ..
        } = request
        else {
            return Err("ask handler received a non-command interaction".into());
        };
        if name != "ask" {
            return Err(format!("ask handler received command {name:?}").into());
        }
        let Some(guild_id) = guild_id else {
            respond_fail_soft(
                responder.as_ref(),
                InteractionResponse::message("This command can only be used in a server.")
                    .ephemeral(),
            )
            .await;
            return Ok(());
        };
        let guild_id = signed_id(guild_id, "guild")?;
        let user_id = signed_id(user_id, "user")?;
        let question = options
            .iter()
            .find(|option| option.name == "question")
            .and_then(|option| match &option.value {
                InteractionValue::String(value) => Some(value.clone()),
                _ => None,
            })
            .ok_or_else(|| InteractionHandlerError::Handler("missing question".to_owned()))?;

        let decision = self.limiter.check(RateLimitRequest {
            guild_id,
            user_id,
            limit: self.config.rate_limit_requests,
            per_seconds: self.config.rate_limit_window_seconds,
        });
        if !decision.allowed {
            respond_fail_soft(
                responder.as_ref(),
                InteractionResponse::message(format!(
                    "Rate limited. Try again in {:.0} seconds.",
                    decision.retry_after_seconds
                ))
                .ephemeral(),
            )
            .await;
            return Ok(());
        }

        if let Err(error) = responder.defer(false).await {
            warn!(%error, "unable to defer ask interaction");
            return Ok(());
        }

        let question_length = question.chars().count();
        if question_length < MIN_QUESTION_LENGTH {
            followup_fail_soft(
                responder.as_ref(),
                InteractionResponse::message("Please provide a more detailed question.")
                    .ephemeral(),
            )
            .await;
            return Ok(());
        }
        if question_length > MAX_QUESTION_LENGTH {
            followup_fail_soft(
                responder.as_ref(),
                InteractionResponse::message(
                    "Question is too long. Please keep it under 500 characters.",
                )
                .ephemeral(),
            )
            .await;
            return Ok(());
        }

        let query = Arc::clone(&self.query);
        let query_question = question.clone();
        let result =
            tokio::task::spawn_blocking(move || query.query(guild_id, &query_question, user_id))
                .await
                .map_err(|error| format!("ask query task failed: {error}"))?;
        let success = result.success;
        let answer = if success {
            format_query_result(&result, EMBED_ANSWER_LENGTH)
        } else {
            warn!(guild_id, user_id, error = ?result.error, "ask query failed");
            "I couldn't answer that. Try asking about player stats, matches, or the leaderboard."
                .to_owned()
        };
        let color = if success {
            EmbedColor::Blue
        } else {
            EmbedColor::Red
        };
        let embed = InteractionEmbed::titled("AI Query Result")
            .color(match color {
                EmbedColor::Blue => DISCORD_BLUE,
                EmbedColor::Red => DISCORD_RED,
            })
            .field(
                "Question",
                question
                    .chars()
                    .take(EMBED_QUESTION_LENGTH)
                    .collect::<String>(),
                false,
            )
            .field(if success { "Answer" } else { "" }, answer, false);
        followup_fail_soft(
            responder.as_ref(),
            InteractionResponse::message("").embed(embed),
        )
        .await;
        Ok(())
    }
}

struct ProductionAskQuery {
    service: SQLQueryService,
    ai: Arc<AIService>,
    guild_config: GuildAiConfig,
}

impl AskQueryPort for ProductionAskQuery {
    fn query(
        &self,
        guild_id: i64,
        question: &str,
        asker_discord_id: i64,
    ) -> cama_app::ai_services::QueryResult {
        self.service.query(
            self.ai.as_ref(),
            Some(&self.guild_config),
            Some(guild_id),
            question,
            Some(asker_discord_id),
        )
    }
}

struct GuildAiConfig {
    repository: GuildConfigRepository,
}

impl GuildAiConfigPort for GuildAiConfig {
    fn ai_enabled(&self, guild_id: i64) -> Result<bool, String> {
        self.repository
            .get_ai_enabled(guild_id)
            .map_err(|error| error.to_string())
    }
}

#[derive(Default)]
struct WindowRateLimiter {
    state: Mutex<RateState>,
}

#[derive(Default)]
struct RateState {
    hits: BTreeMap<(i64, i64), VecDeque<Instant>>,
    maximum_window: Duration,
}

impl AskRateLimiter for WindowRateLimiter {
    fn check(&self, request: RateLimitRequest) -> RateLimitDecision {
        if request.limit == 0 {
            return RateLimitDecision {
                allowed: false,
                retry_after_seconds: 0.0,
            };
        }
        if request.per_seconds == 0 {
            return RateLimitDecision {
                allowed: true,
                retry_after_seconds: 0.0,
            };
        }
        let now = Instant::now();
        let window = Duration::from_secs(request.per_seconds);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.maximum_window = state.maximum_window.max(window);
        if state.hits.len() >= 1_024 {
            let maximum_window = state.maximum_window;
            state.hits.retain(|_, hits| {
                hits.back()
                    .is_some_and(|last| now.duration_since(*last) <= maximum_window)
            });
        }
        let hits = state
            .hits
            .entry((request.guild_id, request.user_id))
            .or_default();
        while hits
            .front()
            .is_some_and(|started| now.duration_since(*started) > window)
        {
            hits.pop_front();
        }
        if hits.len() >= request.limit as usize {
            let remaining = hits
                .front()
                .map(|started| window.saturating_sub(now.duration_since(*started)))
                .unwrap_or_default();
            return RateLimitDecision {
                allowed: false,
                retry_after_seconds: (remaining.as_secs()
                    + u64::from(remaining.subsec_nanos() != 0))
                    as f64,
            };
        }
        hits.push_back(now);
        RateLimitDecision {
            allowed: true,
            retry_after_seconds: 0.0,
        }
    }
}

fn signed_id(value: u64, label: &str) -> Result<i64, InteractionHandlerError> {
    i64::try_from(value)
        .map_err(|_| InteractionHandlerError::Handler(format!("{label} id exceeds SQLite range")))
}

async fn respond_fail_soft(responder: &dyn InteractionResponder, response: InteractionResponse) {
    if let Err(error) = responder.respond(response).await {
        warn!(%error, "ask initial response failed");
    }
}

async fn followup_fail_soft(responder: &dyn InteractionResponder, response: InteractionResponse) {
    if let Err(error) = responder.followup(response).await {
        warn!(%error, "ask followup failed");
    }
}

#[cfg(all(test, feature = "runtime-test-core"))]
#[path = "ask_provider/tests.rs"]
mod tests;
