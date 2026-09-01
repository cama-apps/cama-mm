use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::Connection;
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 9_001;
const OTHER_GUILD: i64 = 9_002;
const TODAY: &str = "2025-06-15";

struct Fixture {
    _file: NamedTempFile,
    repository: ManaRepository,
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("temporary SQLite file");
        let connection = Connection::open(file.path()).expect("open fixture database");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = OFF;
                 CREATE TABLE player_mana (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     current_land TEXT,
                     assigned_date TEXT,
                     bankrupt_insurance_used INTEGER DEFAULT 0,
                     bankrupt_reroll_used INTEGER DEFAULT 0,
                     consumed_today INTEGER DEFAULT 0,
                     white_shield_remaining INTEGER DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );",
            )
            .expect("create disposable mana fixture schema");
        drop(connection);
        let repository = ManaRepository::new(file.path());
        Self {
            _file: file,
            repository,
        }
    }

    /// Add the economy tables the in-transaction White stipend touches.
    fn with_stipend_tables(self) -> Self {
        self.connection()
            .execute_batch(
                "CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     jopacoin_balance INTEGER DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE nonprofit_fund (
                     guild_id INTEGER PRIMARY KEY,
                     total_collected INTEGER NOT NULL DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY,
                     source TEXT,
                     actor_id TEXT,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );",
            )
            .expect("create stipend fixture tables");
        self
    }

    fn connection(&self) -> Connection {
        Connection::open(self._file.path()).expect("open fixture database")
    }

    fn seed_player(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (discord_id, guild_id, jopacoin_balance)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![discord_id, GUILD, balance],
            )
            .expect("seed player");
    }

    fn seed_reserve(&self, total: i64) {
        self.connection()
            .execute(
                "INSERT INTO nonprofit_fund (guild_id, total_collected) VALUES (?1, ?2)",
                rusqlite::params![GUILD, total],
            )
            .expect("seed nonprofit reserve");
    }

    fn balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                rusqlite::params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("read player balance")
    }

    fn reserve(&self) -> i64 {
        self.connection()
            .query_row(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?1",
                [GUILD],
                |row| row.get(0),
            )
            .expect("read nonprofit reserve")
    }
}

#[test]
fn test_get_mana_returns_none_when_not_set() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .repository
            .get_mana(1_234, Some(GUILD))
            .expect("read missing mana"),
        None
    );
}

#[test]
fn test_set_and_get_mana() {
    let fixture = Fixture::new();
    fixture
        .repository
        .set_mana(1_234, Some(GUILD), "Island", TODAY)
        .expect("set mana");
    let row = fixture
        .repository
        .get_mana(1_234, Some(GUILD))
        .expect("get mana")
        .expect("stored row");
    assert_eq!(row.current_land, "Island");
    assert_eq!(row.assigned_date, TODAY);
}

#[test]
fn test_set_mana_overwrites_previous() {
    let fixture = Fixture::new();
    fixture
        .repository
        .set_mana(1_234, Some(GUILD), "Forest", "2025-06-14")
        .expect("set old mana");
    fixture
        .repository
        .set_mana(1_234, Some(GUILD), "Mountain", TODAY)
        .expect("replace mana");
    let row = fixture
        .repository
        .get_mana(1_234, Some(GUILD))
        .expect("get mana")
        .expect("stored row");
    assert_eq!(
        (row.current_land.as_str(), row.assigned_date.as_str()),
        ("Mountain", TODAY)
    );
}

#[test]
fn test_guild_isolation() {
    let fixture = Fixture::new();
    fixture
        .repository
        .set_mana(1_234, Some(GUILD), "Swamp", TODAY)
        .expect("set mana");
    assert!(
        fixture
            .repository
            .get_mana(1_234, Some(OTHER_GUILD))
            .expect("read other guild")
            .is_none()
    );
}

#[test]
fn test_guild_id_none_normalised_to_zero() {
    let fixture = Fixture::new();
    fixture
        .repository
        .set_mana(1_234, None, "Plains", TODAY)
        .expect("set global mana");
    let row = fixture
        .repository
        .get_mana(1_234, Some(0))
        .expect("read normalized guild")
        .expect("stored row");
    assert_eq!(row.current_land, "Plains");
}

#[test]
fn test_get_all_mana_returns_guild_rows() {
    let fixture = Fixture::new();
    for (discord_id, guild_id, land) in [
        (1, GUILD, "Island"),
        (2, GUILD, "Mountain"),
        (3, OTHER_GUILD, "Forest"),
    ] {
        fixture
            .repository
            .set_mana(discord_id, Some(guild_id), land, TODAY)
            .expect("seed mana");
    }
    let ids = fixture
        .repository
        .get_all_mana(Some(GUILD))
        .expect("get guild mana")
        .into_iter()
        .map(|row| row.discord_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(ids, BTreeSet::from([1, 2]));
}

#[test]
fn test_get_all_mana_empty() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .repository
            .get_all_mana(Some(GUILD))
            .expect("get empty guild")
            .is_empty()
    );
}

#[test]
fn test_claim_mana_batch_is_one_connection_and_returns_only_new_claims() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .repository
            .claim_mana_atomic(1, Some(GUILD), "Forest", TODAY)
            .expect("claim today's mana")
    );
    assert!(
        fixture
            .repository
            .claim_mana_atomic(2, Some(GUILD), "Mountain", "2025-06-14")
            .expect("claim old mana")
    );
    assert!(
        fixture
            .repository
            .mark_mana_consumed_atomic(2, Some(GUILD))
            .expect("consume old mana")
    );
    assert!(
        fixture
            .repository
            .claim_bankrupt_buff_atomic(2, Some(GUILD), BankruptBuff::Reroll)
            .expect("claim old reroll")
    );

    let claimed = fixture
        .repository
        .claim_mana_batch_atomic(
            &[
                (1, "Swamp".to_owned()),
                (2, "Plains".to_owned()),
                (3, "Island".to_owned()),
                (2, "Forest".to_owned()),
            ],
            Some(GUILD),
            TODAY,
            0,
        )
        .expect("batch claim");
    assert_eq!(
        claimed
            .iter()
            .map(|row| (row.discord_id, row.current_land.as_str()))
            .collect::<Vec<_>>(),
        [(2, "Plains"), (3, "Island")]
    );
    assert_eq!(
        fixture
            .repository
            .get_mana(1, Some(GUILD))
            .expect("read player one")
            .expect("player one row")
            .current_land,
        "Forest"
    );
    assert_eq!(
        fixture
            .repository
            .get_mana(2, Some(GUILD))
            .expect("read player two")
            .expect("player two row"),
        ManaRow {
            current_land: "Plains".to_owned(),
            assigned_date: TODAY.to_owned(),
            consumed_today: false,
            white_shield_remaining: 25,
        }
    );
    assert!(
        !fixture
            .repository
            .is_bankrupt_buff_used(2, Some(GUILD), BankruptBuff::Reroll)
            .expect("read reroll flag")
    );
}

#[test]
fn test_claim_mana_batch_is_guild_scoped() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .repository
            .claim_mana_atomic(10, Some(OTHER_GUILD), "Swamp", TODAY)
            .expect("claim other guild")
    );
    let claimed = fixture
        .repository
        .claim_mana_batch_atomic(&[(10, "Island".to_owned())], Some(GUILD), TODAY, 0)
        .expect("claim target guild");
    assert_eq!(claimed[0].discord_id, 10);
    assert_eq!(
        fixture
            .repository
            .get_mana(10, Some(GUILD))
            .expect("target guild")
            .expect("target row")
            .current_land,
        "Island"
    );
    assert_eq!(
        fixture
            .repository
            .get_mana(10, Some(OTHER_GUILD))
            .expect("other guild")
            .expect("other row")
            .current_land,
        "Swamp"
    );
}

#[test]
fn test_claim_batch_pays_white_stipend_inside_the_claim_transaction() {
    let fixture = Fixture::new().with_stipend_tables();
    fixture.seed_player(1, 0); // bankrupt Plains winner: paid
    fixture.seed_player(2, 100); // solvent Plains winner: skipped
    fixture.seed_player(3, -5); // bankrupt non-Plains winner: skipped
    fixture.seed_reserve(20);

    let claimed = fixture
        .repository
        .claim_mana_batch_atomic(
            &[
                (1, "Plains".to_owned()),
                (2, "Plains".to_owned()),
                (3, "Forest".to_owned()),
            ],
            Some(GUILD),
            TODAY,
            5,
        )
        .expect("batch claim with stipend");

    assert_eq!(claimed.len(), 3, "all claims land with the stipend");
    assert_eq!(fixture.balance(1), 5, "bankrupt Plains winner is paid");
    assert_eq!(fixture.balance(2), 100, "solvent Plains winner is skipped");
    assert_eq!(fixture.balance(3), -5, "non-Plains winner is skipped");
    assert_eq!(fixture.reserve(), 15, "reserve is debited once");
    let context_rows: i64 = fixture
        .connection()
        .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
            row.get(0)
        })
        .expect("count ledger context rows");
    assert_eq!(context_rows, 0, "ledger context is cleared after payment");
}

#[test]
fn test_claim_batch_stipend_is_idempotent_across_wakes() {
    let fixture = Fixture::new().with_stipend_tables();
    fixture.seed_player(1, 0);
    fixture.seed_reserve(20);
    let assignments = [(1, "Plains".to_owned())];

    let first = fixture
        .repository
        .claim_mana_batch_atomic(&assignments, Some(GUILD), TODAY, 5)
        .expect("first wake claims and pays");
    let second = fixture
        .repository
        .claim_mana_batch_atomic(&assignments, Some(GUILD), TODAY, 5)
        .expect("second wake is a no-op");

    assert_eq!(first.len(), 1);
    assert!(second.is_empty(), "already-claimed day must not re-claim");
    assert_eq!(fixture.balance(1), 5, "stipend is never paid twice");
    assert_eq!(fixture.reserve(), 15, "reserve is only debited once");
}

#[test]
fn test_claim_batch_stipend_is_capped_by_the_reserve() {
    let fixture = Fixture::new().with_stipend_tables();
    fixture.seed_player(1, 0);
    fixture.seed_player(2, 0);
    fixture.seed_reserve(7);

    fixture
        .repository
        .claim_mana_batch_atomic(
            &[(1, "Plains".to_owned()), (2, "Plains".to_owned())],
            Some(GUILD),
            TODAY,
            5,
        )
        .expect("batch claim against a low reserve");

    assert_eq!(fixture.balance(1), 5, "first winner gets the full stipend");
    assert_eq!(fixture.balance(2), 2, "second winner gets the remainder");
    assert_eq!(
        fixture.reserve(),
        0,
        "reserve is drained but never negative"
    );
}

#[test]
fn test_concurrent_mana_batches_return_each_claim_once() {
    let fixture = Fixture::new();
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["Plains", "Forest"].map(|land| {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            repository
                .claim_mana_batch_atomic(
                    &[(20, land.to_owned()), (21, land.to_owned())],
                    Some(GUILD),
                    TODAY,
                    0,
                )
                .expect("concurrent batch")
        })
    });
    let mut lengths = handles
        .into_iter()
        .map(|handle| handle.join().expect("batch thread").len())
        .collect::<Vec<_>>();
    lengths.sort_unstable();
    assert_eq!(lengths, [0, 2]);
    let first = repository
        .get_mana(20, Some(GUILD))
        .expect("read first")
        .expect("first row");
    let second = repository
        .get_mana(21, Some(GUILD))
        .expect("read second")
        .expect("second row");
    assert!(matches!(first.current_land.as_str(), "Plains" | "Forest"));
    assert_eq!(first.current_land, second.current_land);
}

#[test]
fn test_concurrent_claims_only_one_succeeds() {
    let fixture = Fixture::new();
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(10));
    let handles = (0..10)
        .map(|_| {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository
                    .claim_mana_atomic(1, Some(GUILD), "Island", "2026-04-22")
                    .expect("atomic claim")
            })
        })
        .collect::<Vec<_>>();
    let claims = handles
        .into_iter()
        .map(|handle| handle.join().expect("mana thread"))
        .collect::<Vec<_>>();
    assert_eq!(claims.iter().filter(|claimed| **claimed).count(), 1);
    let stored = repository
        .get_mana(1, Some(GUILD))
        .unwrap()
        .expect("stored claim");
    assert_eq!(stored.current_land, "Island");
    assert_eq!(stored.assigned_date, "2026-04-22");
}

#[test]
fn test_claim_idempotent_per_date() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .repository
            .claim_mana_atomic(1, Some(GUILD), "Forest", "2026-04-22")
            .unwrap()
    );
    assert!(
        !fixture
            .repository
            .claim_mana_atomic(1, Some(GUILD), "Swamp", "2026-04-22")
            .unwrap()
    );
    assert_eq!(
        fixture
            .repository
            .get_mana(1, Some(GUILD))
            .unwrap()
            .unwrap()
            .current_land,
        "Forest"
    );
}

#[test]
fn test_claim_succeeds_on_new_date() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .repository
            .claim_mana_atomic(1, Some(GUILD), "Mountain", "2026-04-22")
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .claim_mana_atomic(1, Some(GUILD), "Plains", "2026-04-23")
            .unwrap()
    );
    let stored = fixture
        .repository
        .get_mana(1, Some(GUILD))
        .unwrap()
        .unwrap();
    assert_eq!(stored.current_land, "Plains");
    assert_eq!(stored.assigned_date, "2026-04-23");
    assert_eq!(stored.white_shield_remaining, PLAINS_GUARDIAN_CAPACITY);
}

#[test]
fn test_no_row_means_unused() {
    let fixture = Fixture::new();
    for buff in [BankruptBuff::Insurance, BankruptBuff::Reroll] {
        assert!(
            !fixture
                .repository
                .is_bankrupt_buff_used(11_111, Some(GUILD), buff)
                .expect("missing mana row is unused")
        );
    }
}

#[test]
fn test_no_row_claim_fails() {
    let fixture = Fixture::new();
    for buff in [BankruptBuff::Insurance, BankruptBuff::Reroll] {
        assert!(
            !fixture
                .repository
                .claim_bankrupt_buff_atomic(11_111, Some(GUILD), buff)
                .expect("missing mana row cannot claim")
        );
    }
}

#[test]
fn test_claim_once_then_used() {
    let fixture = Fixture::new();
    fixture
        .repository
        .set_mana(22_222, Some(GUILD), "Forest", TODAY)
        .expect("seed mana row");

    assert!(
        fixture
            .repository
            .claim_bankrupt_buff_atomic(22_222, Some(GUILD), BankruptBuff::Insurance)
            .expect("claim insurance")
    );
    assert!(
        fixture
            .repository
            .is_bankrupt_buff_used(22_222, Some(GUILD), BankruptBuff::Insurance)
            .expect("insurance used")
    );
    assert!(
        !fixture
            .repository
            .is_bankrupt_buff_used(22_222, Some(GUILD), BankruptBuff::Reroll)
            .expect("reroll remains independent")
    );
}

#[test]
fn test_claim_twice_fails() {
    let fixture = Fixture::new();
    fixture
        .repository
        .set_mana(33_333, Some(GUILD), "Mountain", TODAY)
        .expect("seed mana row");

    assert!(
        fixture
            .repository
            .claim_bankrupt_buff_atomic(33_333, Some(GUILD), BankruptBuff::Reroll)
            .expect("first claim")
    );
    assert!(
        !fixture
            .repository
            .claim_bankrupt_buff_atomic(33_333, Some(GUILD), BankruptBuff::Reroll)
            .expect("duplicate claim")
    );
}

#[test]
fn test_new_mana_day_resets_flags() {
    let fixture = Fixture::new();
    fixture
        .repository
        .set_mana(44_444, Some(GUILD), "Forest", "2020-01-01")
        .expect("seed old mana day");
    assert!(
        fixture
            .repository
            .claim_bankrupt_buff_atomic(44_444, Some(GUILD), BankruptBuff::Insurance)
            .expect("claim old insurance")
    );

    assert!(
        fixture
            .repository
            .claim_mana_atomic(44_444, Some(GUILD), "Forest", TODAY)
            .expect("advance mana day")
    );
    assert!(
        !fixture
            .repository
            .is_bankrupt_buff_used(44_444, Some(GUILD), BankruptBuff::Insurance)
            .expect("new day reset")
    );
}

#[test]
fn test_unknown_buff_raises() {
    assert_eq!(
        BankruptBuff::try_from("insurance").expect("known insurance"),
        BankruptBuff::Insurance
    );
    assert_eq!(
        BankruptBuff::try_from("reroll").expect("known reroll"),
        BankruptBuff::Reroll
    );
    assert_eq!(
        BankruptBuff::try_from("bogus")
            .expect_err("unknown buff must be rejected")
            .to_string(),
        "unknown bankrupt buff \"bogus\""
    );
}
