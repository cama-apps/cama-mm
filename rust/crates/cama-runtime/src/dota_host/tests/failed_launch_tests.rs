use super::*;

const GC_UNAVAILABLE: &str = "match details request failed with GC result 2";

async fn loading_match() -> Fixture {
    let f = Fixture::new(false);
    f.launch().await;
    f.running(Some(1));
    *f.port.details_error.lock().unwrap() = Some(GC_UNAVAILABLE.into());
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.session().phase, Phase::Running);
    f
}

fn return_to_lobby(f: &Fixture) {
    let mut current = f.port.lobby.lock().unwrap();
    let current = current.as_mut().unwrap();
    current.stage = LobbyStage::Gathering;
    current.game_state = Some(0);
    current.server_id = None;
    // Valve may retain the failed attempt's match ID on the returned lobby.
}

fn restarted(f: &Fixture) -> DotaHostWorker {
    DotaHostWorker::new(
        &f.worker.path,
        f.worker.config.clone(),
        f.recorder.clone(),
        f.worker.discord.clone(),
        f.worker.live.clone(),
    )
}

#[tokio::test]
async fn failed_connection_recreates_then_invites_and_launches_same_roster() {
    let f = loading_match().await;
    let repo = PendingMatchRepository::new(&f.worker.path);
    let original = repo.pending_match(1, f.pending).unwrap().unwrap();
    use cama_db::betting_service_repository::{BettingServiceRepository, PlaceBetRequest};
    let connection = cama_db::open_runtime_connection(&f.worker.path).unwrap();
    connection.execute("INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance) VALUES(33,1,'viewer',200)", []).unwrap();
    BettingServiceRepository::new(&f.worker.path)
        .place_bet_atomic(PlaceBetRequest {
            guild_id: Some(1),
            pending_match_id: f.pending,
            discord_id: 33,
            team: cama_db::dota_bet_seed_repository::BettingTeam::Radiant,
            amount: 30,
            bet_time: 151,
            leverage: 1,
            max_debt: 0,
            is_blind: false,
            odds_at_placement: None,
        })
        .unwrap();
    return_to_lobby(&f);
    f.worker.tick(&f.port, 155).await.unwrap();
    assert_eq!(f.state().failed_launches.len(), 1);
    assert!(!f.betting_open(155));
    restarted(&f).tick(&f.port, 160).await.unwrap(); // destroy old lobby
    assert_eq!(f.session().lobby_id.as_deref(), Some("777"));
    restarted(&f).tick(&f.port, 165).await.unwrap(); // confirm disappearance
    assert!(f.session().lobby_id.is_none());
    assert!(f.session().valve_match_id.is_none());
    assert!(f.state().launch_requested_at.is_none());
    assert_eq!(f.state().failed_launches[0].match_id, Some(888));
    restarted(&f).tick(&f.port, 170).await.unwrap(); // create exactly once
    {
        let mut current = f.port.lobby.lock().unwrap();
        let current = current.as_mut().unwrap();
        current.id = 778;
        current.members.clear();
    }
    restarted(&f).tick(&f.port, 175).await.unwrap();
    assert_eq!(f.session().phase, Phase::Gathering);
    for id in 1..=10 {
        assert!(
            f.port
                .calls
                .lock()
                .unwrap()
                .contains(&format!("invite:{id}"))
        );
    }
    f.port.lobby.lock().unwrap().as_mut().unwrap().members = lobby(&f.state().settings).members;
    restarted(&f).tick(&f.port, 180).await.unwrap();
    assert_eq!(f.session().phase, Phase::Launching);
    f.running(Some(1));
    f.port.lobby.lock().unwrap().as_mut().unwrap().match_id = Some(889);
    restarted(&f).tick(&f.port, 185).await.unwrap();
    assert_eq!(f.session().valve_match_id.as_deref(), Some("889"));
    let current = repo.pending_match(1, f.pending).unwrap().unwrap();
    assert_eq!(
        current.state.radiant_team_ids,
        original.state.radiant_team_ids
    );
    assert_eq!(current.state.dire_team_ids, original.state.dire_team_ids);
    assert_eq!(current.state.bet_lock_until, original.state.bet_lock_until);
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    let balance: i64 = connection
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=33 AND guild_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        balance, 170,
        "recovery neither refunds nor charges the wager again"
    );
    let calls = f.port.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|c| *c == "create").count(), 2);
    assert_eq!(calls.iter().filter(|c| *c == "launch").count(), 2);
}

#[tokio::test]
async fn match_details_result_two_does_not_restart_connection_or_repeat_launch() {
    let f = loading_match().await;
    assert_eq!(f.session().last_error.as_deref(), Some(GC_UNAVAILABLE));
    f.worker.tick(&f.port, 155).await.unwrap();
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("details:"))
            .count(),
        1
    );
    f.worker.tick(&f.port, 180).await.unwrap();
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("details:"))
            .count(),
        2
    );
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "launch")
            .count(),
        1
    );
    assert!(f.state().lobby_recreation.is_none());
    *f.port.snapshot_error.lock().unwrap() = Some("disconnected".into());
    assert_eq!(
        f.worker.tick(&f.port, 185).await.unwrap_err(),
        "disconnected"
    );
}

#[tokio::test]
async fn replacement_budget_is_three_and_survives_restarts() {
    let f = loading_match().await;
    for attempt in 0..3_u64 {
        let now = 200 + attempt as i64 * 100;
        return_to_lobby(&f);
        restarted(&f).tick(&f.port, now).await.unwrap();
        restarted(&f).tick(&f.port, now + 5).await.unwrap();
        restarted(&f).tick(&f.port, now + 10).await.unwrap();
        restarted(&f).tick(&f.port, now + 15).await.unwrap();
        f.port.lobby.lock().unwrap().as_mut().unwrap().id = 778 + attempt;
        restarted(&f).tick(&f.port, now + 20).await.unwrap();
        f.running(Some(1));
        f.port.lobby.lock().unwrap().as_mut().unwrap().match_id = Some(889 + attempt);
        restarted(&f).tick(&f.port, now + 25).await.unwrap();
    }
    return_to_lobby(&f);
    restarted(&f).tick(&f.port, 500).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(
        f.session()
            .last_error
            .unwrap()
            .contains("retries are exhausted")
    );
    for now in [505, 600, 1000] {
        restarted(&f).tick(&f.port, now).await.unwrap();
    }
    assert_eq!(f.state().failed_launches.len(), 3);
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "create")
            .count(),
        4
    );
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "destroy")
            .count(),
        3
    );
}

#[tokio::test]
async fn lost_destroy_reply_is_reconciled_without_duplicate_creation() {
    let mut f = loading_match().await;
    return_to_lobby(&f);
    f.worker.tick(&f.port, 155).await.unwrap();
    *f.port.destroy_behavior.lock().unwrap() = "lost_reply";
    assert_eq!(
        f.worker.tick(&f.port, 160).await.unwrap_err(),
        "destroy reply lost"
    );
    *f.port.snapshot_error.lock().unwrap() = Some("reconnecting".into());
    assert!(restarted(&f).tick(&f.port, 165).await.is_err());
    assert!(f.state().lobby_recreation.is_some());
    *f.port.snapshot_error.lock().unwrap() = None;
    restarted(&f).tick(&f.port, 170).await.unwrap();
    f.port.lose_create_reply = true;
    assert_eq!(
        restarted(&f).tick(&f.port, 175).await.unwrap_err(),
        "ambiguous response"
    );
    f.port.lobby.lock().unwrap().as_mut().unwrap().id = 778;
    restarted(&f).tick(&f.port, 180).await.unwrap();
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "create")
            .count(),
        2
    );
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "destroy")
            .count(),
        1
    );
}

#[tokio::test]
async fn cleanup_attempts_and_time_are_bounded_without_creating_another_lobby() {
    let f = loading_match().await;
    return_to_lobby(&f);
    f.worker.tick(&f.port, 155).await.unwrap();
    *f.port.destroy_behavior.lock().unwrap() = "ignore";
    for now in [160, 165, 170, 175, 180, 200, 240, 245, 300] {
        restarted(&f).tick(&f.port, now).await.unwrap();
    }
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(
        f.session()
            .last_error
            .unwrap()
            .contains("cleanup timed out")
    );
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "destroy")
            .count(),
        3
    );
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
}

#[tokio::test]
async fn stale_disappeared_foreign_and_played_lobbies_never_trigger_recreation() {
    for case in ["stale", "missing", "foreign", "played", "server", "winner"] {
        let f = loading_match().await;
        if case == "played" {
            f.running(Some(4));
            f.worker.tick(&f.port, 151).await.unwrap();
        }
        return_to_lobby(&f);
        match case {
            "stale" => f.port.observation_stale.store(true, Ordering::SeqCst),
            "missing" => *f.port.lobby.lock().unwrap() = None,
            "foreign" => {
                f.port
                    .lobby
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .owner_account_id = 1234
            }
            "server" => f.port.lobby.lock().unwrap().as_mut().unwrap().server_id = Some(999),
            "winner" => {
                f.port.lobby.lock().unwrap().as_mut().unwrap().winner = Some("radiant".into())
            }
            _ => {}
        }
        f.worker.tick(&f.port, 155).await.unwrap();
        assert!(f.state().lobby_recreation.is_none(), "{case}");
        assert!(
            !f.port.calls.lock().unwrap().iter().any(|c| c == "destroy"),
            "{case}"
        );
        assert_eq!(
            f.port
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| *c == "create")
                .count(),
            1,
            "{case}"
        );
    }
}

#[tokio::test]
async fn ambiguous_launch_without_observed_allocation_never_authorizes_recreation() {
    let f = Fixture::new(false);
    f.worker.tick(&f.port, 100).await.unwrap();
    f.worker.tick(&f.port, 101).await.unwrap();
    assert!(f.state().allocation_observed_at.is_none());
    return_to_lobby(&f);
    f.worker.tick(&f.port, 120).await.unwrap();
    assert!(f.state().lobby_recreation.is_none());
    f.worker.tick(&f.port, 220).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(!f.port.calls.lock().unwrap().iter().any(|c| c == "destroy"));
}

#[tokio::test]
async fn late_old_lobby_and_results_cannot_be_adopted_after_recreation() {
    let f = loading_match().await;
    return_to_lobby(&f);
    let old_lobby = f.port.lobby.lock().unwrap().clone();
    f.worker.tick(&f.port, 155).await.unwrap();
    f.worker.tick(&f.port, 160).await.unwrap();
    f.worker.tick(&f.port, 165).await.unwrap();
    *f.port.lobby.lock().unwrap() = old_lobby;
    f.complete(Some("radiant"));
    f.worker.tick(&f.port, 170).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    assert!(f.session().valve_match_id.is_none());
}

#[tokio::test]
async fn manual_handoff_after_failed_lobby_cleanup_does_not_create_again() {
    let f = loading_match().await;
    return_to_lobby(&f);
    f.worker.tick(&f.port, 155).await.unwrap();
    f.worker.tick(&f.port, 160).await.unwrap();
    f.worker.tick(&f.port, 165).await.unwrap();
    PendingMatchRepository::new(&f.worker.path)
        .request_manual_dota_hosting(1, f.pending)
        .unwrap();
    f.worker.tick(&f.port, 170).await.unwrap();
    assert_eq!(f.session().phase, Phase::Cancelled);
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
    assert!(
        PendingMatchRepository::new(&f.worker.path)
            .pending_match(1, f.pending)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn cleanup_rechecks_server_ownership_and_pending_match_before_destroying() {
    for case in ["running", "foreign", "deleted", "roster", "stale"] {
        let f = loading_match().await;
        return_to_lobby(&f);
        f.worker.tick(&f.port, 155).await.unwrap();
        match case {
            "running" => f.running(Some(1)),
            "foreign" => {
                f.port
                    .lobby
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .owner_account_id = 1234
            }
            "deleted" => {
                PendingMatchRepository::new(&f.worker.path)
                    .delete_pending_match(1, f.pending)
                    .unwrap();
            }
            "roster" => {
                let repo = PendingMatchRepository::new(&f.worker.path);
                let mut pending = repo.pending_match(1, f.pending).unwrap().unwrap();
                pending.state.radiant_team_ids.swap(0, 1);
                // Reordering is harmless; changing sides is not.
                std::mem::swap(
                    &mut pending.state.radiant_team_ids[0],
                    &mut pending.state.dire_team_ids[0],
                );
                repo.update_pending_match(1, f.pending, &pending.state)
                    .unwrap();
            }
            "stale" => f.port.observation_stale.store(true, Ordering::SeqCst),
            _ => unreachable!(),
        }
        restarted(&f).tick(&f.port, 160).await.unwrap();
        assert!(
            !f.port.calls.lock().unwrap().iter().any(|c| c == "destroy"),
            "{case}"
        );
        assert_eq!(
            f.port
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| *c == "create")
                .count(),
            1,
            "{case}"
        );
        if case != "stale" {
            assert_eq!(f.session().phase, Phase::NeedsReview, "{case}");
        }
    }
}
