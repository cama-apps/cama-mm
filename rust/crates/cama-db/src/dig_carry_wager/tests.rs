use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = 20_001;
const GUILD: i64 = 12_345;
const OTHER_GUILD: i64 = 54_321;
const PINNACLE_CARRY: &str = r#"{
    "25":{"status":"defeated"},"50":{"status":"defeated"},
    "75":{"status":"defeated"},"100":{"status":"defeated"},
    "150":{"status":"defeated"},"200":{"status":"defeated"},
    "275":{"status":"defeated"},
    "350":{"boss_id":"forgotten_king","status":"phase1_defeated",
           "carried_wager":200,"carried_risk_tier":"bold","hp_max":500}
}"#;

struct Fixture {
    _database: NamedTempFile,
    repository: DigCarryWagerRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary SQLite database");
        let connection = Connection::open(database.path()).expect("fixture connection");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT NOT NULL,
                     jopacoin_balance INTEGER NOT NULL DEFAULT 3,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE tunnels (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     depth INTEGER DEFAULT 0,
                     pinnacle_phase INTEGER DEFAULT 1,
                     boss_progress TEXT,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY CHECK(id = 1),
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );
                 CREATE TABLE economy_ledger_entries (
                     ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL,
                     account_id INTEGER,
                     delta INTEGER NOT NULL,
                     source TEXT NOT NULL,
                     related_type TEXT,
                     reason TEXT
                 );
                 CREATE TRIGGER ledger_player_update
                 AFTER UPDATE OF jopacoin_balance ON players
                 WHEN OLD.jopacoin_balance != NEW.jopacoin_balance
                 BEGIN
                     INSERT INTO economy_ledger_entries
                         (guild_id, account_id, delta, source, related_type, reason)
                     VALUES (
                         NEW.guild_id, NEW.discord_id,
                         NEW.jopacoin_balance - OLD.jopacoin_balance,
                         COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),
                                  'balance_update'),
                         (SELECT related_type FROM economy_ledger_context WHERE id=1),
                         (SELECT reason FROM economy_ledger_context WHERE id=1)
                     );
                 END;",
            )
            .expect("fixture schema");
        drop(connection);
        let repository = DigCarryWagerRepository::new(database.path());
        Self {
            _database: database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self._database.path()).expect("fixture connection")
    }

    fn seed(&self, guild_id: i64, depth: i64, phase: i64, progress: &str, balance: i64) {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO players
                     (discord_id, guild_id, discord_username, jopacoin_balance)
                 VALUES (?1, ?2, 'Pinnacle', ?3)",
                params![ACTOR, guild_id, balance],
            )
            .expect("seed player");
        connection
            .execute(
                "INSERT INTO tunnels
                     (discord_id, guild_id, depth, pinnacle_phase, boss_progress)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![ACTOR, guild_id, depth, phase, progress],
            )
            .expect("seed tunnel");
    }
}

fn key(guild_id: i64) -> CarryTunnelKey {
    CarryTunnelKey {
        discord_id: ACTOR,
        guild_id: Some(guild_id),
    }
}

fn settlement<'a>(
    snapshot: &'a CarryTunnelSnapshot,
    requested_balance_delta: i64,
    mutation: CarryProgressMutation<'a>,
) -> AtomicCarrySettlement<'a> {
    AtomicCarrySettlement {
        key: snapshot.key,
        expected_depth: snapshot.depth,
        expected_pinnacle_phase: snapshot.pinnacle_phase,
        expected_boss_progress: &snapshot.boss_progress,
        depth_after: snapshot.depth,
        pinnacle_phase_after: snapshot.pinnacle_phase,
        requested_balance_delta,
        mutation,
        ledger_related_type: "pinnacle_fight",
        ledger_related_id: "350:2",
        ledger_reason: "settled carried wager",
        ledger_metadata: r#"{"boundary":350}"#,
    }
}

#[test]
fn snapshot_decodes_valid_and_malformed_json_carry_values() {
    let fixture = Fixture::new();
    fixture.seed(GUILD, 350, 2, PINNACLE_CARRY, 2_000);

    let snapshot = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("snapshot")
        .expect("tunnel");
    assert_eq!(snapshot.balance, Some(2_000));
    assert_eq!(
        snapshot.entry(25).and_then(|entry| entry.status.as_deref()),
        Some("defeated")
    );
    assert_eq!(
        snapshot.entry(350),
        Some(&CarryProgressEntry {
            status: Some("phase1_defeated".to_owned()),
            has_wager_marker: true,
            wager: PersistedWager::Integer(200),
            has_risk_marker: true,
            risk_tier: Some("bold".to_owned()),
        })
    );

    fixture
        .connection()
        .execute(
            "UPDATE tunnels SET boss_progress =
             json_set(boss_progress, '$.\"350\".carried_wager', 'lots')
             WHERE discord_id = ?1 AND guild_id = ?2",
            params![ACTOR, GUILD],
        )
        .expect("corrupt wager marker");
    let malformed = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("snapshot")
        .expect("tunnel");
    assert_eq!(
        malformed.entry(350).expect("pinnacle").wager,
        PersistedWager::Invalid
    );
}

#[test]
fn stale_regular_cleanup_preserves_unrelated_entry_fields() {
    let fixture = Fixture::new();
    fixture.seed(
        GUILD,
        25,
        1,
        r#"{"25":{"status":"phase1_defeated","boss_id":"grothak","hp_max":40,"carried_wager":200,"carried_risk_tier":"bold"}}"#,
        2_000,
    );
    let snapshot = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("tunnel");
    assert!(matches!(
        fixture
            .repository
            .clear_carry_markers(&snapshot, 25)
            .expect("cleanup"),
        ClearCarryOutcome::Cleared { .. }
    ));

    let raw: String = fixture
        .connection()
        .query_row(
            "SELECT boss_progress FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![ACTOR, GUILD],
            |row| row.get(0),
        )
        .expect("progress");
    let (status, boss_id, hp_max, carry_type): (String, String, i64, Option<String>) = fixture
        .connection()
        .query_row(
            "SELECT json_extract(?1, '$.\"25\".status'),
                    json_extract(?1, '$.\"25\".boss_id'),
                    json_extract(?1, '$.\"25\".hp_max'),
                    json_type(?1, '$.\"25\".carried_wager')",
            params![raw],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("inspect JSON");
    assert_eq!(
        (status.as_str(), boss_id.as_str(), hp_max),
        ("phase1_defeated", "grothak", 40)
    );
    assert_eq!(carry_type, None);
}

#[test]
fn missing_tunnel_is_distinct_from_missing_player() {
    let fixture = Fixture::new();
    assert_eq!(fixture.repository.snapshot(key(GUILD)).expect("read"), None);

    fixture
        .connection()
        .execute(
            "INSERT INTO tunnels
                 (discord_id,guild_id,depth,pinnacle_phase,boss_progress)
             VALUES (?1,?2,350,2,?3)",
            params![ACTOR, GUILD, PINNACLE_CARRY],
        )
        .expect("tunnel without player");
    let snapshot = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("tunnel");
    let outcome = fixture
        .repository
        .settle_atomic(settlement(
            &snapshot,
            -100,
            CarryProgressMutation::Clear {
                boundary: 350,
                status_after: None,
            },
        ))
        .expect("settle");
    assert_eq!(outcome, CarrySettlementOutcome::MissingPlayer);
}

#[test]
fn atomic_retreat_clears_markers_and_debits_half_with_ledger_context() {
    let fixture = Fixture::new();
    fixture.seed(GUILD, 350, 2, PINNACLE_CARRY, 2_000);
    let snapshot = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("tunnel");
    let mut request = settlement(
        &snapshot,
        -100,
        CarryProgressMutation::Clear {
            boundary: 350,
            status_after: None,
        },
    );
    request.depth_after = 347;
    request.ledger_related_type = "boss_retreat";
    request.ledger_reason = "forfeited half carried wager";
    let outcome = fixture.repository.settle_atomic(request).expect("settle");
    assert!(matches!(
        outcome,
        CarrySettlementOutcome::Applied {
            balance_before: 2_000,
            balance_after: 1_900,
            actual_balance_delta: -100,
            ..
        }
    ));
    let persisted = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("tunnel");
    assert_eq!(persisted.depth, 347);
    assert!(!persisted.entry(350).expect("pinnacle").has_either_marker());
    let ledger: (i64, String, String) = fixture
        .connection()
        .query_row(
            "SELECT delta, source, related_type FROM economy_ledger_entries",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("ledger row");
    assert_eq!(ledger, (-100, "dig".to_owned(), "boss_retreat".to_owned()));
    let context_count: i64 = fixture
        .connection()
        .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
            row.get(0)
        })
        .expect("context count");
    assert_eq!(context_count, 0);
}

#[test]
fn loss_debit_clamps_to_balance_and_commits_with_marker_clear() {
    let fixture = Fixture::new();
    fixture.seed(GUILD, 350, 2, PINNACLE_CARRY, 75);
    let snapshot = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("tunnel");
    let outcome = fixture
        .repository
        .settle_atomic(settlement(
            &snapshot,
            -200,
            CarryProgressMutation::Clear {
                boundary: 350,
                status_after: None,
            },
        ))
        .expect("settle");
    assert!(matches!(
        outcome,
        CarrySettlementOutcome::Applied {
            balance_before: 75,
            balance_after: 0,
            actual_balance_delta: -75,
            ..
        }
    ));
}

#[test]
fn guild_scope_isolation_keeps_same_signed_actor_independent() {
    let fixture = Fixture::new();
    fixture.seed(GUILD, 350, 2, PINNACLE_CARRY, 2_000);
    fixture.seed(OTHER_GUILD, 350, 2, PINNACLE_CARRY, 3_000);
    let snapshot = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("tunnel");
    fixture
        .repository
        .settle_atomic(settlement(
            &snapshot,
            -200,
            CarryProgressMutation::Clear {
                boundary: 350,
                status_after: None,
            },
        ))
        .expect("settle");

    let first = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("first");
    let second = fixture
        .repository
        .snapshot(key(OTHER_GUILD))
        .expect("read")
        .expect("second");
    assert_eq!(first.balance, Some(1_800));
    assert_eq!(second.balance, Some(3_000));
    assert!(second.entry(350).expect("pinnacle").has_either_marker());
}

#[test]
fn concurrent_duplicate_settlement_applies_exactly_once() {
    let fixture = Fixture::new();
    fixture.seed(GUILD, 350, 2, PINNACLE_CARRY, 2_000);
    let snapshot = Arc::new(
        fixture
            .repository
            .snapshot(key(GUILD))
            .expect("read")
            .expect("tunnel"),
    );
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let snapshot = Arc::clone(&snapshot);
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository
                    .settle_atomic(settlement(
                        &snapshot,
                        -200,
                        CarryProgressMutation::Clear {
                            boundary: 350,
                            status_after: None,
                        },
                    ))
                    .expect("settle")
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
            .filter(|outcome| matches!(outcome, CarrySettlementOutcome::Applied { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, CarrySettlementOutcome::Conflict))
            .count(),
        1
    );
    let persisted = repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("tunnel");
    assert_eq!(persisted.balance, Some(1_800));
}

#[test]
fn forward_mutation_preserves_entry_metadata_and_changes_phase_atomically() {
    let fixture = Fixture::new();
    let active = PINNACLE_CARRY
        .replace("phase1_defeated", "active")
        .replace("\"carried_wager\":200,\"carried_risk_tier\":\"bold\",", "");
    fixture.seed(GUILD, 350, 1, &active, 2_000);
    let snapshot = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("tunnel");
    let mut request = settlement(
        &snapshot,
        0,
        CarryProgressMutation::Forward {
            boundary: 350,
            status_after: "phase1_defeated",
            wager: 150,
            risk_tier: "reckless",
        },
    );
    request.pinnacle_phase_after = 2;
    fixture.repository.settle_atomic(request).expect("settle");
    let persisted = fixture
        .repository
        .snapshot(key(GUILD))
        .expect("read")
        .expect("tunnel");
    assert_eq!(persisted.pinnacle_phase, 2);
    let entry = persisted.entry(350).expect("pinnacle");
    assert_eq!(entry.status.as_deref(), Some("phase1_defeated"));
    assert_eq!(entry.wager, PersistedWager::Integer(150));
    assert_eq!(entry.risk_tier.as_deref(), Some("reckless"));
}
