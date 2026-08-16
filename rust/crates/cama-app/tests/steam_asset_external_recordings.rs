//! Sanitized, offline replay of the Steam CDN image-cache contract.
//!
//! These fixtures intentionally do not close the external-service readiness
//! gate: `live_network_used` is false and the bodies are synthetic. They do,
//! however, exercise the same public cache API and HTTP status/cache semantics
//! used by production and by `services/trivia_image_cache.py`.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cama_app::trivia_image_cache::TriviaImageCache;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const FIXTURE_MANIFEST: &str = include_str!("fixtures/steam/manifest.json");
const HERO_SUCCESS: &[u8] = include_bytes!("fixtures/steam/hero_success.txt");
const SERVICE_UNAVAILABLE: &[u8] = include_bytes!("fixtures/steam/service_unavailable.json");

#[derive(Clone, Copy)]
struct RecordedResponse {
    status: u16,
    body: &'static [u8],
    content_type: &'static str,
}

impl RecordedResponse {
    const fn new(status: u16, body: &'static [u8], content_type: &'static str) -> Self {
        Self {
            status,
            body,
            content_type,
        }
    }
}

struct FixtureServer {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FixtureServer {
    fn start(responses: Vec<RecordedResponse>) -> Self {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind loopback Steam fixture server");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("Steam fixture server address")
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
                        .expect("Steam fixture request lock")
                        .push(request_line);
                }
                write_response(&mut stream, response);
            }
        });
        Self { base_url, requests }
    }

    fn wait_for_requests(&self, expected: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let requests = self
                .requests
                .lock()
                .expect("Steam fixture request lock")
                .clone();
            if requests.len() >= expected || Instant::now() >= deadline {
                return requests;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn write_response(stream: &mut TcpStream, response: RecordedResponse) {
    let reason = match response.status {
        200 => "OK",
        503 => "Service Unavailable",
        _ => "Fixture Status",
    };
    let wire = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
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
        "hero_success.txt" => HERO_SUCCESS,
        "service_unavailable.json" => SERVICE_UNAVAILABLE,
        other => panic!("Steam fixture manifest references unknown body {other:?}"),
    }
}

fn cache_for(server: &FixtureServer, root: &std::path::Path) -> TriviaImageCache {
    let cache = TriviaImageCache::new(root).expect("Steam fixture cache");
    // Keep the server-specific URL construction in one place so the request
    // and cache path remain identical to the Python oracle's URL mapping.
    assert!(server.base_url.starts_with("http://127.0.0.1:"));
    cache
}

#[test]
fn steam_fixture_manifest_is_hashed_and_redacted() {
    let manifest: Value =
        serde_json::from_str(FIXTURE_MANIFEST).expect("Steam fixture manifest JSON");
    assert_eq!(manifest["schema"], "cama.external-service-recording/v1");
    assert_eq!(manifest["provider"], "steam_asset_cdn");
    assert_eq!(manifest["provenance"]["live_network_used"], false);
    assert!(FIXTURE_MANIFEST.contains("sanitized_contract_fixture"));
    assert!(!FIXTURE_MANIFEST.contains("Authorization"));
    assert!(!FIXTURE_MANIFEST.contains("Bearer"));
    assert!(!FIXTURE_MANIFEST.contains("api_key"));

    let mut referenced = BTreeSet::new();
    for contract in manifest["contracts"]
        .as_array()
        .expect("Steam fixture contracts")
    {
        for response in contract["responses"]
            .as_array()
            .expect("Steam fixture responses")
        {
            let name = response["body"].as_str().expect("Steam fixture body name");
            let expected = response["sha256"]
                .as_str()
                .expect("Steam fixture body hash");
            let actual = format!("{:x}", Sha256::digest(fixture_body(name)));
            assert_eq!(actual, expected, "Steam fixture hash mismatch for {name}");
            referenced.insert(name);
        }
    }
    assert_eq!(
        referenced,
        BTreeSet::from(["hero_success.txt", "service_unavailable.json"])
    );
}

#[test]
fn steam_success_recording_replays_through_public_cache_api() {
    let root = tempdir().expect("Steam cache directory");
    let server = FixtureServer::start(vec![RecordedResponse::new(200, HERO_SUCCESS, "image/png")]);
    let cache = cache_for(&server, root.path());
    let url = format!(
        "{}/apps/dota2/images/dota_react/heroes/fixture_hero.png?v=fixture",
        server.base_url
    );

    let image = cache.get_or_download(&url).expect("recorded Steam asset");

    assert_eq!(image.filename, "fixture_hero.png");
    assert_eq!(image.bytes, HERO_SUCCESS);
    assert_eq!(
        server.wait_for_requests(1),
        ["GET /apps/dota2/images/dota_react/heroes/fixture_hero.png?v=fixture HTTP/1.1"]
    );
    assert_eq!(
        std::fs::read(cache.path_for_url(&url)).expect("cached Steam asset"),
        HERO_SUCCESS
    );
}

#[test]
fn steam_http_failure_fails_closed_without_creating_cache_file() {
    let root = tempdir().expect("Steam cache directory");
    let server = FixtureServer::start(vec![RecordedResponse::new(
        503,
        SERVICE_UNAVAILABLE,
        "application/json",
    )]);
    let cache = cache_for(&server, root.path());
    let url = format!(
        "{}/apps/dota2/images/dota_react/heroes/unavailable.png",
        server.base_url
    );

    let error = cache
        .get_or_download(&url)
        .expect_err("503 Steam response must fail closed");

    assert!(error.to_string().contains("could not be downloaded"));
    assert!(!cache.path_for_url(&url).exists());
    assert_eq!(server.wait_for_requests(1).len(), 1);
}

#[test]
fn steam_retry_recording_replays_after_caller_retry_and_then_caches() {
    let root = tempdir().expect("Steam cache directory");
    let server = FixtureServer::start(vec![
        RecordedResponse::new(503, SERVICE_UNAVAILABLE, "application/json"),
        RecordedResponse::new(200, HERO_SUCCESS, "image/png"),
    ]);
    let cache = cache_for(&server, root.path());
    let url = format!(
        "{}/apps/dota2/images/dota_react/heroes/retry_hero.png",
        server.base_url
    );

    assert!(cache.get_or_download(&url).is_err());
    let image = cache
        .get_or_download(&url)
        .expect("caller retry should replay successful Steam recording");
    assert_eq!(image.bytes, HERO_SUCCESS);
    assert_eq!(server.wait_for_requests(2).len(), 2);

    let cached = cache.get_or_download(&url).expect("cache hit after retry");
    assert_eq!(cached.bytes, HERO_SUCCESS);
    assert_eq!(server.wait_for_requests(2).len(), 2);
}
