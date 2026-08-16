use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use cama_db::economy_event_repository::EconomyEventRepository;
use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, OptionalExtension, params};
use tempfile::TempDir;
use tokio::sync::watch;

use super::*;
use crate::gamba_guild_source::GambaDestination;

const GUILD: i64 = 42;
const OTHER_GUILD: i64 = 43;
const CHANNEL: i64 = 420;
const OTHER_CHANNEL: i64 = 430;
const TRIGGER: i64 = 1_784_394_000; // 2026-07-18 10:00 Pacific.

struct FixedWorkerClock(AtomicI64);

impl FixedWorkerClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl EconomyEventsClock for FixedWorkerClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct FakeGuilds(Vec<i64>);

impl FirstGamePoolGuildSource for FakeGuilds {
    fn live_guild_ids(&self) -> Result<Vec<i64>, String> {
        Ok(self.0.clone())
    }
}

#[derive(Clone)]
struct FakeGamba(Vec<GambaDestination>);

impl GambaGuildSource for FakeGamba {
    fn live_gamba_destinations(&self) -> Result<Vec<GambaDestination>, String> {
        Ok(self.0.clone())
    }
}

#[derive(Default)]
struct MockDiscordAnnouncements {
    calls: Mutex<Vec<(i64, i64)>>,
    outcomes: Mutex<VecDeque<Result<(), String>>>,
}

impl MockDiscordAnnouncements {
    fn with_outcomes(outcomes: impl IntoIterator<Item = Result<(), String>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }

    fn calls(&self) -> Vec<(i64, i64)> {
        self.calls.lock().expect("announcement calls lock").clone()
    }
}

#[async_trait]
impl EconomyEventAnnouncementPort for MockDiscordAnnouncements {
    async fn announce(&self, channel_id: i64, event: &EconomyEvent) -> Result<(), String> {
        self.calls
            .lock()
            .expect("announcement calls lock")
            .push((channel_id, event.event_id));
        self.outcomes
            .lock()
            .expect("announcement outcomes lock")
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

    fn seed_economy(&self, guild_id: i64) {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO players
                     (discord_id,guild_id,discord_username,jopacoin_balance)
                 VALUES (?1,?2,'economy-worker-fixture',1000)",
                params![guild_id * 100, guild_id],
            )
            .expect("seed player");
        connection
            .execute(
                "INSERT INTO nonprofit_fund (guild_id,total_collected)
                 VALUES (?1,1000)",
                [guild_id],
            )
            .expect("seed reserve");
    }

    fn event_count(&self, guild_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT COUNT(*) FROM economy_daily_events WHERE guild_id=?1",
                [guild_id],
                |row| row.get(0),
            )
            .expect("count events")
    }

    fn stamps(&self, guild_id: i64) -> Option<(Option<i64>, Option<i64>)> {
        self.connection()
            .query_row(
                "SELECT announced_at,reminder_announced_at
                 FROM economy_daily_events WHERE guild_id=?1",
                [guild_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .expect("read event stamps")
    }
}

fn config(enabled: bool) -> EconomyEventsWorkerConfig {
    EconomyEventsWorkerConfig {
        enabled,
        wake_interval: Duration::from_secs(3_600),
        event: EconomyEventConfig {
            enabled,
            trigger_hour_local: 10,
            ..EconomyEventConfig::default()
        },
    }
}

fn worker(
    path: &Path,
    guilds: Vec<i64>,
    destinations: Vec<GambaDestination>,
    announcements: Arc<dyn EconomyEventAnnouncementPort>,
    clock: Arc<dyn EconomyEventsClock>,
) -> EconomyEventsWorker {
    EconomyEventsWorker::with_clock(
        path,
        config(true),
        Arc::new(FakeGuilds(guilds)),
        Arc::new(FakeGamba(destinations)),
        announcements,
        clock,
    )
}

fn worker_context() -> (watch::Sender<bool>, WorkerContext) {
    let (sender, receiver) = watch::channel(false);
    (sender, WorkerContext::new(receiver))
}

#[test]
fn production_config_consumes_every_application_economy_value_and_gates_spec() {
    let application = ApplicationConfig::from_lookup(|name| {
        [
            ("DISCORD_BOT_TOKEN", "token"),
            ("ECONOMY_EVENTS_ENABLED", "true"),
            ("ECONOMY_EVENT_WAKE_SECONDS", "123"),
            ("ECONOMY_EVENT_TRIGGER_HOUR_LOCAL", "17"),
            ("ECONOMY_EVENT_LOOKBACK_DAYS", "11"),
            ("ECONOMY_EVENT_MAX_RESERVE_BURN_PCT", "0.07"),
            ("ECONOMY_EVENT_MAX_WALLET_BURN_PCT", "0.009"),
            ("ECONOMY_NORMAL_ANNUAL_RATE", "0.03"),
            ("ECONOMY_INFLATION_CEILING", "0.08"),
        ]
        .into_iter()
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
    })
    .expect("application config");
    let runtime = EconomyEventsWorkerConfig::from_application_config(&application);
    assert!(runtime.enabled);
    assert_eq!(runtime.wake_interval, Duration::from_secs(123));
    assert_eq!(runtime.event.trigger_hour_local, 17);
    assert_eq!(runtime.event.lookback_days, 11);
    assert_eq!(runtime.event.max_reserve_burn_pct, 0.07);
    assert_eq!(runtime.event.max_wallet_burn_pct, 0.009);
    assert_eq!(runtime.event.normal_annual_rate, 0.03);
    assert_eq!(runtime.event.inflation_ceiling, 0.08);

    let announcements: Arc<dyn EconomyEventAnnouncementPort> =
        Arc::new(MockDiscordAnnouncements::default());
    let guilds: Arc<dyn FirstGamePoolGuildSource> = Arc::new(FakeGuilds(Vec::new()));
    let gamba: Arc<dyn GambaGuildSource> = Arc::new(FakeGamba(Vec::new()));
    assert!(
        economy_events_worker_spec_with_announcement(
            "unused.db",
            config(false),
            Arc::clone(&guilds),
            Arc::clone(&gamba),
            Arc::clone(&announcements),
        )
        .is_none()
    );
    let spec = economy_events_worker_spec_with_announcement(
        "unused.db",
        config(true),
        guilds,
        gamba,
        announcements,
    )
    .expect("enabled worker spec");
    assert_eq!(spec.name, ECONOMY_EVENTS_WORKER_NAME);
}

#[tokio::test]
async fn startup_wake_activates_and_stamps_initial_delivery_on_migrated_sqlite() {
    let fixture = Fixture::migrated();
    fixture.seed_economy(GUILD);
    let announcements = Arc::new(MockDiscordAnnouncements::default());
    let worker = worker(
        &fixture.path,
        vec![GUILD],
        vec![GambaDestination {
            guild_id: GUILD,
            channel_id: CHANNEL,
        }],
        announcements.clone(),
        Arc::new(FixedWorkerClock::new(TRIGGER)),
    );

    let (_shutdown, mut context) = worker_context();
    let delay = worker.wake_once(&mut context).await.expect("startup wake");

    assert_eq!(fixture.event_count(GUILD), 1);
    assert_eq!(fixture.stamps(GUILD), Some((Some(TRIGGER), None)));
    assert_eq!(announcements.calls().len(), 1);
    assert_eq!(delay, Duration::from_secs(3_600));
}

#[tokio::test]
async fn discord_failure_after_activation_retries_without_duplicate_event_or_effects() {
    let fixture = Fixture::migrated();
    fixture.seed_economy(GUILD);
    let announcements = Arc::new(MockDiscordAnnouncements::with_outcomes([
        Err("Discord unavailable".to_owned()),
        Ok(()),
    ]));
    let worker = worker(
        &fixture.path,
        vec![GUILD],
        vec![GambaDestination {
            guild_id: GUILD,
            channel_id: CHANNEL,
        }],
        announcements.clone(),
        Arc::new(FixedWorkerClock::new(TRIGGER)),
    );
    let initial_stock = EconomyEventRepository::new(&fixture.path)
        .capture_balance_sheet(Some(GUILD))
        .expect("initial sheet")
        .monetary_stock;

    let (_shutdown, mut context) = worker_context();
    worker
        .wake_once(&mut context)
        .await
        .expect("Discord failure remains retryable");
    let activated_stock = EconomyEventRepository::new(&fixture.path)
        .capture_balance_sheet(Some(GUILD))
        .expect("activated sheet")
        .monetary_stock;
    assert_eq!(fixture.event_count(GUILD), 1);
    assert_eq!(fixture.stamps(GUILD), Some((None, None)));
    fixture
        .connection()
        .execute(
            "DELETE FROM economy_daily_snapshots WHERE guild_id=?1",
            [GUILD],
        )
        .expect("simulate crash after activation commit and before snapshot");

    worker
        .wake_once(&mut context)
        .await
        .expect("delivery recovery wake");
    assert_eq!(fixture.event_count(GUILD), 1);
    assert_eq!(fixture.stamps(GUILD), Some((Some(TRIGGER), None)));
    assert_eq!(
        fixture
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM economy_daily_snapshots WHERE guild_id=?1",
                [GUILD],
                |row| row.get::<_, i64>(0),
            )
            .expect("count recovered snapshot"),
        1
    );
    assert_eq!(announcements.calls().len(), 2);
    assert_eq!(
        EconomyEventRepository::new(&fixture.path)
            .capture_balance_sheet(Some(GUILD))
            .expect("recovered sheet")
            .monetary_stock,
        activated_stock
    );
    assert!(activated_stock <= initial_stock);
}

#[tokio::test]
async fn twelve_hour_reminder_has_its_own_durable_retry_stamp() {
    let fixture = Fixture::migrated();
    fixture.seed_economy(GUILD);
    let announcements = Arc::new(MockDiscordAnnouncements::with_outcomes([
        Ok(()),
        Err("reminder send failed".to_owned()),
        Ok(()),
    ]));
    let clock = Arc::new(FixedWorkerClock::new(TRIGGER));
    let worker = worker(
        &fixture.path,
        vec![GUILD],
        vec![GambaDestination {
            guild_id: GUILD,
            channel_id: CHANNEL,
        }],
        announcements.clone(),
        clock.clone(),
    );
    let (_shutdown, mut context) = worker_context();
    worker.wake_once(&mut context).await.expect("initial wake");
    clock.set(TRIGGER + 12 * 60 * 60);

    worker
        .wake_once(&mut context)
        .await
        .expect("failed reminder stays pending");
    assert_eq!(fixture.stamps(GUILD), Some((Some(TRIGGER), None)));
    worker
        .wake_once(&mut context)
        .await
        .expect("reminder retry");
    assert_eq!(
        fixture.stamps(GUILD),
        Some((Some(TRIGGER), Some(TRIGGER + 12 * 60 * 60)))
    );
    assert_eq!(announcements.calls().len(), 3);
}

#[tokio::test]
async fn discord_failure_is_isolated_per_guild() {
    let fixture = Fixture::migrated();
    fixture.seed_economy(GUILD);
    fixture.seed_economy(OTHER_GUILD);
    let announcements = Arc::new(MockDiscordAnnouncements::with_outcomes([
        Err("guild one unavailable".to_owned()),
        Ok(()),
    ]));
    let worker = worker(
        &fixture.path,
        vec![GUILD, OTHER_GUILD],
        vec![
            GambaDestination {
                guild_id: GUILD,
                channel_id: CHANNEL,
            },
            GambaDestination {
                guild_id: OTHER_GUILD,
                channel_id: OTHER_CHANNEL,
            },
        ],
        announcements,
        Arc::new(FixedWorkerClock::new(TRIGGER)),
    );
    let (_shutdown, mut context) = worker_context();

    worker
        .wake_once(&mut context)
        .await
        .expect("Discord failures do not stop other guilds");

    assert_eq!(fixture.stamps(GUILD), Some((None, None)));
    assert_eq!(fixture.stamps(OTHER_GUILD), Some((Some(TRIGGER), None)));
}

#[tokio::test]
async fn sqlite_failures_are_isolated_then_returned_to_the_supervisor() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("unmigrated.db");
    Connection::open(&path).expect("create unmigrated database");
    let announcements = Arc::new(MockDiscordAnnouncements::default());
    let worker = worker(
        &path,
        vec![GUILD, OTHER_GUILD],
        vec![
            GambaDestination {
                guild_id: GUILD,
                channel_id: CHANNEL,
            },
            GambaDestination {
                guild_id: OTHER_GUILD,
                channel_id: OTHER_CHANNEL,
            },
        ],
        announcements,
        Arc::new(FixedWorkerClock::new(TRIGGER)),
    );
    let (_shutdown, mut context) = worker_context();

    let error = worker
        .wake_once(&mut context)
        .await
        .expect_err("persistence failure must restart worker");

    assert!(error.contains("guild 42 activation"));
    assert!(error.contains("guild 43 activation"));
    assert!(error.contains("no such table"));
}

#[tokio::test]
async fn run_performs_startup_catch_up_before_sleep_and_cancels_promptly() {
    let fixture = Fixture::migrated();
    fixture.seed_economy(GUILD);
    let announcements = Arc::new(MockDiscordAnnouncements::default());
    let worker = worker(
        &fixture.path,
        vec![GUILD],
        vec![GambaDestination {
            guild_id: GUILD,
            channel_id: CHANNEL,
        }],
        announcements.clone(),
        Arc::new(FixedWorkerClock::new(TRIGGER)),
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(async move { worker.run(WorkerContext::new(shutdown_receiver)).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        while announcements.calls().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("immediate startup delivery");
    shutdown_sender.send(true).expect("request shutdown");
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("configured sleep is cancellable")
        .expect("worker task did not panic")
        .expect("clean worker shutdown");
}
