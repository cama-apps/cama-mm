use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, params};
use tempfile::TempDir;
use tokio::sync::watch;

use super::*;

const NOW: i64 = 2_000_000_000;
const USER: i64 = 31_337;
const GUILD: i64 = 9_001;

struct FixedClock {
    now: i64,
    calls: Arc<AtomicUsize>,
}

impl UnixClock for FixedClock {
    fn now(&self) -> i64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.now
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

    fn register(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players
                     (discord_id, guild_id, discord_username, jopacoin_balance)
                 VALUES (?1, ?2, 'worker-fixture', ?3)",
                params![discord_id, GUILD, balance],
            )
            .expect("insert player");
    }

    fn debt(&self, discord_id: i64, expires_at: i64) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO manashop_buffs (
                     discord_id, guild_id, buff_type, granted_at,
                     expires_at, triggered, data
                 ) VALUES (?1, ?2, 'dark_bargain', ?3, ?4, 0, ?5)",
                params![
                    discord_id,
                    GUILD,
                    expires_at - 86_400,
                    expires_at,
                    r#"{"amount_due":700,"default_penalty":1600,"default_penalty_games":5}"#,
                ],
            )
            .expect("insert Dark Bargain debt");
        connection.last_insert_rowid()
    }

    fn player_balance(&self, discord_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![discord_id, GUILD],
                |row| row.get(0),
            )
            .expect("read player balance")
    }

    fn triggered(&self, buff_id: i64) -> bool {
        self.connection()
            .query_row(
                "SELECT triggered FROM manashop_buffs WHERE id = ?1",
                params![buff_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("read debt state")
            != 0
    }
}

fn test_worker(path: &Path, calls: Arc<AtomicUsize>, interval: Duration) -> ManashopDebtWorker {
    ManashopDebtWorker::with_clock_and_interval(
        path,
        Arc::new(FixedClock { now: NOW, calls }),
        interval,
    )
}

#[tokio::test]
async fn migrated_database_settlement_is_due_only_and_idempotent() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 1_100);
    let due = fixture.debt(USER, NOW);
    let future = fixture.debt(USER, NOW + 1);
    let worker = test_worker(
        &fixture.path,
        Arc::new(AtomicUsize::new(0)),
        Duration::from_secs(3_600),
    );

    let first = worker.settle_once().await.expect("first settlement wake");
    let second = worker.settle_once().await.expect("idempotent retry wake");

    assert_eq!(first.len(), 1);
    assert!(second.is_empty());
    assert_eq!(fixture.player_balance(USER), 400);
    assert!(fixture.triggered(due));
    assert!(!fixture.triggered(future));
}

#[tokio::test]
async fn run_catches_up_immediately_then_cancels_cleanly_during_hourly_sleep() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 1_100);
    let due = fixture.debt(USER, NOW);
    let clock_calls = Arc::new(AtomicUsize::new(0));
    let worker = test_worker(
        &fixture.path,
        Arc::clone(&clock_calls),
        MANASHOP_DEBT_WAKE_INTERVAL,
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(async move { worker.run(WorkerContext::new(shutdown_receiver)).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture.triggered(due) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("startup catch-up before hourly sleep");
    assert_eq!(fixture.player_balance(USER), 400);
    assert_eq!(clock_calls.load(Ordering::SeqCst), 1);

    shutdown_sender.send(true).expect("request worker shutdown");
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("hourly sleep cancelled promptly")
        .expect("worker task did not panic")
        .expect("clean worker shutdown");
}

#[tokio::test]
async fn cancelled_settlement_worker_restart_does_not_duplicate_durable_debit() {
    let fixture = Fixture::migrated();
    fixture.register(USER, 1_100);
    let due = fixture.debt(USER, NOW);
    let clock_calls = Arc::new(AtomicUsize::new(0));
    let worker = test_worker(
        &fixture.path,
        Arc::clone(&clock_calls),
        MANASHOP_DEBT_WAKE_INTERVAL,
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(async move { worker.run(WorkerContext::new(shutdown_receiver)).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture.triggered(due) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("startup settlement before cancellation");
    shutdown_sender
        .send(true)
        .expect("cancel settlement worker after durable commit");
    task.await
        .expect("worker task did not panic")
        .expect("clean worker cancellation");

    let restarted = test_worker(
        &fixture.path,
        Arc::clone(&clock_calls),
        MANASHOP_DEBT_WAKE_INTERVAL,
    );
    let retry = restarted
        .settle_once()
        .await
        .expect("restart settlement wake");

    assert!(retry.is_empty());
    assert_eq!(fixture.player_balance(USER), 400);
    assert!(fixture.triggered(due));
    assert_eq!(clock_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn shutdown_requested_before_start_is_clean_and_skips_sqlite() {
    let fixture = Fixture::migrated();
    let clock_calls = Arc::new(AtomicUsize::new(0));
    let worker = test_worker(
        &fixture.path,
        Arc::clone(&clock_calls),
        MANASHOP_DEBT_WAKE_INTERVAL,
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    shutdown_sender.send(true).expect("request shutdown");

    worker
        .run(WorkerContext::new(shutdown_receiver))
        .await
        .expect("clean pre-start shutdown");
    assert_eq!(clock_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sqlite_failure_returns_error_for_supervisor_restart() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("unmigrated.db");
    Connection::open(&path).expect("create deliberately unmigrated database");
    let worker = test_worker(
        &path,
        Arc::new(AtomicUsize::new(0)),
        MANASHOP_DEBT_WAKE_INTERVAL,
    );
    let (_shutdown_sender, shutdown_receiver) = watch::channel(false);

    let error = worker
        .run(WorkerContext::new(shutdown_receiver))
        .await
        .expect_err("per-wake SQLite error must reach the supervisor");
    assert!(error.contains("no such table: manashop_buffs"));
}

#[test]
fn production_spec_uses_the_exact_cutover_worker_name_and_hourly_interval() {
    let fixture = Fixture::migrated();
    let spec = manashop_debt_worker_spec(&fixture.path);
    assert_eq!(spec.name, MANASHOP_DEBT_WORKER_NAME);
    assert_eq!(MANASHOP_DEBT_WAKE_INTERVAL, Duration::from_secs(3_600));
}
