use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::{Arc, Barrier, OnceLock};
use std::thread;
use tempfile::NamedTempFile;

use super::{
    AutoBuyRequest, AutoBuySelection, AutoBuyStatus, BuyInsuranceOutcome, BuyItemOutcome,
    BuyItemRequest, DigInventoryRepository, QueueItemOutcome, SetTrapOutcome,
};
use crate::test_support::FastTestDatabase;

const USER: i64 = 10_001;
const GUILD: i64 = 12_345;
const NOW: i64 = 1_000_000;
const INVENTORY_LIMIT: usize = 5;

enum FixtureDatabase {
    Fast(FastTestDatabase),
    Durable(NamedTempFile),
}

impl FixtureDatabase {
    fn path(&self) -> &Path {
        match self {
            Self::Fast(database) => database.path(),
            Self::Durable(database) => database.path(),
        }
    }
}

struct Fixture {
    database: FixtureDatabase,
    repository: DigInventoryRepository,
}

fn fixture_database(durable: bool) -> FixtureDatabase {
    static TEMPLATE: OnceLock<NamedTempFile> = OnceLock::new();
    let template = TEMPLATE.get_or_init(|| {
        let file = NamedTempFile::new().expect("create temporary SQLite file");
        let connection = Connection::open(file.path()).expect("open temporary SQLite database");
        connection
            .execute_batch(
                "
                PRAGMA journal_mode=WAL;
                CREATE TABLE players (
                    discord_id INTEGER NOT NULL,
                    guild_id INTEGER NOT NULL DEFAULT 0,
                    jopacoin_balance INTEGER NOT NULL DEFAULT 0,
                    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (discord_id, guild_id)
                );
                CREATE TABLE tunnels (
                    discord_id INTEGER NOT NULL,
                    guild_id INTEGER NOT NULL DEFAULT 0,
                    depth INTEGER NOT NULL DEFAULT 0,
                    trap_active INTEGER NOT NULL DEFAULT 0,
                    trap_free_today INTEGER NOT NULL DEFAULT 1,
                    trap_date TEXT,
                    insured_until INTEGER,
                    PRIMARY KEY (discord_id, guild_id)
                );
                CREATE TABLE dig_inventory (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    discord_id INTEGER NOT NULL,
                    guild_id INTEGER NOT NULL DEFAULT 0,
                    item_type TEXT NOT NULL,
                    queued INTEGER NOT NULL DEFAULT 0,
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
                    source TEXT NOT NULL DEFAULT 'balance_update',
                    actor_id INTEGER,
                    related_type TEXT,
                    related_id TEXT,
                    reason TEXT,
                    metadata TEXT,
                    created_at INTEGER NOT NULL DEFAULT 0
                );
                CREATE TRIGGER ledger_players_insert
                AFTER INSERT ON players
                WHEN COALESCE(NEW.jopacoin_balance, 0) != 0
                BEGIN
                    INSERT INTO economy_ledger_entries (
                        guild_id, account_type, account_id, delta,
                        balance_before, balance_after, source, actor_id,
                        related_type, related_id, reason, metadata
                    ) VALUES (
                        COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                        COALESCE(NEW.jopacoin_balance, 0), 0,
                        COALESCE(NEW.jopacoin_balance, 0),
                        COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1),
                                 'player_insert'),
                        (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                        (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                        (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                        (SELECT reason FROM economy_ledger_context WHERE id = 1),
                        (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                    );
                END;
                CREATE TRIGGER ledger_players_update
                AFTER UPDATE OF jopacoin_balance ON players
                WHEN COALESCE(OLD.jopacoin_balance, 0) != COALESCE(NEW.jopacoin_balance, 0)
                BEGIN
                    INSERT INTO economy_ledger_entries (
                        guild_id, account_type, account_id, delta,
                        balance_before, balance_after, source, actor_id,
                        related_type, related_id, reason, metadata
                    ) VALUES (
                        COALESCE(NEW.guild_id, 0), 'player', NEW.discord_id,
                        COALESCE(NEW.jopacoin_balance, 0) -
                            COALESCE(OLD.jopacoin_balance, 0),
                        COALESCE(OLD.jopacoin_balance, 0),
                        COALESCE(NEW.jopacoin_balance, 0),
                        COALESCE((SELECT source FROM economy_ledger_context WHERE id = 1),
                                 'balance_update'),
                        (SELECT actor_id FROM economy_ledger_context WHERE id = 1),
                        (SELECT related_type FROM economy_ledger_context WHERE id = 1),
                        (SELECT related_id FROM economy_ledger_context WHERE id = 1),
                        (SELECT reason FROM economy_ledger_context WHERE id = 1),
                        (SELECT metadata FROM economy_ledger_context WHERE id = 1)
                    );
                END;
                ",
            )
            .expect("create test-only existing schema");
        drop(connection);
        file
    });
    if durable {
        let file = NamedTempFile::new().expect("durable inventory database");
        std::fs::copy(template.path(), file.path()).expect("copy inventory fixture template");
        FixtureDatabase::Durable(file)
    } else {
        FixtureDatabase::Fast(FastTestDatabase::from_template(template.path()))
    }
}

impl Fixture {
    fn new() -> Self {
        Self::from_database(fixture_database(false))
    }

    fn durable() -> Self {
        Self::from_database(fixture_database(true))
    }

    fn from_database(database: FixtureDatabase) -> Self {
        let repository = DigInventoryRepository::new(database.path());
        Self {
            database,
            repository,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("open fixture database")
    }

    fn register(&self, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (discord_id, guild_id, jopacoin_balance)
                 VALUES (?1, ?2, ?3)",
                params![USER, GUILD, balance],
            )
            .expect("register player");
    }

    fn create_tunnel(&self, depth: i64) {
        self.connection()
            .execute(
                "INSERT INTO tunnels
                     (discord_id, guild_id, depth, trap_active, trap_free_today)
                 VALUES (?1, ?2, ?3, 0, 1)",
                params![USER, GUILD, depth],
            )
            .expect("create tunnel");
    }

    fn clear_trap(&self) {
        self.connection()
            .execute(
                "UPDATE tunnels SET trap_active = 0
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![USER, GUILD],
            )
            .expect("clear trap");
    }

    fn set_balance(&self, balance: i64) {
        self.connection()
            .execute(
                "UPDATE players SET jopacoin_balance = ?1
                 WHERE discord_id = ?2 AND guild_id = ?3",
                params![balance, USER, GUILD],
            )
            .expect("set balance");
    }

    fn dig_ledger(&self) -> Vec<(i64, String, String, String)> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT delta, source, related_type, reason
                 FROM economy_ledger_entries
                 WHERE guild_id = ?1 AND account_id = ?2 AND source = 'dig'
                 ORDER BY ledger_id",
            )
            .expect("prepare ledger query");
        statement
            .query_map(params![GUILD, USER], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("query ledger")
            .collect::<Result<Vec<_>, _>>()
            .expect("decode ledger")
    }
}

fn buy(
    fixture: &Fixture,
    item_type: &str,
    price: i64,
    auto_queue_if_available: bool,
) -> BuyItemOutcome {
    fixture
        .repository
        .buy_item_atomic(BuyItemRequest {
            discord_id: USER,
            guild_id: Some(GUILD),
            item_type,
            price,
            inventory_limit: INVENTORY_LIMIT,
            auto_queue_if_available,
            created_at: NOW,
        })
        .expect("buy item")
}

fn auto_buy(
    fixture: &Fixture,
    selections: &[AutoBuySelection<'_>],
    reserved_balance: i64,
    observed_balance: Option<i64>,
) -> Vec<super::AutoBuyItemOutcome> {
    fixture
        .repository
        .ensure_auto_buy_items_atomic(AutoBuyRequest {
            discord_id: USER,
            guild_id: Some(GUILD),
            selections,
            reserved_balance,
            inventory_limit: INVENTORY_LIMIT,
            created_at: NOW,
            observed_balance,
        })
        .expect("ensure auto-buy items")
}

fn add_item(fixture: &Fixture, item_type: &str) -> i64 {
    fixture
        .repository
        .add_item(USER, Some(GUILD), item_type, NOW)
        .expect("add inventory item")
}

#[test]
fn test_buy_item_debits_balance_and_adds_inventory() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    let outcome = buy(&fixture, "dynamite", 5, false);
    let BuyItemOutcome::Purchased {
        item_id,
        queued,
        cost,
        balance_after,
    } = outcome
    else {
        panic!("expected successful purchase: {outcome:?}");
    };
    assert_eq!(cost, 5);
    assert_eq!(balance_after, 95);
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 95);
    let inventory = fixture.repository.inventory(USER, Some(GUILD)).unwrap();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].id, item_id);
    assert_eq!(inventory[0].item_type, "dynamite");
    assert!(!inventory[0].queued);
    assert!(!queued);
}

fn assert_boss_preparation_purchase(item_type: &str, cost: i64) {
    let fixture = Fixture::new();
    fixture.register(200);
    fixture.create_tunnel(0);
    assert!(matches!(
        buy(&fixture, item_type, cost, false),
        BuyItemOutcome::Purchased {
            cost: actual_cost,
            queued: false,
            ..
        } if actual_cost == cost
    ));
}

#[test]
fn test_buy_boss_preparation_item_tempered_whetstone_60() {
    assert_boss_preparation_purchase("tempered_whetstone", 60);
}

#[test]
fn test_buy_boss_preparation_item_warding_salts_50() {
    assert_boss_preparation_purchase("warding_salts", 50);
}

#[test]
fn test_buy_boss_preparation_item_rescue_line_40() {
    assert_boss_preparation_purchase("rescue_line", 40);
}

#[test]
fn test_buy_hard_hat_auto_queues() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    let outcome = buy(&fixture, "hard_hat", 8, true);
    let BuyItemOutcome::Purchased {
        item_id,
        queued: true,
        ..
    } = outcome
    else {
        panic!("expected queued purchase: {outcome:?}");
    };
    let queued = fixture.repository.queued_items(USER, Some(GUILD)).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, item_id);
    assert_eq!(queued[0].item_type, "hard_hat");
}

#[test]
fn test_buy_torch_auto_queues() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    let outcome = buy(&fixture, "torch", 6, true);
    let BuyItemOutcome::Purchased {
        item_id,
        queued: true,
        ..
    } = outcome
    else {
        panic!("expected queued purchase: {outcome:?}");
    };
    let queued = fixture.repository.queued_items(USER, Some(GUILD)).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, item_id);
    assert_eq!(queued[0].item_type, "torch");
}

#[test]
fn test_buy_duplicate_auto_queue_item_stores_reserve() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    let first = buy(&fixture, "hard_hat", 8, true);
    let second = buy(&fixture, "hard_hat", 8, true);
    assert!(matches!(
        first,
        BuyItemOutcome::Purchased { queued: true, .. }
    ));
    assert!(matches!(
        second,
        BuyItemOutcome::Purchased { queued: false, .. }
    ));
    assert_eq!(
        fixture
            .repository
            .queued_items(USER, Some(GUILD))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .repository
            .inventory(USER, Some(GUILD))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn test_buy_streak_charm_adds_passive_item() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    assert!(matches!(
        buy(&fixture, "streak_charm", 15, false),
        BuyItemOutcome::Purchased {
            cost: 15,
            queued: false,
            ..
        }
    ));
    let inventory = fixture.repository.inventory(USER, Some(GUILD)).unwrap();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].item_type, "streak_charm");
}

#[test]
fn test_buy_item_requires_tunnel() {
    let fixture = Fixture::new();
    fixture.register(100);
    assert_eq!(
        buy(&fixture, "dynamite", 5, false),
        BuyItemOutcome::MissingTunnel
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 100);
}

#[test]
fn test_buy_item_insufficient_balance_rejected() {
    let fixture = Fixture::new();
    fixture.register(19);
    fixture.create_tunnel(0);
    assert_eq!(
        buy(&fixture, "void_bait", 20, false),
        BuyItemOutcome::InsufficientBalance {
            balance: 19,
            cost: 20,
        }
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 19);
    assert!(
        fixture
            .repository
            .inventory(USER, Some(GUILD))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_buy_item_exact_balance_succeeds() {
    let fixture = Fixture::new();
    fixture.register(4);
    fixture.create_tunnel(0);
    assert!(matches!(
        buy(&fixture, "lantern", 4, false),
        BuyItemOutcome::Purchased {
            balance_after: 0,
            ..
        }
    ));
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 0);
}

#[test]
fn test_buy_item_inventory_full_rejected() {
    let fixture = Fixture::new();
    fixture.register(500);
    fixture.create_tunnel(0);
    for _ in 0..INVENTORY_LIMIT {
        add_item(&fixture, "torch");
    }
    assert_eq!(
        buy(&fixture, "dynamite", 5, false),
        BuyItemOutcome::InventoryFull {
            limit: INVENTORY_LIMIT,
        }
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 500);
    assert_eq!(
        fixture
            .repository
            .inventory(USER, Some(GUILD))
            .unwrap()
            .len(),
        INVENTORY_LIMIT
    );
}

#[test]
fn test_two_purchases_read_each_state_once_and_keep_per_item_ledgers() {
    let fixture = Fixture::new();
    fixture.register(14);
    fixture.create_tunnel(0);
    let results = auto_buy(
        &fixture,
        &[
            AutoBuySelection {
                item_type: "hard_hat",
                price: 8,
            },
            AutoBuySelection {
                item_type: "torch",
                price: 6,
            },
        ],
        0,
        None,
    );
    assert_eq!(
        results
            .iter()
            .map(|result| result.status)
            .collect::<Vec<_>>(),
        vec![AutoBuyStatus::Purchased, AutoBuyStatus::Purchased]
    );
    assert!(results.iter().all(|result| result.item_id.is_some()));
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 0);
    let inventory = fixture.repository.inventory(USER, Some(GUILD)).unwrap();
    assert_eq!(inventory.len(), 2);
    assert!(inventory.iter().all(|item| item.queued));
    assert_eq!(
        fixture.dig_ledger(),
        vec![
            (
                -8,
                "dig".to_owned(),
                "dig_action".to_owned(),
                "dig cost or penalty".to_owned(),
            ),
            (
                -6,
                "dig".to_owned(),
                "dig_action".to_owned(),
                "dig cost or penalty".to_owned(),
            ),
        ]
    );
}

#[test]
fn test_insufficient_first_item_does_not_block_affordable_second() {
    let fixture = Fixture::new();
    fixture.register(7);
    fixture.create_tunnel(0);
    let results = auto_buy(
        &fixture,
        &[
            AutoBuySelection {
                item_type: "hard_hat",
                price: 8,
            },
            AutoBuySelection {
                item_type: "torch",
                price: 6,
            },
        ],
        0,
        None,
    );
    assert_eq!(
        results
            .iter()
            .map(|result| result.status)
            .collect::<Vec<_>>(),
        vec![
            AutoBuyStatus::SkippedInsufficientBalance,
            AutoBuyStatus::Purchased,
        ]
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 1);
    assert_eq!(
        fixture.repository.queued_items(USER, Some(GUILD)).unwrap()[0].item_type,
        "torch"
    );
}

#[test]
fn test_reserved_paid_dig_cost_is_not_spent_by_auto_buy() {
    let fixture = Fixture::new();
    fixture.register(10);
    fixture.create_tunnel(0);
    let results = auto_buy(
        &fixture,
        &[
            AutoBuySelection {
                item_type: "hard_hat",
                price: 8,
            },
            AutoBuySelection {
                item_type: "torch",
                price: 6,
            },
        ],
        3,
        None,
    );
    assert_eq!(
        results
            .iter()
            .map(|result| result.status)
            .collect::<Vec<_>>(),
        vec![
            AutoBuyStatus::SkippedInsufficientBalance,
            AutoBuyStatus::Purchased,
        ]
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 4);
}

#[test]
fn test_reserved_paid_dig_cost_survives_a_stale_balance_snapshot() {
    let fixture = Fixture::new();
    fixture.register(10);
    fixture.create_tunnel(0);
    let results = auto_buy(
        &fixture,
        &[AutoBuySelection {
            item_type: "hard_hat",
            price: 8,
        }],
        3,
        Some(14),
    );
    assert_eq!(results[0].status, AutoBuyStatus::SkippedInsufficientBalance);
    assert!(results[0].item_id.is_none());
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 10);
    assert!(
        fixture
            .repository
            .queued_items(USER, Some(GUILD))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_stale_balance_snapshot_cannot_drive_balance_negative() {
    let fixture = Fixture::new();
    fixture.register(6);
    fixture.create_tunnel(0);
    let results = auto_buy(
        &fixture,
        &[
            AutoBuySelection {
                item_type: "hard_hat",
                price: 8,
            },
            AutoBuySelection {
                item_type: "torch",
                price: 6,
            },
        ],
        0,
        Some(14),
    );
    assert_eq!(
        results
            .iter()
            .map(|result| result.status)
            .collect::<Vec<_>>(),
        vec![
            AutoBuyStatus::SkippedInsufficientBalance,
            AutoBuyStatus::Purchased,
        ]
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 0);
    let queued = fixture.repository.queued_items(USER, Some(GUILD)).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].item_type, "torch");
}

#[test]
fn test_first_purchase_consumes_last_inventory_slot() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    for _ in 0..(INVENTORY_LIMIT - 1) {
        add_item(&fixture, "lantern");
    }
    let results = auto_buy(
        &fixture,
        &[
            AutoBuySelection {
                item_type: "hard_hat",
                price: 8,
            },
            AutoBuySelection {
                item_type: "torch",
                price: 6,
            },
        ],
        0,
        None,
    );
    assert_eq!(
        results
            .iter()
            .map(|result| result.status)
            .collect::<Vec<_>>(),
        vec![
            AutoBuyStatus::Purchased,
            AutoBuyStatus::SkippedInventoryFull,
        ]
    );
    assert_eq!(
        fixture
            .repository
            .inventory(USER, Some(GUILD))
            .unwrap()
            .len(),
        INVENTORY_LIMIT
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 92);
}

#[test]
fn test_use_item_queues_owned_item() {
    let fixture = Fixture::new();
    fixture.register(3);
    fixture.create_tunnel(0);
    let item_id = add_item(&fixture, "dynamite");
    assert_eq!(
        fixture
            .repository
            .queue_item_type_atomic(USER, Some(GUILD), "dynamite")
            .unwrap(),
        QueueItemOutcome::Queued {
            item_id,
            item_type: "dynamite".to_owned(),
        }
    );
    assert_eq!(
        fixture.repository.queued_items(USER, Some(GUILD)).unwrap()[0].id,
        item_id
    );
}

#[test]
fn test_use_item_requires_tunnel() {
    let fixture = Fixture::new();
    fixture.register(3);
    assert_eq!(
        fixture
            .repository
            .queue_item_type_atomic(USER, Some(GUILD), "dynamite")
            .unwrap(),
        QueueItemOutcome::MissingTunnel
    );
}

#[test]
fn test_use_item_not_owned_rejected() {
    let fixture = Fixture::new();
    fixture.register(3);
    fixture.create_tunnel(0);
    add_item(&fixture, "torch");
    assert_eq!(
        fixture
            .repository
            .queue_item_type_atomic(USER, Some(GUILD), "dynamite")
            .unwrap(),
        QueueItemOutcome::NotOwned
    );
}

#[test]
fn test_use_item_already_queued_rejected() {
    let fixture = Fixture::new();
    fixture.register(3);
    fixture.create_tunnel(0);
    add_item(&fixture, "dynamite");
    fixture
        .repository
        .queue_item_type_atomic(USER, Some(GUILD), "dynamite")
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .queue_item_type_atomic(USER, Some(GUILD), "dynamite")
            .unwrap(),
        QueueItemOutcome::AlreadyQueued {
            item_type: "dynamite".to_owned(),
        }
    );
}

#[test]
fn test_use_item_queues_only_one_of_a_duplicate_pair() {
    let fixture = Fixture::new();
    fixture.register(3);
    fixture.create_tunnel(0);
    add_item(&fixture, "dynamite");
    add_item(&fixture, "dynamite");
    assert!(matches!(
        fixture
            .repository
            .queue_item_type_atomic(USER, Some(GUILD), "dynamite")
            .unwrap(),
        QueueItemOutcome::Queued { .. }
    ));
    assert_eq!(
        fixture
            .repository
            .queued_items(USER, Some(GUILD))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn test_only_one_boss_preparation_item_can_be_queued() {
    let fixture = Fixture::new();
    fixture.register(3);
    fixture.create_tunnel(0);
    add_item(&fixture, "tempered_whetstone");
    add_item(&fixture, "rescue_line");
    assert!(matches!(
        fixture
            .repository
            .queue_item_type_atomic(USER, Some(GUILD), "tempered_whetstone")
            .unwrap(),
        QueueItemOutcome::Queued { .. }
    ));
    assert_eq!(
        fixture
            .repository
            .queue_item_type_atomic(USER, Some(GUILD), "rescue_line")
            .unwrap(),
        QueueItemOutcome::BossPreparationAlreadyQueued
    );
}

#[test]
fn test_queue_item_by_id_sets_flag() {
    let fixture = Fixture::new();
    fixture.register(3);
    fixture.create_tunnel(0);
    let item_id = add_item(&fixture, "torch");
    assert!(matches!(
        fixture
            .repository
            .queue_item_id_atomic(USER, Some(GUILD), item_id)
            .unwrap(),
        QueueItemOutcome::Queued {
            item_id: queued_id,
            ..
        } if queued_id == item_id
    ));
    assert_eq!(
        fixture.repository.queued_items(USER, Some(GUILD)).unwrap()[0].id,
        item_id
    );
}

#[test]
fn test_get_inventory_empty_with_tunnel_no_items() {
    let fixture = Fixture::new();
    fixture.register(3);
    fixture.create_tunnel(0);
    assert_eq!(
        fixture
            .repository
            .inventory_for_tunnel(USER, Some(GUILD))
            .unwrap(),
        Some(Vec::new())
    );
}

#[test]
fn test_first_trap_of_day_is_free() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    assert_eq!(
        fixture
            .repository
            .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
            .unwrap(),
        SetTrapOutcome::Set {
            cost: 0,
            balance_after: 100,
        }
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 100);
    let tunnel = fixture
        .repository
        .tunnel(USER, Some(GUILD))
        .unwrap()
        .unwrap();
    assert!(tunnel.trap_active);
    assert_eq!(tunnel.trap_date.as_deref(), Some("2026-05-16"));
}

#[test]
fn test_second_trap_same_day_is_paid() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    fixture
        .repository
        .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
        .unwrap();
    fixture.clear_trap();
    assert_eq!(
        fixture
            .repository
            .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
            .unwrap(),
        SetTrapOutcome::Set {
            cost: 5,
            balance_after: 95,
        }
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 95);
}

#[test]
fn test_paid_trap_scales_with_depth() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(60);
    fixture
        .repository
        .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
        .unwrap();
    fixture.clear_trap();
    assert_eq!(
        fixture
            .repository
            .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
            .unwrap(),
        SetTrapOutcome::Set {
            cost: 7,
            balance_after: 93,
        }
    );
}

#[test]
fn test_new_day_resets_free_trap() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    fixture
        .repository
        .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
        .unwrap();
    fixture.clear_trap();
    assert_eq!(
        fixture
            .repository
            .set_trap_atomic(USER, Some(GUILD), "2026-05-17")
            .unwrap(),
        SetTrapOutcome::Set {
            cost: 0,
            balance_after: 100,
        }
    );
}

#[test]
fn test_set_trap_rejected_when_trap_already_active() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    fixture
        .repository
        .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
            .unwrap(),
        SetTrapOutcome::AlreadyActive
    );
}

#[test]
fn test_set_trap_requires_tunnel() {
    let fixture = Fixture::new();
    fixture.register(100);
    assert_eq!(
        fixture
            .repository
            .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
            .unwrap(),
        SetTrapOutcome::MissingTunnel
    );
}

#[test]
fn test_paid_trap_insufficient_balance_rejected() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    fixture
        .repository
        .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
        .unwrap();
    fixture.clear_trap();
    fixture.set_balance(4);
    assert_eq!(
        fixture
            .repository
            .set_trap_atomic(USER, Some(GUILD), "2026-05-16")
            .unwrap(),
        SetTrapOutcome::InsufficientBalance {
            balance: 4,
            cost: 5,
        }
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 4);
    assert!(
        !fixture
            .repository
            .tunnel(USER, Some(GUILD))
            .unwrap()
            .unwrap()
            .trap_active
    );
}

#[test]
fn test_buy_insurance_charges_and_sets_window() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    assert_eq!(
        fixture
            .repository
            .buy_insurance_atomic(USER, Some(GUILD), NOW)
            .unwrap(),
        BuyInsuranceOutcome::Purchased {
            cost: 5,
            expires_at: NOW + 86_400,
            balance_after: 95,
        }
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 95);
    assert_eq!(
        fixture
            .repository
            .tunnel(USER, Some(GUILD))
            .unwrap()
            .unwrap()
            .insured_until,
        Some(NOW + 86_400)
    );
}

#[test]
fn test_buy_insurance_cost_scales_with_depth() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(75);
    assert!(matches!(
        fixture
            .repository
            .buy_insurance_atomic(USER, Some(GUILD), NOW)
            .unwrap(),
        BuyInsuranceOutcome::Purchased {
            cost: 8,
            balance_after: 92,
            ..
        }
    ));
}

#[test]
fn test_buy_insurance_requires_tunnel() {
    let fixture = Fixture::new();
    fixture.register(100);
    assert_eq!(
        fixture
            .repository
            .buy_insurance_atomic(USER, Some(GUILD), NOW)
            .unwrap(),
        BuyInsuranceOutcome::MissingTunnel
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 100);
}

#[test]
fn test_buy_insurance_insufficient_balance_rejected() {
    let fixture = Fixture::new();
    fixture.register(4);
    fixture.create_tunnel(0);
    assert_eq!(
        fixture
            .repository
            .buy_insurance_atomic(USER, Some(GUILD), NOW)
            .unwrap(),
        BuyInsuranceOutcome::InsufficientBalance {
            balance: 4,
            cost: 5,
        }
    );
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 4);
    assert_eq!(
        fixture
            .repository
            .tunnel(USER, Some(GUILD))
            .unwrap()
            .unwrap()
            .insured_until,
        None
    );
}

#[test]
fn test_queue_item_rejects_duplicate_type() {
    let fixture = Fixture::new();
    fixture.register(3);
    let first_id = add_item(&fixture, "torch");
    let second_id = add_item(&fixture, "torch");
    assert!(matches!(
        fixture
            .repository
            .queue_item_id_atomic(USER, Some(GUILD), first_id)
            .unwrap(),
        QueueItemOutcome::Queued { .. }
    ));
    assert_eq!(
        fixture
            .repository
            .queue_item_id_atomic(USER, Some(GUILD), second_id)
            .unwrap(),
        QueueItemOutcome::AlreadyQueued {
            item_type: "torch".to_owned(),
        }
    );
    assert_eq!(
        fixture
            .repository
            .queued_items(USER, Some(GUILD))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn test_repository_normalizes_none_guild_and_preserves_signed_ids() {
    let fixture = Fixture::new();
    fixture
        .connection()
        .execute(
            "INSERT INTO players (discord_id, guild_id, jopacoin_balance)
             VALUES (-7, 0, 5)",
            [],
        )
        .unwrap();
    fixture
        .connection()
        .execute(
            "INSERT INTO tunnels
                 (discord_id, guild_id, depth, trap_active, trap_free_today)
             VALUES (-7, 0, 0, 0, 1)",
            [],
        )
        .unwrap();
    let outcome = fixture
        .repository
        .buy_item_atomic(BuyItemRequest {
            discord_id: -7,
            guild_id: None,
            item_type: "dynamite",
            price: 5,
            inventory_limit: INVENTORY_LIMIT,
            auto_queue_if_available: false,
            created_at: NOW,
        })
        .unwrap();
    assert!(matches!(
        outcome,
        BuyItemOutcome::Purchased {
            balance_after: 0,
            ..
        }
    ));
    let inventory = fixture.repository.inventory(-7, None).unwrap();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].discord_id, -7);
    assert_eq!(inventory[0].guild_id, 0);
}

#[test]
fn test_auto_buy_queues_owned_reserve_without_charging_or_purchase_id() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    let reserve_id = add_item(&fixture, "hard_hat");
    let results = auto_buy(
        &fixture,
        &[AutoBuySelection {
            item_type: "hard_hat",
            price: 8,
        }],
        0,
        None,
    );
    assert_eq!(results[0].status, AutoBuyStatus::QueuedFromInventory);
    assert_eq!(results[0].cost, 0);
    assert!(results[0].item_id.is_none());
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 100);
    let queued = fixture.repository.queued_items(USER, Some(GUILD)).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, reserve_id);
}

#[test]
fn test_concurrent_buys_cannot_overdraft_or_exceed_capacity() {
    let overdraft_fixture = Fixture::durable();
    overdraft_fixture.register(8);
    overdraft_fixture.create_tunnel(0);
    let repository = Arc::new(overdraft_fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(3));
    let handles = ["hard_hat", "sonar_pulse"].map(|item_type| {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            repository
                .buy_item_atomic(BuyItemRequest {
                    discord_id: USER,
                    guild_id: Some(GUILD),
                    item_type,
                    price: 8,
                    inventory_limit: INVENTORY_LIMIT,
                    auto_queue_if_available: false,
                    created_at: NOW,
                })
                .unwrap()
        })
    });
    barrier.wait();
    let outcomes = handles.map(|handle| handle.join().unwrap());
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BuyItemOutcome::Purchased { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| { matches!(outcome, BuyItemOutcome::InsufficientBalance { .. }) })
            .count(),
        1
    );
    assert_eq!(repository.balance(USER, Some(GUILD)).unwrap(), 0);
    assert_eq!(repository.inventory(USER, Some(GUILD)).unwrap().len(), 1);

    let capacity_fixture = Fixture::durable();
    capacity_fixture.register(100);
    capacity_fixture.create_tunnel(0);
    for _ in 0..(INVENTORY_LIMIT - 1) {
        add_item(&capacity_fixture, "lantern");
    }
    let repository = Arc::new(capacity_fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(3));
    let handles = [("hard_hat", 8), ("torch", 6)].map(|(item_type, price)| {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            repository
                .buy_item_atomic(BuyItemRequest {
                    discord_id: USER,
                    guild_id: Some(GUILD),
                    item_type,
                    price,
                    inventory_limit: INVENTORY_LIMIT,
                    auto_queue_if_available: true,
                    created_at: NOW,
                })
                .unwrap()
        })
    });
    barrier.wait();
    let outcomes = handles.map(|handle| handle.join().unwrap());
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BuyItemOutcome::Purchased { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, BuyItemOutcome::InventoryFull { .. }))
            .count(),
        1
    );
    assert_eq!(
        repository.inventory(USER, Some(GUILD)).unwrap().len(),
        INVENTORY_LIMIT
    );
    assert!(matches!(
        repository.balance(USER, Some(GUILD)).unwrap(),
        92 | 94
    ));
}

#[test]
fn test_failed_inventory_insert_rolls_back_debit_ledger_and_context() {
    let fixture = Fixture::new();
    fixture.register(100);
    fixture.create_tunnel(0);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER reject_forced_inventory_insert
             BEFORE INSERT ON dig_inventory
             WHEN NEW.item_type = 'forced_failure'
             BEGIN
                 SELECT RAISE(ABORT, 'forced inventory failure');
             END;",
        )
        .unwrap();
    let result = fixture.repository.buy_item_atomic(BuyItemRequest {
        discord_id: USER,
        guild_id: Some(GUILD),
        item_type: "forced_failure",
        price: 5,
        inventory_limit: INVENTORY_LIMIT,
        auto_queue_if_available: false,
        created_at: NOW,
    });
    assert!(result.is_err());
    assert_eq!(fixture.repository.balance(USER, Some(GUILD)).unwrap(), 100);
    assert!(
        fixture
            .repository
            .inventory(USER, Some(GUILD))
            .unwrap()
            .is_empty()
    );
    assert!(fixture.dig_ledger().is_empty());
    let context_rows: i64 = fixture
        .connection()
        .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(context_rows, 0);
}
