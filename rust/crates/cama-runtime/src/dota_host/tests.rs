use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct FakePort {
    lobby: Mutex<Option<HostLobby>>,
    snapshot_error: Mutex<Option<String>>,
    details: Mutex<Option<HostMatchDetails>>,
    calls: Mutex<Vec<String>>,
    lose_create_reply: bool,
    path: PathBuf,
    pending: i64,
}

#[async_trait]
impl DotaHostPort for FakePort {
    async fn snapshot(&self) -> Result<Option<HostLobby>, String> {
        if let Some(error) = self.snapshot_error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(self.lobby.lock().unwrap().clone())
    }
    async fn create(&self, settings: &LobbySettings) -> Result<(), String> {
        self.calls.lock().unwrap().push("create".into());
        *self.lobby.lock().unwrap() = Some(lobby(settings));
        if self.lose_create_reply {
            Err("ambiguous response".into())
        } else {
            Ok(())
        }
    }
    async fn invite(&self, _: u64, account: u32) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("invite:{account}"));
        Ok(())
    }
    async fn move_host_to_pool(&self, _: u64) -> Result<(), String> {
        if let Some(lobby) = self.lobby.lock().unwrap().as_mut()
            && let Some(host) = lobby
                .members
                .iter_mut()
                .find(|member| member.account_id == 99)
        {
            host.side = None;
        }
        Ok(())
    }
    async fn kick_from_team(&self, _: u64, account: u32) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("pool:{account}"));
        if let Some(lobby) = self.lobby.lock().unwrap().as_mut()
            && let Some(member) = lobby
                .members
                .iter_mut()
                .find(|member| member.account_id == account)
        {
            member.side = None;
        }
        Ok(())
    }
    async fn kick(&self, _: u64, account: u32) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("kick:{account}"));
        if let Some(lobby) = self.lobby.lock().unwrap().as_mut() {
            lobby.members.retain(|member| member.account_id != account);
        }
        Ok(())
    }
    async fn launch(&self, _: u64) -> Result<(), String> {
        let pending = PendingMatchRepository::new(&self.path)
            .pending_match(1, self.pending)
            .unwrap()
            .unwrap();
        assert_ne!(
            pending.state.extra.get("dota_betting_closed"),
            Some(&true.into())
        );
        assert!(pending.state.hosted_betting_managed());
        self.calls.lock().unwrap().push("launch".into());
        self.lobby.lock().unwrap().as_mut().unwrap().stage = LobbyStage::Allocating;
        Ok(())
    }
    async fn destroy(&self, _: u64) -> Result<(), String> {
        self.calls.lock().unwrap().push("destroy".into());
        *self.lobby.lock().unwrap() = None;
        Ok(())
    }
    async fn match_details(&self, _: u64) -> Result<HostMatchDetails, String> {
        self.details
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "unavailable".into())
    }
}

struct Recorder {
    count: AtomicUsize,
    failures: AtomicUsize,
    betting_failures: AtomicUsize,
    betting_attempts: AtomicUsize,
    path: PathBuf,
}
#[async_trait]
impl HostedMatchRecorder for Recorder {
    async fn betting_window_changed(&self, _: i64, _: i64) -> Result<(), String> {
        Ok(())
    }
    fn try_acquire_launch_guard(&self, _: i64, _: i64) -> Option<Box<dyn Send>> {
        Some(Box::new(()))
    }
    async fn record(&self, result: HostedMatchResult) -> Result<i64, String> {
        self.count.fetch_add(1, Ordering::SeqCst);
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("temporary settlement failure".into());
        }
        PendingMatchRepository::new(&self.path)
            .delete_pending_match(result.guild_id, result.pending_match_id)
            .unwrap();
        Ok(123)
    }
    async fn betting_closed(&self, _: i64, _: i64) -> Result<(), String> {
        self.betting_attempts.fetch_add(1, Ordering::SeqCst);
        if self
            .betting_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("temporary display failure".into());
        }
        Ok(())
    }
}

fn lobby(settings: &LobbySettings) -> HostLobby {
    HostLobby {
        id: 777,
        name: settings.name.clone(),
        owner_account_id: 99,
        league_id: settings.league_id,
        game_mode: settings.game_mode,
        server_region: settings.server_region,
        first_pick_radiant: settings.first_pick_radiant,
        cheats: false,
        fill_bots: false,
        spectating: true,
        tv_delay: settings.tv_delay,
        visibility: settings.visibility,
        stage: LobbyStage::Gathering,
        game_state: None,
        members: (1..=10)
            .map(|id| LobbySeat {
                account_id: id,
                side: Some(if id <= 5 { Side::Radiant } else { Side::Dire }),
                slot: (id - 1) % 5,
            })
            .collect(),
        match_id: None,
        server_id: None,
        winner: None,
    }
}

struct Fixture {
    _db: tempfile::NamedTempFile,
    worker: DotaHostWorker,
    port: FakePort,
    recorder: Arc<Recorder>,
    pending: i64,
}
impl Fixture {
    fn new(lose_create_reply: bool) -> Self {
        let db = tempfile::NamedTempFile::new().unwrap();
        crate::test_support::initialize_test_database(db.path()).unwrap();
        let config = DotaHostConfig::from_lookup(|key| {
            match key {
                "DOTA_HOST_ENABLED" => Some("true"),
                "DOTA_HOST_GUILD_IDS" => Some("1"),
                "DOTA_STEAM_USERNAME" => Some("bot"),
                "DOTA_BOT_ACCOUNT_ID" => Some("99"),
                "DOTA_HOST_START_AFTER" => Some("1"),
                _ => None,
            }
            .map(str::to_owned)
        })
        .unwrap()
        .unwrap();
        let pending = PendingMatchRepository::new(db.path())
            .create_pending_match(
                1,
                &cama_db::match_runtime::PendingMatchState {
                    radiant_team_ids: (1..=5).collect(),
                    dire_team_ids: (6..=10).collect(),
                    shuffle_timestamp: Some(100),
                    bet_lock_until: Some(1000),
                    ..Default::default()
                },
            )
            .unwrap()
            .pending_match_id;
        let state = SessionState {
            settings: LobbySettings {
                name: format!("Cama 1:{pending}"),
                password: String::new(),
                visibility: 0,
                league_id: 123,
                game_mode: 2,
                server_region: 1,
                first_pick_radiant: Some(true),
                tv_delay: 2,
            },
            roster: (1..=10)
                .map(|id| HostedMatchRosterEntry {
                    discord_id: i64::from(id),
                    steam_account_id: id,
                    radiant: id <= 5,
                })
                .collect(),
            channel_id: None,
            message_id: None,
            last_message: String::new(),
            last_message_at: 0,
            last_invite_at: 0,
            create_requested_at: None,
            launch_requested_at: None,
            betting_closed: false,
            betting_window_announced: false,
            betting_notification_sent: false,
            last_betting_notification_at: 0,
            cancel_requested: false,
            resume_requested: false,
            recorded_match_id: None,
            replay: None,
            archive: None,
            replay_last_attempt: 0,
            replay_error: None,
            last_result_poll: 0,
            lobby_deadline: 0,
            recording_failures: 0,
            server_id: None,
        };
        DotaSessionRepository::new(db.path())
            .claim_session(1, pending, "99", serde_json::to_value(state).unwrap(), 100)
            .unwrap();
        let recorder = Arc::new(Recorder {
            count: AtomicUsize::new(0),
            failures: AtomicUsize::new(0),
            betting_failures: AtomicUsize::new(0),
            betting_attempts: AtomicUsize::new(0),
            path: db.path().into(),
        });
        let live = crate::dota_live::DotaLiveFeed::new(&config).unwrap();
        let worker = DotaHostWorker::new(
            db.path(),
            config,
            recorder.clone(),
            Arc::new(crate::SerenityDiscordTransport::new()),
            live,
        );
        let port = FakePort {
            lobby: Mutex::new(None),
            snapshot_error: Mutex::new(None),
            details: Mutex::new(None),
            calls: Mutex::new(Vec::new()),
            lose_create_reply,
            path: db.path().into(),
            pending,
        };
        Self {
            _db: db,
            worker,
            port,
            recorder,
            pending,
        }
    }
    fn session(&self) -> DotaSessionRecord {
        DotaSessionRepository::new(&self.worker.path)
            .session(1, self.pending)
            .unwrap()
            .unwrap()
    }
    fn update_state(&self, apply: impl FnOnce(&mut DotaSessionRecord, &mut SessionState)) {
        let mut record = self.session();
        let mut state: SessionState = serde_json::from_value(record.payload.clone()).unwrap();
        apply(&mut record, &mut state);
        record.payload = serde_json::to_value(state).unwrap();
        DotaSessionRepository::new(&self.worker.path)
            .update(&record, record.revision, 120)
            .unwrap();
    }
    async fn launch(&self) {
        self.worker.tick(&self.port, 100).await.unwrap();
        self.worker.tick(&self.port, 101).await.unwrap();
        self.worker.tick(&self.port, 111).await.unwrap();
        assert_eq!(self.session().phase, Phase::Launching);
    }
    fn complete(&self, winner: Option<&str>) {
        let mut lobby = self.port.lobby.lock().unwrap();
        let lobby = lobby.as_mut().unwrap();
        lobby.stage = LobbyStage::Postgame;
        lobby.game_state = Some(6);
        lobby.match_id = Some(888);
        lobby.server_id = Some(999);
        *self.port.details.lock().unwrap() = Some(HostMatchDetails {
            match_id: 888,
            league_id: 123,
            winner: winner.map(str::to_owned),
            finished: true,
            players: (1..=10)
                .map(|id| HostedMatchPlayer {
                    account32: id,
                    radiant: id <= 5,
                })
                .collect(),
            replay: ReplayMetadata::NotRecorded,
        });
    }

    fn running(&self, game_state: Option<i32>) {
        let mut lobby = self.port.lobby.lock().unwrap();
        let lobby = lobby.as_mut().unwrap();
        lobby.stage = LobbyStage::Running;
        lobby.game_state = game_state;
        lobby.match_id = Some(888);
        lobby.server_id = Some(999);
        *self.port.details.lock().unwrap() = Some(HostMatchDetails {
            match_id: 888,
            league_id: 123,
            winner: None,
            finished: false,
            players: Vec::new(),
            replay: ReplayMetadata::Pending,
        });
    }

    fn betting_open(&self, now: i64) -> bool {
        PendingMatchRepository::new(&self.worker.path)
            .pending_match(1, self.pending)
            .unwrap()
            .unwrap()
            .state
            .betting_open(now)
    }
}

#[tokio::test]
async fn lost_create_reply_is_reconciled_without_second_lobby() {
    let f = Fixture::new(true);
    assert!(f.worker.tick(&f.port, 100).await.is_err());
    f.worker.tick(&f.port, 101).await.unwrap();
    assert_eq!(f.session().lobby_id.as_deref(), Some("777"));
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|s| *s == "create")
            .count(),
        1
    );
}

#[tokio::test]
async fn complete_workflow_keeps_bets_open_at_launch_and_records_once() {
    let f = Fixture::new(false);
    f.launch().await;
    assert!(f.betting_open(111));
    f.complete(Some("radiant"));
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
    let session = f.session();
    assert_eq!(session.valve_match_id.as_deref(), Some("888"));
    let state: SessionState = serde_json::from_value(session.payload).unwrap();
    assert_eq!(state.server_id, Some(999));
    f.worker.tick(&f.port, 155).await.unwrap();
    assert_eq!(f.session().phase, Phase::Finishing);
    f.worker.tick(&f.port, 160).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
    f.worker.tick(&f.port, 165).await.unwrap();
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn betting_stays_open_through_draft_until_playable_pregame() {
    let f = Fixture::new(false);
    f.launch().await;
    // The old fixed deadline was 1000. Loading, hero selection, strategy,
    // showcase and unknown states must not close a hosted betting window.
    for (index, state) in [
        Some(1),
        Some(2),
        Some(3),
        Some(10),
        Some(8),
        Some(12),
        None,
        Some(99),
    ]
    .into_iter()
    .enumerate()
    {
        let now = 1100 + index as i64 * 30;
        f.running(state);
        f.worker.tick(&f.port, now).await.unwrap();
        assert!(
            f.betting_open(now),
            "state {state:?} closed the draft window"
        );
        assert_eq!(f.recorder.betting_attempts.load(Ordering::SeqCst), 0);
    }
    f.running(Some(4));
    f.worker.tick(&f.port, 1400).await.unwrap();
    assert!(!f.betting_open(1400));
    assert_eq!(f.recorder.betting_attempts.load(Ordering::SeqCst), 1);
    // A stale phase update and a process-style reload cannot reopen betting.
    f.running(Some(3));
    f.worker.tick(&f.port, 1430).await.unwrap();
    assert!(!f.betting_open(1430));
    assert_eq!(f.recorder.betting_attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn admin_extension_survives_gameplay_updates_and_expires_without_the_host() {
    let f = Fixture::new(false);
    f.launch().await;
    let repository = PendingMatchRepository::new(&f.worker.path);
    let extension = repository
        .extend_betting_atomic(1, f.pending, 1100, 600)
        .unwrap();
    f.running(Some(4));
    f.worker.tick(&f.port, 1200).await.unwrap();
    assert!(f.betting_open(1200));
    assert!(
        f.worker
            .match_status_summary(&f.session(), 1200)
            .await
            .unwrap()
            .contains(&format!("open until <t:{}:R>", extension.new_lock_until))
    );
    // Each tick reloads the persisted session, as recovery after restart does.
    for now in [1230, 1260] {
        f.worker.tick(&f.port, now).await.unwrap();
        assert!(f.betting_open(now));
    }
    assert!(f.betting_open(extension.new_lock_until - 1));
    assert!(!f.betting_open(extension.new_lock_until));
    assert_eq!(
        f.worker
            .match_status_summary(&f.session(), extension.new_lock_until)
            .await
            .unwrap(),
        "Betting is closed."
    );
    let reopened = repository
        .extend_betting_atomic(1, f.pending, 1800, 300)
        .unwrap();
    f.worker.tick(&f.port, 1830).await.unwrap();
    assert!(f.betting_open(1830));
    assert!(!f.betting_open(reopened.new_lock_until));
    assert_eq!(f.recorder.betting_attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn spectator_game_state_can_close_betting_without_a_gc_game_phase() {
    let mut f = Fixture::new(false);
    let token = "observer-test-token-at-least-32-bytes";
    f.worker.config.gsi_token = Some(crate::Secret::new(token.into()));
    f.worker.live = crate::dota_live::DotaLiveFeed::new(&f.worker.config).unwrap();
    f.launch().await;
    f.running(None);
    f.worker.tick(&f.port, 150).await.unwrap();
    assert!(f.betting_open(150));
    let sample = |phase: &str| {
        serde_json::json!({
            "auth": {"token": token},
            "map": {"matchid": "888", "game_state": phase},
            "allplayers": {"1": {"steamid": "1"}}
        })
    };
    f.worker
        .live
        .ingest_gsi(sample("DOTA_GAMERULES_STATE_HERO_SELECTION"), 151)
        .unwrap();
    f.worker.tick(&f.port, 155).await.unwrap();
    assert!(f.betting_open(155));
    assert!(f.worker.live.snapshot(1, f.pending).is_none());

    f.worker
        .live
        .ingest_gsi(sample("DOTA_GAMERULES_STATE_PRE_GAME"), 156)
        .unwrap();
    // Observation remains internal until the atomic betting close completes.
    assert!(f.worker.live.snapshot(1, f.pending).is_none());
    *f.port.snapshot_error.lock().unwrap() = Some("GC offline".into());
    assert_eq!(f.worker.tick(&f.port, 160).await.unwrap_err(), "GC offline");
    assert!(!f.betting_open(160));
    assert!(f.worker.live.snapshot(1, f.pending).is_some());
}

#[tokio::test]
async fn gameplay_confirmation_closes_betting_even_when_gc_lobby_stage_lags() {
    for stage in [LobbyStage::Gathering, LobbyStage::Allocating] {
        for spectator in [false, true] {
            let mut f = Fixture::new(false);
            let token = "observer-test-token-at-least-32-bytes";
            f.worker.config.gsi_token = Some(crate::Secret::new(token.into()));
            f.worker.live = crate::dota_live::DotaLiveFeed::new(&f.worker.config).unwrap();
            f.launch().await;
            f.running(None);
            f.port.lobby.lock().unwrap().as_mut().unwrap().stage = stage;
            f.worker.tick(&f.port, 150).await.unwrap();
            assert!(f.betting_open(150));

            if spectator {
                f.worker.live.ingest_gsi(serde_json::json!({
                    "auth": {"token": token},
                    "map": {"matchid": "888", "game_state": "DOTA_GAMERULES_STATE_PRE_GAME"},
                    "allplayers": {"1": {"steamid": "1"}}
                }), 151).unwrap();
                assert!(f.worker.live.snapshot(1, f.pending).is_none());
            } else {
                f.port.lobby.lock().unwrap().as_mut().unwrap().game_state = Some(4);
            }
            f.worker.tick(&f.port, 155).await.unwrap();
            assert!(
                !f.betting_open(155),
                "stage {stage:?}, spectator {spectator}"
            );
            assert_eq!(f.recorder.betting_attempts.load(Ordering::SeqCst), 1);
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
    }
}

#[tokio::test]
async fn prelaunch_cancellation_restores_the_original_betting_deadline() {
    let f = Fixture::new(false);
    f.worker.tick(&f.port, 100).await.unwrap();
    assert!(f.betting_open(1100));
    f.update_state(|_, state| state.cancel_requested = true);
    f.worker.tick(&f.port, 121).await.unwrap();
    f.worker.tick(&f.port, 122).await.unwrap();
    assert_eq!(f.session().phase, Phase::Cancelled);
    assert!(f.betting_open(999));
    assert!(!f.betting_open(1000));
}

#[tokio::test]
async fn eligible_queued_game_keeps_betting_open_while_the_host_is_busy() {
    let f = Fixture::new(false);
    cama_domain::guild_config::GuildConfigStore::set_league_id(
        &GuildConfigRepository::new(&f.worker.path, false),
        1,
        123,
    )
    .unwrap();
    let links = OpenDotaPlayerRepository::new(&f.worker.path);
    for account in 11..=20 {
        links.add_steam_id(account, account, true, 100).unwrap();
    }
    let pending_repo = PendingMatchRepository::new(&f.worker.path);
    let queued = pending_repo
        .create_pending_match(
            1,
            &cama_db::match_runtime::PendingMatchState {
                radiant_team_ids: (11..=15).collect(),
                dire_team_ids: (16..=20).collect(),
                shuffle_timestamp: Some(101),
                bet_lock_until: Some(200),
                ..Default::default()
            },
        )
        .unwrap();
    f.worker.tick(&f.port, 100).await.unwrap();
    let queued = pending_repo
        .pending_match(1, queued.pending_match_id)
        .unwrap()
        .unwrap();
    assert!(queued.state.hosted_betting_managed());
    assert!(queued.state.betting_open(5000));
    assert!(
        DotaSessionRepository::new(&f.worker.path)
            .session(1, queued.pending_match_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| *call == "create")
            .count(),
        1
    );
}

#[tokio::test]
async fn gameplay_still_closes_betting_while_hosting_needs_operator_review() {
    let f = Fixture::new(false);
    f.launch().await;
    f.running(Some(2));
    f.worker.tick(&f.port, 150).await.unwrap();
    assert!(f.betting_open(150));
    f.update_state(|record, _| record.phase = Phase::NeedsReview);
    f.running(Some(4));
    f.worker.tick(&f.port, 160).await.unwrap();
    assert!(!f.betting_open(160));
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn restart_restores_live_match_from_saved_ids_without_a_lobby() {
    let f = Fixture::new(false);
    f.launch().await;
    {
        let mut lobby = f.port.lobby.lock().unwrap();
        let lobby = lobby.as_mut().unwrap();
        lobby.stage = LobbyStage::Running;
        lobby.game_state = Some(4);
        lobby.match_id = Some(888);
        lobby.server_id = Some(999);
    }
    assert_eq!(
        f.worker.tick(&f.port, 150).await.unwrap_err(),
        "unavailable"
    );
    assert_eq!(f.session().phase, Phase::Running);

    // A new process has no live cache. Its GC may no longer have the lobby,
    // while Valve has not published completed match details yet.
    *f.port.lobby.lock().unwrap() = None;
    let mut config = f.worker.config.clone();
    config.gsi_token = Some(crate::Secret::new(
        "restart-test-token-at-least-32-bytes".into(),
    ));
    let live = crate::dota_live::DotaLiveFeed::new(&config).unwrap();
    let restarted = DotaHostWorker::new(
        &f.worker.path,
        config,
        f.recorder.clone(),
        Arc::new(crate::SerenityDiscordTransport::new()),
        live.clone(),
    );
    assert_eq!(
        restarted.tick(&f.port, 180).await.unwrap_err(),
        "unavailable"
    );
    live.ingest_gsi(
        serde_json::json!({
            "auth": {"token": "restart-test-token-at-least-32-bytes"},
            "map": {"matchid": "888", "game_time": 120},
            "allplayers": {"1": {"steamid": "1", "kills": 2}}
        }),
        180,
    )
    .unwrap();
    let snapshot = live.snapshot(1, f.pending).unwrap();
    assert_eq!(snapshot.match_id, 888);
    assert_eq!(snapshot.game_time_seconds, Some(120));
    // The session saved enum 2 (120 seconds), even though the new deployment
    // configuration defaults to enum 3 (300 seconds).
    assert_eq!(snapshot.delay_seconds, Some(120));
    let state: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert_eq!(state.server_id, Some(999));
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn public_lobby_keeps_spectators_and_unassigned_outsiders_but_cleans_teams() {
    let f = Fixture::new(false);
    // The first tick creates the public lobby from the persisted settings.
    f.worker.tick(&f.port, 100).await.unwrap();
    {
        let mut lobby = f.port.lobby.lock().unwrap();
        let lobby = lobby.as_mut().unwrap();
        assert_eq!(lobby.visibility, 0);
        // Side=None covers both an actual spectator and a player sitting in
        // the unassigned pool in the GC snapshot.
        lobby.members.push(LobbySeat {
            account_id: 20,
            side: None,
            slot: 10,
        });
        lobby.members.push(LobbySeat {
            account_id: 21,
            side: None,
            slot: 11,
        });
        // Player 1 is shuffled to Radiant but has selected Dire. Account 22
        // is an outsider occupying a playable team seat.
        lobby
            .members
            .iter_mut()
            .find(|member| member.account_id == 1)
            .unwrap()
            .side = Some(Side::Dire);
        lobby.members.push(LobbySeat {
            account_id: 22,
            side: Some(Side::Radiant),
            slot: 0,
        });
    }

    // The host kicks the outsider from the lobby and returns the wrong-side
    // shuffled player to the pool. It must wait for the player to reseat.
    f.worker.tick(&f.port, 101).await.unwrap();
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "launch")
    );
    assert!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "kick:22")
    );
    assert!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "pool:1")
    );
    {
        let lobby = f.port.lobby.lock().unwrap();
        let lobby = lobby.as_ref().unwrap();
        assert!(!lobby.members.iter().any(|member| member.account_id == 22));
        assert!(
            lobby
                .members
                .iter()
                .any(|member| member.account_id == 20 && member.side.is_none())
        );
        assert!(
            lobby
                .members
                .iter()
                .any(|member| member.account_id == 21 && member.side.is_none())
        );
        assert!(
            lobby
                .members
                .iter()
                .any(|member| member.account_id == 1 && member.side.is_none())
        );
    }

    // Once the shuffled player selects the assigned side, the ten correct
    // seats launch on this next tick; no stability delay is required.
    f.port
        .lobby
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .members
        .iter_mut()
        .find(|member| member.account_id == 1)
        .unwrap()
        .side = Some(Side::Radiant);
    f.worker.tick(&f.port, 102).await.unwrap();
    assert_eq!(f.session().phase, Phase::Launching);
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| *call == "launch")
            .count(),
        1
    );
    let lobby = f.port.lobby.lock().unwrap();
    let lobby = lobby.as_ref().unwrap();
    assert_eq!(lobby.stage, LobbyStage::Allocating);
    assert!(
        lobby
            .members
            .iter()
            .any(|member| member.account_id == 20 && member.side.is_none())
    );
    assert!(
        lobby
            .members
            .iter()
            .any(|member| member.account_id == 21 && member.side.is_none())
    );
}

#[tokio::test]
async fn discovery_persists_public_lobby_settings() {
    let f = Fixture::new(false);
    // Fixture::new normally preclaims a session so individual lifecycle
    // tests can start at a known phase. Remove that row and seed the same
    // prerequisites the production discovery path reads.
    rusqlite::Connection::open(&f.worker.path)
        .unwrap()
        .execute(
            "DELETE FROM dota_sessions WHERE guild_id=?1 AND pending_match_id=?2",
            rusqlite::params![1, f.pending],
        )
        .unwrap();
    cama_domain::guild_config::GuildConfigStore::set_league_id(
        &GuildConfigRepository::new(&f.worker.path, false),
        1,
        123,
    )
    .unwrap();
    let links = OpenDotaPlayerRepository::new(&f.worker.path);
    for account in 1..=10 {
        links.add_steam_id(account, account, true, 100).unwrap();
    }

    f.worker.tick(&f.port, 100).await.unwrap();
    let state: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert_eq!(state.settings.password, "");
    assert_eq!(state.settings.visibility, 0);
    assert_eq!(state.settings.server_region, 31);
    let lobby = f.port.lobby.lock().unwrap();
    let lobby = lobby.as_ref().unwrap();
    assert_eq!(lobby.visibility, 0);
    assert_eq!(lobby.server_region, 31);
}

#[tokio::test]
async fn missing_winner_never_becomes_a_dire_win() {
    let f = Fixture::new(false);
    f.launch().await;
    f.complete(None);
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn verified_postgame_lobby_records_when_match_details_are_delayed() {
    let f = Fixture::new(false);
    f.launch().await;
    f.complete(Some("radiant"));
    *f.port.details.lock().unwrap() = None;
    f.port.lobby.lock().unwrap().as_mut().unwrap().winner = Some("radiant".to_owned());
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn disappeared_lobby_after_launch_is_not_recreated() {
    let f = Fixture::new(false);
    f.launch().await;
    *f.port.lobby.lock().unwrap() = None;
    f.worker.tick(&f.port, 120).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|s| *s == "create")
            .count(),
        1
    );
}

#[tokio::test]
async fn mismatched_league_or_roster_never_launches() {
    let f = Fixture::new(false);
    f.worker.tick(&f.port, 100).await.unwrap();
    f.port.lobby.lock().unwrap().as_mut().unwrap().league_id = 999;
    f.worker.tick(&f.port, 101).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(!f.port.calls.lock().unwrap().iter().any(|s| s == "launch"));
}

#[tokio::test]
async fn delayed_create_cache_never_authorizes_a_second_request() {
    let f = Fixture::new(true);
    assert!(f.worker.tick(&f.port, 100).await.is_err());
    *f.port.lobby.lock().unwrap() = None;
    f.worker.tick(&f.port, 110).await.unwrap();
    f.worker.tick(&f.port, 191).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|s| *s == "create")
            .count(),
        1
    );
}

#[tokio::test]
async fn cancel_preserves_launch_intent_and_unrecorded_postgame_evidence() {
    for postgame in [false, true] {
        let f = Fixture::new(false);
        f.launch().await;
        if postgame {
            f.complete(Some("radiant"));
        } else {
            // A lost launch acknowledgement can leave the lobby looking
            // unstarted until its next GC update arrives.
            f.port.lobby.lock().unwrap().as_mut().unwrap().stage = LobbyStage::Gathering;
        }
        f.update_state(|_, state| state.cancel_requested = true);
        f.worker.tick(&f.port, 150).await.unwrap();
        assert_eq!(f.session().phase, Phase::NeedsReview);
        assert!(!f.port.calls.lock().unwrap().iter().any(|s| s == "destroy"));
        assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn cancel_cannot_release_a_disappeared_started_match() {
    let f = Fixture::new(false);
    f.launch().await;
    f.update_state(|record, state| {
        record.phase = Phase::Running;
        record.valve_match_id = Some("888".into());
        state.cancel_requested = true;
    });
    *f.port.lobby.lock().unwrap() = None;
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
}

#[tokio::test]
async fn resume_clears_an_old_cancel_request() {
    let f = Fixture::new(false);
    f.worker.tick(&f.port, 100).await.unwrap();
    f.update_state(|record, state| {
        record.phase = Phase::NeedsReview;
        state.cancel_requested = true;
        state.resume_requested = true;
    });
    f.worker.tick(&f.port, 150).await.unwrap();
    assert!(!f.port.calls.lock().unwrap().iter().any(|s| s == "destroy"));
    let state: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert!(!state.cancel_requested);
}

#[tokio::test]
async fn unfinished_details_after_postgame_wait_for_the_verified_result() {
    let f = Fixture::new(false);
    f.launch().await;
    f.complete(None);
    {
        let mut details = f.port.details.lock().unwrap();
        let details = details.as_mut().unwrap();
        details.finished = false;
        details.players.clear();
    }
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.session().phase, Phase::Finishing);
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    f.complete(Some("dire"));
    f.worker.tick(&f.port, 181).await.unwrap();
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn transient_settlement_and_display_failures_retry_after_restart() {
    let f = Fixture::new(false);
    f.recorder.betting_failures.store(1, Ordering::SeqCst);
    f.recorder.failures.store(1, Ordering::SeqCst);
    f.launch().await;
    f.complete(Some("radiant"));
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.session().phase, Phase::Finishing);
    let state: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert!(!state.betting_notification_sent);
    assert_eq!(f.recorder.betting_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(state.recording_failures, 1);
    // Every tick reloads durable state, as a restarted worker would.
    f.worker.tick(&f.port, 160).await.unwrap();
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
    f.worker.tick(&f.port, 181).await.unwrap();
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 2);
    assert_eq!(f.recorder.betting_attempts.load(Ordering::SeqCst), 2);
    assert!(
        serde_json::from_value::<SessionState>(f.session().payload)
            .unwrap()
            .recorded_match_id
            .is_some()
    );
}

#[tokio::test]
async fn manual_abort_after_launch_preserves_the_game_for_review() {
    let f = Fixture::new(false);
    f.launch().await;
    PendingMatchRepository::new(&f.worker.path)
        .delete_pending_match(1, f.pending)
        .unwrap();
    f.complete(Some("radiant"));
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    assert!(!f.port.calls.lock().unwrap().iter().any(|s| s == "destroy"));
}

#[tokio::test]
async fn lost_launch_is_reviewed_even_if_a_player_leaves_before_allocation() {
    let f = Fixture::new(false);
    f.launch().await;
    {
        let mut lobby = f.port.lobby.lock().unwrap();
        let lobby = lobby.as_mut().unwrap();
        lobby.stage = LobbyStage::Gathering;
        lobby.members.pop();
    }
    f.worker.tick(&f.port, 202).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert_eq!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|s| *s == "launch")
            .count(),
        1
    );
}
