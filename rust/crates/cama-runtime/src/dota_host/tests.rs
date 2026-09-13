use super::*;
mod configuration_tests;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn steam_lobby_slots_allow_any_order_on_the_correct_team_without_kicks() {
    let f = Fixture::new(false);
    f.worker.tick(&f.port, 100).await.unwrap();
    {
        let mut lobby = f.port.lobby.lock().unwrap();
        for member in &mut lobby.as_mut().unwrap().members {
            // Reverse the shuffle order using real GC slot numbers, including
            // slot 5 on each side, which previously triggered a team kick.
            *member = steam::lobby_seat(cama_steam::LobbyMember {
                steam_id: dota_lobby::STEAM_INDIVIDUAL_BASE + u64::from(member.account_id),
                account_id: member.account_id,
                hero_id: 0,
                team: if member.side == Some(Side::Radiant) {
                    cama_steam::Team::Radiant
                } else {
                    cama_steam::Team::Dire
                },
                name: String::new(),
                slot: 5 - (member.account_id - 1) % 5,
                party_id: None,
                coach_team: None,
            });
        }
    }
    f.worker.tick(&f.port, 101).await.unwrap();
    f.worker.tick(&f.port, 111).await.unwrap();
    assert_eq!(f.session().phase, Phase::Launching);
    let calls = f.port.calls.lock().unwrap();
    assert!(calls.iter().any(|call| call == "launch"));
    assert!(
        !calls
            .iter()
            .any(|call| call.starts_with("pool:") || call.starts_with("kick:"))
    );
}

struct FakePort {
    configure_behavior: Mutex<&'static str>,
    lobby: Mutex<Option<HostLobby>>,
    snapshot_error: Mutex<Option<String>>,
    observation_stale: std::sync::atomic::AtomicBool,
    snapshot_calls: AtomicUsize,
    fail_snapshot_at: AtomicUsize,
    details: Mutex<Option<HostMatchDetails>>,
    calls: Mutex<Vec<String>>,
    lose_create_reply: bool,
    path: PathBuf,
    pending: i64,
}

#[async_trait]
impl DotaHostPort for FakePort {
    async fn betting_observation_fresh(&self) -> bool {
        !self.observation_stale.load(Ordering::SeqCst)
    }
    async fn snapshot(&self) -> Result<Option<HostLobby>, String> {
        let call = self.snapshot_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_snapshot_at.load(Ordering::SeqCst) == call {
            return Err("final snapshot failed".into());
        }
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
    async fn configure(&self, lobby_id: u64, settings: &LobbySettings) -> Result<(), String> {
        self.calls.lock().unwrap().push("configure".into());
        let behavior = *self.configure_behavior.lock().unwrap();
        if behavior == "ignore" {
            return Ok(());
        }
        let mut current = self.lobby.lock().unwrap();
        let current = current.as_mut().unwrap();
        assert_eq!(current.id, lobby_id);
        current.server_region = settings.server_region;
        current.game_mode = settings.game_mode;
        current.first_pick_radiant = settings.first_pick_radiant;
        current.tv_delay = settings.tv_delay;
        current.league_id = settings.league_id;
        current.visibility = settings.visibility;
        if behavior == "lost_reply" {
            return Err("configuration reply lost".into());
        }
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
    async fn match_details(&self, match_id: u64) -> Result<HostMatchDetails, String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("details:{match_id}"));
        self.details
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "unavailable".into())
    }
}

struct Recorder {
    statistics: Mutex<Option<serde_json::Value>>,
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
        *self.statistics.lock().unwrap() = result.postgame_statistics.clone();
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
        Self::new_at(lose_create_reply, 100)
    }

    fn new_at(lose_create_reply: bool, created_at: i64) -> Self {
        let db = tempfile::NamedTempFile::new().unwrap();
        crate::test_support::initialize_test_database(db.path()).unwrap();
        let config = DotaHostConfig::from_lookup(|key| {
            match key {
                "DOTA_HOST_ENABLED" => Some("true"),
                "DOTA_HOST_GUILD_IDS" => Some("1"),
                "DOTA_STEAM_USERNAME" => Some("bot"),
                "DOTA_BOT_ACCOUNT_ID" => Some("99"),
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
            configuration: None,
            last_configuration: None,
            test_mode: DotaHostTestMode::Off,
            fake_roster: Vec::new(),
            simulated_winner: None,
            start_mode: StartMode::Automatic,
            manual_start_requested: false,
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
            pending_status: None,
            resolution: None,
            resolution_history: Vec::new(),
            betting_control_audit: Vec::new(),
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
            postgame_statistics: None,
            archive: None,
            replay_last_attempt: 0,
            replay_error: None,
            last_result_poll: 0,
            lobby_deadline: 0,
            recording_failures: 0,
            server_id: None,
        };
        DotaSessionRepository::new(db.path())
            .claim_session(
                1,
                pending,
                "99",
                serde_json::to_value(state).unwrap(),
                created_at,
            )
            .unwrap();
        let recorder = Arc::new(Recorder {
            statistics: Mutex::new(None),
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
            configure_behavior: Mutex::new("normal"),
            lobby: Mutex::new(None),
            snapshot_error: Mutex::new(None),
            observation_stale: std::sync::atomic::AtomicBool::new(false),
            snapshot_calls: AtomicUsize::new(0),
            fail_snapshot_at: AtomicUsize::new(0),
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
            postgame_statistics: None,
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
            postgame_statistics: None,
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

    fn preview(mode: DotaHostTestMode, link_real: bool) -> Self {
        let mut f = Self::new(false);
        f.worker.config.test_mode = mode;
        PendingMatchRepository::new(&f.worker.path)
            .configure_dota_host_routing(Some(cama_db::match_runtime::DotaHostRouting {
                account_key: "99".into(),
                guild_ids: vec![1],
            }))
            .unwrap();
        rusqlite::Connection::open(&f.worker.path)
            .unwrap()
            .execute(
                "DELETE FROM dota_sessions WHERE guild_id=1 AND pending_match_id=?1",
                [f.pending],
            )
            .unwrap();
        let repo = PendingMatchRepository::new(&f.worker.path);
        let mut pending = repo.pending_match(1, f.pending).unwrap().unwrap();
        // Internal fake-port tests seed a reservation explicitly; production
        // creation rejects fake players before reserving the real account.
        pending
            .state
            .extra
            .insert("dota_host_account_key".into(), "99".into());
        pending.state.radiant_team_ids = vec![1, -1, -2, -3, -4];
        pending.state.dire_team_ids = vec![-5, -6, -7, -8, -9];
        repo.update_pending_match(1, f.pending, &pending.state)
            .unwrap();
        GuildConfigRepository::new(&f.worker.path, false)
            .set_league_id(1, 123)
            .unwrap();
        let players = cama_db::core_repositories::PlayerRepository::new(&f.worker.path);
        for id in -9..=-1 {
            players
                .add(&cama_db::core_repositories::NewPlayer::new(
                    id,
                    format!("FakeUser{}", -id),
                    Some(1),
                ))
                .unwrap();
        }
        let links = OpenDotaPlayerRepository::new(&f.worker.path);
        if link_real {
            links.add_steam_id(1, 1, true, 100).unwrap();
        }
        // Even an old link attached to a fake placeholder must never cause
        // a real invitation to that Steam account.
        links.add_steam_id(-1, 42, true, 100).unwrap();
        f
    }
}

#[tokio::test]
async fn latest_operator_intent_wins_when_cancel_follows_resume() {
    for local_cli in [false, true] {
        let f = Fixture::new(false);
        f.update_state(|record, state| {
            record.phase = Phase::NeedsReview;
            state.resume_requested = true;
            state.manual_start_requested = true;
        });
        if local_cli {
            operator_command(f.worker.path.clone(), "cancel", Some(1), Some(f.pending))
                .await
                .unwrap();
        } else {
            guild_operator_command(f.worker.path.clone(), "cancel", 1, Some(f.pending))
                .await
                .unwrap();
        }
        let state: SessionState = serde_json::from_value(f.session().payload).unwrap();
        assert!(state.cancel_requested);
        assert!(!state.resume_requested);
        assert!(!state.manual_start_requested);
        f.worker.tick(&f.port, 125).await.unwrap();
        assert_eq!(f.session().phase, Phase::Cancelled);
        assert!(
            !f.port
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call == "launch")
        );
    }
}

#[tokio::test]
async fn manual_hosting_snapshot_skips_discovery_and_steam_entirely() {
    let f = Fixture::preview(DotaHostTestMode::RealLobby, true);
    let repo = PendingMatchRepository::new(&f.worker.path);
    let mut pending = repo.pending_match(1, f.pending).unwrap().unwrap();
    pending.state.extra.insert(
        "dota_hosting".into(),
        serde_json::json!({"hosting":"manual"}),
    );
    repo.update_pending_match(1, f.pending, &pending.state)
        .unwrap();
    f.worker.tick(&f.port, 100).await.unwrap();
    assert!(f.port.calls.lock().unwrap().is_empty());
    assert!(
        DotaSessionRepository::new(&f.worker.path)
            .session(1, f.pending)
            .unwrap()
            .is_none()
    );
    assert!(
        !repo
            .pending_match(1, f.pending)
            .unwrap()
            .unwrap()
            .state
            .hosted_betting_managed()
    );
}

#[tokio::test]
async fn hosting_options_are_frozen_and_override_environment_defaults() {
    let f = Fixture::preview(DotaHostTestMode::Simulated, true);
    let repo = PendingMatchRepository::new(&f.worker.path);
    let mut pending = repo.pending_match(1, f.pending).unwrap().unwrap();
    pending.state.extra.insert("dota_hosting".into(), serde_json::json!({"region":3,"game_mode":23,"tv_delay":0,"league_id":456,"first_pick":"dire","start":"manual"}));
    repo.update_pending_match(1, f.pending, &pending.state)
        .unwrap();
    GuildConfigRepository::new(&f.worker.path, false)
        .update_dota_hosting_options(
            1,
            &DotaHostingOptions {
                region: Some(31),
                hosting: Some(HostingMode::Manual),
                ..Default::default()
            },
        )
        .unwrap();
    f.worker.tick(&f.port, 100).await.unwrap();
    let saved: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert_eq!(saved.settings.server_region, 3);
    assert_eq!(saved.settings.game_mode, 23);
    assert_eq!(saved.settings.tv_delay, 0);
    assert_eq!(saved.settings.league_id, 456);
    assert_eq!(saved.settings.first_pick_radiant, Some(false));
    assert_eq!(saved.start_mode, StartMode::Manual);
}

#[tokio::test]
async fn manual_start_waits_for_admin_and_does_not_bypass_roster_checks() {
    let f = Fixture::new(false);
    f.update_state(|_, state| state.start_mode = StartMode::Manual);
    for now in [100, 101, 111] {
        f.worker.tick(&f.port, now).await.unwrap();
    }
    assert_eq!(f.session().phase, Phase::Gathering);
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "launch")
    );
    assert!(
        guild_operator_command(f.worker.path.clone(), "start", 2, Some(f.pending))
            .await
            .is_err()
    );
    guild_operator_command(f.worker.path.clone(), "start", 1, Some(f.pending))
        .await
        .unwrap();
    f.port.lobby.lock().unwrap().as_mut().unwrap().members[0].side = Some(Side::Dire);
    f.worker.tick(&f.port, 120).await.unwrap();
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "launch")
    );
    f.port.lobby.lock().unwrap().as_mut().unwrap().members[0].side = Some(Side::Radiant);
    f.worker.tick(&f.port, 125).await.unwrap();
    assert!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "launch")
    );
}

#[tokio::test]
async fn manual_handoff_cleans_owned_preview_but_preserves_pending_match() {
    let f = Fixture::preview(DotaHostTestMode::RealLobby, true);
    f.worker.tick(&f.port, 100).await.unwrap();
    f.worker.tick(&f.port, 105).await.unwrap();
    assert!(
        guild_operator_command(f.worker.path.clone(), "start", 1, Some(f.pending))
            .await
            .is_err()
    );
    guild_operator_command(f.worker.path.clone(), "manual", 1, Some(f.pending))
        .await
        .unwrap();
    f.worker.tick(&f.port, 110).await.unwrap();
    f.worker.tick(&f.port, 115).await.unwrap();
    assert_eq!(f.session().phase, Phase::Cancelled);
    assert!(f.port.lobby.lock().unwrap().is_none());
    let pending = PendingMatchRepository::new(&f.worker.path)
        .pending_match(1, f.pending)
        .unwrap()
        .unwrap();
    assert_eq!(
        DotaHostingOptions::from_extra(&pending.state.extra)
            .unwrap()
            .hosting,
        Some(HostingMode::Manual)
    );
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn hosted_admin_status_is_guild_scoped_and_handoff_rejects_started_games() {
    let f = Fixture::new(false);
    assert!(
        guild_operator_command(f.worker.path.clone(), "status", 2, None)
            .await
            .unwrap()
            .contains("No Dota")
    );
    f.launch().await;
    assert!(
        guild_operator_command(f.worker.path.clone(), "manual", 1, Some(f.pending))
            .await
            .is_err()
    );
    assert!(
        !serde_json::from_value::<SessionState>(f.session().payload)
            .unwrap()
            .cancel_requested
    );
}

#[tokio::test(start_paused = true)]
async fn simulation_obeys_manual_start_before_running_stages() {
    let f = Fixture::preview(DotaHostTestMode::Simulated, true);
    let repo = PendingMatchRepository::new(&f.worker.path);
    let mut pending = repo.pending_match(1, f.pending).unwrap().unwrap();
    pending
        .state
        .extra
        .insert("dota_hosting".into(), serde_json::json!({"start":"manual"}));
    repo.update_pending_match(1, f.pending, &pending.state)
        .unwrap();
    let port = simulated::connect(&f.worker.config);
    f.worker.tick(port.as_ref(), 100).await.unwrap();
    f.worker.tick(port.as_ref(), 105).await.unwrap();
    tokio::time::advance(Duration::from_secs(3)).await;
    f.worker.tick(port.as_ref(), 110).await.unwrap();
    assert_eq!(f.session().phase, Phase::Gathering);
    guild_operator_command(f.worker.path.clone(), "start", 1, None)
        .await
        .unwrap();
    f.worker.tick(port.as_ref(), 115).await.unwrap();
    assert_eq!(f.session().phase, Phase::Launching);
}

#[tokio::test]
async fn real_lobby_preview_invites_only_real_player_and_never_launches_or_settles() {
    let f = Fixture::preview(DotaHostTestMode::RealLobby, true);
    f.worker.tick(&f.port, 100).await.unwrap();
    let saved: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert_eq!(saved.test_mode, DotaHostTestMode::RealLobby);
    assert_eq!(saved.roster.len(), 1);
    assert_eq!(saved.fake_roster.len(), 9);
    assert_eq!(saved.settings.visibility, 2);
    assert_eq!(saved.settings.league_id, 123);
    assert_eq!(saved.settings.server_region, 27);
    f.port.lobby.lock().unwrap().as_mut().unwrap().members = vec![LobbySeat {
        account_id: 99,
        side: Some(Side::Radiant),
        slot: 0,
    }];
    f.worker.tick(&f.port, 101).await.unwrap();
    assert!(
        f.port
            .calls
            .lock()
            .unwrap()
            .contains(&"invite:1".to_owned())
    );
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .contains(&"invite:42".to_owned())
    );
    assert!(
        f.port.lobby.lock().unwrap().as_ref().unwrap().members[0]
            .side
            .is_none()
    );
    // Even ten observed playing accounts must not make a preview launch.
    f.port.lobby.lock().unwrap().as_mut().unwrap().members = lobby(&saved.settings).members;
    for now in [110, 120, 180] {
        f.worker.tick(&f.port, now).await.unwrap();
    }
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "launch" || c.starts_with("details:"))
    );
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    let pending = PendingMatchRepository::new(&f.worker.path)
        .pending_match(1, f.pending)
        .unwrap()
        .unwrap();
    assert!(!pending.state.hosted_betting_managed());
    f.update_state(|_, state| state.cancel_requested = true);
    f.worker.tick(&f.port, 190).await.unwrap();
    f.worker.tick(&f.port, 195).await.unwrap();
    assert!(f.port.lobby.lock().unwrap().is_none());
    assert_eq!(f.session().phase, Phase::Cancelled);
    assert!(
        PendingMatchRepository::new(&f.worker.path)
            .pending_match(1, f.pending)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn real_lobby_preview_returns_wrong_side_player_to_pool_and_preserves_correct_side() {
    let f = Fixture::preview(DotaHostTestMode::RealLobby, true);
    f.worker.tick(&f.port, 100).await.unwrap();
    f.port.lobby.lock().unwrap().as_mut().unwrap().members = vec![LobbySeat {
        account_id: 1,
        side: Some(Side::Dire),
        slot: 1,
    }];
    f.worker.tick(&f.port, 101).await.unwrap();
    assert!(f.port.calls.lock().unwrap().contains(&"pool:1".to_owned()));
    assert!(
        f.port.lobby.lock().unwrap().as_ref().unwrap().members[0]
            .side
            .is_none()
    );
    f.port.calls.lock().unwrap().clear();
    // The unassigned player is left in the pool until they choose their side.
    f.worker.tick(&f.port, 110).await.unwrap();
    assert!(f.port.calls.lock().unwrap().is_empty());
    for slot in [1, 5] {
        {
            let mut lobby = f.port.lobby.lock().unwrap();
            let player = &mut lobby.as_mut().unwrap().members[0];
            player.side = Some(Side::Radiant);
            player.slot = slot;
        }
        f.worker.tick(&f.port, 120 + i64::from(slot)).await.unwrap();
        assert!(f.port.calls.lock().unwrap().is_empty());
        assert_eq!(
            f.port.lobby.lock().unwrap().as_ref().unwrap().members[0].side,
            Some(Side::Radiant)
        );
    }
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_discovery_rejects_unlinked_real_players_unregistered_fakes_and_disabled_mode() {
    for (mode, link_real, remove_fake) in [
        (DotaHostTestMode::Off, true, false),
        (DotaHostTestMode::RealLobby, false, false),
        (DotaHostTestMode::RealLobby, true, true),
    ] {
        let f = Fixture::preview(mode, link_real);
        if remove_fake {
            let connection = rusqlite::Connection::open(&f.worker.path).unwrap();
            connection
                .pragma_update(None, "foreign_keys", false)
                .unwrap();
            connection
                .execute("DELETE FROM players WHERE discord_id=-9 AND guild_id=1", [])
                .unwrap();
        }
        f.worker.tick(&f.port, 100).await.unwrap();
        assert!(
            DotaSessionRepository::new(&f.worker.path)
                .session(1, f.pending)
                .unwrap()
                .is_none()
        );
        assert!(f.port.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn historical_preview_is_quarantined_before_production_steam_actions() {
    let mut f = Fixture::preview(DotaHostTestMode::RealLobby, true);
    f.worker.tick(&f.port, 100).await.unwrap();
    f.worker.config.test_mode = DotaHostTestMode::Off;
    f.port.calls.lock().unwrap().clear();
    f.worker.tick(&f.port, 110).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(f.port.calls.lock().unwrap().is_empty());
    f.update_state(|_, state| state.cancel_requested = true);
    f.worker.tick(&f.port, 120).await.unwrap();
    f.worker.tick(&f.port, 125).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(f.port.calls.lock().unwrap().is_empty());
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn real_preview_never_records_or_destroys_an_externally_started_game() {
    let f = Fixture::preview(DotaHostTestMode::RealLobby, true);
    f.worker.tick(&f.port, 100).await.unwrap();
    {
        let mut current = f.port.lobby.lock().unwrap();
        let lobby = current.as_mut().unwrap();
        lobby.stage = LobbyStage::Postgame;
        lobby.match_id = Some(888);
        lobby.winner = Some("radiant".to_owned());
    }
    f.worker.tick(&f.port, 110).await.unwrap();
    f.update_state(|_, state| state.cancel_requested = true);
    f.worker.tick(&f.port, 120).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c == "destroy" || c == "launch" || c.starts_with("details:"))
    );
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn enabling_test_mode_cannot_adopt_an_existing_production_session() {
    let mut f = Fixture::new(false);
    // Older payloads omit the new discriminator and remain production-only.
    let mut record = f.session();
    record.payload.as_object_mut().unwrap().remove("test_mode");
    DotaSessionRepository::new(&f.worker.path)
        .update(&record, record.revision, 101)
        .unwrap();
    f.worker.config.test_mode = DotaHostTestMode::RealLobby;
    f.worker.tick(&f.port, 110).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(f.port.calls.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn simulated_worker_runs_fake_roster_to_result_without_recording_or_betting_changes() {
    let mut f = Fixture::preview(DotaHostTestMode::Simulated, true);
    let discord = Arc::new(TestDiscord::default());
    f.worker.discord = discord.clone();
    let port = simulated::connect(&f.worker.config);
    let pending = PendingMatchRepository::new(&f.worker.path)
        .pending_match(1, f.pending)
        .unwrap()
        .unwrap();
    f.worker.tick(port.as_ref(), 100).await.unwrap();
    f.update_state(|_, state| state.channel_id = Some(999));
    f.worker.tick(port.as_ref(), 105).await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    f.worker.tick(port.as_ref(), 110).await.unwrap();
    assert_eq!(f.session().phase, Phase::Launching);
    for (seconds, now) in [(5, 115), (20, 135), (20, 155), (20, 175)] {
        tokio::time::advance(Duration::from_secs(seconds)).await;
        f.worker.tick(port.as_ref(), now).await.unwrap();
    }
    let saved: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert_eq!(saved.simulated_winner.as_deref(), Some("radiant"));
    assert!(saved.cancel_requested);
    f.worker.tick(port.as_ref(), 180).await.unwrap();
    f.worker.tick(port.as_ref(), 185).await.unwrap();
    assert_eq!(f.session().phase, Phase::Cancelled);
    assert!(f.session().valve_match_id.is_none());
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
    let messages = discord.sent.lock().unwrap();
    let content = &messages.last().unwrap().1.response.content;
    assert!(content.contains("SIMULATED result: radiant wins"));
    assert!(content.contains("No match result, ratings, or bets were settled"));
    assert_eq!(
        PendingMatchRepository::new(&f.worker.path)
            .pending_match(1, f.pending)
            .unwrap()
            .unwrap()
            .state,
        pending.state
    );
}

#[tokio::test]
async fn removing_pending_match_cleans_up_real_preview_without_recording() {
    let f = Fixture::preview(DotaHostTestMode::RealLobby, true);
    f.worker.tick(&f.port, 100).await.unwrap();
    PendingMatchRepository::new(&f.worker.path)
        .delete_pending_match(1, f.pending)
        .unwrap();
    for now in [110, 115, 120] {
        f.worker.tick(&f.port, now).await.unwrap();
    }
    assert_eq!(f.session().phase, Phase::Cancelled);
    assert!(f.port.lobby.lock().unwrap().is_none());
    assert!(f.port.calls.lock().unwrap().contains(&"destroy".to_owned()));
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn connected_worker_does_not_refetch_or_archive_recorded_match_replays() {
    let now = chrono::Utc::now().timestamp();
    let f = Fixture::new_at(false, now);
    f.update_state(|record, state| {
        record.phase = Phase::Recorded;
        record.valve_match_id = Some("888".to_owned());
        state.recorded_match_id = Some(123);
        state.replay = Some(ReplayMetadata::Pending);
    });
    PendingMatchRepository::new(&f.worker.path)
        .delete_pending_match(1, f.pending)
        .unwrap();
    let before = f.session();
    let (_shutdown, receiver) = tokio::sync::watch::channel(false);
    let connected = f
        .worker
        .run_connected(&f.port, WorkerContext::new(receiver));
    tokio::pin!(connected);

    // Advance beyond the old archive worker's independent 60-second period.
    // Recorded sessions must neither trigger metadata polls nor mutate their
    // replay retry/archive state while the hosting loop continues running.
    tokio::select! {
        result = &mut connected => panic!("hosting loop stopped unexpectedly: {result:?}"),
        _ = tokio::time::sleep(Duration::from_secs(65)) => {}
    }
    assert!(f.port.calls.lock().unwrap().is_empty());
    assert_eq!(f.session().payload, before.payload);
    assert_eq!(f.session().revision, before.revision);
}

#[test]
fn historical_replay_archive_payload_remains_readable_and_preserved() {
    let f = Fixture::new(false);
    let mut payload = f.session().payload;
    let archive = serde_json::json!({
        "match_id": "888",
        "filename": "888.dem.bz2",
        "bytes": 1024,
        "sha256": "historical-checksum",
        "archived_at": 12345
    });
    payload["archive"] = archive.clone();
    payload["replay"] = serde_json::json!({"Available": {"cluster": 123, "salt": 456}});
    payload["replay_last_attempt"] = 12340.into();
    payload["replay_error"] = "historical error".into();

    let state: SessionState = serde_json::from_value(payload.clone()).unwrap();
    let saved = serde_json::to_value(state).unwrap();
    for key in ["archive", "replay", "replay_last_attempt", "replay_error"] {
        assert_eq!(saved[key], payload[key]);
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
    assert!(!f.betting_open(1100));
    f.update_state(|_, state| state.cancel_requested = true);
    f.worker.tick(&f.port, 121).await.unwrap();
    f.worker.tick(&f.port, 122).await.unwrap();
    assert_eq!(f.session().phase, Phase::Cancelled);
    assert!(f.betting_open(999));
    assert!(!f.betting_open(1000));
}

#[tokio::test]
async fn eligible_queued_game_does_not_accept_bets_without_observation() {
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
    assert!(!queued.state.hosted_betting_managed());
    assert!(!queued.state.betting_open(5000));
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
    let mut f = Fixture::new(false);
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

    let pending = PendingMatchRepository::new(&f.worker.path);
    let state = pending.pending_match(1, f.pending).unwrap().unwrap().state;
    pending.delete_pending_match(1, f.pending).unwrap();
    pending
        .configure_dota_host_routing(Some(cama_db::match_runtime::DotaHostRouting {
            account_key: "99".into(),
            guild_ids: vec![1],
        }))
        .unwrap();
    f.pending = pending
        .create_pending_match(1, &state)
        .unwrap()
        .pending_match_id;
    f.port.pending = f.pending;

    f.worker.tick(&f.port, 100).await.unwrap();
    let state: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert_eq!(state.settings.password, "");
    assert_eq!(state.settings.visibility, 0);
    assert_eq!(state.settings.server_region, 27);
    let lobby = f.port.lobby.lock().unwrap();
    let lobby = lobby.as_ref().unwrap();
    assert_eq!(lobby.visibility, 0);
    assert_eq!(lobby.server_region, 27);
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

#[derive(Default)]
struct TestDiscord {
    failures: AtomicUsize,
    sent: Mutex<Vec<(u64, DiscordMessage)>>,
}

#[test]
fn hosting_status_nonce_fits_discord_limit_and_keeps_session_identity() {
    let key = status_delivery_key(1457875036378763460, 2);
    assert_eq!(key.len(), 25);
    assert_eq!(key, status_delivery_key(1457875036378763460, 2));
    assert_ne!(key, status_delivery_key(1457875036378763460, 3));
    assert_ne!(key, status_delivery_key(1457875036378763461, 2));
    assert_eq!(status_delivery_key(i64::MAX, i64::MAX).len(), 25);
}

#[async_trait]
impl DiscordTransport for TestDiscord {
    async fn send_message_with_delivery_key(
        &self,
        channel_id: u64,
        delivery_key: &str,
        message: DiscordMessage,
    ) -> Result<crate::discord_transport::DiscordMessageReceipt, String> {
        assert!(delivery_key.len() <= 25, "Discord rejects overlong nonces");
        self.send_message(channel_id, message).await
    }

    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<crate::discord_transport::DiscordMessageSnapshot>, String> {
        Ok(None)
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<crate::discord_transport::DiscordMessageReceipt, String> {
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("Discord unavailable".into());
        }
        self.sent
            .lock()
            .expect("publication capture")
            .push((channel_id, message));
        Ok(crate::discord_transport::DiscordMessageReceipt {
            channel_id,
            message_id: 1,
            jump_url: "https://discord.invalid/publication".to_owned(),
        })
    }

    async fn edit_message(
        &self,
        channel_id: u64,
        _message_id: u64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("Discord unavailable".into());
        }
        self.sent.lock().unwrap().push((channel_id, message));
        Ok(())
    }

    async fn delete_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(1)
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(
        &self,
        _thread_id: u64,
        _name: &str,
        _locked: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
        _user_id: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn clear_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &crate::discord_transport::DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn unpin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn send_direct_message(
        &self,
        _user_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn guild_member(
        &self,
        _guild_id: u64,
        _user_id: u64,
    ) -> Result<Option<crate::discord_transport::DiscordGuildMemberSnapshot>, String> {
        Ok(None)
    }
}

#[tokio::test]
async fn coordinator_statistics_survive_a_recording_retry_with_sparse_details() {
    let f = Fixture::new(false);
    f.launch().await;
    f.complete(Some("radiant"));
    let statistics =
        serde_json::json!({"match_id":888,"radiant_win":true,"duration":1800,"players":[]});
    f.port
        .details
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .postgame_statistics = Some(statistics.clone());
    f.recorder.failures.store(1, Ordering::SeqCst);
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 1);
    let state: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert_eq!(state.postgame_statistics, Some(statistics.clone()));
    // The next GC result still proves the winner but omits postgame stats.
    f.port
        .details
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .postgame_statistics = None;
    f.worker.tick(&f.port, 181).await.unwrap();
    assert_eq!(*f.recorder.statistics.lock().unwrap(), Some(statistics));
}

#[tokio::test]
async fn final_snapshot_failure_never_persists_launch_intent() {
    let f = Fixture::new(false);
    f.worker.tick(&f.port, 100).await.unwrap();
    f.port.fail_snapshot_at.store(
        f.port.snapshot_calls.load(Ordering::SeqCst) + 2,
        Ordering::SeqCst,
    );
    assert_eq!(
        f.worker.tick(&f.port, 120).await.unwrap_err(),
        "final snapshot failed"
    );
    let saved: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert!(saved.launch_requested_at.is_none());
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "launch")
    );
    f.worker.tick(&f.port, 125).await.unwrap();
    assert!(
        f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "launch")
    );
}

#[tokio::test]
async fn withdrawn_allowlist_pauses_prelaunch_actions_but_retains_account() {
    let mut f = Fixture::new(false);
    f.worker.config.guild_ids.clear();
    f.worker.tick(&f.port, 100).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(f.port.calls.lock().unwrap().is_empty());
    assert_eq!(
        DotaSessionRepository::new(&f.worker.path)
            .active_sessions()
            .unwrap()
            .len(),
        1
    );
    guild_operator_command(f.worker.path.clone(), "cancel", 1, Some(f.pending))
        .await
        .unwrap();
    f.worker.tick(&f.port, 110).await.unwrap();
    assert_eq!(f.session().phase, Phase::Cancelled);
    assert!(
        PendingMatchRepository::new(&f.worker.path)
            .pending_match(1, f.pending)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn coordinator_disconnect_suspends_bets_and_review_propagates_reconnect_error() {
    let f = Fixture::new(false);
    f.launch().await;
    assert!(f.betting_open(120));
    *f.port.snapshot_error.lock().unwrap() = Some("GC disconnected".into());
    assert_eq!(
        f.worker.tick(&f.port, 120).await.unwrap_err(),
        "GC disconnected"
    );
    assert!(!f.betting_open(120));
    f.update_state(|record, _| {
        record.phase = Phase::NeedsReview;
        record.valve_match_id = Some("888".into());
    });
    assert_eq!(
        f.worker.tick(&f.port, 125).await.unwrap_err(),
        "GC disconnected"
    );
    assert!(!f.betting_open(125));
}

#[tokio::test]
async fn operator_void_resolves_aborted_launched_match_and_releases_account() {
    let f = Fixture::new(false);
    f.launch().await;
    PendingMatchRepository::new(&f.worker.path)
        .delete_pending_match(1, f.pending)
        .unwrap();
    f.complete(None);
    f.worker.tick(&f.port, 150).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(
        guild_resolution_command(
            f.worker.path.clone(),
            2,
            f.pending,
            42,
            "void".into(),
            888,
            "Abandoned".into()
        )
        .await
        .is_err()
    );
    assert!(
        guild_resolution_command(
            f.worker.path.clone(),
            1,
            f.pending,
            42,
            "void".into(),
            889,
            "Abandoned".into()
        )
        .await
        .is_err()
    );
    guild_resolution_command(
        f.worker.path.clone(),
        1,
        f.pending,
        42,
        "void".into(),
        888,
        "Confirmed abandoned match".into(),
    )
    .await
    .unwrap();
    f.worker.tick(&f.port, 160).await.unwrap();
    assert!(
        f.session().phase.is_active(),
        "retain reservation until GC confirms removal"
    );
    f.worker.tick(&f.port, 165).await.unwrap();
    assert_eq!(f.session().phase, Phase::Cancelled);
    assert!(
        DotaSessionRepository::new(&f.worker.path)
            .active_sessions()
            .unwrap()
            .is_empty()
    );
    let saved: SessionState = serde_json::from_value(f.session().payload).unwrap();
    let audit = saved.resolution.unwrap();
    assert_eq!(audit.actor_id, 42);
    assert_eq!(audit.reason, "Confirmed abandoned match");
    assert_eq!(f.recorder.count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn operator_void_never_destroys_running_or_foreign_lobby() {
    let f = Fixture::new(false);
    f.launch().await;
    f.running(Some(5));
    f.worker.tick(&f.port, 150).await.unwrap();
    guild_resolution_command(
        f.worker.path.clone(),
        1,
        f.pending,
        42,
        "void".into(),
        888,
        "Remake requested".into(),
    )
    .await
    .unwrap();
    f.worker.tick(&f.port, 160).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(
        PendingMatchRepository::new(&f.worker.path)
            .pending_match(1, f.pending)
            .unwrap()
            .is_some()
    );
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "destroy")
    );
    f.complete(None);
    f.port
        .lobby
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .owner_account_id = 123;
    f.worker.tick(&f.port, 165).await.unwrap();
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "destroy")
    );
}

#[tokio::test]
async fn operator_can_resolve_stale_running_cache_with_complete_valve_end_evidence() {
    let f = Fixture::new(false);
    f.launch().await;
    use cama_db::betting_service_repository::{BettingServiceRepository, PlaceBetRequest};
    let connection = rusqlite::Connection::open(&f.worker.path).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    connection.execute("INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance) VALUES(33,1,'viewer',200)", []).unwrap();
    BettingServiceRepository::new(&f.worker.path)
        .place_bet_atomic(PlaceBetRequest {
            guild_id: Some(1),
            pending_match_id: f.pending,
            discord_id: 33,
            team: cama_db::dota_bet_seed_repository::BettingTeam::Radiant,
            amount: 30,
            bet_time: 115,
            leverage: 1,
            max_debt: 0,
            is_blind: false,
            odds_at_placement: None,
        })
        .unwrap();
    f.running(Some(5));
    f.worker.tick(&f.port, 150).await.unwrap();
    guild_resolution_command(
        f.worker.path.clone(),
        1,
        f.pending,
        42,
        "void".into(),
        888,
        "Abandoned after server finished".into(),
    )
    .await
    .unwrap();
    f.complete(None);
    f.port.lobby.lock().unwrap().as_mut().unwrap().stage = LobbyStage::Running;
    f.port.observation_stale.store(true, Ordering::SeqCst);
    assert!(
        f.worker
            .tick(&f.port, 155)
            .await
            .unwrap_err()
            .contains("refreshing stale GC ownership")
    );
    assert!(
        PendingMatchRepository::new(&f.worker.path)
            .pending_match(1, f.pending)
            .unwrap()
            .is_some()
    );
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "destroy")
    );
    f.port.observation_stale.store(false, Ordering::SeqCst);
    f.worker.tick(&f.port, 160).await.unwrap();
    assert!(
        PendingMatchRepository::new(&f.worker.path)
            .pending_match(1, f.pending)
            .unwrap()
            .is_none()
    );
    f.worker.tick(&f.port, 165).await.unwrap();
    assert_eq!(f.session().phase, Phase::Cancelled);
    let balance: i64 = connection
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=33 AND guild_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(balance, 200, "operator resolution refunded the exact stake");
    assert!(
        BettingServiceRepository::new(&f.worker.path)
            .get_pending_bets(Some(1), None, 0, Some(f.pending))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn review_status_ignores_throttle_and_terminal_failure_retries_after_restart() {
    let mut f = Fixture::new(false);
    let discord = Arc::new(TestDiscord::default());
    f.worker.discord = discord.clone();
    f.update_state(|_, state| {
        state.channel_id = Some(55);
        state.last_message_at = 99;
    });
    let mut record = f.session();
    let mut state: SessionState = serde_json::from_value(record.payload.clone()).unwrap();
    f.worker
        .review(&mut record, &mut state, "identity conflict", 100)
        .await
        .unwrap();
    assert_eq!(discord.sent.lock().unwrap().len(), 1);
    discord.failures.store(1, Ordering::SeqCst);
    record.phase = Phase::Cancelled;
    state.pending_status = Some("Terminal resolution complete".into());
    f.worker.save(&mut record, &state, 101).await.unwrap();
    f.worker
        .announce(&mut record, &mut state, "Terminal resolution complete", 101)
        .await
        .unwrap();
    assert!(state.pending_status.is_some());
    f.worker.tick(&f.port, 110).await.unwrap();
    let state: SessionState = serde_json::from_value(f.session().payload).unwrap();
    assert!(state.pending_status.is_none());
    assert_eq!(state.last_message, "Terminal resolution complete");
    assert_eq!(discord.sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn recorded_resolution_requires_completed_settlement_then_releases_account() {
    let f = Fixture::new(false);
    f.launch().await;
    f.running(Some(5));
    f.worker.tick(&f.port, 150).await.unwrap();
    guild_resolution_command(
        f.worker.path.clone(),
        1,
        f.pending,
        42,
        "recorded".into(),
        888,
        "Verified and manually recorded".into(),
    )
    .await
    .unwrap();
    f.complete(Some("radiant"));
    let connection = rusqlite::Connection::open(&f.worker.path).unwrap();
    connection.execute("INSERT INTO matches(match_id,team1_players,team2_players,winning_team,guild_id,pending_match_id) VALUES(321,'[1,2,3,4,5]','[6,7,8,9,10]',1,1,?1)", [f.pending]).unwrap();
    f.worker.tick(&f.port, 160).await.unwrap();
    assert_eq!(f.session().phase, Phase::NeedsReview);
    assert!(
        !f.port
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "destroy")
    );
    PendingMatchRepository::new(&f.worker.path)
        .delete_pending_match(1, f.pending)
        .unwrap();
    f.worker.tick(&f.port, 165).await.unwrap();
    f.worker.tick(&f.port, 170).await.unwrap();
    assert_eq!(f.session().phase, Phase::Recorded);
    assert!(
        DotaSessionRepository::new(&f.worker.path)
            .active_sessions()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        serde_json::from_value::<SessionState>(f.session().payload)
            .unwrap()
            .recorded_match_id,
        Some(321)
    );
}

#[tokio::test]
async fn readable_but_stale_gc_cache_never_renews_betting_lease() {
    let f = Fixture::new(false);
    f.launch().await;
    assert!(f.betting_open(115));
    f.port.observation_stale.store(true, Ordering::SeqCst);
    f.worker.tick(&f.port, 120).await.unwrap();
    assert!(!f.betting_open(120));
    f.worker.tick(&f.port, 125).await.unwrap();
    assert!(!f.betting_open(125));
    f.port.observation_stale.store(false, Ordering::SeqCst);
    f.worker.tick(&f.port, 130).await.unwrap();
    assert!(f.betting_open(130));
}

#[tokio::test]
async fn incomplete_shuffle_setup_cannot_claim_or_create_a_dota_lobby() {
    let f = Fixture::preview(DotaHostTestMode::RealLobby, true);
    let repo = PendingMatchRepository::new(&f.worker.path);
    let mut pending = repo.pending_match(1, f.pending).unwrap().unwrap();
    pending
        .state
        .extra
        .insert("shuffle_setup_complete".into(), false.into());
    repo.update_pending_match(1, f.pending, &pending.state)
        .unwrap();
    f.worker.tick(&f.port, 100).await.unwrap();
    assert!(
        DotaSessionRepository::new(&f.worker.path)
            .active_sessions()
            .unwrap()
            .is_empty()
    );
    assert!(f.port.calls.lock().unwrap().is_empty());
    pending
        .state
        .extra
        .insert("shuffle_setup_complete".into(), true.into());
    repo.update_pending_match(1, f.pending, &pending.state)
        .unwrap();
    f.worker.tick(&f.port, 105).await.unwrap();
    assert_eq!(
        DotaSessionRepository::new(&f.worker.path)
            .active_sessions()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(*f.port.calls.lock().unwrap(), vec!["create".to_owned()]);
}
