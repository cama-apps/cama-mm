use rusqlite::{Connection, OptionalExtension};
use tempfile::NamedTempFile;

use super::*;

#[test]
fn test_dig_trivia_persists_its_economy_ledger_context() {
    let database = NamedTempFile::new().expect("temporary database");
    let connection = Connection::open(database.path()).expect("fixture connection");
    connection
        .execute_batch(
            "CREATE TABLE players (
                 discord_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL DEFAULT 0,
                 discord_username TEXT,
                 jopacoin_balance INTEGER DEFAULT 3,
                 last_match_date TEXT,
                 updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                 PRIMARY KEY (discord_id, guild_id)
             );
             CREATE TABLE economy_ledger_context (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 source TEXT,
                 actor_id INTEGER,
                 related_type TEXT,
                 related_id TEXT,
                 reason TEXT,
                 metadata TEXT
             );
             CREATE TABLE economy_ledger_entries (
                 ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 account_id INTEGER NOT NULL,
                 guild_id INTEGER NOT NULL,
                 balance_before INTEGER NOT NULL,
                 delta INTEGER NOT NULL,
                 balance_after INTEGER NOT NULL,
                 source TEXT,
                 actor_id INTEGER,
                 related_type TEXT,
                 related_id TEXT,
                 reason TEXT,
                 metadata TEXT,
                 created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
             );
             CREATE TRIGGER players_balance_ledger
             AFTER UPDATE OF jopacoin_balance ON players
             WHEN OLD.jopacoin_balance IS NOT NEW.jopacoin_balance
             BEGIN
                 INSERT INTO economy_ledger_entries (
                     account_id, guild_id, balance_before, delta, balance_after,
                     source, actor_id, related_type, related_id, reason, metadata
                 ) VALUES (
                     NEW.discord_id, NEW.guild_id, OLD.jopacoin_balance,
                     NEW.jopacoin_balance - OLD.jopacoin_balance,
                     NEW.jopacoin_balance,
                     (SELECT source FROM economy_ledger_context WHERE id = 1),
                     (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                     (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                     (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                     (SELECT reason FROM economy_ledger_context WHERE id = 1),
                     (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                 );
             END;",
        )
        .expect("fixture schema");
    connection
        .execute(
            "INSERT INTO players (
                 discord_id, guild_id, discord_username, jopacoin_balance,
                 last_match_date
             ) VALUES (1, 99, 'Digger', 105, '2026-08-11T00:00:00+00:00'),
                      (1, 100, 'Other guild', 900, '2026-08-11T00:00:00+00:00'),
                      (-7, 99, 'Signed boundary', 3, '2026-01-01T00:00:00+00:00')",
            [],
        )
        .expect("fixture players");
    drop(connection);

    let repository = DigBonusRepository::new(database.path());
    let metadata = r#"{"correct":true,"timed_out":false,"gross_jc":15,"reward_multiplier":0.65}"#;
    let settlement = repository
        .adjust_balance_atomic(BalanceAdjustmentRequest {
            discord_id: 1,
            guild_id: Some(99),
            delta: 10,
            source: "dig",
            actor_id: 1,
            related_type: "dig_bonus_trivia",
            related_id: "hero_quote",
            reason: "dig bonus trivia correct answer",
            metadata_json: metadata,
        })
        .unwrap();
    assert_eq!(
        settlement,
        BalanceAdjustment {
            balance_before: 105,
            delta: 10,
            balance_after: 115,
        }
    );

    let connection = Connection::open(database.path()).unwrap();
    let ledger = connection
        .query_row(
            "SELECT delta, source, actor_id, related_type, related_id,
                    reason, metadata
               FROM economy_ledger_entries
              ORDER BY ledger_id DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        ledger,
        (
            10,
            "dig".into(),
            1,
            "dig_bonus_trivia".into(),
            "hero_quote".into(),
            "dig bonus trivia correct answer".into(),
            metadata.into(),
        )
    );
    let other_guild: i64 = connection
        .query_row(
            "SELECT jopacoin_balance FROM players
              WHERE discord_id = 1 AND guild_id = 100",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(other_guild, 900);
    let context_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(context_count, 0);
    drop(connection);

    let before_rollback_count = Connection::open(database.path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert!(matches!(
        repository.adjust_balance_atomic(BalanceAdjustmentRequest {
            discord_id: 404,
            guild_id: Some(99),
            delta: 10,
            source: "dig",
            actor_id: 404,
            related_type: "dig_bonus_trivia",
            related_id: "hero_quote",
            reason: "dig bonus trivia correct answer",
            metadata_json: metadata,
        }),
        Err(DigBonusRepositoryError::PlayerNotFound { .. })
    ));
    let connection = Connection::open(database.path()).unwrap();
    let after_rollback_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(after_rollback_count, before_rollback_count);
    let leaked_context: Option<String> = connection
        .query_row(
            "SELECT source FROM economy_ledger_context WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(leaked_context, None);

    assert_eq!(
        repository
            .recently_active_player_ids(Some(99), "2026-08-01T00:00:00+00:00")
            .unwrap(),
        [1]
    );
}
