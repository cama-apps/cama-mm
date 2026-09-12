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
        test_mode: crate::dota_host_config::DotaHostTestMode::Off,
        guild_ids: vec![7],
        username: Secret::new("bot".to_owned()),
        password: None,
        guard_code: None,
        account_id: 900,
        session_path: PathBuf::from("session.json"),
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

#[test]
fn spectator_events_use_complete_frames_and_preserve_missing_fields() {
    let mut payload = json!({"result": {
        "match": {"match_id": 123, "game_time": 145},
        "teams": [
            {"team_number": 2, "score": 0, "net_worth": 24000},
            {"team_number": 3, "score": 3}
        ],
        "buildings": [
            {"team": 2, "x": 12, "y": 34, "destroyed": false},
            {"team": 3, "x": 56, "y": 78, "destroyed": true},
            {"team": 3, "x": 90, "y": 12},
            {"team": 0, "x": 90, "y": 12, "destroyed": true}
        ]
    }});
    let snapshot = normalize_realtime_stats(&payload, 7, 1, 123, 200).unwrap();
    let frame = snapshot.announcement_frame.unwrap();
    assert_eq!(frame.game_time, 145);
    assert_eq!(frame.radiant_score, Some(0));
    assert_eq!(frame.dire_score, Some(3));
    assert_eq!(frame.radiant_net_worth, Some(24000));
    assert_eq!(frame.dire_net_worth, None);
    assert_eq!(frame.buildings.len(), 2);
    assert!(!frame.buildings[0].destroyed);
    assert!(frame.buildings[1].destroyed);
    payload["result"]["delta_frame"] = json!(true);
    assert!(
        normalize_realtime_stats(&payload, 7, 1, 123, 200)
            .unwrap()
            .announcement_frame
            .is_none()
    );
    payload["result"]
        .as_object_mut()
        .unwrap()
        .remove("delta_frame");
    payload["result"]["match"]
        .as_object_mut()
        .unwrap()
        .remove("game_time");
    assert!(
        normalize_realtime_stats(&payload, 7, 1, 123, 200)
            .unwrap()
            .announcement_frame
            .is_none()
    );
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

fn announcement_roster_payload() -> Value {
    let teams = [2, 3].map(|team| {
        let players = (0..5)
            .map(|slot| {
                json!({
                    "heroid": (team - 2) * 5 + slot + 1,
                    "accountid": (team - 2) * 5 + slot + 100,
                    "team": team, "team_slot": slot,
                    "kill_count": 0, "death_count": 0, "net_worth": 1000,
                    "name": "@everyone", "hero_name": "forged hero"
                })
            })
            .collect::<Vec<_>>();
        json!({"team_number": team, "score": 0, "players": players})
    });
    json!({"match": {"match_id": 123, "game_time": 157}, "teams": teams})
}

fn normalized_announcement(
    payload: &Value,
) -> cama_domain::live_announcements::LiveAnnouncementFrame {
    normalize_realtime_stats(payload, 7, 1, 123, 200)
        .unwrap()
        .announcement_frame
        .unwrap()
}

#[test]
fn commentary_uses_hero_identity_and_complete_team_net_worth() {
    let payload = announcement_roster_payload();
    let frame = normalized_announcement(&payload);
    assert_eq!(frame.players.len(), 10);
    assert_eq!(
        frame.players.iter().filter(|player| player.radiant).count(),
        5
    );
    assert_eq!(frame.players[1].name, "Axe");
    assert_eq!(frame.players[1].kills, Some(0));
    assert_eq!(frame.radiant_net_worth, Some(5000));
    assert_eq!(frame.dire_net_worth, Some(5000));
    assert!(!serde_json::to_string(&frame).unwrap().contains("@everyone"));
    assert!(
        !serde_json::to_string(&frame)
            .unwrap()
            .contains("forged hero")
    );
}

#[test]
fn commentary_never_substitutes_partial_missing_negative_or_overflowed_gold() {
    let base = announcement_roster_payload();
    for broken in [json!(null), json!(-1), json!("no"), json!(i64::MAX)] {
        let mut payload = base.clone();
        payload["teams"][0]["players"][0]["net_worth"] = broken;
        assert_eq!(normalized_announcement(&payload).radiant_net_worth, None);
    }
    let mut payload = base.clone();
    payload["teams"][0]["players"].as_array_mut().unwrap().pop();
    assert_eq!(normalized_announcement(&payload).radiant_net_worth, None);
    let mut payload = base;
    payload["teams"][0]["net_worth"] = json!(7777);
    assert_eq!(
        normalized_announcement(&payload).radiant_net_worth,
        Some(7777)
    );
    payload["teams"][0]["net_worth"] = json!(-1);
    assert_eq!(normalized_announcement(&payload).radiant_net_worth, None);
}

#[test]
fn commentary_rejects_ambiguous_rosters_and_missing_combat_fields_stay_missing() {
    for (key, value) in [("heroid", json!(6)), ("accountid", json!(105))] {
        let mut payload = announcement_roster_payload();
        payload["teams"][0]["players"][0][key] = value;
        let frame = normalized_announcement(&payload);
        assert!(frame.players.is_empty());
        assert_eq!(frame.radiant_net_worth, None);
    }
    for (key, value) in [
        ("team", json!(3)),
        ("team_slot", json!(1)),
        ("heroid", json!(0)),
    ] {
        let mut payload = announcement_roster_payload();
        payload["teams"][0]["players"][0][key] = value;
        let frame = normalized_announcement(&payload);
        assert!(frame.players.len() < 10);
        assert_eq!(frame.radiant_net_worth, None);
    }
    let mut payload = announcement_roster_payload();
    payload["teams"][0]["players"][0]["kill_count"] = json!(null);
    payload["teams"][0]["players"][0]["death_count"] = json!(-1);
    let frame = normalized_announcement(&payload);
    assert_eq!(frame.players[0].kills, None);
    assert_eq!(frame.players[0].deaths, None);
    let duplicate = payload["teams"][0].clone();
    payload["teams"].as_array_mut().unwrap().push(duplicate);
    assert_eq!(normalized_announcement(&payload).radiant_net_worth, None);
    assert_eq!(normalized_announcement(&payload).players.len(), 5);
}

#[test]
fn unsupported_event_and_winner_fields_cannot_fabricate_commentary() {
    let mut payload = announcement_roster_payload();
    payload["match"]["game_state"] = json!(6);
    payload["match"]["radiant_win"] = json!(true);
    payload["radiant_win"] = json!(true);
    payload["events"] = json!([{"kind": "first_blood", "radiant": true}, {"kind": "roshan"}]);
    let frame = normalized_announcement(&payload);
    assert!(frame.events.is_empty());
    assert_eq!(frame.radiant_win, None);
    payload["match"]["match_id"] = json!(124);
    assert!(normalize_realtime_stats(&payload, 7, 1, 123, 200).is_none());
}

#[test]
fn league_commentary_gets_side_from_scoreboard_and_parses_singular_death() {
    let payload = json!({"result": {"games": [{"match_id": 123, "scoreboard": {
        "duration": 150,
        "radiant": {"score": 1, "players": [{"hero_id": 2, "player_slot": 0, "kills": 0, "death": 1}]},
        "dire": {"score": 0, "players": [{"hero_id": 33, "player_slot": 128, "kills": 0, "death": 0}]}
    }}]}});
    let snapshot = normalize_live_league_games(&payload, 7, 1, 123, 200).unwrap();
    assert_eq!(snapshot.players.unwrap()[0].deaths, Some(1));
    let frame = snapshot.announcement_frame.unwrap();
    assert_eq!(frame.players.len(), 2);
    assert_eq!(frame.players[0].name, "Axe");
    assert!(frame.players[0].radiant);
    assert!(!frame.players[1].radiant);
    assert_eq!(frame.players[0].deaths, Some(1));
    assert!(frame.events.is_empty());
    assert_eq!(frame.radiant_net_worth, None);
}

#[test]
fn building_labels_require_explicit_type_and_never_identify_zeroed_tombstones() {
    let mut payload = announcement_roster_payload();
    payload["buildings"] = json!([
        {"team": 2, "x": 1, "y": 2, "type": 0, "tier": 1, "lane": 1, "destroyed": false},
        {"team": 3, "x": 3, "y": 4, "type": 1, "destroyed": false},
        {"team": 2, "x": 5, "y": 6, "type": 2, "destroyed": false},
        {"team": 2, "x": 7, "y": 8, "name": "@everyone mid melee barracks", "destroyed": false},
        {"team": 0, "x": 0, "y": 0, "type": 0, "tier": 0, "lane": 0, "destroyed": true},
        {"team": 3, "x": 9, "y": 10, "type": 99, "tier": 1, "destroyed": true},
        {"team": 3, "x": 11, "y": 12, "type": 0, "tier": 0, "destroyed": true}
    ]);
    let frame = normalized_announcement(&payload);
    assert_eq!(frame.buildings.len(), 6);
    assert_eq!(frame.buildings[0].name.as_deref(), Some("tier 1 tower"));
    assert_eq!(frame.buildings[1].name.as_deref(), Some("barracks"));
    assert_eq!(frame.buildings[2].name.as_deref(), Some("Ancient"));
    assert_eq!(frame.buildings[3].name, None);
    assert_eq!(frame.buildings[4].name, None);
    assert_eq!(frame.buildings[5].name, None);
}

#[test]
fn nested_delta_scoreboard_is_not_an_announcement_baseline() {
    let mut payload = announcement_roster_payload();
    payload["delta_frame"] = json!(true);
    let nested = json!({"match_id": 123, "scoreboard": payload});
    assert!(
        normalize_realtime_stats(&nested, 7, 1, 123, 200)
            .unwrap()
            .announcement_frame
            .is_none()
    );
}

#[test]
fn conflicting_match_aliases_never_attach_another_matchs_commentary() {
    for conflicting in [json!(124), json!(null), json!("bad")] {
        let mut payload = announcement_roster_payload();
        payload["match_id"] = conflicting;
        assert!(normalize_realtime_stats(&payload, 7, 1, 123, 200).is_none());
        let league = json!({"games": [payload]});
        assert!(normalize_live_league_games(&league, 7, 1, 123, 200).is_none());
    }
}

#[test]
fn live_league_fractional_clock_and_outer_delay_are_preserved() {
    // Shape and fractional duration observed in a real Valve live capture.
    let payload = json!({"result":{"games":[{
        "match_id":123,"stream_delay_s":120,
        "scoreboard":{"duration":1693.9666748046875,
            "radiant":{"score":34},"dire":{"score":29}}
    }]}});
    let snapshot = normalize_live_league_games(&payload, 7, 8, 123, 500).unwrap();
    assert_eq!(snapshot.game_time_seconds, Some(1693));
    assert_eq!(snapshot.delay_seconds, Some(120));
    assert_eq!(snapshot.delay_source, Some(LiveDelaySource::Source));
    assert_eq!(snapshot.announcement_frame.unwrap().game_time, 1693);
    assert_eq!(first_game_time(&json!({"duration":-0.25})), Some(-1));
    for invalid in [json!(null), json!(true), json!(1e100)] {
        assert_eq!(first_game_time(&json!({"duration":invalid})), None);
    }
    // Clock parsing must not make fractional IDs or kill counts valid.
    assert_eq!(value_i64(&json!(12.5)), None);
}

#[tokio::test]
async fn partial_realtime_does_not_block_complete_league_announcements() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let partial = json!({"match":{"match_id":"123","game_time":90},
        "delta_frame":true,"teams":[]})
    .to_string();
    let league = json!({"result":{"games":[{"match_id":123,
        "stream_delay_s":120,"scoreboard":{"duration":91.75,
        "radiant":{"score":5},"dire":{"score":6},
        "players":[{"account_id":1,"kills":3}]}}]}})
    .to_string();
    let server = tokio::spawn(async move {
        for (path, body) in [
            ("/IDOTA2MatchStats_570/GetRealtimeStats/", partial),
            ("/IDOTA2Match_570/GetLiveLeagueGames/", league),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).contains(path));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let feed =
        DotaLiveFeed::new_with_upstream_base(&config(None), format!("http://{address}")).unwrap();
    feed.publish_match(7, 8, 123, Some(99), 5, 3, true, vec![1])
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), feed.poll_once())
        .await
        .unwrap();
    server.await.unwrap();
    let snapshot = feed.snapshot(7, 8).unwrap();
    assert_eq!(snapshot.source, LiveSnapshotSource::LiveLeagueGames);
    assert_eq!(snapshot.game_time_seconds, Some(91));
    assert_eq!(snapshot.delay_seconds, Some(120));
    assert!(snapshot.announcement_frame.is_some());
}

#[test]
fn captured_league_tower_flags_produce_losses_without_invented_locations() {
    fn payload(time: f64, towers: Value, barracks: Value) -> Value {
        json!({"result":{"games":[{"match_id":123,"stream_delay_s":120,
            "scoreboard":{"duration":time,"radiant":{"score":40,"tower_state":2047,"barracks_state":63},
            "dire":{"score":31,"tower_state":towers,"barracks_state":barracks}}}]}})
    }
    let before =
        normalize_live_league_games(&payload(1563.75, json!(1974), json!(63)), 7, 8, 123, 500)
            .unwrap()
            .announcement_frame
            .unwrap();
    let after =
        normalize_live_league_games(&payload(1578.5, json!(1972), json!(63)), 7, 8, 123, 515)
            .unwrap()
            .announcement_frame
            .unwrap();
    let lines = cama_domain::live_announcements::announcements(Some(&before), &after);
    assert_eq!(lines, ["Dire's top tier 2 tower goes down."]);
    for invalid in [json!(null), json!(-1), json!(2048), json!(1.5)] {
        let absent =
            normalize_live_league_games(&payload(1578.5, invalid, json!(63)), 7, 8, 123, 515)
                .unwrap()
                .announcement_frame
                .unwrap();
        assert!(cama_domain::live_announcements::announcements(Some(&before), &absent).is_empty());
    }
    let multiple =
        normalize_live_league_games(&payload(1578.5, json!(1956), json!(62)), 7, 8, 123, 515)
            .unwrap()
            .announcement_frame
            .unwrap();
    assert_eq!(
        cama_domain::live_announcements::announcements(Some(&before), &multiple),
        [
            "Dire's top tier 2 tower, mid tier 2 tower and top melee barracks go down. High ground is taking a beating."
        ]
    );
}

fn positioned_roster_payload() -> Value {
    let mut payload = announcement_roster_payload();
    for (team_index, team) in payload["teams"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        for (slot, player) in team["players"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            player["position_x"] = json!(-3798.400390625 + slot as f64 * 100.0);
            player["position_y"] = json!(6562.3505859375 - team_index as f64 * 1000.0);
            player["respawn_timer"] = json!(if slot == 0 { 20 } else { 0 });
        }
        team["tower_state"] = json!(2047);
        team["barracks_state"] = json!(63);
    }
    payload
}

#[test]
fn live_map_preserves_world_positions_death_timers_and_mask_identities() {
    let payload = positioned_roster_payload();
    let snapshot = normalize_realtime_stats(&payload, 7, 1, 123, 200).unwrap();
    let map = snapshot.map_frame.unwrap();
    assert_eq!(
        (map.match_id, map.game_time, map.heroes.len()),
        (123, 157, 10)
    );
    assert_eq!(map.heroes[0].x, Some(-3798.400390625));
    assert_eq!(map.heroes[0].y, Some(6562.3505859375));
    assert_eq!(map.heroes[0].respawn_seconds, Some(20));
    assert_eq!(map.buildings.len(), 34);
    assert_eq!(map.buildings[1].name, "top tier 2 tower");
    assert!(map.buildings.iter().all(|b| b.x.is_none() && b.y.is_none()));
    assert!(map.roshan_respawn_seconds.is_none());
}

#[test]
fn map_missing_invalid_and_placeholder_coordinates_are_not_invented() {
    let normalize = |payload: &Value| {
        normalize_realtime_stats(payload, 7, 1, 123, 200)
            .unwrap()
            .map_frame
    };
    assert!(normalize(&announcement_roster_payload()).is_none());
    let mut payload = positioned_roster_payload();
    payload["teams"][0]["players"][0]["position_x"] = json!(null);
    payload["teams"][0]["players"][1]["position_y"] = json!(20000);
    let map = normalize(&payload).unwrap();
    assert_eq!(map.heroes.len(), 10);
    assert_eq!(
        map.heroes
            .iter()
            .filter(|hero| hero.x.zip(hero.y).is_some())
            .count(),
        8
    );
    assert!(
        map.heroes
            .iter()
            .filter(|hero| [1, 2].contains(&hero.hero_id))
            .all(|hero| hero.x.is_none() && hero.y.is_none())
    );
    assert_eq!(map.heroes[0].respawn_seconds, Some(20));
    for team in payload["teams"].as_array_mut().unwrap() {
        for player in team["players"].as_array_mut().unwrap() {
            player["position_x"] = json!(0);
            player["position_y"] = json!(0);
        }
    }
    assert!(normalize(&payload).is_none());
    payload = positioned_roster_payload();
    payload["teams"][1]["players"][0]["accountid"] = json!(100);
    assert!(normalize(&payload).is_none());
    payload = positioned_roster_payload();
    payload["delta_frame"] = json!(true);
    assert!(normalize(&payload).is_none());
}

#[test]
fn every_single_team_building_bit_has_the_documented_lane_and_tier() {
    let towers = [
        "top tier 1 tower",
        "top tier 2 tower",
        "top tier 3 tower",
        "mid tier 1 tower",
        "mid tier 2 tower",
        "mid tier 3 tower",
        "bottom tier 1 tower",
        "bottom tier 2 tower",
        "bottom tier 3 tower",
        "upper Ancient tier 4 tower",
        "lower Ancient tier 4 tower",
    ];
    let barracks = [
        "top melee barracks",
        "top ranged barracks",
        "mid melee barracks",
        "mid ranged barracks",
        "bottom melee barracks",
        "bottom ranged barracks",
    ];
    for (bit, name) in towers.into_iter().enumerate() {
        assert_eq!(league_building_name("tower_state", bit as u32), name);
    }
    for (bit, name) in barracks.into_iter().enumerate() {
        assert_eq!(league_building_name("barracks_state", bit as u32), name);
    }
}

#[test]
fn map_roster_details_preserve_names_stats_items_and_ultimate_with_missing_fallbacks() {
    let mut payload = positioned_roster_payload();
    let player = &mut payload["teams"][0]["players"][0];
    let account = player["accountid"].clone();
    let hero = player["heroid"].clone();
    player["level"] = json!(15);
    player["gold_per_min"] = json!(564);
    player["ultimate_state"] = json!(3);
    player["ultimate_cooldown"] = json!(0);
    for (slot, id) in [108, 1, -1, 0, 119, 117].iter().enumerate() {
        player[format!("item{slot}")] = json!(id);
    }
    payload["players"] =
        json!([{"account_id": account, "hero_id":hero, "team":0,"name":"  WindTouch  "}]);
    let normalize = |value: &Value| {
        normalize_realtime_stats(value, 7, 1, 123, 200)
            .unwrap()
            .map_frame
            .unwrap()
    };
    let map = normalize(&payload);
    let hero = &map.heroes[0];
    assert_eq!(hero.player_name.as_deref(), Some("WindTouch"));
    assert_eq!(
        (
            hero.level,
            hero.gold_per_min,
            hero.ultimate_state,
            hero.ultimate_cooldown
        ),
        (Some(15), Some(564), Some(3), Some(0))
    );
    assert_eq!(
        hero.items,
        vec![Some(108), Some(1), None, None, Some(119), Some(117)]
    );
    assert!(map.heroes[1].player_name.is_none());
    assert!(map.heroes[1].items.is_empty());
    payload["players"][0]["team"] = json!(1);
    payload["teams"][0]["players"][0]["item2"] = json!(null);
    payload["teams"][0]["players"][0]["ultimate_state"] = json!(99);
    let map = normalize(&payload);
    assert!(map.heroes[0].player_name.is_none());
    assert!(map.heroes[0].items.is_empty());
    assert!(map.heroes[0].ultimate_state.is_none());
    let legacy: LiveMapHero =
        serde_json::from_value(json!({"hero_id":1,"radiant":true,"x":1,"y":2,"respawn_seconds":0}))
            .unwrap();
    assert!(legacy.player_name.is_none());
    assert!(legacy.net_worth.is_none());
    assert!(legacy.items.is_empty());
}

#[test]
fn map_net_worth_requires_complete_observed_player_values() {
    let mut payload = positioned_roster_payload();
    for (team, rate) in [(0, 400), (1, 350)] {
        for player in payload["teams"][team]["players"].as_array_mut().unwrap() {
            player["net_worth"] = json!(rate * 20);
        }
    }
    let normalize = |value: &Value| {
        normalize_realtime_stats(value, 7, 1, 123, 200)
            .unwrap()
            .map_frame
            .unwrap()
    };
    let map = normalize(&payload);
    assert_eq!(
        (map.radiant_net_worth, map.dire_net_worth),
        (Some(40000), Some(35000))
    );
    assert_eq!(map.heroes[0].net_worth, Some(8000));
    assert_eq!(map.heroes[5].net_worth, Some(7000));
    payload["teams"][0]["players"][4]["net_worth"] = json!(null);
    let map = normalize(&payload);
    assert!(map.radiant_net_worth.is_none());
    assert_eq!(map.dire_net_worth, Some(35000));
    let legacy: LiveMapFrame = serde_json::from_value(json!({"match_id":123,"game_time":100,"heroes":[],"buildings":[],"roshan_respawn_seconds":null})).unwrap();
    assert!(legacy.radiant_net_worth.is_none());
    assert!(legacy.dire_net_worth.is_none());
}
