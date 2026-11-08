use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const ACTOR: i64 = 20_001;
const GUILD: i64 = 12_345;
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
    assert_eq!(snapshot.balance, None, "the tunnel has no player row");
}
