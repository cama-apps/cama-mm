use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cama_db::dota_bet_seed_repository::{DotaBetSeedRepository, FirstGamePoolBalances};
use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, params};
use tempfile::TempDir;
use tokio::sync::watch;

use super::*;

const GUILD: i64 = 42;

struct FixedClock(&'static str);

impl GameDateClock for FixedClock {
    fn game_date(&self) -> String {
        self.0.to_owned()
    }
}

#[derive(Clone)]
struct FakeGuildSource {
    guild_ids: Vec<i64>,
}

impl FirstGamePoolGuildSource for FakeGuildSource {
    fn live_guild_ids(&self) -> Result<Vec<i64>, String> {
        Ok(self.guild_ids.clone())
    }
}

#[derive(Default)]
struct FakeDisplay {
    calls: Mutex<Vec<i64>>,
    outcomes: Mutex<VecDeque<Result<(), String>>>,
}

impl FakeDisplay {
    fn with_outcomes(outcomes: impl IntoIterator<Item = Result<(), String>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }

    fn calls(&self) -> Vec<i64> {
        self.calls.lock().expect("display calls lock").clone()
    }
}

#[async_trait]
impl FirstGamePoolDisplayPort for FakeDisplay {
    async fn refresh_first_game_pool_lobbies(&self, guild_id: i64) -> Result<(), String> {
        self.calls
            .lock()
            .expect("display calls lock")
            .push(guild_id);
        self.outcomes
            .lock()
            .expect("display outcomes lock")
            .pop_front()
            .unwrap_or(Ok(()))
    }
}

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
}

impl Fixture {
    fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("cama.db");
        initialize_or_migrate_with_settings(&path, &MigrationSettings::default())
            .expect("migrate temporary database");
        Self {
            _directory: directory,
            path,
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open temporary database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match production SQLite foreign-key mode");
        connection
    }

    fn fund(&self, guild_id: i64, amount: i64) {
        self.connection()
            .execute(
                "INSERT INTO nonprofit_fund (guild_id, total_collected)
                 VALUES (?1, ?2)
                 ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
                params![guild_id, amount],
            )
            .expect("seed nonprofit fund");
    }

    fn repository(&self) -> DotaBetSeedRepository {
        DotaBetSeedRepository::new(&self.path)
    }
}

fn worker(
    path: &Path,
    guild_ids: Vec<i64>,
    display: Arc<dyn FirstGamePoolDisplayPort>,
    game_date: &'static str,
) -> FirstGamePoolWorker {
    FirstGamePoolWorker::with_clock_and_intervals(
        path,
        100,
        Arc::new(FakeGuildSource { guild_ids }),
        display,
        Arc::new(FixedClock(game_date)),
        FIRST_GAME_POOL_WAKE_INTERVAL,
        Duration::ZERO,
    )
}

fn worker_context() -> (watch::Sender<bool>, WorkerContext) {
    let (sender, receiver) = watch::channel(false);
    (sender, WorkerContext::new(receiver))
}

#[test]
fn production_spec_is_conditional_and_uses_exact_cutover_name_and_interval() {
    let fixture = Fixture::migrated();
    let guilds: Arc<dyn FirstGamePoolGuildSource> = Arc::new(FakeGuildSource {
        guild_ids: Vec::new(),
    });
    let display: Arc<dyn FirstGamePoolDisplayPort> = Arc::new(FakeDisplay::default());

    assert!(
        first_game_pool_worker_spec(&fixture.path, 0, Arc::clone(&guilds), Arc::clone(&display))
            .is_none()
    );
    let spec = first_game_pool_worker_spec(&fixture.path, 100, guilds, display)
        .expect("positive amount enables worker");
    assert_eq!(spec.name, FIRST_GAME_POOL_WORKER_NAME);
    assert_eq!(FIRST_GAME_POOL_WAKE_INTERVAL, Duration::from_secs(900));
}

#[tokio::test]
async fn migrated_database_wake_catches_up_missed_dates_immediately() {
    let fixture = Fixture::migrated();
    fixture.fund(GUILD, 600);
    fixture
        .repository()
        .fund_first_game_pools(Some(GUILD), "2026-08-03", 100)
        .expect("seed prior processed date");
    let display = Arc::new(FakeDisplay::default());
    let worker = worker(&fixture.path, vec![GUILD], display.clone(), "2026-08-05");

    let (_shutdown, mut context) = worker_context();
    worker
        .wake_once(&mut context)
        .await
        .expect("startup catch-up wake");

    assert_eq!(
        fixture.repository().pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances {
            open: 300,
            low_skill: 300
        }
    );
    assert_eq!(
        fixture.repository().nonprofit_balance(Some(GUILD)).unwrap(),
        0
    );
    assert_eq!(display.calls(), [GUILD]);
    assert!(
        fixture
            .repository()
            .pending_first_game_pool_refreshes()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn discord_failure_after_commit_is_recovered_without_double_funding() {
    let fixture = Fixture::migrated();
    fixture.fund(GUILD, 200);
    let failed_display = Arc::new(FakeDisplay::with_outcomes([
        Err("Discord unavailable".to_owned()),
        Err("Discord unavailable".to_owned()),
    ]));
    let first = worker(&fixture.path, vec![GUILD], failed_display, "2026-08-05");

    let (_shutdown, mut context) = worker_context();
    let error = first
        .wake_once(&mut context)
        .await
        .expect_err("display failure reaches supervisor");
    assert!(error.contains("display generation 2026-08-05"));
    assert_eq!(
        fixture.repository().pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances {
            open: 100,
            low_skill: 100
        }
    );
    assert_eq!(
        fixture
            .repository()
            .pending_first_game_pool_refreshes()
            .unwrap()
            .len(),
        1
    );

    let recovered_display = Arc::new(FakeDisplay::default());
    let retry = worker(
        &fixture.path,
        vec![GUILD],
        recovered_display.clone(),
        "2026-08-05",
    );
    let (_shutdown, mut context) = worker_context();
    retry
        .wake_once(&mut context)
        .await
        .expect("durable outbox retry");

    assert_eq!(recovered_display.calls(), [GUILD]);
    assert_eq!(
        fixture.repository().pool_balances(Some(GUILD)).unwrap(),
        FirstGamePoolBalances {
            open: 100,
            low_skill: 100
        }
    );
    assert!(
        fixture
            .repository()
            .pending_first_game_pool_refreshes()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn display_failure_isolated_by_guild_and_only_failed_outbox_remains() {
    let fixture = Fixture::migrated();
    fixture.fund(GUILD, 200);
    fixture.fund(GUILD + 1, 200);
    let display = Arc::new(FakeDisplay::with_outcomes([
        Err("first attempt".to_owned()),
        Err("second attempt".to_owned()),
        Ok(()),
    ]));
    let worker = worker(
        &fixture.path,
        vec![GUILD + 1, GUILD, GUILD],
        display.clone(),
        "2026-08-05",
    );

    let (_shutdown, mut context) = worker_context();
    worker
        .wake_once(&mut context)
        .await
        .expect_err("one durable refresh remains failed");

    assert_eq!(display.calls(), [GUILD, GUILD, GUILD + 1]);
    assert_eq!(
        fixture
            .repository()
            .pending_first_game_pool_refreshes()
            .unwrap()
            .into_iter()
            .map(|refresh| refresh.guild_id)
            .collect::<Vec<_>>(),
        [GUILD]
    );
}

#[tokio::test]
async fn run_catches_up_before_first_sleep_and_cancels_promptly() {
    let fixture = Fixture::migrated();
    fixture.fund(GUILD, 200);
    let display = Arc::new(FakeDisplay::default());
    let worker = worker(&fixture.path, vec![GUILD], display.clone(), "2026-08-05");
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(async move { worker.run(WorkerContext::new(shutdown_receiver)).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !display.calls().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("immediate startup wake");
    shutdown_sender.send(true).expect("request worker shutdown");
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("900-second sleep cancelled promptly")
        .expect("worker task did not panic")
        .expect("clean worker shutdown");
}

#[tokio::test]
async fn unmigrated_sqlite_failure_reaches_supervisor() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("unmigrated.db");
    Connection::open(&path).expect("create unmigrated database");
    let worker = worker(
        &path,
        vec![GUILD],
        Arc::new(FakeDisplay::default()),
        "2026-08-05",
    );

    let (_shutdown, mut context) = worker_context();
    let error = worker
        .wake_once(&mut context)
        .await
        .expect_err("SQLite failure must restart worker");
    assert!(error.contains("no such table"));
}
