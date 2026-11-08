use super::*;

fn return_played_game_to_lobby(f: &Fixture) {
    let mut current = f.port.lobby.lock().unwrap();
    let current = current.as_mut().unwrap();
    current.stage = LobbyStage::Gathering;
    current.game_state = Some(0);
    current.server_id = None;
}

async fn played_game() -> Fixture {
    let f = Fixture::new(false);
    f.launch().await;
    f.running(Some(5));
    f.worker.tick(&f.port, 150).await.unwrap();
    assert!(f.state().betting_closed);
    f
}

fn assert_no_lobby_mutation(f: &Fixture) {
    let calls = f.port.calls.lock().unwrap();
    assert!(
        calls.iter().all(|call| call.starts_with("details:")),
        "unexpected lobby mutation: {calls:?}"
    );
}

#[tokio::test]
async fn returned_played_game_keeps_polling_until_result_is_available() {
    let f = played_game().await;
    return_played_game_to_lobby(&f);
    *f.port.details_error.lock().unwrap() = Some("match details are not published yet".into());
    f.port.calls.lock().unwrap().clear();

    f.worker.tick(&f.port, 200).await.unwrap();
    assert!(f.port.calls.lock().unwrap().contains(&"details:888".into()));
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    assert!(f.state().lobby_recreation.is_none());
    assert_no_lobby_mutation(&f);

    *f.port.details_error.lock().unwrap() = None;
    f.complete(Some("radiant"));
    return_played_game_to_lobby(&f);
    f.worker.tick(&f.port, 235).await.unwrap();
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
    assert_eq!(f.state().recorded_match_id, Some(123));
    assert_eq!(f.session().valve_match_id.as_deref(), Some("888"));
    assert_no_lobby_mutation(&f);
    f.worker.tick(&f.port, 240).await.unwrap();
    f.worker.tick(&f.port, 245).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn historical_reviewed_returned_game_reconciles_automatically_or_after_resume() {
    for resume in [false, true] {
        let f = played_game().await;
        f.complete(Some("dire"));
        return_played_game_to_lobby(&f);
        f.update_state(|record, _| {
            record.phase = Phase::NeedsReview;
            record.last_error = Some(
                "Dota returned to the lobby, but a safe pregame reset could not be confirmed; automatic recreation is paused".into(),
            );
        });
        f.port.calls.lock().unwrap().clear();

        if resume {
            guild_operator_command(f.worker.path.clone(), "resume", 1, Some(f.pending))
                .await
                .unwrap();
        }
        f.worker.tick(&f.port, 200).await.unwrap();
        assert_eq!(
            f.recorder.count.load(Ordering::SeqCst),
            1,
            "resume: {resume}"
        );
        assert_eq!(f.state().recorded_match_id, Some(123));
        assert!(!f.state().resume_requested);
        assert_no_lobby_mutation(&f);
    }
}

#[tokio::test]
async fn postgame_without_current_match_id_reconciles_saved_identity() {
    let f = played_game().await;
    f.complete(Some("radiant"));
    f.port.lobby.lock().unwrap().as_mut().unwrap().match_id = None;
    f.port.calls.lock().unwrap().clear();

    f.worker.tick(&f.port, 200).await.unwrap();
    assert_eq!(f.session().valve_match_id.as_deref(), Some("888"));
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
    assert_eq!(f.state().recorded_match_id, Some(123));
    assert_no_lobby_mutation(&f);
}

#[tokio::test]
async fn returned_and_sparse_postgame_results_still_require_verified_identity_roster_and_winner() {
    for sparse_postgame in [false, true] {
        for invalid in ["match", "league", "roster", "winner", "unfinished"] {
            let f = played_game().await;
            f.complete(Some("radiant"));
            if sparse_postgame {
                f.port.lobby.lock().unwrap().as_mut().unwrap().match_id = None;
            } else {
                return_played_game_to_lobby(&f);
            }
            {
                let mut details = f.port.details.lock().unwrap();
                let details = details.as_mut().unwrap();
                match invalid {
                    "match" => details.match_id = 889,
                    "league" => details.league_id = 456,
                    "roster" => details.players[0].account32 = 42,
                    "winner" => details.winner = None,
                    "unfinished" => details.finished = false,
                    _ => unreachable!(),
                }
            }
            f.port.calls.lock().unwrap().clear();
            f.worker.tick(&f.port, 200).await.unwrap();
            assert!(
                f.port.calls.lock().unwrap().contains(&"details:888".into()),
                "sparse postgame {sparse_postgame}, invalid {invalid}"
            );
            assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
            assert!(f.state().recorded_match_id.is_none());
            assert!(f.state().lobby_recreation.is_none());
            assert!(
                PendingMatchRepository::new(&f.worker.path)
                    .pending_match(1, f.pending)
                    .unwrap()
                    .is_some()
            );
            assert_no_lobby_mutation(&f);
        }
    }
}

#[tokio::test]
async fn stale_gathering_observation_waits_for_refresh_before_launch() {
    let f = Fixture::new(false);
    f.worker.tick(&f.port, 100).await.unwrap();
    f.port.calls.lock().unwrap().clear();
    f.port.observation_stale.store(true, Ordering::SeqCst);
    f.worker.tick(&f.port, 101).await.unwrap();
    assert!(f.port.calls.lock().unwrap().is_empty());
    assert!(f.state().launch_requested_at.is_none());

    f.port.observation_stale.store(false, Ordering::SeqCst);
    f.worker.tick(&f.port, 110).await.unwrap();
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.as_str() == "launch")
            .count(),
        1
    );
}

#[tokio::test]
async fn allocation_or_gameplay_with_lagging_stage_never_sends_another_launch() {
    for game_state in [1, 5] {
        let f = Fixture::new(false);
        f.worker.tick(&f.port, 100).await.unwrap();
        {
            let mut current = f.port.lobby.lock().unwrap();
            let current = current.as_mut().unwrap();
            current.stage = LobbyStage::Gathering;
            current.game_state = Some(game_state);
            current.server_id = Some(999);
            current.match_id = Some(888);
            current.launch_history = HostLaunchHistory {
                allocation_observed: true,
                gameplay_observed: game_state == 5,
                match_id: Some(888),
                ..Default::default()
            };
        }
        f.port.calls.lock().unwrap().clear();
        f.worker.tick(&f.port, 101).await.unwrap();
        assert_eq!(f.session().valve_match_id.as_deref(), Some("888"));
        assert!(f.state().launch_requested_at.is_none());
        assert_no_lobby_mutation(&f);
    }
}

#[tokio::test]
async fn gathering_stage_does_not_hide_result_behind_an_old_launch_intent() {
    let f = played_game().await;
    f.complete(Some("radiant"));
    // Keep a current server/game-rules state, but lag the outer lifecycle.
    f.port.lobby.lock().unwrap().as_mut().unwrap().stage = LobbyStage::Gathering;
    f.port.calls.lock().unwrap().clear();
    assert_eq!(f.state().launch_requested_at, Some(101));
    f.worker.tick(&f.port, 200).await.unwrap();
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
    assert_eq!(f.state().recorded_match_id, Some(123));
    assert_no_lobby_mutation(&f);
}

#[tokio::test]
async fn changed_refresh_rejects_roster_mutations_and_launch() {
    for change in ["owner", "lobby", "allocation"] {
        let f = Fixture::new(false);
        f.worker.tick(&f.port, 100).await.unwrap();
        let mut changed = f.port.lobby.lock().unwrap().clone().unwrap();
        match change {
            "owner" => changed.owner_account_id = 42,
            "lobby" => changed.id = 778,
            "allocation" => {
                changed.stage = LobbyStage::Allocating;
                changed.match_id = Some(888);
                changed.server_id = Some(999);
            }
            _ => unreachable!(),
        }
        // A stale roster would cause a kick if admission continues after refresh.
        f.port
            .lobby
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .members
            .push(LobbySeat {
                account_id: 42,
                side: Some(Side::Radiant),
                slot: 0,
            });
        *f.port.lobby_on_fresh_check.lock().unwrap() = Some(changed);
        f.port.calls.lock().unwrap().clear();
        f.worker.tick(&f.port, 101).await.unwrap();
        assert!(f.port.calls.lock().unwrap().is_empty(), "{change}");
        assert!(f.state().launch_requested_at.is_none(), "{change}");
    }
}

struct ChangedBeforeLaunch<'a> {
    port: &'a FakePort,
    replacement: HostLobby,
}

#[async_trait]
impl DotaHostPort for ChangedBeforeLaunch<'_> {
    async fn snapshot(&self) -> Result<Option<HostLobby>, String> {
        self.port.snapshot().await
    }
    async fn betting_observation_fresh(&self) -> bool {
        self.port.betting_observation_fresh().await
    }
    async fn create(&self, settings: &LobbySettings) -> Result<(), String> {
        self.port.create(settings).await
    }
    async fn invite(&self, lobby_id: u64, account_id: u32) -> Result<(), String> {
        self.port.invite(lobby_id, account_id).await
    }
    async fn move_host_to_pool(&self, lobby_id: u64) -> Result<(), String> {
        self.port.move_host_to_pool(lobby_id).await
    }
    async fn kick_from_team(&self, lobby_id: u64, account_id: u32) -> Result<(), String> {
        self.port.kick_from_team(lobby_id, account_id).await
    }
    async fn kick(&self, lobby_id: u64, account_id: u32) -> Result<(), String> {
        self.port.kick(lobby_id, account_id).await
    }
    async fn launch(&self, lobby_id: u64) -> Result<(), String> {
        self.port.launch(lobby_id).await
    }
    async fn destroy(&self, lobby_id: u64) -> Result<(), String> {
        self.port.destroy(lobby_id).await
    }
    async fn match_details(&self, match_id: u64) -> Result<HostMatchDetails, String> {
        self.port.match_details(match_id).await
    }
    async fn launch_observed(&self, expected: &HostLobby) -> Result<bool, String> {
        let saved = DotaSessionRepository::new(&self.port.path)
            .session(1, self.port.pending)
            .unwrap()
            .unwrap();
        let state: SessionState = serde_json::from_value(saved.payload).unwrap();
        assert_eq!(saved.phase, Phase::Launching);
        assert!(state.launch_requested_at.is_some());
        *self.port.lobby.lock().unwrap() = Some(self.replacement.clone());
        self.port.launch_observed(expected).await
    }
}

#[tokio::test]
async fn launch_precondition_change_clears_unsent_durable_intent() {
    for change in ["owner", "lobby", "allocation"] {
        let f = Fixture::new(false);
        f.worker.tick(&f.port, 100).await.unwrap();
        let mut replacement = f.port.lobby.lock().unwrap().clone().unwrap();
        match change {
            "owner" => replacement.owner_account_id = 42,
            "lobby" => replacement.id = 778,
            "allocation" => {
                replacement.stage = LobbyStage::Allocating;
                replacement.match_id = Some(888);
                replacement.server_id = Some(999);
            }
            _ => unreachable!(),
        }
        f.port.calls.lock().unwrap().clear();
        let changing_port = ChangedBeforeLaunch {
            port: &f.port,
            replacement,
        };
        f.worker.tick(&changing_port, 101).await.unwrap();
        assert!(f.port.calls.lock().unwrap().is_empty(), "{change}");
        assert!(f.state().launch_requested_at.is_none(), "{change}");
        assert_eq!(f.session().phase, Phase::Gathering);
    }
}

fn recorded_idle_lobby() -> Fixture {
    let f = Fixture::new(false);
    f.update_state(|record, state| {
        record.phase = Phase::NeedsReview;
        record.lobby_id = Some("777".into());
        record.valve_match_id = Some("888".into());
        state.launch_requested_at = Some(100);
        state.allocation_observed_at = Some(110);
        state.server_id = Some(999);
        state.betting_closed = true;
        state.recorded_match_id = Some(321);
    });
    let mut current = lobby(&f.state().settings);
    current.match_id = Some(888);
    current.game_state = Some(0);
    *f.port.lobby.lock().unwrap() = Some(current);
    rusqlite::Connection::open(&f.worker.path)
        .unwrap()
        .execute(
            "INSERT INTO matches(match_id,team1_players,team2_players,winning_team,guild_id,pending_match_id)
             VALUES(321,'[1,2,3,4,5]','[6,7,8,9,10]',2,1,?1)",
            [f.pending],
        )
        .unwrap();
    PendingMatchRepository::new(&f.worker.path)
        .delete_pending_match(1, f.pending)
        .unwrap();
    f
}

#[tokio::test]
async fn resume_restarts_exhausted_recorded_cleanup_without_losing_match_identity() {
    let f = recorded_idle_lobby();
    *f.port.destroy_behavior.lock().unwrap() = "ignore";
    for now in [200, 210, 220, 230] {
        f.worker.tick(&f.port, now).await.unwrap();
    }
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert_eq!(*f.port.calls.lock().unwrap(), vec!["destroy"; 3]);

    *f.port.destroy_behavior.lock().unwrap() = "normal";
    guild_operator_command(f.worker.path.clone(), "resume", 1, Some(f.pending))
        .await
        .unwrap();
    f.worker.tick(&f.port, 300).await.unwrap();
    assert_eq!(*f.port.calls.lock().unwrap(), vec!["destroy"; 4]);
    assert!(!f.state().resume_requested);
    assert_eq!(f.state().launch_requested_at, Some(100));
    assert_eq!(f.session().valve_match_id.as_deref(), Some("888"));
    f.worker.tick(&f.port, 305).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
    assert_eq!(f.state().recorded_match_id, Some(321));
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cleanup_rechecks_lobby_that_changed_while_refreshing() {
    let f = recorded_idle_lobby();
    let mut running = f.port.lobby.lock().unwrap().clone().unwrap();
    running.stage = LobbyStage::Running;
    running.game_state = Some(5);
    running.server_id = Some(999);
    *f.port.lobby_on_fresh_check.lock().unwrap() = Some(running);

    f.worker.tick(&f.port, 200).await.unwrap();
    assert_no_lobby_mutation(&f);
    assert!(f.port.lobby.lock().unwrap().is_some());
    assert_eq!(f.state().recorded_match_id, Some(321));
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);

    return_played_game_to_lobby(&f);
    f.worker.tick(&f.port, 205).await.unwrap();
    assert!(f.port.calls.lock().unwrap().contains(&"destroy".into()));
    f.worker.tick(&f.port, 210).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
}
