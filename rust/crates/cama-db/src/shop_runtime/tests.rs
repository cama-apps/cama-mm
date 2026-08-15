use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;

use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;

const GUILD: i64 = 9_001;
const PROTECTION_OTHER_GUILD: i64 = 9_002;
const BUYER: i64 = 101;
const VICTIM: i64 = 202;
const NOW: i64 = 2_000_000_000;

struct Fixture {
    database: NamedTempFile,
    repository: ShopRuntimeRepository,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary Shop database");
        let connection = Connection::open(database.path()).expect("open Shop fixture");
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE players (
                   discord_id INTEGER NOT NULL,guild_id INTEGER NOT NULL,
                   discord_username TEXT,wins INTEGER DEFAULT 0,losses INTEGER DEFAULT 0,
                   glicko_rating REAL,glicko_rd REAL,glicko_volatility REAL,
                   os_mu REAL,os_sigma REAL,jopacoin_balance INTEGER DEFAULT 0,
                   lowest_balance_ever INTEGER,last_pingedash INTEGER,last_pingedkevin INTEGER,
                   last_double_or_nothing INTEGER,updated_at TEXT,
                   PRIMARY KEY(discord_id,guild_id));
                 CREATE TABLE economy_ledger_context (
                   id INTEGER PRIMARY KEY,source TEXT,actor_id INTEGER,related_type TEXT,
                   related_id TEXT,reason TEXT,metadata TEXT);
                 CREATE TABLE economy_ledger_entries (
                   ledger_id INTEGER PRIMARY KEY AUTOINCREMENT,guild_id INTEGER,
                   account_id INTEGER,delta INTEGER,balance_before INTEGER,balance_after INTEGER,
                   source TEXT,related_type TEXT,related_id TEXT,reason TEXT,metadata TEXT);
                 CREATE TRIGGER ledger_player_update AFTER UPDATE OF jopacoin_balance ON players
                 WHEN OLD.jopacoin_balance != NEW.jopacoin_balance BEGIN
                   INSERT INTO economy_ledger_entries(
                     guild_id,account_id,delta,balance_before,balance_after,source,
                     related_type,related_id,reason,metadata)
                   VALUES(NEW.guild_id,NEW.discord_id,
                     NEW.jopacoin_balance-OLD.jopacoin_balance,
                     OLD.jopacoin_balance,NEW.jopacoin_balance,
                     COALESCE((SELECT source FROM economy_ledger_context WHERE id=1),'balance_update'),
                     (SELECT related_type FROM economy_ledger_context WHERE id=1),
                     (SELECT related_id FROM economy_ledger_context WHERE id=1),
                     (SELECT reason FROM economy_ledger_context WHERE id=1),
                     (SELECT metadata FROM economy_ledger_context WHERE id=1));
                 END;
                 CREATE TABLE double_or_nothing_spins (
                   spin_id INTEGER PRIMARY KEY AUTOINCREMENT,discord_id INTEGER,guild_id INTEGER,
                   cost INTEGER,balance_before INTEGER,balance_at_risk INTEGER,
                   balance_after INTEGER,won INTEGER,spin_time INTEGER);
                 CREATE TABLE hostile_loss_events (
                   event_id INTEGER PRIMARY KEY AUTOINCREMENT,guild_id INTEGER,victim_id INTEGER,
                   actor_id INTEGER,event_key TEXT,kind TEXT,destination TEXT,recipient_id INTEGER,
                   requested INTEGER,attempted INTEGER,absorbed INTEGER,applied INTEGER,
                   victim_balance_before INTEGER,victim_balance_after INTEGER,
                   destination_balance_before INTEGER,destination_balance_after INTEGER,
                   shieldable INTEGER,retro_covered INTEGER,protection_details TEXT,metadata TEXT,
                   occurred_at INTEGER,created_at INTEGER,
                   UNIQUE(guild_id,victim_id,event_key));
                 CREATE TABLE mana_protection_events (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,hostile_loss_event_id INTEGER,
                   guild_id INTEGER,victim_id INTEGER,protection_type TEXT,pool_key TEXT,
                   buff_id INTEGER,amount INTEGER,rate REAL,capacity_before INTEGER,
                   capacity_after INTEGER,retroactive INTEGER,created_at INTEGER,details TEXT);
                 CREATE TABLE manashop_buffs (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,discord_id INTEGER,guild_id INTEGER,
                   buff_type TEXT,target_id INTEGER,granted_at INTEGER,expires_at INTEGER,
                   triggered INTEGER DEFAULT 0,data TEXT);
                 CREATE TABLE player_mana (
                   discord_id INTEGER,guild_id INTEGER,current_land TEXT,assigned_date TEXT,
                   consumed_today INTEGER DEFAULT 0,white_shield_remaining INTEGER DEFAULT 0,
                   updated_at TEXT,PRIMARY KEY(discord_id,guild_id));
                 CREATE TABLE nonprofit_fund (
                   guild_id INTEGER PRIMARY KEY,total_collected INTEGER DEFAULT 0,updated_at TEXT);
                 CREATE TABLE matches (
                   match_id INTEGER PRIMARY KEY,guild_id INTEGER NOT NULL,winning_team INTEGER);
                 CREATE TABLE bets (
                   bet_id INTEGER PRIMARY KEY,guild_id INTEGER NOT NULL,match_id INTEGER,
                   discord_id INTEGER NOT NULL,team_bet_on TEXT NOT NULL,amount INTEGER NOT NULL,
                   bet_time INTEGER NOT NULL,leverage INTEGER DEFAULT 1);
                 CREATE TABLE wheel_spins (
                   spin_id INTEGER PRIMARY KEY,guild_id INTEGER NOT NULL,discord_id INTEGER NOT NULL,
                   result INTEGER NOT NULL,spin_time INTEGER NOT NULL);
                 CREATE TABLE tunnels (
                   discord_id INTEGER NOT NULL,guild_id INTEGER NOT NULL,depth INTEGER DEFAULT 0,
                   luminosity INTEGER DEFAULT 100,prestige_level INTEGER DEFAULT 0,
                   last_dig_at INTEGER,temp_buffs TEXT,PRIMARY KEY(discord_id,guild_id));
                 CREATE TABLE dig_artifacts (
                   id INTEGER PRIMARY KEY,discord_id INTEGER NOT NULL,guild_id INTEGER NOT NULL,
                   artifact_id TEXT NOT NULL,is_relic INTEGER DEFAULT 0,equipped INTEGER DEFAULT 0);",
            )
            .expect("create Python-shaped Shop schema");
        drop(connection);
        Self {
            repository: ShopRuntimeRepository::new(database.path()),
            database,
        }
    }

    fn connection(&self) -> Connection {
        Connection::open(self.database.path()).expect("open Shop fixture connection")
    }

    fn player(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES(?1,?2,'shop-test',?3)",
                params![discord_id, GUILD, balance],
            )
            .expect("seed Shop player");
    }
}

#[test]
fn paid_ping_kind_preserves_python_sources() {
    assert_eq!(PaidPingKind::Dash.source(), "pingedash");
    assert_eq!(PaidPingKind::Kevin.source(), "pingedkevin");
}

#[test]
fn test_second_claim_within_cooldown_is_rejected() {
    const COOLDOWN: i64 = 30 * 86_400;
    let fixture = Fixture::new();
    fixture.player(BUYER, 0);
    assert!(
        fixture
            .repository
            .try_claim_double_or_nothing(BUYER, GUILD, NOW, COOLDOWN)
            .unwrap()
    );
    assert!(
        !fixture
            .repository
            .try_claim_double_or_nothing(BUYER, GUILD, NOW + 5, COOLDOWN)
            .unwrap()
    );
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .unwrap()
            .expect("claimed player")
            .last_double_or_nothing,
        Some(NOW)
    );
}

#[test]
fn test_claim_succeeds_again_after_cooldown_elapses() {
    const COOLDOWN: i64 = 30 * 86_400;
    let fixture = Fixture::new();
    fixture.player(BUYER, 0);
    assert!(
        fixture
            .repository
            .try_claim_double_or_nothing(BUYER, GUILD, NOW, COOLDOWN)
            .unwrap()
    );
    let later = NOW + COOLDOWN + 1;
    assert!(
        fixture
            .repository
            .try_claim_double_or_nothing(BUYER, GUILD, later, COOLDOWN)
            .unwrap()
    );
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .unwrap()
            .expect("claimed player")
            .last_double_or_nothing,
        Some(later)
    );
}

#[test]
fn test_claim_requires_player_row() {
    let fixture = Fixture::new();
    assert!(
        !fixture
            .repository
            .try_claim_double_or_nothing(999, GUILD, NOW, 30 * 86_400)
            .unwrap()
    );
}

#[test]
fn get_last_double_or_nothing_returns_none_for_new_player() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .expect("read new player")
            .expect("player row")
            .last_double_or_nothing,
        None
    );
}

#[test]
fn log_double_or_nothing_updates_cooldown_timestamp() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: true,
            now: NOW,
            cooldown_seconds: 30 * 86_400,
            bypass_cooldown: false,
        })
        .expect("settle spin");
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .expect("read player")
            .expect("player row")
            .last_double_or_nothing,
        Some(NOW)
    );
}

#[test]
fn log_double_or_nothing_records_history_fields() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    let spin = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: true,
            now: NOW,
            cooldown_seconds: 30 * 86_400,
            bypass_cooldown: false,
        })
        .expect("settle winning spin");
    assert_eq!(
        (
            spin.balance_before,
            spin.balance_at_risk,
            spin.balance_after
        ),
        (100, 75, 150)
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT cost,balance_before,balance_after,won,spin_time
                 FROM double_or_nothing_spins WHERE discord_id=?1 AND guild_id=?2",
                params![BUYER, GUILD],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .expect("read spin history"),
        (25, 75, 150, 1, NOW)
    );
}

#[test]
fn log_double_or_nothing_records_loss_fields() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: false,
            now: NOW,
            cooldown_seconds: 30 * 86_400,
            bypass_cooldown: false,
        })
        .expect("settle losing spin");
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT balance_before,balance_after,won
                 FROM double_or_nothing_spins WHERE discord_id=?1 AND guild_id=?2",
                params![BUYER, GUILD],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("read loss history"),
        (75, 0, 0)
    );
}

#[test]
fn log_double_or_nothing_tracks_multiple_spins_in_order() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    const COOLDOWN: i64 = 30 * 86_400;
    fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: true,
            now: NOW,
            cooldown_seconds: COOLDOWN,
            bypass_cooldown: false,
        })
        .expect("settle first spin");
    let later = NOW + COOLDOWN + 1;
    fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: false,
            now: later,
            cooldown_seconds: COOLDOWN,
            bypass_cooldown: false,
        })
        .expect("settle second spin");
    let rows = fixture
        .connection()
        .prepare(
            "SELECT spin_time,won FROM double_or_nothing_spins
             WHERE discord_id=?1 AND guild_id=?2 ORDER BY spin_time,spin_id",
        )
        .expect("prepare history")
        .query_map(params![BUYER, GUILD], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .expect("query history")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect history");
    assert_eq!(rows, vec![(NOW, 1), (later, 0)]);
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .expect("read player")
            .expect("player row")
            .last_double_or_nothing,
        Some(later)
    );
}

#[test]
fn double_or_nothing_cooldown_not_passed_rejects_a_second_spin() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    const COOLDOWN: i64 = 30 * 86_400;
    fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: true,
            now: NOW,
            cooldown_seconds: COOLDOWN,
            bypass_cooldown: false,
        })
        .expect("settle first spin");
    let blocked = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: true,
            now: NOW + 1,
            cooldown_seconds: COOLDOWN,
            bypass_cooldown: false,
        })
        .expect("read cooldown");
    assert_eq!(blocked.reason, Some(PurchaseFailureReason::OnCooldown));
    assert_eq!(blocked.cooldown_ends_at, Some(NOW + COOLDOWN));
}

#[test]
fn double_or_nothing_cooldown_passed_allows_a_new_spin() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    const COOLDOWN: i64 = 30 * 86_400;
    fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: true,
            now: NOW,
            cooldown_seconds: COOLDOWN,
            bypass_cooldown: false,
        })
        .expect("settle first spin");
    let later = NOW + COOLDOWN + 1;
    let second = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: false,
            now: later,
            cooldown_seconds: COOLDOWN,
            bypass_cooldown: false,
        })
        .expect("settle after cooldown");
    assert!(second.success);
    assert_eq!(second.cooldown_ends_at, Some(later + COOLDOWN));
}

#[test]
fn get_double_or_nothing_history_is_empty_for_new_player() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM double_or_nothing_spins
                 WHERE discord_id=?1 AND guild_id=?2",
                params![BUYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("count history"),
        0
    );
}

#[test]
fn repository_constructor_is_side_effect_free() {
    let temporary = tempfile::tempdir().expect("temporary shop directory");
    let path = temporary.path().join("missing.db");
    let repository = ShopRuntimeRepository::new(&path);
    assert!(!path.exists());
    let error = repository
        .verify_schema()
        .expect_err("unmigrated database must fail closed");
    assert!(error.to_string().contains("SQLite"));
}

#[test]
fn paid_ping_and_double_or_nothing_are_atomic_and_audited() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    let first = fixture
        .repository
        .purchase_paid_ping(PaidPingKind::Dash, BUYER, GUILD, 10, NOW, 60)
        .expect("first Pingedash purchase");
    assert!(first.success);
    assert_eq!(first.balance, 90);
    let blocked = fixture
        .repository
        .purchase_paid_ping(PaidPingKind::Dash, BUYER, GUILD, 10, NOW + 1, 60)
        .expect("blocked Pingedash purchase");
    assert_eq!(blocked.reason, Some(PurchaseFailureReason::OnCooldown));
    assert_eq!(blocked.balance, 90);

    let spin = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 10,
            won: false,
            now: NOW,
            cooldown_seconds: 60,
            bypass_cooldown: false,
        })
        .expect("Double or Nothing settlement");
    assert!(spin.success);
    assert_eq!(
        (
            spin.balance_before,
            spin.balance_at_risk,
            spin.balance_after
        ),
        (90, 80, 0)
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM double_or_nothing_spins WHERE discord_id=?1",
                [BUYER],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT source FROM economy_ledger_entries WHERE account_id=?1 ORDER BY ledger_id LIMIT 1",
                [BUYER],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "pingedash"
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn double_or_nothing_log_failure_rolls_back_and_retry_commits_once() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 200);
    fixture
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_double_log BEFORE INSERT ON double_or_nothing_spins
             BEGIN SELECT RAISE(ABORT, 'forced log failure'); END;",
        )
        .expect("install forced failure");
    let request = DoubleOrNothingRequest {
        discord_id: BUYER,
        guild_id: GUILD,
        cost: 50,
        won: false,
        now: NOW,
        cooldown_seconds: 60,
        bypass_cooldown: false,
    };
    assert!(
        fixture
            .repository
            .settle_double_or_nothing(request)
            .unwrap_err()
            .to_string()
            .contains("forced log failure")
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance,last_double_or_nothing FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![BUYER, GUILD],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .unwrap(),
        (200, None)
    );
    connection
        .execute("DROP TRIGGER fail_double_log", [])
        .expect("remove forced failure");
    drop(connection);
    let settled = fixture
        .repository
        .settle_double_or_nothing(request)
        .expect("retry settlement");
    assert!(settled.success);
    assert_eq!((settled.balance_before, settled.balance_after), (200, 0));
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM double_or_nothing_spins", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
}

#[test]
fn concurrent_paid_ping_and_double_or_nothing_each_settle_once() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 200);
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository
                    .purchase_paid_ping(PaidPingKind::Dash, BUYER, GUILD, 10, NOW, 60)
                    .expect("concurrent paid ping")
            })
        })
        .collect::<Vec<_>>();
    let pings = handles
        .into_iter()
        .map(|handle| handle.join().expect("paid-ping thread"))
        .collect::<Vec<_>>();
    assert_eq!(pings.iter().filter(|result| result.success).count(), 1);
    assert_eq!(
        pings
            .iter()
            .filter(|result| result.reason == Some(PurchaseFailureReason::OnCooldown))
            .count(),
        1
    );

    let barrier = Arc::new(Barrier::new(2));
    let handles = [true, false]
        .into_iter()
        .map(|won| {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository
                    .settle_double_or_nothing(DoubleOrNothingRequest {
                        discord_id: BUYER,
                        guild_id: GUILD,
                        cost: 50,
                        won,
                        now: NOW,
                        cooldown_seconds: 60,
                        bypass_cooldown: false,
                    })
                    .expect("concurrent Double or Nothing")
            })
        })
        .collect::<Vec<_>>();
    let spins = handles
        .into_iter()
        .map(|handle| handle.join().expect("Double-or-Nothing thread"))
        .collect::<Vec<_>>();
    assert_eq!(spins.iter().filter(|result| result.success).count(), 1);
    assert_eq!(
        spins
            .iter()
            .filter(|result| result.reason == Some(PurchaseFailureReason::OnCooldown))
            .count(),
        1
    );
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM double_or_nothing_spins", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
}

#[test]
fn concurrent_conditional_spends_never_overdraw_or_leak_ledger_context() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 10);
    let repository = Arc::new(fixture.repository.clone());
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                repository
                    .try_spend(BUYER, GUILD, 10, "shop_jopa_coin", "test spend")
                    .expect("conditional spend")
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("spend thread"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| **result).count(), 1);
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
                params![BUYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM economy_ledger_context", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn test_protection_guardian_halves_current_loss_and_spends_absorbed_capacity() {
    let fixture = Fixture::new();
    fixture.player(VICTIM, 100);
    fixture
        .connection()
        .execute(
            "INSERT INTO player_mana(
                 discord_id,guild_id,current_land,assigned_date,consumed_today,
                 white_shield_remaining
             ) VALUES(?1,?2,'Plains','2033-05-18',0,25)",
            params![VICTIM, GUILD],
        )
        .expect("seed Plains Guardian");

    let result = fixture
        .repository
        .apply_hostile_loss(&HostileLossRequest {
            victim_id: VICTIM,
            guild_id: GUILD,
            requested: 20,
            kind: "pyroclasm".to_owned(),
            actor_id: Some(BUYER),
            event_key: "pyro:one".to_owned(),
            destination: HostileDestination::Burn,
            recipient_id: None,
            clamp_to_balance: false,
            min_balance: None,
            metadata: json!({}),
            occurred_at: NOW,
            mana_date: "2033-05-18".to_owned(),
        })
        .expect("Guardian-protected loss");

    assert_eq!(
        (
            result.requested,
            result.attempted,
            result.absorbed,
            result.applied
        ),
        (20, 20, 10, 10)
    );
    assert_eq!(result.details[0].source, "guardian");
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        90
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT white_shield_remaining FROM player_mana
                 WHERE discord_id=?1 AND guild_id=?2",
                params![VICTIM, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        15
    );
}

#[test]
fn test_protection_aegis_capacity_reduces_both_sides_of_player_transfer() {
    let fixture = Fixture::new();
    fixture.player(VICTIM, 100);
    fixture.player(BUYER, 0);
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'aegis',?3,?4,0,?5)",
            params![
                VICTIM,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":75,"capacity_remaining":75,"rate":1.0}"#,
            ],
        )
        .expect("seed Aegis");

    let result = fixture
        .repository
        .apply_hostile_loss(&HostileLossRequest {
            victim_id: VICTIM,
            guild_id: GUILD,
            requested: 100,
            kind: "red_shell".to_owned(),
            actor_id: Some(BUYER),
            event_key: "red-shell:one".to_owned(),
            destination: HostileDestination::Player,
            recipient_id: Some(BUYER),
            clamp_to_balance: false,
            min_balance: None,
            metadata: json!({}),
            occurred_at: NOW,
            mana_date: "2033-05-18".to_owned(),
        })
        .expect("Aegis-protected transfer");

    assert_eq!((result.absorbed, result.applied), (75, 25));
    assert_eq!(
        (
            result.victim_balance_after,
            result.destination_balance_after
        ),
        (75, Some(25))
    );
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        75
    );
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        25
    );
    let duplicate = fixture
        .repository
        .apply_hostile_loss(&HostileLossRequest {
            event_key: "red-shell:one".to_owned(),
            ..HostileLossRequest {
                victim_id: VICTIM,
                guild_id: GUILD,
                requested: 100,
                kind: "red_shell".to_owned(),
                actor_id: Some(BUYER),
                event_key: String::new(),
                destination: HostileDestination::Player,
                recipient_id: Some(BUYER),
                clamp_to_balance: false,
                min_balance: None,
                metadata: json!({}),
                occurred_at: NOW,
                mana_date: "2033-05-18".to_owned(),
            }
        })
        .expect("idempotent transfer retry");
    assert!(duplicate.duplicate);
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        25
    );
}

#[test]
fn test_protection_shared_sanctuary_pool_is_consumed_by_caster_and_ally() {
    let fixture = Fixture::new();
    fixture.player(VICTIM, 200);
    fixture.player(303, 200);
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,target_id,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'sanctuary',?3,?4,?5,0,?6)",
            params![
                VICTIM,
                GUILD,
                303_i64,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":150,"capacity_remaining":150,"rate":1.0,"shared":true}"#,
            ],
        )
        .expect("seed shared Sanctuary");

    let request = |victim_id, amount, key: &str| HostileLossRequest {
        victim_id,
        guild_id: GUILD,
        requested: amount,
        kind: "lightning_bolt".to_owned(),
        actor_id: Some(BUYER),
        event_key: key.to_owned(),
        destination: HostileDestination::Reserve,
        recipient_id: None,
        clamp_to_balance: false,
        min_balance: None,
        metadata: json!({}),
        occurred_at: NOW,
        mana_date: "2033-05-18".to_owned(),
    };
    let caster = fixture
        .repository
        .apply_hostile_loss(&request(VICTIM, 100, "bolt:caster"))
        .expect("caster Sanctuary settlement");
    let ally = fixture
        .repository
        .apply_hostile_loss(&request(303, 80, "bolt:ally"))
        .expect("ally Sanctuary settlement");

    assert_eq!((caster.absorbed, caster.applied), (100, 0));
    assert_eq!((ally.absorbed, ally.applied), (50, 30));
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
                [GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        30
    );
    let connection = fixture.connection();
    let (triggered, data): (i64, String) = connection
        .query_row(
            "SELECT triggered,data FROM manashop_buffs
             WHERE discord_id=?1 AND guild_id=?2 AND buff_type='sanctuary'",
            params![VICTIM, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(triggered, 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).unwrap()["capacity_remaining"],
        0
    );
}

#[test]
fn test_protection_hostile_loss_batch_preserves_order_pools_destinations_and_failures() {
    let fixture = Fixture::new();
    for (discord_id, balance) in [
        (VICTIM, 200),
        (303, 200),
        (304, 100),
        (305, 100),
        (BUYER, 0),
    ] {
        fixture.player(discord_id, balance);
    }
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,target_id,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'sanctuary',?3,?4,?5,0,?6)",
            params![
                VICTIM,
                GUILD,
                303_i64,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":150,"capacity_remaining":150,"rate":1.0,"shared":true}"#,
            ],
        )
        .expect("seed batch Sanctuary");
    let request =
        |victim_id, amount, key: &str, destination, recipient_id, order| HostileLossRequest {
            victim_id,
            guild_id: GUILD,
            requested: amount,
            kind: "batch_loss".to_owned(),
            actor_id: Some(BUYER),
            event_key: key.to_owned(),
            destination,
            recipient_id,
            clamp_to_balance: false,
            min_balance: None,
            metadata: json!({"order": order}),
            occurred_at: NOW,
            mana_date: "2033-05-18".to_owned(),
        };
    let losses = vec![
        request(
            VICTIM,
            100,
            "batch:10",
            HostileDestination::Reserve,
            None,
            0,
        ),
        request(404, 20, "batch:missing", HostileDestination::Burn, None, 1),
        request(
            303,
            80,
            "batch:11",
            HostileDestination::Player,
            Some(BUYER),
            2,
        ),
        request(304, 10, "batch:12", HostileDestination::Burn, None, 3),
        request(305, 10, "batch:13", HostileDestination::Reserve, None, 4),
    ];
    let outcomes = fixture
        .repository
        .apply_hostile_losses(&losses)
        .expect("ordered hostile batch");
    assert_eq!(outcomes.len(), 5);
    assert_eq!(
        outcomes
            .iter()
            .enumerate()
            .filter_map(|(index, outcome)| outcome.as_ref().ok().map(|value| (
                index,
                value.absorbed,
                value.applied
            )))
            .collect::<Vec<_>>(),
        vec![(0, 100, 0), (2, 50, 30), (3, 0, 10), (4, 0, 10)]
    );
    assert!(outcomes[1].as_ref().unwrap_err().contains("not registered"));
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        200
    );
    assert_eq!(
        fixture
            .repository
            .player(303, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        170
    );
    assert_eq!(
        fixture
            .repository
            .player(304, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        90
    );
    assert_eq!(
        fixture
            .repository
            .player(305, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        90
    );
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        30
    );
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT total_collected FROM nonprofit_fund WHERE guild_id=?1",
                [GUILD],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
    let rows = fixture
        .connection()
        .prepare(
            "SELECT event_key,destination,metadata FROM hostile_loss_events
             WHERE event_key LIKE 'batch:%' ORDER BY event_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows.iter().map(|row| (&row.0, &row.1)).collect::<Vec<_>>(),
        vec![
            (&"batch:10".to_owned(), &"reserve".to_owned()),
            (&"batch:11".to_owned(), &"player".to_owned()),
            (&"batch:12".to_owned(), &"burn".to_owned()),
            (&"batch:13".to_owned(), &"reserve".to_owned()),
        ]
    );
    assert_eq!(
        rows.iter()
            .map(
                |row| serde_json::from_str::<serde_json::Value>(&row.2).unwrap()["order"]
                    .as_i64()
                    .unwrap()
            )
            .collect::<Vec<_>>(),
        vec![0, 2, 3, 4]
    );
    let duplicates = fixture
        .repository
        .apply_hostile_losses(&losses)
        .expect("idempotent hostile batch retry");
    assert!(
        [0, 2, 3, 4]
            .iter()
            .all(|index| duplicates[*index].as_ref().unwrap().duplicate)
    );
    assert!(
        duplicates[1]
            .as_ref()
            .unwrap_err()
            .contains("not registered")
    );
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        30
    );
}

#[test]
fn test_protection_reprieve_then_guardian_compound_in_documented_order() {
    let fixture = Fixture::new();
    fixture.player(VICTIM, 100);
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'reprieve',?3,?4,0,?5)",
            params![
                VICTIM,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":25,"capacity_remaining":25,"rate":0.5}"#,
            ],
        )
        .expect("seed Reprieve");
    fixture
        .connection()
        .execute(
            "INSERT INTO player_mana(
                 discord_id,guild_id,current_land,assigned_date,consumed_today,
                 white_shield_remaining
             ) VALUES(?1,?2,'Plains','2033-05-18',0,25)",
            params![VICTIM, GUILD],
        )
        .expect("seed Guardian");

    let result = fixture
        .repository
        .apply_hostile_loss(&HostileLossRequest {
            victim_id: VICTIM,
            guild_id: GUILD,
            requested: 20,
            kind: "soul_harvest".to_owned(),
            actor_id: Some(BUYER),
            event_key: "harvest:one".to_owned(),
            destination: HostileDestination::Burn,
            recipient_id: None,
            clamp_to_balance: false,
            min_balance: None,
            metadata: json!({}),
            occurred_at: NOW,
            mana_date: "2033-05-18".to_owned(),
        })
        .expect("compound protection");
    assert_eq!((result.absorbed, result.applied), (15, 5));
    assert_eq!(
        result
            .details
            .iter()
            .map(|detail| detail.source.as_str())
            .collect::<Vec<_>>(),
        vec!["reprieve", "guardian"]
    );
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        95
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row("SELECT white_shield_remaining FROM player_mana WHERE discord_id=?1 AND guild_id=?2", params![VICTIM, GUILD], |row| row.get::<_, i64>(0))
            .unwrap(),
        20
    );
    let reprieve: String = connection
        .query_row("SELECT data FROM manashop_buffs WHERE discord_id=?1 AND guild_id=?2 AND buff_type='reprieve'", params![VICTIM, GUILD], |row| row.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&reprieve).unwrap()["capacity_remaining"],
        15
    );
}

#[test]
fn test_protection_self_caused_loss_bypasses_shields_and_retro() {
    let fixture = Fixture::new();
    fixture.player(VICTIM, 100);
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'aegis',?3,?4,0,?5)",
            params![
                VICTIM,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":75,"capacity_remaining":75,"rate":1.0}"#,
            ],
        )
        .expect("seed Aegis");
    fixture
        .connection()
        .execute(
            "INSERT INTO player_mana(
                 discord_id,guild_id,current_land,assigned_date,consumed_today,
                 white_shield_remaining
             ) VALUES(?1,?2,'Plains','2033-05-18',0,25)",
            params![VICTIM, GUILD],
        )
        .expect("seed Guardian");
    let result = fixture
        .repository
        .apply_hostile_loss(&HostileLossRequest {
            victim_id: VICTIM,
            guild_id: GUILD,
            requested: 20,
            kind: "blue_shell".to_owned(),
            actor_id: Some(VICTIM),
            event_key: "blue-shell:self".to_owned(),
            destination: HostileDestination::Reserve,
            recipient_id: None,
            clamp_to_balance: false,
            min_balance: None,
            metadata: json!({}),
            occurred_at: NOW,
            mana_date: "2033-05-18".to_owned(),
        })
        .expect("self-caused loss");
    assert_eq!((result.absorbed, result.applied), (0, 20));
    assert!(result.details.is_empty());
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        80
    );
    let connection = fixture.connection();
    assert_eq!(connection.query_row("SELECT white_shield_remaining FROM player_mana WHERE discord_id=?1 AND guild_id=?2", params![VICTIM, GUILD], |row| row.get::<_, i64>(0)).unwrap(), 25);
    let data: String = connection.query_row("SELECT data FROM manashop_buffs WHERE discord_id=?1 AND guild_id=?2 AND buff_type='aegis'", params![VICTIM, GUILD], |row| row.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).unwrap()["capacity_remaining"],
        75
    );
}

#[test]
fn test_protection_reprieve_retro_uses_rolling_pool_and_is_idempotent() {
    let fixture = Fixture::new();
    fixture.player(VICTIM, 100);
    let loss = fixture
        .repository
        .apply_hostile_loss(&HostileLossRequest {
            victim_id: VICTIM,
            guild_id: GUILD,
            requested: 20,
            kind: "pyroclasm".to_owned(),
            actor_id: Some(BUYER),
            event_key: "pyro:retro".to_owned(),
            destination: HostileDestination::Burn,
            recipient_id: None,
            clamp_to_balance: false,
            min_balance: None,
            metadata: json!({}),
            occurred_at: NOW - 100,
            mana_date: "2033-05-18".to_owned(),
        })
        .expect("loss before Reprieve")
        .event_id;
    let connection = fixture.connection();
    connection
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'reprieve',?3,?4,0,?5)",
            params![
                VICTIM,
                GUILD,
                NOW,
                NOW + 86_400,
                r#"{"capacity":25,"capacity_remaining":25,"rate":0.5,"rolling_retroactive":true}"#,
            ],
        )
        .expect("seed rolling Reprieve");
    let buff_id = connection.last_insert_rowid();
    drop(connection);
    assert!(loss > 0);
    assert_eq!(
        fixture
            .repository
            .reconcile_purchased_pool(VICTIM, GUILD, buff_id, NOW - 86_400, NOW)
            .unwrap(),
        10
    );
    assert_eq!(
        fixture
            .repository
            .reconcile_purchased_pool(VICTIM, GUILD, buff_id, NOW - 86_400, NOW)
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        90
    );
    let connection = fixture.connection();
    let data: String = connection
        .query_row(
            "SELECT data FROM manashop_buffs WHERE id=?1",
            [buff_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).unwrap()["capacity_remaining"],
        15
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM mana_protection_events WHERE hostile_loss_event_id=?1",
                [loss],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn test_protection_min_balance_short_circuits_before_spending_pool() {
    let fixture = Fixture::new();
    fixture.player(VICTIM, 49);
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'aegis',?3,?4,0,?5)",
            params![
                VICTIM,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":75,"capacity_remaining":75,"rate":1.0}"#,
            ],
        )
        .expect("seed Aegis");
    let result = fixture
        .repository
        .apply_hostile_loss(&HostileLossRequest {
            victim_id: VICTIM,
            guild_id: GUILD,
            requested: 20,
            kind: "bomb_omb".to_owned(),
            actor_id: Some(BUYER),
            event_key: "bomb:threshold".to_owned(),
            destination: HostileDestination::Burn,
            recipient_id: None,
            clamp_to_balance: true,
            min_balance: Some(50),
            metadata: json!({}),
            occurred_at: NOW,
            mana_date: "2033-05-18".to_owned(),
        })
        .expect("minimum balance short circuit");
    assert_eq!(
        (
            result.requested,
            result.attempted,
            result.absorbed,
            result.applied
        ),
        (20, 0, 0, 0)
    );
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        49
    );
    let data: String = fixture.connection().query_row("SELECT data FROM manashop_buffs WHERE discord_id=?1 AND guild_id=?2 AND buff_type='aegis'", params![VICTIM, GUILD], |row| row.get(0)).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&data).unwrap()["capacity_remaining"],
        75
    );
}

#[test]
fn test_protection_non_jc_attack_consumes_aegis_once() {
    let fixture = Fixture::new();
    fixture.player(VICTIM, 100);
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'aegis',?3,?4,0,?5)",
            params![
                VICTIM,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":75,"capacity_remaining":75,"rate":1.0}"#,
            ],
        )
        .expect("seed Aegis");
    let first = fixture
        .repository
        .block_non_jc_attack(VICTIM, GUILD, Some(BUYER), "sabotage:one", NOW)
        .expect("block non-JC attack");
    let duplicate = fixture
        .repository
        .block_non_jc_attack(VICTIM, GUILD, Some(BUYER), "sabotage:one", NOW)
        .expect("idempotent non-JC retry");
    assert!(first.blocked);
    assert_eq!(first.source.as_deref(), Some("aegis"));
    assert!(!first.duplicate);
    assert!(duplicate.blocked && duplicate.duplicate);
    assert_eq!(duplicate.event_id, first.event_id);
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        100
    );
    let connection = fixture.connection();
    assert_eq!(connection.query_row("SELECT triggered FROM manashop_buffs WHERE discord_id=?1 AND guild_id=?2 AND buff_type='aegis'", params![VICTIM, GUILD], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM hostile_loss_events WHERE event_key='sabotage:one'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM mana_protection_events WHERE hostile_loss_event_id=?1",
                [first.event_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn test_protection_is_guild_isolated() {
    let fixture = Fixture::new();
    fixture.player(VICTIM, 100);
    fixture
        .connection()
        .execute(
            "INSERT INTO players(
                 discord_id,guild_id,discord_username,jopacoin_balance
             ) VALUES(?1,?2,'other-guild-player',100)",
            params![VICTIM, PROTECTION_OTHER_GUILD],
        )
        .expect("seed secondary-guild player");
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'aegis',?3,?4,0,?5)",
            params![
                VICTIM,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":75,"capacity_remaining":75,"rate":1.0}"#,
            ],
        )
        .expect("seed primary-guild Aegis");
    let result = fixture
        .repository
        .apply_hostile_loss(&HostileLossRequest {
            victim_id: VICTIM,
            guild_id: PROTECTION_OTHER_GUILD,
            requested: 20,
            kind: "pyroclasm".to_owned(),
            actor_id: Some(BUYER),
            event_key: "pyro:other-guild".to_owned(),
            destination: HostileDestination::Burn,
            recipient_id: None,
            clamp_to_balance: false,
            min_balance: None,
            metadata: json!({}),
            occurred_at: NOW,
            mana_date: "2033-05-18".to_owned(),
        })
        .expect("secondary-guild loss");
    assert_eq!((result.absorbed, result.applied), (0, 20));
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .unwrap()
            .balance,
        100
    );
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, PROTECTION_OTHER_GUILD)
            .unwrap()
            .unwrap()
            .balance,
        80
    );
    let capacity: i64 = fixture
        .connection()
        .query_row(
            "SELECT json_extract(data,'$.capacity_remaining')
             FROM manashop_buffs WHERE discord_id=?1 AND guild_id=?2",
            params![VICTIM, GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(capacity, 75);
}

#[test]
fn hostile_transfer_consumes_protection_once_and_attributes_both_ledger_sides() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 10);
    fixture.player(VICTIM, 100);
    fixture
        .connection()
        .execute(
            "INSERT INTO manashop_buffs(
               discord_id,guild_id,buff_type,granted_at,expires_at,triggered,data)
             VALUES(?1,?2,'aegis',?3,?4,0,?5)",
            params![
                VICTIM,
                GUILD,
                NOW - 1,
                NOW + 100,
                r#"{"capacity":75,"capacity_remaining":75,"rate":1.0}"#
            ],
        )
        .expect("seed Aegis");
    let request = HostileLossRequest {
        victim_id: VICTIM,
        guild_id: GUILD,
        requested: 100,
        kind: "soul_harvest".to_owned(),
        actor_id: Some(BUYER),
        event_key: "shop-test:hostile".to_owned(),
        destination: HostileDestination::Player,
        recipient_id: Some(BUYER),
        clamp_to_balance: true,
        min_balance: Some(50),
        metadata: json!({"oracle": "python"}),
        occurred_at: NOW,
        mana_date: "2033-05-18".to_owned(),
    };
    let first = fixture
        .repository
        .apply_hostile_loss(&request)
        .expect("hostile transfer");
    assert_eq!((first.absorbed, first.applied), (75, 25));
    assert_eq!(
        (first.victim_balance_after, first.destination_balance_after),
        (75, Some(35))
    );
    let duplicate = fixture
        .repository
        .apply_hostile_loss(&request)
        .expect("idempotent hostile retry");
    assert!(duplicate.duplicate);
    let connection = fixture.connection();
    let balances = connection
        .prepare("SELECT discord_id,jopacoin_balance FROM players ORDER BY discord_id")
        .unwrap()
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(balances, vec![(BUYER, 35), (VICTIM, 75)]);
    let sources = connection
        .prepare("SELECT source FROM economy_ledger_entries ORDER BY ledger_id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(sources, vec!["hostile_loss", "hostile_loss"]);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mana_protection_events", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
}

#[test]
fn hostile_batch_uses_per_request_savepoints_and_detects_conflicting_retry() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 10);
    fixture.player(VICTIM, 100);
    fixture.player(303, 100);
    let valid = |victim_id, suffix: &str| HostileLossRequest {
        victim_id,
        guild_id: GUILD,
        requested: 10,
        kind: "wildfire".to_owned(),
        actor_id: Some(BUYER),
        event_key: format!("batch:{suffix}"),
        destination: HostileDestination::Burn,
        recipient_id: None,
        clamp_to_balance: true,
        min_balance: Some(50),
        metadata: json!({}),
        occurred_at: NOW,
        mana_date: "2033-05-18".to_owned(),
    };
    let invalid = HostileLossRequest {
        requested: 0,
        event_key: "batch:invalid".to_owned(),
        ..valid(VICTIM, "discarded-template")
    };
    let first = valid(VICTIM, "one");
    let third = valid(303, "three");
    let results = fixture
        .repository
        .apply_hostile_losses(&[first.clone(), invalid, third])
        .expect("savepoint batch");
    assert!(results[0].is_ok());
    assert!(
        results[1]
            .as_ref()
            .unwrap_err()
            .contains("must be positive")
    );
    assert!(results[2].is_ok());
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT COUNT(*) FROM hostile_loss_events", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        2
    );
    let conflict = HostileLossRequest {
        requested: 11,
        ..first
    };
    assert!(matches!(
        fixture.repository.apply_hostile_loss(&conflict),
        Err(ShopRuntimeRepositoryError::EventPayloadConflict)
    ));
    assert_eq!(
        fixture
            .repository
            .player(VICTIM, GUILD)
            .unwrap()
            .expect("victim")
            .balance,
        90
    );
}

#[test]
fn recent_losses_and_dig_effects_match_python_queries_and_survive_reopen() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    let connection = fixture.connection();
    connection
        .execute_batch(&format!(
            "INSERT INTO matches(match_id,guild_id,winning_team) VALUES(1,{GUILD},1),(2,{GUILD},2);
             INSERT INTO bets(guild_id,match_id,discord_id,team_bet_on,amount,bet_time,leverage)
             VALUES
               ({GUILD},1,{BUYER},'radiant',10,{NOW},2),
               ({GUILD},1,{BUYER},'dire',7,{NOW},3),
               ({GUILD},2,{BUYER},'radiant',5,{NOW},2);
             INSERT INTO wheel_spins(guild_id,discord_id,result,spin_time)
             VALUES({GUILD},{BUYER},-8,{NOW}),({GUILD},{BUYER},-50,{});
             INSERT INTO tunnels(discord_id,guild_id,depth,luminosity,prestige_level,last_dig_at)
             VALUES({BUYER},{GUILD},42,88,2,1000);
             INSERT INTO dig_artifacts(discord_id,guild_id,artifact_id,is_relic,equipped)
             VALUES({BUYER},{GUILD},'mana_conduit',1,1),
                   ({BUYER},{GUILD},'unused',1,0);",
            NOW - 100
        ))
        .expect("seed loss and dig rows");
    drop(connection);
    assert_eq!(
        fixture
            .repository
            .recent_loss_aggregates(BUYER, GUILD, NOW - 10)
            .unwrap(),
        RecentLossAggregates {
            largest: 21,
            total: 39
        }
    );
    assert_eq!(
        fixture
            .repository
            .shave_dig_cooldown(BUYER, GUILD, 45 * 60)
            .unwrap(),
        45 * 60
    );
    fixture
        .repository
        .set_dig_temp_buff(
            BUYER,
            GUILD,
            &json!({
                "id": "dynamite_cache",
                "name": "Dynamite Cache",
                "digs_remaining": 3,
                "effect": {"yield_multiplier": 1.75}
            }),
        )
        .expect("persist dig buff");
    assert!(
        fixture
            .repository
            .has_equipped_relic(BUYER, GUILD, "mana_conduit")
            .unwrap()
    );
    let info = ShopRuntimeRepository::new(fixture.database.path())
        .dig_tunnel_info(BUYER, GUILD)
        .unwrap()
        .expect("reopened tunnel");
    assert_eq!(
        info,
        DigTunnelInfo {
            depth: 42,
            luminosity: 88,
            prestige_level: 2,
            relic_ids: vec!["mana_conduit".to_owned()],
        }
    );
    let connection = fixture.connection();
    assert_eq!(
        connection
            .query_row(
                "SELECT last_dig_at FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![BUYER, GUILD],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    let payload: String = connection
        .query_row(
            "SELECT temp_buffs FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
            params![BUYER, GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload).unwrap()["digs_remaining"],
        3
    );
}

/// Python owns the schema, Rust performs live Shop settlements, then Python
/// verifies balances, cooldown, idempotency, and trigger-authored audit rows.
#[test]
fn double_or_nothing_new_player_has_no_previous_spin() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    let account = fixture
        .repository
        .player(BUYER, GUILD)
        .expect("player lookup")
        .expect("seeded player");
    assert_eq!(account.last_double_or_nothing, None);
}

#[test]
fn double_or_nothing_log_updates_cooldown_timestamp() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    let result = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: true,
            now: NOW,
            cooldown_seconds: 30 * 86_400,
            bypass_cooldown: false,
        })
        .expect("double settlement");
    assert!(result.success);
    assert_eq!(
        fixture
            .repository
            .player(BUYER, GUILD)
            .unwrap()
            .unwrap()
            .last_double_or_nothing,
        Some(NOW)
    );
}

#[test]
fn double_or_nothing_win_history_preserves_amounts() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    let result = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: true,
            now: NOW,
            cooldown_seconds: 60,
            bypass_cooldown: false,
        })
        .unwrap();
    assert_eq!(
        (
            result.balance_before,
            result.balance_at_risk,
            result.balance_after
        ),
        (100, 75, 150)
    );
    let row = fixture
        .connection()
        .query_row(
            "SELECT cost,balance_before,balance_after,won,spin_time
             FROM double_or_nothing_spins WHERE discord_id=?1",
            [BUYER],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row, (25, 75, 150, 1, NOW));
}

#[test]
fn double_or_nothing_loss_history_records_zero_result() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    let result = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: false,
            now: NOW,
            cooldown_seconds: 60,
            bypass_cooldown: false,
        })
        .unwrap();
    assert_eq!(result.balance_after, 0);
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT won,balance_after FROM double_or_nothing_spins WHERE discord_id=?1",
                [BUYER],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
        (0, 0)
    );
}

#[test]
fn double_or_nothing_multiple_spins_are_ordered() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 500);
    for (now, won) in [(NOW, true), (NOW + 30 * 86_400 + 1, false)] {
        fixture
            .repository
            .settle_double_or_nothing(DoubleOrNothingRequest {
                discord_id: BUYER,
                guild_id: GUILD,
                cost: 25,
                won,
                now,
                cooldown_seconds: 30 * 86_400,
                bypass_cooldown: false,
            })
            .unwrap();
    }
    let times = fixture
        .connection()
        .prepare(
            "SELECT spin_time FROM double_or_nothing_spins WHERE discord_id=?1 ORDER BY spin_time",
        )
        .unwrap()
        .query_map([BUYER], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(times, vec![NOW, NOW + 30 * 86_400 + 1]);
}

#[test]
fn double_or_nothing_history_is_empty_before_first_spin() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    let count: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM double_or_nothing_spins WHERE discord_id=?1 AND guild_id=?2",
            params![BUYER, GUILD],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn double_or_nothing_cooldown_blocks_before_expiry() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 100);
    let request = DoubleOrNothingRequest {
        discord_id: BUYER,
        guild_id: GUILD,
        cost: 25,
        won: true,
        now: NOW,
        cooldown_seconds: 30 * 86_400,
        bypass_cooldown: false,
    };
    assert!(
        fixture
            .repository
            .settle_double_or_nothing(request)
            .unwrap()
            .success
    );
    let blocked = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            now: NOW + 1,
            ..request
        })
        .unwrap();
    assert_eq!(blocked.reason, Some(PurchaseFailureReason::OnCooldown));
}

#[test]
fn double_or_nothing_cooldown_allows_after_expiry() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 500);
    let cooldown = 30 * 86_400;
    let request = DoubleOrNothingRequest {
        discord_id: BUYER,
        guild_id: GUILD,
        cost: 25,
        won: true,
        now: NOW,
        cooldown_seconds: cooldown,
        bypass_cooldown: false,
    };
    assert!(
        fixture
            .repository
            .settle_double_or_nothing(request)
            .unwrap()
            .success
    );
    let allowed = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            now: NOW + cooldown + 1,
            ..request
        })
        .unwrap();
    assert!(allowed.success);
}

#[test]
fn double_or_nothing_cost_equal_to_balance_is_rejected() {
    let fixture = Fixture::new();
    fixture.player(BUYER, 25);
    let result = fixture
        .repository
        .settle_double_or_nothing(DoubleOrNothingRequest {
            discord_id: BUYER,
            guild_id: GUILD,
            cost: 25,
            won: true,
            now: NOW,
            cooldown_seconds: 60,
            bypass_cooldown: false,
        })
        .unwrap();
    assert_eq!(result.reason, Some(PurchaseFailureReason::NothingToDouble));
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE discord_id=?1",
                [BUYER],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        25
    );
}

#[test]
#[ignore = "cross-language smoke invokes the repository Python environment"]
fn python_migrated_database_shop_runtime_interop_smoke() {
    let database = NamedTempFile::new().expect("temporary interop database");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repository root");
    let python = root.join(".venv/bin/python");
    let initialize = Command::new(&python)
        .current_dir(root)
        .args([
            "-c",
            "import sqlite3,sys\nfrom infrastructure.schema_manager import SchemaManager\np=sys.argv[1]; SchemaManager(p).initialize(); c=sqlite3.connect(p)\nc.executemany('INSERT INTO players(discord_id,guild_id,discord_username,jopacoin_balance) VALUES(?,?,?,?)',[(-77001,0,'buyer',100),(-77002,0,'victim',100)])\nc.commit(); c.close()",
        ])
        .arg(database.path())
        .output()
        .expect("initialize Python Shop schema");
    assert!(
        initialize.status.success(),
        "Python initialization failed: {}",
        String::from_utf8_lossy(&initialize.stderr)
    );
    let repository = ShopRuntimeRepository::new(database.path());
    repository
        .verify_schema()
        .expect("admit Python Shop schema");
    assert!(
        repository
            .purchase_paid_ping(PaidPingKind::Dash, -77_001, 0, 10, NOW, 60)
            .unwrap()
            .success
    );
    let request = HostileLossRequest {
        victim_id: -77_002,
        guild_id: 0,
        requested: 20,
        kind: "soul_harvest".to_owned(),
        actor_id: Some(-77_001),
        event_key: "interop:soul".to_owned(),
        destination: HostileDestination::Player,
        recipient_id: Some(-77_001),
        clamp_to_balance: true,
        min_balance: Some(50),
        metadata: json!({}),
        occurred_at: NOW,
        mana_date: "2033-05-18".to_owned(),
    };
    assert_eq!(repository.apply_hostile_loss(&request).unwrap().applied, 20);
    assert!(repository.apply_hostile_loss(&request).unwrap().duplicate);

    let verify = Command::new(python)
        .current_dir(root)
        .args([
            "-c",
            "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1])\nassert c.execute('SELECT jopacoin_balance,last_pingedash FROM players WHERE discord_id=-77001 AND guild_id=0').fetchone()==(110,2000000000)\nassert c.execute('SELECT jopacoin_balance FROM players WHERE discord_id=-77002 AND guild_id=0').fetchone()[0]==80\nassert c.execute(\"SELECT COUNT(*) FROM hostile_loss_events WHERE event_key='interop:soul'\").fetchone()[0]==1\nsources=[r[0] for r in c.execute(\"SELECT source FROM economy_ledger_entries WHERE account_id IN (-77001,-77002) AND source IN ('pingedash','hostile_loss') ORDER BY ledger_id\")]\nassert sources==['pingedash','hostile_loss','hostile_loss'],sources\nassert c.execute('SELECT COUNT(*) FROM economy_ledger_context').fetchone()[0]==0\nc.close()",
        ])
        .arg(database.path())
        .output()
        .expect("verify Rust Shop writes from Python");
    assert!(
        verify.status.success(),
        "Python verification failed: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}
