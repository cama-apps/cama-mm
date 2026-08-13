use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const TEST_GUILD_ID: i64 = 123;

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
