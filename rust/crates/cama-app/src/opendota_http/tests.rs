use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use std::time::{Duration, Instant};

use super::*;

#[derive(Clone, Debug)]
struct ScriptedResponse {
    status: u16,
    body: String,
    headers: Vec<(String, String)>,
    delay: Duration,
}

impl ScriptedResponse {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_owned(),
            headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
            delay: Duration::ZERO,
        }
    }

    fn status(status: u16) -> Self {
        Self::json(status, "")
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

#[derive(Clone)]
struct ScriptedServer {
    base_url: String,
    requests: Arc<StdMutex<Vec<String>>>,
}

impl ScriptedServer {
    fn start(responses: Vec<ScriptedResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test server");
        let base_url = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let captured = requests.clone();
        thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                if let Some(request_line) = read_request_line(&mut stream) {
                    captured
                        .lock()
                        .expect("request capture lock")
                        .push(request_line);
                }
                thread::sleep(response.delay);
                let reason = match response.status {
                    200 => "OK",
                    403 => "Forbidden",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    502 => "Bad Gateway",
                    503 => "Service Unavailable",
                    504 => "Gateway Timeout",
                    _ => "Test Status",
                };
                let mut headers = response.headers;
                if !headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
                    && !headers
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case("Transfer-Encoding"))
                {
                    headers.push(("Content-Length".to_owned(), response.body.len().to_string()));
                }
                let mut wire = format!("HTTP/1.1 {} {reason}\r\n", response.status);
                for (name, value) in headers {
                    wire.push_str(&format!("{name}: {value}\r\n"));
                }
                wire.push_str("Connection: close\r\n\r\n");
                wire.push_str(&response.body);
                let _ = stream.write_all(wire.as_bytes());
            }
        });
        Self { base_url, requests }
    }

    fn request_lines(&self) -> Vec<String> {
        self.requests.lock().expect("request capture lock").clone()
    }

    fn wait_for_requests(&self, expected: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let requests = self.request_lines();
            if requests.len() >= expected || Instant::now() >= deadline {
                return requests;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
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

#[tokio::test]
async fn test_request_observer_increments_on_actual_request_attempt() {
    let server = ScriptedServer::start(vec![ScriptedResponse::json(200, r#"{"profile":{}}"#)]);
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&attempts);
    let mut config = OpenDotaHttpConfig::new(&server.base_url, None);
    config.retry_delays = vec![Duration::ZERO];
    config = config.with_request_observer(move |provider| {
        assert_eq!(provider, "opendota");
        observed.fetch_add(1, Ordering::SeqCst);
    });
    let client = OpenDotaHttpClient::with_config(config).expect("observed OpenDota client");

    assert!(client.get_player_data(12_345).await.is_some());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(server.wait_for_requests(1).len(), 1);
}

#[derive(Default)]
struct RecordingSleeper {
    durations: StdMutex<Vec<Duration>>,
}

impl RecordingSleeper {
    fn durations(&self) -> Vec<Duration> {
        self.durations.lock().expect("sleeper lock").clone()
    }
}

impl RetrySleeper for RecordingSleeper {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.durations.lock().expect("sleeper lock").push(duration);
        Box::pin(async {})
    }
}

fn client_for(server: &ScriptedServer) -> OpenDotaHttpClient {
    client_with(server, vec![Duration::ZERO; 3], Duration::from_secs(90))
}

fn client_with(
    server: &ScriptedServer,
    retry_delays: Vec<Duration>,
    deadline: Duration,
) -> OpenDotaHttpClient {
    let mut config = OpenDotaHttpConfig::new(&server.base_url, Some("test".to_owned()));
    config.retry_delays = retry_delays;
    config.request_deadline = deadline;
    config.requests_per_minute = 1_200;
    OpenDotaHttpClient::with_config(config).expect("test client")
}

#[test]
fn test_registration_projection_truncates_json_float_mmr_like_python_int() {
    let player_data = project_registration_player(serde_json::json!({
        "computed_mmr": 3811.28
    }))
    .expect("registration player projection");

    assert_eq!(
        get_player_mmr_from_data(Some(&player_data)),
        Ok(Some(3_811))
    );

    let negative = project_registration_player(serde_json::json!({
        "computed_mmr": -3811.28
    }))
    .expect("negative registration player projection");
    assert_eq!(get_player_mmr_from_data(Some(&negative)), Ok(Some(-3_811)));
}

#[test]
fn test_registration_projection_preserves_integer_numeric_strings() {
    let player_data = project_registration_player(serde_json::json!({
        "computed_mmr": " 3811 "
    }))
    .expect("registration player projection");

    assert_eq!(
        get_player_mmr_from_data(Some(&player_data)),
        Ok(Some(3_811))
    );
}

#[test]
fn test_registration_projection_rejects_fractional_and_non_finite_strings() {
    for invalid in ["3811.28", "NaN", "Infinity", "-Infinity"] {
        let player_data = project_registration_player(serde_json::json!({
            "computed_mmr": invalid
        }))
        .expect("registration player projection");

        assert!(get_player_mmr_from_data(Some(&player_data)).is_err());
    }
}

#[test]
fn test_registration_projection_rejects_out_of_range_numeric_mmr() {
    let player_data = project_registration_player(
        serde_json::from_str(r#"{"computed_mmr":9223372036854775808}"#)
            .expect("valid JSON with an unsigned integer"),
    )
    .expect("registration player projection");

    assert!(get_player_mmr_from_data(Some(&player_data)).is_err());
}

#[test]
fn test_mmr_float_truncation_rejects_non_finite_and_unsafe_values() {
    assert_eq!(truncate_finite_f64_to_i64(f64::NAN), None);
    assert_eq!(truncate_finite_f64_to_i64(f64::INFINITY), None);
    assert_eq!(truncate_finite_f64_to_i64(f64::NEG_INFINITY), None);
    assert_eq!(
        truncate_finite_f64_to_i64(9_223_372_036_854_775_808.0),
        None
    );
    assert_eq!(
        truncate_finite_f64_to_i64(-9_223_372_036_854_777_000.0),
        None
    );
}

#[test]
fn test_shared_i64_projection_truncates_safely() {
    assert_eq!(value_i64(&serde_json::json!(3811.28)), Some(3_811));
    assert_eq!(value_i64(&serde_json::json!(-3811.28)), Some(-3_811));
    assert_eq!(
        value_i64(
            &serde_json::from_str::<Value>("9223372036854775808")
                .expect("valid unsigned JSON integer")
        ),
        None
    );
    assert_eq!(value_i64(&serde_json::json!(1e100)), None);
}

#[tokio::test]
async fn test_get_player_data_passes_timeout() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::json(200, r#"{"profile":{}}"#).delayed(Duration::from_millis(100)),
        ScriptedResponse::json(200, r#"{"profile":{}}"#).delayed(Duration::from_millis(100)),
    ]);
    let mut config = OpenDotaHttpConfig::new(&server.base_url, Some("test".to_owned()));
    config.request_timeout = Duration::from_millis(20);
    config.request_deadline = Duration::from_millis(80);
    config.retry_delays = vec![Duration::ZERO];
    let client = OpenDotaHttpClient::with_config(config).expect("test client");

    let started_at = Instant::now();
    assert_eq!(client.get_player_data(12_345).await, None);
    assert!(started_at.elapsed() < Duration::from_secs(1));
    assert_eq!(server.wait_for_requests(2).len(), 2);
}

#[test]
fn test_slow_work_uses_dedicated_executor() {
    let services = OpenDotaRuntimeServices::from_lookup(|name| match name {
        "OPENDOTA_API_KEY" => Some("test".to_owned()),
        "ENRICHMENT_RETRY_DELAYS" => Some("0,0,0".to_owned()),
        _ => None,
    })
    .expect("offline production runtime services");
    let debug = format!("{services:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("api_key: Some(\"test\")"));
    assert!(services.shared_client().config.hero_names.len() > 100);
    assert_eq!(services.hero_name(1), Some("Anti-Mage"));
    let _profile_service = services.profile_player_service("/private/tmp/cama-profile-offline.db");
    let client = services.player_api();
    let discovery = services.discovery_api();
    let enrichment = services.enrichment_api();
    let registration = services.registration_api();
    let extractor = services.dotabuff_id_extractor();
    for port in [&discovery, &enrichment, &registration, &extractor] {
        assert!(Arc::ptr_eq(&client.config, &port.config));
        assert!(Arc::ptr_eq(&client.limiter, &port.limiter));
        assert!(Arc::ptr_eq(&client.executor, &port.executor));
    }

    let (thread_name, first_token, second_token) = client
        .run_on_dedicated(async {
            let limiter = TokenBucket::new(1);
            (
                thread::current()
                    .name()
                    .map_or_else(String::new, ToOwned::to_owned),
                limiter.acquire(Duration::ZERO).await,
                limiter.acquire(Duration::ZERO).await,
            )
        })
        .expect("executor result");

    assert!(thread_name.starts_with("opendota-io"));
    assert!(first_token);
    assert!(!second_token);

    let configured = OpenDotaRuntimeServices::from_production_settings(
        Some("typed-snapshot-secret".to_owned()),
        &[0, 2, 4],
    )
    .expect("typed production settings");
    assert_eq!(
        configured.shared_client().config.retry_delays,
        [
            Duration::ZERO,
            Duration::from_secs(2),
            Duration::from_secs(4)
        ]
    );
    let configured_debug = format!("{configured:?}");
    assert!(configured_debug.contains("[REDACTED]"));
    assert!(!configured_debug.contains("typed-snapshot-secret"));
}

#[tokio::test]
async fn test_get_match_details_passes_timeout() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::json(200, r#"{"match_id":123}"#).delayed(Duration::from_millis(100)),
        ScriptedResponse::json(200, r#"{"match_id":123}"#).delayed(Duration::from_millis(100)),
    ]);
    let mut config = OpenDotaHttpConfig::new(&server.base_url, Some("test".to_owned()));
    config.request_timeout = Duration::from_millis(20);
    config.request_deadline = Duration::from_millis(80);
    config.retry_delays = vec![Duration::ZERO];
    let client = OpenDotaHttpClient::with_config(config).expect("test client");

    let started_at = Instant::now();
    assert_eq!(client.get_match_details(123).await, None);
    assert!(started_at.elapsed() < Duration::from_secs(1));
    assert_eq!(server.wait_for_requests(2).len(), 2);
}

#[tokio::test]
async fn test_retries_on_429_then_succeeds() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::status(429),
        ScriptedResponse::status(429),
        ScriptedResponse::json(200, r#"{"profile":{"personaname":"pf"}}"#),
    ]);
    let client = client_for(&server);

    let value = client.get_player_data(12_345).await;
    assert_eq!(
        value,
        serde_json::from_str(r#"{"profile":{"personaname":"pf"}}"#).ok()
    );
    assert_eq!(
        value.and_then(project_player_identity),
        Some(PlayerIdentity {
            persona_name: Some("pf".to_owned()),
            ..PlayerIdentity::default()
        })
    );
    assert_eq!(server.request_lines().len(), 3);
}

#[tokio::test]
async fn test_retries_on_503_then_succeeds() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::status(503),
        ScriptedResponse::json(
            200,
            r#"{"match_id":42,"duration":2400,"radiant_win":true,"players":[{"account_id":12345,"player_slot":0,"kills":8,"lane_efficiency_pct":71,"total_gold":16000}]}"#,
        ),
    ]);
    let client = client_for(&server);

    let value = client.get_match_details(42).await.expect("match payload");
    let projected = project_match_details(value, 42).expect("typed match details");
    assert_eq!(projected.match_id, ValveMatchId(42));
    assert_eq!(projected.players[0].stats.kills, 8);
    assert_eq!(projected.players[0].stats.lane_efficiency, Some(71));
    assert_eq!(projected.players[0].stats.net_worth, 16_000);
    assert_eq!(server.request_lines().len(), 2);
}

#[test]
fn test_project_match_player_extracts_last_hits_at_12_from_lh_t() {
    let value = serde_json::json!({
        "match_id": 42,
        "duration": 2400,
        "radiant_win": true,
        "players": [{
            "account_id": 12345,
            "player_slot": 0,
            "last_hits": 220,
            "lh_t": (0..=20).map(|minute| minute * 8).collect::<Vec<_>>(),
        }],
    });

    let projected = project_match_details(value, 42).expect("typed match details");

    assert_eq!(projected.players[0].stats.last_hits, 220);
    assert_eq!(projected.players[0].stats.last_hits_at_12, Some(96));
}

#[test]
fn test_project_match_player_last_hits_at_12_is_none_when_lh_t_is_absent_or_short() {
    let absent = serde_json::json!({
        "match_id": 42,
        "duration": 300,
        "radiant_win": true,
        "players": [{"account_id": 12345, "player_slot": 0, "last_hits": 10}],
    });
    let projected = project_match_details(absent, 42).expect("typed match details");
    assert_eq!(projected.players[0].stats.last_hits_at_12, None);

    let short_game = serde_json::json!({
        "match_id": 43,
        "duration": 300,
        "radiant_win": true,
        "players": [{
            "account_id": 12345,
            "player_slot": 0,
            "last_hits": 10,
            "lh_t": [0, 2, 5],
        }],
    });
    let projected = project_match_details(short_game, 43).expect("typed match details");
    assert_eq!(projected.players[0].stats.last_hits_at_12, None);
}

#[tokio::test]
async fn test_no_retry_on_403() {
    let server = ScriptedServer::start(vec![ScriptedResponse::status(403)]);
    let client = client_for(&server);

    assert_eq!(client.get_player_data(12_345).await, None);
    assert_eq!(server.request_lines().len(), 1);
}

#[tokio::test]
async fn test_retry_exhausted_returns_none() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::status(429),
        ScriptedResponse::status(429),
        ScriptedResponse::status(429),
        ScriptedResponse::status(429),
    ]);
    let client = client_for(&server);

    assert_eq!(client.get_player_data(12_345).await, None);
    assert_eq!(server.request_lines().len(), 4);
}

#[tokio::test]
async fn test_get_player_data_malformed_json_returns_none() {
    let server = ScriptedServer::start(vec![ScriptedResponse::json(200, "<html>not json</html>")]);
    let client = client_for(&server);

    assert_eq!(client.get_player_data(12_345).await, None);
}

#[tokio::test]
async fn test_get_match_details_malformed_json_returns_none() {
    let server = ScriptedServer::start(vec![ScriptedResponse::json(200, "not-json-body")]);
    let client = client_for(&server);

    assert_eq!(client.get_match_details(42).await, None);
}

#[tokio::test]
async fn test_get_player_matches_rejects_oversized_content_length() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::json(200, r#"[{"match_id":1}]"#)
            .with_header("Content-Length", &(10 * 1024 * 1024).to_string()),
    ]);
    let client = client_for(&server);

    assert_eq!(client.get_player_matches(12_345, 20).await, None);
}

#[tokio::test]
async fn test_get_player_matches_passes_limit_param() {
    let server = ScriptedServer::start(vec![ScriptedResponse::json(
        200,
        r#"[{"match_id":7,"hero_id":1,"lane_role":2}]"#,
    )]);
    let client = client_for(&server);

    let matches = client
        .get_player_matches(12_345, 20)
        .await
        .expect("match list");
    assert_eq!(
        project_history_matches(matches.clone())[0].match_id,
        ValveMatchId(7)
    );
    assert_eq!(project_matches(Some(matches))[0].lane_role, Some(2));
    let requests = server.request_lines();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("limit=20"));
    assert!(requests[0].contains("api_key=test"));
}

#[tokio::test]
async fn test_opendota_honors_retry_after_larger_than_delay() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::status(429).with_header("Retry-After", "60"),
        ScriptedResponse::json(200, r#"{"profile":{"personaname":"pf"}}"#),
    ]);
    let sleeper = Arc::new(RecordingSleeper::default());
    let client = client_for(&server).with_test_sleeper(sleeper.clone());

    assert!(client.get_player_data(12_345).await.is_some());
    assert_eq!(sleeper.durations(), vec![Duration::from_secs(60)]);
}

#[tokio::test]
async fn test_opendota_ignores_retry_after_smaller_than_delay() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::status(429).with_header("Retry-After", "1"),
        ScriptedResponse::json(200, r#"{"profile":{}}"#),
    ]);
    let sleeper = Arc::new(RecordingSleeper::default());
    let client = client_with(
        &server,
        vec![Duration::from_secs(30); 3],
        Duration::from_secs(90),
    )
    .with_test_sleeper(sleeper.clone());

    assert!(client.get_player_data(12_345).await.is_some());
    assert_eq!(sleeper.durations(), vec![Duration::from_secs(30)]);
}

#[tokio::test]
async fn test_opendota_tolerates_malformed_retry_after() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::status(429).with_header("Retry-After", "not-a-number"),
        ScriptedResponse::json(200, r#"{"profile":{}}"#),
    ]);
    let sleeper = Arc::new(RecordingSleeper::default());
    let client = client_for(&server).with_test_sleeper(sleeper.clone());

    assert!(client.get_player_data(12_345).await.is_some());
    assert_eq!(sleeper.durations(), vec![Duration::ZERO]);
}

#[tokio::test]
async fn test_retry_after_is_capped_by_total_request_deadline() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::status(429).with_header("Retry-After", "600"),
        ScriptedResponse::json(200, r#"{"profile":{}}"#),
    ]);
    let sleeper = Arc::new(RecordingSleeper::default());
    let client = client_for(&server).with_test_sleeper(sleeper.clone());

    assert!(client.get_player_data(12_345).await.is_some());
    let sleeps = sleeper.durations();
    assert_eq!(sleeps.len(), 1);
    assert!(sleeps[0] > Duration::ZERO);
    assert!(sleeps[0] <= DEFAULT_REQUEST_DEADLINE);
}

#[tokio::test]
async fn test_get_player_roles_returns_none_on_malformed_json() {
    let server = ScriptedServer::start(vec![ScriptedResponse::json(200, "<html>not json</html>")]);
    let client = client_for(&server);

    assert_eq!(client.get_player_roles(12_345).await, None);
}

#[tokio::test]
async fn test_get_player_roles_returns_none_on_oversized_body() {
    let server = ScriptedServer::start(vec![
        ScriptedResponse::json(200, "f\r\n[{\"hero_id\":1}]\r\n0\r\n\r\n")
            .with_header("Transfer-Encoding", "chunked"),
    ]);
    let mut config = OpenDotaHttpConfig::new(&server.base_url, Some("test".to_owned()));
    config.max_response_bytes = 8;
    config.retry_delays = vec![Duration::ZERO; 3];
    let client = OpenDotaHttpClient::with_config(config).expect("test client");

    assert_eq!(client.get_player_roles(12_345).await, None);
}
