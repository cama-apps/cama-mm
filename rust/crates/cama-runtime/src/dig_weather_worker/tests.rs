use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, params};
use tempfile::TempDir;
use tokio::sync::{Notify, watch};

use super::*;
use crate::gamba_guild_source::GambaDestination;

const NOW: i64 = 2_000_000_000;
const TODAY: &str = "2033-05-18";
const TOMORROW: &str = "2033-05-19";
const CONFIGURED: i64 = 501;

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

    fn weather_count(&self, guild_id: i64, game_date: &str) -> i64 {
        Connection::open(&self.path)
            .expect("open fixture")
            .query_row(
                "SELECT COUNT(*) FROM dig_weather WHERE guild_id=?1 AND game_date=?2",
                params![guild_id, game_date],
                |row| row.get(0),
            )
            .expect("count weather")
    }
}

struct FakeGuilds(Vec<i64>);

impl FirstGamePoolGuildSource for FakeGuilds {
    fn live_guild_ids(&self) -> Result<Vec<i64>, String> {
        Ok(self.0.clone())
    }
}

struct FakeGamba(Vec<GambaDestination>);

impl GambaGuildSource for FakeGamba {
    fn live_gamba_destinations(&self) -> Result<Vec<GambaDestination>, String> {
        Ok(self.0.clone())
    }
}

struct SequenceClock {
    dates: Mutex<VecDeque<String>>,
    last: Mutex<String>,
}

impl SequenceClock {
    fn new(dates: impl IntoIterator<Item = &'static str>) -> Self {
        let mut dates = dates
            .into_iter()
            .map(str::to_owned)
            .collect::<VecDeque<_>>();
        let last = dates.front().cloned().unwrap_or_else(|| TODAY.to_owned());
        Self {
            dates: Mutex::new(std::mem::take(&mut dates)),
            last: Mutex::new(last),
        }
    }
}

impl DigWeatherClock for SequenceClock {
    fn game_date(&self) -> String {
        let next = self.dates.lock().expect("clock lock").pop_front();
        if let Some(next) = next {
            *self.last.lock().expect("clock last lock") = next;
        }
        self.last.lock().expect("clock last lock").clone()
    }

    fn now(&self) -> i64 {
        NOW
    }
}

#[derive(Default)]
struct FakeDiscord {
    configured_guilds: BTreeSet<i64>,
    failures: BTreeSet<i64>,
    sends: Mutex<Vec<(i64, Vec<DigWeatherEntry>)>>,
    entered: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
}

#[async_trait]
impl DigWeatherDiscordPort for FakeDiscord {
    async fn configured_channel(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<Option<i64>, String> {
        Ok(
            (channel_id == CONFIGURED && self.configured_guilds.contains(&guild_id))
                .then_some(channel_id),
        )
    }

    async fn announce(&self, channel_id: i64, weather: &[DigWeatherEntry]) -> Result<(), String> {
        if let Some(entered) = &self.entered {
            entered.notify_one();
        }
        if let Some(release) = &self.release {
            release.notified().await;
        }
        self.sends
            .lock()
            .expect("send lock")
            .push((channel_id, weather.to_vec()));
        if self.failures.contains(&channel_id) {
            Err(format!("channel {channel_id} rejected send"))
        } else {
            Ok(())
        }
    }
}

fn test_worker(
    path: &Path,
    guilds: Vec<i64>,
    fallback: BTreeMap<i64, i64>,
    discord: Arc<FakeDiscord>,
    clock: Arc<dyn DigWeatherClock>,
    interval: Duration,
) -> DigWeatherWorker {
    let guild_source: Arc<dyn FirstGamePoolGuildSource> = Arc::new(FakeGuilds(guilds));
    let gamba_source: Arc<dyn GambaGuildSource> = Arc::new(FakeGamba(
        fallback
            .into_iter()
            .map(|(guild_id, channel_id)| GambaDestination {
                guild_id,
                channel_id,
            })
            .collect(),
    ));
    let weather_discord: Arc<dyn DigWeatherDiscordPort> = discord;
    DigWeatherWorker::with_clock_and_interval(
        path,
        Some(CONFIGURED),
        guild_source,
        gamba_source,
        weather_discord,
        clock,
        interval,
    )
}

#[tokio::test]
async fn startup_suppresses_current_day_then_rollover_broadcasts_once() {
    let fixture = Fixture::migrated();
    let discord = Arc::new(FakeDiscord::default());
    let clock: Arc<dyn DigWeatherClock> = Arc::new(SequenceClock::new([
        TODAY, TODAY, TOMORROW, TOMORROW, TOMORROW,
    ]));
    let worker = test_worker(
        &fixture.path,
        vec![1],
        BTreeMap::from([(1, 601)]),
        Arc::clone(&discord),
        clock,
        Duration::from_millis(10),
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(async move { worker.run(WorkerContext::new(shutdown_receiver)).await });

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if discord.sends.lock().expect("send lock").len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("rollover broadcast");
    assert_eq!(fixture.weather_count(1, TODAY), 0);
    assert_eq!(fixture.weather_count(1, TOMORROW), 2);
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(discord.sends.lock().expect("send lock").len(), 1);

    shutdown_sender.send(true).expect("request shutdown");
    task.await
        .expect("worker task did not panic")
        .expect("clean shutdown");
}

#[tokio::test]
async fn configured_route_wins_and_fallback_and_send_failures_are_isolated() {
    let fixture = Fixture::migrated();
    let discord = Arc::new(FakeDiscord {
        configured_guilds: BTreeSet::from([1]),
        failures: BTreeSet::from([602]),
        ..FakeDiscord::default()
    });
    let clock: Arc<dyn DigWeatherClock> = Arc::new(SequenceClock::new([TOMORROW]));
    let worker = test_worker(
        &fixture.path,
        vec![3, 2, 1, 4],
        BTreeMap::from([(1, 601), (2, 602), (3, 603)]),
        Arc::clone(&discord),
        clock,
        Duration::from_secs(600),
    );
    let (_shutdown_sender, shutdown_receiver) = watch::channel(false);
    let mut context = WorkerContext::new(shutdown_receiver);

    worker
        .broadcast_rollover(TOMORROW, &mut context)
        .await
        .expect("guild failures remain isolated");

    let channels = discord
        .sends
        .lock()
        .expect("send lock")
        .iter()
        .map(|(channel, _)| *channel)
        .collect::<Vec<_>>();
    assert_eq!(channels, vec![CONFIGURED, 602, 603]);
    for guild_id in 1..=4 {
        assert_eq!(fixture.weather_count(guild_id, TOMORROW), 2);
    }
}

#[tokio::test]
async fn cancellation_interrupts_in_flight_delivery_before_next_guild() {
    let fixture = Fixture::migrated();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let discord = Arc::new(FakeDiscord {
        entered: Some(Arc::clone(&entered)),
        release: Some(release),
        ..FakeDiscord::default()
    });
    let clock: Arc<dyn DigWeatherClock> = Arc::new(SequenceClock::new([TOMORROW]));
    let worker = test_worker(
        &fixture.path,
        vec![1, 2],
        BTreeMap::from([(1, 601), (2, 602)]),
        Arc::clone(&discord),
        clock,
        Duration::from_secs(600),
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let mut context = WorkerContext::new(shutdown_receiver);
    let task = tokio::spawn(async move { worker.broadcast_rollover(TOMORROW, &mut context).await });

    entered.notified().await;
    shutdown_sender.send(true).expect("request shutdown");
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("delivery cancellation is prompt")
        .expect("worker task did not panic")
        .expect("clean cancellation");
    assert!(discord.sends.lock().expect("send lock").is_empty());
    assert_eq!(fixture.weather_count(1, TOMORROW), 2);
    assert_eq!(fixture.weather_count(2, TOMORROW), 0);
}

#[test]
fn embed_copy_and_production_constants_match_python() {
    let weather = vec![
        DigWeatherEntry {
            guild_id: 1,
            game_date: TOMORROW.to_owned(),
            layer_name: "Dirt".to_owned(),
            weather_id: "earthworm_migration".to_owned(),
        },
        DigWeatherEntry {
            guild_id: 1,
            game_date: TOMORROW.to_owned(),
            layer_name: "Magma".to_owned(),
            weather_id: "eruption".to_owned(),
        },
    ];
    let embed = weather_embed(&weather).expect("known weather embed");

    assert_eq!(DIG_WEATHER_WORKER_NAME, "dig_weather_broadcast");
    assert_eq!(DIG_WEATHER_WAKE_INTERVAL, Duration::from_secs(600));
    assert_eq!(embed.title.as_deref(), Some("⛅ Daily Layer Weather"));
    assert_eq!(
        embed.description.as_deref(),
        Some("New conditions have settled across the depths.")
    );
    assert_eq!(embed.color, Some(0x58_65_F2));
    assert_eq!(embed.fields[0].name, "Dirt — Earthworm Migration");
    assert_eq!(
        embed.footer.as_deref(),
        Some("Weather affects all diggers in that layer today. Use /dig weather for details.")
    );
}

#[tokio::test]
async fn pre_cancelled_worker_never_reads_clock_or_sqlite() {
    struct CountingClock(AtomicUsize);
    impl DigWeatherClock for CountingClock {
        fn game_date(&self) -> String {
            self.0.fetch_add(1, Ordering::SeqCst);
            TODAY.to_owned()
        }
        fn now(&self) -> i64 {
            NOW
        }
    }

    let fixture = Fixture::migrated();
    let clock = Arc::new(CountingClock(AtomicUsize::new(0)));
    let worker = test_worker(
        &fixture.path,
        vec![1],
        BTreeMap::from([(1, 601)]),
        Arc::new(FakeDiscord::default()),
        clock.clone(),
        DIG_WEATHER_WAKE_INTERVAL,
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    shutdown_sender.send(true).expect("pre-cancel worker");

    worker
        .run(WorkerContext::new(shutdown_receiver))
        .await
        .expect("clean pre-start cancellation");
    assert_eq!(clock.0.load(Ordering::SeqCst), 0);
}
