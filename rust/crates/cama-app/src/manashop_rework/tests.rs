use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use cama_db::manashop_rework_repository::{
    BuffData, DarkBargainStatus, GrantBuffRequest, ManashopRepository,
};
use rusqlite::{Connection, params};

use super::*;

const USER: i64 = 111;
const ALLY: i64 = 222;
const TARGET: i64 = 333;
const GUILD: i64 = 9_001;
const OTHER_GUILD: i64 = 9_002;
const NOW: i64 = 2_000_000_000;

struct Fixture {
    path: PathBuf,
    repository: ManashopRepository,
}

impl Fixture {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "cama-manashop-app-{}-{}.db",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let schema = "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = OFF;
                 CREATE TABLE players (
                     discord_id INTEGER NOT NULL,
                     guild_id INTEGER NOT NULL,
                     discord_username TEXT,
                     jopacoin_balance INTEGER DEFAULT 3,
                     lowest_balance_ever INTEGER,
                     updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                     PRIMARY KEY (discord_id, guild_id)
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
                 CREATE TABLE economy_ledger_context (
                     id INTEGER PRIMARY KEY CHECK(id = 1),
                     source TEXT,
                     actor_id INTEGER,
                     related_type TEXT,
                     related_id TEXT,
                     reason TEXT,
                     metadata TEXT
                 );";
        Connection::open(&path)
            .and_then(|connection| connection.execute_batch(schema))
            .expect("initialize manashop fixture schema");
        Self {
            repository: ManashopRepository::new(&path),
            path,
        }
    }

    fn service(&self) -> BuffService {
        BuffService::new(self.repository.clone(), NOW)
    }

    fn register(&self, discord_id: i64, guild_id: i64, balance: i64) {
        Connection::open(&self.path)
            .and_then(|connection| {
                connection.execute(
                    "INSERT INTO players \
                     (discord_id, guild_id, discord_username, jopacoin_balance) \
                     VALUES (?1, ?2, 'player', ?3)",
                    params![discord_id, guild_id, balance],
                )
            })
            .expect("register manashop fixture player");
    }

    fn grant_raw(
        &self,
        owner: i64,
        guild: i64,
        kind: BuffType,
        target: Option<i64>,
        expires_at: i64,
        data: Option<&BuffData>,
    ) -> i64 {
        self.repository
            .grant_buff(GrantBuffRequest {
                discord_id: owner,
                guild_id: Some(guild),
                buff_type: kind.as_str(),
                target_id: target,
                granted_at: NOW - 100,
                expires_at,
                data,
            })
            .expect("grant fixture buff")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for candidate in [
            self.path.clone(),
            self.path.with_extension("db-wal"),
            self.path.with_extension("db-shm"),
        ] {
            let _ = std::fs::remove_file(candidate);
        }
    }
}

#[test]
fn test_buff_service_grant_and_active_for() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_counterspell(USER, GUILD).unwrap();
    let active = service
        .active_for(USER, GUILD, BuffType::Counterspell)
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].buff_type, "counterspell");
    assert!(active[0].expires_at > NOW);
}

#[test]
fn test_reprieve_grants_rolling_partial_protection_pool() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_reprieve(USER, GUILD).unwrap();
    let data = &service.active_for(USER, GUILD, BuffType::Reprieve).unwrap()[0].data;
    assert_eq!(data.capacity, Some(25));
    assert_eq!(data.capacity_remaining, Some(25));
    assert_eq!(data.rate, Some(0.5));
    assert_eq!(data.shared, Some(false));
    assert_eq!(data.rolling_retroactive, Some(true));
}

#[test]
fn test_aegis_grants_full_75_jc_protection_pool() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_aegis(USER, GUILD).unwrap();
    let data = &service.active_for(USER, GUILD, BuffType::Aegis).unwrap()[0].data;
    assert_eq!(data.capacity, Some(75));
    assert_eq!(data.capacity_remaining, Some(75));
    assert_eq!(data.rate, Some(1.0));
    assert_eq!(data.shared, Some(false));
    assert_eq!(data.rolling_retroactive, Some(false));
}

#[test]
fn test_blood_pact_target_snapshot_is_batched_and_guild_scoped() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_blood_pact(USER, GUILD, TARGET).unwrap();
    service.grant_blood_pact(USER, OTHER_GUILD, ALLY).unwrap();
    let triggered = service.grant_blood_pact(USER, GUILD, ALLY).unwrap();
    fixture.repository.consume_buff_atomic(triggered).unwrap();
    assert_eq!(
        service
            .blood_pact_targets(&[ALLY, TARGET, 999_999, TARGET], GUILD)
            .unwrap(),
        BTreeSet::from([TARGET])
    );
}

fn expired_dark_data() -> BuffData {
    BuffData {
        amount_due: Some(700),
        default_penalty: Some(1_600),
        default_penalty_games: Some(5),
        ..BuffData::default()
    }
}

#[test]
fn test_settle_due_dark_bargain_paid_once() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(USER, GUILD, 1_103);
    fixture.grant_raw(
        USER,
        GUILD,
        BuffType::DarkBargain,
        None,
        NOW - 1,
        Some(&expired_dark_data()),
    );
    let first = service.settle_due_dark_bargains().unwrap();
    let second = service.settle_due_dark_bargains().unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].status, DarkBargainStatus::Paid);
    assert_eq!(first[0].amount, 700);
    assert!(second.is_empty());
    assert_eq!(
        fixture.repository.balance(USER, Some(GUILD)).unwrap(),
        Some(403)
    );
}

#[test]
fn test_settle_due_dark_bargain_default_adds_penalty() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.register(USER, GUILD, 303);
    fixture.grant_raw(
        USER,
        GUILD,
        BuffType::DarkBargain,
        None,
        NOW - 1,
        Some(&expired_dark_data()),
    );
    let settled = service.settle_due_dark_bargains().unwrap();
    assert_eq!(settled[0].status, DarkBargainStatus::Defaulted);
    assert_eq!(settled[0].amount, 1_600);
    assert_eq!(
        fixture.repository.balance(USER, Some(GUILD)).unwrap(),
        Some(-1_297)
    );
    assert_eq!(
        fixture
            .repository
            .bankruptcy_penalty_games(USER, Some(GUILD))
            .unwrap(),
        5
    );
}

#[test]
fn test_settle_due_dark_bargain_missing_player_keeps_debt_active() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let id = fixture.grant_raw(
        999_999,
        GUILD,
        BuffType::DarkBargain,
        None,
        NOW - 1,
        Some(&expired_dark_data()),
    );
    let settled = service.settle_due_dark_bargains().unwrap();
    assert_eq!(settled[0].status, DarkBargainStatus::MissingPlayer);
    assert_eq!(settled[0].amount, 0);
    assert!(
        !fixture
            .repository
            .buff_by_id(id)
            .unwrap()
            .unwrap()
            .triggered
    );
}

#[test]
fn test_overgrowth_regrant_refreshes_not_extends() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let first = service.grant_overgrowth(USER, GUILD).unwrap();
    let second = service.grant_overgrowth(USER, GUILD).unwrap();
    let active = service
        .active_for(USER, GUILD, BuffType::Overgrowth)
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_ne!(first, second);
    assert_eq!(active[0].id, second);
}

#[test]
fn test_overgrowth_regrant_collapses_pre_existing_duplicates() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let leaked_a = fixture.grant_raw(
        USER,
        GUILD,
        BuffType::Overgrowth,
        None,
        NOW + 12 * HOURS,
        None,
    );
    let leaked_b = fixture.grant_raw(
        USER,
        GUILD,
        BuffType::Overgrowth,
        None,
        NOW + 12 * HOURS,
        None,
    );
    assert_eq!(
        service
            .active_for(USER, GUILD, BuffType::Overgrowth)
            .unwrap()
            .len(),
        2
    );
    let new_id = service.grant_overgrowth(USER, GUILD).unwrap();
    let active = service
        .active_for(USER, GUILD, BuffType::Overgrowth)
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, new_id);
    assert!(![leaked_a, leaked_b].contains(&new_id));
}

#[test]
fn test_sanctuary_records_both_caster_and_ally() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_sanctuary(USER, GUILD, ALLY).unwrap();
    let active = service
        .active_for(USER, GUILD, BuffType::Sanctuary)
        .unwrap();
    let row = &active[0];
    assert_eq!(row.target_id, Some(ALLY));
    assert_eq!(row.data.capacity, Some(150));
    assert_eq!(row.data.capacity_remaining, Some(150));
    assert_eq!(row.data.rate, Some(1.0));
    assert_eq!(row.data.shared, Some(true));
    assert_eq!(row.data.rolling_retroactive, Some(false));
    assert_eq!(row.data.caster_id, Some(USER));
    assert_eq!(row.data.ally_id, Some(ALLY));
    assert_eq!(row.data.protected_user_ids, vec![USER, ALLY]);
}

#[test]
fn dark_bargain_debt_and_principal_commit_together() {
    // The default penalty is charged off the debt buff alone, so a debt that
    // outlives a failed credit bills the player for money they never got.
    let fixture = Fixture::new();
    let service = fixture.service();

    // No player row: the credit cannot apply, so the debt must not stand.
    let error = service.grant_dark_bargain(USER, GUILD, 700, 800);
    assert!(error.is_err(), "an unappliable credit fails the purchase");
    assert!(
        service
            .active_for(USER, GUILD, BuffType::DarkBargain)
            .unwrap()
            .is_empty(),
        "no debt is left behind when the principal could not be paid"
    );

    fixture.register(USER, GUILD, 100);
    service.grant_dark_bargain(USER, GUILD, 700, 800).unwrap();
    assert_eq!(
        service
            .active_for(USER, GUILD, BuffType::DarkBargain)
            .unwrap()
            .len(),
        1
    );
    let balance: i64 = Connection::open(&fixture.path)
        .expect("open manashop fixture")
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![USER, GUILD],
            |row| row.get(0),
        )
        .expect("player balance");
    assert_eq!(balance, 900, "the principal is credited with the debt");
}

#[test]
fn test_overgrowth_grants_ten_dig_charges_to_the_buyer_only() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.grant_overgrowth(USER, GUILD).unwrap();
    assert_eq!(
        service
            .active_for(USER, GUILD, BuffType::Overgrowth)
            .unwrap()[0]
            .data
            .charges_remaining,
        Some(10)
    );
    assert!(
        service
            .active_for(ALLY, GUILD, BuffType::Overgrowth)
            .unwrap()
            .is_empty()
    );
}
