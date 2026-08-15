use super::*;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::ThreadId;

type EventLog = Arc<Mutex<Vec<(String, ThreadId)>>>;

#[derive(Default)]
struct ScriptedProvider {
    responses: Mutex<VecDeque<Result<ProviderResponse, ProviderError>>>,
    requests: Mutex<Vec<ProviderRequest>>,
    delay: Mutex<Duration>,
    events: Option<EventLog>,
}

impl ScriptedProvider {
    fn with_responses(responses: Vec<Result<ProviderResponse, ProviderError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            ..Self::default()
        })
    }

    fn requests(&self) -> Vec<ProviderRequest> {
        self.requests.lock().expect("request lock").clone()
    }
}

impl AiProvider for ScriptedProvider {
    fn complete(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        if let Some(events) = &self.events {
            events
                .lock()
                .expect("event lock")
                .push(("provider".to_owned(), thread::current().id()));
        }
        self.requests.lock().expect("request lock").push(request);
        let delay = *self.delay.lock().expect("delay lock");
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        self.responses
            .lock()
            .expect("response lock")
            .pop_front()
            .unwrap_or_else(|| {
                Ok(ProviderResponse {
                    content: Some("done".to_owned()),
                    ..ProviderResponse::default()
                })
            })
    }
}

struct StaticLoader {
    provider: Arc<dyn AiProvider>,
    loads: AtomicUsize,
    delay: Duration,
    events: Option<EventLog>,
    fail_first: AtomicBool,
}

impl StaticLoader {
    fn new(provider: Arc<dyn AiProvider>) -> Arc<Self> {
        Arc::new(Self {
            provider,
            loads: AtomicUsize::new(0),
            delay: Duration::ZERO,
            events: None,
            fail_first: AtomicBool::new(false),
        })
    }
}

impl ProviderLoader for StaticLoader {
    fn load(&self) -> Result<Arc<dyn AiProvider>, ProviderError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        if let Some(events) = &self.events {
            events
                .lock()
                .expect("event lock")
                .push(("configured".to_owned(), thread::current().id()));
        }
        if !self.delay.is_zero() {
            thread::sleep(self.delay);
        }
        if self.fail_first.swap(false, Ordering::SeqCst) {
            return Err(ProviderError::new(
                ProviderErrorKind::Loader,
                "cold load failed",
            ));
        }
        Ok(Arc::clone(&self.provider))
    }
}

fn provider_config(model: &str) -> ProviderConfig {
    ProviderConfig::new(
        model,
        ProviderCredential::new("test-api-key").expect("credential"),
        Duration::from_millis(100),
        500,
    )
    .expect("provider config")
}

fn service_with_provider(
    model: &str,
    provider: Arc<ScriptedProvider>,
) -> (AIService, Arc<StaticLoader>) {
    let adapter: Arc<dyn AiProvider> = provider;
    let loader = StaticLoader::new(adapter);
    let loader_port: Arc<dyn ProviderLoader> = loader.clone();
    (AIService::new(provider_config(model), loader_port), loader)
}

fn content_response(content: &str) -> ProviderResponse {
    ProviderResponse {
        content: Some(content.to_owned()),
        ..ProviderResponse::default()
    }
}

fn tool_response(name: &str, arguments: BTreeMap<String, Value>) -> ProviderResponse {
    ProviderResponse {
        tool_calls: vec![ToolCall {
            name: name.to_owned(),
            arguments,
        }],
        ..ProviderResponse::default()
    }
}

fn sql_arguments(sql: &str, explanation: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("sql".to_owned(), Value::Text(sql.to_owned())),
        (
            "explanation".to_owned(),
            Value::Text(explanation.to_owned()),
        ),
    ])
}

fn basic_tool_request() -> ToolRequest {
    ToolRequest {
        messages: vec![Message::new(MessageRole::User, "test")],
        tools: vec![ToolDefinition::sql()],
        tool_choice: ToolChoice::Auto,
        max_tokens: None,
        temperature: None,
        feature: "llm.tool_call".to_owned(),
    }
}

#[derive(Default)]
struct RecordingAttempts(Mutex<Vec<AttemptMetadata>>);

impl AttemptRecorder for RecordingAttempts {
    fn record_attempt(&self, attempt: AttemptMetadata) -> Result<(), String> {
        self.0.lock().expect("attempt lock").push(attempt);
        Ok(())
    }
}

#[test]
fn test_ai_service_import_and_construction_do_not_load_litellm() {
    let provider = ScriptedProvider::with_responses(Vec::new());
    let (service, loader) = service_with_provider("cerebras/zai-glm-4.7", provider);
    assert_eq!(service.provider_count(), 1);
    assert_eq!(loader.loads.load(Ordering::SeqCst), 0);
}

#[test]
fn test_acompletion_first_use_configures_and_delegates_to_litellm() {
    let provider = ScriptedProvider::with_responses(vec![Ok(content_response("fake-response"))]);
    let (service, loader) = service_with_provider("cerebras/zai-glm-4.7", provider.clone());
    assert_eq!(
        service.complete("test", None, 0.7, None, "llm.complete"),
        Some("fake-response".to_owned())
    );
    assert_eq!(loader.loads.load(Ordering::SeqCst), 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].num_retries, 0);
}

#[test]
fn test_first_provider_use_loads_litellm_off_loop_before_timeout() {
    let caller = thread::current().id();
    let events = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(vec![Ok(content_response("done"))].into()),
        events: Some(events.clone()),
        ..ScriptedProvider::default()
    });
    let adapter: Arc<dyn AiProvider> = provider;
    let loader = Arc::new(StaticLoader {
        provider: adapter,
        loads: AtomicUsize::new(0),
        delay: Duration::from_millis(40),
        events: Some(events.clone()),
        fail_first: AtomicBool::new(false),
    });
    let mut config = provider_config("cerebras/zai-glm-4.7");
    config.timeout = Duration::from_millis(20);
    let loader_port: Arc<dyn ProviderLoader> = loader.clone();
    let service = Arc::new(AIService::new(config, loader_port));
    let barrier = Arc::new(std::sync::Barrier::new(7));
    let callers = (0..6)
        .map(|_| {
            let service = service.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                service.complete("prompt", None, 0.7, None, "llm.complete")
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for caller_thread in callers {
        assert_eq!(
            caller_thread.join().expect("caller thread"),
            Some("done".to_owned())
        );
    }
    assert_eq!(loader.loads.load(Ordering::SeqCst), 1);
    let events = events.lock().expect("event lock");
    assert_eq!(
        events.first().map(|event| event.0.as_str()),
        Some("configured")
    );
    assert_eq!(
        events.iter().filter(|event| event.0 == "provider").count(),
        6
    );
    assert_ne!(events[0].1, caller);
}

#[test]
fn test_litellm_error_classification_uses_only_loaded_module() {
    let primary = ScriptedProvider::with_responses(vec![Err(ProviderError::new(
        ProviderErrorKind::RateLimit,
        "busy",
    ))]);
    let (single, _) = service_with_provider("groq/openai/gpt-oss-20b", primary);
    assert_eq!(
        single
            .try_complete("prompt", None, 0.7, None, "llm.complete")
            .expect_err("typed provider failure")
            .kind,
        ProviderErrorKind::RateLimit
    );

    let primary = ScriptedProvider::with_responses(vec![Err(ProviderError::new(
        ProviderErrorKind::RateLimit,
        "busy",
    ))]);
    let fallback = ScriptedProvider::with_responses(vec![Ok(content_response("fallback"))]);
    let (service, _) = service_with_provider("groq/openai/gpt-oss-20b", primary);
    let fallback_adapter: Arc<dyn AiProvider> = fallback;
    let fallback_loader = StaticLoader::new(fallback_adapter);
    let loader_port: Arc<dyn ProviderLoader> = fallback_loader;
    let service = service.with_fallback(provider_config("cerebras/zai-glm-4.7"), loader_port);
    assert_eq!(
        service.complete("prompt", None, 0.7, None, "llm.complete"),
        Some("fallback".to_owned())
    );
}

#[test]
fn test_litellm_pydantic_serializer_warning_is_suppressed() {
    assert!(is_suppressed_provider_warning(
        "Pydantic serializer warnings:\n  PydanticSerializationUnexpectedValue(Expected 10 fields but got 5: Expected `Message`)"
    ));
    assert!(!is_suppressed_provider_warning("unrelated warning"));
    let credential = ProviderCredential::new("do-not-log-me").expect("credential");
    assert_eq!(format!("{credential:?}"), "ProviderCredential([REDACTED])");
    assert!(!format!("{credential:?}").contains("do-not-log-me"));
}

#[test]
fn test_call_with_tools_returns_tool_args() {
    let provider = ScriptedProvider::with_responses(vec![Ok(tool_response(
        SQL_TOOL_NAME,
        sql_arguments("SELECT * FROM players", "Get all players"),
    ))]);
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider);
    let result = service.call_with_tools(basic_tool_request());
    assert_eq!(result.tool_name.as_deref(), Some(SQL_TOOL_NAME));
    assert_eq!(
        result.tool_args["sql"].as_str(),
        Some("SELECT * FROM players")
    );
    assert_eq!(
        result.tool_args["explanation"].as_str(),
        Some("Get all players")
    );
}

#[test]
fn test_groq_gpt_oss_tool_calls_use_supported_reasoning_params() {
    let provider = ScriptedProvider::with_responses(vec![Ok(tool_response(
        SQL_TOOL_NAME,
        sql_arguments("SELECT wins FROM players LIMIT 1", "one"),
    ))]);
    let (service, _) = service_with_provider("groq/openai/gpt-oss-120b", provider.clone());
    let _ = service.call_with_tools(basic_tool_request());
    let request = provider.requests().pop().expect("request");
    assert_eq!(request.options.reasoning_format, None);
    assert_eq!(request.options.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(request.options.parallel_tool_calls, Some(false));
}

#[test]
fn test_default_groq_qwen_tool_calls_use_litellm_param_allowlist() {
    let provider = ScriptedProvider::with_responses(vec![Ok(ProviderResponse::default())]);
    let (service, _) = service_with_provider("groq/qwen/qwen3.6-27b", provider.clone());
    let _ = service.call_with_tools(basic_tool_request());
    let request = provider.requests().pop().expect("request");
    assert_eq!(request.options.reasoning_format.as_deref(), Some("parsed"));
    assert_eq!(request.options.reasoning_effort.as_deref(), Some("none"));
    assert_eq!(request.options.allowed_openai_params, ["reasoning_effort"]);
    assert_eq!(request.options.parallel_tool_calls, Some(false));
}

#[test]
fn test_default_groq_qwen_completion_uses_parsed_reasoning_only() {
    let provider = ScriptedProvider::with_responses(vec![Ok(content_response("done"))]);
    let (service, _) = service_with_provider("groq/qwen/qwen3.6-27b", provider.clone());
    assert_eq!(
        service
            .complete("test", None, 0.7, None, "llm.complete")
            .as_deref(),
        Some("done")
    );
    let request = provider.requests().pop().expect("request");
    assert_eq!(request.options.reasoning_format.as_deref(), Some("parsed"));
    assert_eq!(request.options.reasoning_effort, None);
    assert!(request.options.allowed_openai_params.is_empty());
    assert_eq!(request.options.parallel_tool_calls, None);
}

#[test]
fn test_generate_sql_returns_sql_and_explanation() {
    let provider = ScriptedProvider::with_responses(vec![Ok(tool_response(
        SQL_TOOL_NAME,
        sql_arguments(
            "SELECT discord_username, wins FROM players ORDER BY wins DESC LIMIT 5",
            "Get top 5 players by wins",
        ),
    ))]);
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider);
    let result = service
        .generate_sql("who has the most wins?", "schema context", None, None)
        .expect("generated SQL");
    assert!(result.sql.contains("SELECT discord_username"));
    assert_eq!(result.explanation, "Get top 5 players by wins");
}

#[test]
fn test_generate_flavor_returns_comment() {
    let provider = ScriptedProvider::with_responses(vec![Ok(tool_response(
        FLAVOR_TOOL_NAME,
        BTreeMap::from([(
            "comment".to_owned(),
            Value::Text("The house always wins, but at least you tried!".to_owned()),
        )]),
    ))]);
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider);
    let input = FlavorPromptInput {
        event_type: "bankruptcy_declared".to_owned(),
        examples: vec!["Example roast".to_owned()],
        ..FlavorPromptInput::default()
    };
    assert_eq!(
        service.generate_flavor(&input).as_deref(),
        Some("The house always wins, but at least you tried!")
    );
}

#[test]
fn test_generate_flavor_match_win_injects_persona() {
    let provider = ScriptedProvider::with_responses(vec![Ok(tool_response(
        FLAVOR_TOOL_NAME,
        BTreeMap::from([("comment".to_owned(), Value::Text("ok".to_owned()))]),
    ))]);
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider.clone());
    let input = FlavorPromptInput {
        event_type: "match_win".to_owned(),
        event_details: BTreeMap::from([
            ("is_big_gainer".to_owned(), Value::Bool(true)),
            ("rating_change".to_owned(), Value::Integer(90)),
        ]),
        examples: vec!["unused fallback".to_owned()],
        persona: Some(FlavorPersona {
            key: "test".to_owned(),
            name: "Test Persona Voice".to_owned(),
            system_prompt: "UNIQUE_VOICE_SENTINEL".to_owned(),
            examples: vec![
                "UNIQUE_EXAMPLE_SENTINEL_ONE".to_owned(),
                "UNIQUE_EXAMPLE_SENTINEL_TWO".to_owned(),
            ],
        }),
        ..FlavorPromptInput::default()
    };
    assert_eq!(service.generate_flavor(&input).as_deref(), Some("ok"));
    let request = provider.requests().pop().expect("request");
    let system = &request.messages[0].content;
    assert!(system.contains("UNIQUE_VOICE_SENTINEL"));
    assert!(system.contains("UNIQUE_EXAMPLE_SENTINEL_ONE"));
    assert!(system.contains("Test Persona Voice"));
    assert!(!system.contains("unused fallback"));
    assert_eq!(request.temperature, Some(0.95));
}

#[test]
fn test_generate_flavor_gambling_event_skips_persona() {
    let provider = ScriptedProvider::with_responses(vec![Ok(tool_response(
        FLAVOR_TOOL_NAME,
        BTreeMap::from([("comment".to_owned(), Value::Text("ok".to_owned()))]),
    ))]);
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider.clone());
    let input = FlavorPromptInput {
        event_type: "loan_taken".to_owned(),
        examples: vec!["Example A".to_owned()],
        ..FlavorPromptInput::default()
    };
    let _ = service.generate_flavor(&input);
    assert_eq!(
        provider.requests().pop().expect("request").temperature,
        None
    );

    // The same-provider failover contract is structured tool -> plain content
    // -> a second direct completion; no automatic retry is introduced.
    let provider = ScriptedProvider::with_responses(vec![
        Ok(ProviderResponse::default()),
        Ok(content_response("direct fallback")),
    ]);
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider.clone());
    assert_eq!(
        service.generate_flavor(&input).as_deref(),
        Some("direct fallback")
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].operation, AiOperation::ToolCall);
    assert_eq!(requests[1].operation, AiOperation::Completion);
}

#[test]
fn test_generate_flavor_bet_warning_frames_minutes_not_final_call() {
    let provider = ScriptedProvider::with_responses(vec![Ok(tool_response(
        FLAVOR_TOOL_NAME,
        BTreeMap::from([("comment".to_owned(), Value::Text("ok".to_owned()))]),
    ))]);
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider.clone());
    let input = FlavorPromptInput {
        event_type: "bet_warning".to_owned(),
        event_details: BTreeMap::from([
            ("angle".to_owned(), Value::Text("roast_underdog".to_owned())),
            ("underdog_side".to_owned(), Value::Text("dire".to_owned())),
            ("seconds_left".to_owned(), Value::Integer(300)),
        ]),
        ..FlavorPromptInput::default()
    };
    let _ = service.generate_flavor(&input);
    let request = provider.requests().pop().expect("request");
    let system = &request.messages[0].content;
    assert!(system.to_ascii_lowercase().contains("minute"));
    assert!(!system.contains("FINAL CALL"));
    assert!(system.contains("Dire"));
    assert_eq!(request.temperature, Some(0.95));
}

#[test]
fn test_generate_flavor_bet_last_call_still_says_final_call() {
    let provider = ScriptedProvider::with_responses(vec![Ok(tool_response(
        FLAVOR_TOOL_NAME,
        BTreeMap::from([("comment".to_owned(), Value::Text("ok".to_owned()))]),
    ))]);
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider.clone());
    let _ = service.generate_flavor(&FlavorPromptInput {
        event_type: "bet_last_call".to_owned(),
        event_details: BTreeMap::from([("seconds_left".to_owned(), Value::Integer(60))]),
        ..FlavorPromptInput::default()
    });
    assert!(
        provider.requests()[0].messages[0]
            .content
            .contains("FINAL CALL")
    );
}

#[test]
fn test_complete_returns_text_content() {
    let provider = ScriptedProvider::with_responses(vec![Ok(ProviderResponse {
        content: Some("This is the AI response.".to_owned()),
        reasoning_content: Some("private reasoning".to_owned()),
        ..ProviderResponse::default()
    })]);
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider);
    assert_eq!(
        service
            .complete("Test prompt", Some("Be helpful"), 0.7, None, "llm.complete")
            .as_deref(),
        Some("This is the AI response.")
    );
}

#[test]
fn test_request_telemetry_records_metadata_and_token_usage() {
    let provider = ScriptedProvider::with_responses(vec![Ok(ProviderResponse {
        content: Some("done".to_owned()),
        usage: TokenUsage {
            prompt_tokens: Some(41),
            completion_tokens: Some(7),
            total_tokens: Some(48),
        },
        ..ProviderResponse::default()
    })]);
    let recorder = Arc::new(RecordingAttempts::default());
    let (service, _) = service_with_provider("groq/openai/gpt-oss-20b", provider);
    let recorder_port: Arc<dyn AttemptRecorder> = recorder.clone();
    let service = service.with_attempt_recorder(recorder_port);
    assert_eq!(
        service
            .complete("secret prompt body", None, 0.7, None, "dig.flavor")
            .as_deref(),
        Some("done")
    );
    let attempts = recorder.0.lock().expect("attempt lock");
    let attempt = attempts.first().expect("attempt");
    assert_eq!(attempt.feature, "dig.flavor");
    assert_eq!(attempt.operation, AiOperation::Completion);
    assert_eq!(attempt.provider, "groq");
    assert_eq!(attempt.model, "openai/gpt-oss-20b");
    assert!(attempt.success);
    assert_eq!(attempt.usage.total_tokens, Some(48));
    assert!(!format!("{attempt:?}").contains("secret prompt body"));
    assert!(!format!("{attempt:?}").contains("test-api-key"));
}

#[test]
fn test_request_telemetry_records_failures() {
    let provider = ScriptedProvider::with_responses(vec![Err(ProviderError::new(
        ProviderErrorKind::Unavailable,
        "provider unavailable",
    ))]);
    let recorder = Arc::new(RecordingAttempts::default());
    let (service, _) = service_with_provider("cerebras/zai-glm-4.7", provider);
    let recorder_port: Arc<dyn AttemptRecorder> = recorder.clone();
    let service = service.with_attempt_recorder(recorder_port);
    assert_eq!(service.complete("prompt", None, 0.7, None, "ask.sql"), None);
    let attempts = recorder.0.lock().expect("attempt lock");
    assert_eq!(attempts[0].feature, "ask.sql");
    assert!(!attempts[0].success);
    assert_eq!(attempts[0].error_kind, Some(ProviderErrorKind::Unavailable));
    assert_eq!(attempts[0].usage.prompt_tokens, None);
}

fn column(name: &str, data_type: &str) -> ColumnSchema {
    ColumnSchema::new(name, data_type, true, false)
}

fn table(name: &str, columns: &[(&str, &str)]) -> TableSchema {
    TableSchema {
        name: name.to_owned(),
        columns: columns
            .iter()
            .map(|(name, data_type)| column(name, data_type))
            .collect(),
        foreign_keys: Vec::new(),
    }
}

fn base_schemas() -> Vec<TableSchema> {
    vec![
        table(
            "players",
            &[
                ("discord_id", "INTEGER"),
                ("guild_id", "INTEGER"),
                ("discord_username", "TEXT"),
                ("wins", "INTEGER"),
                ("losses", "INTEGER"),
            ],
        ),
        table(
            "matches",
            &[
                ("match_id", "INTEGER"),
                ("guild_id", "INTEGER"),
                ("winner", "TEXT"),
            ],
        ),
        table(
            "bets",
            &[
                ("guild_id", "INTEGER"),
                ("discord_id", "INTEGER"),
                ("amount", "INTEGER"),
            ],
        ),
        table(
            "loan_state",
            &[
                ("guild_id", "INTEGER"),
                ("discord_id", "INTEGER"),
                ("total_loans_taken", "INTEGER"),
            ],
        ),
        table(
            "player_steam_ids",
            &[("discord_id", "INTEGER"), ("steam_id", "INTEGER")],
        ),
    ]
}

fn validator_service() -> SQLQueryService {
    let repository: Arc<dyn AIQueryRepository> =
        Arc::new(InMemoryAIQueryRepository::new(Vec::<TableSchema>::new()));
    SQLQueryService::new(repository)
}

fn service_with_schemas(
    schemas: Vec<TableSchema>,
) -> (SQLQueryService, Arc<InMemoryAIQueryRepository>) {
    let repository = Arc::new(InMemoryAIQueryRepository::new(schemas));
    let port: Arc<dyn AIQueryRepository> = repository.clone();
    (SQLQueryService::new(port), repository)
}

fn assert_valid(service: &SQLQueryService, sql: &str) {
    assert!(
        service.validate_sql(sql).is_ok(),
        "expected valid SQL: {sql:?}, got {:?}",
        service.validate_sql(sql)
    );
}

fn assert_invalid(service: &SQLQueryService, sql: &str, message: &str) {
    let error = service
        .validate_sql(sql)
        .expect_err("expected SQL validation failure");
    assert!(
        error
            .0
            .to_ascii_lowercase()
            .contains(&message.to_ascii_lowercase()),
        "unexpected error for {sql:?}: {error}"
    );
}

macro_rules! sql_validation_tests {
    (
        $(
            $name:ident, $sql:expr, $expected:expr, $message:expr
        );+ $(;)?
    ) => {
        $(
            #[test]
            fn $name() {
                let service = validator_service();
                let result = service.validate_sql($sql);
                if $expected {
                    assert!(result.is_ok(), "expected valid SQL {:?}, got {:?}", $sql, result);
                } else {
                    let error = result.expect_err("expected SQL validation failure");
                    assert!(
                        error.0.to_ascii_lowercase().contains($message),
                        "unexpected error for {:?}: {}",
                        $sql,
                        error
                    );
                }
            }
        )+
    };
}

sql_validation_tests! {
    test_validate_sql_rejects_low_priority_multiplier_anywhere_structural_select,
    "SELECT low_priority_gain_multiplier FROM rating_history", false, "forbidden";
    test_validate_sql_rejects_low_priority_multiplier_anywhere_structural_where,
    "SELECT p.discord_username FROM players p JOIN rating_history rh ON p.discord_id = rh.discord_id WHERE rh.low_priority_gain_multiplier > 1", false, "forbidden";
    test_validate_sql_rejects_low_priority_multiplier_anywhere_structural_join_on,
    "SELECT p.discord_username FROM players p JOIN rating_history rh ON rh.low_priority_gain_multiplier > 1", false, "forbidden";
    test_validate_sql_rejects_low_priority_multiplier_anywhere_structural_group_by,
    "SELECT COUNT(*) FROM rating_history GROUP BY low_priority_gain_multiplier", false, "forbidden";
    test_validate_sql_rejects_low_priority_multiplier_anywhere_structural_having,
    "SELECT COUNT(*) FROM rating_history HAVING MAX(low_priority_gain_multiplier) > 1", false, "forbidden";
    test_validate_sql_rejects_low_priority_multiplier_anywhere_structural_order_by,
    "SELECT rating_after FROM rating_history ORDER BY low_priority_gain_multiplier", false, "forbidden";
    test_validate_sql_rejects_low_priority_multiplier_anywhere_structural_function,
    "SELECT max(low_priority_gain_multiplier) FROM rating_history", false, "forbidden";
    test_validate_sql_rejects_low_priority_multiplier_anywhere_structural_json_object,
    "SELECT json_object('multiplier', low_priority_gain_multiplier) FROM rating_history", false, "forbidden";
    test_validate_sql_rejects_low_priority_multiplier_anywhere_structural_quoted,
    "SELECT \"low_priority_gain_multiplier\" FROM rating_history", false, "forbidden";

    test_validate_sql_ignores_forbidden_identifier_text_in_string_literal,
    "SELECT discord_username FROM players WHERE discord_username = 'low_priority_gain_multiplier'", true, "";

    test_validate_sql_rejects_single_quoted_identifier_bypass_from_table,
    "SELECT reason FROM 'low_priority_state'", false, "not allowed";
    test_validate_sql_rejects_single_quoted_identifier_bypass_join_table,
    "SELECT lp.reason FROM players p JOIN 'low_priority_state' lp ON 1 = 1", false, "not allowed";
    test_validate_sql_rejects_single_quoted_identifier_bypass_comma_join_table,
    "SELECT reason FROM players, 'low_priority_state'", false, "not allowed";
    test_validate_sql_rejects_single_quoted_identifier_bypass_qualified_blocked_column,
    "SELECT p.'discord_id' FROM players p", false, "blocked column";
    test_validate_sql_rejects_single_quoted_identifier_bypass_qualified_forbidden_select,
    "SELECT rh.'low_priority_gain_multiplier' FROM rating_history rh", false, "forbidden";
    test_validate_sql_rejects_single_quoted_identifier_bypass_qualified_forbidden_where,
    "SELECT rh.rating_after FROM rating_history rh WHERE rh.'low_priority_gain_multiplier' > 1", false, "forbidden";
    test_validate_sql_rejects_single_quoted_identifier_bypass_using_column,
    "SELECT rh.rating_after FROM rating_history rh JOIN rating_history b USING ('low_priority_gain_multiplier')", false, "forbidden";
    test_validate_sql_rejects_single_quoted_identifier_bypass_using_second_column,
    "SELECT a.rating_after FROM rating_history a JOIN rating_history b USING (match_id, 'low_priority_gain_multiplier')", false, "forbidden";
    test_validate_sql_rejects_single_quoted_identifier_bypass_quoted_table_after_schema,
    "SELECT discord_username FROM main.'players'", false, "schema-qualified";
    test_validate_sql_rejects_single_quoted_identifier_bypass_quoted_schema,
    "SELECT discord_username FROM 'main'.players", false, "schema-qualified";
    test_validate_sql_rejects_single_quoted_identifier_bypass_quoted_schema_and_table,
    "SELECT discord_username FROM 'main'.'players'", false, "schema-qualified";

    test_validate_sql_allows_standalone_single_quoted_literals_projection,
    "SELECT 'low_priority_gain_multiplier' AS label FROM rating_history", true, "";
    test_validate_sql_allows_standalone_single_quoted_literals_where,
    "SELECT discord_username FROM players WHERE discord_username = 'low_priority_gain_multiplier'", true, "";
    test_validate_sql_allows_standalone_single_quoted_literals_function,
    "SELECT upper('low_priority_state') AS label FROM players", true, "";
    test_validate_sql_allows_standalone_single_quoted_literals_function_with_two_literals,
    "SELECT printf('%s', 'main') AS label FROM players", true, "";

    test_validate_sql_rejects_parenthesized_source_bypass_from,
    "SELECT reason FROM ('low_priority_state')", false, "not allowed";
    test_validate_sql_rejects_parenthesized_source_bypass_join,
    "SELECT lp.reason FROM players JOIN ('low_priority_state') lp ON 1", false, "not allowed";
    test_validate_sql_rejects_parenthesized_source_bypass_comma_join,
    "SELECT reason FROM players, ('low_priority_state')", false, "not allowed";
    test_validate_sql_rejects_parenthesized_source_bypass_parenthesized_join,
    "SELECT reason FROM (players JOIN ('low_priority_state') ON 1)", false, "not allowed";
    test_validate_sql_rejects_parenthesized_source_bypass_parenthesized_comma_join,
    "SELECT reason FROM (players, low_priority_state)", false, "not allowed";
    test_validate_sql_rejects_parenthesized_source_bypass_nested_parenthesized_comma_join,
    "SELECT reason FROM ((players, low_priority_state))", false, "not allowed";
    test_validate_sql_rejects_parenthesized_source_bypass_parenthesized_comma_join_quoted,
    "SELECT reason FROM (players, ('low_priority_state'))", false, "not allowed";
    test_validate_sql_rejects_parenthesized_source_bypass_separately_parenthesized_comma_join,
    "SELECT reason FROM ((players), (low_priority_state))", false, "not allowed";
    test_validate_sql_rejects_parenthesized_source_bypass_pragma_function,
    "SELECT name FROM (pragma_table_info('players'))", false, "not allowed";
    test_validate_sql_rejects_parenthesized_source_bypass_single_quoted_pragma_function,
    "SELECT name FROM ('pragma_table_info'('players'))", false, "not allowed";

    test_validate_sql_rejects_in_source_bypass_table,
    "SELECT discord_username FROM players WHERE discord_username IN low_priority_state", false, "not allowed";
    test_validate_sql_rejects_in_source_bypass_quoted_table,
    "SELECT discord_username FROM players WHERE discord_username IN 'low_priority_state'", false, "not allowed";
    test_validate_sql_rejects_in_source_bypass_table_function,
    "SELECT discord_username FROM players WHERE discord_username IN pragma_table_info('players')", false, "not allowed";
    test_validate_sql_rejects_in_source_bypass_quoted_table_function,
    "SELECT discord_username FROM players WHERE discord_username IN 'pragma_table_info'('players')", false, "not allowed";

    test_validate_sql_rejects_blocked_columns_inside_projection_expressions_grouped,
    "SELECT (discord_id) FROM players", false, "blocked column";
    test_validate_sql_rejects_blocked_columns_inside_projection_expressions_function,
    "SELECT max(discord_id) FROM players", false, "blocked column";
    test_validate_sql_rejects_blocked_columns_inside_projection_expressions_cast,
    "SELECT CAST(discord_id AS TEXT) FROM players", false, "blocked column";
    test_validate_sql_rejects_blocked_columns_inside_projection_expressions_json_object,
    "SELECT json_object('player_id', discord_id) FROM players", false, "blocked column";
    test_validate_sql_rejects_blocked_columns_inside_projection_expressions_nested_parentheses,
    "SELECT coalesce((discord_id), 0) FROM players", false, "blocked column";

    test_validate_sql_allows_legitimate_parenthesized_sql_subquery_source,
    "SELECT p.discord_username FROM (SELECT discord_username FROM players) p", true, "";
    test_validate_sql_allows_legitimate_parenthesized_sql_parenthesized_join,
    "SELECT p.discord_username FROM (players p JOIN matches m ON p.wins = m.match_id)", true, "";
    test_validate_sql_allows_legitimate_parenthesized_sql_subquery_where,
    "SELECT (SELECT COUNT(*) FROM bets WHERE discord_id = p.discord_id) FROM players p", true, "";
    test_validate_sql_allows_legitimate_parenthesized_sql_in_literal_list,
    "SELECT discord_username FROM players WHERE discord_username IN ('low_priority_state')", true, "";
    test_validate_sql_allows_legitimate_parenthesized_sql_table_function_argument_comma,
    "SELECT value FROM (json_each('[1,2]', '$'))", true, "";

    test_validate_sql_allows_projection_blocked_columns_in_join_predicates,
    "SELECT p.discord_username FROM players p JOIN rating_history rh ON p.discord_id = rh.discord_id WHERE rh.discord_id > 0", true, "";
}

#[test]
fn test_schema_context_hides_low_priority_history_multiplier() {
    let repository = Arc::new(InMemoryAIQueryRepository::new([TableSchema {
        name: "rating_history".to_owned(),
        columns: vec![
            ColumnSchema::new("guild_id", "INTEGER", true, false),
            ColumnSchema::new("rating_after", "REAL", true, false),
            ColumnSchema::new("low_priority_gain_multiplier", "REAL", true, false),
        ],
        foreign_keys: Vec::new(),
    }]));
    let port: Arc<dyn AIQueryRepository> = repository;
    let context = SQLQueryService::new(port)
        .schema_context()
        .expect("schema context");
    assert!(context.contains("### rating_history"));
    assert!(!context.contains("low_priority_gain_multiplier"));
}

#[test]
fn test_validate_sql_rejects_non_select() {
    let service = validator_service();
    for sql in [
        "INSERT INTO players VALUES (1, 'test')",
        "UPDATE players SET name='test'",
        "DELETE FROM players",
    ] {
        assert_invalid(&service, sql, "select");
    }
}

#[test]
fn test_validate_sql_rejects_dangerous_keywords() {
    let service = validator_service();
    for sql in [
        "SELECT wins FROM players; DROP TABLE players",
        "SELECT wins FROM players WHERE wins=1; TRUNCATE players",
        "SELECT wins FROM players; ALTER TABLE players ADD x INT",
    ] {
        assert!(service.validate_sql(sql).is_err(), "should reject {sql}");
    }
}

#[test]
fn test_validate_sql_rejects_blocked_columns() {
    let service = validator_service();
    for blocked in BLOCKED_COLUMNS {
        assert_invalid(&service, &format!("SELECT {blocked} FROM players"), blocked);
    }
}

#[test]
fn test_validate_sql_allows_valid_select() {
    let service = validator_service();
    for sql in [
        "SELECT discord_username, wins, losses FROM players",
        "SELECT COUNT(*) FROM matches",
        "SELECT discord_username FROM players ORDER BY wins DESC LIMIT 10",
    ] {
        assert_valid(&service, sql);
    }
}

#[test]
fn test_validate_sql_rejects_blocked_columns_via_union() {
    let service = validator_service();
    assert_invalid(
        &service,
        "SELECT discord_username FROM players UNION SELECT discord_id FROM players",
        "blocked column",
    );
    assert!(
        service
            .validate_sql(
                "SELECT discord_username FROM players UNION ALL SELECT steam_id FROM player_steam_ids"
            )
            .is_err()
    );
}

#[test]
fn test_validate_sql_rejects_quoted_identifier_bypass() {
    let service = validator_service();
    for sql in [
        "SELECT \"discord_id\" FROM players",
        "SELECT p.\"discord_id\" FROM players p",
        "SELECT [discord_id] FROM players",
        "SELECT avoider_discord_id FROM \"soft_avoids\"",
        "SELECT \"t\".* FROM players \"t\"",
        "SELECT * FROM \"players\"",
    ] {
        assert!(service.validate_sql(sql).is_err(), "quoted bypass: {sql}");
    }
}

#[test]
fn test_validate_sql_rejects_blocked_columns_via_subquery() {
    let service = validator_service();
    for sql in [
        "SELECT (SELECT 1 FROM matches LIMIT 1) AS a, discord_id FROM players",
        "SELECT (SELECT discord_id FROM players LIMIT 1) AS x FROM matches",
    ] {
        assert_invalid(&service, sql, "blocked column");
    }
}

#[test]
fn test_validate_sql_allows_blocked_column_only_in_subquery_where() {
    let service = validator_service();
    assert_valid(
        &service,
        "SELECT (SELECT COUNT(*) FROM bets WHERE discord_id = p.discord_id) AS bet_count, discord_username FROM players p",
    );
}

#[test]
fn test_validate_sql_allows_semicolon_inside_string_literal() {
    assert_valid(
        &validator_service(),
        "SELECT discord_username FROM players WHERE discord_username = 'a;b'",
    );
}

#[test]
fn test_validate_sql_literal_value_does_not_trip_keyword_guard() {
    assert_valid(
        &validator_service(),
        "SELECT discord_username FROM players WHERE discord_username = 'DROP'",
    );
}

#[test]
fn test_validate_sql_rejects_schema_qualified_references() {
    let service = validator_service();
    for sql in [
        "SELECT discord_username FROM main.players",
        "SELECT discord_username FROM temp.players",
        "SELECT p.discord_username FROM main.players p",
    ] {
        assert_invalid(&service, sql, "schema-qualified");
    }
    assert_valid(
        &service,
        "SELECT p.discord_username FROM players p JOIN loan_state l ON p.discord_id = l.discord_id",
    );
    assert_valid(
        &service,
        "SELECT discord_username FROM players WHERE discord_username = 'main'",
    );
}

#[test]
fn test_validate_sql_rejects_comment_tokens() {
    let service = validator_service();
    for sql in [
        "SELECT discord_username FROM main/**/.players",
        "SELECT reason FROM/**/soft_avoids",
        "SELECT/**/* FROM players",
        "SELECT reason FROM--\nsoft_avoids",
    ] {
        assert_invalid(&service, sql, "comment");
    }
}

#[test]
fn test_validate_sql_allows_comment_chars_inside_string_literal() {
    let service = validator_service();
    for sql in [
        "SELECT discord_username FROM players WHERE discord_username = '--'",
        "SELECT discord_username FROM players WHERE discord_username = 'a/*b'",
    ] {
        assert_valid(&service, sql);
    }
}

#[test]
fn test_validate_sql_rejects_player_trivia_tables() {
    let service = validator_service();
    for sql in [
        "SELECT question_text FROM player_trivia_questions",
        "SELECT score FROM player_trivia_sessions",
    ] {
        assert_invalid(&service, sql, "not allowed");
    }
}

#[test]
fn test_validate_sql_rejects_all_survey_tables() {
    let service = validator_service();
    for (table, projection) in [
        ("surveys", "title"),
        ("survey_questions", "prompt"),
        ("survey_recipients", "delivery_status"),
        ("survey_answers", "answer_text"),
    ] {
        assert_invalid(
            &service,
            &format!("SELECT {projection} FROM {table}"),
            "not allowed",
        );
    }
}

#[test]
fn test_validate_sql_blocks_unscoped_table_not_in_blocklist() {
    let schemas = vec![
        table(
            "players",
            &[("guild_id", "INTEGER"), ("discord_username", "TEXT")],
        ),
        table("new_global_table", &[("label", "TEXT")]),
    ];
    let (service, _) = service_with_schemas(schemas);
    assert_invalid(
        &service,
        "SELECT label FROM new_global_table",
        "not allowed",
    );
    assert_valid(&service, "SELECT discord_username FROM players");
}

#[test]
fn test_schema_context_bulk_metadata_preserves_output_and_cache() {
    let mut schemas = base_schemas();
    schemas[0].foreign_keys.push(ForeignKey {
        from: "guild_id".to_owned(),
        table: "matches".to_owned(),
        to: "guild_id".to_owned(),
    });
    let (service, repository) = service_with_schemas(schemas);
    let first = service.schema_context().expect("schema context");
    let second = service.schema_context().expect("cached schema context");
    assert_eq!(first, second);
    assert!(first.contains("### players"));
    assert!(first.contains("players.guild_id = matches.guild_id"));
    assert_eq!(repository.metadata_read_count(), 1);
}

#[test]
fn test_schema_context_bulk_metadata_blocks_unscoped_tables_fail_closed() {
    let schemas = vec![
        table(
            "players",
            &[("guild_id", "INTEGER"), ("discord_username", "TEXT")],
        ),
        table("new_global_table", &[("label", "TEXT")]),
    ];
    let (service, repository) = service_with_schemas(schemas);
    let context = service.schema_context().expect("schema context");
    assert!(context.contains("### players"));
    assert!(!context.contains("new_global_table"));
    assert!(
        service
            .blocked_table_names()
            .expect("blocked names")
            .contains("new_global_table")
    );
    assert_eq!(repository.metadata_read_count(), 1);
}

#[test]
fn test_no_guild_id_tables_are_blocked_or_allowlisted() {
    let schemas = base_schemas();
    let (service, repository) = service_with_schemas(schemas);
    let all = repository.get_all_tables().expect("tables");
    let scoped = repository.get_guild_scoped_tables().expect("scoped tables");
    let blocked = service.blocked_table_names().expect("blocked tables");
    let allowlisted = UNSCOPED_TABLE_ALLOWLIST
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let unclassified = all.into_iter().filter(|table| {
        !scoped.contains(table)
            && !blocked.contains(&table.to_ascii_lowercase())
            && !allowlisted.contains(table.as_str())
    });
    assert_eq!(unclassified.count(), 0);
}

#[test]
fn test_blocked_tables_blocklist() {
    let blocked = BLOCKED_TABLES.iter().copied().collect::<BTreeSet<_>>();
    for table in [
        "sqlite_sequence",
        "schema_migrations",
        "pending_matches",
        "guild_config",
        "llm_request_attempts",
        "match_bans",
        "player_trivia_questions",
        "player_trivia_sessions",
        "low_priority_state",
        "lobby_suspensions",
        "moderation_events",
        "surveys",
        "survey_questions",
        "survey_recipients",
        "survey_answers",
    ] {
        assert!(blocked.contains(table), "missing {table}");
    }
}

#[test]
fn test_blocked_columns_blocklist() {
    let blocked = BLOCKED_COLUMNS.iter().copied().collect::<BTreeSet<_>>();
    assert!(blocked.contains("discord_id"));
    assert!(blocked.contains("steam_id"));
    assert!(blocked.contains("dotabuff_url"));
}

#[derive(Default)]
struct FakeFlavorAi {
    flavors: Mutex<VecDeque<Result<Option<String>, String>>>,
    insights: Mutex<VecDeque<Result<Option<String>, String>>>,
    flavor_inputs: Mutex<Vec<FlavorPromptInput>>,
    insight_calls: AtomicUsize,
}

impl FlavorAiPort for FakeFlavorAi {
    fn generate_flavor(&self, input: &FlavorPromptInput) -> Result<Option<String>, String> {
        self.flavor_inputs
            .lock()
            .expect("flavor input lock")
            .push(input.clone());
        self.flavors
            .lock()
            .expect("flavor result lock")
            .pop_front()
            .unwrap_or(Ok(None))
    }

    fn complete_insight(
        &self,
        _prompt: &str,
        _system_prompt: &str,
        _feature: &str,
    ) -> Result<Option<String>, String> {
        self.insight_calls.fetch_add(1, Ordering::SeqCst);
        self.insights
            .lock()
            .expect("insight result lock")
            .pop_front()
            .unwrap_or(Ok(None))
    }
}

struct SequenceBettingRandom(Mutex<VecDeque<usize>>);

impl SequenceBettingRandom {
    fn new(values: impl IntoIterator<Item = usize>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(values.into_iter().collect())))
    }
}

impl BettingFlavorRandom for SequenceBettingRandom {
    fn choose_index(&self, upper_bound: usize) -> usize {
        if upper_bound == 0 {
            return 0;
        }
        self.0
            .lock()
            .expect("betting random lock")
            .pop_front()
            .unwrap_or_default()
            % upper_bound
    }
}

struct FakePlayerPort {
    context: Mutex<Option<PlayerContext>>,
}

impl PlayerContextPort for FakePlayerPort {
    fn player_context(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
    ) -> Result<Option<PlayerContext>, String> {
        Ok(self.context.lock().expect("player lock").clone())
    }
}

struct FakeConfig(bool);

impl GuildAiConfigPort for FakeConfig {
    fn ai_enabled(&self, _guild_id: i64) -> Result<bool, String> {
        Ok(self.0)
    }
}

fn sample_context() -> PlayerContext {
    PlayerContext::from_record(PlayerRecord {
        username: "TestPlayer".to_owned(),
        balance: 100,
        wins: 10,
        losses: 5,
        lowest_balance: Some(-20),
    })
}

fn flavor_service(
    ai: Arc<FakeFlavorAi>,
    enabled: Option<bool>,
    context: Option<PlayerContext>,
) -> FlavorTextService {
    let ai_port: Arc<dyn FlavorAiPort> = ai;
    let players: Arc<dyn PlayerContextPort> = Arc::new(FakePlayerPort {
        context: Mutex::new(context),
    });
    let service = FlavorTextService::new(ai_port, players);
    if let Some(enabled) = enabled {
        let config: Arc<dyn GuildAiConfigPort> = Arc::new(FakeConfig(enabled));
        service.with_config(config)
    } else {
        service
    }
}

#[test]
fn test_generate_event_flavor_returns_comment() {
    let ai = Arc::new(FakeFlavorAi {
        flavors: Mutex::new(vec![Ok(Some("Snarky AI comment".to_owned()))].into()),
        ..FakeFlavorAi::default()
    });
    let service = flavor_service(ai.clone(), Some(true), Some(sample_context()));
    let result = service.generate_event_flavor(
        Some(123),
        FlavorEvent::LoanTaken,
        456,
        BTreeMap::from([("amount".to_owned(), Value::Integer(50))]),
    );
    assert_eq!(result.as_deref(), Some("Snarky AI comment"));
    assert_eq!(ai.flavor_inputs.lock().expect("input lock").len(), 1);
}

#[test]
fn test_generate_event_flavor_respects_ai_disabled() {
    let ai = Arc::new(FakeFlavorAi::default());
    let service = flavor_service(ai.clone(), Some(false), Some(sample_context()));
    let result =
        service.generate_event_flavor(Some(123), FlavorEvent::LoanTaken, 456, BTreeMap::new());
    assert!(
        FlavorEvent::LoanTaken
            .examples()
            .contains(&result.as_deref().expect("fallback"))
    );
    assert!(ai.flavor_inputs.lock().expect("input lock").is_empty());
}

#[test]
fn test_generate_event_flavor_rejects_links_with_fallback() {
    let ai = Arc::new(FakeFlavorAi {
        flavors: Mutex::new(
            vec![Ok(Some(
                "Join my server discord.gg/evil for free coins".to_owned(),
            ))]
            .into(),
        ),
        ..FakeFlavorAi::default()
    });
    let service = flavor_service(ai, Some(true), Some(sample_context()));
    let result =
        service.generate_event_flavor(Some(123), FlavorEvent::LoanTaken, 456, BTreeMap::new());
    assert!(
        FlavorEvent::LoanTaken
            .examples()
            .contains(&result.as_deref().expect("fallback"))
    );
}

#[test]
fn test_generate_event_flavor_neutralizes_mentions() {
    let ai = Arc::new(FakeFlavorAi {
        flavors: Mutex::new(vec![Ok(Some("big win @everyone bow down".to_owned()))].into()),
        ..FakeFlavorAi::default()
    });
    let service = flavor_service(ai, Some(true), Some(sample_context()));
    assert_eq!(
        service
            .generate_event_flavor(Some(123), FlavorEvent::LoanTaken, 456, BTreeMap::new())
            .as_deref(),
        Some("big win ＠everyone bow down")
    );
}

#[test]
fn test_generate_betting_lines_reject_links_with_fallback() {
    let ai = Arc::new(FakeFlavorAi {
        flavors: Mutex::new(
            vec![
                Ok(Some("Bet now at https://evil.example".to_owned())),
                Ok(Some("Bet now at https://evil.example".to_owned())),
            ]
            .into(),
        ),
        ..FakeFlavorAi::default()
    });
    let service = flavor_service(ai, Some(true), Some(sample_context()));
    let last_call = service.generate_betting_last_call(Some(123), BTreeMap::new());
    let warning = service.generate_betting_warning(Some(123), BTreeMap::new());
    assert!(
        FlavorEvent::BetLastCall
            .examples()
            .contains(&last_call.as_deref().expect("fallback"))
    );
    assert!(
        FlavorEvent::BetLastCall
            .examples()
            .contains(&warning.as_deref().expect("fallback"))
    );
}

fn betting_details() -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "standings".to_owned(),
            Value::Text("R 100 | D 200".to_owned()),
        ),
        ("seconds_left".to_owned(), Value::Integer(60)),
    ])
}

#[test]
fn test_disabled_returns_static_without_leader() {
    let ai = Arc::new(FakeFlavorAi::default());
    let service = flavor_service(ai.clone(), Some(false), Some(sample_context()));
    let result = service.generate_betting_last_call(Some(123), betting_details());
    assert!(
        BETTING_LAST_CALL_EXAMPLES.contains(&result.as_deref().expect("fallback")),
        "disabled betting flavor must use a static example"
    );
    assert!(ai.flavor_inputs.lock().expect("input lock").is_empty());
}

#[test]
fn test_disabled_returns_static_even_with_leader() {
    let ai = Arc::new(FakeFlavorAi::default());
    let service = flavor_service(ai.clone(), Some(false), Some(sample_context()));
    let mut details = betting_details();
    details.insert("leader_discord_id".to_owned(), Value::Integer(999));
    details.insert("leader_amount".to_owned(), Value::Integer(500));
    let result = service.generate_betting_last_call(Some(123), details);
    assert!(BETTING_LAST_CALL_EXAMPLES.contains(&result.as_deref().expect("fallback")));
    assert!(ai.flavor_inputs.lock().expect("input lock").is_empty());
}

#[test]
fn test_enabled_empty_pool_calls_ai_player_agnostic() {
    let ai = Arc::new(FakeFlavorAi {
        flavors: Mutex::new(vec![Ok(Some("EMPTY POOL TAUNT".to_owned()))].into()),
        ..FakeFlavorAi::default()
    });
    let random = SequenceBettingRandom::new([0]);
    let service =
        flavor_service(ai.clone(), Some(true), Some(sample_context())).with_betting_random(random);
    let result = service.generate_betting_last_call(Some(123), betting_details());
    assert_eq!(result.as_deref(), Some("EMPTY POOL TAUNT"));
    let inputs = ai.flavor_inputs.lock().expect("input lock");
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].event_type, "bet_last_call");
    assert_eq!(inputs[0].event_details["has_bettor"], Value::Bool(false));
    assert_eq!(
        inputs[0].event_details["angle"],
        Value::Text("taunt_crowd".to_owned())
    );
    assert!(inputs[0].player_context.is_empty());
    assert_eq!(
        inputs[0]
            .persona
            .as_ref()
            .map(|persona| persona.key.as_str()),
        Some("sleazy_bookie")
    );
}

#[test]
fn test_enabled_with_leader_uses_player_context() {
    let ai = Arc::new(FakeFlavorAi {
        flavors: Mutex::new(vec![Ok(Some("ROAST THE LEADER".to_owned()))].into()),
        ..FakeFlavorAi::default()
    });
    let mut context = sample_context();
    context.username = "BigBettor".to_owned();
    context.balance = 1_234;
    context.bet_win_rate = Some(0.0);
    context.degen_score = Some(88);
    context.bankruptcy_count = 3;
    let random = SequenceBettingRandom::new([1, 0]);
    let service = flavor_service(ai.clone(), Some(true), Some(context)).with_betting_random(random);
    let mut details = betting_details();
    details.insert(
        "standings".to_owned(),
        Value::Text("R 100 | D 800".to_owned()),
    );
    details.insert("leader_discord_id".to_owned(), Value::Integer(777));
    details.insert("leader_amount".to_owned(), Value::Integer(800));
    details.insert("leader_team".to_owned(), Value::Text("dire".to_owned()));
    assert_eq!(
        service
            .generate_betting_last_call(Some(123), details)
            .as_deref(),
        Some("ROAST THE LEADER")
    );
    let inputs = ai.flavor_inputs.lock().expect("input lock");
    assert_eq!(inputs[0].event_details["has_bettor"], Value::Bool(true));
    assert_eq!(
        inputs[0].event_details["leader_name"],
        Value::Text("BigBettor".to_owned())
    );
    assert_eq!(
        inputs[0].event_details["angle"],
        Value::Text("roast_leader".to_owned())
    );
    assert_eq!(
        inputs[0].player_context["username"],
        Value::Text("BigBettor".to_owned())
    );
    assert_eq!(
        inputs[0].player_context["bet_win_rate"],
        Value::Text("0%".to_owned())
    );
    assert!(!inputs[0].event_details.contains_key("leader_discord_id"));
}

#[test]
fn test_enabled_ai_none_falls_back_to_static() {
    let ai = Arc::new(FakeFlavorAi::default());
    let service = flavor_service(ai, Some(true), Some(sample_context()));
    let result = service.generate_betting_last_call(Some(123), betting_details());
    assert!(BETTING_LAST_CALL_EXAMPLES.contains(&result.as_deref().expect("fallback")));
}

#[test]
fn test_warning_disabled_returns_static() {
    let ai = Arc::new(FakeFlavorAi::default());
    let service = flavor_service(ai.clone(), Some(false), Some(sample_context()));
    let mut details = betting_details();
    details.insert("seconds_left".to_owned(), Value::Integer(300));
    details.insert(
        "underdog_side".to_owned(),
        Value::Text("radiant".to_owned()),
    );
    let result = service.generate_betting_warning(Some(123), details);
    assert!(BETTING_LAST_CALL_EXAMPLES.contains(&result.as_deref().expect("fallback")));
    assert!(ai.flavor_inputs.lock().expect("input lock").is_empty());
}

#[test]
fn test_warning_routes_to_bet_warning_event_with_underdog() {
    let ai = Arc::new(FakeFlavorAi {
        flavors: Mutex::new(vec![Ok(Some("UNDERDOG ROAST".to_owned()))].into()),
        ..FakeFlavorAi::default()
    });
    let service = flavor_service(ai.clone(), Some(true), Some(sample_context()))
        .with_betting_random(SequenceBettingRandom::new([0, 0]));
    let mut details = betting_details();
    details.insert("seconds_left".to_owned(), Value::Integer(300));
    details.insert(
        "underdog_side".to_owned(),
        Value::Text("radiant".to_owned()),
    );
    assert_eq!(
        service
            .generate_betting_warning(Some(123), details)
            .as_deref(),
        Some("UNDERDOG ROAST")
    );
    let inputs = ai.flavor_inputs.lock().expect("input lock");
    assert_eq!(inputs[0].event_type, "bet_warning");
    assert_eq!(
        inputs[0].event_details["underdog_side"],
        Value::Text("radiant".to_owned())
    );
    assert_eq!(
        inputs[0].event_details["angle"],
        Value::Text("roast_underdog".to_owned())
    );
    assert!(inputs[0].persona.is_some());
}

#[test]
fn test_warning_without_underdog_uses_crowd_angle() {
    let ai = Arc::new(FakeFlavorAi {
        flavors: Mutex::new(vec![Ok(Some("CROWD TAUNT".to_owned()))].into()),
        ..FakeFlavorAi::default()
    });
    let service = flavor_service(ai.clone(), Some(true), Some(sample_context()))
        .with_betting_random(SequenceBettingRandom::new([0]));
    let mut details = betting_details();
    details.insert("seconds_left".to_owned(), Value::Integer(300));
    assert_eq!(
        service
            .generate_betting_warning(Some(123), details)
            .as_deref(),
        Some("CROWD TAUNT")
    );
    let inputs = ai.flavor_inputs.lock().expect("input lock");
    assert_eq!(inputs[0].event_details["underdog_side"], Value::Null);
    assert_eq!(
        inputs[0].event_details["angle"],
        Value::Text("taunt_crowd".to_owned())
    );
    assert_eq!(inputs[0].event_details["has_bettor"], Value::Bool(false));
}

#[test]
fn test_warning_ai_none_falls_back_to_static() {
    let ai = Arc::new(FakeFlavorAi::default());
    let service = flavor_service(ai, Some(true), Some(sample_context()));
    let mut details = betting_details();
    details.insert("seconds_left".to_owned(), Value::Integer(300));
    details.insert("underdog_side".to_owned(), Value::Text("dire".to_owned()));
    let result = service.generate_betting_warning(Some(123), details);
    assert!(BETTING_LAST_CALL_EXAMPLES.contains(&result.as_deref().expect("fallback")));
}

#[test]
fn test_generate_data_insight_sanitized() {
    let ai = Arc::new(FakeFlavorAi {
        insights: Mutex::new(
            vec![
                Ok(Some("Queue with @Player1 more".to_owned())),
                Ok(Some("See http://evil.example for details".to_owned())),
            ]
            .into(),
        ),
        ..FakeFlavorAi::default()
    });
    let service = flavor_service(ai, Some(true), Some(sample_context()));
    assert_eq!(
        service
            .generate_data_insight(Some(123), "leaderboard", &BTreeMap::new(), None)
            .as_deref(),
        Some("Queue with ＠Player1 more")
    );
    assert_eq!(
        service.generate_data_insight(Some(123), "leaderboard", &BTreeMap::new(), None),
        None
    );
}

#[test]
fn test_generate_data_insight_returns_insight() {
    let ai = Arc::new(FakeFlavorAi {
        insights: Mutex::new(vec![Ok(Some("AI insight about data".to_owned()))].into()),
        ..FakeFlavorAi::default()
    });
    let service = flavor_service(ai.clone(), Some(true), Some(sample_context()));
    let data = BTreeMap::from([(
        "top_players".to_owned(),
        Value::List(vec![Value::Text("Player1".to_owned())]),
    )]);
    assert_eq!(
        service
            .generate_data_insight(Some(123), "leaderboard", &data, None)
            .as_deref(),
        Some("AI insight about data")
    );
    assert_eq!(ai.insight_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn test_generate_data_insight_respects_ai_disabled() {
    let ai = Arc::new(FakeFlavorAi::default());
    let service = flavor_service(ai.clone(), Some(false), Some(sample_context()));
    assert_eq!(
        service.generate_data_insight(Some(123), "leaderboard", &BTreeMap::new(), None),
        None
    );
    assert_eq!(ai.insight_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn test_player_context_from_services() {
    let context = PlayerContext::from_record(PlayerRecord {
        username: "TestPlayer".to_owned(),
        balance: 100,
        wins: 10,
        losses: 5,
        lowest_balance: None,
    });
    assert_eq!(context.username, "TestPlayer");
    assert_eq!(context.balance, 100);
    assert_eq!(context.win_rate, Some(10.0 / 15.0 * 100.0));
}

#[test]
fn test_get_stats_win_rate_variations() {
    let win_rate = |wins, losses| {
        PlayerContext::from_record(PlayerRecord {
            username: "WinRatePlayer".to_owned(),
            balance: 0,
            wins,
            losses,
            lowest_balance: None,
        })
        .win_rate
    };

    assert_eq!(win_rate(0, 0), None);
    assert_eq!(win_rate(5, 0), Some(100.0));
    assert_eq!(win_rate(0, 4), Some(0.0));
    assert_eq!(win_rate(3, 3), Some(50.0));
}

#[test]
fn test_player_context_returns_none_for_unknown_player() {
    let players = FakePlayerPort {
        context: Mutex::new(None),
    };
    assert_eq!(players.player_context(999, None).expect("lookup"), None);
}

fn row(values: &[(&str, Value)]) -> QueryRow {
    values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn repository_fixture() -> Arc<InMemoryAIQueryRepository> {
    Arc::new(InMemoryAIQueryRepository::new(base_schemas()))
}

#[test]
fn test_execute_readonly_returns_results() {
    let repository = repository_fixture();
    repository
        .insert_row(
            "players",
            row(&[
                ("discord_id", Value::Integer(123)),
                ("guild_id", Value::Integer(0)),
                ("discord_username", Value::Text("TestPlayer".to_owned())),
                ("wins", Value::Integer(0)),
                ("losses", Value::Integer(0)),
            ]),
        )
        .expect("insert");
    let results = repository
        .execute_readonly(
            "SELECT discord_username FROM players WHERE discord_id = 123",
            &[],
            25,
        )
        .expect("query");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["discord_username"].as_str(), Some("TestPlayer"));
}

#[test]
fn test_execute_readonly_respects_max_rows() {
    let repository = repository_fixture();
    for index in 0..10 {
        repository
            .insert_row(
                "players",
                row(&[
                    ("discord_id", Value::Integer(1_000 + index)),
                    ("guild_id", Value::Integer(0)),
                    ("discord_username", Value::Text(format!("Player{index}"))),
                    ("wins", Value::Integer(0)),
                    ("losses", Value::Integer(0)),
                ]),
            )
            .expect("insert");
    }
    let rows = repository
        .execute_readonly(
            "SELECT discord_username FROM players WHERE discord_id >= 1000",
            &[],
            5,
        )
        .expect("query");
    assert_eq!(rows.len(), 5);
}

#[test]
fn test_execute_readonly_rejects_writes() {
    let repository = repository_fixture();
    assert_eq!(
        repository.execute_readonly("INSERT INTO players (discord_id) VALUES (999)", &[], 25),
        Err(QueryRepositoryError::ReadOnlyViolation)
    );
}

#[test]
fn test_get_guild_scoped_tables() {
    let repository = repository_fixture();
    let scoped = repository.get_guild_scoped_tables().expect("scoped tables");
    assert!(scoped.contains("players"));
    assert!(scoped.contains("matches"));
    assert!(!scoped.contains("player_steam_ids"));
}

#[test]
fn test_get_schema_metadata_uses_one_connection_and_preserves_pragma_order() {
    let repository = repository_fixture();
    let metadata = repository.get_schema_metadata().expect("metadata");
    assert_eq!(repository.metadata_read_count(), 1);
    assert_eq!(
        metadata
            .tables
            .iter()
            .map(|table| table.name.clone())
            .collect::<Vec<_>>(),
        repository.get_all_tables().expect("tables")
    );
    for name in ["players", "matches"] {
        let table = metadata
            .tables
            .iter()
            .find(|table| table.name == name)
            .expect("table metadata");
        assert_eq!(
            table.columns,
            repository.get_table_schema(name).expect("columns")
        );
        assert_eq!(
            table.foreign_keys,
            repository.get_foreign_keys(name).expect("foreign keys")
        );
    }
}

#[test]
fn test_execute_readonly_guild_scoped_isolates_guilds() {
    let repository = repository_fixture();
    for (discord_id, guild_id, username) in [(1, 100, "alice"), (2, 100, "bob"), (3, 200, "eve")] {
        repository
            .insert_row(
                "players",
                row(&[
                    ("discord_id", Value::Integer(discord_id)),
                    ("guild_id", Value::Integer(guild_id)),
                    ("discord_username", Value::Text(username.to_owned())),
                    ("wins", Value::Integer(0)),
                    ("losses", Value::Integer(0)),
                ]),
            )
            .expect("insert");
    }
    let guild_100 = repository
        .execute_readonly_guild_scoped(
            "SELECT discord_username FROM players ORDER BY discord_username",
            Some(100),
            &[],
            25,
        )
        .expect("guild query");
    let names = guild_100
        .iter()
        .filter_map(|row| row["discord_username"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["alice", "bob"]);
    let guild_200 = repository
        .execute_readonly_guild_scoped("SELECT discord_username FROM players", Some(200), &[], 25)
        .expect("guild query");
    assert_eq!(guild_200[0]["discord_username"].as_str(), Some("eve"));
}

#[test]
fn test_execute_readonly_guild_scoped_reads_global_tables_unscoped() {
    let repository = repository_fixture();
    repository
        .insert_row(
            "player_steam_ids",
            row(&[
                ("discord_id", Value::Integer(1)),
                ("steam_id", Value::Integer(111)),
            ]),
        )
        .expect("insert");
    let rows = repository
        .execute_readonly_guild_scoped(
            "SELECT steam_id FROM player_steam_ids",
            Some(999_999),
            &[],
            25,
        )
        .expect("global query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["steam_id"].as_i64(), Some(111));
}

#[test]
fn test_execute_readonly_guild_scoped_rejects_writes() {
    let repository = repository_fixture();
    assert_eq!(
        repository.execute_readonly_guild_scoped(
            "UPDATE players SET discord_username = 'x'",
            Some(100),
            &[],
            25
        ),
        Err(QueryRepositoryError::ReadOnlyViolation)
    );
}

#[test]
fn test_all_events_have_examples() {
    for event in ALL_FLAVOR_EVENTS {
        assert!(
            !event.examples().is_empty(),
            "missing examples for {event:?}"
        );
    }
}

#[test]
fn test_flavor_events_match_expected() {
    let events = ALL_FLAVOR_EVENTS
        .into_iter()
        .map(FlavorEvent::as_str)
        .collect::<BTreeSet<_>>();
    for expected in [
        "loan_taken",
        "negative_loan",
        "bankruptcy_declared",
        "debt_paid",
        "bet_won",
        "bet_lost",
        "leverage_loss",
        "match_win",
        "mvp_callout",
    ] {
        assert!(events.contains(expected), "missing event {expected}");
    }
}

#[test]
fn test_rejects_wildcard_in_any_projection_position() {
    let service = validator_service();
    for sql in [
        "SELECT 1, * FROM players",
        "SELECT 1, p.* FROM players p",
        "SELECT (SELECT 1), * FROM players",
        "SELECT wins, * FROM players",
    ] {
        assert_invalid(&service, sql, "select *");
    }
}

#[test]
fn test_allows_star_that_is_not_a_wildcard_column() {
    let service = validator_service();
    for sql in [
        "SELECT COUNT(*) FROM players",
        "SELECT wins * 2 FROM players",
        "SELECT COUNT(*) AS c FROM players ORDER BY c LIMIT 5",
    ] {
        assert_valid(&service, sql);
    }
}

#[test]
fn test_rejects_blocked_table_reached_through_a_comma_join() {
    let service = validator_service();
    for sql in [
        "SELECT wins FROM players, player_steam_ids",
        "SELECT 1, * FROM players, player_steam_ids",
        "SELECT wins FROM players, soft_avoids",
    ] {
        let error = service.validate_sql(sql).expect_err("blocked join");
        assert!(
            error.0.to_ascii_lowercase().contains("not allowed")
                || error.0.to_ascii_lowercase().contains("select *")
        );
    }
}

#[test]
fn test_comma_outside_the_from_clause_is_not_read_as_a_table() {
    assert_valid(
        &validator_service(),
        "SELECT wins, losses FROM players WHERE wins > 3 GROUP BY wins, losses ORDER BY wins, losses LIMIT 5",
    );
}

#[test]
fn test_rejects_pragma_table_valued_functions() {
    let service = validator_service();
    for sql in [
        "SELECT name FROM pragma_table_info('soft_avoids')",
        "SELECT name FROM pragma_database_list",
    ] {
        assert_invalid(&service, sql, "not allowed");
    }
}

#[test]
fn test_rejects_wildcard_behind_a_set_quantifier() {
    let service = validator_service();
    for sql in [
        "SELECT DISTINCT * FROM players",
        "SELECT ALL * FROM players",
        "SELECT DISTINCT p.* FROM players p",
        "SELECT ALL p.* FROM players p",
        "SELECT DISTINCT*FROM players",
        "SELECT   distinct   p . *   FROM players p",
    ] {
        assert_invalid(&service, sql, "select *");
    }
}

#[test]
fn test_set_quantifier_on_real_columns_still_allowed() {
    let service = validator_service();
    for sql in [
        "SELECT DISTINCT wins FROM players",
        "SELECT DISTINCT wins, losses FROM players",
        "SELECT ALL wins FROM players",
    ] {
        assert_valid(&service, sql);
    }
}

#[test]
fn test_rejects_comma_join_written_after_an_explicit_join() {
    let service = validator_service();
    for sql in [
        "SELECT 1 FROM players p JOIN matches m ON 1=1, player_steam_ids s",
        "SELECT 1 FROM players p LEFT JOIN matches m ON 1=1, player_steam_ids s",
        "SELECT 1 FROM players p INNER JOIN matches m ON 1=1, player_steam_ids s",
        "SELECT 1 FROM players p CROSS JOIN matches m, player_steam_ids s",
        "SELECT 1 FROM players p JOIN matches m USING (x), player_steam_ids s",
        "SELECT 1 FROM players p JOIN matches m ON 1=1, sqlite_master",
        "SELECT t.name FROM players p JOIN players q ON 1=1, pragma_table_info('players') t",
    ] {
        assert_invalid(&service, sql, "not allowed");
    }
}

#[test]
fn test_join_chains_without_a_comma_join_still_allowed() {
    assert_valid(
        &validator_service(),
        "SELECT p.wins FROM players p JOIN matches m ON m.match_id = p.wins LEFT JOIN players q ON q.wins = p.wins WHERE p.wins > 1 GROUP BY p.wins ORDER BY p.wins LIMIT 5",
    );
}
