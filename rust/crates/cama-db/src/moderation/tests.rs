use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const NOW: i64 = 2_000_000_000;
const GUILD: i64 = 98_765;
const OTHER_GUILD: i64 = 98_766;

struct Fixture {
    database: NamedTempFile,
    repository: ModerationRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        let connection = Connection::open(database.path()).expect("fixture connection");
        connection
            .execute_batch(FIXTURE_SCHEMA)
            .expect("fixture-only moderation schema");
        drop(connection);
        let repository = ModerationRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("fixture connection")
    }

    fn seed_pending(&self, guild_id: i64) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO pending_matches (guild_id,payload) VALUES (?1,'{}')",
                params![guild_id],
            )
            .expect("seed pending match");
        connection.last_insert_rowid()
    }

    fn seed_recorded_pending(&self, guild_id: i64, pending_match_id: i64, kind: &str) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO matches (guild_id,pending_match_id,lobby_kind)
                 VALUES (?1,?2,?3)",
                params![guild_id, pending_match_id, kind],
            )
            .expect("seed recorded match");
        connection.last_insert_rowid()
    }
}

fn save_request(
    discord_id: i64,
    guild_id: Option<i64>,
    completion: SuspensionCompletion,
    expires_at: Option<i64>,
    matches_required: Option<i64>,
) -> SaveSuspensionRequest<'static> {
    SaveSuspensionRequest {
        discord_id,
        guild_id,
        scope: SuspensionScope::All,
        completion,
        expires_at,
        matches_required,
        pending_match_watermark: None,
        reason: "Repeated no-shows",
        source: ModerationSource::Admin,
        actor_id: 900,
        replace: false,
        now: NOW,
    }
}

fn saved(outcome: SaveSuspensionOutcome) -> LobbySuspension {
    let SaveSuspensionOutcome::Saved(state) = outcome else {
        panic!("expected a saved suspension");
    };
    state
}

#[test]
fn migrated_schema_contract_and_legacy_low_priority_progress_are_read_only() {
    let fixture = Fixture::new();
    fixture
        .connection()
        .execute(
            "INSERT INTO low_priority_state (
                 discord_id,guild_id,wins_required,wins_remaining,
                 start_pending_match_id,active,reason,set_by
             ) VALUES (101,77,3,2,NULL,1,'legacy',900)",
            [],
        )
        .expect("seed migrated low-priority row");

    let contract = fixture.repository.schema_contract().expect("contract");
    assert!(contract.has_lobby_suspensions);
    assert!(contract.has_moderation_events);
    assert!(contract.matches_has_lobby_kind);
    assert!(contract.append_only_update_trigger);
    assert!(contract.append_only_delete_trigger);
    assert!(
        [
            "scope",
            "completion",
            "expires_at",
            "matches_required",
            "matches_remaining",
            "pending_match_watermark",
            "revision",
            "active",
        ]
        .iter()
        .all(|column| contract.suspension_columns.contains(*column))
    );
    assert!(
        [
            "matches_remaining",
            "wins_required",
            "wins_remaining",
            "match_id"
        ]
        .iter()
        .all(|column| contract.event_columns.contains(*column))
    );

    assert_eq!(
        fixture
            .repository
            .low_priority_migration_state(101, Some(77))
            .expect("read low-priority state"),
        Some(LowPriorityMigrationState {
            wins_required: 3,
            wins_remaining: 2,
            start_pending_match_id: None,
            reason: Some("legacy".to_owned()),
        })
    );
}

#[test]
fn save_captures_guild_watermark_and_writes_a_matching_audit_event() {
    let fixture = Fixture::new();
    let pending = fixture.seed_pending(GUILD);
    let other_pending = fixture.seed_pending(OTHER_GUILD);
    let recorded_pending = other_pending + 10;
    fixture.seed_recorded_pending(GUILD, recorded_pending, "open");
    fixture.seed_recorded_pending(OTHER_GUILD, recorded_pending + 50, "open");

    let mut request = save_request(
        -7_000_000_000_001,
        Some(GUILD),
        SuspensionCompletion::Matches,
        None,
        Some(2),
    );
    request.scope = SuspensionScope::Open;
    let state = saved(fixture.repository.save_suspension(request).expect("save"));
    assert_eq!(state.pending_match_watermark, recorded_pending.max(pending));
    assert_eq!(state.matches_remaining, Some(2));
    assert_eq!(state.revision, 1);
    assert_eq!(
        fixture
            .repository
            .pending_match_watermark(Some(GUILD))
            .expect("watermark"),
        recorded_pending
    );

    let history = fixture
        .repository
        .history(Some(GUILD), Some(state.discord_id), 50, 0)
        .expect("history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].event_type, ModerationEventType::Suspend);
    assert_eq!(history[0].pending_match_watermark, Some(recorded_pending));
    assert_eq!(history[0].matches_remaining, Some(2));
}

#[test]
fn duplicate_is_rejected_and_replace_increments_revision_atomically() {
    let fixture = Fixture::new();
    let original = saved(
        fixture
            .repository
            .save_suspension(save_request(
                301,
                Some(GUILD),
                SuspensionCompletion::Time,
                Some(NOW + 300),
                None,
            ))
            .expect("save original"),
    );
    let duplicate = fixture
        .repository
        .save_suspension(save_request(
            301,
            Some(GUILD),
            SuspensionCompletion::Time,
            Some(NOW + 600),
            None,
        ))
        .expect("duplicate outcome");
    assert_eq!(
        duplicate,
        SaveSuspensionOutcome::ActiveExists(original.clone())
    );

    let mut replacement = save_request(
        301,
        Some(GUILD),
        SuspensionCompletion::Matches,
        None,
        Some(3),
    );
    replacement.reason = "Escalated";
    replacement.actor_id = 901;
    replacement.replace = true;
    let replaced = saved(
        fixture
            .repository
            .save_suspension(replacement)
            .expect("replace"),
    );
    assert_eq!(replaced.revision, original.revision + 1);
    assert_eq!(replaced.matches_remaining, Some(3));
    assert_eq!(replaced.reason, "Escalated");
    assert_eq!(
        fixture
            .repository
            .history(Some(GUILD), Some(301), 50, 0)
            .expect("history")
            .into_iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>(),
        vec![ModerationEventType::Replace, ModerationEventType::Suspend]
    );
}

#[test]
fn stale_close_cannot_close_replacement_or_append_completion() {
    let fixture = Fixture::new();
    let original = saved(
        fixture
            .repository
            .save_suspension(save_request(
                302,
                Some(GUILD),
                SuspensionCompletion::Time,
                Some(NOW + 300),
                None,
            ))
            .expect("save original"),
    );
    let mut replacement = save_request(
        302,
        Some(GUILD),
        SuspensionCompletion::Time,
        Some(NOW + 600),
        None,
    );
    replacement.replace = true;
    replacement.reason = "Replacement";
    let replaced = saved(
        fixture
            .repository
            .save_suspension(replacement)
            .expect("replace"),
    );

    assert_eq!(
        fixture
            .repository
            .close_suspension(CloseSuspensionRequest {
                discord_id: 302,
                guild_id: Some(GUILD),
                event_type: ModerationEventType::Complete,
                source: ModerationSource::System,
                actor_id: None,
                reason: Some("stale natural completion"),
                expected_revision: original.revision,
                now: NOW + 300,
            })
            .expect("stale close"),
        CloseSuspensionOutcome::ConflictOrMissing
    );
    assert_eq!(
        fixture
            .repository
            .suspension(302, Some(GUILD))
            .expect("current state"),
        Some(replaced)
    );
    assert_eq!(
        fixture
            .repository
            .history(Some(GUILD), Some(302), 50, 0)
            .expect("history")
            .len(),
        2
    );
}

#[test]
fn completed_match_progress_honors_watermark_scope_and_completion_policy() {
    let fixture = Fixture::new();
    let grandfathered = fixture.seed_pending(GUILD);
    let mut request = save_request(
        102,
        Some(GUILD),
        SuspensionCompletion::Matches,
        None,
        Some(1),
    );
    request.scope = SuspensionScope::Open;
    let created = saved(fixture.repository.save_suspension(request).expect("save"));
    assert_eq!(created.pending_match_watermark, grandfathered);

    let ignored = fixture
        .repository
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: grandfathered,
            lobby_kind: "open",
            match_id: 1,
            recorded_at: NOW,
        })
        .expect("grandfathered progress");
    assert!(ignored.progressed_discord_ids.is_empty());

    let lowskill = fixture.seed_pending(GUILD);
    let ignored = fixture
        .repository
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: lowskill,
            lobby_kind: "lowskill",
            match_id: 2,
            recorded_at: NOW,
        })
        .expect("out-of-scope progress");
    assert!(ignored.progressed_discord_ids.is_empty());

    let open = fixture.seed_pending(GUILD);
    let completed = fixture
        .repository
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: open,
            lobby_kind: "open",
            match_id: 77,
            recorded_at: NOW,
        })
        .expect("in-scope progress");
    assert_eq!(completed.progressed_discord_ids, vec![102]);
    assert_eq!(completed.completed_events.len(), 1);
    assert_eq!(completed.completed_events[0].match_id, Some(77));
    assert_eq!(
        fixture
            .repository
            .suspension(102, Some(GUILD))
            .expect("state")
            .expect("suspension")
            .matches_remaining,
        Some(0)
    );
    let retry = fixture
        .repository
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: open,
            lobby_kind: "open",
            match_id: 77,
            recorded_at: NOW,
        })
        .expect("completed-match retry");
    assert!(retry.progressed_discord_ids.is_empty());
    assert_eq!(
        fixture
            .repository
            .history(Some(GUILD), Some(102), 50, 0)
            .expect("completion history")
            .into_iter()
            .filter(|event| event.event_type == ModerationEventType::Complete)
            .count(),
        1
    );
}

#[test]
fn either_and_both_match_completion_semantics_match_time_state() {
    let fixture = Fixture::new();
    let either = save_request(
        201,
        Some(GUILD),
        SuspensionCompletion::Either,
        Some(NOW + 300),
        Some(1),
    );
    let both = save_request(
        202,
        Some(GUILD),
        SuspensionCompletion::Both,
        Some(NOW + 300),
        Some(1),
    );
    fixture
        .repository
        .save_suspension(either)
        .expect("save either");
    fixture.repository.save_suspension(both).expect("save both");

    let pending = fixture.seed_pending(GUILD);
    let before_expiry = fixture
        .repository
        .progress_completed_match(CompletedMatch {
            guild_id: Some(GUILD),
            pending_match_id: pending,
            lobby_kind: "open",
            match_id: 81,
            recorded_at: NOW + 299,
        })
        .expect("progress before expiry");
    assert_eq!(before_expiry.progressed_discord_ids, vec![201, 202]);
    assert_eq!(before_expiry.completed_events.len(), 1);
    assert_eq!(before_expiry.completed_events[0].discord_id, 201);
    assert!(
        fixture
            .repository
            .suspension(202, Some(GUILD))
            .expect("both state")
            .expect("both suspension")
            .active
    );

    let close = fixture
        .repository
        .suspension(202, Some(GUILD))
        .expect("both state")
        .expect("both suspension");
    assert!(!close.is_active_at(NOW + 300));
}

#[test]
fn guild_default_and_signed_ids_are_isolated_for_state_and_history() {
    let fixture = Fixture::new();
    let signed = -9_223_372_036_854_000_000;
    fixture
        .repository
        .save_suspension(save_request(
            signed,
            None,
            SuspensionCompletion::Time,
            Some(NOW + 300),
            None,
        ))
        .expect("default-guild save");
    fixture
        .repository
        .save_suspension(save_request(
            signed,
            Some(GUILD),
            SuspensionCompletion::Time,
            Some(NOW + 300),
            None,
        ))
        .expect("guild save");

    assert_eq!(
        fixture
            .repository
            .list_suspensions(None, true)
            .expect("default-guild list")
            .into_iter()
            .map(|state| state.guild_id)
            .collect::<Vec<_>>(),
        vec![0]
    );
    assert_eq!(
        fixture
            .repository
            .history(None, Some(signed), 50, 0)
            .expect("default-guild history")
            .len(),
        1
    );
    assert_eq!(
        fixture
            .repository
            .history(Some(OTHER_GUILD), Some(signed), 50, 0)
            .expect("other-guild history"),
        Vec::new()
    );
}

#[test]
fn kick_and_low_priority_history_is_newest_first_and_guild_scoped() {
    let fixture = Fixture::new();
    let kick = fixture
        .repository
        .record_event(RecordModerationEventRequest {
            discord_id: 403,
            guild_id: Some(GUILD),
            event_type: ModerationEventType::Kick,
            source: ModerationSource::Creator,
            actor_id: Some(901),
            reason: Some("Removed from current lobby"),
            scope: Some(SuspensionScope::Open),
            completion: None,
            expires_at: None,
            matches_required: None,
            matches_remaining: None,
            pending_match_watermark: None,
            wins_required: None,
            wins_remaining: None,
            match_id: None,
            now: NOW,
        })
        .expect("kick event");
    let lowprio = fixture
        .repository
        .record_event(RecordModerationEventRequest {
            discord_id: 404,
            guild_id: Some(GUILD),
            event_type: ModerationEventType::LowprioComplete,
            source: ModerationSource::System,
            actor_id: None,
            reason: Some("Required wins completed"),
            scope: None,
            completion: None,
            expires_at: None,
            matches_required: None,
            matches_remaining: None,
            pending_match_watermark: Some(20),
            wins_required: Some(3),
            wins_remaining: Some(0),
            match_id: Some(77),
            now: NOW + 1,
        })
        .expect("low-priority event");
    fixture
        .repository
        .record_event(RecordModerationEventRequest {
            guild_id: Some(OTHER_GUILD),
            ..RecordModerationEventRequest {
                discord_id: 999,
                guild_id: None,
                event_type: ModerationEventType::Kick,
                source: ModerationSource::Admin,
                actor_id: Some(900),
                reason: None,
                scope: Some(SuspensionScope::Open),
                completion: None,
                expires_at: None,
                matches_required: None,
                matches_remaining: None,
                pending_match_watermark: None,
                wins_required: None,
                wins_remaining: None,
                match_id: None,
                now: NOW,
            }
        })
        .expect("other-guild event");

    let history = fixture
        .repository
        .history(Some(GUILD), None, 50, 0)
        .expect("guild history");
    assert_eq!(history, vec![lowprio.clone(), kick]);
    assert_eq!(history[0].wins_remaining, Some(0));
    assert_eq!(history[0].match_id, Some(77));
    assert_eq!(
        fixture
            .repository
            .history(Some(GUILD), Some(404), 1, 0)
            .expect("target history"),
        vec![lowprio]
    );
}

#[test]
fn moderation_events_are_enforced_as_append_only_by_migrated_triggers() {
    let fixture = Fixture::new();
    let event = fixture
        .repository
        .record_event(RecordModerationEventRequest {
            discord_id: 501,
            guild_id: Some(GUILD),
            event_type: ModerationEventType::Kick,
            source: ModerationSource::Admin,
            actor_id: Some(900),
            reason: None,
            scope: Some(SuspensionScope::Open),
            completion: None,
            expires_at: None,
            matches_required: None,
            matches_remaining: None,
            pending_match_watermark: None,
            wins_required: None,
            wins_remaining: None,
            match_id: None,
            now: NOW,
        })
        .expect("event");
    let connection = fixture.connection();
    assert!(
        connection
            .execute(
                "UPDATE moderation_events SET reason='rewritten' WHERE event_id=?1",
                params![event.event_id],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "DELETE FROM moderation_events WHERE event_id=?1",
                params![event.event_id],
            )
            .is_err()
    );
}

#[test]
fn simultaneous_non_replace_saves_have_one_winner() {
    let fixture = Fixture::new();
    let repository_a = fixture.repository.clone();
    let repository_b = fixture.repository.clone();
    let barrier = Arc::new(Barrier::new(3));
    let barrier_a = Arc::clone(&barrier);
    let barrier_b = Arc::clone(&barrier);
    let worker = |repository: ModerationRepository, barrier: Arc<Barrier>, actor_id| {
        thread::spawn(move || {
            let mut request = save_request(
                601,
                Some(GUILD),
                SuspensionCompletion::Time,
                Some(NOW + 300),
                None,
            );
            request.actor_id = actor_id;
            barrier.wait();
            repository
                .save_suspension(request)
                .expect("concurrent save")
        })
    };
    let first = worker(repository_a, barrier_a, 901);
    let second = worker(repository_b, barrier_b, 902);
    barrier.wait();
    let outcomes = [
        first.join().expect("first worker"),
        second.join().expect("second worker"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, SaveSuspensionOutcome::Saved(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, SaveSuspensionOutcome::ActiveExists(_)))
            .count(),
        1
    );
    assert_eq!(
        fixture
            .repository
            .history(Some(GUILD), Some(601), 50, 0)
            .expect("history")
            .len(),
        1
    );
}

const FIXTURE_SCHEMA: &str = r#"
    PRAGMA journal_mode=WAL;
    PRAGMA busy_timeout=5000;
    CREATE TABLE matches (
        match_id INTEGER PRIMARY KEY AUTOINCREMENT,
        guild_id INTEGER NOT NULL DEFAULT 0,
        pending_match_id INTEGER,
        lobby_kind TEXT CHECK(lobby_kind IS NULL OR lobby_kind IN ('open','lowskill'))
    );
    CREATE TABLE pending_matches (
        pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
        guild_id INTEGER NOT NULL,
        payload TEXT NOT NULL,
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
        updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
    );
    CREATE TABLE lobby_suspensions (
        guild_id INTEGER NOT NULL DEFAULT 0,
        discord_id INTEGER NOT NULL,
        scope TEXT NOT NULL CHECK(scope IN ('all','open','lowskill')),
        completion TEXT NOT NULL CHECK(completion IN ('time','matches','either','both')),
        expires_at INTEGER,
        matches_required INTEGER CHECK(matches_required > 0),
        matches_remaining INTEGER CHECK(matches_remaining >= 0),
        pending_match_watermark INTEGER NOT NULL DEFAULT 0 CHECK(pending_match_watermark >= 0),
        reason TEXT NOT NULL CHECK(length(trim(reason)) > 0),
        source TEXT NOT NULL CHECK(source IN ('admin','vote','creator','system')),
        actor_id INTEGER NOT NULL,
        revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
        active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        PRIMARY KEY (guild_id,discord_id),
        CHECK (
            (completion='time' AND expires_at IS NOT NULL AND matches_required IS NULL AND matches_remaining IS NULL)
            OR (completion='matches' AND expires_at IS NULL AND matches_required IS NOT NULL AND matches_remaining IS NOT NULL)
            OR (completion IN ('either','both') AND expires_at IS NOT NULL AND matches_required IS NOT NULL AND matches_remaining IS NOT NULL)
        ),
        CHECK(matches_remaining IS NULL OR matches_remaining <= matches_required)
    );
    CREATE TABLE moderation_events (
        event_id INTEGER PRIMARY KEY AUTOINCREMENT,
        guild_id INTEGER NOT NULL DEFAULT 0,
        discord_id INTEGER NOT NULL,
        event_type TEXT NOT NULL CHECK(event_type IN (
            'suspend','replace','lift','complete','kick',
            'lowprio_assign','lowprio_replace','lowprio_clear','lowprio_complete'
        )),
        source TEXT NOT NULL CHECK(source IN ('admin','vote','creator','system')),
        actor_id INTEGER,
        reason TEXT,
        scope TEXT CHECK(scope IS NULL OR scope IN ('all','open','lowskill')),
        completion TEXT CHECK(completion IS NULL OR completion IN ('time','matches','either','both')),
        expires_at INTEGER,
        matches_required INTEGER CHECK(matches_required > 0),
        matches_remaining INTEGER CHECK(matches_remaining >= 0),
        pending_match_watermark INTEGER CHECK(pending_match_watermark >= 0),
        wins_required INTEGER CHECK(wins_required >= 0),
        wins_remaining INTEGER CHECK(wins_remaining >= 0),
        match_id INTEGER,
        created_at INTEGER NOT NULL
    );
    CREATE TRIGGER moderation_events_no_update
    BEFORE UPDATE ON moderation_events BEGIN
        SELECT RAISE(ABORT,'moderation_events is append-only');
    END;
    CREATE TRIGGER moderation_events_no_delete
    BEFORE DELETE ON moderation_events BEGIN
        SELECT RAISE(ABORT,'moderation_events is append-only');
    END;
    CREATE TABLE low_priority_state (
        discord_id INTEGER NOT NULL,
        guild_id INTEGER NOT NULL DEFAULT 0,
        wins_required INTEGER NOT NULL DEFAULT 3 CHECK(wins_required BETWEEN 1 AND 20),
        wins_remaining INTEGER NOT NULL DEFAULT 3 CHECK(wins_remaining BETWEEN 0 AND 20),
        start_pending_match_id INTEGER CHECK(start_pending_match_id >= 0),
        active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
        reason TEXT,
        reason_player_visible INTEGER NOT NULL DEFAULT 0 CHECK(reason_player_visible IN (0,1)),
        set_by INTEGER NOT NULL,
        removed_by INTEGER,
        removed_reason TEXT,
        created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
        PRIMARY KEY (discord_id,guild_id),
        CHECK(wins_remaining <= wins_required)
    );
"#;
