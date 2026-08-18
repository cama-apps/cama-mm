use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Barrier};

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::core_repositories::{NewPlayer, PlayerRepository};
use crate::{audit_database, expected_migrations};

fn empty_database() -> NamedTempFile {
    NamedTempFile::new().expect("create disposable database")
}

fn ledger_names(path: &Path) -> BTreeSet<String> {
    let connection = Connection::open(path).expect("open migration fixture");
    let mut statement = connection
        .prepare("SELECT name FROM schema_migrations ORDER BY name")
        .expect("prepare migration ledger read");
    statement
        .query_map([], |row| row.get(0))
        .expect("read migration ledger")
        .collect::<Result<_, _>>()
        .expect("collect migration ledger")
}

fn object_signatures(path: &Path) -> Vec<(String, String, String)> {
    let connection = Connection::open(path).expect("open signature fixture");
    connection
        .prepare(
            "SELECT type, name, sql FROM sqlite_master
             WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        )
        .expect("prepare signature query")
        .query_map([], |row| {
            let sql: String = row.get(2)?;
            Ok((row.get(0)?, row.get(1)?, normalize_sql(&sql)))
        })
        .expect("read schema signatures")
        .collect::<Result<_, _>>()
        .expect("collect schema signatures")
}

#[test]
fn test_delivery_migrations_quarantine_legacy_claims_and_add_retry_intent() {
    let database = empty_database();
    {
        let connection = Connection::open(database.path()).expect("open legacy survey database");
        connection
            .execute_batch(
                "CREATE TABLE survey_recipients (
                   recipient_id INTEGER PRIMARY KEY,
                   survey_id INTEGER NOT NULL,
                   guild_id INTEGER NOT NULL DEFAULT 0,
                   discord_id INTEGER NOT NULL,
                   delivery_status TEXT NOT NULL,
                   attempt_count INTEGER NOT NULL,
                   claimed_at INTEGER,
                   dm_channel_id INTEGER,
                   dm_message_id INTEGER,
                   ui_channel_id INTEGER,
                   ui_message_id INTEGER,
                   controls_finalized INTEGER NOT NULL DEFAULT 0,
                   current_question_id INTEGER,
                   in_review INTEGER NOT NULL DEFAULT 0,
                   started_at INTEGER,
                   submitted_at INTEGER,
                   last_error TEXT,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL
                 );
                 INSERT INTO survey_recipients
                   (recipient_id,survey_id,guild_id,discord_id,delivery_status,
                    attempt_count,claimed_at,created_at,updated_at)
                 VALUES (1,9,10,101,'pending',2,NULL,100,123);",
            )
            .expect("seed legacy delivery row");
    }
    initialize_or_migrate(database.path()).expect("migrate legacy survey database");
    let connection = Connection::open(database.path()).expect("reopen migrated database");
    let row = connection
        .query_row(
            "SELECT delivery_status,claimed_at,receipt_checked_attempt,retry_requested
             FROM survey_recipients WHERE recipient_id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .expect("read migrated delivery row");
    assert_eq!(row, ("sending".to_owned(), Some(123), 0, 0));
}

#[test]
fn migration_settings_match_python_rating_environment_semantics() {
    let values = BTreeMap::from([
        ("STREAK_THRESHOLD", "5"),
        ("STREAK_MULTIPLIER_PER_GAME", "0.42"),
        ("NEW_PLAYER_MMR_DISCOUNT", "725"),
        ("OPENSKILL_CALIBRATION_SIGMA_THRESHOLD", "3.25"),
        ("OPENSKILL_PERFORMANCE_STRENGTH", "0.18"),
        ("OPENSKILL_SIGMA_DECAY_GRACE_PERIOD_DAYS", "11"),
        ("OPENSKILL_SIGMA_DECAY_PER_WEEK", "1.75"),
        ("OPENSKILL_WIN_PROBABILITY_TEMPERATURE", "7.25"),
    ]);
    let settings =
        MigrationSettings::from_lookup(|name| values.get(name).map(|value| (*value).to_owned()));

    assert_eq!(settings.streak_threshold, 5);
    assert_eq!(settings.new_player_mmr_discount, 725);
    assert_eq!(settings.openskill.streak_threshold, 5);
    assert_eq!(settings.openskill.streak_multiplier_per_game, 0.42);
    assert_eq!(settings.openskill.calibration_threshold, 3.25);
    assert_eq!(settings.openskill.performance_strength, 0.18);
    assert_eq!(settings.openskill.sigma_decay_grace_period_days, 11);
    assert_eq!(settings.openskill.sigma_decay_per_week, 1.75);
    assert_eq!(settings.openskill.win_probability_temperature, 7.25);

    let invalid = MigrationSettings::from_lookup(|_| Some("not-a-number".to_owned()));
    assert_eq!(invalid, MigrationSettings::default());

    let whitespace = MigrationSettings::from_lookup(|name| match name {
        "STREAK_THRESHOLD" => Some(" 7 ".to_owned()),
        "OPENSKILL_PERFORMANCE_STRENGTH" => Some(" 0.25 ".to_owned()),
        "NEW_PLAYER_MMR_DISCOUNT" => Some(" 700 ".to_owned()),
        _ => None,
    });
    assert_eq!(whitespace.streak_threshold, 7);
    assert_eq!(whitespace.openskill.streak_threshold, 7);
    assert_eq!(whitespace.openskill.performance_strength, 0.25);
    assert_eq!(whitespace.new_player_mmr_discount, 700);
}

#[test]
fn test_streak_threshold_column_defaults_to_3_for_fresh_inserts() {
    let file = empty_database();
    initialize_or_migrate(file.path()).expect("initialize current schema");
    let connection = Connection::open(file.path()).expect("open threshold fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production migration connection");
    let columns = table_columns(&connection, "rating_history").expect("rating-history columns");
    let threshold = columns
        .iter()
        .find(|column| column.name == "streak_threshold")
        .expect("streak-threshold column");
    assert_eq!(threshold.default_value.as_deref(), Some("3"));

    connection
        .execute(
            "INSERT INTO players(discord_id,guild_id,discord_username)
             VALUES(98999,42,'ThresholdDefault')",
            [],
        )
        .expect("insert threshold player");
    connection
        .execute(
            "INSERT INTO rating_history(discord_id,guild_id,rating,won)
             VALUES(98999,42,1500.0,1)",
            [],
        )
        .expect("insert history without explicit threshold");
    let stored: i64 = connection
        .query_row(
            "SELECT streak_threshold FROM rating_history
             WHERE discord_id=98999 AND guild_id=42",
            [],
            |row| row.get(0),
        )
        .expect("read default threshold");
    assert_eq!(stored, 3);
}

#[test]
fn test_protection_white_shield_migration_backfills_existing_unconsumed_plains() {
    let file = empty_database();
    initialize_or_migrate(file.path()).expect("initialize protection fixture");
    let connection = Connection::open(file.path()).expect("open protection fixture");
    connection
        .execute(
            "INSERT INTO player_mana(
                 discord_id,guild_id,current_land,assigned_date,consumed_today,
                 white_shield_remaining
             ) VALUES(77002,42,'Plains','2033-05-18',0,0)",
            [],
        )
        .expect("insert legacy Plains row");
    connection
        .execute(
            "DELETE FROM schema_migrations
             WHERE name='add_white_shield_remaining_to_player_mana'",
            [],
        )
        .expect("make Guardian migration pending");
    drop(connection);

    initialize_or_migrate(file.path()).expect("apply Guardian backfill");
    let capacity: i64 = Connection::open(file.path())
        .expect("reopen protection fixture")
        .query_row(
            "SELECT white_shield_remaining FROM player_mana
             WHERE discord_id=77002 AND guild_id=42",
            [],
            |row| row.get(0),
        )
        .expect("read Guardian capacity");
    assert_eq!(capacity, 25);
}

#[test]
fn test_migration_backfills_pre_column_rows_with_live_config_threshold() {
    let file = empty_database();
    initialize_or_migrate(file.path()).expect("initialize fixture");
    let connection = Connection::open(file.path()).expect("open threshold fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production migration connection");
    connection
        .execute(
            "INSERT INTO rating_history(discord_id,rating,streak_threshold)
             VALUES(77001,1500,3)",
            [],
        )
        .expect("insert legacy history row");
    connection
        .execute(
            "DELETE FROM schema_migrations
             WHERE name='add_streak_threshold_to_rating_history'",
            [],
        )
        .expect("make threshold migration pending");
    drop(connection);

    let settings = MigrationSettings {
        streak_threshold: 4,
        openskill: OpenSkillConfig {
            streak_threshold: 4,
            ..OpenSkillConfig::default()
        },
        ..MigrationSettings::default()
    };
    initialize_or_migrate_with_settings(file.path(), &settings)
        .expect("apply live configured threshold");
    let connection = Connection::open(file.path()).expect("inspect threshold backfill");
    let threshold: i64 = connection
        .query_row(
            "SELECT streak_threshold FROM rating_history WHERE discord_id=77001",
            [],
            |row| row.get(0),
        )
        .expect("read configured threshold");
    assert_eq!(threshold, 4);
    connection
        .execute(
            "UPDATE rating_history SET streak_threshold=6 WHERE discord_id=77001",
            [],
        )
        .expect("record later historical threshold");
    drop(connection);

    initialize_or_migrate_with_settings(file.path(), &settings)
        .expect("idempotent migration rerun");
    let threshold: i64 = Connection::open(file.path())
        .expect("reopen threshold fixture")
        .query_row(
            "SELECT streak_threshold FROM rating_history WHERE discord_id=77001",
            [],
            |row| row.get(0),
        )
        .expect("read preserved threshold");
    assert_eq!(threshold, 6);
}

#[test]
fn explicit_migration_settings_drive_durable_backfills() {
    let file = empty_database();
    initialize_or_migrate_with_settings(file.path(), &MigrationSettings::default())
        .expect("initialize fixture");
    let connection = Connection::open(file.path()).expect("open initialized fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production migration connection");
    connection
        .execute(
            "INSERT INTO rating_history(discord_id,rating,streak_threshold)
             VALUES (9001,1500,3)",
            [],
        )
        .expect("insert historical rating row");
    connection
        .execute(
            "DELETE FROM schema_migrations
             WHERE name='add_streak_threshold_to_rating_history'",
            [],
        )
        .expect("make threshold backfill pending");
    drop(connection);

    let settings = MigrationSettings {
        streak_threshold: 7,
        openskill: OpenSkillConfig {
            streak_threshold: 7,
            ..OpenSkillConfig::default()
        },
        ..MigrationSettings::default()
    };
    initialize_or_migrate_with_settings(file.path(), &settings)
        .expect("apply configured threshold backfill");

    let connection = Connection::open(file.path()).expect("inspect configured backfill");
    let threshold: i64 = connection
        .query_row(
            "SELECT streak_threshold FROM rating_history WHERE discord_id=9001",
            [],
            |row| row.get(0),
        )
        .expect("read configured threshold");
    assert_eq!(threshold, 7);
}

#[test]
fn clean_database_is_initialized_without_python() {
    let file = empty_database();
    let report = initialize_or_migrate(file.path()).expect("initialize clean database in Rust");

    assert_eq!(report.previously_applied, 0);
    assert_eq!(report.newly_applied, expected_migrations());
    assert!(report.created_tables.len() > 80, "{report:?}");
    assert!(report.rebuilt_tables.is_empty());

    let audit = audit_database(file.path()).expect("audit Rust-initialized database");
    assert!(audit.is_compatible(), "{:?}", audit.issues());
    assert_eq!(
        ledger_names(file.path()),
        expected_migrations()
            .into_iter()
            .map(str::to_owned)
            .collect()
    );

    let connection = Connection::open(file.path()).expect("inspect SQLite pragmas");
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal mode");
    let busy_timeout: i64 = connection
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .expect("read busy timeout");
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    // Busy timeout is connection-local; initialize sets it on the migration
    // connection while a new rusqlite connection uses its own default.
    assert!(busy_timeout >= 0);
}

#[test]
fn initialize_is_idempotent_and_keeps_historical_ledger_rows() {
    let file = empty_database();
    initialize_or_migrate(file.path()).expect("initialize fixture");
    let connection = Connection::open(file.path()).expect("open initialized fixture");
    connection
        .execute(
            "INSERT INTO schema_migrations(name, applied_at)
             VALUES ('retired_pre_cutover_experiment', '2024-01-02 03:04:05')",
            [],
        )
        .expect("insert historical migration");
    drop(connection);

    let signatures_before = object_signatures(file.path());
    let report = initialize_or_migrate(file.path()).expect("repeat initialization");

    assert!(report.was_current(), "{report:?}");
    assert_eq!(
        report.historical_extra_migrations,
        ["retired_pre_cutover_experiment"]
    );
    assert_eq!(signatures_before, object_signatures(file.path()));
    assert!(ledger_names(file.path()).contains("retired_pre_cutover_experiment"));
}

#[test]
fn base_schema_snapshot_reaches_head_and_preserves_rows_and_extra_columns() {
    let file = empty_database();
    let connection = Connection::open(file.path()).expect("open legacy fixture");
    connection
        .execute_batch(
            "CREATE TABLE players (
                 discord_id INTEGER PRIMARY KEY,
                 discord_username TEXT NOT NULL,
                 steam_id INTEGER,
                 created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                 legacy_note TEXT
             );
             CREATE TABLE matches (
                 match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 team1_players TEXT NOT NULL,
                 team2_players TEXT NOT NULL,
                 winning_team INTEGER,
                 match_date TIMESTAMP DEFAULT CURRENT_TIMESTAMP
             );
             CREATE TABLE match_participants (
                 match_id INTEGER,
                 discord_id INTEGER,
                 team_number INTEGER,
                 won BOOLEAN,
                 side TEXT,
                 PRIMARY KEY(match_id, discord_id)
             );
             CREATE TABLE rating_history (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 discord_id INTEGER,
                 rating REAL,
                 match_id INTEGER,
                 timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP
             );
             CREATE TABLE match_predictions (
                 match_id INTEGER PRIMARY KEY,
                 radiant_rating REAL,
                 dire_rating REAL,
                 radiant_rd REAL,
                 dire_rd REAL,
                 expected_radiant_win_prob REAL,
                 timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP
             );",
        )
        .expect("create historical base schema");
    connection
        .execute(
            "INSERT INTO players(
                 discord_id, discord_username, steam_id, created_at, legacy_note
             ) VALUES (?1, 'legacy-player', 7654321, '2024-02-03 04:05:06', 'retain me')",
            [101_i64],
        )
        .expect("insert historical player");
    connection
        .execute(
            "INSERT INTO matches(
                 match_id, team1_players, team2_players, winning_team, match_date
             ) VALUES (7, '[101]', '[202]', NULL, '2024-02-04 05:06:07')",
            [],
        )
        .expect("insert historical match");
    connection
        .execute(
            "INSERT INTO match_participants(match_id, discord_id, team_number, won, side)
             VALUES (7, 101, 1, 1, 'radiant')",
            [],
        )
        .expect("insert historical participant");
    drop(connection);

    let report = initialize_or_migrate(file.path()).expect("migrate historical base schema");
    assert!(report.rebuilt_tables.contains(&"players".to_owned()));
    assert!(report.rebuilt_tables.contains(&"matches".to_owned()));

    let connection = Connection::open(file.path()).expect("inspect migrated historical data");
    let player: (String, i64, Option<String>, String) = connection
        .query_row(
            "SELECT discord_username, guild_id, last_match_date, legacy_note
             FROM players WHERE discord_id=101 AND guild_id=0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read preserved player");
    assert_eq!(
        player,
        (
            "legacy-player".to_owned(),
            0,
            Some("2024-02-03 04:05:06".to_owned()),
            "retain me".to_owned()
        )
    );
    let steam_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM player_steam_ids
             WHERE discord_id=101 AND steam_id=7654321 AND is_primary=1",
            [],
            |row| row.get(0),
        )
        .expect("read migrated Steam ID");
    assert_eq!(steam_count, 1);
    let participant_guild: i64 = connection
        .query_row(
            "SELECT guild_id FROM match_participants
             WHERE match_id=7 AND discord_id=101",
            [],
            |row| row.get(0),
        )
        .expect("read migrated participant");
    assert_eq!(participant_guild, 0);
    drop(connection);

    let audit = audit_database(file.path()).expect("audit migrated historical fixture");
    assert!(audit.is_compatible(), "{:?}", audit.issues());
}

#[test]
fn failed_pending_batch_rolls_back_and_retry_succeeds() {
    let file = empty_database();
    let error = initialize_or_migrate_inner(file.path(), &MigrationSettings::default(), Some(3))
        .expect_err("inject migration interruption");
    assert!(matches!(error, SchemaMigrationError::InjectedFailure(3)));

    let connection = Connection::open(file.path()).expect("inspect rolled-back fixture");
    let ledger_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count ledger rows");
    let leaked_tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type='table' AND name!='schema_migrations'",
            [],
            |row| row.get(0),
        )
        .expect("count rolled-back schema objects");
    assert_eq!(ledger_count, 0);
    assert_eq!(leaked_tables, 0);
    drop(connection);

    let retry = initialize_or_migrate(file.path()).expect("retry complete migration batch");
    assert_eq!(retry.newly_applied.len(), expected_migrations().len());
    assert!(
        audit_database(file.path())
            .expect("audit retry result")
            .is_compatible()
    );
}

#[test]
fn interrupted_legacy_rebuilds_merge_old_and_live_rows() {
    let file = empty_database();
    let connection = Connection::open(file.path()).expect("open interrupted rebuild fixture");
    connection
        .execute_batch(
            "CREATE TABLE pending_matches_old (
                 guild_id INTEGER PRIMARY KEY,
                 payload TEXT NOT NULL,
                 updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
             );
             CREATE TABLE pending_matches (
                 pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 guild_id INTEGER NOT NULL,
                 payload TEXT NOT NULL,
                 created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                 updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
             );
             INSERT INTO pending_matches_old(guild_id, payload, updated_at)
             VALUES (10, 'old-pending', '2024-01-01 00:00:00');
             INSERT INTO pending_matches(guild_id, payload, updated_at)
             VALUES (20, 'live-pending', '2024-01-02 00:00:00');

             CREATE TABLE dig_boss_echoes_old (
                 guild_id INTEGER NOT NULL,
                 depth INTEGER NOT NULL,
                 killer_discord_id INTEGER NOT NULL,
                 weakened_until INTEGER NOT NULL,
                 PRIMARY KEY (guild_id, depth)
             );
             CREATE TABLE dig_boss_echoes (
                 guild_id INTEGER NOT NULL,
                 boss_id TEXT NOT NULL,
                 depth INTEGER NOT NULL,
                 killer_discord_id INTEGER NOT NULL,
                 weakened_until INTEGER NOT NULL,
                 PRIMARY KEY (guild_id, boss_id)
             );
             INSERT INTO dig_boss_echoes_old
             VALUES (10, 25, 101, 1001);
             INSERT INTO dig_boss_echoes
             VALUES (20, 'crystalia', 50, 202, 2002);",
        )
        .expect("create both halves of interrupted rebuilds");
    drop(connection);

    initialize_or_migrate(file.path()).expect("resume interrupted rebuilds");

    let connection = Connection::open(file.path()).expect("inspect recovered rebuilds");
    let pending_rows: Vec<(i64, String)> = connection
        .prepare("SELECT guild_id, payload FROM pending_matches ORDER BY guild_id")
        .expect("prepare pending row query")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("read pending rows")
        .collect::<Result<_, _>>()
        .expect("collect pending rows");
    assert_eq!(
        pending_rows,
        vec![
            (10, "old-pending".to_owned()),
            (20, "live-pending".to_owned())
        ]
    );
    let echo_rows: Vec<(i64, String, i64)> = connection
        .prepare(
            "SELECT guild_id, boss_id, weakened_until
             FROM dig_boss_echoes ORDER BY guild_id",
        )
        .expect("prepare echo row query")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("read echo rows")
        .collect::<Result<_, _>>()
        .expect("collect echo rows");
    assert_eq!(
        echo_rows,
        vec![
            (10, "grothak".to_owned(), 1001),
            (20, "crystalia".to_owned(), 2002)
        ]
    );
    assert!(!table_exists(&connection, "pending_matches_old").expect("query old pending table"));
    assert!(!table_exists(&connection, "dig_boss_echoes_old").expect("query old echo table"));
}

#[test]
fn concurrent_initializers_serialize_one_pending_batch() {
    let file = empty_database();
    let path = file.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(2));
    let reports = std::thread::scope(|scope| {
        let handles = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    initialize_or_migrate(path).expect("concurrent initializer")
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("initializer thread"))
            .collect::<Vec<_>>()
    });

    assert_eq!(
        reports
            .iter()
            .filter(|report| !report.newly_applied.is_empty())
            .count(),
        1
    );
    assert_eq!(
        reports
            .iter()
            .map(|report| report.previously_applied)
            .sum::<usize>(),
        expected_migrations().len()
    );
    assert!(
        audit_database(file.path())
            .expect("audit concurrently initialized database")
            .is_compatible()
    );
}

fn assert_close(left: f64, right: f64, tolerance: f64, label: &str) {
    assert!(
        (left - right).abs() <= tolerance,
        "{label}: Rust={left:.17}, Python={right:.17}"
    );
}

fn v4_streak_replay_snapshot(path: &Path) -> (Vec<String>, Vec<String>, Vec<String>) {
    let connection = Connection::open(path).expect("open v4 replay snapshot");
    let players = connection
        .prepare(
            "SELECT discord_id,os_mu,os_sigma,os_rating_version,os_algorithm_fingerprint
             FROM players WHERE guild_id=42 AND discord_id BETWEEN 99000 AND 99009
             ORDER BY discord_id",
        )
        .expect("prepare v4 players")
        .query_map([], |row| {
            Ok(format!(
                "{}|{:.17}|{:.17}|{}|{}",
                row.get::<_, i64>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?
            ))
        })
        .expect("read v4 players")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect v4 players");
    let history = connection
        .prepare(
            "SELECT match_id,discord_id,os_mu_before,os_mu_after,
                    os_sigma_before,os_sigma_after,os_algorithm_version,
                    os_algorithm_fingerprint,streak_multiplier_per_game
             FROM rating_history
             WHERE guild_id=42 AND match_id BETWEEN 99100 AND 99103
             ORDER BY match_id,discord_id",
        )
        .expect("prepare v4 history")
        .query_map([], |row| {
            Ok(format!(
                "{}|{}|{:.17}|{:.17}|{:.17}|{:.17}|{}|{}|{:.17}",
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, f64>(4)?,
                row.get::<_, f64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, f64>(8)?
            ))
        })
        .expect("read v4 history")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect v4 history");
    let predictions = connection
        .prepare(
            "SELECT match_id,openskill_algorithm_version,
                    openskill_algorithm_fingerprint,openskill_radiant_win_prob,
                    openskill_raw_radiant_win_prob
             FROM match_predictions WHERE match_id BETWEEN 99100 AND 99103
             ORDER BY match_id",
        )
        .expect("prepare v4 predictions")
        .query_map([], |row| {
            Ok(format!(
                "{}|{}|{}|{:.17}|{:.17}",
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, f64>(4)?
            ))
        })
        .expect("read v4 predictions")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect v4 predictions");
    (players, history, predictions)
}

#[test]
fn test_v4_migration_replays_recorded_streak_rates_idempotently() {
    const GUILD: i64 = 42;
    const MIGRATION: &str = "openskill_v4_streak_replay";
    let player_ids = (99_000..99_010).collect::<Vec<_>>();
    let file = empty_database();
    initialize_or_migrate(file.path()).expect("initialize current schema");
    let connection = Connection::open(file.path()).expect("open v4 replay fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production migration connection");
    for &player_id in &player_ids {
        connection
            .execute(
                "INSERT INTO players(
                     discord_id,guild_id,discord_username,initial_mmr,
                     os_mu,os_sigma,os_rating_version
                 ) VALUES(?1,?2,?3,3000,999,1,3)",
                params![player_id, GUILD, format!("StreakReplay{player_id}")],
            )
            .expect("insert v3 replay player");
    }
    for (offset, streak_rate) in [0.10, 0.10, 0.10, 0.30].into_iter().enumerate() {
        let match_id = 99_100 + i64::try_from(offset).expect("small match offset");
        let match_date = format!("2026-01-0{} 00:00:00", offset + 1);
        connection
            .execute(
                "INSERT INTO matches(
                     match_id,guild_id,team1_players,team2_players,
                     winning_team,match_date
                 ) VALUES(?1,?2,?3,?4,1,?5)",
                params![
                    match_id,
                    GUILD,
                    serde_json::to_string(&player_ids[..5]).unwrap(),
                    serde_json::to_string(&player_ids[5..]).unwrap(),
                    match_date
                ],
            )
            .expect("insert replay match");
        for (index, &player_id) in player_ids.iter().enumerate() {
            let team_number = if index < 5 { 1 } else { 2 };
            let won = i64::from(team_number == 1);
            connection
                .execute(
                    "INSERT INTO match_participants(
                         match_id,guild_id,discord_id,team_number,won
                     ) VALUES(?1,?2,?3,?4,?5)",
                    params![match_id, GUILD, player_id, team_number, won],
                )
                .expect("insert replay participant");
            connection
                .execute(
                    "INSERT INTO rating_history(
                         discord_id,guild_id,match_id,timestamp,team_number,won,
                         os_mu_before,os_mu_after,os_sigma_before,os_sigma_after,
                         os_algorithm_version,streak_multiplier_per_game
                     ) VALUES(?1,?2,?3,?4,?5,?6,999,999,1,1,3,?7)",
                    params![
                        player_id,
                        GUILD,
                        match_id,
                        match_date,
                        team_number,
                        won,
                        streak_rate
                    ],
                )
                .expect("insert replay history");
        }
    }
    connection
        .execute("DELETE FROM schema_migrations WHERE name=?1", [MIGRATION])
        .expect("make v4 replay pending");
    drop(connection);

    initialize_or_migrate(file.path()).expect("run v4 streak replay");
    let first = v4_streak_replay_snapshot(file.path());
    assert_eq!(first.0.len(), 10);
    assert_eq!(first.1.len(), 40);
    assert_eq!(first.2.len(), 4);
    assert!(
        first
            .0
            .iter()
            .all(|row| !row.contains("|999.00000000000000000|"))
    );
    let connection = Connection::open(file.path()).expect("inspect v4 replay marker");
    assert!(
        connection
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE name=?1",
                [MIGRATION],
                |_| Ok(())
            )
            .is_ok()
    );
    connection
        .execute("DELETE FROM schema_migrations WHERE name=?1", [MIGRATION])
        .expect("repeat v4 replay");
    drop(connection);

    initialize_or_migrate(file.path()).expect("repeat v4 streak replay");
    assert_eq!(v4_streak_replay_snapshot(file.path()), first);
}

#[test]
fn test_registration_after_v5_upgrade_stamps_current_algorithm_identity() {
    const GUILD: i64 = 987_654_321;
    const LEGACY_PLAYER: i64 = 99_498;
    const NEW_PLAYER: i64 = 99_499;
    let file = empty_database();
    initialize_or_migrate(file.path()).expect("initialize current schema");
    let connection = Connection::open(file.path()).expect("open v5 registration fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production migration connection");
    connection
        .execute(
            "INSERT INTO players (
                 discord_id,guild_id,discord_username,initial_mmr,
                 os_mu,os_sigma,os_rating_version
             ) VALUES (?1,?2,'LegacyV4Player',3000,25,8.333,4)",
            params![LEGACY_PLAYER, GUILD],
        )
        .expect("insert legacy v4 player");
    connection
        .execute_batch(
            "ALTER TABLE players RENAME COLUMN os_rating_version TO replaced_rating_version;
             ALTER TABLE players ADD COLUMN os_rating_version INTEGER NOT NULL DEFAULT 3;
             ALTER TABLE players DROP COLUMN replaced_rating_version;",
        )
        .expect("simulate legacy algorithm-version default");
    connection
        .execute(
            "UPDATE players
             SET os_rating_version=4,
                 os_algorithm_fingerprint='legacy-v4-fingerprint'
             WHERE discord_id=?1 AND guild_id=?2",
            params![LEGACY_PLAYER, GUILD],
        )
        .expect("restore legacy player identity");
    connection
        .execute(
            "DELETE FROM schema_migrations
             WHERE name='openskill_v5_gain_multiplier_replay'",
            [],
        )
        .expect("make v5 migration pending");
    drop(connection);

    initialize_or_migrate(file.path()).expect("upgrade through v5 replay");
    PlayerRepository::new(file.path())
        .add(&NewPlayer::new(
            NEW_PLAYER,
            "RegisteredAfterV5",
            Some(GUILD),
        ))
        .expect("register post-upgrade player");

    let identity: (i64, String) = Connection::open(file.path())
        .expect("open upgraded registration fixture")
        .query_row(
            "SELECT os_rating_version,os_algorithm_fingerprint
             FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![NEW_PLAYER, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("load post-upgrade identity");
    assert_eq!(identity.0, OPENSKILL_ALGORITHM_VERSION);
    assert_eq!(
        identity.1,
        CamaOpenSkillSystem::default().algorithm_fingerprint()
    );
}

#[test]
fn test_v5_migration_replays_recorded_gain_multipliers() {
    const GUILD: i64 = 987_654_321;
    const MATCH_ID: i64 = 99_600;
    let player_ids = (99_500..99_510).collect::<Vec<_>>();
    let file = empty_database();
    initialize_or_migrate(file.path()).expect("initialize current schema");
    let connection = Connection::open(file.path()).expect("open v5 replay fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production migration connection");
    for &player_id in &player_ids {
        connection
            .execute(
                "INSERT INTO players (
                     discord_id,guild_id,discord_username,initial_mmr,
                     os_mu,os_sigma,os_rating_version
                 ) VALUES (?1,?2,?3,3000,999,1,4)",
                params![player_id, GUILD, format!("GainReplay{player_id}")],
            )
            .expect("insert legacy replay player");
    }
    connection
        .execute(
            "INSERT INTO matches (
                 match_id,guild_id,team1_players,team2_players,
                 winning_team,match_date
             ) VALUES (?1,?2,?3,?4,1,'2026-08-10 12:00:00')",
            params![
                MATCH_ID,
                GUILD,
                serde_json::to_string(&player_ids[..5]).unwrap(),
                serde_json::to_string(&player_ids[5..]).unwrap()
            ],
        )
        .expect("insert legacy replay match");
    for (index, &player_id) in player_ids.iter().enumerate() {
        let team_number = if index < 5 { 1 } else { 2 };
        let won = i64::from(team_number == 1);
        let multiplier = if player_id == player_ids[0] {
            1.10
        } else {
            1.0
        };
        connection
            .execute(
                "INSERT INTO match_participants (
                     match_id,guild_id,discord_id,team_number,won
                 ) VALUES (?1,?2,?3,?4,?5)",
                params![MATCH_ID, GUILD, player_id, team_number, won],
            )
            .expect("insert legacy replay participant");
        connection
            .execute(
                "INSERT INTO rating_history (
                     discord_id,guild_id,match_id,timestamp,team_number,won,
                     os_mu_before,os_mu_after,os_sigma_before,os_sigma_after,
                     os_algorithm_version,low_priority_gain_multiplier
                 ) VALUES (
                     ?1,?2,?3,'2026-08-10 12:00:00',?4,?5,
                     999,999,1,1,4,?6
                 )",
                params![player_id, GUILD, MATCH_ID, team_number, won, multiplier],
            )
            .expect("insert legacy replay history");
    }
    connection
        .execute(
            "DELETE FROM schema_migrations
             WHERE name='openskill_v5_gain_multiplier_replay'",
            [],
        )
        .expect("make v5 replay pending");
    drop(connection);

    initialize_or_migrate(file.path()).expect("run v5 gain-multiplier replay");
    let connection = Connection::open(file.path()).expect("inspect v5 replay");
    assert!(
        connection
            .query_row(
                "SELECT 1 FROM schema_migrations
                 WHERE name='openskill_v5_gain_multiplier_replay'",
                [],
                |_| Ok(()),
            )
            .is_ok()
    );
    let history = connection
        .prepare(
            "SELECT discord_id,os_mu_before,os_mu_after,
                    os_algorithm_version,os_algorithm_fingerprint,
                    low_priority_gain_multiplier
             FROM rating_history
             WHERE guild_id=?1 AND match_id=?2 ORDER BY discord_id",
        )
        .expect("prepare v5 history read")
        .query_map(params![GUILD, MATCH_ID], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, f64>(5)?,
            ))
        })
        .expect("read v5 replay history")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect v5 replay history");
    assert_eq!(history.len(), 10);
    let target = &history[0];
    let peer = &history[1];
    assert_close(target.5, 1.10, 1.0e-12, "target gain multiplier");
    assert_close(peer.5, 1.0, 1.0e-12, "peer gain multiplier");
    assert_close(
        target.2 - target.1,
        (peer.2 - peer.1) * 1.10,
        1.0e-12,
        "positive gain replay",
    );
    assert!(
        history
            .iter()
            .all(|row| { row.3 == OPENSKILL_ALGORITHM_VERSION && !row.4.is_empty() })
    );
}

#[test]
fn migration_report_records_manifest_order() {
    let file = empty_database();
    let report = initialize_or_migrate(file.path()).expect("initialize manifest fixture");
    assert_eq!(report.newly_applied, expected_migrations());

    let connection = Connection::open(file.path()).expect("inspect applied rows");
    let final_name: String = connection
        .query_row(
            "SELECT name FROM schema_migrations ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("read final ledger row");
    assert_eq!(final_name, expected_migrations().last().copied().unwrap());

    let application_time_count: i64 = connection
        .query_row(
            "SELECT COUNT(DISTINCT applied_at) FROM schema_migrations",
            params![],
            |row| row.get(0),
        )
        .expect("count ledger timestamps");
    assert_eq!(application_time_count, 1);
}

#[test]
fn match_ban_migration_uses_python_numeric_projection() {
    assert!(json_false_or_zero(&serde_json::json!(false)));
    assert!(json_false_or_zero(&serde_json::json!(0.0)));
    assert!(!json_false_or_zero(&serde_json::json!("0")));
    assert_eq!(json_ban_integer(&serde_json::json!(1.9)), Some(1));
    assert_eq!(json_ban_integer(&serde_json::json!(" 7_400 ")), Some(7_400));
    assert_eq!(json_ban_integer(&serde_json::json!(true)), None);
}
