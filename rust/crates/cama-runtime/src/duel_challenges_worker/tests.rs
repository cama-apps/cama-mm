use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cama_app::ai_services::{
    AiProvider, ProviderConfig, ProviderCredential, ProviderError, ProviderErrorKind,
    ProviderLoader, ProviderRequest, ProviderResponse,
};
use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use cama_domain::guild_config::GuildConfigStore;
use rusqlite::{Connection, params};
use tempfile::TempDir;
use tokio::sync::watch;

use super::*;

const NOW: i64 = 2_000_000_000;
const DAY: i64 = 86_400;
const GUILD: i64 = 9_001;
const MAIN_CHANNEL: i64 = 900;
const ORIGIN_CHANNEL: i64 = 901;
const CHALLENGER: i64 = 41;
const RECIPIENT: i64 = 42;

struct FlavorProvider {
    result: Result<ProviderResponse, ProviderError>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl AiProvider for FlavorProvider {
    fn complete(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        self.requests
            .lock()
            .expect("provider request lock")
            .push(request);
        self.result.clone()
    }
}

struct FlavorProviderLoader(Arc<FlavorProvider>);

impl ProviderLoader for FlavorProviderLoader {
    fn load(&self) -> Result<Arc<dyn AiProvider>, ProviderError> {
        Ok(self.0.clone())
    }
}

fn ai_service(
    result: Result<ProviderResponse, ProviderError>,
) -> (Arc<AIService>, Arc<FlavorProvider>) {
    let provider = Arc::new(FlavorProvider {
        result,
        requests: Mutex::new(Vec::new()),
    });
    let config = ProviderConfig::new(
        "cerebras/test-model",
        ProviderCredential::new("test-api-key").expect("test credential"),
        Duration::from_secs(1),
        500,
    )
    .expect("test provider config");
    let loader: Arc<dyn ProviderLoader> = Arc::new(FlavorProviderLoader(Arc::clone(&provider)));
    (Arc::new(AIService::new(config, loader)), provider)
}

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

#[derive(Clone, Debug)]
struct SentMessage {
    channel_id: i64,
    message: DiscordMessage,
}

#[derive(Default)]
struct MockDiscord {
    guilds: Mutex<BTreeMap<i64, DuelGuildSnapshot>>,
    cached: Mutex<BTreeMap<i64, DuelChannelSnapshot>>,
    fetched: Mutex<BTreeMap<i64, DuelChannelSnapshot>>,
    sent: Mutex<Vec<SentMessage>>,
    edits: Mutex<Vec<(i64, i64, DiscordMessage)>>,
    fail_challenges: Mutex<BTreeSet<i64>>,
}

impl MockDiscord {
    fn text_channel(id: i64, name: &str) -> DuelChannelSnapshot {
        DuelChannelSnapshot {
            id,
            guild_id: Some(GUILD),
            name: name.to_owned(),
            kind: DuelChannelKind::Text,
            can_send: true,
        }
    }

    fn with_channels(channels: Vec<DuelChannelSnapshot>) -> Arc<Self> {
        let discord = Arc::new(Self::default());
        discord.guilds.lock().expect("guild lock").insert(
            GUILD,
            DuelGuildSnapshot {
                id: GUILD,
                text_channels: channels.clone(),
            },
        );
        discord
            .cached
            .lock()
            .expect("cache lock")
            .extend(channels.into_iter().map(|channel| (channel.id, channel)));
        discord
    }

    fn sent(&self) -> Vec<SentMessage> {
        self.sent.lock().expect("sent lock").clone()
    }
}

#[async_trait]
impl DuelDiscordPort for MockDiscord {
    async fn guild_snapshot(&self, guild_id: i64) -> Result<Option<DuelGuildSnapshot>, String> {
        Ok(self
            .guilds
            .lock()
            .expect("guild lock")
            .get(&guild_id)
            .cloned())
    }

    async fn cached_channel(&self, channel_id: i64) -> Result<Option<DuelChannelSnapshot>, String> {
        Ok(self
            .cached
            .lock()
            .expect("cache lock")
            .get(&channel_id)
            .cloned())
    }

    async fn fetch_channel(&self, channel_id: i64) -> Result<Option<DuelChannelSnapshot>, String> {
        Ok(self
            .fetched
            .lock()
            .expect("fetch lock")
            .get(&channel_id)
            .cloned())
    }

    async fn send_message(&self, channel_id: i64, message: DiscordMessage) -> Result<(), String> {
        let failed = self
            .fail_challenges
            .lock()
            .expect("failure lock")
            .iter()
            .any(|challenge_id| {
                message
                    .response
                    .content
                    .contains(&format!("Challenge #{challenge_id}"))
            });
        self.sent.lock().expect("sent lock").push(SentMessage {
            channel_id,
            message,
        });
        if failed {
            Err("mock Discord send failure".to_owned())
        } else {
            Ok(())
        }
    }

    async fn edit_message(
        &self,
        channel_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.edits
            .lock()
            .expect("edit lock")
            .push((channel_id, message_id, message));
        Ok(())
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
        let fixture = Self {
            _directory: directory,
            path,
        };
        fixture.register(CHALLENGER, 1_000);
        fixture.register(RECIPIENT, 1_000);
        fixture
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
                     (discord_id, guild_id, discord_username, jopacoin_balance,
                      glicko_rating, glicko_rd)
                 VALUES (?1, ?2, 'duel-worker-fixture', ?3, 1500, 100)",
                params![discord_id, GUILD, balance],
            )
            .expect("insert player");
    }

    fn pending_challenge(&self, expires_at: i64, next_reminder_at: i64) -> i64 {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO duel_challenges (
                     guild_id, channel_id, message_id, challenger_id, recipient_id,
                     wager, issuance_fee, status, challenger_glicko, challenger_rd,
                     recipient_glicko, recipient_rd, created_at, expires_at,
                     next_reminder_at
                 ) VALUES (
                     ?1, ?2, 777, ?3, ?4, 500, 50, 'pending', 1500, 100,
                     1500, 100, ?5, ?6, ?7
                 )",
                params![
                    GUILD,
                    ORIGIN_CHANNEL,
                    CHALLENGER,
                    RECIPIENT,
                    NOW - DAY,
                    expires_at,
                    next_reminder_at
                ],
            )
            .expect("insert pending challenge");
        connection.last_insert_rowid()
    }

    fn state(&self, challenge_id: i64) -> (String, Option<i64>) {
        self.connection()
            .query_row(
                "SELECT status, next_reminder_at FROM duel_challenges
                 WHERE challenge_id = ?1",
                [challenge_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read challenge state")
    }

    fn challenge(&self, challenge_id: i64) -> DuelChallenge {
        DuelChallengeRepository::new(&self.path)
            .get_challenge(challenge_id, Some(GUILD))
            .expect("read duel challenge")
            .expect("duel challenge exists")
    }

    fn set_ai_enabled(&self, enabled: bool) {
        GuildConfigRepository::new(&self.path, false)
            .set_ai_enabled(GUILD, enabled)
            .expect("set guild AI flag");
    }

    fn balance(&self, user_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![user_id, GUILD],
                |row| row.get(0),
            )
            .expect("read player balance")
    }
}

fn test_worker(
    path: &Path,
    configured_channel_id: Option<i64>,
    discord: Arc<MockDiscord>,
    calls: Arc<AtomicUsize>,
    wake_interval: Duration,
) -> DuelChallengesWorker {
    test_worker_at(
        path,
        configured_channel_id,
        discord,
        calls,
        wake_interval,
        NOW,
    )
}

fn test_worker_at(
    path: &Path,
    configured_channel_id: Option<i64>,
    discord: Arc<MockDiscord>,
    calls: Arc<AtomicUsize>,
    wake_interval: Duration,
    now: i64,
) -> DuelChallengesWorker {
    DuelChallengesWorker::with_runtime(
        path,
        configured_channel_id,
        discord,
        Arc::new(ProductionDuelFlavorRuntime::new(path, None, false)),
        Arc::new(FixedClock { now, calls }),
        wake_interval,
    )
}

fn worker_context() -> (watch::Sender<bool>, WorkerContext) {
    let (sender, receiver) = watch::channel(false);
    (sender, WorkerContext::new(receiver))
}

#[tokio::test]
async fn production_flavor_uses_ai_when_the_guild_flag_is_enabled() {
    let fixture = Fixture::migrated();
    fixture.set_ai_enabled(true);
    let challenge_id = fixture.pending_challenge(NOW + 2 * DAY, NOW);
    let (ai, provider) = ai_service(Ok(ProviderResponse {
        content: Some("The AI herald names the hour.".to_owned()),
        ..ProviderResponse::default()
    }));
    let flavor = ProductionDuelFlavorRuntime::new(&fixture.path, Some(ai), false)
        .generate(FlavorEvent::Reminder, &fixture.challenge(challenge_id))
        .await;

    assert_eq!(flavor, "The AI herald names the hour.");
    let requests = provider.requests.lock().expect("provider request lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].temperature, Some(0.7));
    assert!(
        requests[0]
            .messages
            .last()
            .expect("user prompt")
            .content
            .contains("Event: reminder")
    );
}

#[tokio::test]
async fn production_flavor_skips_ai_when_the_guild_flag_is_disabled() {
    let fixture = Fixture::migrated();
    fixture.set_ai_enabled(false);
    let challenge_id = fixture.pending_challenge(NOW + 2 * DAY, NOW);
    let (ai, provider) = ai_service(Ok(ProviderResponse {
        content: Some("must not be used".to_owned()),
        ..ProviderResponse::default()
    }));
    let flavor = ProductionDuelFlavorRuntime::new(&fixture.path, Some(ai), true)
        .generate(FlavorEvent::Reminder, &fixture.challenge(challenge_id))
        .await;

    assert!(fallbacks(FlavorEvent::Reminder).contains(&flavor.as_str()));
    assert!(
        provider
            .requests
            .lock()
            .expect("provider request lock")
            .is_empty()
    );
}

#[tokio::test]
async fn production_flavor_provider_failure_is_fail_soft() {
    let fixture = Fixture::migrated();
    fixture.set_ai_enabled(true);
    let challenge_id = fixture.pending_challenge(NOW + 2 * DAY, NOW);
    let (ai, provider) = ai_service(Err(ProviderError::new(
        ProviderErrorKind::Unavailable,
        "provider offline",
    )));
    let flavor = ProductionDuelFlavorRuntime::new(&fixture.path, Some(ai), false)
        .generate(FlavorEvent::Reminder, &fixture.challenge(challenge_id))
        .await;

    assert!(fallbacks(FlavorEvent::Reminder).contains(&flavor.as_str()));
    assert_eq!(
        provider
            .requests
            .lock()
            .expect("provider request lock")
            .len(),
        1
    );
}

#[tokio::test]
async fn migrated_sqlite_expiry_is_atomic_and_confirmed_after_discord_send() {
    let fixture = Fixture::migrated();
    let challenge_id = fixture.pending_challenge(NOW, NOW);
    let discord = MockDiscord::with_channels(vec![
        MockDiscord::text_channel(MAIN_CHANNEL, MAIN_CHANNEL_NAME),
        MockDiscord::text_channel(ORIGIN_CHANNEL, "gamba"),
    ]);
    let worker = test_worker(
        &fixture.path,
        None,
        Arc::clone(&discord),
        Arc::new(AtomicUsize::new(0)),
        DUEL_CHALLENGES_WAKE_INTERVAL,
    );

    let (_shutdown_sender, mut context) = worker_context();
    worker
        .wake_once(&mut context)
        .await
        .expect("expiry catch-up wake");

    assert_eq!(fixture.state(challenge_id), ("expired".to_owned(), None));
    assert_eq!(fixture.balance(CHALLENGER), 1_750);
    assert_eq!(fixture.balance(RECIPIENT), 750);
    let sent = discord.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].channel_id, MAIN_CHANNEL);
    assert!(
        sent[0]
            .message
            .response
            .content
            .contains("cowardice by silence")
    );
    assert_eq!(
        sent[0].message.allowed_mentions,
        DiscordAllowedMentions::None
    );
    let edits = discord.edits.lock().expect("edit lock");
    assert_eq!(edits.len(), 1);
    assert_eq!((edits[0].0, edits[0].1), (ORIGIN_CHANNEL, 777));
    assert_eq!(
        edits[0].2.response.embeds[0].title.as_deref(),
        Some("Challenge of Honor")
    );
    assert_eq!(
        edits[0].2.content_mode,
        crate::discord_transport::DiscordMessageContentMode::Preserve
    );
}

#[tokio::test]
async fn failed_send_rearms_reminder_and_does_not_block_later_due_items() {
    let fixture = Fixture::migrated();
    let failed_id = fixture.pending_challenge(NOW + 2 * DAY, NOW);
    let delivered_id = fixture.pending_challenge(NOW + 2 * DAY, NOW);
    let discord =
        MockDiscord::with_channels(vec![MockDiscord::text_channel(ORIGIN_CHANNEL, "gamba")]);
    discord
        .fail_challenges
        .lock()
        .expect("failure lock")
        .insert(failed_id);
    let worker = test_worker(
        &fixture.path,
        None,
        Arc::clone(&discord),
        Arc::new(AtomicUsize::new(0)),
        DUEL_CHALLENGES_WAKE_INTERVAL,
    );

    let (_shutdown_sender, mut context) = worker_context();
    worker
        .wake_once(&mut context)
        .await
        .expect("send failures remain per-item");

    assert_eq!(fixture.state(failed_id).1, Some(NOW + 600));
    assert_eq!(fixture.state(delivered_id).1, Some(NOW + DAY));
    let sent = discord.sent();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().any(|send| {
        send.message
            .response
            .content
            .contains(&format!("Challenge #{delivered_id}"))
    }));
}

#[tokio::test]
async fn failed_expiry_send_keeps_durable_marker_and_retries_without_resettling() {
    let fixture = Fixture::migrated();
    let challenge_id = fixture.pending_challenge(NOW, NOW);
    let discord =
        MockDiscord::with_channels(vec![MockDiscord::text_channel(ORIGIN_CHANNEL, "gamba")]);
    discord
        .fail_challenges
        .lock()
        .expect("failure lock")
        .insert(challenge_id);
    let worker = test_worker(
        &fixture.path,
        None,
        Arc::clone(&discord),
        Arc::new(AtomicUsize::new(0)),
        DUEL_CHALLENGES_WAKE_INTERVAL,
    );
    let (_shutdown_sender, mut context) = worker_context();

    worker
        .wake_once(&mut context)
        .await
        .expect("initial expiry settlement");
    assert_eq!(
        fixture.state(challenge_id),
        ("expired".to_owned(), Some(NOW))
    );
    assert_eq!(fixture.balance(CHALLENGER), 1_750);
    assert_eq!(fixture.balance(RECIPIENT), 750);

    worker
        .wake_once(&mut context)
        .await
        .expect("expired announcement retry claim");
    assert_eq!(
        fixture.state(challenge_id),
        ("expired".to_owned(), Some(NOW + 600))
    );
    assert_eq!(fixture.balance(CHALLENGER), 1_750);
    assert_eq!(fixture.balance(RECIPIENT), 750);

    discord
        .fail_challenges
        .lock()
        .expect("failure lock")
        .clear();
    let retry_worker = test_worker_at(
        &fixture.path,
        None,
        Arc::clone(&discord),
        Arc::new(AtomicUsize::new(0)),
        DUEL_CHALLENGES_WAKE_INTERVAL,
        NOW + 600,
    );
    retry_worker
        .wake_once(&mut context)
        .await
        .expect("successful expiry redelivery");
    assert_eq!(fixture.state(challenge_id), ("expired".to_owned(), None));
    assert_eq!(fixture.balance(CHALLENGER), 1_750);
    assert_eq!(fixture.balance(RECIPIENT), 750);
}

#[tokio::test]
async fn configured_channel_wins_and_missing_config_falls_back_to_unique_dota_mm() {
    let fixture = Fixture::migrated();
    let challenge_id = fixture.pending_challenge(NOW + 2 * DAY, NOW);
    let configured = MockDiscord::text_channel(777_777, "duel-lifecycle");
    let discord = MockDiscord::with_channels(vec![
        configured.clone(),
        MockDiscord::text_channel(MAIN_CHANNEL, "⚔️dota-mm⚔️"),
        MockDiscord::text_channel(ORIGIN_CHANNEL, "gamba"),
    ]);
    let configured_worker = test_worker(
        &fixture.path,
        Some(configured.id),
        Arc::clone(&discord),
        Arc::new(AtomicUsize::new(0)),
        DUEL_CHALLENGES_WAKE_INTERVAL,
    );

    let (_shutdown_sender, mut context) = worker_context();
    configured_worker
        .wake_once(&mut context)
        .await
        .expect("configured-channel wake");
    assert_eq!(discord.sent()[0].channel_id, configured.id);

    fixture
        .connection()
        .execute(
            "UPDATE duel_challenges SET next_reminder_at = ?1
             WHERE challenge_id = ?2",
            params![NOW, challenge_id],
        )
        .expect("make reminder due again");
    discord.sent.lock().expect("sent lock").clear();
    let fallback_worker = test_worker(
        &fixture.path,
        Some(888_888),
        Arc::clone(&discord),
        Arc::new(AtomicUsize::new(0)),
        DUEL_CHALLENGES_WAKE_INTERVAL,
    );
    let (_shutdown_sender, mut context) = worker_context();
    fallback_worker
        .wake_once(&mut context)
        .await
        .expect("name-fallback wake");
    assert_eq!(discord.sent()[0].channel_id, MAIN_CHANNEL);
}

#[tokio::test]
async fn run_catches_up_immediately_then_cancels_during_fixed_sixty_second_sleep() {
    let fixture = Fixture::migrated();
    fixture.pending_challenge(NOW + 2 * DAY, NOW);
    let discord =
        MockDiscord::with_channels(vec![MockDiscord::text_channel(ORIGIN_CHANNEL, "gamba")]);
    let clock_calls = Arc::new(AtomicUsize::new(0));
    let worker = test_worker(
        &fixture.path,
        None,
        Arc::clone(&discord),
        Arc::clone(&clock_calls),
        DUEL_CHALLENGES_WAKE_INTERVAL,
    );
    let (shutdown_sender, shutdown_receiver) = watch::channel(false);
    let task = tokio::spawn(async move { worker.run(WorkerContext::new(shutdown_receiver)).await });

    tokio::time::timeout(Duration::from_secs(5), async {
        while discord.sent().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("immediate startup catch-up before interval sleep");
    assert_eq!(clock_calls.load(Ordering::SeqCst), 1);

    shutdown_sender.send(true).expect("request worker shutdown");
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("60-second sleep cancelled promptly")
        .expect("worker task did not panic")
        .expect("clean worker shutdown");
}

#[tokio::test]
async fn sqlite_failure_is_visible_to_the_runtime_supervisor() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("unmigrated.db");
    Connection::open(&path).expect("create deliberately unmigrated database");
    let discord = MockDiscord::with_channels(Vec::new());
    let worker = test_worker(
        &path,
        None,
        discord,
        Arc::new(AtomicUsize::new(0)),
        DUEL_CHALLENGES_WAKE_INTERVAL,
    );
    let (_shutdown_sender, shutdown_receiver) = watch::channel(false);

    let error = worker
        .run(WorkerContext::new(shutdown_receiver))
        .await
        .expect_err("SQLite wake failure must reach the supervisor");
    assert!(error.contains("no such table: duel_challenges"));
}

#[test]
fn production_spec_exposes_exact_worker_name_and_interval() {
    let fixture = Fixture::migrated();
    let spec = duel_challenges_worker_spec(
        &fixture.path,
        Some(MAIN_CHANNEL),
        Arc::new(SerenityDiscordTransport::new()),
        None,
        false,
    );
    assert_eq!(spec.name, DUEL_CHALLENGES_WORKER_NAME);
    assert_eq!(DUEL_CHALLENGES_WAKE_INTERVAL, Duration::from_secs(60));
}
