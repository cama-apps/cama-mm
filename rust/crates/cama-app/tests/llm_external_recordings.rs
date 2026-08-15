//! Offline replay of the OpenAI-compatible Groq/Cerebras response contract.
//!
//! The response files are deliberately small, sanitized synthetic fixtures;
//! they are not captured provider traffic and do not close the live external
//! service readiness gate. The loopback server only supplies those checked-in
//! bodies to the public Rust HTTP adapter.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cama_app::ai_http::{
    OpenAiCompatibleEndpoint, OpenAiCompatibleProvider, OpenAiCompatibleProviderLoader,
};
use cama_app::ai_services::{
    AIService, AiOperation, AiProvider, AttemptMetadata, AttemptRecorder, Message, MessageRole,
    ProviderConfig, ProviderCredential, ProviderErrorKind, ProviderLoader, ProviderName,
    ProviderOptions, ProviderRequest, ToolChoice,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

const FIXTURE_MANIFEST: &str = include_str!("fixtures/llm/manifest.json");
const MALFORMED: &[u8] = include_bytes!("fixtures/llm/malformed.txt");
const RATE_LIMITED: &[u8] = include_bytes!("fixtures/llm/rate_limited.json");
const SERVER_ERROR: &[u8] = include_bytes!("fixtures/llm/server_error.json");
const SUCCESS: &[u8] = include_bytes!("fixtures/llm/success.json");

#[derive(Clone, Debug)]
struct ResponseSpec {
    status: u16,
    body: &'static [u8],
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    request_line: String,
    headers: String,
    body: Value,
}

struct FixtureServer {
    url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    join: Option<JoinHandle<()>>,
}

impl FixtureServer {
    fn start(responses: Vec<ResponseSpec>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind LLM replay server");
        let url = format!(
            "http://{}{}",
            listener.local_addr().expect("LLM replay server address"),
            "/openai/v1/chat/completions"
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let join = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept LLM replay request");
                let request = read_request(&mut stream);
                captured
                    .lock()
                    .expect("LLM replay request lock")
                    .push(request);
                write_response(&mut stream, &response);
            }
        });
        Self {
            url,
            requests,
            join: Some(join),
        }
    }

    fn finish(mut self) -> Vec<CapturedRequest> {
        let requests = Arc::clone(&self.requests);
        self.join
            .take()
            .expect("LLM replay server join handle")
            .join()
            .expect("LLM replay server thread");
        requests.lock().expect("LLM replay request lock").clone()
    }
}

fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("LLM replay request timeout");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4_096];
    let header_end = loop {
        let read = stream.read(&mut buffer).expect("read LLM replay request");
        assert!(read > 0, "LLM replay request ended before headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).expect("UTF-8 request headers");
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content length"))
        })
        .expect("LLM replay content length");
    while bytes.len() - header_end < content_length {
        let read = stream
            .read(&mut buffer)
            .expect("read LLM replay request body");
        assert!(read > 0, "LLM replay request ended before body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])
        .expect("LLM replay JSON request body");
    CapturedRequest {
        request_line: headers.lines().next().expect("request line").to_owned(),
        headers,
        body,
    }
}

fn write_response(stream: &mut TcpStream, response: &ResponseSpec) {
    let reason = match response.status {
        200 => "OK",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Fixture Status",
    };
    let wire = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.body.len()
    );
    stream
        .write_all(wire.as_bytes())
        .expect("write LLM replay headers");
    stream
        .write_all(response.body)
        .expect("write LLM replay body");
}

fn fixture_body(name: &str) -> &'static [u8] {
    match name {
        "malformed.txt" => MALFORMED,
        "rate_limited.json" => RATE_LIMITED,
        "server_error.json" => SERVER_ERROR,
        "success.json" => SUCCESS,
        other => panic!("unknown LLM fixture body {other:?}"),
    }
}

fn request(credential: &str, model: &str) -> ProviderRequest {
    ProviderRequest {
        operation: AiOperation::Completion,
        model: model.to_owned(),
        credential: ProviderCredential::new(credential).expect("fixture credential"),
        messages: vec![
            Message::new(MessageRole::System, "Synthetic system contract."),
            Message::new(MessageRole::User, "Synthetic user contract."),
        ],
        tools: Vec::new(),
        tool_choice: ToolChoice::None,
        temperature: Some(0.2),
        max_tokens: 64,
        timeout: Duration::from_secs(1),
        num_retries: 0,
        options: ProviderOptions::default(),
    }
}

fn provider_for(provider: ProviderName, url: String) -> OpenAiCompatibleProvider {
    let endpoint = OpenAiCompatibleEndpoint::custom(provider, url);
    OpenAiCompatibleProvider::new(endpoint).expect("fixture OpenAI-compatible provider")
}

fn service_for(provider: ProviderName, model: &str, credential: &str, url: String) -> AIService {
    let config = ProviderConfig::new(
        model,
        ProviderCredential::new(credential).expect("fixture service credential"),
        Duration::from_secs(1),
        64,
    )
    .expect("fixture service config");
    let endpoint = OpenAiCompatibleEndpoint::custom(provider, url);
    let loader: Arc<dyn ProviderLoader> = Arc::new(OpenAiCompatibleProviderLoader::new(endpoint));
    AIService::new(config, loader)
}

#[derive(Default)]
struct RecordingAttemptRecorder {
    attempts: Mutex<Vec<AttemptMetadata>>,
}

impl AttemptRecorder for RecordingAttemptRecorder {
    fn record_attempt(&self, attempt: AttemptMetadata) -> Result<(), String> {
        self.attempts
            .lock()
            .expect("attempt recorder lock")
            .push(attempt);
        Ok(())
    }
}

#[test]
fn llm_fixture_manifest_is_hashed_and_redacted() {
    let manifest: Value = serde_json::from_str(FIXTURE_MANIFEST).expect("LLM manifest JSON");
    assert_eq!(manifest["schema"], "cama.external-service-recording/v1");
    assert_eq!(manifest["provider"], "openai_compatible_groq_cerebras");
    assert_eq!(manifest["provenance"]["live_network_used"], false);
    assert_eq!(manifest["provenance"]["captured_at"], Value::Null);
    assert!(manifest["provenance"]["kind"] == "sanitized_contract_fixture");
    assert!(!FIXTURE_MANIFEST.contains("Authorization"));
    assert!(!FIXTURE_MANIFEST.contains("Bearer"));
    assert!(!FIXTURE_MANIFEST.contains("api_key"));
    assert!(!String::from_utf8_lossy(SUCCESS).contains("secret"));
    assert!(!String::from_utf8_lossy(RATE_LIMITED).contains("secret"));
    assert!(!String::from_utf8_lossy(SERVER_ERROR).contains("secret"));

    let mut referenced = BTreeSet::new();
    for contract in manifest["contracts"].as_array().expect("LLM contracts") {
        for response in contract["responses"].as_array().expect("LLM responses") {
            let name = response["body"].as_str().expect("LLM fixture body name");
            let expected = response["sha256"].as_str().expect("LLM fixture body hash");
            assert_eq!(
                format!("{:x}", Sha256::digest(fixture_body(name))),
                expected,
                "LLM fixture hash mismatch for {name}"
            );
            referenced.insert(name);
        }
    }
    assert_eq!(
        referenced,
        BTreeSet::from([
            "malformed.txt",
            "rate_limited.json",
            "server_error.json",
            "success.json",
        ])
    );
}

#[test]
fn llm_success_recording_replays_through_public_provider_and_preserves_request_contract() {
    let server = FixtureServer::start(vec![ResponseSpec {
        status: 200,
        body: SUCCESS,
    }]);
    let provider = provider_for(ProviderName::Groq, server.url.clone());
    let request = request("synthetic-groq-secret", "groq/qwen/qwen3.6-27b");
    let debug = format!("{provider:?} {request:?}");
    assert!(!debug.contains("synthetic-groq-secret"));

    let response = provider.complete(request).expect("recorded LLM completion");
    assert_eq!(
        response.content.as_deref(),
        Some("Synthetic provider completion.")
    );
    assert_eq!(
        response.reasoning_content.as_deref(),
        Some("Synthetic hidden reasoning must not be displayed.")
    );
    assert_eq!(response.usage.total_tokens, Some(16));

    let requests = server.finish();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].request_line,
        "POST /openai/v1/chat/completions HTTP/1.1"
    );
    assert!(
        requests[0]
            .headers
            .contains("authorization: Bearer synthetic-groq-secret")
    );
    assert_eq!(requests[0].body["model"], "qwen/qwen3.6-27b");
    assert_eq!(requests[0].body["messages"][0]["role"], "system");
    assert_eq!(
        requests[0].body["messages"][1]["content"],
        "Synthetic user contract."
    );
    assert_eq!(requests[0].body["max_tokens"], 64);
    assert!(
        !requests[0]
            .body
            .to_string()
            .contains("synthetic-groq-secret")
    );
}

#[test]
fn llm_rate_limit_and_server_failure_recordings_map_to_typed_errors() {
    let rate_server = FixtureServer::start(vec![ResponseSpec {
        status: 429,
        body: RATE_LIMITED,
    }]);
    let rate_provider = provider_for(ProviderName::Groq, rate_server.url.clone());
    assert_eq!(
        rate_provider
            .complete(request("rate-secret", "groq/qwen/qwen3.6-27b"))
            .expect_err("rate limit must fail")
            .kind,
        ProviderErrorKind::RateLimit
    );
    rate_server.finish();

    let outage_server = FixtureServer::start(vec![ResponseSpec {
        status: 503,
        body: SERVER_ERROR,
    }]);
    let outage_provider = provider_for(ProviderName::Cerebras, outage_server.url.clone());
    assert_eq!(
        outage_provider
            .complete(request("outage-secret", "cerebras/gemma-4-31b"))
            .expect_err("server outage must fail")
            .kind,
        ProviderErrorKind::Unavailable
    );
    outage_server.finish();
}

#[test]
fn llm_rate_limit_recording_falls_back_to_second_provider_and_records_only_metadata() {
    let primary = FixtureServer::start(vec![ResponseSpec {
        status: 429,
        body: RATE_LIMITED,
    }]);
    let fallback = FixtureServer::start(vec![ResponseSpec {
        status: 200,
        body: SUCCESS,
    }]);

    let primary_config = ProviderConfig::new(
        "groq/qwen/qwen3.6-27b",
        ProviderCredential::new("primary-secret").expect("primary credential"),
        Duration::from_secs(1),
        64,
    )
    .expect("primary config");
    let primary_loader: Arc<dyn ProviderLoader> = Arc::new(OpenAiCompatibleProviderLoader::new(
        OpenAiCompatibleEndpoint::custom(ProviderName::Groq, primary.url.clone()),
    ));
    let fallback_config = ProviderConfig::new(
        "cerebras/gemma-4-31b",
        ProviderCredential::new("fallback-secret").expect("fallback credential"),
        Duration::from_secs(1),
        64,
    )
    .expect("fallback config");
    let fallback_loader: Arc<dyn ProviderLoader> = Arc::new(OpenAiCompatibleProviderLoader::new(
        OpenAiCompatibleEndpoint::custom(ProviderName::Cerebras, fallback.url.clone()),
    ));
    let recorder = Arc::new(RecordingAttemptRecorder::default());
    let service = AIService::new(primary_config, primary_loader)
        .with_fallback(fallback_config, fallback_loader)
        .with_attempt_recorder(Arc::clone(&recorder) as Arc<dyn AttemptRecorder>);

    assert_eq!(
        service
            .try_complete(
                "Synthetic prompt body.",
                Some("Synthetic system body."),
                0.2,
                Some(64),
                "fixture.llm.fallback",
            )
            .expect("fallback completion"),
        "Synthetic provider completion."
    );

    assert_eq!(primary.finish().len(), 1);
    assert_eq!(fallback.finish().len(), 1);
    let attempts = recorder.attempts.lock().expect("attempt recorder lock");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].provider, "groq");
    assert_eq!(attempts[0].model, "qwen/qwen3.6-27b");
    assert_eq!(attempts[0].error_kind, Some(ProviderErrorKind::RateLimit));
    assert!(!attempts[0].success);
    assert_eq!(attempts[1].provider, "cerebras");
    assert_eq!(attempts[1].model, "gemma-4-31b");
    assert!(attempts[1].success);
    assert_eq!(attempts[1].usage.total_tokens, Some(16));
    let debug = format!("{attempts:?}");
    assert!(!debug.contains("primary-secret"));
    assert!(!debug.contains("fallback-secret"));
    assert!(!debug.contains("Synthetic prompt body."));
}

#[test]
fn llm_malformed_recording_fails_closed_through_service_and_redacts_credentials() {
    let server = FixtureServer::start(vec![ResponseSpec {
        status: 200,
        body: MALFORMED,
    }]);
    let service = service_for(
        ProviderName::Groq,
        "groq/qwen/qwen3.6-27b",
        "malformed-secret",
        server.url.clone(),
    );
    assert_eq!(
        service.complete(
            "Synthetic prompt body.",
            None,
            0.2,
            Some(64),
            "fixture.llm.malformed",
        ),
        None
    );
    server.finish();

    let credential = ProviderCredential::new("malformed-secret").expect("credential");
    assert_eq!(credential.to_string(), "[REDACTED]");
    assert!(!format!("{credential:?}").contains("malformed-secret"));
}
