//! Exact Rust counterparts for `tests/test_pet_dig_integration.py`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cama_db::schema_manager::initialize_or_migrate;
use cama_domain::pet::{DIG_WORK_UNITS_PER_BLOCK, PetDigWorkClaim};
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use crate::dig_loot::{LootEntropy, SeededLootEntropy};
use crate::dig_runtime::{
    DigRuntimeCommit, DigRuntimeConfig, DigRuntimeRequest, DigRuntimeService, DigRuntimeSnapshot,
    DigRuntimeStore, DigRuntimeStoreError, DigRuntimeVersion, DigRuntimeWeather,
    SqliteDigRuntimeStore,
};

const DAY: i64 = DIG_WORK_UNITS_PER_BLOCK;
const USER_ID: i64 = 10_001;
const GUILD_ID: i64 = 777;
const T0: i64 = 1_800_000_000;

#[derive(Clone, Debug)]
enum Race {
    PetUnits { pet_id: i64, units: i64 },
    PaidCounter { date: String },
}

#[derive(Clone, Debug)]
struct TestStore {
    inner: SqliteDigRuntimeStore,
    path: PathBuf,
    race: Arc<Mutex<Option<Race>>>,
}

impl TestStore {
    fn new(path: PathBuf) -> Self {
        Self {
            inner: SqliteDigRuntimeStore::new(&path),
            path,
            race: Arc::new(Mutex::new(None)),
        }
    }

    fn arm(&self, race: Race) {
        *self.race.lock().expect("race lock") = Some(race);
    }

    fn run_race(&self) {
        let Some(race) = self.race.lock().expect("race lock").take() else {
            return;
        };
        let connection = Connection::open(&self.path).expect("race connection");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("Python foreign-key behavior");
        match race {
            Race::PetUnits { pet_id, units } => {
                connection
                    .execute(
                        "UPDATE pets SET dig_work_units=?1 WHERE pet_id=?2",
                        params![units, pet_id],
                    )
                    .expect("race pet work");
            }
            Race::PaidCounter { date } => {
                connection
                    .execute(
                        "UPDATE tunnels SET paid_dig_date=?1,paid_digs_today=1
                          WHERE discord_id=?2 AND guild_id=?3",
                        params![date, USER_ID, GUILD_ID],
                    )
                    .expect("race paid counter");
            }
        }
    }
}

impl DigRuntimeStore for TestStore {
    fn snapshot(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<DigRuntimeSnapshot, DigRuntimeStoreError> {
        self.inner.snapshot(discord_id, guild_id)
    }

    fn commit(
        &self,
        request: DigRuntimeCommit,
    ) -> Result<crate::dig_runtime::DigRuntimeCommitReceipt, DigRuntimeStoreError> {
        self.run_race();
        self.inner.commit(request)
    }

    fn ensure_weather(
        &self,
        _guild_id: i64,
        _game_date: &str,
        _now: i64,
    ) -> Result<Vec<DigRuntimeWeather>, DigRuntimeStoreError> {
        Ok(Vec::new())
    }

    fn preview_pet_dig_work(
        &self,
        discord_id: i64,
        guild_id: i64,
        now: i64,
        decay_per_day: i64,
    ) -> Result<Option<cama_domain::pet::PetDigWork>, DigRuntimeStoreError> {
        self.inner
            .preview_pet_dig_work(discord_id, guild_id, now, decay_per_day)
    }
}

struct Fixture {
    database: NamedTempFile,
    store: TestStore,
}

impl Fixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary migrated database");
        initialize_or_migrate(database.path()).expect("canonical migration");
        let store = TestStore::new(database.path().to_path_buf());
        let fixture = Self { database, store };
        fixture
            .connection()
            .execute(
                "INSERT INTO players (
                     discord_id,guild_id,discord_username,jopacoin_balance
                 ) VALUES (?1,?2,'Miner',1000)",
                params![USER_ID, GUILD_ID],
            )
            .expect("seed player");
        fixture
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("fixture connection");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("Python foreign-key behavior");
        connection
    }

    fn service(&self) -> DigRuntimeService<TestStore> {
        let config = DigRuntimeConfig {
            pet_decay_per_day: 20,
            ..DigRuntimeConfig::default()
        };
        DigRuntimeService::with_config(self.store.clone(), config)
    }

    fn seed_tunnel(&self, depth: i64, last_dig_at: i64) {
        self.connection()
            .execute(
                "INSERT INTO tunnels (
                     discord_id,guild_id,tunnel_name,depth,max_depth,total_digs,
                     last_dig_at,prestige_perks,boss_progress,boss_attempts
                 ) VALUES (?1,?2,'Pet Mine',?3,?3,1,?4,'[]','{}','{}')",
                params![USER_ID, GUILD_ID, depth, last_dig_at],
            )
            .expect("seed tunnel");
    }

    fn seed_pet(&self, hatched_at: i64, work_units: i64, work_at: i64) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO pets (
                     discord_id,guild_id,name,species,adopted_at,hatched_at,
                     adopt_fee,last_fed_at,hunger_at_last_fed,
                     dig_work_units,dig_work_at
                 ) VALUES (?1,?2,'Blep','common_cama',?3,?3,0,?3,100,?4,?5)",
                params![USER_ID, GUILD_ID, hatched_at, work_units, work_at],
            )
            .expect("seed pet");
        connection.last_insert_rowid()
    }

    fn pet_work(&self, pet_id: i64) -> (i64, i64) {
        self.connection()
            .query_row(
                "SELECT dig_work_units,dig_work_at FROM pets WHERE pet_id=?1",
                params![pet_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("pet work")
    }

    fn tunnel(&self) -> (i64, i64, Option<String>) {
        self.connection()
            .query_row(
                "SELECT depth,paid_digs_today,paid_dig_date FROM tunnels
                  WHERE discord_id=?1 AND guild_id=?2",
                params![USER_ID, GUILD_ID],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("tunnel state")
    }

    fn balance(&self) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![USER_ID, GUILD_ID],
                |row| row.get(0),
            )
            .expect("player balance")
    }
}

fn seed(now: i64, paid: bool) -> u64 {
    let mut value = USER_ID as u64;
    value = value.rotate_left(17) ^ GUILD_ID as u64;
    value = value.rotate_left(23) ^ now as u64;
    value ^ u64::from(paid)
}

fn first_dig_time_for_base(expected: i64) -> i64 {
    (T0..T0 + 10_000)
        .find(|now| SeededLootEntropy::new(seed(*now, false)).advance(3, 7) == expected)
        .expect("deterministic first-dig seed")
}

fn normal_dig_time(cave_in: bool, base_advance: Option<i64>, paid: bool) -> i64 {
    (T0..T0 + 10_000)
        .find(|now| {
            let mut entropy = SeededLootEntropy::new(seed(*now, paid));
            let caves = entropy.unit() < 0.05;
            let advance = entropy.advance(1, 3);
            caves == cave_in && base_advance.is_none_or(|expected| advance == expected)
        })
        .expect("deterministic normal-dig seed")
}

fn dig(now: i64, paid: bool) -> DigRuntimeRequest {
    DigRuntimeRequest {
        discord_id: USER_ID,
        guild_id: GUILD_ID,
        now,
        paid,
        forced_event: false,
    }
}

#[test]
fn first_dig_spends_at_most_twelve_pet_blocks() {
    let fixture = Fixture::new();
    let now = first_dig_time_for_base(5);
    let hatched_at = now - 2 * DAY;
    let pet_id = fixture.seed_pet(hatched_at, 0, hatched_at);

    let result = fixture.service().dig(dig(now, false)).expect("first dig");

    assert_eq!(result.advance, 17);
    assert_eq!(result.pet_dig_bonus, 12);
    assert_eq!(result.pet_name.as_deref(), Some("Blep"));
    assert_eq!(fixture.pet_work(pet_id), (10 * DAY + DAY / 5, now));
}

#[test]
fn pet_bonus_stops_at_boss_boundary_and_retains_the_rest() {
    let fixture = Fixture::new();
    let now = normal_dig_time(false, Some(1), false);
    fixture.seed_tunnel(20, 0);
    let pet_id = fixture.seed_pet(now, 12 * DAY, now);

    let result = fixture.service().dig(dig(now, false)).expect("boss dig");

    assert_eq!(result.depth_after, 24);
    assert_eq!(result.pet_dig_bonus, 3);
    assert_eq!(result.boss_boundary, Some(25));
    assert_eq!(fixture.pet_work(pet_id).0, 9 * DAY);
}

#[test]
fn cave_in_does_not_consume_pet_work() {
    let fixture = Fixture::new();
    let now = normal_dig_time(true, None, false);
    fixture.seed_tunnel(10, 0);
    let pet_id = fixture.seed_pet(now, 12 * DAY, now);

    let result = fixture.service().dig(dig(now, false)).expect("cave-in dig");

    assert!(result.cave_in);
    assert_eq!(result.pet_dig_bonus, 0);
    assert_eq!(fixture.pet_work(pet_id).0, 12 * DAY);
}

#[test]
fn paid_normal_dig_uses_pet_work_once() {
    let fixture = Fixture::new();
    let now = normal_dig_time(false, Some(1), true);
    fixture.seed_tunnel(10, now);
    let pet_id = fixture.seed_pet(now, 12 * DAY, now);
    let balance_before = fixture.balance();

    let result = fixture.service().dig(dig(now, true)).expect("paid dig");

    assert!(result.success);
    assert_eq!(result.pet_dig_bonus, 12);
    assert_eq!(fixture.pet_work(pet_id).0, 0);
    assert_eq!(
        fixture.balance(),
        balance_before - result.paid_dig_cost + result.jc_earned
    );
    assert_eq!(fixture.tunnel().1, 1);
}

#[test]
fn stale_pet_work_claim_rolls_back_tunnel_advance() {
    let fixture = Fixture::new();
    fixture.seed_tunnel(10, 0);
    let pet_id = fixture.seed_pet(T0, 12 * DAY, T0);
    let snapshot = fixture.store.snapshot(USER_ID, GUILD_ID).expect("snapshot");
    let mut next = snapshot.clone();
    next.tunnel.as_mut().expect("tunnel").depth = 20;

    let error = fixture
        .store
        .commit(DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: Some(PetDigWorkClaim {
                pet_id,
                expected_units: 13 * DAY,
                expected_at: T0,
                new_units: 0,
                new_at: T0,
            }),
            consume_overgrowth: false,
            depth_before: 10,
            depth_after: 20,
            jc_delta: 0,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: "{}".to_owned(),
            now: T0,
        })
        .expect_err("stale pet claim");
    assert!(matches!(error, DigRuntimeStoreError::PetWorkConflict));
    assert_eq!(fixture.tunnel().0, 10);
    assert_eq!(fixture.pet_work(pet_id).0, 12 * DAY);
}

#[test]
fn deterministic_dig_applies_pet_work() {
    let fixture = Fixture::new();
    let now = normal_dig_time(false, Some(1), false);
    fixture.seed_tunnel(10, 0);
    let pet_id = fixture.seed_pet(now, 12 * DAY, now);

    let result = fixture
        .service()
        .dig(dig(now, false))
        .expect("deterministic dig");

    assert_eq!(result.pet_dig_bonus, 12);
    assert_eq!(result.pet_name.as_deref(), Some("Blep"));
    assert_eq!(fixture.pet_work(pet_id).0, 0);
}

#[test]
fn deterministic_cave_in_does_not_consume_pet_work() {
    let fixture = Fixture::new();
    let now = normal_dig_time(true, None, false);
    fixture.seed_tunnel(10, 0);
    let pet_id = fixture.seed_pet(now, 12 * DAY, now);

    let result = fixture
        .service()
        .dig(dig(now, false))
        .expect("deterministic cave in");

    assert!(result.cave_in);
    assert_eq!(result.pet_dig_bonus, 0);
    assert_eq!(fixture.pet_work(pet_id).0, 12 * DAY);
}

#[test]
fn applied_outcome_stops_pet_work_at_boss_boundary() {
    let fixture = Fixture::new();
    fixture.seed_tunnel(20, 0);
    let pet_id = fixture.seed_pet(T0, 12 * DAY, T0);
    let snapshot = fixture.store.snapshot(USER_ID, GUILD_ID).expect("snapshot");
    let work = fixture
        .store
        .preview_pet_dig_work(USER_ID, GUILD_ID, T0, 20)
        .expect("pet preview")
        .expect("work offer");
    let mut next = snapshot.clone();
    next.tunnel.as_mut().expect("tunnel").depth = 24;

    fixture
        .store
        .commit(DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: Some(work.claim(3).expect("boss-capped claim")),
            consume_overgrowth: false,
            depth_before: 20,
            depth_after: 24,
            jc_delta: 0,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: r#"{"pet_dig_bonus":3,"boss_encounter":true}"#.to_owned(),
            now: T0,
        })
        .expect("applied boss outcome");

    assert_eq!(fixture.tunnel().0, 24);
    assert_eq!(fixture.pet_work(pet_id).0, 9 * DAY);
}

#[test]
fn applied_cave_in_does_not_consume_pet_work() {
    let fixture = Fixture::new();
    fixture.seed_tunnel(10, 0);
    let pet_id = fixture.seed_pet(T0, 12 * DAY, T0);
    let snapshot = fixture.store.snapshot(USER_ID, GUILD_ID).expect("snapshot");
    let mut next = snapshot.clone();
    next.tunnel.as_mut().expect("tunnel").depth = 8;

    fixture
        .store
        .commit(DigRuntimeCommit {
            expected: DigRuntimeVersion::from(&snapshot),
            next,
            delivery_draft: None,
            consumed_item_ids: Vec::new(),
            pet_work_claim: None,
            consume_overgrowth: false,
            depth_before: 10,
            depth_after: 8,
            jc_delta: 0,
            balance_cost: 0,
            action_type: "dig".to_owned(),
            detail: r#"{"cave_in":true,"pet_dig_bonus":0}"#.to_owned(),
            now: T0,
        })
        .expect("applied cave-in outcome");

    assert_eq!(fixture.tunnel().0, 8);
    assert_eq!(fixture.pet_work(pet_id).0, 12 * DAY);
}

#[test]
fn pet_work_race_does_not_charge_a_paid_dig() {
    let fixture = Fixture::new();
    let now = normal_dig_time(false, Some(1), true);
    fixture.seed_tunnel(10, now);
    let pet_id = fixture.seed_pet(now, 12 * DAY, now);
    let balance_before = fixture.balance();
    fixture.store.arm(Race::PetUnits {
        pet_id,
        units: 13 * DAY,
    });

    let error = fixture
        .service()
        .dig(dig(now, true))
        .expect_err("pet race must reject paid dig");

    assert!(matches!(error, DigRuntimeStoreError::PetWorkConflict));
    assert_eq!(fixture.balance(), balance_before);
    assert_eq!(fixture.tunnel().0, 10);
    assert_eq!(fixture.tunnel().1, 0);
    assert_eq!(fixture.pet_work(pet_id).0, 13 * DAY);
}

#[test]
fn paid_dig_counter_race_rolls_back_the_dig() {
    let fixture = Fixture::new();
    let now = normal_dig_time(false, Some(1), true);
    fixture.seed_tunnel(10, now);
    let balance_before = fixture.balance();
    let date = cama_domain::game_date::game_date_for_timestamp(now as f64).expect("game date");
    fixture.store.arm(Race::PaidCounter { date: date.clone() });

    let error = fixture
        .service()
        .dig(dig(now, true))
        .expect_err("paid counter race must reject dig");

    assert!(matches!(error, DigRuntimeStoreError::Conflict));
    assert_eq!(fixture.balance(), balance_before);
    let tunnel = fixture.tunnel();
    assert_eq!(tunnel.0, 10);
    assert_eq!(tunnel.1, 1);
    assert_eq!(tunnel.2.as_deref(), Some(date.as_str()));
}
