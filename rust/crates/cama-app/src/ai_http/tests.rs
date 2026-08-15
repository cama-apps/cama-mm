use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{Value as JsonValue, json};
use tempfile::NamedTempFile;

use super::*;
use crate::ai_services::{
    AiOperation, Message, ProviderCredential, ProviderOptions, ToolDefinition, ToolProperty,
    ToolPropertySchema,
};

struct ReceivedRequest {
    request_line: String,
    headers: String,
    body: JsonValue,
}

fn scripted_server(
    status: &str,
    body: Vec<u8>,
    delay: Duration,
) -> (
    String,
    mpsc::Receiver<ReceivedRequest>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let address = listener.local_addr().expect("loopback address");
    let (sender, receiver) = mpsc::channel();
    let status = status.to_owned();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let request = read_request(&mut stream);
        sender.send(request).expect("publish request");
        thread::sleep(delay);
        let headers_written = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .is_ok();
        if headers_written {
            let _ = stream.write_all(&body);
        }
    });
    (
        format!("http://{address}/v1/chat/completions"),
        receiver,
        handle,
    )
}

fn read_request(stream: &mut TcpStream) -> ReceivedRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("request timeout");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).expect("read request");
        assert!(read > 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = String::from_utf8(bytes[..header_end].to_vec()).expect("UTF-8 headers");
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content length"))
        })
        .expect("content length header");
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut buffer).expect("read body");
        assert!(read > 0, "request ended before body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    ReceivedRequest {
        request_line: header_text.lines().next().expect("request line").to_owned(),
        headers: header_text,
        body: serde_json::from_slice(&bytes[header_end..header_end + content_length])
            .expect("JSON request"),
    }
}

fn request(credential: &str) -> ProviderRequest {
    ProviderRequest {
        operation: AiOperation::Completion,
        model: "groq/qwen/qwen3.6-27b".to_owned(),
        credential: ProviderCredential::new(credential).expect("credential"),
        messages: vec![
            Message::new(MessageRole::System, "System facts"),
            Message::new(MessageRole::User, "Say hello"),
        ],
        tools: Vec::new(),
        tool_choice: ToolChoice::None,
        temperature: Some(0.7),
        max_tokens: 500,
        timeout: Duration::from_secs(1),
        num_retries: 0,
        options: ProviderOptions {
            reasoning_format: Some("parsed".to_owned()),
            ..ProviderOptions::default()
        },
    }
}

#[test]
fn production_endpoints_are_provider_specific_and_unknown_is_rejected() {
    assert_eq!(
        OpenAiCompatibleEndpoint::for_provider(ProviderName::Groq)
            .expect("Groq endpoint")
            .url,
        GROQ_CHAT_COMPLETIONS_URL
    );
    assert_eq!(
        OpenAiCompatibleEndpoint::for_provider(ProviderName::Cerebras)
            .expect("Cerebras endpoint")
            .url,
        CEREBRAS_CHAT_COMPLETIONS_URL
    );
    assert!(OpenAiCompatibleEndpoint::for_provider(ProviderName::Unknown).is_none());
}

#[test]
fn completion_sends_bearer_auth_bare_model_messages_and_supported_options() {
    let response = json!({
        "choices": [{"message": {"content": "hello", "reasoning_content": "hidden"}}],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10},
    });
    let (url, received, server) = scripted_server(
        "200 OK",
        serde_json::to_vec(&response).expect("response JSON"),
        Duration::ZERO,
    );
    let provider =
        OpenAiCompatibleProvider::new(OpenAiCompatibleEndpoint::custom(ProviderName::Groq, url))
            .expect("provider");
    let result = provider
        .complete(request("super-secret"))
        .expect("completion");
    let received = received.recv().expect("received request");
    server.join().expect("server");

    assert_eq!(received.request_line, "POST /v1/chat/completions HTTP/1.1");
    assert!(
        received
            .headers
            .contains("authorization: Bearer super-secret")
    );
    assert_eq!(received.body["model"], "qwen/qwen3.6-27b");
    assert_eq!(received.body["messages"][0]["role"], "system");
    assert_eq!(received.body["messages"][1]["content"], "Say hello");
    assert_eq!(received.body["reasoning_format"], "parsed");
    assert!(received.body.get("tool_choice").is_none());
    assert!(received.body.get("num_retries").is_none());
    assert_eq!(result.content.as_deref(), Some("hello"));
    assert_eq!(result.reasoning_content.as_deref(), Some("hidden"));
    assert_eq!(result.usage.total_tokens, Some(10));
}

#[test]
fn tools_and_required_choice_round_trip_structured_arguments() {
    let response = json!({
        "choices": [{"message": {
            "content": null,
            "tool_calls": [{"function": {
                "name": "execute_sql_query",
                "arguments": "{\"sql\":\"SELECT 1\",\"explanation\":\"one\",\"ok\":true}"
            }}]
        }}]
    });
    let (url, received, server) = scripted_server(
        "200 OK",
        serde_json::to_vec(&response).expect("response JSON"),
        Duration::ZERO,
    );
    let provider =
        OpenAiCompatibleProvider::new(OpenAiCompatibleEndpoint::custom(ProviderName::Groq, url))
            .expect("provider");
    let mut request = request("key");
    request.operation = AiOperation::ToolCall;
    request.tools = vec![ToolDefinition::sql()];
    request.tool_choice = ToolChoice::Required("execute_sql_query".to_owned());
    request.options.reasoning_effort = Some("none".to_owned());
    request.options.parallel_tool_calls = Some(false);
    let result = provider.complete(request).expect("tool response");
    let received = received.recv().expect("received request");
    server.join().expect("server");

    assert_eq!(received.body["tools"][0]["type"], "function");
    assert_eq!(
        received.body["tools"][0]["function"]["parameters"]["required"],
        json!(["sql", "explanation"])
    );
    assert_eq!(
        received.body["tools"][0]["function"]["parameters"]["properties"]["sql"]["type"],
        "string"
    );
    assert_eq!(
        received.body["tool_choice"]["function"]["name"],
        "execute_sql_query"
    );
    assert_eq!(received.body["reasoning_effort"], "none");
    assert_eq!(received.body["parallel_tool_calls"], false);
    assert_eq!(result.tool_calls[0].name, "execute_sql_query");
    assert_eq!(
        result.tool_calls[0].arguments["sql"],
        Value::Text("SELECT 1".to_owned())
    );
    assert_eq!(result.tool_calls[0].arguments["ok"], Value::Bool(true));
}

#[test]
fn array_tool_schema_preserves_cardinality_uniqueness_and_closed_object() {
    let tool = ToolDefinition {
        name: "bundle".to_owned(),
        description: "bundle".to_owned(),
        properties: vec![ToolProperty {
            name: "status".to_owned(),
            description: String::new(),
            enum_values: Vec::new(),
            schema: ToolPropertySchema::StringArray {
                min_items: 6,
                max_items: 6,
                unique_items: true,
            },
        }],
        required: vec!["status".to_owned()],
        additional_properties: Some(false),
    };
    let body = tool_body(&tool);
    let parameters = &body["function"]["parameters"];
    assert_eq!(parameters["additionalProperties"], false);
    assert_eq!(parameters["properties"]["status"]["type"], "array");
    assert_eq!(
        parameters["properties"]["status"]["items"]["type"],
        "string"
    );
    assert_eq!(parameters["properties"]["status"]["minItems"], 6);
    assert_eq!(parameters["properties"]["status"]["maxItems"], 6);
    assert_eq!(parameters["properties"]["status"]["uniqueItems"], true);
    assert!(
        parameters["properties"]["status"]
            .get("description")
            .is_none()
    );
}

#[test]
fn numeric_and_nested_object_tool_schema_matches_narrate_dig_shape() {
    let tool = ToolDefinition {
        name: "narrate_dig".to_owned(),
        description: "dig flavor".to_owned(),
        properties: vec![
            ToolProperty {
                name: "flavor_bonus_pct".to_owned(),
                description: "Signed percentage nudge.".to_owned(),
                enum_values: Vec::new(),
                schema: ToolPropertySchema::Number,
            },
            ToolProperty {
                name: "npc_appearance".to_owned(),
                description: "Optional NPC line.".to_owned(),
                enum_values: Vec::new(),
                schema: ToolPropertySchema::Object {
                    properties: vec![
                        ToolProperty {
                            name: "id_or_name".to_owned(),
                            description: "Roster id or invented title.".to_owned(),
                            enum_values: Vec::new(),
                            schema: ToolPropertySchema::String,
                        },
                        ToolProperty {
                            name: "line".to_owned(),
                            description: "One short line.".to_owned(),
                            enum_values: Vec::new(),
                            schema: ToolPropertySchema::String,
                        },
                    ],
                    required: vec!["id_or_name".to_owned(), "line".to_owned()],
                    additional_properties: Some(false),
                },
            },
        ],
        required: Vec::new(),
        additional_properties: Some(false),
    };
    let body = tool_body(&tool);
    let properties = &body["function"]["parameters"]["properties"];

    assert_eq!(properties["flavor_bonus_pct"]["type"], "number");
    assert_eq!(properties["npc_appearance"]["type"], "object");
    assert_eq!(
        properties["npc_appearance"]["required"],
        json!(["id_or_name", "line"])
    );
    assert_eq!(properties["npc_appearance"]["additionalProperties"], false);
    assert_eq!(
        properties["npc_appearance"]["properties"]["id_or_name"]["type"],
        "string"
    );
    assert_eq!(
        properties["npc_appearance"]["properties"]["line"]["type"],
        "string"
    );
    assert_eq!(
        properties["npc_appearance"]["properties"]["line"]["description"],
        "One short line."
    );
}

#[test]
fn invalid_tool_arguments_match_python_empty_argument_fallback() {
    let payload = json!({
        "choices": [{"message": {"tool_calls": [{"function": {
            "name": "generate_flavor_text", "arguments": "not-json"
        }}]}}]
    });
    let result = parse_provider_response(&payload).expect("provider response");
    assert_eq!(result.tool_calls[0].name, "generate_flavor_text");
    assert!(result.tool_calls[0].arguments.is_empty());
}

#[test]
fn status_codes_are_typed_without_leaking_response_bodies_or_credentials() {
    for (status, expected) in [
        ("401 Unauthorized", ProviderErrorKind::Authentication),
        ("429 Too Many Requests", ProviderErrorKind::RateLimit),
        ("503 Service Unavailable", ProviderErrorKind::Unavailable),
    ] {
        let (url, _received, server) = scripted_server(
            status,
            b"{\"error\":\"provider echoed super-secret\"}".to_vec(),
            Duration::ZERO,
        );
        let provider = OpenAiCompatibleProvider::new(OpenAiCompatibleEndpoint::custom(
            ProviderName::Groq,
            url,
        ))
        .expect("provider");
        let error = provider
            .complete(request("super-secret"))
            .expect_err("status rejected");
        server.join().expect("server");
        assert_eq!(error.kind, expected);
        assert!(!error.to_string().contains("super-secret"));
    }
}

#[test]
fn malformed_and_oversized_responses_fail_closed() {
    let (url, _received, server) = scripted_server("200 OK", b"not-json".to_vec(), Duration::ZERO);
    let provider =
        OpenAiCompatibleProvider::new(OpenAiCompatibleEndpoint::custom(ProviderName::Groq, url))
            .expect("provider");
    assert_eq!(
        provider
            .complete(request("key"))
            .expect_err("malformed response")
            .kind,
        ProviderErrorKind::InvalidResponse
    );
    server.join().expect("server");

    let (url, _received, server) = scripted_server(
        "200 OK",
        br#"{"choices":[{"message":{"content":"large"}}]}"#.to_vec(),
        Duration::ZERO,
    );
    let mut endpoint = OpenAiCompatibleEndpoint::custom(ProviderName::Groq, url);
    endpoint.max_response_bytes = 8;
    let provider = OpenAiCompatibleProvider::new(endpoint).expect("provider");
    assert_eq!(
        provider
            .complete(request("key"))
            .expect_err("oversized response")
            .kind,
        ProviderErrorKind::InvalidResponse
    );
    server.join().expect("server");
}

#[test]
fn hard_timeout_is_classified_and_credential_debug_is_redacted() {
    let response = json!({"choices": [{"message": {"content": "late"}}]});
    let (url, _received, server) = scripted_server(
        "200 OK",
        serde_json::to_vec(&response).expect("response JSON"),
        Duration::from_millis(200),
    );
    let provider =
        OpenAiCompatibleProvider::new(OpenAiCompatibleEndpoint::custom(ProviderName::Groq, url))
            .expect("provider");
    let mut request = request("super-secret");
    request.timeout = Duration::from_millis(30);
    assert!(!format!("{request:?}").contains("super-secret"));
    assert_eq!(
        provider.complete(request).expect_err("timeout").kind,
        ProviderErrorKind::Timeout
    );
    server.join().expect("server");
}

#[test]
fn sqlite_attempt_recorder_persists_metadata_without_bodies_or_credentials() {
    let file = NamedTempFile::new().expect("temporary database");
    let connection = Connection::open(file.path()).expect("open database");
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE llm_request_attempts (
                attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
                feature TEXT NOT NULL,
                operation TEXT NOT NULL,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                success INTEGER NOT NULL,
                latency_ms INTEGER NOT NULL,
                prompt_tokens INTEGER,
                completion_tokens INTEGER,
                total_tokens INTEGER,
                error_type TEXT,
                created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
             );",
        )
        .expect("create schema");
    drop(connection);
    let recorder = SqliteAttemptRecorder::new(file.path());
    recorder
        .record_attempt(AttemptMetadata {
            feature: "ask.sql".to_owned(),
            operation: AiOperation::ToolCall,
            provider: "groq".to_owned(),
            model: "qwen/qwen3.6-27b".to_owned(),
            success: false,
            latency: Duration::from_micros(1_234_600),
            usage: TokenUsage {
                prompt_tokens: Some(5),
                completion_tokens: Some(2),
                total_tokens: Some(7),
            },
            error_kind: Some(ProviderErrorKind::RateLimit),
        })
        .expect("record attempt");
    let connection = Connection::open(file.path()).expect("reopen database");
    let row = connection
        .query_row(
            "SELECT feature, operation, provider, model, success, latency_ms,
                    prompt_tokens, completion_tokens, total_tokens, error_type
             FROM llm_request_attempts",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .expect("attempt row");
    assert_eq!(
        row,
        (
            "ask.sql".to_owned(),
            "tool_call".to_owned(),
            "groq".to_owned(),
            "qwen/qwen3.6-27b".to_owned(),
            0,
            1_235,
            5,
            2,
            7,
            "RateLimit".to_owned(),
        )
    );
    let schema = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='llm_request_attempts'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("schema");
    for forbidden in [
        "prompt_body",
        "response_body",
        "api_key",
        "guild_id",
        "discord_id",
    ] {
        assert!(!schema.contains(forbidden));
    }
}

#[test]
fn production_factory_validates_settings_before_any_network_call() {
    let file = NamedTempFile::new().expect("temporary database path");
    assert!(matches!(
        production_ai_service_from_settings("groq/qwen/qwen3.6-27b", "key", 0.0, 500, file.path()),
        Err(ProductionAiBuildError::Config(ConfigError::ZeroTimeout))
            | Err(ProductionAiBuildError::InvalidTimeout(_))
    ));
    assert!(matches!(
        production_ai_service_from_settings("groq/qwen/qwen3.6-27b", "key", 3.0, -1, file.path()),
        Err(ProductionAiBuildError::InvalidMaxTokens(-1))
    ));
    assert!(matches!(
        production_ai_service_from_settings("unknown-model", "key", 3.0, 500, file.path()),
        Err(ProductionAiBuildError::Provider(ProviderError {
            kind: ProviderErrorKind::Loader,
            ..
        }))
    ));
}
