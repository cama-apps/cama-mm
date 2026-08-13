use std::sync::Mutex;

use cama_app::ai_services::{
    AIService, AiProvider, ProviderConfig, ProviderCredential, ProviderError, ProviderLoader,
    ProviderRequest, ProviderResponse, QueryResult, SQL_TOOL_NAME, ToolCall, Value,
};
use cama_app::ask::RecordingQueryPort;
use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, params};

use super::*;
use crate::registration::InteractionResponseError;
use crate::{InteractionOption, Registry};

#[derive(Default)]
struct RecordingResponder {
    responses: Mutex<Vec<InteractionResponse>>,
    deferrals: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
}

#[async_trait]
impl InteractionResponder for RecordingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.responses.lock().expect("responses").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.deferrals.lock().expect("deferrals").push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        Ok(())
    }
}

fn provider(limit: u32, query: Arc<RecordingQueryPort>) -> AskRegistrationProvider {
    AskRegistrationProvider::with_ports(
        AskConfig {
            rate_limit_requests: limit,
            rate_limit_window_seconds: 60,
        },
        Arc::new(WindowRateLimiter::default()),
        query,
    )
}

fn request(question: &str, guild_id: Option<u64>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "ask".to_owned(),
        user_id: 42,
        user_display_name: "Asker".to_owned(),
        guild_id,
        channel_id: Some(99),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "question".to_owned(),
            value: InteractionValue::String(question.to_owned()),
        }],
    }
}

fn registry(provider: &AskRegistrationProvider) -> Registry {
    let mut registry = RegistryBuilder::default();
    registry
        .add_provider(provider)
        .expect("register ask provider");
    registry.build()
}

struct FixedSqlProvider;

impl AiProvider for FixedSqlProvider {
    fn complete(&self, _request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        Ok(ProviderResponse {
            tool_calls: vec![ToolCall {
                name: SQL_TOOL_NAME.to_owned(),
                arguments: BTreeMap::from([
                    (
                        "sql".to_owned(),
                        Value::Text(
                            "SELECT discord_username, wins FROM players ORDER BY wins DESC"
                                .to_owned(),
                        ),
                    ),
                    (
                        "explanation".to_owned(),
                        Value::Text("League leaders".to_owned()),
                    ),
                ]),
            }],
            ..ProviderResponse::default()
        })
    }
}

struct FixedSqlLoader;

impl ProviderLoader for FixedSqlLoader {
    fn load(&self) -> Result<Arc<dyn AiProvider>, ProviderError> {
        Ok(Arc::new(FixedSqlProvider))
    }
}

#[test]
fn registration_preserves_exact_python_command_and_option_contract() {
    let provider = provider(3, Arc::new(RecordingQueryPort::default()));
    let registry = registry(&provider);
    let command = registry.commands().next().expect("ask command");
    assert_eq!(command.name, "ask");
    assert_eq!(
        command.description,
        "Ask a question about league data in natural language"
    );
    assert_eq!(command.options.len(), 1);
    assert_eq!(command.options[0].name, "question");
    assert_eq!(command.options[0].kind, CommandOptionKind::String);
    assert!(command.options[0].required);
    assert_eq!(command.options[0].min_length, None);
    assert_eq!(command.options[0].max_length, None);
}

#[tokio::test]
async fn public_defer_query_and_blue_embed_use_typed_policy() {
    let query = Arc::new(RecordingQueryPort::default());
    *query.result.lock().expect("query result") = QueryResult {
        success: true,
        results: vec![BTreeMap::from([
            (
                "discord_username".to_owned(),
                Value::Text("Alice".to_owned()),
            ),
            ("wins".to_owned(), Value::Integer(12)),
        ])],
        row_count: 1,
        ..QueryResult::default()
    };
    let handler = registry(&provider(3, query.clone()))
        .command_handler("ask")
        .expect("ask handler");
    let responder = Arc::new(RecordingResponder::default());
    handler
        .handle(
            request("who wins the most?", Some(12_345)),
            responder.clone(),
        )
        .await
        .expect("handle ask");

    assert_eq!(*responder.deferrals.lock().expect("deferrals"), [false]);
    let followups = responder.followups.lock().expect("followups");
    assert_eq!(followups.len(), 1);
    assert!(!followups[0].ephemeral);
    assert_eq!(followups[0].embeds[0].color, Some(DISCORD_BLUE));
    assert_eq!(followups[0].embeds[0].fields[1].name, "Answer");
    assert!(followups[0].embeds[0].fields[1].value.contains("Alice"));
    assert_eq!(
        *query.calls.lock().expect("query calls"),
        [(12_345, "who wins the most?".to_owned(), 42)]
    );
}

#[tokio::test]
async fn dm_validation_length_and_rate_limit_preserve_response_timing() {
    let query = Arc::new(RecordingQueryPort::default());
    let handler = registry(&provider(1, query.clone()))
        .command_handler("ask")
        .expect("ask handler");

    let dm = Arc::new(RecordingResponder::default());
    handler
        .handle(request("who wins?", None), dm.clone())
        .await
        .expect("DM guard");
    assert!(dm.responses.lock().expect("responses")[0].ephemeral);
    assert!(dm.deferrals.lock().expect("deferrals").is_empty());

    let short = Arc::new(RecordingResponder::default());
    handler
        .handle(request("hi", Some(7)), short.clone())
        .await
        .expect("short validation");
    assert_eq!(*short.deferrals.lock().expect("deferrals"), [false]);
    assert!(short.followups.lock().expect("followups")[0].ephemeral);

    let limited = Arc::new(RecordingResponder::default());
    handler
        .handle(request("who wins?", Some(7)), limited.clone())
        .await
        .expect("rate limit");
    assert!(limited.responses.lock().expect("responses")[0].ephemeral);
    assert!(
        limited.responses.lock().expect("responses")[0]
            .content
            .contains("Rate limited")
    );
    assert!(limited.deferrals.lock().expect("deferrals").is_empty());
    assert!(query.calls.lock().expect("query calls").is_empty());
}

#[tokio::test]
async fn backend_error_is_red_public_and_never_leaks_internal_detail() {
    let query = Arc::new(RecordingQueryPort::default());
    *query.result.lock().expect("query result") = QueryResult {
        error: Some("ZZZINTERNALSENTINELZZZ".to_owned()),
        ..QueryResult::default()
    };
    let handler = registry(&provider(3, query))
        .command_handler("ask")
        .expect("ask handler");
    let responder = Arc::new(RecordingResponder::default());
    handler
        .handle(
            request("what is the average rating?", Some(12_345)),
            responder.clone(),
        )
        .await
        .expect("handle failed query");
    let followups = responder.followups.lock().expect("followups");
    let embed = &followups[0].embeds[0];
    assert_eq!(embed.color, Some(DISCORD_RED));
    assert_eq!(embed.fields[1].name, "");
    assert!(embed.fields[1].value.contains("I couldn't answer that"));
    assert!(!format!("{embed:?}").contains("ZZZINTERNALSENTINELZZZ"));
}

#[tokio::test]
async fn production_provider_runs_llm_generation_and_guild_scoped_migrated_sqlite() {
    let directory = tempfile::tempdir().expect("temporary ask database");
    let path = directory.path().join("cama.db");
    initialize_or_migrate_with_settings(&path, &MigrationSettings::default())
        .expect("migrate ask database");
    let connection = Connection::open(&path).expect("open ask database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production foreign-key mode");
    connection
        .execute(
            "INSERT INTO players
                 (discord_id, guild_id, discord_username, wins, jopacoin_balance)
             VALUES (?1, ?2, 'Alice', 12, 100),
                    (?3, ?4, 'Other Guild', 99, 100)",
            params![42_i64, 12_345_i64, 99_i64, 54_321_i64],
        )
        .expect("seed cross-guild players");
    let config = ProviderConfig::new(
        "test/sql",
        ProviderCredential::new("offline-test-key").expect("credential"),
        Duration::from_secs(1),
        500,
    )
    .expect("provider config");
    let ai = Arc::new(AIService::new(config, Arc::new(FixedSqlLoader)));
    let provider = AskRegistrationProvider::new(&path, ai, true, 10, 60);
    let handler = registry(&provider)
        .command_handler("ask")
        .expect("ask handler");
    let responder = Arc::new(RecordingResponder::default());

    handler
        .handle(
            request("who wins the most?", Some(12_345)),
            responder.clone(),
        )
        .await
        .expect("production ask query");

    let followups = responder.followups.lock().expect("followups");
    let answer = &followups[0].embeds[0].fields[1].value;
    assert!(answer.contains("Alice"));
    assert!(!answer.contains("Other Guild"));
}

#[test]
fn nonpositive_rate_limit_config_matches_python_fail_soft_behavior() {
    let config = normalize_rate_config(-1, -5);
    assert_eq!(config.rate_limit_requests, 0);
    assert_eq!(config.rate_limit_window_seconds, 0);

    let limiter = WindowRateLimiter::default();
    assert!(
        !limiter
            .check(RateLimitRequest {
                guild_id: 1,
                user_id: 2,
                limit: config.rate_limit_requests,
                per_seconds: config.rate_limit_window_seconds,
            })
            .allowed
    );
    assert!(
        limiter
            .check(RateLimitRequest {
                guild_id: 1,
                user_id: 2,
                limit: 1,
                per_seconds: 0,
            })
            .allowed
    );
}
