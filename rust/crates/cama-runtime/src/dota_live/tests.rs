use std::net::SocketAddr;
use std::path::PathBuf;

use axum::http::HeaderValue;
use cama_domain::dota_lobby::STEAM_INDIVIDUAL_BASE;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::{Secret, dota_host_config::DotaHostConfig};

fn config(live_bind: Option<SocketAddr>) -> DotaHostConfig {
    DotaHostConfig {
        guild_ids: vec![7],
        username: Secret::new("bot".to_owned()),
        account_id: 900,
        session_path: PathBuf::from("session.json"),
        start_after: 1,
        server_region: 1,
        game_mode: 2,
        tv_delay: 3,
        lobby_timeout_seconds: 1_800,
        web_api_key: Some(Secret::new("web-key".to_owned())),
        live_bind,
        live_token: Some(Secret::new(
            "viewer-token-with-at-least-32-bytes".to_owned(),
        )),
        gsi_token: Some(Secret::new(
            "gsi-token-with-at-least-32-bytes-long".to_owned(),
        )),
        replay_directory: PathBuf::from("replays"),
        replay_max_bytes: 1_024,
        replay_retention_days: 30,
    }
}

#[test]
fn dota_tv_delay_enum_uses_current_lobby_wire_values() {
    assert_eq!(tv_delay_seconds(0), Some(10));
    assert_eq!(tv_delay_seconds(1), Some(60));
    assert_eq!(tv_delay_seconds(2), Some(120));
    assert_eq!(tv_delay_seconds(3), Some(300));
    assert_eq!(tv_delay_seconds(4), Some(900));
    assert_eq!(tv_delay_seconds(5), None);
}

#[test]
fn parses_playable_gsi_game_states_from_symbolic_and_raw_values() {
    let cases = [
        (json!("DOTA_GAMERULES_STATE_PRE_GAME"), 4),
        (json!("DOTA_GAMERULES_STATE_GAME_IN_PROGRESS"), 5),
        (json!("DOTA_GAMERULES_STATE_POST_GAME"), 6),
        (json!(4), 4),
        (json!(5), 5),
        (json!(6), 6),
        (json!("4"), 4),
        (json!("5"), 5),
        (json!("6"), 6),
    ];
    for (value, expected) in cases {
        let payload = json!({"game_state": value});
        let map = payload.as_object().expect("state object");
        assert_eq!(parse_gsi_game_state(map), Some(expected));
    }
}

#[test]
fn realtime_stats_requires_the_expected_match_and_keeps_optional_fields_absent() {
    let steam_id = (STEAM_INDIVIDUAL_BASE + 1).to_string();
    let payload = json!({
        "result": {
            "match_id": "123",
            "game_time": 90,
            "radiant_score": 4,
            "players": [{
                "account_id": steam_id,
                "hero": {"id": 2, "name": "Axe"},
                "kills": 3,
                "items": [{"id": 42, "charges": 2}, {"name": "boots"}]
            }]
        }
    });

    let snapshot = normalize_realtime_stats(&payload, 7, 8, 123, 500).expect("matching sample");
    assert_eq!(snapshot.match_id, 123);
    assert_eq!(snapshot.observed_at, 500);
    assert_eq!(snapshot.game_time_seconds, Some(90));
    assert_eq!(snapshot.delay_source, None);
    assert_eq!(snapshot.dire_score, None);
    let player = &snapshot.players.as_ref().expect("players")[0];
    assert_eq!(player.account_id, Some(1));
    assert_eq!(player.hero_name.as_deref(), Some("Axe"));
    assert_eq!(player.deaths, None);
    assert_eq!(player.items.as_ref().expect("items")[0].id, Some(42));
    assert_eq!(player.items.as_ref().expect("items")[0].name, None);
    assert_eq!(
        player.items.as_ref().expect("items")[1].name.as_deref(),
        Some("boots")
    );

    assert!(normalize_realtime_stats(&payload, 7, 8, 124, 500).is_none());
    assert!(
        normalize_realtime_stats(&json!({"result": {"game_time": 90}}), 7, 8, 123, 500).is_none()
    );
}

#[test]
fn realtime_stats_reads_match_metadata_and_team_player_arrays() {
    let payload = json!({
        "result": {
            "match": {"match_id": 321, "game_time": 77},
            "teams": [
                {"team_number": 2, "score": 3, "players": [{"accountid": 1, "kills": 2}]},
                {"team_number": 3, "score": 4, "players": [{"account_id": 2, "assists": 5}]}
            ]
        }
    });
    let snapshot = normalize_realtime_stats(&payload, 7, 8, 321, 503).expect("matching sample");
    assert_eq!(snapshot.game_time_seconds, Some(77));
    assert_eq!(snapshot.radiant_score, Some(3));
    assert_eq!(snapshot.dire_score, Some(4));
    let players = snapshot.players.expect("team players");
    assert_eq!(players.len(), 2);
    assert_eq!(players[0].account_id, Some(1));
    assert_eq!(players[1].assists, Some(5));
}

#[test]
fn league_games_fallback_selects_only_the_expected_game() {
    let payload = json!({
        "result": {
            "games": [
                {"match_id": "11", "scoreboard": {"game_time": 1}},
                {
                    "matchid": "22",
                    "scoreboard": {
                        "duration": 120,
                        "dire_team_score": 6,
                        "players": {
                            "1": {"steamid": "1", "kills": 2}
                        }
                    }
                }
            ]
        }
    });

    let snapshot = normalize_live_league_games(&payload, 7, 8, 22, 501).expect("matching game");
    assert_eq!(snapshot.source, LiveSnapshotSource::LiveLeagueGames);
    assert_eq!(snapshot.game_time_seconds, Some(120));
    assert_eq!(snapshot.radiant_score, None);
    assert_eq!(snapshot.dire_score, Some(6));
    assert_eq!(
        snapshot.players.as_ref().expect("players")[0].account_id,
        Some(1)
    );
    assert!(normalize_live_league_games(&payload, 7, 8, 33, 501).is_none());
}

#[test]
fn league_scoreboard_reads_nested_team_scores_and_players() {
    let payload = json!({
        "result": {
            "games": [{
                "match_id": 22,
                "scoreboard": {
                    "duration": 240,
                    "radiant": {"score": 8, "players": [{"account_id": 1, "kills": 4}]},
                    "dire": {"score": 9, "players": [{"account_id": 2, "deaths": 3}]}
                }
            }]
        }
    });
    let snapshot = normalize_live_league_games(&payload, 7, 8, 22, 502).expect("matching game");
    assert_eq!(snapshot.radiant_score, Some(8));
    assert_eq!(snapshot.dire_score, Some(9));
    let players = snapshot.players.expect("nested players");
    assert_eq!(players.len(), 2);
    assert_eq!(players[0].kills, Some(4));
    assert_eq!(players[1].deaths, Some(3));
}

#[tokio::test]
async fn private_publish_can_observe_start_before_close_and_repeated_publish_preserves_sample() {
    let feed = DotaLiveFeed::new(&config(None)).expect("feed");
    feed.publish_match(7, 8, 123, None, 5, 2, false, vec![1])
        .await;
    assert!(
        feed.matches
            .read()
            .expect("cache lock")
            .get(&(7, 8))
            .is_some_and(|registration| !registration.betting_closed)
    );
    assert!(feed.snapshot(7, 8).is_none());

    feed.ingest_gsi(
        json!({
            "auth": {"token": "gsi-token-with-at-least-32-bytes-long"},
            "map": {
                "matchid": "123",
                "game_state": "DOTA_GAMERULES_STATE_PRE_GAME",
                "game_time": 30
            },
            "allplayers": {"1": {"steamid": "1", "kills": 1}}
        }),
        600,
    )
    .expect("private GSI sample");
    assert!(feed.gameplay_started(7, 8, 123));
    assert!(feed.snapshot(7, 8).is_none());

    feed.ingest_gsi(
        json!({
            "auth": {"token": "gsi-token-with-at-least-32-bytes-long"},
            "map": {
                "matchid": "123",
                "game_state": "DOTA_GAMERULES_STATE_HERO_SELECTION"
            },
            "allplayers": {"1": {"steamid": "1"}}
        }),
        601,
    )
    .expect("later non-playable GSI sample");
    assert!(feed.gameplay_started(7, 8, 123));

    feed.publish_match(7, 8, 123, None, 5, 2, true, vec![1])
        .await;
    let first = feed.snapshot(7, 8).expect("cached sample");
    assert_eq!(first.delay_seconds, Some(120));
    assert_eq!(first.delay_source, Some(LiveDelaySource::Configured));

    feed.publish_match(7, 8, 123, Some(99), 6, 2, true, vec![1])
        .await;
    let repeated = feed.snapshot(7, 8).expect("sample survives republish");
    assert_eq!(repeated.observed_at, first.observed_at);
    assert_eq!(repeated.fetched_at, first.fetched_at);
    assert_eq!(repeated.source, LiveSnapshotSource::Gsi);

    // Closure is monotonic even if a recovery pass later arrives with the
    // old pre-close flag.
    feed.publish_match(7, 8, 123, None, 5, 2, false, vec![1])
        .await;
    assert!(feed.snapshot(7, 8).is_some());

    feed.finish_match(7, 8).await;
    assert!(feed.snapshot(7, 8).expect("retained sample").stale);
    assert!(feed.summary(7, 8).await.expect("summary").contains("stale"));
}

#[tokio::test]
async fn invalid_or_nonplayable_gsi_input_cannot_create_start_evidence() {
    let feed = DotaLiveFeed::new(&config(None)).expect("feed");
    feed.publish_match(7, 8, 123, None, 5, 2, false, vec![1])
        .await;

    let payload = |token: &str, match_id: &str, game_state: Value, account: &str| {
        let mut players = Map::new();
        players.insert(account.to_owned(), json!({"steamid": account}));
        json!({
            "auth": {"token": token},
            "map": {"matchid": match_id, "game_state": game_state},
            "allplayers": players
        })
    };

    assert_eq!(
        feed.ingest_gsi(
            payload(
                "wrong-token",
                "123",
                "DOTA_GAMERULES_STATE_PRE_GAME".into(),
                "1"
            ),
            700
        ),
        Err("GSI authentication failed".to_owned())
    );
    assert!(!feed.gameplay_started(7, 8, 123));

    assert_eq!(
        feed.ingest_gsi(
            payload(
                "gsi-token-with-at-least-32-bytes-long",
                "999",
                "DOTA_GAMERULES_STATE_PRE_GAME".into(),
                "1"
            ),
            701
        ),
        Err("GSI match is not an active registered match".to_owned())
    );
    assert!(!feed.gameplay_started(7, 8, 123));

    assert_eq!(
        feed.ingest_gsi(
            payload(
                "gsi-token-with-at-least-32-bytes-long",
                "123",
                "DOTA_GAMERULES_STATE_PRE_GAME".into(),
                "2"
            ),
            702
        ),
        Err("GSI roster does not match the registered roster".to_owned())
    );
    assert!(!feed.gameplay_started(7, 8, 123));

    for game_state in [
        "DOTA_GAMERULES_STATE_HERO_SELECTION",
        "DOTA_GAMERULES_STATE_STRATEGY_TIME",
        "DOTA_GAMERULES_STATE_TEAM_SHOWCASE",
    ] {
        feed.ingest_gsi(
            payload(
                "gsi-token-with-at-least-32-bytes-long",
                "123",
                game_state.into(),
                "1",
            ),
            703,
        )
        .expect("non-playable GSI state is still a valid sample");
        assert!(!feed.gameplay_started(7, 8, 123));
    }

    for game_state in [json!(99), json!("99")] {
        feed.ingest_gsi(
            payload(
                "gsi-token-with-at-least-32-bytes-long",
                "123",
                game_state,
                "1",
            ),
            704,
        )
        .expect("unknown GSI state is still a valid sample");
        assert!(!feed.gameplay_started(7, 8, 123));
    }
}

#[tokio::test]
async fn gsi_requires_active_match_and_rejects_accounts_outside_frozen_roster() {
    let feed = DotaLiveFeed::new(&config(None)).expect("feed");
    feed.publish_match(7, 8, 123, None, 5, 3, true, vec![1, 2])
        .await;
    let base = json!({
        "auth": {"token": "gsi-token-with-at-least-32-bytes-long"},
        "map": {"matchid": "123", "delay": 17},
        "allplayers": {
            "1": {"steamid": "1"},
            "2": {"steamid": "2"}
        }
    });
    feed.ingest_gsi(base.clone(), 700).expect("exact roster");
    assert_eq!(
        feed.snapshot(7, 8)
            .expect("sample")
            .players
            .as_ref()
            .map(Vec::len),
        Some(2)
    );
    let source_delayed = feed.snapshot(7, 8).expect("source delayed sample");
    assert_eq!(source_delayed.delay_seconds, Some(17));
    assert_eq!(source_delayed.delay_source, Some(LiveDelaySource::Source));

    let subset = json!({
        "auth": {"token": "gsi-token-with-at-least-32-bytes-long"},
        "map": {"matchid": "123"},
        "allplayers": {"1": {"steamid": "1"}}
    });
    feed.ingest_gsi(subset, 700)
        .expect("observer may report a subset");

    let wrong_roster = json!({
        "auth": {"token": "gsi-token-with-at-least-32-bytes-long"},
        "map": {"matchid": "123"},
        "allplayers": {"1": {"steamid": "1"}, "3": {"steamid": "3"}}
    });
    assert_eq!(
        feed.ingest_gsi(wrong_roster, 701),
        Err("GSI roster does not match the registered roster".to_owned())
    );

    feed.finish_match(7, 8).await;
    assert_eq!(
        feed.ingest_gsi(base, 702),
        Err("GSI match is not an active registered match".to_owned())
    );
}

#[test]
fn gsi_observer_team_slots_merge_player_hero_and_items_sections() {
    let payload = json!({
        "player": {
            "team2": {"player0": {"steamid": (STEAM_INDIVIDUAL_BASE + 1).to_string(), "kills": 2}},
            "team3": {"player0": {"steamid": "2", "deaths": 1}}
        },
        "hero": {
            "team2": {"player0": {"id": 11, "name": "npc_dota_hero_antimage"}},
            "team3": {"player0": {"id": 12}}
        },
        "items": {
            "team2": {"player0": {"slot0": {"id": 42}}}
        }
    });
    let players = parse_gsi_players(&payload).expect("observer players");
    assert_eq!(players.len(), 2);
    assert_eq!(players[0].account_id, Some(1));
    assert_eq!(players[0].hero_id, Some(11));
    assert_eq!(players[0].items.as_ref().expect("items")[0].id, Some(42));
    assert_eq!(players[1].account_id, Some(2));
    assert_eq!(players[1].hero_id, Some(12));
}

#[tokio::test]
async fn cache_is_bounded_and_prefers_evicting_finished_matches() {
    let feed = DotaLiveFeed::new(&config(None)).expect("feed");
    feed.publish_match(7, 1, 1, None, 5, 3, true, Vec::new())
        .await;
    feed.ingest_gsi(
        json!({
            "auth": {"token": "gsi-token-with-at-least-32-bytes-long"},
            "map": {"matchid": "1"}
        }),
        1,
    )
    .expect("sample");
    feed.finish_match(7, 1).await;
    for match_id in 2..=DOTA_LIVE_MAX_MATCHES as u64 {
        feed.publish_match(7, match_id as i64, match_id, None, 5, 3, true, Vec::new())
            .await;
    }
    {
        let matches = feed.matches.read().expect("cache lock");
        assert_eq!(matches.len(), DOTA_LIVE_MAX_MATCHES);
        assert!(matches.contains_key(&(7, 1)));
    }
    feed.publish_match(
        7,
        DOTA_LIVE_MAX_MATCHES as i64 + 1,
        DOTA_LIVE_MAX_MATCHES as u64 + 1,
        None,
        5,
        3,
        true,
        Vec::new(),
    )
    .await;
    {
        let matches = feed.matches.read().expect("cache lock");
        assert_eq!(matches.len(), DOTA_LIVE_MAX_MATCHES);
        assert!(matches.contains_key(&(7, 2)));
    }
}

#[test]
fn workers_are_opt_in_to_valve_polling_and_http() {
    let hosting_only_config = {
        let mut config = config(None);
        config.web_api_key = None;
        config.live_token = None;
        config.gsi_token = None;
        config
    };
    let hosting_only = DotaLiveFeed::new(&hosting_only_config).expect("hosting-only feed");
    assert!(hosting_only.workers().is_empty());

    let api_only = DotaLiveFeed::new(&config(None)).expect("Valve polling feed");
    let api_workers = api_only.workers();
    assert_eq!(api_workers.len(), 1);
    assert_eq!(api_workers[0].name, "dota-live-valve-poll");

    let http_only_config = {
        let mut config = config(Some("127.0.0.1:0".parse().expect("address")));
        config.web_api_key = None;
        config.gsi_token = None;
        config
    };
    let http_only = DotaLiveFeed::new(&http_only_config).expect("HTTP-only feed");
    let http_workers = http_only.workers();
    assert_eq!(http_workers.len(), 1);
    assert_eq!(http_workers[0].name, "dota-live-http");
}

#[test]
fn viewer_auth_is_exact_and_json_body_limit_is_enforced() {
    let feed =
        DotaLiveFeed::new(&config(Some("127.0.0.1:0".parse().expect("address")))).expect("feed");
    let mut headers = HeaderMap::new();
    assert!(!feed.authorize_viewer(&headers));
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer viewer-token-with-at-least-32-bytes"),
    );
    assert!(feed.authorize_viewer(&headers));
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer viewer-token-with-at-least-32-bytes-extra"),
    );
    assert!(!feed.authorize_viewer(&headers));

    assert!(matches!(
        parse_limited_json_bytes(&vec![b' '; DOTA_LIVE_MAX_BODY_BYTES + 1]),
        Err(UpstreamError::BodyTooLarge)
    ));
    assert!(matches!(
        parse_limited_json_bytes(br#"{"ok":true}"#),
        Ok(Value::Object(_))
    ));
}

#[tokio::test]
async fn polling_uses_realtime_stats_then_normalizes_the_response_once() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let body = json!({
        "result": {
            "match": {"match_id": 123, "game_time": 90},
            "teams": [
                {"team_number": 2, "score": 3, "players": [{"account_id": 1, "kills": 2}]},
                {"team_number": 3, "score": 4, "players": [{"account_id": 2, "deaths": 1}]}
            ]
        }
    })
    .to_string();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("request");
        let mut request = vec![0_u8; 4096];
        let read = socket.read(&mut request).await.expect("read request");
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("GET /IDOTA2MatchStats_570/GetRealtimeStats/v1/"));
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write response");
    });

    let feed = DotaLiveFeed::new_with_upstream_base(&config(None), format!("http://{address}"))
        .expect("feed");
    feed.publish_match(7, 8, 123, Some(99), 5, 3, true, vec![1, 2])
        .await;
    feed.poll_once().await;
    server.await.expect("server");
    let snapshot = feed.snapshot(7, 8).expect("polled snapshot");
    assert_eq!(snapshot.source, LiveSnapshotSource::RealtimeStats);
    assert_eq!(snapshot.radiant_score, Some(3));
    assert_eq!(snapshot.dire_score, Some(4));
    assert_eq!(snapshot.delay_seconds, Some(300));
    assert_eq!(snapshot.delay_source, Some(LiveDelaySource::Configured));
}

#[tokio::test]
async fn invalid_realtime_stats_falls_back_to_live_league_games() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let league_body = json!({
        "result": {
            "games": [{
                "match_id": "123",
                "scoreboard": {
                    "duration": 91,
                    "radiant_score": 5,
                    "dire_score": 6,
                    "players": [{"account_id": 1, "kills": 3}]
                }
            }]
        }
    })
    .to_string();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("realtime request");
        let mut request = vec![0_u8; 4096];
        let read = socket
            .read(&mut request)
            .await
            .expect("read realtime request");
        let request_text = String::from_utf8_lossy(&request[..read]);
        assert!(request_text.starts_with("GET /IDOTA2MatchStats_570/GetRealtimeStats/v1/"));
        assert!(request_text.contains("server_steam_id=99"));
        let invalid = "not-json";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            invalid.len(),
            invalid
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write invalid realtime response");

        let (mut socket, _) = listener.accept().await.expect("league request");
        let read = socket
            .read(&mut request)
            .await
            .expect("read league request");
        let request_text = String::from_utf8_lossy(&request[..read]);
        assert!(request_text.starts_with("GET /IDOTA2Match_570/GetLiveLeagueGames/v1/"));
        assert!(request_text.contains("league_id=5"));
        assert!(request_text.contains("match_id=123"));
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            league_body.len(),
            league_body
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write league response");
    });

    let feed = DotaLiveFeed::new_with_upstream_base(&config(None), format!("http://{address}"))
        .expect("feed");
    feed.publish_match(7, 8, 123, Some(99), 5, 3, true, vec![1])
        .await;
    feed.poll_once().await;
    server.await.expect("server");

    let snapshot = feed.snapshot(7, 8).expect("fallback snapshot");
    assert_eq!(snapshot.source, LiveSnapshotSource::LiveLeagueGames);
    assert_eq!(snapshot.game_time_seconds, Some(91));
    assert_eq!(snapshot.radiant_score, Some(5));
    assert_eq!(snapshot.dire_score, Some(6));
}

#[tokio::test]
async fn disconnected_realtime_stats_falls_back_to_live_league_games() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let league_body = json!({
        "result": {
            "games": [{
                "matchid": 123,
                "scoreboard": {"duration": 92, "dire_team_score": 7}
            }]
        }
    })
    .to_string();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("realtime request");
        let mut request = vec![0_u8; 4096];
        let read = socket
            .read(&mut request)
            .await
            .expect("read realtime request");
        let request_text = String::from_utf8_lossy(&request[..read]);
        assert!(request_text.starts_with("GET /IDOTA2MatchStats_570/GetRealtimeStats/v1/"));
        assert!(request_text.contains("server_steam_id=99"));
        drop(socket);

        let (mut socket, _) = listener.accept().await.expect("league request");
        let read = socket
            .read(&mut request)
            .await
            .expect("read league request");
        let request_text = String::from_utf8_lossy(&request[..read]);
        assert!(request_text.starts_with("GET /IDOTA2Match_570/GetLiveLeagueGames/v1/"));
        assert!(request_text.contains("league_id=5"));
        assert!(request_text.contains("match_id=123"));
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            league_body.len(),
            league_body
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write league response");
    });

    let feed = DotaLiveFeed::new_with_upstream_base(&config(None), format!("http://{address}"))
        .expect("feed");
    feed.publish_match(7, 8, 123, Some(99), 5, 3, true, vec![1])
        .await;
    feed.poll_once().await;
    server.await.expect("server");

    let snapshot = feed.snapshot(7, 8).expect("fallback snapshot");
    assert_eq!(snapshot.source, LiveSnapshotSource::LiveLeagueGames);
    assert_eq!(snapshot.game_time_seconds, Some(92));
    assert_eq!(snapshot.dire_score, Some(7));
}
