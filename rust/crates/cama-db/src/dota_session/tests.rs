use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::Connection;
use serde_json::json;
use tempfile::NamedTempFile;

use super::*;

const GUILD_A: i64 = 7;
const GUILD_B: i64 = 8;
const ACCOUNT: &str = "steam-bot-main";

fn manual_override_fixture() -> (NamedTempFile, DotaSessionRepository, DotaSessionRecord) {
    let file = NamedTempFile::new().unwrap();
    crate::schema_manager::initialize_or_migrate(file.path()).unwrap();
    let repository = DotaSessionRepository::new(file.path());
    let mut session = claim_created(&repository, GUILD_A, 604, ACCOUNT, 10);
    session.phase = DotaSessionPhase::Running;
    session.lobby_id = Some("old-lobby".into());
    session.valve_match_id = Some("1000".into());
    session.payload = json!({
        "roster":[{"account_id":11,"is_radiant":true},{"account_id":22,"is_radiant":false}],
        "settings":{"region":27},"opaque":{"keep":[1,2,3]},
        "launch_requested_at":15,"resolution":{"outcome":"aborted"},
        "configuration":{"server_region":31},"pending_status":"old result",
        "cancel_requested":true,"resume_requested":true,"manual_start_requested":true,
        "manual_record_override":null
    });
    let session = repository.update(&session, session.revision, 20).unwrap();
    let connection = open_runtime_connection(file.path()).unwrap();
    connection
        .execute(
            "INSERT INTO pending_matches(pending_match_id,guild_id,payload) VALUES (604,?1,?2)",
            params![
                GUILD_A,
                json!({
                    "team1_players":[11],"team2_players":[22],"betting_deadline":300,
                    "dota_betting_extended_until":400,"dota_betting_closed":true,
                    "dota_betting_closed_at":200,"shuffle_setup_complete":true,
                    "draft_setup_complete":true,"dota_host_account_key":ACCOUNT,
                    "dota_hosting":{"hosting":"bot","server_region":27},
                    "dota_hosted_betting":true,"dota_hosted_betting_started_at":10,
                    "dota_hosted_betting_observed_at":200,"seed_reservations":{"pool":500},
                    "dota_betting_control_audit":[{"action":"resume","actor_id":99}],
                    "opaque":{"future_field":"keep"},"manual_record_override":null
                })
                .to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO bets(guild_id,pending_match_id,discord_id,team_bet_on,amount,bet_time)
        VALUES (?1,604,11,'radiant',100,12)",
            [GUILD_A],
        )
        .unwrap();
    (file, repository, session)
}

fn override_pending(connection: &Connection) -> Value {
    let raw: String = connection
        .query_row(
            "SELECT payload FROM pending_matches WHERE guild_id=?1 AND pending_match_id=604",
            [GUILD_A],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn null_manual_override_does_not_replace_an_unlaunched_session() {
    let (_file, repository, mut session) = manual_override_fixture();
    session.phase = DotaSessionPhase::NeedsReview;
    session.valve_match_id = None;
    session.payload["launch_requested_at"] = Value::Null;
    let session = repository.update(&session, session.revision, 21).unwrap();
    assert!(
        !repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 250)
            .unwrap()
    );
    assert_eq!(repository.session(GUILD_A, 604).unwrap().unwrap(), session);
}

#[test]
fn manual_override_reserves_replacement_before_core_recording() {
    let (file, repository, _) = manual_override_fixture();
    let mut other = claim_created(&repository, GUILD_A, 605, "second-host", 20);
    other.phase = DotaSessionPhase::Running;
    other.valve_match_id = Some("1001".into());
    let other = repository.update(&other, other.revision, 21).unwrap();
    let connection = open_runtime_connection(file.path()).unwrap();
    connection
        .execute(
            "INSERT INTO pending_matches(pending_match_id,guild_id,payload)
         SELECT 605,guild_id,payload FROM pending_matches WHERE pending_match_id=604",
            [],
        )
        .unwrap();
    let original_pending: String = connection
        .query_row(
            "SELECT payload FROM pending_matches WHERE pending_match_id=605",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 250)
            .unwrap()
    );
    assert!(matches!(
        repository.replace_hosted_match_for_manual_record(GUILD_A,605,2000,99,251),
        Err(DotaSessionRepositoryError::InvalidManualOverride(message))
            if message.contains("reserved by another pending match")
    ));
    assert_eq!(repository.session(GUILD_A, 605).unwrap().unwrap(), other);
    let after_pending: String = connection
        .query_row(
            "SELECT payload FROM pending_matches WHERE pending_match_id=605",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after_pending, original_pending);
    let committed: i64 = connection
        .query_row("SELECT COUNT(*) FROM matches", [], |row| row.get(0))
        .unwrap();
    assert_eq!(committed, 0);
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 252)
            .unwrap()
    );
}

#[test]
fn manual_override_quarantines_old_game_preserving_roster_money_and_deadlines() {
    let (file, repository, original) = manual_override_fixture();
    let connection = open_runtime_connection(file.path()).unwrap();
    let original_pending = override_pending(&connection);
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 250)
            .unwrap()
    );
    let updated = repository.session(GUILD_A, 604).unwrap().unwrap();
    assert_eq!(updated.phase, DotaSessionPhase::NeedsReview);
    assert_eq!(updated.lobby_id, original.lobby_id);
    assert_eq!(updated.valve_match_id, original.valve_match_id);
    assert_eq!(updated.account_key, original.account_key);
    assert_eq!(updated.revision, original.revision + 1);
    assert_eq!(updated.payload["roster"], original.payload["roster"]);
    assert_eq!(updated.payload["opaque"], original.payload["opaque"]);
    assert_eq!(updated.payload["settings"], original.payload["settings"]);
    assert_eq!(updated.payload["launch_requested_at"], json!(15));
    for key in ["resolution", "configuration", "pending_status"] {
        assert!(updated.payload.get(key).is_none());
    }
    for key in [
        "cancel_requested",
        "resume_requested",
        "manual_start_requested",
    ] {
        assert_eq!(updated.payload[key], json!(false));
    }
    let pending = override_pending(&connection);
    assert_eq!(
        pending["manual_record_override"],
        updated.payload["manual_record_override"]
    );
    assert_eq!(
        pending["manual_record_override"],
        json!({"replacement_match_id":2000,
        "previous_valve_match_id":"1000","actor_id":99,"requested_at":250})
    );
    assert_eq!(pending["dota_hosting"]["hosting"], json!("manual"));
    assert_eq!(pending["dota_hosting"]["server_region"], json!(27));
    assert_eq!(pending["dota_betting_suspended"], json!(true));
    assert_eq!(
        pending["dota_betting_control_audit"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for key in [
        "team1_players",
        "team2_players",
        "betting_deadline",
        "dota_betting_extended_until",
        "dota_betting_closed",
        "dota_betting_closed_at",
        "seed_reservations",
        "opaque",
    ] {
        assert_eq!(pending[key], original_pending[key], "{key}");
    }
    for key in [
        "dota_host_account_key",
        "dota_hosted_betting",
        "dota_hosted_betting_started_at",
        "dota_hosted_betting_observed_at",
    ] {
        assert!(pending.get(key).is_none());
    }
    let bet: (i64, Option<i64>, Option<i64>) = connection
        .query_row(
            "SELECT amount,payout,match_id FROM bets WHERE guild_id=?1 AND pending_match_id=604",
            [GUILD_A],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(bet, (100, None, None));
    assert!(matches!(
        repository
            .claim_session(GUILD_B, 605, ACCOUNT, json!({}), 251)
            .unwrap(),
        DotaSessionClaim::Busy(_)
    ));
    assert!(matches!(
        repository.update(&original, original.revision, 252),
        Err(DotaSessionRepositoryError::StaleRevision { .. })
    ));
}

#[test]
fn manual_override_retries_without_mutation_even_after_pending_consumption() {
    let (file, repository, _) = manual_override_fixture();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 250)
            .unwrap()
    );
    let first = repository.session(GUILD_A, 604).unwrap().unwrap();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 100, 300)
            .unwrap()
    );
    assert_eq!(repository.session(GUILD_A, 604).unwrap().unwrap(), first);
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 3000, 100, 300)
            .is_err()
    );
    let connection = open_runtime_connection(file.path()).unwrap();
    connection.execute("INSERT INTO matches(guild_id,pending_match_id,valve_match_id,team1_players,team2_players)
        VALUES (?1,604,NULL,'[11]','[22]')",[GUILD_A]).unwrap();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 100, 350)
            .unwrap()
    );
    assert_eq!(repository.session(GUILD_A, 604).unwrap().unwrap(), first);
    connection
        .execute(
            "UPDATE matches SET valve_match_id=2000 WHERE guild_id=?1",
            [GUILD_A],
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM pending_matches WHERE guild_id=?1 AND pending_match_id=604",
            [GUILD_A],
        )
        .unwrap();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 100, 400)
            .unwrap()
    );
    assert_eq!(repository.session(GUILD_A, 604).unwrap().unwrap(), first);
    connection
        .execute(
            "UPDATE matches SET valve_match_id=3000 WHERE guild_id=?1",
            [GUILD_A],
        )
        .unwrap();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 100, 400)
            .is_err()
    );
    assert_eq!(repository.session(GUILD_A, 604).unwrap().unwrap(), first);
}

#[test]
fn manual_override_validates_guild_setup_committed_matches_and_terminal_state() {
    let (file, repository, original) = manual_override_fixture();
    let connection = open_runtime_connection(file.path()).unwrap();
    let pending = override_pending(&connection);
    for (guild, pending_id, valve_id, actor) in [
        (0, 604, 2000, 99),
        (GUILD_A, 0, 2000, 99),
        (GUILD_A, 604, 0, 99),
        (GUILD_A, 604, 2000, 0),
    ] {
        assert!(
            repository
                .replace_hosted_match_for_manual_record(guild, pending_id, valve_id, actor, 250)
                .is_err()
        );
        assert_eq!(repository.session(GUILD_A, 604).unwrap().unwrap(), original);
        assert_eq!(override_pending(&connection), pending);
    }
    assert!(
        !repository
            .replace_hosted_match_for_manual_record(GUILD_B, 604, 2000, 99, 250)
            .unwrap()
    );
    assert!(
        !repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 1000, 99, 250)
            .unwrap()
    );
    for flag in ["shuffle_setup_complete", "draft_setup_complete"] {
        let mut incomplete = pending.clone();
        incomplete[flag] = json!(false);
        connection
            .execute(
                "UPDATE pending_matches SET payload=?1 WHERE pending_match_id=604",
                [incomplete.to_string()],
            )
            .unwrap();
        assert!(matches!(
            repository.replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 250),
            Err(DotaSessionRepositoryError::PendingMatch(
                PendingMatchRepositoryError::SetupIncomplete(604)
            ))
        ));
        assert_eq!(repository.session(GUILD_A, 604).unwrap().unwrap(), original);
    }
    connection
        .execute(
            "UPDATE pending_matches SET payload=?1 WHERE pending_match_id=604",
            [pending.to_string()],
        )
        .unwrap();
    connection.execute("INSERT INTO matches(guild_id,pending_match_id,valve_match_id,team1_players,team2_players)
        VALUES (?1,604,1000,'[11]','[22]')",[GUILD_A]).unwrap();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 250)
            .is_err()
    );
    connection
        .execute(
            "UPDATE matches SET pending_match_id=605,valve_match_id=2000",
            [],
        )
        .unwrap();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 250)
            .is_err()
    );
    connection.execute("DELETE FROM matches", []).unwrap();
    let terminal = repository
        .transition_phase(
            &original,
            DotaSessionPhase::Recorded,
            original.revision,
            250,
        )
        .unwrap();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 251)
            .is_err()
    );
    assert_eq!(repository.session(GUILD_A, 604).unwrap().unwrap(), terminal);
    assert_eq!(override_pending(&connection), pending);
}

#[test]
fn manual_override_rolls_back_both_rows_when_pending_write_fails() {
    let (file, repository, original) = manual_override_fixture();
    let connection = open_runtime_connection(file.path()).unwrap();
    let pending = override_pending(&connection);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_pending_override BEFORE UPDATE ON pending_matches
        BEGIN SELECT RAISE(ABORT,'simulated pending write failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        repository.replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 250),
        Err(DotaSessionRepositoryError::Sqlite(_))
    ));
    assert_eq!(repository.session(GUILD_A, 604).unwrap().unwrap(), original);
    assert_eq!(override_pending(&connection), pending);
}

#[test]
fn manual_override_handles_launched_game_without_assigned_valve_id() {
    let (_file, repository, mut original) = manual_override_fixture();
    original.valve_match_id = None;
    original.phase = DotaSessionPhase::Launching;
    let original = repository.update(&original, original.revision, 21).unwrap();
    assert!(
        repository
            .replace_hosted_match_for_manual_record(GUILD_A, 604, 2000, 99, 250)
            .unwrap()
    );
    let updated = repository.session(GUILD_A, 604).unwrap().unwrap();
    assert_eq!(updated.valve_match_id, None);
    assert_eq!(updated.lobby_id, original.lobby_id);
    assert_eq!(
        updated.payload["manual_record_override"]["previous_valve_match_id"],
        Value::Null
    );
}

#[test]
fn guild_history_filters_before_limit_and_orders_only_its_own_rows() {
    let (_file, repository) = fixture();
    claim_created(&repository, GUILD_A, 1, "host-a1", 10);
    claim_created(&repository, GUILD_A, 2, "host-a2", 20);
    for pending_id in 1..=5 {
        claim_created(
            &repository,
            GUILD_B,
            pending_id,
            &format!("host-b{pending_id}"),
            100 + pending_id,
        );
    }
    let recent = repository.recent_sessions_for_guild(GUILD_A, 1).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(
        (recent[0].guild_id, recent[0].pending_match_id),
        (GUILD_A, 2)
    );
    let recent = repository.recent_sessions_for_guild(GUILD_A, 10).unwrap();
    assert_eq!(
        recent
            .iter()
            .map(|row| row.pending_match_id)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert!(
        repository
            .recent_sessions_for_guild(999, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .recent_sessions_for_guild(GUILD_A, 0)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        repository.recent_sessions_for_guild(GUILD_A, 1001),
        Err(DotaSessionRepositoryError::InvalidHistoryLimit(1001))
    ));
}

fn fixture() -> (NamedTempFile, DotaSessionRepository) {
    let file = NamedTempFile::new().expect("temporary database");
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute_batch(
            "CREATE TABLE dota_sessions (
                guild_id INTEGER NOT NULL,
                pending_match_id INTEGER NOT NULL,
                account_key TEXT NOT NULL,
                phase TEXT NOT NULL,
                lobby_id TEXT,
                valve_match_id TEXT,
                payload TEXT NOT NULL,
                revision INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                last_error TEXT,
                PRIMARY KEY (guild_id, pending_match_id)
             );
             CREATE UNIQUE INDEX uq_dota_sessions_active_account
                 ON dota_sessions(account_key)
                 WHERE phase IN ('creating','gathering','launching','running',
                                 'finishing','needs_review');
             CREATE UNIQUE INDEX uq_dota_sessions_valve_match
                 ON dota_sessions(valve_match_id)
                 WHERE valve_match_id IS NOT NULL;",
        )
        .expect("create fixture schema");
    drop(connection);
    let path = file.path().to_owned();
    (file, DotaSessionRepository::new(path))
}

fn claim_created(
    repository: &DotaSessionRepository,
    guild_id: i64,
    pending_match_id: i64,
    account_key: &str,
    now: i64,
) -> DotaSessionRecord {
    match repository
        .claim_session(
            guild_id,
            pending_match_id,
            account_key,
            json!({"step": "create"}),
            now,
        )
        .expect("claim session")
    {
        DotaSessionClaim::Created(record) => record,
        other => panic!("expected created session, got {other:?}"),
    }
}

#[test]
fn phase_storage_names_and_lease_policy_are_stable() {
    let expected = [
        (DotaSessionPhase::Creating, "creating", true),
        (DotaSessionPhase::Gathering, "gathering", true),
        (DotaSessionPhase::Launching, "launching", true),
        (DotaSessionPhase::Running, "running", true),
        (DotaSessionPhase::Finishing, "finishing", true),
        (DotaSessionPhase::Recorded, "recorded", false),
        (DotaSessionPhase::Cancelled, "cancelled", false),
        (DotaSessionPhase::Failed, "failed", false),
        (DotaSessionPhase::NeedsReview, "needs_review", true),
    ];
    for (phase, name, active) in expected {
        assert_eq!(phase.as_str(), name);
        assert_eq!(DotaSessionPhase::parse(name), Some(phase));
        assert_eq!(phase.is_active(), active);
    }
    assert_eq!(DotaSessionPhase::parse("unknown"), None);
}

#[test]
fn claim_is_idempotent_and_busy_is_scoped_to_the_host_account() {
    let (_file, repository) = fixture();
    let created = claim_created(&repository, GUILD_A, 100, ACCOUNT, 10);

    let existing = repository
        .claim_session(GUILD_A, 100, ACCOUNT, json!({"new": true}), 11)
        .expect("retry claim");
    assert_eq!(existing, DotaSessionClaim::Existing(created.clone()));

    let busy = repository
        .claim_session(GUILD_B, 200, ACCOUNT, json!({}), 12)
        .expect("cross-guild claim");
    assert_eq!(busy, DotaSessionClaim::Busy(created.clone()));

    let other = claim_created(&repository, GUILD_B, 200, "steam-bot-secondary", 13);
    assert_eq!(other.guild_id, GUILD_B);
    assert_eq!(
        repository.active_sessions().expect("active sessions").len(),
        2
    );
}

#[test]
fn terminal_failed_row_releases_account_but_needs_review_keeps_it_leased() {
    let (_file, repository) = fixture();
    let created = claim_created(&repository, GUILD_A, 100, ACCOUNT, 10);
    let failed = repository
        .transition_phase(&created, DotaSessionPhase::Failed, created.revision, 11)
        .expect("mark known failure");
    let next = claim_created(&repository, GUILD_B, 200, ACCOUNT, 12);
    assert_eq!(next.account_key, ACCOUNT);

    let review = repository
        .transition_phase(&next, DotaSessionPhase::NeedsReview, next.revision, 13)
        .expect("mark review");
    let busy = repository
        .claim_session(GUILD_A, 300, ACCOUNT, json!({}), 14)
        .expect("claim while review is active");
    assert_eq!(busy, DotaSessionClaim::Busy(review));
    assert_eq!(failed.phase, DotaSessionPhase::Failed);
}

#[test]
fn optimistic_update_and_id_association_are_durable_after_pending_cleanup() {
    let (file, repository) = fixture();
    let created = claim_created(&repository, GUILD_A, 100, ACCOUNT, 10);

    let mut gathering = created.clone();
    gathering.phase = DotaSessionPhase::Gathering;
    gathering.payload = json!({"step": "gather", "players": [1, 2]});
    let gathering = repository
        .update(&gathering, created.revision, 20)
        .expect("update gathering");
    assert_eq!(gathering.revision, created.revision + 1);
    assert_eq!(gathering.updated_at, 20);

    let lobby = repository
        .attach_lobby_id(&gathering, "lobby-42", gathering.revision, 21)
        .expect("attach lobby");
    let running = repository
        .transition_phase(&lobby, DotaSessionPhase::Running, lobby.revision, 22)
        .expect("mark running");
    let recorded = repository
        .attach_valve_match_id(&running, "987654321", running.revision, 23)
        .expect("attach Valve match");
    let recorded = repository
        .transition_phase(&recorded, DotaSessionPhase::Recorded, recorded.revision, 24)
        .expect("mark recorded");

    Connection::open(file.path())
        .expect("open fixture")
        .execute(
            "CREATE TABLE pending_matches(pending_match_id INTEGER PRIMARY KEY)",
            [],
        )
        .expect("create pending fixture");
    Connection::open(file.path())
        .expect("open fixture")
        .execute(
            "DELETE FROM pending_matches WHERE pending_match_id=?1",
            [100],
        )
        .expect("pending cleanup no-op");

    let loaded = repository
        .session(GUILD_A, 100)
        .expect("reload recorded session")
        .expect("record remains after pending cleanup");
    assert_eq!(loaded, recorded);
    assert_eq!(
        repository
            .session_by_valve_match_id("987654321")
            .expect("lookup Valve ID")
            .expect("Valve ID exists")
            .pending_match_id,
        100
    );
}

#[test]
fn stale_update_and_duplicate_valve_ids_fail_closed() {
    let (_file, repository) = fixture();
    let first = claim_created(&repository, GUILD_A, 100, ACCOUNT, 10);
    let second = claim_created(&repository, GUILD_B, 200, "steam-bot-secondary", 10);

    let mut first_next = first.clone();
    first_next.phase = DotaSessionPhase::Gathering;
    let first_next = repository
        .update(&first_next, first.revision, 11)
        .expect("update first");
    let mut stale = first_next.clone();
    stale.phase = DotaSessionPhase::Launching;
    assert!(matches!(
        repository.update(&stale, first.revision, 12),
        Err(DotaSessionRepositoryError::StaleRevision {
            expected: 0,
            actual: 1,
            ..
        })
    ));

    let first_with_valve = repository
        .attach_valve_match_id(&first_next, "123", first_next.revision, 13)
        .expect("attach first Valve ID");
    let mut duplicate = second.clone();
    duplicate.valve_match_id = Some("123".to_owned());
    assert!(matches!(
        repository.update(&duplicate, second.revision, 14),
        Err(DotaSessionRepositoryError::ValveMatchAlreadyAssociated {
            valve_match_id,
            guild_id: GUILD_A,
            pending_match_id: 100,
        }) if valve_match_id == "123"
    ));
    assert_eq!(first_with_valve.valve_match_id.as_deref(), Some("123"));
}

#[test]
fn concurrent_claims_allow_only_one_active_host_lease() {
    let (_file, repository) = fixture();
    let barrier = Arc::new(Barrier::new(3));
    let workers = [(GUILD_A, 100), (GUILD_B, 200)]
        .into_iter()
        .map(|(guild_id, pending_match_id)| {
            let repository = repository.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository.claim_session(guild_id, pending_match_id, ACCOUNT, json!({}), 10)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().expect("join claim worker").expect("claim"))
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, DotaSessionClaim::Created(_)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, DotaSessionClaim::Busy(_)))
            .count(),
        1
    );
    assert_eq!(
        repository.active_sessions().expect("active sessions").len(),
        1
    );
}

#[test]
fn invalid_account_key_is_rejected_before_sqlite_write() {
    let (_file, repository) = fixture();
    assert!(matches!(
        repository.claim_session(GUILD_A, 100, "  ", json!({}), 10),
        Err(DotaSessionRepositoryError::EmptyAccountKey)
    ));
}

#[test]
fn phase_serialization_matches_the_payload_contract() {
    let encoded = serde_json::to_string(&DotaSessionPhase::NeedsReview).expect("serialize phase");
    assert_eq!(encoded, "\"needs_review\"");
    assert_eq!(
        serde_json::from_str::<DotaSessionPhase>(&encoded).expect("deserialize phase"),
        DotaSessionPhase::NeedsReview
    );
}

#[test]
fn active_sessions_include_review_and_exclude_terminal_rows() {
    let (_file, repository) = fixture();
    let created = claim_created(&repository, GUILD_A, 100, ACCOUNT, 10);
    let review = repository
        .transition_phase(
            &created,
            DotaSessionPhase::NeedsReview,
            created.revision,
            11,
        )
        .expect("needs review");
    let other = claim_created(&repository, GUILD_B, 200, "steam-bot-secondary", 12);
    repository
        .transition_phase(&other, DotaSessionPhase::Cancelled, other.revision, 13)
        .expect("cancel other");
    assert_eq!(
        repository.active_sessions().expect("active sessions"),
        [review]
    );
}

#[test]
fn recent_sessions_are_newest_first_and_bounded() {
    let (_file, repository) = fixture();
    let first = claim_created(&repository, GUILD_A, 100, ACCOUNT, 10);
    let first = repository
        .transition_phase(&first, DotaSessionPhase::Failed, first.revision, 30)
        .expect("fail first");
    let second = claim_created(&repository, GUILD_B, 200, "steam-bot-secondary", 20);
    let _second = repository
        .transition_phase(&second, DotaSessionPhase::Recorded, second.revision, 40)
        .expect("record second");

    assert_eq!(
        repository
            .recent_sessions(1)
            .expect("recent session")
            .first()
            .expect("one row")
            .pending_match_id,
        200
    );
    assert_eq!(repository.recent_sessions(0).expect("zero rows"), []);
    assert!(matches!(
        repository.recent_sessions(1_001),
        Err(DotaSessionRepositoryError::InvalidHistoryLimit(1_001))
    ));
    assert_eq!(first.phase, DotaSessionPhase::Failed);
}

#[test]
fn sqlite_unique_valve_index_is_retained_by_the_fixture_contract() {
    let (_file, repository) = fixture();
    let first = claim_created(&repository, GUILD_A, 100, ACCOUNT, 10);
    let first = repository
        .attach_valve_match_id(&first, "1", first.revision, 11)
        .expect("attach Valve ID");
    let second = claim_created(&repository, GUILD_B, 200, "steam-bot-secondary", 12);
    let mut duplicate = second.clone();
    duplicate.valve_match_id = Some("1".to_owned());
    let error = repository.update(&duplicate, second.revision, 13);
    assert!(matches!(
        error,
        Err(DotaSessionRepositoryError::ValveMatchAlreadyAssociated { .. })
    ));
    assert_eq!(first.valve_match_id.as_deref(), Some("1"));
    let raw_count: i64 = Connection::open(repository.path)
        .expect("open fixture")
        .query_row(
            "SELECT COUNT(*) FROM dota_sessions WHERE valve_match_id='1'",
            [],
            |row| row.get(0),
        )
        .expect("count Valve IDs");
    assert_eq!(raw_count, 1);
}

#[test]
fn terminal_status_outbox_is_account_scoped_and_retains_old_undelivered_rows() {
    let (_file, repository) = fixture();
    for (id, account, phase, notice, channel) in [
        (
            1,
            ACCOUNT,
            DotaSessionPhase::Cancelled,
            json!("retry"),
            json!(55),
        ),
        (
            2,
            ACCOUNT,
            DotaSessionPhase::Recorded,
            json!(null),
            json!(55),
        ),
        (
            3,
            "other",
            DotaSessionPhase::Cancelled,
            json!("private"),
            json!(55),
        ),
        (
            4,
            ACCOUNT,
            DotaSessionPhase::Cancelled,
            json!("no destination"),
            json!(null),
        ),
        (
            5,
            ACCOUNT,
            DotaSessionPhase::Gathering,
            json!("active"),
            json!(55),
        ),
    ] {
        let mut row = claim_created(&repository, GUILD_A, id, account, id);
        row.phase = phase;
        row.payload = json!({"pending_status":notice,"channel_id":channel});
        repository.update(&row, row.revision, id).unwrap();
    }
    let rows = repository.terminal_status_pending(ACCOUNT).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pending_match_id, 1);
    let mut delivered = rows[0].clone();
    delivered.payload["pending_status"] = json!(null);
    repository
        .update(&delivered, delivered.revision, 100)
        .unwrap();
    assert!(
        repository
            .terminal_status_pending(ACCOUNT)
            .unwrap()
            .is_empty()
    );
}
