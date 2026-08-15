use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::schema_manager::initialize_or_migrate;

const TEST_GUILD_ID: i64 = 123;

fn empty_database() -> NamedTempFile {
    NamedTempFile::new().expect("create disposable database")
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root")
}

fn run_python(database: &Path, body: &str) {
    let script = format!(
        "import json,sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\np=sys.argv[1]\n{body}"
    );
    let output = Command::new("uv")
        .current_dir(repository_root())
        .args(["run", "--locked", "python", "-c", &script])
        .arg(database)
        .output()
        .expect("run Python schema authority");
    assert!(
        output.status.success(),
        "Python schema fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn python_fixture(body: &str) -> NamedTempFile {
    let file = NamedTempFile::new().expect("schema-manager fixture");
    run_python(file.path(), body);
    file
}

fn base_fixture() -> &'static NamedTempFile {
    static FIXTURE: OnceLock<NamedTempFile> = OnceLock::new();
    FIXTURE.get_or_init(|| python_fixture("SchemaManager(p).initialize()"))
}

fn copied_base_fixture() -> NamedTempFile {
    let file = NamedTempFile::new().expect("schema-manager fixture copy");
    std::fs::copy(base_fixture().path(), file.path()).expect("copy Python-migrated fixture");
    file
}

fn query_f64(file: &NamedTempFile, sql: &str) -> f64 {
    Connection::open(file.path())
        .expect("open fixture")
        .query_row(sql, [], |row| row.get(0))
        .expect("query f64")
}

fn table_column_contract(
    connection: &Connection,
    table: &str,
) -> Vec<(String, String, bool, Option<String>, i64)> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table contract");
    statement
        .query_map([], |row| {
            Ok((
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .expect("read table contract")
        .collect::<Result<_, _>>()
        .expect("collect table contract")
}

fn index_contract(connection: &Connection, table: &str) -> Vec<(bool, Vec<String>)> {
    let mut statement = connection
        .prepare(&format!("PRAGMA index_list({table})"))
        .expect("prepare index contract");
    let indexes: Vec<(String, bool)> = statement
        .query_map([], |row| Ok((row.get(1)?, row.get::<_, i64>(2)? != 0)))
        .expect("read index contract")
        .collect::<Result<_, _>>()
        .expect("collect index contract");

    let mut contract = indexes
        .into_iter()
        .map(|(name, unique)| {
            let mut columns_statement = connection
                .prepare(&format!("PRAGMA index_info({name})"))
                .expect("prepare index columns");
            let columns = columns_statement
                .query_map([], |row| row.get(2))
                .expect("read index columns")
                .collect::<Result<_, _>>()
                .expect("collect index columns");
            (unique, columns)
        })
        .collect::<Vec<(bool, Vec<String>)>>();
    contract.sort();
    contract
}

fn index_names(connection: &Connection, table: &str) -> BTreeSet<String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA index_list({table})"))
        .expect("prepare index names");
    statement
        .query_map([], |row| row.get(1))
        .expect("read index names")
        .collect::<Result<_, _>>()
        .expect("collect index names")
}

fn foreign_key_contract(connection: &Connection, table: &str) -> Vec<(String, String, String)> {
    let mut statement = connection
        .prepare(&format!("PRAGMA foreign_key_list({table})"))
        .expect("prepare foreign-key contract");
    let mut contract = statement
        .query_map([], |row| Ok((row.get(3)?, row.get(2)?, row.get(4)?)))
        .expect("read foreign-key contract")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect foreign-key contract");
    contract.sort();
    contract
}

fn seed_legacy_draft_parents(connection: &Connection) {
    connection
        .execute_batch(
            "CREATE TABLE pending_matches (
                 pending_match_id INTEGER PRIMARY KEY AUTOINCREMENT,
                 guild_id INTEGER NOT NULL,
                 payload TEXT NOT NULL,
                 created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                 updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                 completion_key TEXT
             );
             INSERT INTO pending_matches(
                 pending_match_id,guild_id,payload,completion_key
             ) VALUES (7,42,'{}','draft:42:1');
             CREATE TABLE draft_legacy_marker(marker TEXT NOT NULL);
             INSERT INTO draft_legacy_marker(marker) VALUES ('preserve me');",
        )
        .expect("seed legacy Draft parents");
}

fn expected_finalization_columns() -> Vec<(String, String, bool, Option<String>, i64)> {
    vec![
        (
            "completion_key".to_owned(),
            "TEXT".to_owned(),
            true,
            None,
            1,
        ),
        ("guild_id".to_owned(), "INTEGER".to_owned(), true, None, 0),
        ("session_id".to_owned(), "INTEGER".to_owned(), true, None, 0),
        (
            "pending_match_id".to_owned(),
            "INTEGER".to_owned(),
            true,
            None,
            0,
        ),
        (
            "revision".to_owned(),
            "INTEGER".to_owned(),
            true,
            Some("1".to_owned()),
            0,
        ),
        ("stage".to_owned(), "TEXT".to_owned(), true, None, 0),
        ("plan_json".to_owned(), "TEXT".to_owned(), true, None, 0),
        (
            "progress_json".to_owned(),
            "TEXT".to_owned(),
            true,
            Some("'{}'".to_owned()),
            0,
        ),
        ("lease_owner".to_owned(), "TEXT".to_owned(), false, None, 0),
        (
            "lease_until".to_owned(),
            "INTEGER".to_owned(),
            false,
            None,
            0,
        ),
        ("last_error".to_owned(), "TEXT".to_owned(), false, None, 0),
        (
            "created_at".to_owned(),
            "TIMESTAMP".to_owned(),
            true,
            Some("CURRENT_TIMESTAMP".to_owned()),
            0,
        ),
        (
            "updated_at".to_owned(),
            "TIMESTAMP".to_owned(),
            true,
            Some("CURRENT_TIMESTAMP".to_owned()),
            0,
        ),
        ("legacy_note".to_owned(), "TEXT".to_owned(), false, None, 0),
    ]
}

fn expected_financial_effect_columns() -> Vec<(String, String, bool, Option<String>, i64)> {
    vec![
        ("effect_key".to_owned(), "TEXT".to_owned(), true, None, 1),
        (
            "completion_key".to_owned(),
            "TEXT".to_owned(),
            true,
            None,
            0,
        ),
        ("guild_id".to_owned(), "INTEGER".to_owned(), true, None, 0),
        ("session_id".to_owned(), "INTEGER".to_owned(), true, None, 0),
        (
            "pending_match_id".to_owned(),
            "INTEGER".to_owned(),
            true,
            None,
            0,
        ),
        ("effect_kind".to_owned(), "TEXT".to_owned(), true, None, 0),
        ("ordinal".to_owned(), "INTEGER".to_owned(), true, None, 0),
        ("plan_sha256".to_owned(), "TEXT".to_owned(), true, None, 0),
        ("intended_json".to_owned(), "TEXT".to_owned(), true, None, 0),
        ("status".to_owned(), "TEXT".to_owned(), true, None, 0),
        ("receipt_json".to_owned(), "TEXT".to_owned(), true, None, 0),
        (
            "created_at".to_owned(),
            "TIMESTAMP".to_owned(),
            true,
            Some("CURRENT_TIMESTAMP".to_owned()),
            0,
        ),
        (
            "updated_at".to_owned(),
            "TIMESTAMP".to_owned(),
            true,
            Some("CURRENT_TIMESTAMP".to_owned()),
            0,
        ),
        ("legacy_note".to_owned(), "TEXT".to_owned(), false, None, 0),
    ]
}

#[test]
fn test_schema_manager_initializes_tables() {
    let audit = audit_existing_schema(base_fixture().path()).expect("audit Python schema");
    assert!(audit.is_compatible(), "{audit:?}");

    let connection = Connection::open(base_fixture().path()).expect("open fixture");
    connection
        .execute(
            "INSERT INTO reminder_preferences(discord_id,guild_id) VALUES (?1,?2)",
            params![123_i64, TEST_GUILD_ID],
        )
        .expect("insert reminder preference");
    let lobby_enabled: i64 = connection
        .query_row(
            "SELECT lobby_enabled FROM reminder_preferences
             WHERE discord_id=123 AND guild_id=?1",
            [TEST_GUILD_ID],
            |row| row.get(0),
        )
        .expect("read lobby default");
    assert_eq!(lobby_enabled, 0);
}

#[test]
fn test_draft_finalization_jobs_migration_is_additive_and_constrained() {
    let file = empty_database();
    {
        let connection = Connection::open(file.path()).expect("open legacy Draft fixture");
        seed_legacy_draft_parents(&connection);
        connection
            .execute_batch(
                "CREATE TABLE draft_finalization_jobs (
                     completion_key TEXT PRIMARY KEY NOT NULL,
                     guild_id INTEGER NOT NULL,
                     session_id INTEGER NOT NULL,
                     pending_match_id INTEGER NOT NULL,
                     stage TEXT NOT NULL,
                     plan_json TEXT NOT NULL,
                     legacy_note TEXT
                 );
                 INSERT INTO draft_finalization_jobs(
                     completion_key,guild_id,session_id,pending_match_id,
                     stage,plan_json,legacy_note
                 ) VALUES (
                     'draft:42:1',42,1,7,'linked','{\"future\":true}','retain me'
                 );",
            )
            .expect("seed legacy finalization job");
    }

    let report = initialize_or_migrate(file.path()).expect("migrate Draft finalization jobs");
    assert!(
        report
            .rebuilt_tables
            .contains(&"draft_finalization_jobs".to_owned()),
        "legacy target table must be rebuilt additively: {report:?}"
    );
    assert!(
        report
            .newly_applied
            .contains(&"create_draft_finalization_jobs".to_owned())
    );

    let connection = Connection::open(file.path()).expect("open migrated finalization jobs");
    assert_eq!(
        table_column_contract(&connection, "draft_finalization_jobs"),
        expected_finalization_columns()
    );
    assert_eq!(
        foreign_key_contract(&connection, "draft_finalization_jobs"),
        Vec::<(String, String, String)>::new()
    );
    let mut expected_indexes = vec![
        (
            false,
            vec![
                "stage".to_owned(),
                "lease_until".to_owned(),
                "updated_at".to_owned(),
            ],
        ),
        (true, vec!["completion_key".to_owned()]),
        (true, vec!["pending_match_id".to_owned()]),
        (true, vec!["guild_id".to_owned(), "session_id".to_owned()]),
    ];
    expected_indexes.sort();
    assert_eq!(
        index_contract(&connection, "draft_finalization_jobs"),
        expected_indexes
    );
    assert!(
        index_names(&connection, "draft_finalization_jobs")
            .contains("idx_draft_finalization_jobs_incomplete")
    );

    let preserved: (String, String, i64, String, String, Option<String>) = connection
        .query_row(
            "SELECT completion_key,stage,revision,plan_json,progress_json,legacy_note
             FROM draft_finalization_jobs WHERE completion_key='draft:42:1'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("read preserved finalization job");
    assert_eq!(
        preserved,
        (
            "draft:42:1".to_owned(),
            "linked".to_owned(),
            1,
            r#"{"future":true}"#.to_owned(),
            "{}".to_owned(),
            Some("retain me".to_owned()),
        )
    );
    assert_eq!(
        connection
            .query_row("SELECT marker FROM draft_legacy_marker", [], |row| row
                .get::<_, String>(
                0
            ),)
            .expect("read unrelated legacy marker"),
        "preserve me"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT payload FROM pending_matches WHERE pending_match_id=7",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("read preserved pending match"),
        "{}"
    );

    connection
        .execute(
            "INSERT INTO draft_finalization_jobs(
                 completion_key,guild_id,session_id,pending_match_id,stage,plan_json
             ) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                "draft:42:2",
                42_i64,
                2_i64,
                8_i64,
                "linked",
                r#"{"schema":1}"#
            ],
        )
        .expect("insert valid finalization job");
    let defaults: (i64, String, String, String, String) = connection
        .query_row(
            "SELECT revision,progress_json,created_at,updated_at,stage
             FROM draft_finalization_jobs WHERE completion_key='draft:42:2'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("read finalization defaults");
    assert_eq!(defaults.0, 1);
    assert_eq!(defaults.1, "{}");
    assert!(!defaults.2.is_empty());
    assert!(!defaults.3.is_empty());
    assert_eq!(defaults.4, "linked");

    for (label, sql) in [
        (
            "session check",
            "INSERT INTO draft_finalization_jobs(
                 completion_key,guild_id,session_id,pending_match_id,stage,plan_json
             ) VALUES ('bad-session',42,0,9,'linked','{}')",
        ),
        (
            "revision check",
            "INSERT INTO draft_finalization_jobs(
                 completion_key,guild_id,session_id,pending_match_id,revision,stage,plan_json
             ) VALUES ('bad-revision',42,3,10,0,'linked','{}')",
        ),
        (
            "stage check",
            "INSERT INTO draft_finalization_jobs(
                 completion_key,guild_id,session_id,pending_match_id,stage,plan_json
             ) VALUES ('bad-stage',42,4,11,'','{}')",
        ),
        (
            "plan object check",
            "INSERT INTO draft_finalization_jobs(
                 completion_key,guild_id,session_id,pending_match_id,stage,plan_json
             ) VALUES ('bad-plan',42,5,12,'linked','[]')",
        ),
        (
            "lease pair check",
            "INSERT INTO draft_finalization_jobs(
                 completion_key,guild_id,session_id,pending_match_id,stage,plan_json,lease_owner
             ) VALUES ('bad-lease',42,6,13,'linked','{}','worker-1')",
        ),
        (
            "duplicate pending-match check",
            "INSERT INTO draft_finalization_jobs(
                 completion_key,guild_id,session_id,pending_match_id,stage,plan_json
             ) VALUES ('duplicate-pending',42,7,8,'linked','{}')",
        ),
        (
            "duplicate guild-session check",
            "INSERT INTO draft_finalization_jobs(
                 completion_key,guild_id,session_id,pending_match_id,stage,plan_json
             ) VALUES ('duplicate-session',42,2,14,'linked','{}')",
        ),
    ] {
        assert!(
            connection.execute(sql, []).is_err(),
            "{label} must reject invalid row"
        );
    }
    assert!(
        connection
            .execute(
                "INSERT INTO draft_finalization_jobs(
                     completion_key,guild_id,session_id,pending_match_id,stage,plan_json,
                     lease_owner,lease_until
                 ) VALUES ('leased',42,8,15,'linked','{}','worker-1',12345)",
                [],
            )
            .is_ok(),
        "paired lease values must be accepted"
    );

    let second = initialize_or_migrate(file.path()).expect("repeat finalization migration");
    assert!(
        second.was_current(),
        "repeat migration should be current: {second:?}"
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM draft_finalization_jobs", [], |row| {
                row.get::<_, i64>(0)
            },)
            .expect("count finalization jobs"),
        3
    );
    let migrations = migration_ledger_snapshot(file.path()).expect("read migration ledger");
    assert!(migrations.names.contains("create_draft_finalization_jobs"));
}

#[test]
fn test_draft_financial_effects_migration_is_additive_and_constrained() {
    let file = empty_database();
    {
        let connection = Connection::open(file.path()).expect("open legacy Draft fixture");
        seed_legacy_draft_parents(&connection);
        connection
            .execute_batch(
                "CREATE TABLE draft_finalization_jobs (
                     completion_key TEXT PRIMARY KEY NOT NULL,
                     guild_id INTEGER NOT NULL,
                     session_id INTEGER NOT NULL,
                     pending_match_id INTEGER NOT NULL,
                     stage TEXT NOT NULL,
                     plan_json TEXT NOT NULL,
                     legacy_note TEXT
                 );
                 INSERT INTO draft_finalization_jobs(
                     completion_key,guild_id,session_id,pending_match_id,
                     stage,plan_json,legacy_note
                 ) VALUES (
                     'draft:42:1',42,1,7,'linked','{\"schema_version\":1}','retain job'
                 );
                 CREATE TABLE draft_financial_effects (
                     effect_key TEXT PRIMARY KEY NOT NULL,
                     completion_key TEXT NOT NULL,
                     guild_id INTEGER NOT NULL,
                     session_id INTEGER NOT NULL,
                     pending_match_id INTEGER NOT NULL,
                     effect_kind TEXT NOT NULL,
                     ordinal INTEGER NOT NULL,
                     plan_sha256 TEXT NOT NULL,
                     intended_json TEXT NOT NULL,
                     status TEXT NOT NULL,
                     receipt_json TEXT NOT NULL,
                     legacy_note TEXT
                 );
                 INSERT INTO draft_financial_effects(
                     effect_key,completion_key,guild_id,session_id,pending_match_id,
                     effect_kind,ordinal,plan_sha256,intended_json,status,receipt_json,
                     legacy_note
                 ) VALUES (
                     'draft:42:1:seed:0','draft:42:1',42,1,7,
                     'seed',0,
                     '0000000000000000000000000000000000000000000000000000000000000000',
                     '{\"reserved\":0}','applied','{\"reserved\":0}','retain effect'
                 );",
            )
            .expect("seed legacy financial effect");
    }

    let report = initialize_or_migrate(file.path()).expect("migrate Draft financial effects");
    assert!(
        report
            .rebuilt_tables
            .contains(&"draft_financial_effects".to_owned()),
        "legacy target table must be rebuilt additively: {report:?}"
    );
    assert!(
        report
            .newly_applied
            .contains(&"create_draft_financial_effects".to_owned())
    );

    let connection = Connection::open(file.path()).expect("open migrated financial effects");
    assert_eq!(
        table_column_contract(&connection, "draft_financial_effects"),
        expected_financial_effect_columns()
    );
    assert_eq!(
        foreign_key_contract(&connection, "draft_financial_effects"),
        vec![
            (
                "completion_key".to_owned(),
                "draft_finalization_jobs".to_owned(),
                "completion_key".to_owned(),
            ),
            (
                "pending_match_id".to_owned(),
                "pending_matches".to_owned(),
                "pending_match_id".to_owned(),
            ),
        ]
    );
    let mut expected_indexes = vec![
        (
            false,
            vec!["completion_key".to_owned(), "ordinal".to_owned()],
        ),
        (true, vec!["effect_key".to_owned()]),
        (
            true,
            vec![
                "completion_key".to_owned(),
                "effect_kind".to_owned(),
                "ordinal".to_owned(),
            ],
        ),
    ];
    expected_indexes.sort();
    assert_eq!(
        index_contract(&connection, "draft_financial_effects"),
        expected_indexes
    );
    assert!(
        index_names(&connection, "draft_financial_effects")
            .contains("idx_draft_financial_effects_completion")
    );

    let preserved: (String, String, String, Option<String>) = connection
        .query_row(
            "SELECT effect_key,intended_json,receipt_json,legacy_note
             FROM draft_financial_effects WHERE effect_key='draft:42:1:seed:0'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read preserved financial effect");
    assert_eq!(
        preserved,
        (
            "draft:42:1:seed:0".to_owned(),
            r#"{"reserved":0}"#.to_owned(),
            r#"{"reserved":0}"#.to_owned(),
            Some("retain effect".to_owned()),
        )
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT completion_key,legacy_note FROM draft_finalization_jobs",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .expect("read preserved parent job"),
        ("draft:42:1".to_owned(), Some("retain job".to_owned()))
    );

    connection
        .pragma_update(None, "foreign_keys", true)
        .expect("enable foreign keys for constraint checks");
    connection
        .execute(
            "INSERT INTO draft_financial_effects(
                 effect_key,completion_key,guild_id,session_id,pending_match_id,
                 effect_kind,ordinal,plan_sha256,intended_json,status,receipt_json
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                "draft:42:1:blind:1",
                "draft:42:1",
                42_i64,
                1_i64,
                7_i64,
                "blind",
                1_i64,
                "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd",
                r#"{"amount":1}"#,
                "skipped",
                r#"{"amount":1}"#,
            ],
        )
        .expect("insert valid financial effect");
    let defaults: (String, String) = connection
        .query_row(
            "SELECT created_at,updated_at FROM draft_financial_effects
             WHERE effect_key='draft:42:1:blind:1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read financial-effect defaults");
    assert!(!defaults.0.is_empty());
    assert!(!defaults.1.is_empty());

    let invalid_rows = [
        (
            "session check",
            "'bad-session','draft:42:1',42,0,7,'seed',2,
             '0000000000000000000000000000000000000000000000000000000000000000','{}','applied','{}'",
        ),
        (
            "effect-kind check",
            "'bad-kind','draft:42:1',42,1,7,'other',3,
             '0000000000000000000000000000000000000000000000000000000000000000','{}','applied','{}'",
        ),
        (
            "ordinal check",
            "'bad-ordinal','draft:42:1',42,1,7,'seed',-1,
             '0000000000000000000000000000000000000000000000000000000000000000','{}','applied','{}'",
        ),
        (
            "hash length check",
            "'bad-hash-length','draft:42:1',42,1,7,'seed',4,'abc','{}','applied','{}'",
        ),
        (
            "hash alphabet check",
            "'bad-hash-alphabet','draft:42:1',42,1,7,'seed',5,
             '000000000000000000000000000000000000000000000000000000000000000G','{}','applied','{}'",
        ),
        (
            "intended object check",
            "'bad-intended','draft:42:1',42,1,7,'seed',6,
             '0000000000000000000000000000000000000000000000000000000000000000','[]','applied','{}'",
        ),
        (
            "status check",
            "'bad-status','draft:42:1',42,1,7,'seed',7,
             '0000000000000000000000000000000000000000000000000000000000000000','{}','pending','{}'",
        ),
        (
            "receipt object check",
            "'bad-receipt','draft:42:1',42,1,7,'seed',8,
             '0000000000000000000000000000000000000000000000000000000000000000','{}','applied','[]'",
        ),
        (
            "duplicate effect identity check",
            "'bad-duplicate','draft:42:1',42,1,7,'blind',1,
             '0000000000000000000000000000000000000000000000000000000000000000','{}','applied','{}'",
        ),
        (
            "completion foreign key check",
            "'bad-completion','missing',42,1,7,'seed',9,
             '0000000000000000000000000000000000000000000000000000000000000000','{}','applied','{}'",
        ),
        (
            "pending-match foreign key check",
            "'bad-pending','draft:42:1',42,1,999,'seed',10,
             '0000000000000000000000000000000000000000000000000000000000000000','{}','applied','{}'",
        ),
    ];
    for (label, values) in invalid_rows {
        let sql = format!(
            "INSERT INTO draft_financial_effects(
                 effect_key,completion_key,guild_id,session_id,pending_match_id,
                 effect_kind,ordinal,plan_sha256,intended_json,status,receipt_json
             ) VALUES ({values})"
        );
        assert!(
            connection.execute(&sql, []).is_err(),
            "{label} must reject invalid row"
        );
    }

    let second = initialize_or_migrate(file.path()).expect("repeat financial-effects migration");
    assert!(
        second.was_current(),
        "repeat migration should be current: {second:?}"
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM draft_financial_effects", [], |row| {
                row.get::<_, i64>(0)
            },)
            .expect("count financial effects"),
        2
    );
    let migrations = migration_ledger_snapshot(file.path()).expect("read migration ledger");
    assert!(migrations.names.contains("create_draft_financial_effects"));
}

#[test]
fn test_schema_manager_drops_retired_tables() {
    let file = python_fixture(
        r#"
m=SchemaManager(p); m.initialize(); c=sqlite3.connect(p)
c.execute('CREATE TABLE wheel_wars(war_id INTEGER PRIMARY KEY)')
c.execute('CREATE TABLE war_bets(bet_id INTEGER PRIMARY KEY)')
m._migration_create_protected_hero_purchases_table(c.cursor())
m._migration_create_curses_table(c.cursor())
c.execute("DELETE FROM schema_migrations WHERE name IN ('drop_retired_wheel_war_tables','drop_protected_hero_purchases_table','drop_curses_table')")
c.commit(); c.close(); m.initialize()
"#,
    );
    let audit = audit_existing_schema(file.path()).expect("audit retired tables");
    assert!(audit.retired_tables.is_empty(), "{audit:?}");
}

#[test]
fn test_schema_manager_adds_lobby_target_subscription_table_and_index() {
    let audit = audit_existing_schema(base_fixture().path()).expect("audit subscriptions");
    assert!(audit.missing_tables.is_empty(), "{audit:?}");
    assert!(audit.malformed_columns.is_empty(), "{audit:?}");

    let connection = Connection::open(base_fixture().path()).expect("open fixture");
    let columns: Vec<(String, i64)> = connection
        .prepare("PRAGMA table_info(lobby_target_subscriptions)")
        .expect("prepare table info")
        .query_map([], |row| Ok((row.get(1)?, row.get(5)?)))
        .expect("query columns")
        .collect::<Result<_, _>>()
        .expect("collect columns");
    assert_eq!(
        columns,
        [
            ("guild_id".to_owned(), 1),
            ("target_id".to_owned(), 2),
            ("subscriber_id".to_owned(), 3),
            ("created_at".to_owned(), 0),
        ]
    );
}

#[test]
fn test_schema_manager_initialize_is_idempotent() {
    let file =
        python_fixture("manager=SchemaManager(p)\nmanager.initialize()\nmanager.initialize()");
    let ledger = migration_ledger_snapshot(file.path()).expect("read migration ledger");
    assert!(ledger.applied_count > 0);
    assert_eq!(ledger.applied_count, ledger.distinct_count);
}

#[test]
fn test_streak_rate_migration_backfills_legacy_history() {
    let file = python_fixture(
        r#"
c=sqlite3.connect(p); c.row_factory=sqlite3.Row
c.execute('CREATE TABLE rating_history(id INTEGER PRIMARY KEY AUTOINCREMENT,streak_length INTEGER,streak_multiplier REAL)')
c.execute('INSERT INTO rating_history(streak_length,streak_multiplier) VALUES(4,1.40)')
SchemaManager(p)._migration_add_streak_multiplier_per_game_to_rating_history(c.cursor())
c.commit(); c.close()
"#,
    );
    assert!(
        (query_f64(
            &file,
            "SELECT streak_multiplier_per_game FROM rating_history"
        ) - 0.20)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn test_base_delta_multiplier_migration_backfills_legacy_history() {
    let file = python_fixture(
        r#"
c=sqlite3.connect(p); c.row_factory=sqlite3.Row
c.execute('CREATE TABLE rating_history(id INTEGER PRIMARY KEY AUTOINCREMENT,rating REAL)')
c.execute('INSERT INTO rating_history(rating) VALUES(1510.0)')
SchemaManager(p)._migration_add_base_rating_delta_multiplier_to_rating_history(c.cursor())
c.commit(); c.close()
"#,
    );
    assert!(
        (query_f64(
            &file,
            "SELECT base_rating_delta_multiplier FROM rating_history"
        ) - 0.75)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn test_low_priority_gain_migration_keeps_legacy_history_unboosted() {
    let file = python_fixture(
        r#"
c=sqlite3.connect(p); c.row_factory=sqlite3.Row
c.execute('CREATE TABLE rating_history(id INTEGER PRIMARY KEY AUTOINCREMENT,rating REAL)')
c.execute('INSERT INTO rating_history(rating) VALUES(1510.0)')
SchemaManager(p)._migration_add_low_priority_gain_multiplier_to_rating_history(c.cursor())
c.commit(); c.close()
"#,
    );
    assert!(
        (query_f64(
            &file,
            "SELECT low_priority_gain_multiplier FROM rating_history"
        ) - 1.0)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn test_wrapped_enrichment_facts_migration_backfills_safely_and_idempotently() {
    let file = python_fixture(
        r#"
m=SchemaManager(p); m.initialize()
c=sqlite3.connect(p)
c.executemany('INSERT INTO players(discord_id,guild_id,discord_username,steam_id) VALUES(?,?,?,?)',[(100,123,'matched',99999),(200,123,'unmatched',None),(300,123,'malformed',None)])
c.execute('INSERT INTO player_steam_ids(discord_id,steam_id,is_primary,added_at) VALUES(100,12345,1,1)')
c.executemany("INSERT INTO matches(match_id,guild_id,team1_players,team2_players,winning_team) VALUES(?,?,?,?,1)",[(9876,123,'[100]','[200]'),(9877,123,'[300]','[]')])
c.executemany("INSERT INTO match_participants(match_id,discord_id,guild_id,team_number,won,side) VALUES(?,?,?,?,?,?)",[(9876,100,123,1,1,'radiant'),(9876,200,123,2,0,'dire'),(9877,300,123,1,1,'radiant')])
payload={'players':[{'account_id':12345,'actions_per_min':321.5,'courier_kills':2,'pings':44,'lane_role':2,'purchase_log':[{'key':'rapier'},{'key':'ward_observer'},{'key':'rapier'}]}],'comeback':9000,'throw':4000}
c.execute('UPDATE matches SET enrichment_data=? WHERE match_id=9876',(json.dumps(payload),))
c.execute("UPDATE matches SET enrichment_data='{not-json' WHERE match_id=9877")
c.execute('DROP TABLE wrapped_enrichment_facts')
c.execute("DELETE FROM schema_migrations WHERE name='create_wrapped_enrichment_facts'")
c.commit(); c.close(); m.initialize()
c=sqlite3.connect(p)
c.execute('UPDATE wrapped_enrichment_facts SET actions_per_min=-1 WHERE guild_id=123 AND match_id=9876 AND discord_id=100')
c.execute("DELETE FROM schema_migrations WHERE name='create_wrapped_enrichment_facts'")
c.commit(); c.close(); m.initialize()
"#,
    );
    let facts = load_wrapped_enrichment_facts(file.path(), 123, 9876).expect("read facts");
    assert_eq!(facts.len(), 2);
    assert_eq!(
        facts[0],
        WrappedEnrichmentFact {
            discord_id: 100,
            actions_per_min: Some(321.5),
            courier_kills: Some(2),
            pings: Some(44),
            rapier_count: 2,
            lane_role: Some(2),
            comeback: Some(9000.0),
            throw: Some(4000.0),
        }
    );
    assert_eq!(
        facts[1],
        WrappedEnrichmentFact {
            discord_id: 200,
            actions_per_min: None,
            courier_kills: None,
            pings: None,
            rapier_count: 0,
            lane_role: None,
            comeback: Some(9000.0),
            throw: Some(4000.0),
        }
    );
    assert!(
        load_wrapped_enrichment_facts(file.path(), 123, 9877)
            .expect("read malformed projection")
            .is_empty()
    );
}

#[test]
fn test_scout_ban_migration_backfills_existing_payloads() {
    let file = copied_base_fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute(
            "INSERT INTO matches (
                 match_id,guild_id,team1_players,team2_players,winning_team,
                 enrichment_data
             ) VALUES (?1,?2,'[101]','[202]',1,?3)",
            params![
                98_786_i64,
                TEST_GUILD_ID,
                r#"{"picks_bans":[{"is_pick":false,"team":1,"hero_id":10}]}"#,
            ],
        )
        .expect("insert legacy enriched match");
    connection
        .execute(
            "INSERT INTO match_participants (
                 match_id,discord_id,team_number,won,side,guild_id
             ) VALUES (?1,101,1,1,'radiant',?2)",
            params![98_786_i64, TEST_GUILD_ID],
        )
        .expect("insert legacy participant");
    connection
        .execute(
            "DELETE FROM schema_migrations WHERE name='create_match_bans_for_scout'",
            [],
        )
        .expect("rewind Scout migration ledger");
    connection
        .execute("DROP TABLE match_bans", [])
        .expect("drop normalized projection");
    connection
        .execute("DROP INDEX IF EXISTS idx_match_participants_scout", [])
        .expect("drop Scout participant index");
    drop(connection);

    crate::schema_manager::initialize_or_migrate(file.path())
        .expect("Rust replays Scout projection migration");
    let bans = crate::scout_repository::ScoutRepository::new(file.path())
        .get_bans_for_players(&[101], Some(TEST_GUILD_ID))
        .expect("read backfilled Scout bans");
    assert_eq!(bans, std::collections::BTreeMap::from([(10, 1)]));
}

#[test]
fn test_prediction_probability_migration_recomputes_history_symmetrically() {
    let file = python_fixture(
        r#"
c=sqlite3.connect(p); c.row_factory=sqlite3.Row
c.execute('CREATE TABLE match_predictions(match_id INTEGER PRIMARY KEY,radiant_rating REAL,dire_rating REAL,radiant_rd REAL,dire_rd REAL,expected_radiant_win_prob REAL)')
c.execute('CREATE TABLE rating_history(discord_id INTEGER,match_id INTEGER,team_number INTEGER,expected_team_win_prob REAL)')
c.execute('INSERT INTO match_predictions VALUES(9876,1700.0,1500.0,350.0,50.0,0.7571404149989154)')
c.executemany('INSERT INTO rating_history VALUES(?,?,?,?)',[(1,9876,1,0.7571404149989154),(2,9876,2,0.31641538274428405)])
SchemaManager(p)._migration_recompute_glicko_prediction_probabilities(c.cursor())
c.commit(); c.close()
"#,
    );
    let connection = Connection::open(file.path()).expect("open fixture");
    let stored: f64 = connection
        .query_row(
            "SELECT expected_radiant_win_prob FROM match_predictions WHERE match_id=9876",
            [],
            |row| row.get(0),
        )
        .expect("stored prediction");
    let history: Vec<(i64, f64)> = connection
        .prepare(
            "SELECT team_number,expected_team_win_prob FROM rating_history
             WHERE match_id=9876 ORDER BY team_number",
        )
        .expect("prepare history")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query history")
        .collect::<Result<_, _>>()
        .expect("collect history");
    let expected = symmetric_prediction_probabilities(1700.0, 350.0, 1500.0, 50.0);
    assert!((stored - expected.0).abs() < 1e-12);
    assert!((history[0].1 - expected.0).abs() < 1e-12);
    assert!((history[1].1 - expected.1).abs() < 1e-12);
}

#[test]
fn test_duration_migrations_cap_legacy_stacks() {
    let file = python_fixture(
        r#"
m=SchemaManager(p); m.initialize(); c=sqlite3.connect(p)
c.execute('INSERT INTO soft_avoids(guild_id,avoider_discord_id,avoided_discord_id,games_remaining,created_at,updated_at) VALUES(123,100,200,25,1,1)')
c.execute('DROP TRIGGER trg_package_deals_games_remaining_insert_cap')
c.execute('DROP TRIGGER trg_package_deals_games_remaining_update_cap')
c.execute('INSERT INTO package_deals(guild_id,buyer_discord_id,partner_discord_id,games_remaining,cost_paid,created_at,updated_at) VALUES(123,100,200,25,500,1,1)')
c.execute("DELETE FROM schema_migrations WHERE name IN ('cap_soft_avoid_games_remaining','cap_package_deal_games_remaining')")
c.commit(); c.close(); m.initialize()
"#,
    );
    let connection = Connection::open(file.path()).expect("open fixture");
    let avoid: i64 = connection
        .query_row("SELECT games_remaining FROM soft_avoids", [], |row| {
            row.get(0)
        })
        .expect("soft avoid duration");
    let deal: i64 = connection
        .query_row("SELECT games_remaining FROM package_deals", [], |row| {
            row.get(0)
        })
        .expect("package duration");
    assert_eq!((avoid, deal), (10, 10));
}

#[test]
fn test_package_deal_duration_cap_is_enforced_after_migrations() {
    let file = copied_base_fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute(
            "INSERT INTO package_deals(guild_id,buyer_discord_id,partner_discord_id,
             games_remaining,cost_paid,created_at,updated_at) VALUES(123,100,200,10,500,1,1)",
            [],
        )
        .expect("insert capped package deal");
    assert!(
        connection
            .execute(
                "INSERT INTO package_deals(guild_id,buyer_discord_id,partner_discord_id,
                 games_remaining,cost_paid,created_at,updated_at)
                 VALUES(123,300,400,11,500,1,1)",
                [],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "UPDATE package_deals SET games_remaining=11
                 WHERE guild_id=123 AND buyer_discord_id=100 AND partner_discord_id=200",
                [],
            )
            .is_err()
    );
}

#[test]
fn test_failed_pending_batch_rolls_back_and_retries_cleanly() {
    let file = python_fixture(
        r#"
m=SchemaManager(p)
def a(c): c.execute('CREATE TABLE synthetic_retry_a(value TEXT NOT NULL)'); c.execute("INSERT INTO synthetic_retry_a VALUES('a')")
def fail(c): c.execute('CREATE TABLE synthetic_retry_b(value TEXT NOT NULL)'); c.execute("INSERT INTO synthetic_retry_b VALUES('b')"); raise RuntimeError('synthetic migration B failed')
m._get_migrations=lambda:[('synthetic_retry_a',a),('synthetic_retry_b',fail)]
try: m.initialize()
except RuntimeError: pass
else: raise AssertionError('expected migration failure')
c=sqlite3.connect(p)
assert c.execute("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'synthetic_retry_%'").fetchall()==[]
assert c.execute("SELECT name FROM schema_migrations WHERE name LIKE 'synthetic_retry_%'").fetchall()==[]
c.close()
def b(c): c.execute('CREATE TABLE synthetic_retry_b(value TEXT NOT NULL)'); c.execute("INSERT INTO synthetic_retry_b VALUES('b')")
m._get_migrations=lambda:[('synthetic_retry_a',a),('synthetic_retry_b',b)]
m.initialize(); m.initialize()
"#,
    );
    let ledger = migration_ledger_snapshot(file.path()).expect("read synthetic ledger");
    assert_eq!(ledger.applied_count, 2);
    assert_eq!(ledger.distinct_count, 2);
    assert_eq!(
        ledger.names,
        BTreeSet::from([
            "synthetic_retry_a".to_owned(),
            "synthetic_retry_b".to_owned(),
        ])
    );
    let connection = Connection::open(file.path()).expect("open fixture");
    let counts: (i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM synthetic_retry_a),
                    (SELECT COUNT(*) FROM synthetic_retry_b)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("retry row counts");
    assert_eq!(counts, (1, 1));
}

#[test]
fn test_migration_normalize_null_guild_id_registered_and_safe_on_clean_db() {
    let file = copied_base_fixture();
    run_python(
        file.path(),
        "c=sqlite3.connect(p); c.row_factory=sqlite3.Row\nSchemaManager(p)._migration_normalize_null_guild_id_pairings_and_neon(c.cursor())\nc.commit(); c.close()",
    );
    let ledger = migration_ledger_snapshot(file.path()).expect("read migration ledger");
    assert!(
        ledger
            .names
            .contains("normalize_null_guild_id_pairings_and_neon")
    );
}

#[test]
fn test_economy_ledger_triggers_record_player_and_nonprofit_changes() {
    let file = python_fixture(
        r#"
m=SchemaManager(p); m.initialize(); c=sqlite3.connect(p)
c.execute("INSERT INTO players(discord_id,guild_id,discord_username) VALUES(111,123,'taxpayer')")
c.execute('UPDATE players SET jopacoin_balance=50 WHERE discord_id=111 AND guild_id=123')
c.execute('INSERT INTO nonprofit_fund(guild_id,total_collected) VALUES(123,20)')
c.commit(); c.close()
"#,
    );

    let rows = load_economy_ledger(file.path()).expect("read economy ledger");
    assert!(rows.contains(&EconomyLedgerRow {
        account_type: "player".to_owned(),
        account_id: 111,
        delta: 3,
        balance_before: 0,
        balance_after: 3,
        source: "player_insert".to_owned(),
    }));
    assert!(rows.contains(&EconomyLedgerRow {
        account_type: "player".to_owned(),
        account_id: 111,
        delta: 47,
        balance_before: 3,
        balance_after: 50,
        source: "balance_update".to_owned(),
    }));
    assert!(rows.contains(&EconomyLedgerRow {
        account_type: "nonprofit".to_owned(),
        account_id: 123,
        delta: 20,
        balance_before: 0,
        balance_after: 20,
        source: "nonprofit_insert".to_owned(),
    }));
}

#[test]
fn test_economy_ledger_migration_backfills_existing_balances() {
    let file = python_fixture(
        r#"
m=SchemaManager(p); m.initialize(); c=sqlite3.connect(p); c.row_factory=sqlite3.Row
c.execute("INSERT INTO players(discord_id,guild_id,discord_username) VALUES(222,123,'existing')")
c.execute('UPDATE players SET jopacoin_balance=77 WHERE discord_id=222 AND guild_id=123')
c.execute('INSERT INTO nonprofit_fund(guild_id,total_collected) VALUES(123,33)')
c.execute('DELETE FROM economy_ledger_entries')
m._migration_create_economy_ledger_tables(c.cursor()); c.commit(); c.close()
"#,
    );
    assert_eq!(
        load_economy_ledger(file.path()).expect("read backfill"),
        [
            EconomyLedgerRow {
                account_type: "player".to_owned(),
                account_id: 222,
                delta: 77,
                balance_before: 0,
                balance_after: 77,
                source: "ledger_backfill".to_owned(),
            },
            EconomyLedgerRow {
                account_type: "nonprofit".to_owned(),
                account_id: 123,
                delta: 33,
                balance_before: 0,
                balance_after: 33,
                source: "ledger_backfill".to_owned(),
            },
        ]
    );
}

#[test]
fn test_economy_event_severity_migration_preserves_history_and_allows_level_five() {
    let file = python_fixture(
        r#"
m=SchemaManager(p); m.initialize(); c=sqlite3.connect(p)
c.execute("INSERT INTO economy_daily_events(guild_id,event_date,name,hero,direction,severity,target_effect_jc,forecast_flow_jc,expected_effect_jc,monetary_stock_before,effects,announcement,starts_at,ends_at,created_at,announced_at) VALUES(123,'2026-07-28','Legacy Edict','Doom','deflationary',3,-30,40,-30,1000,'{}','Legacy announcement',100,200,90,110)")
c.execute('DROP INDEX idx_economy_events_active')
c.execute('ALTER TABLE economy_daily_events RENAME TO economy_daily_events_level_five')
sql=c.execute("SELECT sql FROM sqlite_master WHERE type='table' AND name='economy_daily_events_level_five'").fetchone()[0]
c.execute(sql.replace('economy_daily_events_level_five','economy_daily_events').replace('BETWEEN 1 AND 5','BETWEEN 1 AND 3'))
cols=[r[1] for r in c.execute('PRAGMA table_info(economy_daily_events_level_five)')]; names=', '.join(cols)
c.execute(f'INSERT INTO economy_daily_events({names}) SELECT {names} FROM economy_daily_events_level_five')
c.execute('DROP TABLE economy_daily_events_level_five')
c.execute("DELETE FROM schema_migrations WHERE name='expand_economy_event_severity_levels'")
c.commit(); c.close(); m.initialize(); c=sqlite3.connect(p)
c.execute("INSERT INTO economy_daily_events(guild_id,event_date,name,hero,direction,severity,target_effect_jc,forecast_flow_jc,expected_effect_jc,monetary_stock_before,effects,announcement,starts_at,ends_at,created_at) VALUES(123,'2026-07-29','Level Five Edict','Doom','deflationary',5,-50,60,-50,1000,'{}','Level five announcement',200,300,190)")
c.commit(); c.close()
"#,
    );
    let connection = Connection::open(file.path()).expect("open fixture");
    let legacy: (i64, String, i64, i64) = connection
        .query_row(
            "SELECT event_id,name,severity,announced_at FROM economy_daily_events
             WHERE event_date='2026-07-28'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("legacy event");
    assert_eq!(legacy, (1, "Legacy Edict".to_owned(), 3, 110));
    let level_five: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM economy_daily_events WHERE severity=5",
            [],
            |row| row.get(0),
        )
        .expect("level five event");
    assert_eq!(level_five, 1);
}

#[test]
fn test_followup_ledger_backfill_accounts_for_existing_deltas() {
    let file = python_fixture(
        r#"
m=SchemaManager(p); m.initialize(); c=sqlite3.connect(p); c.row_factory=sqlite3.Row
c.execute("INSERT INTO players(discord_id,guild_id,discord_username) VALUES(333,123,'partially-logged')")
c.execute('UPDATE players SET jopacoin_balance=100 WHERE discord_id=333 AND guild_id=123')
c.execute('DELETE FROM economy_ledger_entries')
c.execute("INSERT INTO economy_ledger_entries(guild_id,account_type,account_id,delta,balance_before,balance_after,source) VALUES(123,'player',333,25,75,100,'balance_update')")
m._migration_backfill_economy_ledger_opening_balances(c.cursor()); c.commit(); c.close()
"#,
    );
    assert_eq!(ledger_opening_balance(100, 25), 75);
    assert_eq!(
        load_economy_ledger(file.path()).expect("read follow-up backfill"),
        [
            EconomyLedgerRow {
                account_type: "player".to_owned(),
                account_id: 333,
                delta: 25,
                balance_before: 75,
                balance_after: 100,
                source: "balance_update".to_owned(),
            },
            EconomyLedgerRow {
                account_type: "player".to_owned(),
                account_id: 333,
                delta: 75,
                balance_before: 0,
                balance_after: 75,
                source: "ledger_backfill".to_owned(),
            },
        ]
    );
}

#[test]
fn test_tunnels_columns_stay_in_sync_with_dig_update_whitelist() {
    let audit = audit_existing_schema(base_fixture().path()).expect("audit tunnel contract");
    assert!(audit.tunnel_columns_without_writer.is_empty(), "{audit:?}");
    assert!(audit.tunnel_writers_without_column.is_empty(), "{audit:?}");
    assert!(
        audit.tunnel_integer_contract_without_column.is_empty(),
        "{audit:?}"
    );
}

#[test]
fn test_pickaxe_tier_renumber_migration_shifts_5_to_6_and_6_to_7() {
    let file = copied_base_fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute_batch(
            "DELETE FROM dig_gear;
             DELETE FROM tunnels;
             DELETE FROM schema_migrations
             WHERE name='renumber_pickaxe_tier_for_stormrend_insert';
             INSERT INTO tunnels(discord_id,guild_id,depth,pickaxe_tier)
             VALUES(1,0,0,5),(2,0,0,6),(3,0,0,4);
             INSERT INTO dig_gear(
                 discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source
             ) VALUES
                 (1,0,'weapon',5,20,0,0,'shop'),
                 (2,0,'armor',6,20,0,0,'shop'),
                 (3,0,'boots',4,20,0,0,'shop');",
        )
        .expect("seed pre-Stormrend tier values");
    drop(connection);

    let report = crate::schema_manager::initialize_or_migrate(file.path())
        .expect("apply pending Stormrend renumber migration");
    assert!(
        report
            .newly_applied
            .iter()
            .any(|name| name == "renumber_pickaxe_tier_for_stormrend_insert")
    );

    let connection = Connection::open(file.path()).expect("reopen migrated fixture");
    let tunnels = connection
        .prepare("SELECT discord_id,pickaxe_tier FROM tunnels ORDER BY discord_id")
        .expect("prepare tunnel tiers")
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .expect("query tunnel tiers")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect tunnel tiers");
    let gear = connection
        .prepare("SELECT discord_id,tier FROM dig_gear ORDER BY discord_id")
        .expect("prepare gear tiers")
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .expect("query gear tiers")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect gear tiers");
    assert_eq!(tunnels, [(1, 6), (2, 7), (3, 4)]);
    assert_eq!(gear, [(1, 6), (2, 7), (3, 4)]);
}

#[test]
fn test_pickaxe_tier_renumber_migration_is_listed() {
    assert!(crate::expected_migrations().contains(&"renumber_pickaxe_tier_for_stormrend_insert"));
}

#[test]
fn test_pickaxe_tier_renumber_migration_ledger_prevents_second_row_shift() {
    let file = copied_base_fixture();
    let connection = Connection::open(file.path()).expect("open fixture");
    connection
        .execute_batch(
            "DELETE FROM tunnels;
             INSERT INTO tunnels(discord_id,guild_id,depth,pickaxe_tier)
             VALUES(10,0,0,6),(11,0,0,7);",
        )
        .expect("seed canonical post-migration values");
    drop(connection);

    let report = crate::schema_manager::initialize_or_migrate(file.path())
        .expect("repeat schema initialization");
    assert!(report.newly_applied.is_empty());

    let connection = Connection::open(file.path()).expect("reopen fixture");
    let tunnels = connection
        .prepare("SELECT discord_id,pickaxe_tier FROM tunnels ORDER BY discord_id")
        .expect("prepare tunnel tiers")
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .expect("query tunnel tiers")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect tunnel tiers");
    assert_eq!(tunnels, [(10, 6), (11, 7)]);
}
