use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cama_app::opendota_http::{OpenDotaHttpClient, OpenDotaHttpConfig};
use serde_json::Value;
use sha2::{Digest, Sha256};

const FIXTURE_MANIFEST: &str = include_str!("fixtures/opendota/manifest.json");
const FORBIDDEN: &[u8] = include_bytes!("fixtures/opendota/forbidden.json");
const MALFORMED: &[u8] = include_bytes!("fixtures/opendota/malformed.txt");
const OVERSIZED: &[u8] = include_bytes!("fixtures/opendota/oversized.json");
const PLAYER_SUCCESS: &[u8] = include_bytes!("fixtures/opendota/player_success.json");
const RATE_LIMITED: &[u8] = include_bytes!("fixtures/opendota/rate_limited.json");

#[derive(Clone)]
struct RecordedResponse {
    status: u16,
    body: &'static [u8],
    headers: Vec<(&'static str, &'static str)>,
}

impl RecordedResponse {
    fn json(status: u16, body: &'static [u8]) -> Self {
        Self {
            status,
            body,
            headers: vec![("Content-Type", "application/json")],
        }
    }

    fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }
}

struct FixtureServer {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FixtureServer {
    fn start(responses: Vec<RecordedResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback fixture server");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("fixture server address")
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                if let Some(request_line) = read_request_line(&mut stream) {
                    captured
                        .lock()
                        .expect("fixture request lock")
                        .push(request_line);
                }
                write_response(&mut stream, &response);
            }
        });
        Self { base_url, requests }
    }

    fn wait_for_requests(&self, expected: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let requests = self.requests.lock().expect("fixture request lock").clone();
            if requests.len() >= expected || Instant::now() >= deadline {
                return requests;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn write_response(stream: &mut TcpStream, response: &RecordedResponse) {
    let reason = match response.status {
        200 => "OK",
        403 => "Forbidden",
        429 => "Too Many Requests",
        _ => "Fixture Status",
    };
    let mut wire = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\n",
        response.status,
        response.body.len()
    );
    for (name, value) in &response.headers {
        wire.push_str(&format!("{name}: {value}\r\n"));
    }
    wire.push_str("Connection: close\r\n\r\n");
    let _ = stream.write_all(wire.as_bytes());
    let _ = stream.write_all(response.body);
}

fn read_request_line(stream: &mut TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 512];
    while bytes.len() < 16 * 1024 {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(bytes)
        .ok()?
        .lines()
        .next()
        .map(ToOwned::to_owned)
}

fn fixture_body(name: &str) -> &'static [u8] {
    match name {
        "forbidden.json" => FORBIDDEN,
        "malformed.txt" => MALFORMED,
        "oversized.json" => OVERSIZED,
        "player_success.json" => PLAYER_SUCCESS,
        "rate_limited.json" => RATE_LIMITED,
        other => panic!("fixture manifest references unknown body {other:?}"),
    }
}

fn client_for(server: &FixtureServer, retry_delays: Vec<Duration>) -> OpenDotaHttpClient {
    let mut config = OpenDotaHttpConfig::new(&server.base_url, Some("fixture-api-key".to_owned()));
    config.retry_delays = retry_delays;
    config.request_deadline = Duration::from_secs(3);
    config.requests_per_minute = 1_200;
    OpenDotaHttpClient::with_config(config).expect("fixture OpenDota client")
}

#[test]
fn external_opendota_fixture_manifest_is_hashed_and_redacted() {
    let manifest: Value = serde_json::from_str(FIXTURE_MANIFEST).expect("fixture manifest JSON");
    assert_eq!(manifest["schema"], "cama.external-service-recording/v1");
    assert_eq!(manifest["provider"], "opendota");
    assert_eq!(manifest["provenance"]["live_network_used"], false);
    assert!(FIXTURE_MANIFEST.contains("sanitized_contract_fixture"));
    assert!(!FIXTURE_MANIFEST.contains("Authorization"));
    assert!(!FIXTURE_MANIFEST.contains("Bearer"));

    let mut referenced = BTreeSet::new();
    for contract in manifest["contracts"].as_array().expect("fixture contracts") {
        for response in contract["responses"].as_array().expect("fixture responses") {
            let name = response["body"].as_str().expect("fixture body name");
            let expected = response["sha256"].as_str().expect("fixture body hash");
            let actual = format!("{:x}", Sha256::digest(fixture_body(name)));
            assert_eq!(actual, expected, "fixture hash mismatch for {name}");
            referenced.insert(name);
        }
    }
    assert_eq!(
        referenced,
        BTreeSet::from([
            "forbidden.json",
            "malformed.txt",
            "oversized.json",
            "player_success.json",
            "rate_limited.json",
        ])
    );
}

#[tokio::test]
async fn external_opendota_success_recording_replays_through_public_client() {
    let server = FixtureServer::start(vec![RecordedResponse::json(200, PLAYER_SUCCESS)]);
    let client = client_for(&server, vec![Duration::ZERO]);

    let value = client
        .get_player_data(12_345)
        .await
        .expect("recorded player response");
    assert_eq!(value["profile"]["account_id"], 12_345);
    assert_eq!(value["profile"]["personaname"], "fixture-player");

    let requests = server.wait_for_requests(1);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0],
        "GET /players/12345?api_key=fixture-api-key HTTP/1.1"
    );
}

#[tokio::test]
async fn external_opendota_retry_after_recording_replays_then_succeeds() {
    let server = FixtureServer::start(vec![
        RecordedResponse::json(429, RATE_LIMITED).with_header("Retry-After", "1"),
        RecordedResponse::json(200, PLAYER_SUCCESS),
    ]);
    let client = client_for(&server, vec![Duration::ZERO]);
    let started = Instant::now();

    let value = client
        .get_player_data(12_345)
        .await
        .expect("recorded retry response");
    assert_eq!(value["profile"]["personaname"], "fixture-player");
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "Retry-After was not honored: {:?}",
        started.elapsed()
    );
    assert_eq!(server.wait_for_requests(2).len(), 2);
}

#[tokio::test]
async fn external_opendota_non_retryable_recording_fails_once() {
    let server = FixtureServer::start(vec![RecordedResponse::json(403, FORBIDDEN)]);
    let client = client_for(&server, vec![Duration::ZERO; 3]);

    assert!(client.get_player_data(12_345).await.is_none());
    assert_eq!(server.wait_for_requests(1).len(), 1);
}

#[tokio::test]
async fn external_opendota_malformed_recording_fails_closed() {
    let server = FixtureServer::start(vec![RecordedResponse::json(200, MALFORMED)]);
    let client = client_for(&server, vec![Duration::ZERO]);

    assert!(client.get_player_data(12_345).await.is_none());
    assert_eq!(server.wait_for_requests(1).len(), 1);
}

#[tokio::test]
async fn external_opendota_oversized_recording_fails_closed() {
    let server = FixtureServer::start(vec![RecordedResponse::json(200, OVERSIZED)]);
    let mut config = OpenDotaHttpConfig::new(&server.base_url, Some("fixture-api-key".to_owned()));
    config.max_response_bytes = 8;
    config.retry_delays = vec![Duration::ZERO];
    config.request_deadline = Duration::from_secs(3);
    config.requests_per_minute = 1_200;
    let client = OpenDotaHttpClient::with_config(config).expect("fixture OpenDota client");

    assert!(client.get_player_data(12_345).await.is_none());
    assert_eq!(server.wait_for_requests(1).len(), 1);
}

#[test]
fn external_opendota_client_debug_redacts_fixture_credentials() {
    let config = OpenDotaHttpConfig::new(
        "http://127.0.0.1:9",
        Some("recording-only-api-key".to_owned()),
    );
    let client = OpenDotaHttpClient::with_config(config).expect("fixture OpenDota client");
    let debug = format!("{client:?}");

    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("recording-only-api-key"));
}
