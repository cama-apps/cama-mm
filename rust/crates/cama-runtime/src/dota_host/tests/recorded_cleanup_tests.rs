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
    assert_eq!(f.session().phase, Phase::NeedsReview);
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
        f.worker.tick(&f.port, 205).await.unwrap();
        assert_eq!(f.session().phase, Phase::Recorded);
        assert!(f.session().valve_match_id.is_none());
        assert_eq!(f.state().recorded_match_id, Some(321));
        assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
        assert_eq!(f.port.calls.lock().unwrap().len(), usize::from(owned_lobby));
    }
}

#[tokio::test]
async fn recorded_resolution_rejects_replacement_conflicts() {
    for conflict in [
        "roster",
        "dota_identity",
        "launch_requested",
        "server_assigned",
    ] {
        let f = reviewed_fixture();
        commit_manual_result(&f, true);
        let connection = rusqlite::Connection::open(&f.worker.path).unwrap();
        connection
            .execute(
                "UPDATE matches SET valve_match_id=9002542581 WHERE match_id=321",
                [],
            )
            .unwrap();
        match conflict {
            "roster" => {
                connection
                    .execute(
                        "UPDATE matches SET team1_players='[11,2,3,4,5]' WHERE match_id=321",
                        [],
                    )
                    .unwrap();
            }
            "dota_identity" => {
                f.update_state(|record, _| record.valve_match_id = Some("888".into()))
            }
            "launch_requested" => f.update_state(|_, state| state.launch_requested_at = Some(150)),
            "server_assigned" => f.update_state(|_, state| state.server_id = Some(999)),
            _ => unreachable!(),
        }
        guild_resolution_command(
            f.worker.path.clone(),
            1,
            f.pending,
            42,
            "recorded".into(),
            if conflict == "dota_identity" { 888 } else { 0 },
            "Inspect replacement".into(),
        )
        .await
        .unwrap();
        f.worker.tick(&f.port, 200).await.unwrap();
        assert_eq!(f.session().phase, Phase::NeedsReview, "{conflict}");
        assert!(f.port.calls.lock().unwrap().is_empty(), "{conflict}");
        assert!(
            f.session()
                .last_error
                .unwrap()
                .contains(if conflict == "roster" {
                    "different roster"
                } else {
                    "different Dota identity"
                })
        );
    }
}
