use super::*;
use cama_db::opendota_player::OpenDotaPlayerRepository;
use cama_domain::dota_lobby::STEAM_INDIVIDUAL_BASE;

#[tokio::test]
async fn hosted_draft_replaces_timed_reminders_and_countdown_with_gameplay_start() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 900);
    fixture
        .provider
        .handler
        .schedule_betting_reminders(&pending, false);
    let key = (GUILD, pending.pending_match_id);
    assert!(
        fixture
            .provider
            .handler
            .betting_tasks
            .lock()
            .unwrap()
            .contains_key(&key)
    );
    let repository = PendingMatchRepository::new(fixture.database.path());
    let mut managed = repository
        .begin_hosted_betting(GUILD, pending.pending_match_id, unix_seconds())
        .unwrap();
    fixture
        .provider
        .hosted_betting_window_changed(GUILD, pending.pending_match_id)
        .await
        .unwrap();
    assert!(
        !fixture
            .provider
            .handler
            .betting_tasks
            .lock()
            .unwrap()
            .contains_key(&key)
    );
    // Even a stale historical deadline must not be rendered as a close.
    managed.state.bet_lock_until = Some(unix_seconds() - 1);
    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&managed)
        .await
        .unwrap();
    let wager = embed
        .fields
        .iter()
        .find(|field| field.name.contains("Betting"))
        .unwrap();
    assert!(wager.value.contains("closes when gameplay starts"));
    assert!(!wager.value.contains("Betting closed"));
    assert!(!wager.value.contains("Closes <t:"));
    let closed = repository
        .close_betting_now(GUILD, pending.pending_match_id, unix_seconds())
        .unwrap();
    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&closed.pending_match)
        .await
        .unwrap();
    let wager = embed
        .fields
        .iter()
        .find(|field| field.name.contains("Betting"))
        .unwrap();
    assert!(wager.value.contains("Betting closed"));
    assert!(!wager.value.contains("through the hero draft"));
}

#[tokio::test]
async fn admin_extension_reopens_hosted_betting_and_restores_timed_reminders() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() - 1);
    let repository = PendingMatchRepository::new(fixture.database.path());
    repository
        .begin_hosted_betting(GUILD, pending.pending_match_id, unix_seconds())
        .unwrap();
    repository
        .close_betting_now(GUILD, pending.pending_match_id, unix_seconds())
        .unwrap();
    let extension = fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 10,
            pending_match_id: pending.pending_match_id,
        })
        .await
        .unwrap();
    assert!(!extension.waits_for_gameplay_start);
    // A delayed host notification must neither cancel the override nor its
    // replacement countdown/reminders.
    fixture
        .provider
        .hosted_betting_closed(GUILD, pending.pending_match_id)
        .await
        .unwrap();
    assert!(
        fixture
            .provider
            .handler
            .betting_tasks
            .lock()
            .unwrap()
            .contains_key(&(GUILD, pending.pending_match_id))
    );
    let reopened = repository
        .pending_match(GUILD, pending.pending_match_id)
        .unwrap()
        .unwrap();
    assert!(reopened.state.betting_closed());
    assert!(reopened.state.betting_open(unix_seconds()));
    assert!(!reopened.state.betting_open(extension.new_bet_lock_until));
    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&reopened)
        .await
        .unwrap();
    let wager = embed
        .fields
        .iter()
        .find(|field| field.name.contains("Betting"))
        .unwrap();
    assert!(
        wager
            .value
            .contains(&format!("Closes <t:{}:R>", extension.new_bet_lock_until))
    );
    assert!(!wager.value.contains("Betting closed"));
    assert!(!wager.value.contains("through the hero draft"));
}

#[tokio::test]
async fn admin_extension_during_hosted_draft_keeps_the_gameplay_cutoff() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() - 1);
    let repository = PendingMatchRepository::new(fixture.database.path());
    repository
        .begin_hosted_betting(GUILD, pending.pending_match_id, unix_seconds())
        .unwrap();
    let extension = fixture
        .provider
        .handler
        .extend_betting(AdminExtendBettingRequest {
            guild_id: GUILD,
            actor_id: 99,
            minutes: 10,
            pending_match_id: pending.pending_match_id,
        })
        .await
        .unwrap();
    assert!(extension.waits_for_gameplay_start);
    assert!(
        !fixture
            .provider
            .handler
            .betting_tasks
            .lock()
            .unwrap()
            .contains_key(&(GUILD, pending.pending_match_id))
    );
    let extended = repository
        .pending_match(GUILD, pending.pending_match_id)
        .unwrap()
        .unwrap();
    assert!(extended.state.betting_open(extension.new_bet_lock_until));
    let embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&extended)
        .await
        .unwrap();
    assert!(embed.fields.iter().any(|field| {
        field
            .value
            .contains("open through the hero draft and at least until")
    }));
    repository
        .close_betting_now(GUILD, pending.pending_match_id, unix_seconds())
        .unwrap();
    fixture
        .provider
        .hosted_betting_closed(GUILD, pending.pending_match_id)
        .await
        .unwrap();
    assert!(
        fixture
            .provider
            .handler
            .betting_tasks
            .lock()
            .unwrap()
            .contains_key(&(GUILD, pending.pending_match_id))
    );
}

fn linked_hosted_result(
    fixture: &MatchRuntimeFixture,
    pending: &PendingMatchRecord,
    valve_match_id: u64,
    winner: &str,
) -> HostedMatchResult {
    let steam = OpenDotaPlayerRepository::new(fixture.database.path());
    let mut expected_roster = Vec::new();
    let mut players = Vec::new();
    for (index, discord_id) in pending
        .state
        .radiant_team_ids
        .iter()
        .chain(&pending.state.dire_team_ids)
        .copied()
        .enumerate()
    {
        let account32 = u32::try_from(700_000 + index).expect("fixture account ID");
        steam
            .set_steam_id(discord_id, i64::from(account32), unix_seconds())
            .expect("link fixture Steam account");
        let radiant = pending.state.radiant_team_ids.contains(&discord_id);
        expected_roster.push(HostedMatchRosterEntry {
            discord_id,
            steam_account_id: account32,
            radiant,
        });
        players.push(HostedMatchPlayer { account32, radiant });
    }
    HostedMatchResult {
        guild_id: pending.guild_id,
        pending_match_id: pending.pending_match_id,
        valve_match_id,
        winning_team: winner.to_owned(),
        expected_roster,
        players,
    }
}

#[test]
fn hosted_finalization_guard_excludes_and_releases() {
    let fixture = MatchRuntimeFixture::new();
    let first = fixture
        .provider
        .try_acquire_hosted_finalization_guard(GUILD, MATCH_ID)
        .expect("first hosted finalization guard");
    assert!(
        fixture
            .provider
            .try_acquire_hosted_finalization_guard(GUILD, MATCH_ID)
            .is_none()
    );

    // A blocking task may outlive the future awaiting it. Its clone keeps
    // the shared lease held until the task drops it too.
    let task_lease = first.clone();
    drop(first);
    assert!(
        fixture
            .provider
            .try_acquire_hosted_finalization_guard(GUILD, MATCH_ID)
            .is_none()
    );
    drop(task_lease);
    let second = fixture
        .provider
        .try_acquire_hosted_finalization_guard(GUILD, MATCH_ID)
        .expect("guard is released when its owner drops it");
    drop(second);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hosted_finalization_guard_excludes_manual_abort() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let guard = fixture
        .provider
        .try_acquire_hosted_finalization_guard(GUILD, pending.pending_match_id)
        .expect("hosted finalization guard");
    let responder = Arc::new(RecordingMatchResponder::default());

    fixture
        .provider
        .handler
        .finalize_abort(&pending, responder.clone())
        .await
        .expect("manual abort should defer while hosted finalization is held");
    assert!(
        responder
            .contents()
            .iter()
            .any(|content| content.contains("being launched or finalized"))
    );
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read pending match while guard is held")
            .is_some()
    );

    drop(guard);
    let released = fixture
        .provider
        .try_acquire_hosted_finalization_guard(GUILD, pending.pending_match_id)
        .expect("guard is released after hosted operation returns");
    drop(released);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hosted_record_defers_to_abort_before_reading_pending_state() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let result = linked_hosted_result(&fixture, &pending, 8_123_456_789, "radiant");
    let guard = fixture
        .provider
        .handler
        .try_acquire_finalization_guard(GUILD, pending.pending_match_id)
        .expect("abort owns finalization");
    PendingMatchRepository::new(fixture.database.path())
        .delete_pending_match(GUILD, pending.pending_match_id)
        .expect("abort removes pending match while retaining finalization");

    let error = fixture
        .provider
        .record_hosted_match(result.clone())
        .await
        .expect_err("hosted record must defer before reading changing state");
    assert!(error.contains("already in progress"), "{error}");
    drop(guard);

    let error = fixture
        .provider
        .record_hosted_match(result)
        .await
        .expect_err("aborted pending match must not be recorded");
    assert!(error.contains("was not found"), "{error}");
    assert!(
        MatchRepository::new(fixture.database.path())
            .match_id_for_pending_match(GUILD, pending.pending_match_id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hosted_record_uses_production_saga_and_is_idempotent() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let result = linked_hosted_result(&fixture, &pending, 8_123_456_789, "radiant");
    let steam = OpenDotaPlayerRepository::new(fixture.database.path());
    let first = &result.expected_roster[0];
    steam
        .remove_steam_id(first.discord_id, i64::from(first.steam_account_id))
        .expect("remove account32 fixture link");
    steam
        .set_steam_id(
            first.discord_id,
            i64::try_from(STEAM_INDIVIDUAL_BASE + u64::from(first.steam_account_id))
                .expect("fixture Steam64 ID"),
            unix_seconds(),
        )
        .expect("link Steam64 fixture account");
    let radiant = pending.state.radiant_team_ids.clone();
    let dire = pending.state.dire_team_ids.clone();

    let match_id = fixture
        .provider
        .record_hosted_match(result.clone())
        .await
        .expect("record hosted match");
    let summary = MatchRepository::new(fixture.database.path())
        .get_match(match_id, Some(GUILD))
        .expect("read hosted match")
        .expect("hosted match exists");
    assert_eq!(summary.valve_match_id, Some(8_123_456_789));
    assert_eq!(summary.team1_players, radiant);
    assert_eq!(summary.team2_players, dire);
    // A stale pending payload (including an unexpired admin override) must
    // never advertise open betting after the durable result has committed.
    let mut stale_pending = pending.clone();
    stale_pending
        .state
        .extra
        .insert("dota_hosted_betting".into(), json!(true));
    stale_pending
        .state
        .extra
        .insert("dota_betting_closed".into(), json!(true));
    stale_pending.state.extra.insert(
        "dota_betting_extended_until".into(),
        json!(unix_seconds() + 600),
    );
    let stale_embed = fixture
        .provider
        .handler
        .render_shuffle_embed(&stale_pending)
        .await
        .expect("render stale recorded betting state");
    assert!(
        stale_embed.fields.iter().any(|field| {
            field.name.contains("Betting") && field.value.contains("Betting closed")
        })
    );
    assert!(
        PendingMatchRepository::new(fixture.database.path())
            .pending_match(GUILD, pending.pending_match_id)
            .expect("read cleared hosted pending match")
            .is_none()
    );

    let players = PlayerRepository::new(fixture.database.path());
    for discord_id in summary.team1_players.iter().chain(&summary.team2_players) {
        let player = players
            .get_by_id(*discord_id, Some(GUILD))
            .expect("read recorded hosted player")
            .expect("recorded hosted player exists");
        if summary.team1_players.contains(discord_id) {
            assert_eq!((player.wins, player.losses), (1, 0));
        } else {
            assert_eq!((player.wins, player.losses), (0, 1));
        }
        let changes = summary
            .jc_changes
            .get(discord_id)
            .expect("production economy changes for hosted player");
        assert!(changes.contains_key("payout"));
    }
    let connection = Connection::open(fixture.database.path()).expect("inspect hosted effects");
    let history_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rating_history WHERE guild_id=?1 AND match_id=?2",
            params![GUILD, match_id],
            |row| row.get(0),
        )
        .expect("count hosted rating history");
    let economy_ledger_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM economy_ledger_entries WHERE guild_id=?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("count hosted economy ledger");
    assert_eq!(history_count, 10);
    assert!(economy_ledger_count > 0);

    assert_eq!(
        fixture
            .provider
            .record_hosted_match(result)
            .await
            .expect("idempotent hosted retry"),
        match_id
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hosted_record_rejects_unlinked_or_conflicting_result() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let result = linked_hosted_result(&fixture, &pending, 8_123_456_790, "dire");
    let steam = OpenDotaPlayerRepository::new(fixture.database.path());
    steam
        .remove_steam_id(
            result.expected_roster[0].discord_id,
            i64::from(result.expected_roster[0].steam_account_id),
        )
        .expect("unlink fixture Steam account");
    let error = fixture
        .provider
        .record_hosted_match(result.clone())
        .await
        .expect_err("unlinked hosted account must fail");
    assert!(error.contains("not linked"));

    let first = &result.expected_roster[0];
    let mut duplicate_account = result.clone();
    duplicate_account.expected_roster[0].steam_account_id =
        duplicate_account.expected_roster[1].steam_account_id;
    let error = fixture
        .provider
        .record_hosted_match(duplicate_account)
        .await
        .expect_err("duplicate hosted account must fail");
    assert!(error.contains("reuses Steam account"));
    steam
        .set_steam_id(
            first.discord_id,
            i64::from(first.steam_account_id),
            unix_seconds(),
        )
        .expect("restore fixture Steam account link");
    let match_id = fixture
        .provider
        .record_hosted_match(result.clone())
        .await
        .expect("record second hosted match");

    let abort_responder = Arc::new(RecordingMatchResponder::default());
    fixture
        .provider
        .handler
        .finalize_abort(&pending, abort_responder.clone())
        .await
        .expect("abort guard for hosted match");
    assert!(
        abort_responder
            .contents()
            .iter()
            .any(|content| content.contains("already been recorded"))
    );

    let mut conflicting = result.clone();
    conflicting.winning_team = "radiant".to_owned();
    let error = fixture
        .provider
        .record_hosted_match(conflicting)
        .await
        .expect_err("conflicting hosted winner must fail");
    assert!(error.contains("winner conflicts"));

    // The pending row was consumed by the successful result, so the request
    // is now checked against the durable match and must reject a new Valve ID.
    let mut retry = result;
    retry.valve_match_id = 8_123_456_793;
    let error = fixture
        .provider
        .record_hosted_match(retry)
        .await
        .expect_err("conflicting Valve ID must fail");
    assert!(error.contains("Valve match ID"));
    assert!(match_id > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hosted_record_rejects_account32_steam64_owner_conflict() {
    let fixture = MatchRuntimeFixture::new();
    let pending = fixture.pending(unix_seconds() + 120);
    let result = linked_hosted_result(&fixture, &pending, 8_123_456_794, "radiant");
    let steam = OpenDotaPlayerRepository::new(fixture.database.path());
    let first = &result.expected_roster[0];
    let second = &result.expected_roster[1];
    steam
        .set_steam_id(
            second.discord_id,
            i64::try_from(STEAM_INDIVIDUAL_BASE + u64::from(first.steam_account_id))
                .expect("fixture Steam64 ID"),
            unix_seconds(),
        )
        .expect("install conflicting Steam64 fixture link");

    let error = fixture
        .provider
        .record_hosted_match(result)
        .await
        .expect_err("equivalent account forms owned by different players must fail");
    assert!(error.contains("owned by a different Discord player"));
}
