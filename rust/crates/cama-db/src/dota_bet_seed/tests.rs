use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::{NamedTempFile, TempDir};

use super::*;
use crate::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};

const GUILD: i64 = 42;

struct Fixture {
    _file: NamedTempFile,
    repository: DotaBetSeedRepository,
    path: PathBuf,
}

struct MigratedFixture {
    _directory: TempDir,
    repository: DotaBetSeedRepository,
    path: PathBuf,
}

impl MigratedFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary migrated database");
        let path = directory.path().join("cama.db");
        initialize_or_migrate_with_settings(&path, &MigrationSettings::default())
            .expect("migrate temporary database");
        Self {
            repository: DotaBetSeedRepository::new(&path),
            _directory: directory,
            path,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open migrated fixture");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production SQLite foreign-key mode");
        connection
    }

    fn fund(&self, guild_id: i64, total: i64) {
        self.connection()
            .execute(
                "INSERT INTO nonprofit_fund (guild_id, total_collected)
                 VALUES (?1, ?2)
                 ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
                params![guild_id, total],
            )
            .expect("set migrated fund");
    }

    fn set_pools(&self, guild_id: i64, open: i64, low_skill: i64) {
        self.connection()
            .execute(
                "UPDATE nonprofit_fund
                    SET first_game_open_pool = ?2,
                        first_game_lowskill_pool = ?3
                  WHERE guild_id = ?1",
                params![guild_id, open, low_skill],
            )
            .expect("set migrated pools");
    }

    fn set_queued(&self, guild_id: i64, amount: i64) {
        self.connection()
            .execute(
                "UPDATE nonprofit_fund SET next_match_pot = ?2 WHERE guild_id = ?1",
                params![guild_id, amount],
            )
            .expect("set migrated queued pot");
    }

    fn pending(&self, guild_id: i64, pending_match_id: i64) {
        self.connection()
            .execute(
                "INSERT INTO pending_matches (guild_id, pending_match_id, payload)
                 VALUES (?1, ?2, '{\"shuffle_timestamp\":1700000000}')",
                params![guild_id, pending_match_id],
            )
            .expect("insert migrated pending match");
    }
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("create disposable database");
        let path = file.path().to_path_buf();
        let connection = Connection::open(&path).expect("open disposable database");
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE nonprofit_fund (
                     guild_id INTEGER PRIMARY KEY,
                     total_collected INTEGER NOT NULL DEFAULT 0,
                     next_match_pot INTEGER NOT NULL DEFAULT 0,
                     first_game_open_pool INTEGER NOT NULL DEFAULT 0,
                     first_game_lowskill_pool INTEGER NOT NULL DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                 );
                 CREATE TABLE pending_matches (
                     pending_match_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     payload TEXT NOT NULL DEFAULT '{}',
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (guild_id, pending_match_id)
                 );
                 CREATE TABLE first_game_pool_funding_days (
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     game_date TEXT NOT NULL,
                     daily_amount INTEGER NOT NULL,
                     funded INTEGER NOT NULL CHECK(funded IN (0, 1)),
                     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (guild_id, game_date)
                 );
                 CREATE TABLE first_game_pool_claims (
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     lobby_kind TEXT NOT NULL CHECK(lobby_kind IN ('open', 'lowskill')),
                     game_date TEXT NOT NULL,
                     pending_match_id INTEGER NOT NULL,
                     amount INTEGER NOT NULL CHECK(amount >= 0),
                     settled INTEGER NOT NULL DEFAULT 0 CHECK(settled IN (0, 1)),
                     created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (guild_id, lobby_kind, game_date),
                     UNIQUE (guild_id, pending_match_id)
                 );",
            )
            .expect("create Python-shaped fixture schema");
        drop(connection);
        Self {
            repository: DotaBetSeedRepository::new(&path),
            _file: file,
            path,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(&self.path).expect("open fixture")
    }

    fn fund(&self, guild_id: i64, total: i64) {
        self.connection()
            .execute(
                "INSERT INTO nonprofit_fund (guild_id, total_collected)
                 VALUES (?1, ?2)
                 ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
                params![guild_id, total],
            )
            .expect("set fund");
    }

    fn queued_fund(&self, guild_id: i64, total: i64, queued: i64) {
        self.connection()
            .execute(
                "INSERT INTO nonprofit_fund (guild_id, total_collected, next_match_pot)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(guild_id) DO UPDATE SET
                    total_collected = excluded.total_collected,
                    next_match_pot = excluded.next_match_pot",
                params![guild_id, total, queued],
            )
            .expect("set queued fund");
    }

    fn pending(&self, guild_id: i64, pending_match_id: i64) {
        self.connection()
            .execute(
                "INSERT INTO pending_matches (guild_id, pending_match_id, payload)
                 VALUES (?1, ?2, '{\"shuffle_timestamp\":1700000000}')",
                params![guild_id, pending_match_id],
            )
            .expect("insert pending match");
    }

    fn queued(&self, guild_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT next_match_pot FROM nonprofit_fund WHERE guild_id = ?1",
                [guild_id],
                |row| row.get(0),
            )
            .expect("read queued pot")
    }

    fn legacy_seed(&self, guild_id: i64, pending_match_id: i64, seed: SeedReservation) {
        self.connection()
            .execute(
                "UPDATE pending_matches SET payload = json_set(
                    payload,
                    '$.bet_seed_reserved', ?1,
                    '$.bet_seed_radiant', ?2,
                    '$.bet_seed_dire', ?3,
                    '$.bet_seed_bonus', ?4,
                    '$.first_game_pool_reserved', ?5
                 ) WHERE guild_id = ?6 AND pending_match_id = ?7",
                params![
                    seed.reserved,
                    seed.split.radiant,
                    seed.split.dire,
                    seed.split.bonus,
                    seed.first_game_reserved,
                    guild_id,
                    pending_match_id
                ],
            )
            .expect("write legacy seed")
            .eq(&1)
            .then_some(())
            .expect("legacy pending match exists");
    }
}

fn fund_daily_and_claim(
    fixture: &Fixture,
    lobby: FirstGameLobby,
    date: &str,
    pending_match_id: i64,
) -> i64 {
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), date, 100)
        .expect("fund daily pools");
    fixture
        .repository
        .claim_first_game_pool(Some(GUILD), lobby, date, pending_match_id)
        .expect("claim pool")
}

// Exact mappings for tests/test_first_game_pool.py. These intentionally use
// the real Rust schema manager instead of the smaller seed-only fixture above,
// proving that the repository works against the database admitted in
// production.

#[test]
fn test_daily_funding_moves_exact_pair_and_preserves_monetary_stock() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 500);
    let stock_before = fixture
        .repository
        .seed_stock(Some(GUILD))
        .unwrap()
        .monetary_stock();

    let result = fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();

    assert_eq!(result.processed_dates, ["2026-08-05"]);
    assert_eq!(result.funded_dates, ["2026-08-05"]);
    assert!(result.skipped_dates.is_empty());
    assert_eq!(
        result.balances,
        FirstGamePoolBalances {
            open: 100,
            low_skill: 100
        }
    );
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        300
    );
    assert_eq!(
        fixture
            .repository
            .seed_stock(Some(GUILD))
            .unwrap()
            .monetary_stock(),
        stock_before
    );
}

#[test]
fn test_daily_funding_skips_both_pools_when_reserve_is_below_pair_cost() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 199);

    let first = fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();
    fixture.fund(GUILD, 399);
    let retry = fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();

    assert!(first.funded_dates.is_empty());
    assert_eq!(first.skipped_dates, ["2026-08-05"]);
    assert!(retry.processed_dates.is_empty());
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        399
    );
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances::default()
    );
}

#[test]
fn test_daily_funding_stacks_missed_game_dates_oldest_first() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 600);
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-03", 100)
        .unwrap();

    let result = fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();

    assert_eq!(result.processed_dates, ["2026-08-04", "2026-08-05"]);
    assert_eq!(result.funded_dates, ["2026-08-04", "2026-08-05"]);
    assert!(result.skipped_dates.is_empty());
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        0
    );
    assert_eq!(
        result.balances,
        FirstGamePoolBalances {
            open: 300,
            low_skill: 300
        }
    );
}

#[test]
fn test_daily_funding_is_guild_scoped() {
    let fixture = MigratedFixture::new();
    let other = GUILD + 1;
    fixture.fund(GUILD, 200);
    fixture.fund(other, 199);

    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();
    fixture
        .repository
        .fund_first_game_pools(Some(other), "2026-08-05", 100)
        .unwrap();

    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances {
            open: 100,
            low_skill: 100
        }
    );
    assert_eq!(
        fixture.repository.pool_balances(Some(other)).unwrap(),
        FirstGamePoolBalances::default()
    );
}

#[test]
fn test_pool_previews_add_regular_seed_to_each_lobby_balance() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 200);
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 40)
        .unwrap();

    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(GUILD), 50, None, 0)
            .unwrap(),
        FirstGamePoolBalances {
            open: 90,
            low_skill: 90
        }
    );
}

#[test]
fn test_pool_previews_include_unprocessed_current_day_funding() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 250);

    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(GUILD), 50, Some("2026-08-05"), 100)
            .unwrap(),
        FirstGamePoolBalances {
            open: 150,
            low_skill: 150
        }
    );
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        250
    );
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances::default()
    );
}

#[test]
fn test_pool_previews_do_not_repeat_processed_current_day_funding() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 500);
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();

    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(GUILD), 50, Some("2026-08-05"), 100)
            .unwrap(),
        FirstGamePoolBalances {
            open: 150,
            low_skill: 150
        }
    );
}

#[test]
fn test_pool_previews_limit_regular_seed_to_available_reserve() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 30);

    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(GUILD), 50, None, 0)
            .unwrap(),
        FirstGamePoolBalances {
            open: 30,
            low_skill: 30
        }
    );
}

#[test]
fn test_pool_previews_use_entire_queued_next_match_pot() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 500);
    fixture.set_queued(GUILD, 125);

    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(GUILD), 50, None, 0)
            .unwrap(),
        FirstGamePoolBalances {
            open: 125,
            low_skill: 125
        }
    );
}

#[test]
fn test_pool_previews_keep_lobby_balances_isolated() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 300);
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(GUILD), 50, None, 0)
            .unwrap(),
        FirstGamePoolBalances {
            open: 150,
            low_skill: 150
        }
    );

    fixture.set_pools(GUILD, 40, 70);
    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(GUILD), 50, None, 0)
            .unwrap(),
        FirstGamePoolBalances {
            open: 90,
            low_skill: 120
        }
    );
}

#[test]
fn test_pool_previews_return_empty_lobby_balances_for_missing_fund_row() {
    let fixture = MigratedFixture::new();
    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(GUILD), 50, None, 0)
            .unwrap(),
        FirstGamePoolBalances::default()
    );
}

#[test]
fn test_pool_previews_are_guild_scoped() {
    let fixture = MigratedFixture::new();
    let other = GUILD + 1;
    fixture.fund(GUILD, 300);
    fixture.fund(other, 100);
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();
    fixture
        .repository
        .fund_first_game_pools(Some(other), "2026-08-05", 40)
        .unwrap();

    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(GUILD), 50, None, 0)
            .unwrap(),
        FirstGamePoolBalances {
            open: 150,
            low_skill: 150
        }
    );
    assert_eq!(
        fixture
            .repository
            .first_game_pool_previews(Some(other), 50, None, 0)
            .unwrap(),
        FirstGamePoolBalances {
            open: 60,
            low_skill: 60
        }
    );
}

#[test]
fn test_claims_are_once_per_game_date_and_independent_by_lobby() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 400);
    for pending_match_id in [501, 502, 503] {
        fixture.pending(GUILD, pending_match_id);
    }
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();

    assert_eq!(
        fixture
            .repository
            .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-05", 501)
            .unwrap(),
        100
    );
    assert_eq!(
        fixture
            .repository
            .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-05", 502)
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .repository
            .claim_first_game_pool(Some(GUILD), FirstGameLobby::LowSkill, "2026-08-05", 503)
            .unwrap(),
        100
    );
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances::default()
    );
}

#[test]
fn test_same_pending_match_reclaim_after_game_date_rollover_is_idempotent() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 400);
    fixture.pending(GUILD, 701);
    fixture.pending(GUILD, 702);
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-05", 701)
            .unwrap(),
        100
    );

    assert_eq!(
        fixture
            .repository
            .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-06", 701)
            .unwrap(),
        100
    );
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-06", 100)
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-06", 702)
            .unwrap(),
        100
    );
}

#[test]
fn test_next_game_date_claim_receives_newly_stacked_balance() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 400);
    fixture.pending(GUILD, 601);
    fixture.pending(GUILD, 602);
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-05", 601)
            .unwrap(),
        100
    );

    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-06", 100)
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-06", 602)
            .unwrap(),
        100
    );
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap().open,
        0
    );
}

#[test]
fn stronger_funding_and_display_outbox_are_one_atomic_commit() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 200);
    fixture
        .connection()
        .execute("DROP TABLE app_kv", [])
        .expect("remove outbox table to inject enqueue failure");

    let error = fixture
        .repository
        .fund_first_game_pools_and_queue_refresh(Some(GUILD), "2026-08-05", 100)
        .expect_err("outbox failure must roll back funding");

    assert!(error.to_string().contains("app_kv"));
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        200
    );
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances::default()
    );
    let processed: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM first_game_pool_funding_days",
            [],
            |row| row.get(0),
        )
        .expect("read rolled-back funding days");
    assert_eq!(processed, 0);
}

#[test]
fn stronger_display_ack_cannot_clear_a_newer_generation() {
    let fixture = MigratedFixture::new();
    fixture.fund(GUILD, 400);
    fixture
        .repository
        .fund_first_game_pools_and_queue_refresh(Some(GUILD), "2026-08-05", 100)
        .unwrap();
    let old = fixture
        .repository
        .pending_first_game_pool_refreshes()
        .unwrap()
        .pop()
        .expect("old display generation");
    fixture
        .repository
        .fund_first_game_pools_and_queue_refresh(Some(GUILD), "2026-08-06", 100)
        .unwrap();

    assert!(
        !fixture
            .repository
            .acknowledge_first_game_pool_refresh(&old)
            .unwrap()
    );
    let pending = fixture
        .repository
        .pending_first_game_pool_refreshes()
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].generation, "2026-08-06");
}

#[test]
fn test_pool_shuffle_reserves_split_seed_from_nonprofit_fund() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 17);
    fixture.pending(GUILD, 1);

    let seed = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 1, 50, 0, BettingMode::Pool)
        .expect("reserve pool seed");

    assert_eq!(seed.reserved, 17);
    assert_eq!(
        seed.split,
        SeedSplit {
            radiant: 9,
            dire: 8,
            bonus: 0
        }
    );
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        0
    );
}

#[test]
fn test_first_pool_match_claims_stacked_seed_once_for_each_lobby() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 500);
    fixture.pending(GUILD, 10);
    fixture.pending(GUILD, 11);
    fixture.pending(GUILD, 12);
    fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-08-05", 100)
        .unwrap();

    let open_first = fixture
        .repository
        .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-05", 10)
        .unwrap();
    let open_first_seed = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 10, 50, open_first, BettingMode::Pool)
        .unwrap();
    let open_second = fixture
        .repository
        .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-05", 11)
        .unwrap();
    let open_second_seed = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 11, 50, open_second, BettingMode::Pool)
        .unwrap();
    let low_first = fixture
        .repository
        .claim_first_game_pool(Some(GUILD), FirstGameLobby::LowSkill, "2026-08-05", 12)
        .unwrap();
    let low_first_seed = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 12, 50, low_first, BettingMode::Pool)
        .unwrap();

    assert_eq!((open_first, open_second, low_first), (100, 0, 100));
    assert_eq!(open_first_seed.reserved, 150);
    assert_eq!(
        open_first_seed.split,
        SeedSplit {
            radiant: 75,
            dire: 75,
            bonus: 0
        }
    );
    assert_eq!(open_second_seed.reserved, 50);
    assert_eq!(low_first_seed, open_first_seed);
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances::default()
    );
}

#[test]
fn test_cancelled_first_pool_match_restores_pool_and_daily_claim() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 300);
    fixture.pending(GUILD, 20);
    fixture.pending(GUILD, 21);
    let first_game = fund_daily_and_claim(&fixture, FirstGameLobby::Open, "2026-08-05", 20);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 20, 50, first_game, BettingMode::Pool)
        .unwrap();

    let abort = fixture
        .repository
        .abort_seed_atomic(Some(GUILD), 20)
        .unwrap();
    let second_claim = fixture
        .repository
        .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-05", 21)
        .unwrap();
    let second = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 21, 50, second_claim, BettingMode::Pool)
        .unwrap();

    assert_eq!(abort.first_game_restored, 100);
    assert_eq!(abort.returned_to_fund, 50);
    assert_eq!(second_claim, 100);
    assert_eq!(second.reserved, 150);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        50
    );
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances {
            open: 0,
            low_skill: 100
        }
    );
}

#[test]
fn test_claimed_first_pool_stays_in_monetary_stock_while_match_is_pending() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 300);
    fixture.pending(GUILD, 30);
    let before = fixture
        .repository
        .seed_stock(Some(GUILD))
        .unwrap()
        .monetary_stock();
    let first_game = fund_daily_and_claim(&fixture, FirstGameLobby::Open, "2026-08-05", 30);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 30, 0, first_game, BettingMode::Pool)
        .unwrap();

    assert_eq!(first_game, 100);
    assert_eq!(
        fixture
            .repository
            .seed_stock(Some(GUILD))
            .unwrap()
            .monetary_stock(),
        before
    );
}

#[test]
fn test_settled_first_game_claim_cannot_be_restored_by_later_abort_retry() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 300);
    fixture.pending(GUILD, 40);
    let before = fixture
        .repository
        .seed_stock(Some(GUILD))
        .unwrap()
        .monetary_stock();
    let first_game = fund_daily_and_claim(&fixture, FirstGameLobby::Open, "2026-08-05", 40);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 40, 0, first_game, BettingMode::Pool)
        .unwrap();

    let settlement = fixture
        .repository
        .settle_seed_atomic(Some(GUILD), 40, BettingMode::Pool, None)
        .unwrap();
    let abort = fixture
        .repository
        .abort_seed_atomic(Some(GUILD), 40)
        .unwrap();

    assert_eq!(settlement.first_game_settled, 100);
    assert_eq!(abort, SeedAbort::default());
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances {
            open: 0,
            low_skill: 100
        }
    );
    assert_eq!(
        fixture
            .repository
            .seed_stock(Some(GUILD))
            .unwrap()
            .monetary_stock(),
        before
    );
}

#[test]
fn test_settled_daily_claim_stays_consumed_after_older_match_restores_pool() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 400);
    fixture.pending(GUILD, 50);
    fixture.pending(GUILD, 51);
    fixture.pending(GUILD, 52);
    let older = fund_daily_and_claim(&fixture, FirstGameLobby::Open, "2026-08-04", 50);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 50, 0, older, BettingMode::Pool)
        .unwrap();
    let current = fund_daily_and_claim(&fixture, FirstGameLobby::Open, "2026-08-05", 51);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 51, 0, current, BettingMode::Pool)
        .unwrap();
    fixture
        .repository
        .settle_seed_atomic(Some(GUILD), 51, BettingMode::Pool, None)
        .unwrap();
    fixture
        .repository
        .abort_seed_atomic(Some(GUILD), 51)
        .unwrap();
    fixture
        .repository
        .abort_seed_atomic(Some(GUILD), 50)
        .unwrap();

    let later = fixture
        .repository
        .claim_first_game_pool(Some(GUILD), FirstGameLobby::Open, "2026-08-05", 52)
        .unwrap();

    assert_eq!(later, 0);
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap().open,
        100
    );
}

#[test]
fn test_first_game_claim_retry_recovers_after_seed_state_persist_failure() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 200);
    fixture.pending(GUILD, 60);
    let claim = fund_daily_and_claim(&fixture, FirstGameLobby::Open, "2026-08-05", 60);
    let committed = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 60, 0, claim, BettingMode::Pool)
        .unwrap();
    let reloaded = fixture.repository.seed_state(Some(GUILD), 60).unwrap();
    let retry = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 60, 0, claim, BettingMode::Pool)
        .unwrap();

    assert_eq!(
        committed,
        SeedReservation {
            reserved: 100,
            split: SeedSplit {
                radiant: 50,
                dire: 50,
                bonus: 0
            },
            first_game_reserved: 100
        }
    );
    assert_eq!(reloaded, committed);
    assert_eq!(retry, committed);
}

#[test]
fn test_settlement_recovers_first_game_claim_when_seed_state_never_persisted() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 200);
    fixture.pending(GUILD, 70);
    let before = fixture
        .repository
        .seed_stock(Some(GUILD))
        .unwrap()
        .monetary_stock();
    assert_eq!(
        fund_daily_and_claim(&fixture, FirstGameLobby::Open, "2026-08-05", 70),
        100
    );

    let settlement = fixture
        .repository
        .settle_seed_atomic(Some(GUILD), 70, BettingMode::Pool, None)
        .unwrap();

    assert_eq!(
        settlement.routed,
        SeedSplit {
            radiant: 50,
            dire: 50,
            bonus: 0
        }
    );
    assert_eq!(settlement.returned_to_fund, 100);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        100
    );
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances {
            open: 0,
            low_skill: 100
        }
    );
    assert_eq!(
        fixture
            .repository
            .seed_stock(Some(GUILD))
            .unwrap()
            .monetary_stock(),
        before
    );
}

#[test]
fn test_regular_seed_persist_failure_cannot_strand_or_double_deduct() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 300);
    fixture.pending(GUILD, 80);
    let claim = fund_daily_and_claim(&fixture, FirstGameLobby::Open, "2026-08-05", 80);
    let first = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 80, 50, claim, BettingMode::Pool)
        .unwrap();
    let retry = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 80, 50, claim, BettingMode::Pool)
        .unwrap();

    assert_eq!(first.reserved, 150);
    assert_eq!(retry, first);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        50
    );
    let abort = fixture
        .repository
        .abort_seed_atomic(Some(GUILD), 80)
        .unwrap();
    assert_eq!(abort.returned_to_fund, 50);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        100
    );
    assert_eq!(
        fixture.repository.pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances {
            open: 100,
            low_skill: 100
        }
    );
}

#[test]
fn test_reserve_bet_seed_atomic_retry_deducts_fund_exactly_once() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 300);
    fixture.pending(GUILD, 90);
    let first = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 90, 50, 100, BettingMode::Pool)
        .unwrap();
    let second = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 90, 50, 100, BettingMode::Pool)
        .unwrap();

    assert_eq!(
        first,
        SeedReservation {
            reserved: 150,
            split: SeedSplit {
                radiant: 75,
                dire: 75,
                bonus: 0
            },
            first_game_reserved: 100
        }
    );
    assert_eq!(second, first);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        250
    );
}

#[test]
fn test_settlement_recovers_regular_seed_when_seed_state_never_persisted() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 60);
    fixture.pending(GUILD, 100);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 100, 50, 0, BettingMode::House)
        .unwrap();
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        10
    );

    let settlement = fixture
        .repository
        .settle_seed_atomic(Some(GUILD), 100, BettingMode::House, None)
        .unwrap();

    assert_eq!(settlement.returned_to_fund, 50);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        60
    );
}

#[test]
fn test_pool_shuffle_consumes_entire_queued_next_match_pot() {
    let fixture = Fixture::new();
    fixture.queued_fund(GUILD, 25, 175);
    fixture.pending(GUILD, 110);

    let seed = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 110, 50, 0, BettingMode::Pool)
        .unwrap();

    assert_eq!(seed.reserved, 175);
    assert_eq!(
        seed.split,
        SeedSplit {
            radiant: 88,
            dire: 87,
            bonus: 0
        }
    );
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        25
    );
    assert_eq!(fixture.queued(GUILD), 0);
}

#[test]
fn test_house_shuffle_reserves_partial_seed_as_bonus_pool() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 17);
    fixture.pending(GUILD, 120);

    let seed = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 120, 50, 0, BettingMode::House)
        .unwrap();

    assert_eq!(
        seed,
        SeedReservation {
            reserved: 17,
            split: SeedSplit {
                radiant: 0,
                dire: 0,
                bonus: 17
            },
            first_game_reserved: 0
        }
    );
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        0
    );
}

#[test]
fn test_shuffle_with_empty_reserve_stores_zero_seed_pool() {
    let fixture = Fixture::new();
    fixture.pending(GUILD, 130);

    let seed = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 130, 50, 0, BettingMode::Pool)
        .unwrap();

    assert_eq!(seed, SeedReservation::default());
    assert_eq!(
        fixture.repository.seed_state(Some(GUILD), 130).unwrap(),
        SeedReservation::default()
    );
}

#[test]
fn test_shuffle_with_empty_reserve_stores_zero_seed_house() {
    let fixture = Fixture::new();
    fixture.pending(GUILD, 131);

    let seed = fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 131, 50, 0, BettingMode::House)
        .unwrap();

    assert_eq!(seed, SeedReservation::default());
    assert_eq!(
        fixture.repository.seed_state(Some(GUILD), 131).unwrap(),
        SeedReservation::default()
    );
}

#[test]
fn test_settlement_returns_all_seed_when_match_has_no_bets() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 100);
    fixture.pending(GUILD, 140);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 140, 50, 0, BettingMode::Pool)
        .unwrap();

    let settlement = fixture
        .repository
        .settle_seed_atomic(Some(GUILD), 140, BettingMode::Pool, None)
        .unwrap();

    assert_eq!(settlement.returned_to_fund, 50);
    assert_eq!(settlement.winner_seed, 0);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        100
    );
}

#[test]
fn test_settlement_zeroes_persisted_seed_before_retry() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 100);
    fixture.pending(GUILD, 150);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 150, 50, 0, BettingMode::Pool)
        .unwrap();
    let first = fixture
        .repository
        .settle_seed_atomic(Some(GUILD), 150, BettingMode::Pool, None)
        .unwrap();
    let reloaded = fixture.repository.seed_state(Some(GUILD), 150).unwrap();
    let second = fixture
        .repository
        .settle_seed_atomic(Some(GUILD), 150, BettingMode::Pool, None)
        .unwrap();

    assert_eq!(first.returned_to_fund, 50);
    assert_eq!(reloaded, SeedReservation::default());
    assert_eq!(second, SeedSettlement::default());
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        100
    );
}

#[test]
fn test_house_settlement_returns_seed_bonus_when_no_real_winners() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 100);
    fixture.pending(GUILD, 160);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 160, 50, 0, BettingMode::House)
        .unwrap();

    let settlement = fixture
        .repository
        .settle_seed_atomic(Some(GUILD), 160, BettingMode::House, None)
        .unwrap();

    assert_eq!(settlement.returned_to_fund, 50);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        100
    );
}

#[test]
fn test_abort_refunds_reserved_seed_even_without_bets() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 40);
    fixture.pending(GUILD, 170);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 170, 50, 0, BettingMode::Pool)
        .unwrap();

    let abort = fixture
        .repository
        .abort_seed_atomic(Some(GUILD), 170)
        .unwrap();

    assert_eq!(abort.returned_to_fund, 40);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        40
    );
}

#[test]
fn test_abort_refund_zeroes_persisted_seed_before_retry() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 40);
    fixture.pending(GUILD, 180);
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 180, 50, 0, BettingMode::Pool)
        .unwrap();

    let first = fixture
        .repository
        .abort_seed_atomic(Some(GUILD), 180)
        .unwrap();
    let reloaded = fixture.repository.seed_state(Some(GUILD), 180).unwrap();
    let second = fixture
        .repository
        .abort_seed_atomic(Some(GUILD), 180)
        .unwrap();

    assert_eq!(first.returned_to_fund, 40);
    assert_eq!(reloaded, SeedReservation::default());
    assert_eq!(second, SeedAbort::default());
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        40
    );
}

#[test]
fn test_house_settlement_returns_legacy_split_seed_when_no_winners() {
    let fixture = Fixture::new();
    fixture.pending(GUILD, 190);
    fixture.legacy_seed(
        GUILD,
        190,
        SeedReservation {
            reserved: 175,
            split: SeedSplit {
                radiant: 88,
                dire: 87,
                bonus: 0,
            },
            first_game_reserved: 0,
        },
    );

    let settlement = fixture
        .repository
        .settle_seed_atomic(Some(GUILD), 190, BettingMode::House, None)
        .unwrap();

    assert_eq!(
        settlement.routed,
        SeedSplit {
            radiant: 0,
            dire: 0,
            bonus: 175
        }
    );
    assert_eq!(settlement.returned_to_fund, 175);
    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        175
    );
}

#[test]
fn stronger_reservation_is_atomic_under_concurrent_retries() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 100);
    fixture.pending(GUILD, 200);
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            repository
                .reserve_seed_atomic(Some(GUILD), 200, 50, 0, BettingMode::Pool)
                .expect("concurrent reserve")
        }));
    }
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("join reserve"))
        .collect::<Vec<_>>();

    assert_eq!(results[0], results[1]);
    assert_eq!(repository.nonprofit_balance(Some(GUILD)).unwrap(), 50);
}

#[test]
fn stronger_seed_rows_are_guild_isolated_even_with_same_pending_id() {
    let fixture = Fixture::new();
    for guild_id in [GUILD, GUILD + 1] {
        fixture.fund(guild_id, 80);
        fixture.pending(guild_id, 210);
    }
    fixture
        .repository
        .reserve_seed_atomic(Some(GUILD), 210, 50, 0, BettingMode::Pool)
        .unwrap();

    assert_eq!(
        fixture.repository.nonprofit_balance(Some(GUILD)).unwrap(),
        30
    );
    assert_eq!(
        fixture
            .repository
            .nonprofit_balance(Some(GUILD + 1))
            .unwrap(),
        80
    );
    assert_eq!(
        fixture.repository.seed_state(Some(GUILD + 1), 210).unwrap(),
        SeedReservation::default()
    );
}

#[test]
fn stronger_daily_funding_catches_up_dates_and_records_unfunded_days() {
    let fixture = Fixture::new();
    fixture.fund(GUILD, 500);
    let first = fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-02-28", 100)
        .unwrap();
    let catch_up = fixture
        .repository
        .fund_first_game_pools(Some(GUILD), "2026-03-02", 100)
        .unwrap();

    assert_eq!(first.funded_dates, ["2026-02-28"]);
    assert_eq!(catch_up.processed_dates, ["2026-03-01", "2026-03-02"]);
    assert_eq!(catch_up.funded_dates, ["2026-03-01"]);
    assert_eq!(catch_up.skipped_dates, ["2026-03-02"]);
    assert_eq!(
        catch_up.balances,
        FirstGamePoolBalances {
            open: 200,
            low_skill: 200
        }
    );
}

#[test]
#[ignore = "cross-language smoke invokes the repository Python environment"]
fn python_migrated_database_dota_seed_interop_smoke() {
    use std::path::Path;
    use std::process::Command;

    let file = NamedTempFile::new().expect("temporary interop database");
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root");
    let python = repository_root.join(".venv/bin/python");
    let initialize = Command::new(&python)
        .current_dir(repository_root)
        .args([
            "-c",
            "import sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\np=sys.argv[1]\nSchemaManager(p).initialize()\nc=sqlite3.connect(p)\nc.execute(\"INSERT INTO nonprofit_fund (guild_id,total_collected,next_match_pot,first_game_open_pool,first_game_lowskill_pool) VALUES (?,?,?,?,?)\",(4242,260,0,0,0))\nc.execute(\"INSERT INTO pending_matches (pending_match_id,guild_id,payload) VALUES (?,?,?)\",(900,4242,'{\\\"shuffle_timestamp\\\":1700000000}'))\nc.commit(); c.close()",
            file.path().to_str().expect("UTF-8 path"),
        ])
        .status()
        .expect("initialize Python-migrated database");
    assert!(initialize.success());

    let repository = DotaBetSeedRepository::new(file.path());
    let funding = repository
        .fund_first_game_pools(Some(4242), "2026-08-05", 100)
        .expect("fund through Python schema");
    let claim = repository
        .claim_first_game_pool(Some(4242), FirstGameLobby::Open, "2026-08-05", 900)
        .expect("claim through Python schema");
    let seed = repository
        .reserve_seed_atomic(Some(4242), 900, 50, claim, BettingMode::Pool)
        .expect("reserve through Python pending payload");
    let settlement = repository
        .settle_seed_atomic(Some(4242), 900, BettingMode::Pool, None)
        .expect("settle through Python schema");

    assert_eq!(funding.funded_dates, ["2026-08-05"]);
    assert_eq!(claim, 100);
    assert_eq!(seed.reserved, 150);
    assert_eq!(settlement.returned_to_fund, 150);
    assert_eq!(repository.nonprofit_balance(Some(4242)).unwrap(), 160);
    assert_eq!(
        repository.pool_balances(Some(4242)).unwrap(),
        FirstGamePoolBalances {
            open: 0,
            low_skill: 100
        }
    );
}
