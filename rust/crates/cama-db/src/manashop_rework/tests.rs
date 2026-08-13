use std::sync::{Arc, Barrier};
use std::thread;
use std::{path::Path, process::Command};

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const USER: i64 = 111;
const ALLY: i64 = 222;
const TARGET: i64 = 333;
const GUILD: i64 = 9_001;
const OTHER_GUILD: i64 = 9_002;
const NOW: i64 = 2_000_000_000;
const TODAY: &str = "2026-05-09";

struct Fixture {
    file: NamedTempFile,
    repository: ManashopRepository,
}

impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().expect("temporary SQLite file");
        let connection = Connection::open(file.path()).expect("open fixture");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = OFF;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     discord_username TEXT,
                     jopacoin_balance INTEGER DEFAULT 3,
                     lowest_balance_ever INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
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
                 );
                 CREATE TABLE manashop_daily_uses (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     item_id TEXT NOT NULL,
                     used_date TEXT NOT NULL,
                     purchase_id TEXT,
                     PRIMARY KEY (discord_id, guild_id, item_id, used_date)
                 );
                 CREATE UNIQUE INDEX idx_manashop_daily_purchase
                     ON manashop_daily_uses(purchase_id) WHERE purchase_id IS NOT NULL;
                 CREATE TABLE manashop_purchases (
                     purchase_id TEXT PRIMARY KEY,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     item_id TEXT NOT NULL,
                     used_date TEXT NOT NULL,
                     cost INTEGER NOT NULL,
                     tap_mana INTEGER NOT NULL DEFAULT 0,
                     status TEXT NOT NULL DEFAULT 'pending',
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE manashop_buffs (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     buff_type TEXT NOT NULL,
                     target_id INTEGER,
                     granted_at INTEGER NOT NULL,
                     expires_at INTEGER NOT NULL,
                     triggered INTEGER NOT NULL DEFAULT 0,
                     data TEXT
                 );
                 CREATE TABLE bankruptcy_state (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     last_bankruptcy_at INTEGER,
                     penalty_games_remaining INTEGER NOT NULL DEFAULT 0,
                     bankruptcy_count INTEGER NOT NULL DEFAULT 0,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
                 );
                 CREATE TABLE slow_drip_claims (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL DEFAULT 0,
                     claim_date TEXT NOT NULL,
                     claimed_today INTEGER NOT NULL DEFAULT 0,
                     last_claim_at INTEGER NOT NULL,
                     PRIMARY KEY (discord_id, guild_id, claim_date)
                 );
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY CHECK(id = 1),
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );",
            )
            .expect("create Python-shaped temporary schema");
        drop(connection);
        Self {
            repository: ManashopRepository::new(file.path()),
            file,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.file.path()).expect("open fixture connection")
    }

    fn register(&self, discord_id: i64, guild_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players
                     (discord_id, guild_id, discord_username, jopacoin_balance)
                 VALUES (?1, ?2, 'player', ?3)",
                params![discord_id, guild_id, balance],
            )
            .expect("register fixture player");
    }
}

fn purchase<'a>(
    purchase_id: &'a str,
    item_id: &'a str,
    cost: i64,
    tap: bool,
) -> PurchaseRequest<'a> {
    PurchaseRequest {
        purchase_id,
        discord_id: USER,
        guild_id: Some(GUILD),
        item_id,
        used_date: TODAY,
        cost,
        tap_mana: tap,
        now: NOW,
    }
}

#[test]
fn test_mark_mana_consumed_atomic_first_call_succeeds() {
    let fixture = Fixture::new();
    fixture
        .repository
        .claim_mana_atomic(USER, Some(GUILD), "Mountain", TODAY)
        .expect("claim mana");
    assert!(
        !fixture
            .repository
            .is_mana_consumed(USER, Some(GUILD))
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .mark_mana_consumed_atomic(USER, Some(GUILD))
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .is_mana_consumed(USER, Some(GUILD))
            .unwrap()
    );
}

#[test]
fn test_mark_mana_consumed_atomic_idempotent_within_day() {
    let fixture = Fixture::new();
    fixture
        .repository
        .claim_mana_atomic(USER, Some(GUILD), "Mountain", TODAY)
        .unwrap();
    assert!(
        fixture
            .repository
            .mark_mana_consumed_atomic(USER, Some(GUILD))
            .unwrap()
    );
    assert!(
        !fixture
            .repository
            .mark_mana_consumed_atomic(USER, Some(GUILD))
            .unwrap()
    );
}

#[test]
fn test_claim_mana_resets_consumed_flag_next_day() {
    let fixture = Fixture::new();
    fixture
        .repository
        .claim_mana_atomic(USER, Some(GUILD), "Mountain", TODAY)
        .unwrap();
    fixture
        .repository
        .mark_mana_consumed_atomic(USER, Some(GUILD))
        .unwrap();
    assert!(
        fixture
            .repository
            .claim_mana_atomic(USER, Some(GUILD), "Forest", "2026-05-10")
            .unwrap()
    );
    assert!(
        !fixture
            .repository
            .is_mana_consumed(USER, Some(GUILD))
            .unwrap()
    );
}

#[test]
fn test_protection_plains_claim_resets_guardian_capacity_only_for_plains() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .repository
            .claim_mana_atomic(USER, Some(GUILD), "Plains", TODAY)
            .unwrap()
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT white_shield_remaining FROM player_mana
                 WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        25
    );
    fixture
        .connection()
        .execute(
            "UPDATE player_mana SET white_shield_remaining=7
             WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
        )
        .unwrap();
    assert!(
        !fixture
            .repository
            .claim_mana_atomic(USER, Some(GUILD), "Plains", TODAY)
            .unwrap()
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT white_shield_remaining FROM player_mana
                 WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        7
    );
    assert!(
        fixture
            .repository
            .claim_mana_atomic(USER, Some(GUILD), "Forest", "2026-05-10")
            .unwrap()
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT white_shield_remaining FROM player_mana
                 WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert!(
        fixture
            .repository
            .claim_mana_atomic(USER, Some(GUILD), "Plains", "2026-05-11")
            .unwrap()
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT white_shield_remaining FROM player_mana
                 WHERE discord_id=?1 AND guild_id=?2",
                params![USER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        25
    );
}

#[test]
fn test_was_item_used_today_starts_false_then_true() {
    let fixture = Fixture::new();
    assert!(
        !fixture
            .repository
            .was_item_used_today(USER, Some(GUILD), "aegis", TODAY)
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .mark_item_used_atomic(USER, Some(GUILD), "aegis", TODAY)
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .was_item_used_today(USER, Some(GUILD), "aegis", TODAY)
            .unwrap()
    );
}

#[test]
fn test_mark_item_used_atomic_blocks_double_use_same_day() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .repository
            .mark_item_used_atomic(USER, Some(GUILD), "regrowth", TODAY)
            .unwrap()
    );
    assert!(
        !fixture
            .repository
            .mark_item_used_atomic(USER, Some(GUILD), "regrowth", TODAY)
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .mark_item_used_atomic(USER, Some(GUILD), "blood_pact", TODAY)
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .mark_item_used_atomic(USER, Some(GUILD), "regrowth", "2026-05-10")
            .unwrap()
    );
}

#[test]
fn test_manashop_purchase_shortfall_does_not_claim_or_tap() {
    let fixture = Fixture::new();
    fixture.register(USER, GUILD, 40);
    fixture
        .repository
        .claim_mana_atomic(USER, Some(GUILD), "Mountain", TODAY)
        .unwrap();
    let result = fixture
        .repository
        .try_purchase_item_atomic(purchase("short", "wildfire", 150, true))
        .unwrap();
    assert_eq!(
        result,
        PurchaseResult::rejected(PurchaseFailureReason::InsufficientBalance, 40)
    );
    assert!(
        !fixture
            .repository
            .was_item_used_today(USER, Some(GUILD), "wildfire", TODAY)
            .unwrap()
    );
    assert!(
        !fixture
            .repository
            .is_mana_consumed(USER, Some(GUILD))
            .unwrap()
    );
}

#[test]
fn test_manashop_purchase_rolls_back_claim_and_tap_on_debit_error() {
    let fixture = Fixture::new();
    fixture.register(USER, GUILD, 200);
    fixture
        .repository
        .claim_mana_atomic(USER, Some(GUILD), "Mountain", TODAY)
        .unwrap();
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_manashop_debit
         BEFORE UPDATE OF jopacoin_balance ON players
         BEGIN SELECT RAISE(ABORT, 'forced debit failure'); END;",
        )
        .unwrap();
    let error = fixture
        .repository
        .try_purchase_item_atomic(purchase("rollback", "wildfire", 150, true))
        .expect_err("trigger aborts debit");
    assert!(error.to_string().contains("forced debit failure"));
    assert_eq!(
        fixture.repository.balance(USER, Some(GUILD)).unwrap(),
        Some(200)
    );
    assert!(
        !fixture
            .repository
            .was_item_used_today(USER, Some(GUILD), "wildfire", TODAY)
            .unwrap()
    );
    assert!(
        !fixture
            .repository
            .is_mana_consumed(USER, Some(GUILD))
            .unwrap()
    );
}

#[test]
fn test_concurrent_manashop_purchases_cannot_overdraw_or_leak_claims() {
    let fixture = Fixture::new();
    fixture.register(USER, GUILD, 100);
    fixture
        .repository
        .claim_mana_atomic(USER, Some(GUILD), "Mountain", TODAY)
        .unwrap();
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["item_a", "item_b"].map(|item| {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            repository
                .try_purchase_item_atomic(purchase(item, item, 75, false))
                .unwrap()
        })
    });
    let results = handles.map(|handle| handle.join().expect("purchase thread"));
    assert_eq!(results.iter().filter(|result| result.success).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.reason)
            .collect::<Vec<_>>(),
        vec![PurchaseFailureReason::InsufficientBalance]
    );
    assert_eq!(repository.balance(USER, Some(GUILD)).unwrap(), Some(25));
    let claimed = ["item_a", "item_b"]
        .into_iter()
        .filter(|item| {
            repository
                .was_item_used_today(USER, Some(GUILD), item, TODAY)
                .unwrap()
        })
        .count();
    assert_eq!(claimed, 1);
}

#[test]
fn test_manashop_refund_is_atomic_idempotent_and_restores_tap() {
    let fixture = Fixture::new();
    fixture.register(USER, GUILD, 200);
    fixture
        .repository
        .claim_mana_atomic(USER, Some(GUILD), "Mountain", TODAY)
        .unwrap();
    let bought = fixture
        .repository
        .try_purchase_item_atomic(purchase("refund", "wildfire", 150, true))
        .unwrap();
    assert!(bought.success);
    assert_eq!(
        fixture
            .repository
            .get_item_purchase("refund")
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
    assert!(
        fixture
            .repository
            .refund_item_purchase_atomic("refund", NOW + 1)
            .unwrap()
    );
    assert!(
        !fixture
            .repository
            .refund_item_purchase_atomic("refund", NOW + 2)
            .unwrap()
    );
    assert_eq!(
        fixture
            .repository
            .get_item_purchase("refund")
            .unwrap()
            .unwrap()
            .status,
        "refunded"
    );
    assert_eq!(
        fixture.repository.balance(USER, Some(GUILD)).unwrap(),
        Some(200)
    );
    assert!(
        !fixture
            .repository
            .was_item_used_today(USER, Some(GUILD), "wildfire", TODAY)
            .unwrap()
    );
    assert!(
        !fixture
            .repository
            .is_mana_consumed(USER, Some(GUILD))
            .unwrap()
    );
}

#[test]
fn test_manashop_stale_refund_cannot_reverse_repurchase() {
    let fixture = Fixture::new();
    fixture.register(USER, GUILD, 400);
    fixture
        .repository
        .claim_mana_atomic(USER, Some(GUILD), "Mountain", TODAY)
        .unwrap();
    fixture
        .repository
        .try_purchase_item_atomic(purchase("first", "wildfire", 150, true))
        .unwrap();
    assert!(
        fixture
            .repository
            .refund_item_purchase_atomic("first", NOW + 1)
            .unwrap()
    );
    let mut second = purchase("second", "wildfire", 150, true);
    second.now = NOW + 2;
    assert!(
        fixture
            .repository
            .try_purchase_item_atomic(second)
            .unwrap()
            .success
    );
    assert!(
        !fixture
            .repository
            .refund_item_purchase_atomic("first", NOW + 3)
            .unwrap()
    );
    assert_eq!(
        fixture.repository.balance(USER, Some(GUILD)).unwrap(),
        Some(250)
    );
    assert!(
        fixture
            .repository
            .was_item_used_today(USER, Some(GUILD), "wildfire", TODAY)
            .unwrap()
    );
    assert!(
        fixture
            .repository
            .is_mana_consumed(USER, Some(GUILD))
            .unwrap()
    );
}

#[test]
fn test_manashop_retry_recovers_purchase_abandoned_before_effects() {
    let fixture = Fixture::new();
    fixture.register(USER, GUILD, 400);
    fixture
        .repository
        .claim_mana_atomic(USER, Some(GUILD), "Mountain", TODAY)
        .unwrap();
    fixture
        .repository
        .try_purchase_item_atomic(purchase("abandoned", "wildfire", 150, true))
        .unwrap();
    fixture
        .connection()
        .execute(
            "UPDATE manashop_purchases SET updated_at = 0 WHERE purchase_id = 'abandoned'",
            [],
        )
        .unwrap();
    let mut retry = purchase("retried", "wildfire", 150, true);
    retry.now = NOW + 100;
    let result = fixture.repository.try_purchase_item_atomic(retry).unwrap();
    assert!(result.success);
    assert_eq!(
        fixture
            .repository
            .get_item_purchase("abandoned")
            .unwrap()
            .unwrap()
            .status,
        "refunded"
    );
    assert_eq!(
        fixture.repository.balance(USER, Some(GUILD)).unwrap(),
        Some(250)
    );
    assert!(
        fixture
            .repository
            .is_mana_consumed(USER, Some(GUILD))
            .unwrap()
    );
}

#[test]
fn test_active_for_many_batches_owners_and_preserves_missing() {
    let fixture = Fixture::new();
    let grant = |owner, guild, kind, expires| {
        fixture
            .repository
            .grant_buff(GrantBuffRequest {
                discord_id: owner,
                guild_id: Some(guild),
                buff_type: kind,
                target_id: None,
                granted_at: NOW,
                expires_at: expires,
                data: None,
            })
            .unwrap()
    };
    let first_user = grant(USER, GUILD, "counterspell", NOW + 3_600);
    let second_user = grant(USER, GUILD, "counterspell", NOW + 3_600);
    let ally = grant(ALLY, GUILD, "counterspell", NOW + 3_600);
    grant(ALLY, OTHER_GUILD, "counterspell", NOW + 3_600);
    grant(TARGET, GUILD, "overgrowth", NOW + 3_600);
    grant(TARGET, GUILD, "counterspell", NOW - 1);
    let active = fixture
        .repository
        .active_for_many(
            &[ALLY, USER, ALLY, TARGET],
            Some(GUILD),
            "counterspell",
            NOW,
        )
        .unwrap();
    assert_eq!(
        active.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        vec![ALLY, USER, TARGET]
    );
    assert_eq!(
        active[0].1.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![ally]
    );
    assert_eq!(
        active[1]
            .1
            .iter()
            .map(|row| row.id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([first_user, second_user])
    );
    assert!(active[2].1.is_empty());
}

#[test]
fn test_overgrowth_migration_backfills_missing_charges() {
    let fixture = Fixture::new();
    fixture
        .repository
        .grant_buff(GrantBuffRequest {
            discord_id: USER,
            guild_id: Some(GUILD),
            buff_type: "overgrowth",
            target_id: None,
            granted_at: NOW,
            expires_at: NOW + 3_600,
            data: Some(&BuffData::default()),
        })
        .unwrap();
    assert_eq!(
        fixture.repository.backfill_overgrowth_charges(NOW).unwrap(),
        1
    );
    let active = fixture
        .repository
        .active_for(USER, Some(GUILD), "overgrowth", NOW)
        .unwrap();
    assert_eq!(active[0].data.charges_remaining, Some(10));
}

#[test]
fn test_slow_drip_get_today_defaults() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .repository
            .slow_drip_today(USER, Some(GUILD), TODAY)
            .unwrap(),
        SlowDripState::default()
    );
}

#[test]
fn test_slow_drip_add_claim_accumulates() {
    let fixture = Fixture::new();
    fixture
        .repository
        .add_slow_drip_claim(USER, Some(GUILD), TODAY, 30, NOW)
        .unwrap();
    fixture
        .repository
        .add_slow_drip_claim(USER, Some(GUILD), TODAY, 50, NOW + 1)
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .slow_drip_today(USER, Some(GUILD), TODAY)
            .unwrap(),
        SlowDripState {
            claimed_today: 80,
            last_claim_at: NOW + 1
        }
    );
}

#[test]
fn test_slow_drip_per_day_isolation() {
    let fixture = Fixture::new();
    fixture
        .repository
        .add_slow_drip_claim(USER, Some(GUILD), TODAY, 100, NOW)
        .unwrap();
    assert_eq!(
        fixture
            .repository
            .slow_drip_today(USER, Some(GUILD), TODAY)
            .unwrap()
            .claimed_today,
        100
    );
    assert_eq!(
        fixture
            .repository
            .slow_drip_today(USER, Some(GUILD), "2026-05-10")
            .unwrap()
            .claimed_today,
        0
    );
}

#[test]
fn test_slow_drip_stamp_seen_preserves_gross_cap_and_refreshes_anchor() {
    let fixture = Fixture::new();
    fixture
        .repository
        .stamp_slow_drip_seen(USER, Some(GUILD), TODAY, NOW)
        .expect("initial zero-amount stamp");
    assert_eq!(
        fixture
            .repository
            .slow_drip_today(USER, Some(GUILD), TODAY)
            .expect("stamped state"),
        SlowDripState {
            claimed_today: 0,
            last_claim_at: NOW,
        }
    );
    fixture
        .repository
        .add_slow_drip_claim(USER, Some(GUILD), TODAY, 30, NOW + 1)
        .expect("record gross claim");
    fixture
        .repository
        .stamp_slow_drip_seen(USER, Some(GUILD), TODAY, NOW + 2)
        .expect("refresh capped anchor");
    assert_eq!(
        fixture
            .repository
            .slow_drip_today(USER, Some(GUILD), TODAY)
            .expect("refreshed state"),
        SlowDripState {
            claimed_today: 30,
            last_claim_at: NOW + 2,
        }
    );
}

#[test]
#[ignore = "cross-language smoke invokes the repository Python environment"]
fn python_migrated_database_manashop_interop_smoke() {
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
            "import sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\np=sys.argv[1]\nSchemaManager(p).initialize()\nc=sqlite3.connect(p)\nc.execute(\"INSERT INTO players (discord_id,guild_id,discord_username,jopacoin_balance) VALUES (?,?,?,?)\",(-70001,0,'interop',400))\nc.execute(\"INSERT INTO player_mana (discord_id,guild_id,current_land,assigned_date,consumed_today) VALUES (?,?,?,?,0)\",(-70001,0,'Mountain','2026-05-09'))\nc.commit(); c.close()",
            file.path().to_str().expect("UTF-8 path"),
        ])
        .status()
        .expect("initialize Python-migrated database");
    assert!(initialize.success());

    let repository = ManashopRepository::new(file.path());
    let purchase = repository
        .try_purchase_item_atomic(PurchaseRequest {
            purchase_id: "rust-interop-purchase",
            discord_id: -70_001,
            guild_id: Some(0),
            item_id: "wildfire",
            used_date: "2026-05-09",
            cost: 150,
            tap_mana: true,
            now: NOW,
        })
        .expect("Rust purchase against Python schema");
    assert!(purchase.success);
    assert!(
        repository
            .mark_item_purchase_applying_atomic("rust-interop-purchase", NOW + 1)
            .unwrap()
    );
    assert!(
        repository
            .complete_item_purchase_atomic("rust-interop-purchase", NOW + 2)
            .unwrap()
    );
    repository
        .grant_buff(GrantBuffRequest {
            discord_id: -70_001,
            guild_id: Some(0),
            buff_type: "blood_pact",
            target_id: Some(-70_002),
            granted_at: NOW,
            expires_at: NOW + 3_600,
            data: Some(&BuffData {
                skimmed_total: Some(0),
                cap: Some(150),
                skim_rate: Some(0.25),
                ..BuffData::default()
            }),
        })
        .expect("Rust buff against Python schema");
    repository
        .add_slow_drip_claim(-70_001, Some(0), "2026-05-09", 30, NOW)
        .expect("Rust slow-drip write against Python schema");

    let verify = Command::new(python)
        .args([
            "-c",
            "import json,sqlite3,sys\nc=sqlite3.connect(sys.argv[1])\nb=c.execute(\"SELECT jopacoin_balance FROM players WHERE discord_id=-70001 AND guild_id=0\").fetchone()[0]\nm=c.execute(\"SELECT consumed_today FROM player_mana WHERE discord_id=-70001 AND guild_id=0\").fetchone()[0]\np=c.execute(\"SELECT status FROM manashop_purchases WHERE purchase_id='rust-interop-purchase'\").fetchone()[0]\nu=c.execute(\"SELECT COUNT(*) FROM manashop_daily_uses WHERE purchase_id='rust-interop-purchase'\").fetchone()[0]\ns=c.execute(\"SELECT claimed_today FROM slow_drip_claims WHERE discord_id=-70001 AND guild_id=0\").fetchone()[0]\nd=json.loads(c.execute(\"SELECT data FROM manashop_buffs WHERE discord_id=-70001 AND buff_type='blood_pact'\").fetchone()[0])\nassert (b,m,p,u,s,d['cap'],d['skim_rate']) == (250,1,'completed',1,30,150,0.25)",
            file.path().to_str().expect("UTF-8 path"),
        ])
        .status()
        .expect("verify Rust writes from Python");
    assert!(verify.success());
}
