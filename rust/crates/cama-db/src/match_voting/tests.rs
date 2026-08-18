use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 12_345;
const OTHER_GUILD: i64 = 54_321;
const BASE_PAYLOAD: &str = r#"{
    "radiant_team_ids":[1,2,3,4,5],
    "dire_team_ids":[6,7,8,9,10],
    "excluded_player_ids":[11],
    "excluded_conditional_player_ids":[12],
    "record_submissions":{},
    "shuffle_timestamp":1700000000,
    "lobby_kind":"open"
}"#;

struct Fixture {
    _database: NamedTempFile,
    repository: MatchVotingRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        let connection = Connection::open(database.path()).expect("fixture connection");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE pending_matches (
                     pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL,
                     payload TEXT NOT NULL,
                     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );",
            )
            .expect("fixture schema");
        drop(connection);
        let repository = MatchVotingRepository::new(database.path());
        Self {
            _database: database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self._database.path()).expect("fixture connection")
    }

    fn seed(&self, guild_id: i64, payload: &str) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO pending_matches (guild_id,payload) VALUES (?1,?2)",
                params![guild_id, payload],
            )
            .expect("seed pending match");
        connection.last_insert_rowid()
    }
}

fn explicit_key(guild_id: i64, pending_match_id: i64) -> PendingVoteKey {
    PendingVoteKey {
        guild_id: Some(guild_id),
        pending_match_id: Some(pending_match_id),
    }
}

fn cas<'a>(
    snapshot: &'a PendingVoteSnapshot,
    user_id: i64,
    result: &'a str,
    is_admin: bool,
) -> VoteSubmissionCas<'a> {
    VoteSubmissionCas {
        guild_id: Some(snapshot.guild_id),
        pending_match_id: snapshot.pending_match_id,
        expected_payload: &snapshot.payload,
        user_id,
        result,
        is_admin,
    }
}

#[test]
fn implicit_lookup_requires_exactly_one_match_and_explicit_unknown_is_safe() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .repository
            .snapshot(PendingVoteKey {
                guild_id: Some(GUILD),
                pending_match_id: None,
            })
            .expect("empty lookup"),
        None
    );
    let first = fixture.seed(GUILD, BASE_PAYLOAD);
    assert_eq!(
        fixture
            .repository
            .snapshot(PendingVoteKey {
                guild_id: Some(GUILD),
                pending_match_id: None,
            })
            .expect("single lookup")
            .expect("single match")
            .pending_match_id,
        first
    );
    assert_eq!(
        fixture
            .repository
            .snapshot(explicit_key(GUILD, first + 999))
            .expect("unknown lookup"),
        None
    );
    fixture.seed(GUILD, BASE_PAYLOAD);
    assert_eq!(
        fixture
            .repository
            .snapshot(PendingVoteKey {
                guild_id: Some(GUILD),
                pending_match_id: None,
            })
            .expect("ambiguous lookup"),
        None
    );
}

#[test]
fn snapshot_decodes_teams_exclusions_and_shared_submissions() {
    let fixture = Fixture::new();
    let payload = BASE_PAYLOAD.replace(
        r#""record_submissions":{}"#,
        r#""record_submissions":{"1":{"result":"radiant","is_admin":false},"999":{"result":"abort","is_admin":true}}"#,
    );
    let pending_match_id = fixture.seed(GUILD, &payload);
    let snapshot = fixture
        .repository
        .snapshot(explicit_key(GUILD, pending_match_id))
        .expect("lookup")
        .expect("match");
    assert_eq!(snapshot.radiant_team_ids, vec![1, 2, 3, 4, 5]);
    assert_eq!(snapshot.dire_team_ids, vec![6, 7, 8, 9, 10]);
    assert_eq!(snapshot.excluded_player_ids, vec![11]);
    assert_eq!(snapshot.excluded_conditional_player_ids, vec![12]);
    assert_eq!(
        snapshot.submissions,
        vec![
            PersistedVoteSubmission {
                user_id: Some(1),
                result: Some("radiant".to_owned()),
                is_admin: false,
            },
            PersistedVoteSubmission {
                user_id: Some(999),
                result: Some("abort".to_owned()),
                is_admin: true,
            },
        ]
    );
}

#[test]
fn cas_preserves_unrelated_payload_fields_and_boolean_shape() {
    let fixture = Fixture::new();
    let pending_match_id = fixture.seed(GUILD, BASE_PAYLOAD);
    let before = fixture
        .repository
        .snapshot(explicit_key(GUILD, pending_match_id))
        .expect("lookup")
        .expect("match");
    let applied = fixture
        .repository
        .compare_and_set_submission(cas(&before, 1, "radiant", false))
        .expect("CAS");
    let VoteSubmissionCasOutcome::Applied(after) = applied else {
        panic!("submission should apply");
    };
    assert_eq!(after.submissions.len(), 1);
    let (timestamp, lobby_kind, result, admin_type): (i64, String, String, String) = fixture
        .connection()
        .query_row(
            "SELECT json_extract(payload,'$.shuffle_timestamp'),
                    json_extract(payload,'$.lobby_kind'),
                    json_extract(payload,'$.record_submissions.\"1\".result'),
                    json_type(payload,'$.record_submissions.\"1\".is_admin')
             FROM pending_matches WHERE pending_match_id=?1",
            params![pending_match_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("inspect payload");
    assert_eq!((timestamp, lobby_kind.as_str()), (1_700_000_000, "open"));
    assert_eq!((result.as_str(), admin_type.as_str()), ("radiant", "false"));
}

#[test]
fn signed_user_ids_and_default_guild_are_round_tripped() {
    let fixture = Fixture::new();
    let pending_match_id = fixture.seed(0, BASE_PAYLOAD);
    let before = fixture
        .repository
        .snapshot(PendingVoteKey {
            guild_id: None,
            pending_match_id: Some(pending_match_id),
        })
        .expect("lookup")
        .expect("match");
    fixture
        .repository
        .compare_and_set_submission(VoteSubmissionCas {
            guild_id: None,
            pending_match_id,
            expected_payload: &before.payload,
            user_id: -70_001,
            result: "dire",
            is_admin: true,
        })
        .expect("CAS");
    let after = fixture
        .repository
        .snapshot(PendingVoteKey {
            guild_id: None,
            pending_match_id: Some(pending_match_id),
        })
        .expect("lookup")
        .expect("match");
    assert!(after.submissions.contains(&PersistedVoteSubmission {
        user_id: Some(-70_001),
        result: Some("dire".to_owned()),
        is_admin: true,
    }));
}

#[test]
fn pending_match_ids_are_guild_scoped_for_reads_and_writes() {
    let fixture = Fixture::new();
    let pending_match_id = fixture.seed(GUILD, BASE_PAYLOAD);
    fixture.seed(OTHER_GUILD, BASE_PAYLOAD);
    assert_eq!(
        fixture
            .repository
            .snapshot(explicit_key(OTHER_GUILD, pending_match_id))
            .expect("cross-guild lookup"),
        None
    );
    let forged = VoteSubmissionCas {
        guild_id: Some(OTHER_GUILD),
        pending_match_id,
        expected_payload: BASE_PAYLOAD,
        user_id: 999,
        result: "radiant",
        is_admin: true,
    };
    assert_eq!(
        fixture
            .repository
            .compare_and_set_submission(forged)
            .expect("guarded CAS"),
        VoteSubmissionCasOutcome::MissingMatch
    );
}

#[test]
fn stale_payload_cas_reports_conflict_without_overwriting_winner() {
    let fixture = Fixture::new();
    let pending_match_id = fixture.seed(GUILD, BASE_PAYLOAD);
    let stale = fixture
        .repository
        .snapshot(explicit_key(GUILD, pending_match_id))
        .expect("lookup")
        .expect("match");
    fixture
        .repository
        .compare_and_set_submission(cas(&stale, 1, "radiant", false))
        .expect("first CAS");
    let conflict = fixture
        .repository
        .compare_and_set_submission(cas(&stale, 2, "dire", false))
        .expect("stale CAS");
    let VoteSubmissionCasOutcome::Conflict(persisted) = conflict else {
        panic!("stale writer must conflict");
    };
    assert_eq!(persisted.submissions.len(), 1);
    assert_eq!(persisted.submissions[0].user_id, Some(1));
}

#[test]
fn concurrent_same_snapshot_has_one_cas_winner() {
    let fixture = Fixture::new();
    let pending_match_id = fixture.seed(GUILD, BASE_PAYLOAD);
    let snapshot = Arc::new(
        fixture
            .repository
            .snapshot(explicit_key(GUILD, pending_match_id))
            .expect("lookup")
            .expect("match"),
    );
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(3));
    let handles = [(1, "radiant"), (2, "dire")]
        .into_iter()
        .map(|(user_id, result)| {
            let repository = Arc::clone(&repository);
            let snapshot = Arc::clone(&snapshot);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository
                    .compare_and_set_submission(cas(&snapshot, user_id, result, false))
                    .expect("CAS")
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, VoteSubmissionCasOutcome::Applied(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, VoteSubmissionCasOutcome::Conflict(_)))
            .count(),
        1
    );
}

#[test]
fn votes_are_visible_to_fresh_repository_instances() {
    let fixture = Fixture::new();
    let pending_match_id = fixture.seed(GUILD, BASE_PAYLOAD);
    let before = fixture
        .repository
        .snapshot(explicit_key(GUILD, pending_match_id))
        .expect("lookup")
        .expect("match");
    fixture
        .repository
        .compare_and_set_submission(cas(&before, 3, "radiant", false))
        .expect("CAS");
    let reloaded = MatchVotingRepository::new(fixture._database.path())
        .snapshot(explicit_key(GUILD, pending_match_id))
        .expect("fresh repository lookup")
        .expect("match");
    assert_eq!(reloaded.submissions[0].user_id, Some(3));
}

#[test]
fn malformed_payload_is_reported_instead_of_mutated() {
    let fixture = Fixture::new();
    let pending_match_id = fixture.seed(GUILD, "{not json");
    assert!(matches!(
        fixture
            .repository
            .snapshot(explicit_key(GUILD, pending_match_id)),
        Err(MatchVotingRepositoryError::MalformedPayload(id)) if id == pending_match_id
    ));
}
