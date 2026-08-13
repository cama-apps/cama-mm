use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, OptionalExtension, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 12_345;
const OTHER_GUILD: i64 = 54_321;

struct Fixture {
    database: NamedTempFile,
    repository: BankruptcyRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary database");
        let connection = Connection::open(database.path()).expect("fixture connection");
        connection
            .execute_batch(
                "CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT,
                     jopacoin_balance INTEGER DEFAULT 3,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE bankruptcy_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     last_bankruptcy_at INTEGER,
                     penalty_games_remaining INTEGER DEFAULT 0,
                     bankruptcy_count INTEGER DEFAULT 0,
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
        let repository = BankruptcyRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("fixture connection")
    }

    fn add_player(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![discord_id, guild_id, format!("Player{discord_id}"), balance],
            )
            .expect("insert player");
    }

    fn set_balance(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.connection()
            .execute(
                "UPDATE players SET jopacoin_balance = ?1
                  WHERE discord_id = ?2 AND guild_id = ?3",
                params![balance, discord_id, guild_id],
            )
            .expect("set fixture balance");
    }

    fn declaration(&self, discord_id: i64, now: i64) -> DeclarationOutcome {
        self.repository
            .declare_atomic(DeclarationRequest {
                discord_id,
                guild_id: Some(GUILD),
                fresh_start_balance: 3,
                cooldown_seconds: 604_800,
                penalty_games: 5,
                now,
            })
            .expect("declare bankruptcy")
    }
}

#[test]
fn test_bankruptcy_clears_debt() {
    let fixture = Fixture::new();
    fixture.add_player(1_001, GUILD, -300);

    let outcome = fixture.declaration(1_001, 1_700_000_000);

    assert_eq!(
        outcome,
        DeclarationOutcome::Applied {
            debt_cleared: 300,
            new_balance: 3,
            penalty_games: 5,
            bankruptcy_count: 1,
        }
    );
    assert_eq!(
        fixture.repository.balance(1_001, Some(GUILD)).unwrap(),
        Some(3)
    );
    let ledger = fixture
        .connection()
        .query_row(
            "SELECT delta, source, related_type, related_id
               FROM economy_ledger_entries",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        ledger,
        (303, "bankruptcy".into(), "bankruptcy".into(), "1001".into())
    );
    let context_count: i64 = fixture
        .connection()
        .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(context_count, 0);
}

#[test]
fn test_bankruptcy_sets_penalty_games() {
    let fixture = Fixture::new();
    fixture.add_player(1_001, GUILD, -300);
    fixture.declaration(1_001, 1_700_000_000);

    let state = fixture
        .repository
        .get_state(1_001, Some(GUILD))
        .unwrap()
        .unwrap();
    assert_eq!(state.penalty_games_remaining, 5);
    assert_eq!(state.bankruptcy_count, 1);
}

#[test]
fn test_bankruptcy_state_persistence() {
    let fixture = Fixture::new();
    fixture.add_player(1_020, GUILD, -100);
    fixture.declaration(1_020, 1_700_000_000);

    let restarted = BankruptcyRepository::new(fixture.database.path());
    assert_eq!(restarted.get_penalty_games(1_020, Some(GUILD)).unwrap(), 5);
}

#[test]
fn test_clears_debt_and_records_state() {
    let fixture = Fixture::new();
    fixture.add_player(10_001, GUILD, -75);
    let outcome = fixture
        .repository
        .declare_atomic(DeclarationRequest {
            discord_id: 10_001,
            guild_id: Some(GUILD),
            fresh_start_balance: 0,
            cooldown_seconds: 3_600,
            penalty_games: 5,
            now: 1_700_000_000,
        })
        .expect("atomic bankruptcy");
    assert_eq!(
        outcome,
        DeclarationOutcome::Applied {
            debt_cleared: 75,
            new_balance: 0,
            penalty_games: 5,
            bankruptcy_count: 1,
        }
    );
    assert_eq!(
        fixture.repository.balance(10_001, Some(GUILD)).unwrap(),
        Some(0)
    );
    let state = fixture
        .repository
        .get_state(10_001, Some(GUILD))
        .unwrap()
        .unwrap();
    assert_eq!(state.penalty_games_remaining, 5);
    assert_eq!(state.last_bankruptcy_at, Some(1_700_000_000));
}

#[test]
fn test_raises_not_in_debt_when_balance_non_negative() {
    let fixture = Fixture::new();
    fixture.add_player(10_002, GUILD, 10);
    let outcome = fixture
        .repository
        .declare_atomic(DeclarationRequest {
            discord_id: 10_002,
            guild_id: Some(GUILD),
            fresh_start_balance: 0,
            cooldown_seconds: 3_600,
            penalty_games: 5,
            now: 0,
        })
        .expect("validation result");
    assert_eq!(outcome, DeclarationOutcome::NotInDebt { balance: 10 });
    assert_eq!(
        fixture.repository.balance(10_002, Some(GUILD)).unwrap(),
        Some(10)
    );
}

#[test]
fn test_concurrent_declarations_do_not_double_bump() {
    let fixture = Fixture::new();
    fixture.add_player(10_003, GUILD, -100);
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(5));
    let handles = (0..5)
        .map(|_| {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository.declare_atomic(DeclarationRequest {
                    discord_id: 10_003,
                    guild_id: Some(GUILD),
                    fresh_start_balance: 0,
                    cooldown_seconds: 3_600,
                    penalty_games: 5,
                    now: 1_700_000_000,
                })
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("bankruptcy thread")
                .expect("SQLite result")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, DeclarationOutcome::Applied { .. }))
            .count(),
        1
    );
    assert!(outcomes.iter().all(|outcome| matches!(
        outcome,
        DeclarationOutcome::Applied { .. }
            | DeclarationOutcome::NotInDebt { .. }
            | DeclarationOutcome::Cooldown { .. }
    )));
    let state = repository.get_state(10_003, Some(GUILD)).unwrap().unwrap();
    assert_eq!(state.penalty_games_remaining, 5);
    assert_eq!(state.bankruptcy_count, 1);
}

#[test]
fn test_penalty_wins_decrement() {
    let fixture = Fixture::new();
    fixture
        .repository
        .upsert_state(1_001, Some(GUILD), 1_700_000_000, 5)
        .unwrap();

    for expected in [4, 3, 2] {
        assert_eq!(
            fixture
                .repository
                .decrement_penalty_games(1_001, Some(GUILD))
                .unwrap(),
            expected
        );
    }
}

#[test]
fn test_penalty_expires_after_wins() {
    let fixture = Fixture::new();
    fixture
        .repository
        .upsert_state(1_001, Some(GUILD), 1_700_000_000, 5)
        .unwrap();

    for _ in 0..5 {
        fixture
            .repository
            .decrement_penalty_games(1_001, Some(GUILD))
            .unwrap();
    }
    assert_eq!(
        fixture
            .repository
            .get_penalty_games(1_001, Some(GUILD))
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .repository
            .decrement_penalty_games(1_001, Some(GUILD))
            .unwrap(),
        0
    );
}

#[test]
fn test_win_bonus_batches_bankruptcy_io() {
    let fixture = Fixture::new();
    let winners = (1_101..=1_105).collect::<Vec<_>>();
    for discord_id in &winners {
        fixture.add_player(*discord_id, GUILD, 100);
        fixture
            .repository
            .upsert_state(*discord_id, Some(GUILD), 1_700_000_000, 3)
            .unwrap();
    }

    let receipts = fixture
        .repository
        .award_winners_atomic(WinnerAwardRequest {
            discord_ids: &winners,
            guild_id: Some(GUILD),
            gross_reward: 10,
            penalty_kept_rate: 0.5,
            decrement_penalty: true,
        })
        .unwrap();
    assert_eq!(receipts.len(), 5);
    assert!(receipts.iter().all(|receipt| {
        receipt.net == 5
            && receipt.bankruptcy_penalty == 5
            && receipt.balance_after == 105
            && receipt.penalty_games_remaining == 2
    }));
    let ledger_count: i64 = fixture
        .connection()
        .query_row("SELECT COUNT(*) FROM economy_ledger_entries", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ledger_count, 10);

    let before = fixture.repository.balance(1_101, Some(GUILD)).unwrap();
    let error = fixture
        .repository
        .award_winners_atomic(WinnerAwardRequest {
            discord_ids: &[1_101, 9_999_999],
            guild_id: Some(GUILD),
            gross_reward: 10,
            penalty_kept_rate: 0.5,
            decrement_penalty: true,
        })
        .unwrap_err();
    assert!(matches!(
        error,
        BankruptcyRepositoryError::PlayerNotFound(9_999_999)
    ));
    assert_eq!(
        fixture.repository.balance(1_101, Some(GUILD)).unwrap(),
        before
    );
    assert_eq!(
        fixture
            .repository
            .get_penalty_games(1_101, Some(GUILD))
            .unwrap(),
        2
    );
}

#[test]
fn test_first_bankruptcy_sets_count_to_one() {
    let fixture = Fixture::new();
    fixture.add_player(1_001, GUILD, -200);
    fixture.declaration(1_001, 1_700_000_000);

    assert_eq!(
        fixture
            .repository
            .get_state(1_001, Some(GUILD))
            .unwrap()
            .unwrap()
            .bankruptcy_count,
        1
    );
}

#[test]
fn test_multiple_bankruptcies_increment_count() {
    let fixture = Fixture::new();
    fixture.add_player(1_001, GUILD, -200);

    for (iteration, balance) in [-200, -100, -50].into_iter().enumerate() {
        fixture.set_balance(1_001, GUILD, balance);
        let outcome = fixture
            .repository
            .declare_atomic(DeclarationRequest {
                discord_id: 1_001,
                guild_id: Some(GUILD),
                fresh_start_balance: 3,
                cooldown_seconds: 0,
                penalty_games: 5,
                now: 1_700_000_000 + i64::try_from(iteration).unwrap(),
            })
            .unwrap();
        assert!(matches!(outcome, DeclarationOutcome::Applied { .. }));
    }
    assert_eq!(
        fixture
            .repository
            .get_state(1_001, Some(GUILD))
            .unwrap()
            .unwrap()
            .bankruptcy_count,
        3
    );
}

#[test]
fn test_reset_cooldown_does_not_increment_count() {
    let fixture = Fixture::new();
    fixture.add_player(1_001, GUILD, -200);
    fixture.declaration(1_001, 1_700_000_000);

    assert!(
        fixture
            .repository
            .reset_cooldown_only(1_001, Some(GUILD), 0, 0)
            .unwrap()
    );
    let state = fixture
        .repository
        .get_state(1_001, Some(GUILD))
        .unwrap()
        .unwrap();
    assert_eq!(state.bankruptcy_count, 1);
    assert_eq!(state.last_bankruptcy_at, Some(0));
    assert_eq!(state.penalty_games_remaining, 0);
}

#[test]
fn test_player_with_no_bankruptcy_has_zero_count() {
    let fixture = Fixture::new();
    fixture.add_player(1_001, GUILD, 100);
    assert_eq!(
        fixture.repository.get_state(1_001, Some(GUILD)).unwrap(),
        None
    );
}

#[test]
fn test_get_bulk_states_repo_returns_only_existing() {
    let fixture = Fixture::new();
    fixture.add_player(1_001, GUILD, -200);
    fixture.add_player(1_002, GUILD, 50);
    fixture.declaration(1_001, 1_700_000_000);

    let states = fixture
        .repository
        .get_bulk_states(&[1_001, 1_002, 1_001], Some(GUILD))
        .unwrap();
    assert_eq!(states.len(), 1);
    assert!(states.contains_key(&1_001));
    assert!(!states.contains_key(&1_002));
}

#[test]
fn test_bulk_decrement_preserves_duplicates_and_guild() {
    let fixture = Fixture::new();
    fixture
        .repository
        .upsert_state(1_201, Some(GUILD), 1_700_000_000, 3)
        .unwrap();
    fixture
        .repository
        .upsert_state(1_202, Some(GUILD), 1_700_000_000, 1)
        .unwrap();
    fixture
        .repository
        .upsert_state(1_201, Some(OTHER_GUILD), 1_700_000_000, 4)
        .unwrap();

    let remaining = fixture
        .repository
        .decrement_penalty_games_bulk(&[1_201, 1_202, 1_201, 1_299], Some(GUILD))
        .unwrap();
    assert_eq!(
        remaining,
        BTreeMap::from([(1_201, 1), (1_202, 0), (1_299, 0)])
    );
    assert_eq!(
        fixture
            .repository
            .get_penalty_games(1_201, Some(OTHER_GUILD))
            .unwrap(),
        4
    );

    let missing: Option<i64> = fixture
        .connection()
        .query_row(
            "SELECT penalty_games_remaining FROM bankruptcy_state
              WHERE discord_id = 1299 AND guild_id = ?1",
            [GUILD],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(missing, None);
}
