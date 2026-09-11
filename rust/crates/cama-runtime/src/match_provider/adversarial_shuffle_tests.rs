//! Failed setup and publication recovery must conserve money and identity.
use super::*;

#[tokio::test]
async fn seed_failure_compensates_pending_and_first_game_claim() {
    let mut config = production_test_config();
    config.values.first_game_pool_daily_amount = 100;
    config.values.dota_bet_seed_amount = 50;
    let fixture = MatchRuntimeFixture::new_with_config_and_discord(
        config,
        Arc::new(PublicationDiscord::default()),
    );
    let player_ids = fixture.add_shuffle_pool(10, false);
    let connection = Connection::open(fixture.database.path()).unwrap();
    connection
        .execute(
            "INSERT INTO nonprofit_fund(guild_id,total_collected) VALUES(?1,1000)
         ON CONFLICT(guild_id) DO UPDATE SET total_collected=1000",
            [GUILD],
        )
        .unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER adversarial_fail_seed BEFORE UPDATE OF payload ON pending_matches
         WHEN json_extract(NEW.payload,'$.bet_seed_reserved') > 0
         BEGIN SELECT RAISE(ABORT,'adversarial seed write failure'); END;",
        )
        .unwrap();
    let outcome = fixture
        .provider
        .handler
        .prepare_shuffle(PrepareShuffleRequest {
            source_lobby_message_id: Some(123),
            guild_id: GUILD,
            lobby_kind: LobbyKind::Open,
            player_ids,
            excluded_conditional_ids: Vec::new(),
            lobby_wait_minutes: HashMap::new(),
            rating_system: "glicko".into(),
            shuffle_mode: "balanced".into(),
            shuffle_timestamp: unix_seconds(),
            is_bomb_pot: false,
            dota_hosting: DotaHostingOptions::default(),
        });
    assert!(outcome.is_err());
    let repository = PendingMatchRepository::new(fixture.database.path());
    let rows = repository.pending_matches(GUILD).unwrap();
    assert!(
        rows.is_empty(),
        "failed setup must not remain host-discoverable"
    );
    let claims: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM first_game_pool_claims WHERE guild_id=?1",
            [GUILD],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(claims, 0);
    let funds:i64=connection.query_row("SELECT total_collected+first_game_open_pool+first_game_lowskill_pool FROM nonprofit_fund WHERE guild_id=?1",[GUILD],|r|r.get(0)).unwrap();
    assert_eq!(funds, 1000);
    let exclusions: i64 = connection
        .query_row(
            "SELECT SUM(exclusion_count) FROM players WHERE guild_id=?1",
            [GUILD],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        exclusions, 50,
        "failed setup must not credit played-game exclusion"
    );
}

#[tokio::test]
async fn interrupted_preparing_setup_is_refunded_once_after_lease() {
    let f = MatchRuntimeFixture::new();
    let p = f.pending(unix_seconds() + 120);
    let repo = PendingMatchRepository::new(f.database.path());
    let pending = repo
        .mutate_pending_match(GUILD, p.pending_match_id, |s| {
            s.extra
                .insert("shuffle_setup_complete".into(), json!(false));
            s.extra.insert(
                "_cama_shuffle_setup".into(),
                json!({"phase":"preparing","lease_until":0}),
            );
        })
        .unwrap()
        .unwrap()
        .0;
    f.provider
        .handler
        .recover_pending_match(pending.clone())
        .await
        .unwrap();
    assert!(
        repo.pending_match(GUILD, p.pending_match_id)
            .unwrap()
            .is_none()
    );
    f.provider
        .handler
        .recover_pending_match(pending)
        .await
        .unwrap();
    let count: i64 = Connection::open(f.database.path())
        .unwrap()
        .query_row(
            "SELECT SUM(exclusion_count) FROM players WHERE guild_id=?1",
            [GUILD],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 50);
}

#[tokio::test]
async fn expired_confirmation_does_not_lose_shuffle_receipts() {
    let f = MatchRuntimeFixture::new();
    let ids = f.add_shuffle_pool(10, false);
    let prepared = f.prepare_shuffle(ids, "glicko", Vec::new());
    let id = prepared.pending.pending_match_id;
    f.provider
        .handler
        .finalize_shuffle(
            shuffle_context(Some(200)),
            Arc::new(FailingConfirmationResponder {
                started: AtomicBool::new(false),
            }),
            publication_snapshot(LobbyKind::Open, None),
            prepared,
        )
        .await
        .unwrap();
    let saved = PendingMatchRepository::new(f.database.path())
        .pending_match(GUILD, id)
        .unwrap()
        .unwrap();
    assert!(saved.state.shuffle_message_id.is_some());
    assert!(saved.state.cmd_shuffle_message_id.is_some());
    assert_eq!(
        saved.state.extra.get("shuffle_setup_complete"),
        Some(&json!(true))
    );
}

#[tokio::test]
async fn partial_publication_retries_missing_destination_without_repeating_saved_send() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let f = MatchRuntimeFixture::new_with_discord(discord.clone());
    let ids = f.add_shuffle_pool(10, false);
    let mut prepared = f.prepare_shuffle(ids, "glicko", Vec::new());
    let repo = PendingMatchRepository::new(f.database.path());
    let id = prepared.pending.pending_match_id;
    prepared.pending = repo
        .mutate_pending_match(GUILD, id, |s| {
            s.extra
                .insert("shuffle_setup_complete".into(), json!(false));
            s.extra
                .insert("_cama_shuffle_setup".into(), json!({"phase":"prepared"}));
        })
        .unwrap()
        .unwrap()
        .0;
    discord.fail_sends_to([200]);
    assert!(
        f.provider
            .handler
            .finalize_shuffle(
                shuffle_context(Some(200)),
                Arc::new(RecordingMatchResponder::default()),
                publication_snapshot(LobbyKind::Open, None),
                prepared
            )
            .await
            .is_err()
    );
    let saved = repo.pending_match(GUILD, id).unwrap().unwrap();
    assert!(saved.state.shuffle_message_id.is_some());
    assert!(saved.state.cmd_shuffle_message_id.is_none());
    assert!(!saved.state.betting_open(unix_seconds()));
    discord.failed_send_channels.lock().unwrap().clear();
    f.provider
        .handler
        .recover_pending_match(saved)
        .await
        .unwrap();
    let done = repo.pending_match(GUILD, id).unwrap().unwrap();
    assert_eq!(
        done.state.extra.get("shuffle_setup_complete"),
        Some(&json!(true))
    );
    assert!(done.state.cmd_shuffle_message_id.is_some());
    assert_eq!(
        discord
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(channel, _)| *channel == 100)
            .count(),
        1
    );
}

#[tokio::test]
async fn thread_publication_recovery_does_not_repeat_saved_embed_or_player_ping() {
    let discord = Arc::new(PublicationProbeDiscord::default());
    let fixture = MatchRuntimeFixture::new_with_discord(discord.clone());
    let repo = PendingMatchRepository::new(fixture.database.path());
    let pending = repo
        .create_pending_match(
            GUILD,
            &PendingMatchState {
                radiant_team_ids: vec![10, -1],
                dire_team_ids: vec![20],
                ..PendingMatchState::default()
            },
        )
        .unwrap();
    let snapshot = publication_snapshot(LobbyKind::Open, Some(42));
    fixture
        .provider
        .handler
        .publish_shuffle_thread(&snapshot, &pending, InteractionEmbed::titled("Teams"))
        .await
        .unwrap();
    let saved = repo
        .pending_match(GUILD, pending.pending_match_id)
        .unwrap()
        .unwrap();
    assert!(saved.state.thread_shuffle_message_id.is_some());
    fixture
        .provider
        .handler
        .publish_shuffle_thread(&snapshot, &saved, InteractionEmbed::titled("Teams"))
        .await
        .unwrap();
    assert_eq!(discord.sent_messages().len(), 2);
}

#[tokio::test]
async fn stale_preparing_snapshot_cannot_abort_an_already_prepared_shuffle() {
    let f = MatchRuntimeFixture::new();
    let pending = f.pending(unix_seconds() + 120);
    let repo = PendingMatchRepository::new(f.database.path());
    let stale = repo
        .mutate_pending_match(GUILD, pending.pending_match_id, |s| {
            s.extra
                .insert("shuffle_setup_complete".into(), json!(false));
            s.extra.insert(
                "_cama_shuffle_setup".into(),
                json!({"phase":"preparing","lease_until":0}),
            );
        })
        .unwrap()
        .unwrap()
        .0;
    repo.mutate_pending_match(GUILD, pending.pending_match_id, |s| {
        s.extra.get_mut("_cama_shuffle_setup").unwrap()["phase"] = json!("prepared");
    })
    .unwrap();
    f.provider
        .handler
        .recover_pending_match(stale)
        .await
        .unwrap();
    assert!(
        repo.pending_match(GUILD, pending.pending_match_id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn busy_bot_shuffle_result_explains_manual_creation_and_recording() {
    let fixture = MatchRuntimeFixture::new();
    let mut pending = fixture.pending(unix_seconds() + 120);
    pending
        .state
        .extra
        .insert("dota_hosting".into(), json!({"hosting":"manual"}));
    pending
        .state
        .extra
        .insert("dota_hosting_fallback_reason".into(), json!("bot_busy"));
    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&pending)
        .await
        .unwrap();
    let text = &embed
        .fields
        .iter()
        .find(|f| f.name == "🎮 Dota Lobby")
        .unwrap()
        .value;
    assert!(text.contains("hosting another match"));
    assert!(text.contains("create the Dota lobby yourself"));
    assert!(text.contains("`/record`"));
}
