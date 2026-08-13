use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use cama_db::prediction_worker_repository::{
    PredictionRefreshSettings, PredictionWorkerRepository,
};
use cama_db::predictions_repository::{NewLevel, PredictionRepository};
use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, params};
use tempfile::TempDir;
use tokio::sync::watch;

use super::*;
use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessageReceipt, DiscordMessageSnapshot,
};

const GUILD: i64 = 42;
const OTHER_GUILD: i64 = 43;
const GAMBA: i64 = 420;
const OTHER_GAMBA: i64 = 430;
const NOW: i64 = 1_783_944_000; // 2026-07-13 12:00:00 UTC.

struct FixedClock(AtomicI64);

impl FixedClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }
}

impl PredictionClock for FixedClock {
    fn now_seconds(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct FixedDrift(i64);

impl PredictionDriftSource for FixedDrift {
    fn drift(&self, minimum: i64, maximum: i64) -> i64 {
        self.0.clamp(minimum, maximum)
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
struct MockPredictionDiscord {
    edits: Mutex<Vec<(i64, i64, DiscordMessage)>>,
    summaries: Mutex<Vec<(i64, DiscordMessage)>>,
    failing_threads: Mutex<BTreeSet<i64>>,
}

impl MockPredictionDiscord {
    fn edits(&self) -> Vec<(i64, i64, DiscordMessage)> {
        self.edits.lock().expect("prediction edit lock").clone()
    }

    fn summaries(&self) -> Vec<(i64, DiscordMessage)> {
        self.summaries
            .lock()
            .expect("prediction summary lock")
            .clone()
    }
}

#[async_trait]
impl PredictionDiscordPort for MockPredictionDiscord {
    async fn edit_prediction_market_message(
        &self,
        thread_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.edits
            .lock()
            .expect("prediction edit lock")
            .push((thread_id, message_id, message));
        if self
            .failing_threads
            .lock()
            .expect("prediction failures lock")
            .contains(&thread_id)
        {
            Err("mock prediction edit failure".to_owned())
        } else {
            Ok(())
        }
    }

    async fn send_prediction_thread_message(
        &self,
        thread_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.summaries
            .lock()
            .expect("prediction summary lock")
            .push((thread_id, message));
        if self
            .failing_threads
            .lock()
            .expect("prediction failures lock")
            .contains(&thread_id)
        {
            Err("mock prediction summary failure".to_owned())
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct MockDigestDiscord {
    sends: Mutex<Vec<(u64, DiscordMessage)>>,
    outcomes: Mutex<BTreeMap<u64, VecDeque<Result<(), String>>>>,
}

impl MockDigestDiscord {
    fn sends(&self) -> Vec<(u64, DiscordMessage)> {
        self.sends.lock().expect("digest sends lock").clone()
    }

    fn fail_once(&self, channel_id: u64) {
        self.outcomes
            .lock()
            .expect("digest outcomes lock")
            .entry(channel_id)
            .or_default()
            .push_back(Err("mock digest send failure".to_owned()));
    }
}

#[async_trait]
impl DiscordTransport for MockDigestDiscord {
    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        Ok(None)
    }

    async fn send_message(
        &self,
        channel_id: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        self.sends
            .lock()
            .expect("digest sends lock")
            .push((channel_id, message));
        let outcome = self
            .outcomes
            .lock()
            .expect("digest outcomes lock")
            .get_mut(&channel_id)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Ok(()));
        outcome?;
        Ok(DiscordMessageReceipt {
            channel_id,
            message_id: 1,
            jump_url: format!("https://discord.com/channels/1/{channel_id}/1"),
        })
    }

    async fn edit_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn delete_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(1)
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(
        &self,
        _thread_id: u64,
        _name: &str,
        _locked: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
        _user_id: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn clear_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn unpin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn send_direct_message(
        &self,
        _user_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn guild_member(
        &self,
        _guild_id: u64,
        _user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        Ok(None)
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
        let connection = Connection::open(&self.path).expect("open fixture database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match runtime foreign-key policy");
        connection
    }

    fn market(&self, guild_id: i64, last_refresh: i64) -> i64 {
        let repository = PredictionRepository::new(&self.path);
        let prediction_id = repository
            .create_orderbook_prediction(
                Some(guild_id),
                99,
                &format!("Will market {guild_id} move?"),
                50,
                Some(500 + guild_id),
                &[
                    NewLevel::ask(52, 50),
                    NewLevel::ask(53, 50),
                    NewLevel::ask(54, 50),
                    NewLevel::bid(48, 50),
                    NewLevel::bid(47, 50),
                    NewLevel::bid(46, 50),
                ],
            )
            .expect("create prediction market");
        repository
            .update_discord_ids(
                prediction_id,
                Some(700 + prediction_id),
                Some(800 + prediction_id),
                Some(900 + prediction_id),
            )
            .expect("persist prediction Discord IDs");
        self.connection()
            .execute(
                "UPDATE predictions SET last_refresh_at=?1,created_at=?1
                 WHERE prediction_id=?2",
                params![last_refresh, prediction_id],
            )
            .expect("set deterministic market timestamp");
        prediction_id
    }

    fn pending_refreshes(&self) -> usize {
        PredictionWorkerRepository::new(&self.path)
            .pending_refresh_publications()
            .expect("read refresh outbox")
            .len()
    }

    fn pending_digests(&self) -> usize {
        PredictionWorkerRepository::new(&self.path)
            .pending_digest_publications()
            .expect("read digest outbox")
            .len()
    }
}

fn refresh_settings() -> PredictionRefreshSettings {
    PredictionRefreshSettings {
        refresh_seconds: 100,
        levels_per_side: 3,
        size_per_level: 10,
        outer_level_sizes: vec![8, 6, 4, 2],
        refresh_spread_ticks: 4,
        initial_spread_ticks: 2,
        tick_size: 1,
        fade_ticks: 5,
        price_low: 5,
        price_high: 95,
        initial_fair: 50,
        recent_trades_shown: 5,
        economy_events_enabled: false,
    }
}

fn refresh_worker(
    path: &Path,
    discord: Arc<dyn PredictionDiscordPort>,
    now: i64,
    wake: Duration,
) -> PredictionRefreshWorker {
    PredictionRefreshWorker::with_test_dependencies(
        path,
        refresh_settings(),
        wake,
        discord,
        Arc::new(FixedClock::new(now)),
        Arc::new(FixedDrift(0)),
    )
}

fn digest_worker(
    path: &Path,
    destinations: Vec<GambaDestination>,
    discord: Arc<dyn DiscordTransport>,
    now: i64,
    wake: Duration,
) -> PredictionDigestWorker {
    PredictionDigestWorker::with_test_dependencies(
        path,
        Arc::new(FakeGamba(destinations)),
        discord,
        100,
        12,
        wake,
        Arc::new(FixedClock::new(now)),
    )
}

fn worker_context() -> (watch::Sender<bool>, WorkerContext) {
    let (sender, receiver) = watch::channel(false);
    (sender, WorkerContext::new(receiver))
}

#[test]
fn configured_values_and_exact_worker_names_feed_production_specs() {
    let config = ApplicationConfig::from_lookup(|name| {
        [
            ("DISCORD_BOT_TOKEN", "token"),
            ("PREDICTION_CONTRACT_VALUE", "17"),
            ("PREDICTION_REFRESH_SECONDS", "321"),
            ("PREDICTION_REFRESH_WAKE_SECONDS", "17"),
            ("PREDICTION_REFRESH_LEVELS_PER_SIDE", "4"),
            ("PREDICTION_REFRESH_SIZE_PER_LEVEL", "12"),
            ("PREDICTION_REFRESH_OUTER_LEVEL_SIZES", "9,7"),
            ("PREDICTION_REFRESH_SPREAD_TICKS", "6"),
            ("PREDICTION_DIGEST_HOUR_UTC", "25"),
        ]
        .into_iter()
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
    })
    .expect("application config");
    let worker_config = PredictionWorkerConfig::from_application_config(&config);
    assert_eq!(worker_config.refresh.refresh_seconds, 321);
    assert_eq!(worker_config.refresh_wake_interval, Duration::from_secs(17));
    assert_eq!(worker_config.refresh.levels_per_side, 4);
    assert_eq!(worker_config.refresh.size_per_level, 12);
    assert_eq!(worker_config.refresh.outer_level_sizes, [9, 7]);
    assert_eq!(worker_config.refresh.refresh_spread_ticks, 6);
    assert_eq!(worker_config.contract_value, 17);
    assert_eq!(worker_config.digest_hour_utc, 1);

    let prediction: Arc<dyn PredictionDiscordPort> = Arc::new(MockPredictionDiscord::default());
    let transport: Arc<dyn DiscordTransport> = Arc::new(MockDigestDiscord::default());
    let gamba: Arc<dyn GambaGuildSource> = Arc::new(FakeGamba(Vec::new()));
    assert_eq!(
        prediction_refresh_worker_spec("unused.db", &config, prediction).name,
        PREDICTION_REFRESH_WORKER_NAME
    );
    assert_eq!(
        prediction_digest_worker_spec("unused.db", &config, gamba, transport).name,
        PREDICTION_DIGEST_WORKER_NAME
    );
    assert_eq!(PREDICTION_DIGEST_WAKE_INTERVAL, Duration::from_secs(60));
}

#[tokio::test]
async fn refresh_catches_up_due_markets_before_first_sleep_and_delivers_live_embed() {
    let fixture = Fixture::migrated();
    let prediction_id = fixture.market(GUILD, NOW - 200);
    let discord = Arc::new(MockPredictionDiscord::default());
    let worker = refresh_worker(
        &fixture.path,
        discord.clone(),
        NOW,
        Duration::from_secs(3_600),
    );

    let (_shutdown, context) = worker_context();
    worker
        .wake_once(&context)
        .await
        .expect("startup refresh wake");

    let market = PredictionRepository::new(&fixture.path)
        .get_prediction(prediction_id)
        .unwrap()
        .unwrap();
    assert_eq!(market.last_refresh_at, Some(NOW));
    assert_eq!(fixture.pending_refreshes(), 0);
    let edits = discord.edits();
    assert_eq!((edits[0].0, edits[0].1), (701, 801));
    assert_eq!(
        edits[0].2.response.embeds[0].title.as_deref(),
        Some("📈 Market #1")
    );
    assert_eq!(edits[0].2.response.attachments[0].filename, "predict_1.png");
    assert!(
        edits[0].2.response.attachments[0]
            .bytes
            .starts_with(b"\x89PNG")
    );
}

#[tokio::test]
async fn refresh_discord_failure_after_commit_retries_without_second_refresh() {
    let fixture = Fixture::migrated();
    let prediction_id = fixture.market(GUILD, NOW - 200);
    let failing = Arc::new(MockPredictionDiscord::default());
    failing
        .failing_threads
        .lock()
        .unwrap()
        .insert(700 + prediction_id);
    let first = refresh_worker(&fixture.path, failing, NOW, Duration::ZERO);
    let (_shutdown, context) = worker_context();
    assert!(first.wake_once(&context).await.is_err());
    assert_eq!(fixture.pending_refreshes(), 1);
    assert_eq!(
        PredictionRepository::new(&fixture.path)
            .get_prediction(prediction_id)
            .unwrap()
            .unwrap()
            .last_refresh_at,
        Some(NOW)
    );

    let recovered = Arc::new(MockPredictionDiscord::default());
    let retry = refresh_worker(&fixture.path, recovered.clone(), NOW, Duration::ZERO);
    let (_shutdown, context) = worker_context();
    retry.wake_once(&context).await.expect("durable retry wake");
    assert_eq!(fixture.pending_refreshes(), 0);
    assert_eq!(recovered.edits().len(), 1);
    assert_eq!(
        PredictionRepository::new(&fixture.path)
            .fair_history(prediction_id, Some(GUILD))
            .unwrap()
            .iter()
            .filter(|(timestamp, _)| *timestamp == NOW)
            .count(),
        1
    );
}

#[tokio::test]
async fn refresh_failure_isolated_per_market() {
    let fixture = Fixture::migrated();
    let first_id = fixture.market(GUILD, NOW - 200);
    let second_id = fixture.market(OTHER_GUILD, NOW - 200);
    let discord = Arc::new(MockPredictionDiscord::default());
    discord
        .failing_threads
        .lock()
        .unwrap()
        .insert(700 + first_id);
    let worker = refresh_worker(&fixture.path, discord.clone(), NOW, Duration::ZERO);
    let (_shutdown, context) = worker_context();
    assert!(worker.wake_once(&context).await.is_err());
    assert_eq!(discord.edits().len(), 2);
    assert_eq!(fixture.pending_refreshes(), 1);
    let pending = PredictionWorkerRepository::new(&fixture.path)
        .pending_refresh_publications()
        .unwrap();
    assert_eq!(pending[0].guild_id, GUILD);
    assert!(pending[0].key.contains(&first_id.to_string()));
    assert!(!pending[0].key.contains(&second_id.to_string()));
}

#[tokio::test]
async fn refresh_trade_summary_is_live_and_not_acknowledged_when_thread_send_fails() {
    let fixture = Fixture::migrated();
    let prediction_id = fixture.market(GUILD, NOW - 200);
    fixture
        .connection()
        .execute(
            "INSERT INTO prediction_trades
                 (prediction_id,discord_id,action,contracts,jopacoins,vwap_x100,
                  last_fill_price,trade_time)
             VALUES (?1,77,'buy_yes',4,20,5300,53,?2)",
            params![prediction_id, NOW - 10],
        )
        .unwrap();
    let discord = Arc::new(MockPredictionDiscord::default());
    discord
        .failing_threads
        .lock()
        .unwrap()
        .insert(700 + prediction_id);
    let worker = refresh_worker(&fixture.path, discord.clone(), NOW, Duration::ZERO);
    let (_shutdown, context) = worker_context();
    assert!(worker.wake_once(&context).await.is_err());
    assert_eq!(discord.summaries().len(), 1);
    assert!(
        discord.summaries()[0]
            .1
            .response
            .content
            .contains("Daily refresh")
    );
    assert_eq!(fixture.pending_refreshes(), 2);
}

#[test]
fn digest_slot_is_twice_daily_and_strictly_latest_for_startup_catch_up() {
    assert_eq!(latest_digest_slot(NOW, 12).unwrap(), NOW);
    assert_eq!(latest_digest_slot(NOW - 1, 12).unwrap(), NOW - 12 * 60 * 60);
    assert_eq!(
        latest_digest_slot(NOW + 12 * 60 * 60, 12).unwrap(),
        NOW + 12 * 60 * 60
    );
}

#[tokio::test]
async fn digest_catches_up_immediately_and_posts_live_guild_channel_with_png() {
    let fixture = Fixture::migrated();
    let prediction_id = fixture.market(GUILD, NOW - 200);
    fixture
        .connection()
        .execute(
            "UPDATE predictions SET prev_price=50,current_price=58 WHERE prediction_id=?1",
            [prediction_id],
        )
        .unwrap();
    let discord = Arc::new(MockDigestDiscord::default());
    let worker = digest_worker(
        &fixture.path,
        vec![GambaDestination {
            guild_id: GUILD,
            channel_id: GAMBA,
        }],
        discord.clone(),
        NOW,
        Duration::from_secs(60),
    );
    let (_shutdown, context) = worker_context();
    worker
        .wake_once(&context)
        .await
        .expect("startup digest catch-up");

    assert_eq!(fixture.pending_digests(), 0);
    let sends = discord.sends();
    assert_eq!(sends[0].0, GAMBA as u64);
    assert_eq!(
        sends[0].1.response.embeds[0].title.as_deref(),
        Some("📈 Today in prediction markets")
    );
    assert!(
        sends[0].1.response.embeds[0]
            .description
            .as_deref()
            .unwrap()
            .contains("Biggest mover")
    );
    assert!(
        sends[0].1.response.attachments[0]
            .bytes
            .starts_with(b"\x89PNG")
    );
}

#[tokio::test]
async fn digest_failure_after_queue_retries_and_other_guild_still_posts() {
    let fixture = Fixture::migrated();
    fixture.market(GUILD, NOW - 200);
    fixture.market(OTHER_GUILD, NOW - 200);
    let discord = Arc::new(MockDigestDiscord::default());
    discord.fail_once(GAMBA as u64);
    let destinations = vec![
        GambaDestination {
            guild_id: GUILD,
            channel_id: GAMBA,
        },
        GambaDestination {
            guild_id: OTHER_GUILD,
            channel_id: OTHER_GAMBA,
        },
    ];
    let first = digest_worker(
        &fixture.path,
        destinations.clone(),
        discord.clone(),
        NOW,
        Duration::ZERO,
    );
    let (_shutdown, context) = worker_context();
    assert!(first.wake_once(&context).await.is_err());
    assert_eq!(fixture.pending_digests(), 1);
    assert_eq!(
        discord
            .sends()
            .iter()
            .map(|send| send.0)
            .collect::<Vec<_>>(),
        [420, 430]
    );

    let retry = digest_worker(
        &fixture.path,
        destinations,
        discord.clone(),
        NOW,
        Duration::ZERO,
    );
    let (_shutdown, context) = worker_context();
    retry
        .wake_once(&context)
        .await
        .expect("retry queued digest");
    assert_eq!(fixture.pending_digests(), 0);
    assert_eq!(
        discord
            .sends()
            .iter()
            .filter(|send| send.0 == GAMBA as u64)
            .count(),
        2
    );
    assert_eq!(
        discord
            .sends()
            .iter()
            .filter(|send| send.0 == OTHER_GAMBA as u64)
            .count(),
        1
    );
}

#[tokio::test]
async fn digest_attachment_failure_retries_embed_only_without_losing_publication() {
    let fixture = Fixture::migrated();
    let prediction_id = fixture.market(GUILD, NOW - 200);
    fixture
        .connection()
        .execute(
            "UPDATE predictions SET prev_price=50,current_price=58 WHERE prediction_id=?1",
            [prediction_id],
        )
        .unwrap();
    let discord = Arc::new(MockDigestDiscord::default());
    discord.fail_once(GAMBA as u64);
    let worker = digest_worker(
        &fixture.path,
        vec![GambaDestination {
            guild_id: GUILD,
            channel_id: GAMBA,
        }],
        discord.clone(),
        NOW,
        Duration::ZERO,
    );
    let (_shutdown, context) = worker_context();
    worker
        .wake_once(&context)
        .await
        .expect("embed-only fallback succeeds");

    assert_eq!(fixture.pending_digests(), 0);
    let sends = discord.sends();
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0].1.response.attachments.len(), 1);
    assert!(sends[1].1.response.attachments.is_empty());
}

#[tokio::test]
async fn both_workers_cancel_promptly_from_long_sleep() {
    let fixture = Fixture::migrated();
    let refresh = refresh_worker(
        &fixture.path,
        Arc::new(MockPredictionDiscord::default()),
        NOW,
        Duration::from_secs(3_600),
    );
    let (refresh_shutdown, receiver) = watch::channel(false);
    let refresh_task = tokio::spawn(async move { refresh.run(WorkerContext::new(receiver)).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    refresh_shutdown.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), refresh_task)
        .await
        .expect("refresh cancellation")
        .unwrap()
        .unwrap();

    let digest = digest_worker(
        &fixture.path,
        Vec::new(),
        Arc::new(MockDigestDiscord::default()),
        NOW,
        Duration::from_secs(3_600),
    );
    let (digest_shutdown, receiver) = watch::channel(false);
    let digest_task = tokio::spawn(async move { digest.run(WorkerContext::new(receiver)).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    digest_shutdown.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), digest_task)
        .await
        .expect("digest cancellation")
        .unwrap()
        .unwrap();
}
