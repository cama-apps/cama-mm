use super::*;

fn reviewed_fixture() -> Fixture {
    let f = Fixture::new(false);
    f.update_state(|record, state| {
        record.phase = Phase::NeedsReview;
        record.last_error = Some("lobby creation is still unconfirmed".into());
        state.create_requested_at = Some(100);
    });
    f
}

fn commit_manual_result(f: &Fixture, settlement_complete: bool) {
    rusqlite::Connection::open(&f.worker.path)
        .unwrap()
        .execute(
            "INSERT INTO matches(match_id,team1_players,team2_players,winning_team,guild_id,pending_match_id)
             VALUES(321,'[1,2,3,4,5]','[6,7,8,9,10]',2,1,?1)",
            [f.pending],
        )
        .unwrap();
    if settlement_complete {
        PendingMatchRepository::new(&f.worker.path)
            .delete_pending_match(1, f.pending)
            .unwrap();
    }
}

fn returned_recorded_fixture() -> Fixture {
    let f = reviewed_fixture();
    f.update_state(|record, state| {
        record.lobby_id = Some("777".into());
        record.valve_match_id = Some("888".into());
        record.last_error = Some("launch was requested but Dota has not allocated a server".into());
        state.launch_requested_at = Some(100);
        state.allocation_observed_at = Some(110);
        state.server_id = Some(999);
        state.betting_closed = true;
    });
    let mut current = lobby(&f.state().settings);
    current.match_id = Some(888);
    current.game_state = Some(0);
    *f.port.lobby.lock().unwrap() = Some(current);
    commit_manual_result(&f, true);
    f
}

async fn tick_after_restart(f: &Fixture, now: i64) -> Result<(), String> {
    DotaHostWorker::new(
        &f.worker.path,
        f.worker.config.clone(),
        f.recorder.clone(),
        f.worker.discord.clone(),
        f.worker.live.clone(),
    )
    .tick(&f.port, now)
    .await
}

#[tokio::test]
async fn recorded_failed_launch_releases_host_automatically_and_with_queued_resolution() {
    for explicit in [false, true] {
        let f = returned_recorded_fixture();
        if explicit {
            guild_resolution_command(
                f.worker.path.clone(),
                1,
                f.pending,
                42,
                "recorded".into(),
                888,
                "Players recorded the replacement".into(),
            )
            .await
            .unwrap();
            let status =
                guild_operator_command(f.worker.path.clone(), "status", 1, Some(f.pending))
                    .await
                    .unwrap();
            assert!(status.contains("Resolution queued"));
            assert!(!status.contains("has not allocated a server"));
        }
        f.worker.tick(&f.port, 200).await.unwrap();
        assert_eq!(*f.port.calls.lock().unwrap(), vec!["destroy"]);
        assert_eq!(f.session().phase, Phase::Finishing);
        assert!(
            f.session()
                .last_error
                .unwrap()
                .contains("awaiting confirmation")
        );
        assert_eq!(
            DotaSessionRepository::new(&f.worker.path)
                .active_sessions()
                .unwrap()
                .len(),
            1
        );
        tick_after_restart(&f, 205).await.unwrap();
        assert_eq!(f.session().phase, Phase::Recorded);
        assert_eq!(f.state().recorded_match_id, Some(321));
        assert!(f.session().last_error.is_none());
        assert!(
            DotaSessionRepository::new(&f.worker.path)
                .active_sessions()
                .unwrap()
                .is_empty()
        );
        assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn recorded_cleanup_bounds_refreshes_across_restarts_then_accepts_fresh_state() {
    let f = returned_recorded_fixture();
    f.port.observation_stale.store(true, Ordering::SeqCst);
    for now in [200, 205, 210] {
        assert!(
            tick_after_restart(&f, now)
                .await
                .unwrap_err()
                .contains("refreshing stale GC ownership")
        );
    }
    for now in [215, 300] {
        tick_after_restart(&f, now).await.unwrap();
    }
    assert!(
        f.session()
            .last_error
            .unwrap()
            .contains("three GC refresh attempts")
    );
    assert!(f.port.calls.lock().unwrap().is_empty());
    f.port.observation_stale.store(false, Ordering::SeqCst);
    tick_after_restart(&f, 305).await.unwrap();
    tick_after_restart(&f, 310).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
}

#[tokio::test]
async fn recorded_cleanup_stops_destroy_retries_and_waits_for_confirmed_removal() {
    let f = returned_recorded_fixture();
    *f.port.destroy_behavior.lock().unwrap() = "ignore";
    for now in [200, 205, 210, 220, 230, 300] {
        tick_after_restart(&f, now).await.unwrap();
    }
    assert_eq!(
        *f.port.calls.lock().unwrap(),
        vec!["destroy", "destroy", "destroy"]
    );
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(
        f.session()
            .last_error
            .unwrap()
            .contains("retries are exhausted")
    );
    *f.port.lobby.lock().unwrap() = None;
    tick_after_restart(&f, 305).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
}

#[tokio::test]
async fn recorded_returned_lobby_requires_ownership_matching_identity_and_no_active_server() {
    for case in ["owner", "match", "lobby", "server", "gameplay", "settings"] {
        for explicit in [false, true] {
            let f = returned_recorded_fixture();
            if explicit {
                guild_resolution_command(
                    f.worker.path.clone(),
                    1,
                    f.pending,
                    42,
                    "recorded".into(),
                    888,
                    "Already recorded".into(),
                )
                .await
                .unwrap();
            }
            {
                let mut current = f.port.lobby.lock().unwrap();
                let current = current.as_mut().unwrap();
                match case {
                    "owner" => current.owner_account_id = 42,
                    "match" => current.match_id = Some(9999),
                    "lobby" => current.id = 8888,
                    "server" => current.server_id = Some(999),
                    "gameplay" => current.game_state = Some(5),
                    "settings" => current.league_id = 999,
                    _ => unreachable!(),
                }
            }
            f.worker.tick(&f.port, 200).await.unwrap();
            assert!(
                !f.port.calls.lock().unwrap().iter().any(|c| c == "destroy"),
                "{case}, explicit={explicit}"
            );
            assert_eq!(
                f.session().phase,
                Phase::NeedsReview,
                "{case}, explicit={explicit}"
            );
        }
    }
}

#[tokio::test]
async fn recording_during_failed_launch_recreation_cleans_up_without_creating_again() {
    let f = Fixture::new(false);
    f.launch().await;
    f.running(Some(1));
    f.worker.tick(&f.port, 150).await.unwrap();
    {
        let mut current = f.port.lobby.lock().unwrap();
        let current = current.as_mut().unwrap();
        current.stage = LobbyStage::Gathering;
        current.game_state = Some(0);
        current.server_id = None;
    }
    f.worker.tick(&f.port, 155).await.unwrap();
    assert!(f.state().lobby_recreation.is_some());
    commit_manual_result(&f, true);
    tick_after_restart(&f, 160).await.unwrap();
    tick_after_restart(&f, 165).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
    assert!(f.state().lobby_recreation.is_none());
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "create")
            .count(),
        1
    );
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn manual_recording_releases_reviewed_account_without_recreating_lobby() {
    let f = reviewed_fixture();
    commit_manual_result(&f, true);
    f.worker.tick(&f.port, 200).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
    assert_eq!(f.state().recorded_match_id, Some(321));
    assert!(f.session().last_error.is_none());
    assert!(f.port.calls.lock().unwrap().is_empty());
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    assert!(
        DotaSessionRepository::new(&f.worker.path)
            .active_sessions()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn reviewed_account_stays_reserved_until_manual_settlement_completes() {
    for has_result in [false, true] {
        let f = reviewed_fixture();
        if has_result {
            commit_manual_result(&f, false);
        } else {
            PendingMatchRepository::new(&f.worker.path)
                .delete_pending_match(1, f.pending)
                .unwrap();
        }
        f.worker.tick(&f.port, 200).await.unwrap();
        assert_eq!(f.session().phase, Phase::NeedsReview);
        assert!(f.state().recorded_match_id.is_none());
        assert!(f.port.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn manual_recording_cleans_owned_unlaunched_lobby_then_releases_account() {
    let f = reviewed_fixture();
    *f.port.lobby.lock().unwrap() = Some(lobby(&f.state().settings));
    commit_manual_result(&f, true);
    f.worker.tick(&f.port, 200).await.unwrap();
    assert_eq!(*f.port.calls.lock().unwrap(), vec!["destroy"]);
    assert_eq!(f.session().phase, Phase::Finishing);
    f.worker.tick(&f.port, 205).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn manual_recording_does_not_destroy_foreign_or_potentially_active_lobby() {
    for scenario in [
        "foreign",
        "allocating",
        "running",
        "launch_requested",
        "server_assigned",
    ] {
        let f = reviewed_fixture();
        let mut current = lobby(&f.state().settings);
        match scenario {
            "foreign" => current.owner_account_id = 42,
            "allocating" => current.stage = LobbyStage::Allocating,
            "running" => current.stage = LobbyStage::Running,
            "launch_requested" => {
                f.update_state(|_, state| state.launch_requested_at = Some(150));
            }
            "server_assigned" => current.server_id = Some(999),
            _ => unreachable!(),
        }
        *f.port.lobby.lock().unwrap() = Some(current);
        commit_manual_result(&f, true);
        f.worker.tick(&f.port, 200).await.unwrap();
        assert_eq!(f.session().phase, Phase::NeedsReview, "{scenario}");
        assert!(f.port.calls.lock().unwrap().is_empty(), "{scenario}");
    }
}

#[tokio::test]
async fn manual_recording_waits_for_reconnection_and_fresh_ownership() {
    let f = reviewed_fixture();
    commit_manual_result(&f, true);
    *f.port.snapshot_error.lock().unwrap() = Some("disconnected".into());
    assert!(f.worker.tick(&f.port, 200).await.is_err());
    assert_eq!(f.session().phase, Phase::NeedsReview);
    *f.port.snapshot_error.lock().unwrap() = None;
    *f.port.lobby.lock().unwrap() = Some(lobby(&f.state().settings));
    f.port.observation_stale.store(true, Ordering::SeqCst);
    assert!(f.worker.tick(&f.port, 205).await.is_err());
    assert!(f.port.calls.lock().unwrap().is_empty());
    assert_eq!(f.session().phase, Phase::NeedsReview);
}

#[tokio::test]
async fn recorded_resolution_accepts_manual_replacement_of_unassigned_host() {
    for owned_lobby in [false, true] {
        let f = reviewed_fixture();
        commit_manual_result(&f, true);
        rusqlite::Connection::open(&f.worker.path)
            .unwrap()
            .execute(
                "UPDATE matches SET valve_match_id=9002542581 WHERE match_id=321",
                [],
            )
            .unwrap();
        if owned_lobby {
            *f.port.lobby.lock().unwrap() = Some(lobby(&f.state().settings));
        }
        guild_resolution_command(
            f.worker.path.clone(),
            1,
            f.pending,
            42,
            "recorded".into(),
            0,
            "Manually recorded replacement".into(),
        )
        .await
        .unwrap();
        f.worker.tick(&f.port, 200).await.unwrap();
        if owned_lobby {
            assert_eq!(f.session().phase, Phase::Finishing);
        }
        tick_after_restart(&f, 205).await.unwrap();
        assert_eq!(f.session().phase, Phase::Recorded);
        assert!(f.session().valve_match_id.is_none());
        assert_eq!(f.state().recorded_match_id, Some(321));
        assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
        assert_eq!(f.port.calls.lock().unwrap().len(), usize::from(owned_lobby));
    }
}

fn record_replacement_identity(f: &Fixture) {
    cama_db::open_runtime_connection(&f.worker.path)
        .unwrap()
        .execute(
            "UPDATE matches SET valve_match_id=9002542581 WHERE match_id=321",
            [],
        )
        .unwrap();
}

async fn queue_recorded_resolution(f: &Fixture) {
    guild_resolution_command(
        f.worker.path.clone(),
        1,
        f.pending,
        42,
        "recorded".into(),
        888,
        "Replacement result recorded by players; release the old lobby".into(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn recorded_replacement_releases_old_host_without_changing_either_game_identity() {
    for old_lobby in ["reset", "postgame", "missing", "finished_details"] {
        let f = returned_recorded_fixture();
        record_replacement_identity(&f);
        // Reproduce #615: an already recorded result, a different old hosted
        // match ID, and an exhausted refresh budget from the previous process.
        f.update_state(|_, state| {
            state.recorded_match_id = Some(321);
            state.lobby_cleanup = serde_json::from_value(serde_json::json!({
                "refresh_attempts": 3,
            }))
            .unwrap();
        });
        match old_lobby {
            "postgame" => f.complete(Some("radiant")),
            "missing" => *f.port.lobby.lock().unwrap() = None,
            "finished_details" => {
                f.complete(Some("radiant"));
                f.port.lobby.lock().unwrap().as_mut().unwrap().stage = LobbyStage::Running;
            }
            _ => {}
        }
        queue_recorded_resolution(&f).await;
        tick_after_restart(&f, 200).await.unwrap();
        tick_after_restart(&f, 205).await.unwrap();
        assert_eq!(f.session().phase, Phase::Recorded, "{old_lobby}");
        assert_eq!(f.session().valve_match_id.as_deref(), Some("888"));
        assert_eq!(f.session().lobby_id.as_deref(), Some("777"));
        assert_eq!(f.state().recorded_match_id, Some(321));
        assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
        assert!(
            DotaSessionRepository::new(&f.worker.path)
                .active_sessions()
                .unwrap()
                .is_empty()
        );
        let committed: (i64, i64) = cama_db::open_runtime_connection(&f.worker.path)
            .unwrap()
            .query_row(
                "SELECT valve_match_id,winning_team FROM matches WHERE match_id=321",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            committed,
            (9002542581, 2),
            "old Radiant result must not replace recorded Dire result"
        );
        let calls = f.port.calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|c| *c == "destroy").count(),
            usize::from(old_lobby != "missing")
        );
        assert!(
            !calls
                .iter()
                .any(|c| c == "create" || c == "launch" || c == "details:9002542581")
        );
    }
}

#[tokio::test]
async fn replacement_result_does_not_authorize_destroying_an_active_or_foreign_old_lobby() {
    for blocked in [
        "active",
        "foreign",
        "replacement_lobby",
        "stale",
        "changed_intent",
    ] {
        let f = returned_recorded_fixture();
        record_replacement_identity(&f);
        queue_recorded_resolution(&f).await;
        match blocked {
            "active" => f.running(Some(5)),
            "foreign" => {
                f.port
                    .lobby
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .owner_account_id = 42
            }
            "replacement_lobby" => {
                f.port.lobby.lock().unwrap().as_mut().unwrap().match_id = Some(9002542581)
            }
            "stale" => f.port.observation_stale.store(true, Ordering::SeqCst),
            "changed_intent" => {
                f.update_state(|record, _| record.valve_match_id = Some("999".into()))
            }
            _ => unreachable!(),
        }
        let outcome = f.worker.tick(&f.port, 200).await;
        if blocked == "stale" {
            assert!(outcome.is_err());
        } else {
            outcome.unwrap();
        }
        assert_eq!(f.session().phase, Phase::NeedsReview, "{blocked}");
        assert!(
            !f.port.calls.lock().unwrap().iter().any(|c| c == "destroy"),
            "{blocked}"
        );
        assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn recording_conflicts_are_reported_before_stale_gc_without_consuming_refresh_budget() {
    for conflict in [
        "roster",
        "other_pending",
        "other_guild",
        "unfinished_settlement",
    ] {
        let f = returned_recorded_fixture();
        record_replacement_identity(&f);
        let connection = cama_db::open_runtime_connection(&f.worker.path).unwrap();
        match conflict {
            "roster" => {
                connection
                    .execute(
                        "UPDATE matches SET team1_players='[11,2,3,4,5]' WHERE match_id=321",
                        [],
                    )
                    .unwrap();
            }
            "other_pending" => {
                connection
                    .execute(
                        "UPDATE matches SET pending_match_id=999 WHERE match_id=321",
                        [],
                    )
                    .unwrap();
            }
            "other_guild" => {
                connection
                    .execute("UPDATE matches SET guild_id=2 WHERE match_id=321", [])
                    .unwrap();
            }
            "unfinished_settlement" => {
                connection.execute("INSERT INTO pending_matches(pending_match_id,guild_id,payload) VALUES(?1,1,'{}')", [f.pending]).unwrap();
            }
            _ => unreachable!(),
        }
        f.port.observation_stale.store(true, Ordering::SeqCst);
        queue_recorded_resolution(&f).await;
        for now in [200, 205, 300] {
            tick_after_restart(&f, now).await.unwrap();
        }
        assert_eq!(f.session().phase, Phase::NeedsReview, "{conflict}");
        let error = f.session().last_error.unwrap();
        assert!(
            error.contains(match conflict {
                "roster" => "different roster",
                "unfinished_settlement" => "settlement is incomplete",
                _ => "record the verified winner",
            }),
            "{conflict}: {error}"
        );
        assert_eq!(f.session().payload["lobby_cleanup"]["refresh_attempts"], 0);
        assert!(f.port.calls.lock().unwrap().is_empty());
    }
}
