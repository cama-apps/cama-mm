use cama_domain::dig_economy::scale_positive_dig_jc;
use cama_domain::dig_splash::strengthen_dig_event_penalty;
use cama_domain::economy_scaling::{
    DEFAULT_MINIGAME_JC_DELTA_SCALE, scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta,
};
use rusqlite::{Connection, params};
use std::path::Path;
use std::process::Command;
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 12_345;
const OTHER_GUILD: i64 = 54_321;
const DIGGER: i64 = 10_001;
const NOW: i64 = 2_000_000_000;
const RECENT_ISO: &str = "2033-05-18T03:33:20+00:00";

struct Fixture {
    _database: NamedTempFile,
    repository: DigTunnelEncounterRepository,
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
                     jopacoin_balance INTEGER NOT NULL DEFAULT 0,
                     last_match_date TEXT,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE tunnels (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     depth INTEGER NOT NULL DEFAULT 0,
                     max_depth INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE dig_actions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     actor_id INTEGER NOT NULL,
                     target_id INTEGER,
                     action_type TEXT NOT NULL,
                     depth_before INTEGER NOT NULL DEFAULT 0,
                     depth_after INTEGER NOT NULL DEFAULT 0,
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
                     account_type TEXT NOT NULL,
                     account_id INTEGER,
                     delta INTEGER NOT NULL,
                     balance_before INTEGER NOT NULL,
                     balance_after INTEGER NOT NULL,
                     source TEXT NOT NULL,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT,
                     created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
                 );
                 CREATE TRIGGER ledger_player_update
                 AFTER UPDATE OF jopacoin_balance ON players
                 WHEN OLD.jopacoin_balance != NEW.jopacoin_balance
                 BEGIN
                     INSERT INTO economy_ledger_entries (
                         guild_id, account_type, account_id, delta,
                         balance_before, balance_after, source, actor_id,
                         related_type, related_id, reason, metadata
                     ) VALUES (
                         NEW.guild_id, 'player', NEW.discord_id,
                         NEW.jopacoin_balance - OLD.jopacoin_balance,
                         OLD.jopacoin_balance, NEW.jopacoin_balance,
                         COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1), 'balance_update'),
                         (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                         (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                         (SELECT reason FROM economy_ledger_context WHERE id = 1),
                         (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                     );
                 END;
                 CREATE TRIGGER force_encounter_audit_failure
                 BEFORE INSERT ON dig_actions
                 WHEN NEW.action_type = 'event' AND NEW.detail LIKE '%force-rollback%'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced encounter audit failure');
                 END;",
            )
            .expect("fixture schema");
        drop(connection);
        Self {
            repository: DigTunnelEncounterRepository::new(database.path()),
            _database: database,
        }
    }

    fn connection(&self) -> Connection {
        self.repository.connection().expect("fixture connection")
    }

    fn add_player(
        &self,
        discord_id: i64,
        guild_id: i64,
        balance: i64,
        last_match_date: Option<&str>,
    ) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username,
                     jopacoin_balance, last_match_date
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    discord_id,
                    guild_id,
                    format!("User{discord_id}"),
                    balance,
                    last_match_date
                ],
            )
            .expect("insert player");
    }

    fn add_tunnel(&self, discord_id: i64, guild_id: i64, depth: i64) {
        self.connection()
            .execute(
                "INSERT INTO tunnels (discord_id, guild_id, depth, max_depth)
                 VALUES (?1, ?2, ?3, ?3)",
                params![discord_id, guild_id, depth],
            )
            .expect("insert tunnel");
    }

    fn log_dig(&self, discord_id: i64, guild_id: i64, created_at: i64) {
        self.connection()
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, 'dig', 0, 1, 0, '{}', ?3)",
                params![guild_id, discord_id, created_at],
            )
            .expect("insert dig action");
    }

    fn balance(&self, discord_id: i64, guild_id: i64) -> i64 {
        self.repository
            .balance(discord_id, Some(guild_id))
            .expect("read balance")
            .expect("player exists")
    }

    fn total(&self, ids: &[i64], guild_id: i64) -> i64 {
        ids.iter().map(|id| self.balance(*id, guild_id)).sum()
    }

    fn count(&self, table: &str) -> i64 {
        assert!(matches!(table, "dig_actions" | "economy_ledger_entries"));
        self.connection()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count rows")
    }
}

fn candidate_query(
    strategy: EncounterVictimStrategy,
    limit: Option<usize>,
) -> EncounterCandidateQuery {
    EncounterCandidateQuery {
        guild_id: Some(GUILD),
        digger_discord_id: DIGGER,
        strategy,
        active_since: NOW - 14 * 86_400,
        limit,
    }
}

struct SuccessRequestSpec<'a> {
    event_id: &'a str,
    event_name: &'a str,
    event_key: &'a str,
    strategy: EncounterVictimStrategy,
    victim_count: usize,
    penalty: i64,
    reward: i64,
    preferred_victim_ids: Vec<i64>,
}

fn success_request(spec: SuccessRequestSpec<'_>) -> EncounterSettlementRequest {
    EncounterSettlementRequest {
        digger_discord_id: DIGGER,
        guild_id: Some(GUILD),
        event_id: spec.event_id.to_owned(),
        event_name: spec.event_name.to_owned(),
        event_key: spec.event_key.to_owned(),
        outcome: EncounterOutcome::Success {
            strategy: spec.strategy,
            victim_count: spec.victim_count,
            authored_penalty_jc: spec.penalty,
            jittered_authored_reward_jc: spec.reward,
            preferred_victim_ids: spec.preferred_victim_ids,
        },
        minigame_jc_delta_scale: DEFAULT_MINIGAME_JC_DELTA_SCALE,
        active_since: NOW - 14 * 86_400,
        now: NOW,
    }
}

#[test]
fn test_richest_n_burn_is_net_deflationary() {
    let fixture = Fixture::new();
    for (id, balance) in [(DIGGER, 50), (10_002, 1_000), (10_003, 800), (10_004, 500)] {
        fixture.add_player(id, GUILD, balance, None);
    }
    fixture.add_tunnel(DIGGER, GUILD, 120);
    fixture.add_player(10_002, OTHER_GUILD, 20_000, None);
    let ids = [DIGGER, 10_002, 10_003, 10_004];
    let total_before = fixture.total(&ids, GUILD);
    assert_eq!(
        fixture
            .repository
            .candidate_ids(candidate_query(EncounterVictimStrategy::RichestN, Some(2)))
            .unwrap(),
        vec![10_002, 10_003]
    );

    let rollback = success_request(SuccessRequestSpec {
        event_id: "hungering_dark",
        event_name: "The Hungering Dark",
        event_key: "force-rollback",
        strategy: EncounterVictimStrategy::RichestN,
        victim_count: 2,
        penalty: 5,
        reward: 5,
        preferred_victim_ids: Vec::new(),
    });
    assert!(
        fixture
            .repository
            .settle_encounter_atomic(&rollback)
            .is_err()
    );
    assert_eq!(fixture.total(&ids, GUILD), total_before);
    assert_eq!(fixture.count("dig_actions"), 0);
    assert_eq!(fixture.count("economy_ledger_entries"), 0);

    let request = success_request(SuccessRequestSpec {
        event_id: "hungering_dark",
        event_name: "The Hungering Dark",
        event_key: "richest-success",
        strategy: EncounterVictimStrategy::RichestN,
        victim_count: 2,
        penalty: 5,
        reward: 5,
        preferred_victim_ids: Vec::new(),
    });
    let result = fixture
        .repository
        .settle_encounter_atomic(&request)
        .unwrap();
    let burn = scale_deflationary_minigame_jc_delta(
        strengthen_dig_event_penalty(5) as f64,
        DEFAULT_MINIGAME_JC_DELTA_SCALE,
    );
    let payout = scale_positive_dig_jc(scale_minigame_jc_delta(
        5.0,
        DEFAULT_MINIGAME_JC_DELTA_SCALE,
    ));
    assert!(result.succeeded);
    assert_eq!(result.actor_delta, payout);
    assert_eq!(
        result.splash.unwrap().victims,
        vec![(10_002, burn), (10_003, burn)]
    );
    assert_eq!(fixture.balance(10_002, GUILD), 1_000 - burn);
    assert_eq!(fixture.balance(10_003, GUILD), 800 - burn);
    assert_eq!(fixture.balance(10_004, GUILD), 500);
    assert_eq!(fixture.balance(DIGGER, GUILD), 50 + payout);
    assert_eq!(fixture.balance(10_002, OTHER_GUILD), 20_000);
    assert_eq!(fixture.total(&ids, GUILD), total_before + payout - 2 * burn);
    assert_eq!(fixture.count("economy_ledger_entries"), 3);

    let duplicate = fixture
        .repository
        .settle_encounter_atomic(&request)
        .unwrap();
    assert!(!duplicate.applied_now);
    assert_eq!(fixture.total(&ids, GUILD), total_before + payout - 2 * burn);
    assert_eq!(fixture.count("economy_ledger_entries"), 3);
}

#[test]
fn test_empty_pool_pays_nothing() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, GUILD, 50, None);
    fixture.add_tunnel(DIGGER, GUILD, 120);
    let request = success_request(SuccessRequestSpec {
        event_id: "hungering_dark",
        event_name: "The Hungering Dark",
        event_key: "empty-pool",
        strategy: EncounterVictimStrategy::RichestN,
        victim_count: 2,
        penalty: 5,
        reward: 5,
        preferred_victim_ids: Vec::new(),
    });

    let result = fixture
        .repository
        .settle_encounter_atomic(&request)
        .unwrap();
    assert!(result.succeeded);
    assert_eq!(result.actor_delta, 0);
    assert!(result.splash.is_none());
    assert_eq!(fixture.balance(DIGGER, GUILD), 50);
    assert_eq!(fixture.count("economy_ledger_entries"), 0);
}

#[test]
fn test_low_balance_burn_targets_are_threshold_protected() {
    let fixture = Fixture::new();
    for (id, balance) in [(DIGGER, 50), (10_002, 3), (10_003, 3)] {
        fixture.add_player(id, GUILD, balance, None);
    }
    let ids = [DIGGER, 10_002, 10_003];
    let total_before = fixture.total(&ids, GUILD);
    let request = success_request(SuccessRequestSpec {
        event_id: "hungering_dark",
        event_name: "The Hungering Dark",
        event_key: "protected-pool",
        strategy: EncounterVictimStrategy::RichestN,
        victim_count: 2,
        penalty: 5,
        reward: 5,
        preferred_victim_ids: Vec::new(),
    });

    let result = fixture
        .repository
        .settle_encounter_atomic(&request)
        .unwrap();
    assert_eq!(result.actor_delta, 0);
    assert!(result.splash.is_none());
    assert_eq!(fixture.balance(10_002, GUILD), 3);
    assert_eq!(fixture.balance(10_003, GUILD), 3);
    assert_eq!(fixture.balance(DIGGER, GUILD), 50);
    assert_eq!(fixture.total(&ids, GUILD), total_before);
}

#[test]
fn test_failure_does_not_burn() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, GUILD, 50, None);
    fixture.add_player(10_002, GUILD, 1_000, None);
    fixture.add_player(10_002, OTHER_GUILD, 2_000, None);
    let request = EncounterSettlementRequest {
        digger_discord_id: DIGGER,
        guild_id: Some(GUILD),
        event_id: "hungering_dark".to_owned(),
        event_name: "The Hungering Dark".to_owned(),
        event_key: "failed-risk".to_owned(),
        outcome: EncounterOutcome::Failure {
            tuned_authored_loss_jc: -6,
        },
        minigame_jc_delta_scale: DEFAULT_MINIGAME_JC_DELTA_SCALE,
        active_since: NOW - 14 * 86_400,
        now: NOW,
    };

    let result = fixture
        .repository
        .settle_encounter_atomic(&request)
        .unwrap();
    let expected = scale_deflationary_minigame_jc_delta(
        strengthen_dig_event_penalty(-6) as f64,
        DEFAULT_MINIGAME_JC_DELTA_SCALE,
    );
    assert!(!result.succeeded);
    assert_eq!(result.actor_delta, expected);
    assert!(result.splash.is_none());
    assert_eq!(fixture.balance(DIGGER, GUILD), 50 + expected);
    assert_eq!(fixture.balance(10_002, GUILD), 1_000);
    assert_eq!(fixture.balance(10_002, OTHER_GUILD), 2_000);
}

#[test]
fn test_active_diggers_burn_full_when_funded() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, GUILD, 50, None);
    for id in [10_002, 10_003, 10_004] {
        fixture.add_player(id, GUILD, 100, None);
        fixture.log_dig(id, GUILD, NOW);
    }
    fixture.add_player(10_005, GUILD, 100, None);
    fixture.log_dig(10_005, GUILD, NOW - 8 * 86_400);
    fixture.add_player(10_002, OTHER_GUILD, 500, None);
    fixture.log_dig(10_002, OTHER_GUILD, NOW);
    let ids = [DIGGER, 10_002, 10_003, 10_004];
    let total_before = fixture.total(&ids, GUILD);
    let mut query = candidate_query(EncounterVictimStrategy::ActiveDiggers, Some(20));
    query.active_since = NOW - 7 * 86_400;
    let candidates = fixture.repository.candidate_ids(query).unwrap();
    assert_eq!(candidates, vec![10_002, 10_003, 10_004]);
    let request = success_request(SuccessRequestSpec {
        event_id: "turf_war",
        event_name: "Turf War",
        event_key: "active-diggers",
        strategy: EncounterVictimStrategy::ActiveDiggers,
        victim_count: 3,
        penalty: 6,
        reward: 8,
        preferred_victim_ids: candidates,
    });

    let result = fixture
        .repository
        .settle_encounter_atomic(&request)
        .unwrap();
    let burn = scale_deflationary_minigame_jc_delta(
        strengthen_dig_event_penalty(6) as f64,
        DEFAULT_MINIGAME_JC_DELTA_SCALE,
    );
    let payout = scale_positive_dig_jc(scale_minigame_jc_delta(
        8.0,
        DEFAULT_MINIGAME_JC_DELTA_SCALE,
    ));
    assert_eq!(result.actor_delta, payout);
    assert_eq!(result.splash.unwrap().total_burned, 3 * burn);
    for id in [10_002, 10_003, 10_004] {
        assert_eq!(fixture.balance(id, GUILD), 100 - burn);
    }
    assert_eq!(fixture.balance(10_005, GUILD), 100);
    assert_eq!(fixture.balance(DIGGER, GUILD), 50 + payout);
    assert_eq!(fixture.total(&ids, GUILD), total_before + payout - 3 * burn);
}

#[test]
fn test_random_active_burn_full_when_funded() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, GUILD, 50, Some(RECENT_ISO));
    for id in [10_002, 10_003, 10_004] {
        fixture.add_player(id, GUILD, 100, Some(RECENT_ISO));
    }
    fixture.add_player(10_005, GUILD, 100, Some("2020-01-01T00:00:00+00:00"));
    fixture.add_player(10_002, OTHER_GUILD, 900, Some(RECENT_ISO));
    let ids = [DIGGER, 10_002, 10_003, 10_004];
    let total_before = fixture.total(&ids, GUILD);
    let candidates = fixture
        .repository
        .candidate_ids(candidate_query(EncounterVictimStrategy::RandomActive, None))
        .unwrap();
    assert_eq!(candidates, vec![10_002, 10_003, 10_004]);
    let request = success_request(SuccessRequestSpec {
        event_id: "the_tear",
        event_name: "The Tear",
        event_key: "random-active",
        strategy: EncounterVictimStrategy::RandomActive,
        victim_count: 3,
        penalty: 8,
        reward: 10,
        preferred_victim_ids: candidates,
    });

    let result = fixture
        .repository
        .settle_encounter_atomic(&request)
        .unwrap();
    let burn = scale_deflationary_minigame_jc_delta(
        strengthen_dig_event_penalty(8) as f64,
        DEFAULT_MINIGAME_JC_DELTA_SCALE,
    );
    let payout = scale_positive_dig_jc(scale_minigame_jc_delta(
        10.0,
        DEFAULT_MINIGAME_JC_DELTA_SCALE,
    ));
    assert_eq!(result.actor_delta, payout);
    let splash = result.splash.unwrap();
    assert_eq!(splash.victims.len(), 3);
    assert_eq!(splash.total_burned, 3 * burn);
    assert_eq!(fixture.balance(DIGGER, GUILD), 50 + payout);
    assert_eq!(fixture.total(&ids, GUILD), total_before + payout - 3 * burn);
    assert_eq!(fixture.balance(10_002, OTHER_GUILD), 900);
}

/// Cross-language smoke: Python creates the disposable production schema,
/// Rust settles the whole encounter, and Python observes wallets and audits.
#[test]
#[ignore = "cross-language smoke; run explicitly when repository Python is available"]
fn unmapped_python_migrated_database_tunnel_encounter_interop_smoke() {
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
            "import sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\np=sys.argv[1]\nSchemaManager(p).initialize()\nc=sqlite3.connect(p)\nrows=[(-70001,0,'encounter-actor',50),(-70002,0,'encounter-richest',1000),(-70003,0,'encounter-next',800),(-70002,99,'other-guild',9000)]\nc.executemany('INSERT INTO players (discord_id,guild_id,discord_username,jopacoin_balance) VALUES (?,?,?,?)',rows)\nc.execute(\"INSERT INTO tunnels (discord_id,guild_id,depth,tunnel_name) VALUES (-70001,0,120,'interop')\")\nc.commit(); c.close()",
        ])
        .arg(database.path())
        .output()
        .expect("run Python schema authority");
    assert!(
        initialize.status.success(),
        "Python initialization failed: {}",
        String::from_utf8_lossy(&initialize.stderr)
    );

    let repository = DigTunnelEncounterRepository::new(database.path());
    let settlement = repository
        .settle_encounter_atomic(&EncounterSettlementRequest {
            digger_discord_id: -70_001,
            guild_id: None,
            event_id: "hungering_dark".to_owned(),
            event_name: "The Hungering Dark".to_owned(),
            event_key: "python-migrated-smoke".to_owned(),
            outcome: EncounterOutcome::Success {
                strategy: EncounterVictimStrategy::RichestN,
                victim_count: 2,
                authored_penalty_jc: 5,
                jittered_authored_reward_jc: 5,
                preferred_victim_ids: Vec::new(),
            },
            minigame_jc_delta_scale: DEFAULT_MINIGAME_JC_DELTA_SCALE,
            active_since: 0,
            now: NOW,
        })
        .expect("Rust encounter settlement");
    assert!(settlement.succeeded);
    assert_eq!(settlement.actor_delta, 3);
    assert_eq!(settlement.splash.unwrap().total_burned, 16);

    let verify = Command::new(python)
        .current_dir(repository_root)
        .args([
            "-c",
            "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1])\nassert c.execute('SELECT jopacoin_balance FROM players WHERE discord_id=-70001 AND guild_id=0').fetchone()==(53,)\nassert c.execute('SELECT jopacoin_balance FROM players WHERE discord_id=-70002 AND guild_id=0').fetchone()==(992,)\nassert c.execute('SELECT jopacoin_balance FROM players WHERE discord_id=-70003 AND guild_id=0').fetchone()==(792,)\nassert c.execute('SELECT jopacoin_balance FROM players WHERE discord_id=-70002 AND guild_id=99').fetchone()==(9000,)\nassert c.execute(\"SELECT COUNT(*) FROM dig_actions WHERE guild_id=0 AND action_type='splash_victim'\").fetchone()==(2,)\nassert c.execute(\"SELECT COUNT(*) FROM dig_actions WHERE guild_id=0 AND actor_id=-70001 AND action_type='event'\").fetchone()==(1,)\nassert c.execute(\"SELECT COUNT(*) FROM economy_ledger_entries WHERE guild_id=0 AND source='dig'\").fetchone()==(3,)\nassert c.execute('SELECT COUNT(*) FROM economy_ledger_context').fetchone()==(0,)\nc.close()",
        ])
        .arg(database.path())
        .output()
        .expect("run Python post-Rust verification");
    assert!(
        verify.status.success(),
        "Python verification failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}
