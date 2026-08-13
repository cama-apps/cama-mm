use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = -70_001;
const GUILD: i64 = 12_345;
const OTHER_GUILD: i64 = 54_321;

struct Fixture {
    _database: NamedTempFile,
    repository: DigEventThreatRepository,
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
                     depth INTEGER NOT NULL DEFAULT 0,
                     streak_days INTEGER NOT NULL DEFAULT 0,
                     luminosity INTEGER DEFAULT 100,
                     temp_buffs TEXT,
                     temp_curses TEXT,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE dig_actions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     actor_id INTEGER NOT NULL,
                     target_id INTEGER,
                     action_type TEXT NOT NULL,
                     depth_before INTEGER NOT NULL,
                     depth_after INTEGER NOT NULL,
                     jc_delta INTEGER NOT NULL DEFAULT 0,
                     detail TEXT,
                     created_at INTEGER NOT NULL
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
                     balance_before INTEGER,
                     balance_after INTEGER,
                     source TEXT NOT NULL,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );
                 CREATE TRIGGER ledger_player_update
                 AFTER UPDATE OF jopacoin_balance ON players
                 WHEN OLD.jopacoin_balance != NEW.jopacoin_balance
                 BEGIN
                     INSERT INTO economy_ledger_entries (
                         guild_id, account_id, delta, balance_before,
                         balance_after, source, related_type, related_id,
                         reason, metadata
                     ) VALUES (
                         NEW.guild_id,
                         NEW.discord_id,
                         NEW.jopacoin_balance - OLD.jopacoin_balance,
                         OLD.jopacoin_balance,
                         NEW.jopacoin_balance,
                         COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),
                                  'balance_update'),
                         (SELECT related_type FROM economy_ledger_context WHERE id=1),
                         (SELECT related_id FROM economy_ledger_context WHERE id=1),
                         (SELECT reason FROM economy_ledger_context WHERE id=1),
                         (SELECT metadata FROM economy_ledger_context WHERE id=1)
                     );
                 END;",
            )
            .expect("fixture schema");
        drop(connection);
        let repository = DigEventThreatRepository::new(database.path());
        Self {
            _database: database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self._database.path()).expect("fixture connection")
    }

    fn seed(&self, discord_id: i64, guild_id: i64, balance: i64) {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO players
                     (discord_id, guild_id, discord_username, jopacoin_balance)
                 VALUES (?1, ?2, 'threat-test', ?3)",
                params![discord_id, guild_id, balance],
            )
            .expect("seed player");
        connection
            .execute(
                "INSERT INTO tunnels (
                     discord_id, guild_id, depth, streak_days, luminosity,
                     temp_buffs, temp_curses
                 ) VALUES (?1, ?2, 50, 10, 100, ?3, NULL)",
                params![discord_id, guild_id, r#"{"id":"power","digs_remaining":3}"#],
            )
            .expect("seed tunnel");
    }
}

fn key(guild_id: Option<i64>) -> ThreatTunnelKey {
    ThreatTunnelKey {
        discord_id: ACTOR,
        guild_id,
    }
}

fn curse(id: &str, remaining: i64) -> ThreatCurse {
    ThreatCurse {
        id: id.to_owned(),
        name: format!("{id} name"),
        digs_remaining: remaining,
        effects: ThreatCurseEffects {
            advance_bonus: Some(-4),
            cave_in_bonus: Some(0.1),
            cooldown_penalty: None,
            jc_bonus: None,
            luminosity_drain: None,
        },
    }
}

fn settlement<'a>(
    expected: &'a ThreatSnapshot,
    curse_mutation: ThreatCurseMutation<'a>,
    delta: i64,
) -> AtomicThreatSettlement<'a> {
    AtomicThreatSettlement {
        expected,
        depth_after: 48,
        streak_days_after: 7,
        luminosity_after: 85,
        curse_mutation,
        balance_delta: delta,
        action_type: "event",
        related_id: "synthetic-threat",
        reason: "dig event loss",
        detail_json: r#"{"event_id":"synthetic-threat","streak_days_lost":3}"#,
        created_at: 1_000_000,
    }
}

#[test]
fn helper_replaces_only_curse_and_preserves_buff() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 500);
    let first = curse("first", 5);
    let second = ThreatCurse {
        id: "second".to_owned(),
        name: "Second Hex".to_owned(),
        digs_remaining: 2,
        effects: ThreatCurseEffects {
            jc_bonus: Some(-10),
            ..ThreatCurseEffects::default()
        },
    };
    assert_eq!(
        fixture
            .repository
            .replace_temp_curse(key(Some(GUILD)), Some(&first))
            .expect("first curse"),
        ReplaceCurseOutcome::Applied
    );
    fixture
        .repository
        .replace_temp_curse(key(Some(GUILD)), Some(&second))
        .expect("second curse");
    let snapshot = fixture
        .repository
        .snapshot(key(Some(GUILD)))
        .expect("snapshot")
        .expect("tunnel");
    assert_eq!(snapshot.temp_curse.active(), Some(&second));
    assert_eq!(
        snapshot.temp_buff_json.as_deref(),
        Some(r#"{"id":"power","digs_remaining":3}"#)
    );
}

#[test]
fn atomic_resolution_commits_signed_debt_tunnel_audit_and_ledger() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 30);
    let expected = fixture
        .repository
        .snapshot(key(Some(GUILD)))
        .expect("snapshot")
        .expect("tunnel");
    let applied_curse = curse("event-curse", 4);
    let result = fixture
        .repository
        .settle_atomic(settlement(
            &expected,
            ThreatCurseMutation::Replace(&applied_curse),
            -394,
        ))
        .expect("settlement");
    assert_eq!(
        result,
        ThreatSettlementOutcome::Applied {
            balance_before: 30,
            balance_after: -364,
        }
    );
    let persisted = fixture
        .repository
        .snapshot(key(Some(GUILD)))
        .expect("snapshot")
        .expect("tunnel");
    assert_eq!(persisted.depth, 48);
    assert_eq!(persisted.streak_days, 7);
    assert_eq!(persisted.luminosity, 85);
    assert_eq!(persisted.temp_curse.active(), Some(&applied_curse));
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row("SELECT action_type, jc_delta FROM dig_actions", [], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            },)
            .expect("audit row"),
        ("event".to_owned(), -394)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT delta, balance_after, source, related_id
                   FROM economy_ledger_entries",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .expect("ledger row"),
        (-394, -364, "dig".to_owned(), "synthetic-threat".to_owned())
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("context count"),
        0
    );
}

#[test]
fn stale_snapshot_conflicts_without_partial_wallet_or_audit() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 500);
    let stale = fixture
        .repository
        .snapshot(key(Some(GUILD)))
        .expect("snapshot")
        .expect("tunnel");
    fixture
        .connection()
        .execute(
            "UPDATE tunnels SET streak_days = 11 WHERE discord_id = ?1 AND guild_id = ?2",
            params![ACTOR, GUILD],
        )
        .expect("concurrent mutation");
    let result = fixture
        .repository
        .settle_atomic(settlement(&stale, ThreatCurseMutation::Clear, -100))
        .expect("conflict result");
    assert_eq!(result, ThreatSettlementOutcome::Conflict);
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![ACTOR, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("balance"),
        500
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_actions", [], |row| row
                .get::<_, i64>(0))
            .expect("audit count"),
        0
    );
}

#[test]
fn missing_player_rolls_back_tunnel_mutation() {
    let fixture = Fixture::new();
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO tunnels (
                 discord_id,guild_id,depth,streak_days,luminosity,temp_buffs,temp_curses
             ) VALUES (?1,?2,50,10,100,NULL,NULL)",
            params![ACTOR, GUILD],
        )
        .expect("orphan tunnel");
    drop(connection);
    let expected = fixture
        .repository
        .snapshot(key(Some(GUILD)))
        .expect("snapshot")
        .expect("tunnel");
    assert_eq!(
        fixture
            .repository
            .settle_atomic(settlement(&expected, ThreatCurseMutation::Clear, 20))
            .expect("missing player"),
        ThreatSettlementOutcome::MissingPlayer
    );
    let after = fixture
        .repository
        .snapshot(key(Some(GUILD)))
        .expect("snapshot")
        .expect("tunnel");
    assert_eq!(after.depth, 50);
    assert_eq!(after.streak_days, 10);
}

#[test]
fn signed_ids_none_guild_normalization_and_guild_isolation_hold() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, 0, 100);
    fixture.seed(ACTOR, OTHER_GUILD, 900);
    let expected = fixture
        .repository
        .snapshot(key(None))
        .expect("global snapshot")
        .expect("tunnel");
    fixture
        .repository
        .settle_atomic(settlement(&expected, ThreatCurseMutation::Preserve, -150))
        .expect("signed settlement");
    let global = fixture
        .repository
        .snapshot(key(None))
        .expect("global snapshot")
        .expect("tunnel");
    let other = fixture
        .repository
        .snapshot(key(Some(OTHER_GUILD)))
        .expect("other snapshot")
        .expect("tunnel");
    assert_eq!(global.balance, -50);
    assert_eq!(other.balance, 900);
    assert_eq!(other.depth, 50);
}

#[test]
fn concurrent_snapshot_settlement_applies_exactly_once() {
    let fixture = Arc::new(Fixture::new());
    fixture.seed(ACTOR, GUILD, 500);
    let expected = Arc::new(
        fixture
            .repository
            .snapshot(key(Some(GUILD)))
            .expect("snapshot")
            .expect("tunnel"),
    );
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let fixture = Arc::clone(&fixture);
            let expected = Arc::clone(&expected);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                fixture.repository.settle_atomic(settlement(
                    &expected,
                    ThreatCurseMutation::Preserve,
                    -100,
                ))
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("worker").expect("settlement"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ThreatSettlementOutcome::Applied { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == ThreatSettlementOutcome::Conflict)
            .count(),
        1
    );
    assert_eq!(
        fixture
            .repository
            .snapshot(key(Some(GUILD)))
            .expect("snapshot")
            .expect("tunnel")
            .balance,
        400
    );
}

#[test]
fn malformed_and_expired_curse_json_are_not_active() {
    let fixture = Fixture::new();
    fixture.seed(ACTOR, GUILD, 500);
    let connection = fixture.connection();
    connection
        .execute(
            "UPDATE tunnels SET temp_curses='not-json' WHERE discord_id=?1 AND guild_id=?2",
            params![ACTOR, GUILD],
        )
        .expect("malformed curse");
    assert_eq!(
        fixture
            .repository
            .snapshot(key(Some(GUILD)))
            .expect("snapshot")
            .expect("tunnel")
            .temp_curse,
        PersistedCurse::Malformed
    );
    connection
        .execute(
            "UPDATE tunnels SET temp_curses=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![
                r#"{"id":"old","name":"Old Hex","digs_remaining":0,"effect":{"advance_bonus":-9}}"#,
                ACTOR,
                GUILD
            ],
        )
        .expect("expired curse");
    assert!(
        fixture
            .repository
            .snapshot(key(Some(GUILD)))
            .expect("snapshot")
            .expect("tunnel")
            .temp_curse
            .active()
            .is_none()
    );
}

/// Python creates the disposable production schema; Rust applies a guarded
/// curse/debt event; Python verifies the JSON, signed balance, action, ledger,
/// and preserved sibling buff. This remains ignored during normal unit runs.
#[test]
#[ignore = "cross-language smoke; run explicitly when repository Python is available"]
fn unmapped_python_migrated_database_dig_event_threat_interop_smoke() {
    let database = NamedTempFile::new().expect("temporary interop database");
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root");
    let python = repository_root.join(".venv/bin/python");
    let initialize = Command::new(&python)
        .current_dir(&repository_root)
        .args([
            "-c",
            "import sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\np=sys.argv[1]\nSchemaManager(p).initialize()\nc=sqlite3.connect(p)\nc.execute(\"INSERT INTO players (discord_id,guild_id,discord_username,jopacoin_balance) VALUES (?,?,?,?)\",(-70001,0,'threat-interop',30))\nc.execute(\"INSERT INTO tunnels (discord_id,guild_id,depth,streak_days,luminosity,temp_buffs,temp_curses,tunnel_name) VALUES (?,?,?,?,?,?,?,?)\",(-70001,0,50,10,100,'{\\\"id\\\":\\\"power\\\",\\\"digs_remaining\\\":3}',None,'interop'))\nc.commit(); c.close()",
        ])
        .arg(database.path())
        .output()
        .expect("run Python schema authority");
    assert!(
        initialize.status.success(),
        "Python initialization failed: {}",
        String::from_utf8_lossy(&initialize.stderr)
    );

    let repository = DigEventThreatRepository::new(database.path());
    let interop_key = key(None);
    let expected = repository
        .snapshot(interop_key)
        .expect("read migrated schema")
        .expect("interop tunnel");
    let event_curse = curse("interop-hex", 4);
    let outcome = repository
        .settle_atomic(settlement(
            &expected,
            ThreatCurseMutation::Replace(&event_curse),
            -394,
        ))
        .expect("Rust settlement");
    assert!(matches!(
        outcome,
        ThreatSettlementOutcome::Applied {
            balance_after: -364,
            ..
        }
    ));

    let verify = Command::new(python)
        .current_dir(repository_root)
        .args([
            "-c",
            "import json,sqlite3,sys\nc=sqlite3.connect(sys.argv[1])\nt=c.execute('SELECT depth,streak_days,luminosity,temp_buffs,temp_curses FROM tunnels WHERE discord_id=-70001 AND guild_id=0').fetchone()\nassert t[:3]==(48,7,85)\nassert json.loads(t[3])['id']=='power'\nassert json.loads(t[4])=={'id':'interop-hex','name':'interop-hex name','digs_remaining':4,'effect':{'advance_bonus':-4,'cave_in_bonus':0.1}}\nassert c.execute('SELECT jopacoin_balance FROM players WHERE discord_id=-70001 AND guild_id=0').fetchone()==(-364,)\nassert c.execute(\"SELECT COUNT(*) FROM dig_actions WHERE actor_id=-70001 AND guild_id=0 AND action_type='event' AND jc_delta=-394\").fetchone()==(1,)\nassert c.execute(\"SELECT COUNT(*) FROM economy_ledger_entries WHERE account_id=-70001 AND guild_id=0 AND source='dig' AND delta=-394\").fetchone()==(1,)\nassert c.execute('SELECT COUNT(*) FROM economy_ledger_context').fetchone()==(0,)\nc.close()",
        ])
        .arg(database.path())
        .output()
        .expect("run Python verification");
    assert!(
        verify.status.success(),
        "Python verification failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}
