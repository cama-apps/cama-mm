use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 12_345;
const DIGGER: i64 = 20_001;
const VICTIM: i64 = 20_002;

struct Fixture {
    _database: NamedTempFile,
    repository: DigNewEventsRepository,
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
                     lowest_balance_ever INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
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
                 CREATE TRIGGER force_thief_audit_failure
                 BEFORE INSERT ON dig_actions
                 WHEN NEW.action_type = 'splash_thief'
                      AND NEW.detail LIKE '%force-rollback%'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced thief audit failure');
                 END;",
            )
            .expect("fixture schema");
        drop(connection);
        let repository = DigNewEventsRepository::new(database.path());
        Self {
            _database: database,
            repository,
        }
    }

    fn add_player(&self, discord_id: i64, balance: i64) {
        let connection = Connection::open(&self.repository.path).expect("fixture connection");
        connection
            .execute(
                "INSERT INTO players
                 (discord_id, guild_id, discord_username, jopacoin_balance, last_match_date)
                 VALUES (?1, ?2, ?3, ?4, '2026-01-01T00:00:00+00:00')",
                params![discord_id, GUILD, format!("User{discord_id}"), balance],
            )
            .expect("insert player");
    }
}

fn request<'a>(event_key: &'a str, detail: &'a str) -> StealSettlementRequest<'a> {
    StealSettlementRequest {
        thief_discord_id: DIGGER,
        victim_discord_id: VICTIM,
        guild_id: Some(GUILD),
        amount: 15,
        minimum_victim_balance: 50,
        event_name: "Steal Test",
        event_key,
        detail,
        now: 2_000_000_000,
    }
}

#[test]
fn test_steal_credits_digger_and_debits_victim() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, 100);
    fixture.add_player(VICTIM, 200);
    assert_eq!(
        fixture
            .repository
            .random_active_candidates(Some(GUILD), DIGGER)
            .unwrap(),
        vec![VICTIM]
    );

    let result = fixture
        .repository
        .settle_steal_atomic(request("steal-credit", "Steal Test"))
        .unwrap();

    assert_eq!(
        result,
        StealSettlement::Applied {
            amount: 15,
            thief_new_balance: 115,
            victim_new_balance: 185,
            applied_now: true,
        }
    );
    assert_eq!(
        fixture.repository.balance(DIGGER, Some(GUILD)).unwrap(),
        Some(115)
    );
    assert_eq!(
        fixture.repository.balance(VICTIM, Some(GUILD)).unwrap(),
        Some(185)
    );
}

#[test]
fn test_steal_logs_both_victim_and_thief() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, 100);
    fixture.add_player(VICTIM, 200);

    let error = fixture
        .repository
        .settle_steal_atomic(request("rollback", "force-rollback"))
        .unwrap_err();
    assert!(matches!(error, DigNewEventsRepositoryError::Sqlite(_)));
    assert_eq!(
        fixture.repository.balance(DIGGER, Some(GUILD)).unwrap(),
        Some(100)
    );
    assert_eq!(
        fixture.repository.balance(VICTIM, Some(GUILD)).unwrap(),
        Some(200)
    );
    assert!(
        fixture
            .repository
            .splash_actions(Some(GUILD))
            .unwrap()
            .is_empty()
    );

    let first = fixture
        .repository
        .settle_steal_atomic(request("audit", "Audit Test"))
        .unwrap();
    let duplicate = fixture
        .repository
        .settle_steal_atomic(request("audit", "Audit Test"))
        .unwrap();
    assert!(matches!(
        first,
        StealSettlement::Applied {
            applied_now: true,
            ..
        }
    ));
    assert!(matches!(
        duplicate,
        StealSettlement::Applied {
            applied_now: false,
            ..
        }
    ));

    let actions = fixture.repository.splash_actions(Some(GUILD)).unwrap();
    assert_eq!(actions.len(), 2);
    assert!(actions.iter().any(|action| {
        action.actor_id == VICTIM && action.action_type == "splash_victim" && action.jc_delta == -15
    }));
    assert!(actions.iter().any(|action| {
        action.actor_id == DIGGER && action.action_type == "splash_thief" && action.jc_delta == 15
    }));
    assert_eq!(
        fixture.repository.balance(DIGGER, Some(GUILD)).unwrap(),
        Some(115)
    );
    assert_eq!(
        fixture.repository.balance(VICTIM, Some(GUILD)).unwrap(),
        Some(185)
    );
}

#[test]
fn test_steal_skips_victim_below_auto_blind_threshold() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, 0);
    fixture.add_player(VICTIM, 5);

    assert_eq!(
        fixture
            .repository
            .settle_steal_atomic(request("deep-steal", "Deep Steal"))
            .unwrap(),
        StealSettlement::SkippedBelowThreshold { victim_balance: 5 }
    );
    assert_eq!(
        fixture.repository.balance(DIGGER, Some(GUILD)).unwrap(),
        Some(0)
    );
    assert_eq!(
        fixture.repository.balance(VICTIM, Some(GUILD)).unwrap(),
        Some(5)
    );
    assert!(
        fixture
            .repository
            .splash_actions(Some(GUILD))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_steal_with_empty_pool_returns_empty() {
    let fixture = Fixture::new();
    fixture.add_player(DIGGER, 100);

    let candidates = fixture
        .repository
        .random_active_candidates(Some(GUILD), DIGGER)
        .unwrap();
    assert!(candidates.is_empty());
    assert_eq!(
        fixture.repository.balance(DIGGER, Some(GUILD)).unwrap(),
        Some(100)
    );
    assert!(
        fixture
            .repository
            .splash_actions(Some(GUILD))
            .unwrap()
            .is_empty()
    );
}
