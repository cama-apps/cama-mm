use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};

use cama_app::ai_services::Value as AiValue;
use cama_app::pet::{Clock, SeededPetRandom};
use cama_app::pet_commands::PetCommandService;
use cama_app::pet_flavor::LINE_TOOL_NAME;
use cama_app::pet_sqlite::SqlitePetCommandService;
use cama_db::core_repositories::{NewPlayer, PlayerRepository};
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_db::pet_brawl_repository::PetBrawlRepository;
use cama_db::pet_eating_repository::{EatAdultPetRequest, PetEatingRepository};
use cama_db::pet_repository::PetRepository;
use cama_db::schema_manager::initialize_or_migrate;
use cama_domain::guild_config::GuildConfigStore;
use cama_domain::pet::{ADULT_AGE_SECONDS, EGG_HATCH_SECONDS, Pet};
use rusqlite::{Connection, params};
use tempfile::{NamedTempFile, TempDir};
use tokio::sync::{Notify, watch};

use crate::discord_transport::DiscordAllowedMentions;
use crate::pet_death_delivery::DirectDeathDeliveryGuard;

use super::*;

const GUILD: i64 = 70_701;
const OTHER_GUILD: i64 = 70_702;
const CHANNEL: i64 = 80_801;

#[derive(Clone)]
enum TestAiOutcome {
    Line(String),
    Failure,
}

struct TestAiProvider {
    outcome: TestAiOutcome,
    delay: Duration,
    calls: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    requests: Mutex<Vec<cama_app::ai_services::ProviderRequest>>,
}

impl TestAiProvider {
    fn line(line: &str) -> Arc<Self> {
        Arc::new(Self {
            outcome: TestAiOutcome::Line(line.to_owned()),
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn failure() -> Arc<Self> {
        Arc::new(Self {
            outcome: TestAiOutcome::Failure,
            delay: Duration::ZERO,
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn delayed(line: &str, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            outcome: TestAiOutcome::Line(line.to_owned()),
            delay,
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }
}

impl cama_app::ai_services::AiProvider for TestAiProvider {
    fn complete(
        &self,
        request: cama_app::ai_services::ProviderRequest,
    ) -> Result<cama_app::ai_services::ProviderResponse, cama_app::ai_services::ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.requests.lock().expect("AI requests").push(request);
        std::thread::sleep(self.delay);
        self.active.fetch_sub(1, Ordering::SeqCst);
        match &self.outcome {
            TestAiOutcome::Line(line) => Ok(cama_app::ai_services::ProviderResponse {
                tool_calls: vec![cama_app::ai_services::ToolCall {
                    name: LINE_TOOL_NAME.to_owned(),
                    arguments: BTreeMap::from([("line".to_owned(), AiValue::Text(line.clone()))]),
                }],
                ..cama_app::ai_services::ProviderResponse::default()
            }),
            TestAiOutcome::Failure => Err(cama_app::ai_services::ProviderError::new(
                cama_app::ai_services::ProviderErrorKind::Unavailable,
                "test provider unavailable",
            )),
        }
    }
}

#[derive(Clone)]
struct TestAiLoader(Arc<TestAiProvider>);

impl cama_app::ai_services::ProviderLoader for TestAiLoader {
    fn load(
        &self,
    ) -> Result<Arc<dyn cama_app::ai_services::AiProvider>, cama_app::ai_services::ProviderError>
    {
        Ok(self.0.clone())
    }
}

fn test_ai_service(provider: Arc<TestAiProvider>) -> Arc<AIService> {
    let config = cama_app::ai_services::ProviderConfig::new(
        "groq/test-pet-flavor",
        cama_app::ai_services::ProviderCredential::new("test-key").expect("test credential"),
        Duration::from_secs(1),
        500,
    )
    .expect("test AI config");
    Arc::new(AIService::new(config, Arc::new(TestAiLoader(provider))))
}

#[derive(Clone, Copy)]
struct FixedClock(i64);

impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.0
    }
}

#[derive(Default)]
struct FakeReminders {
    enabled: Mutex<BTreeSet<(i64, i64)>>,
    rearmed: Mutex<Vec<(i64, i64)>>,
    cancelled: Mutex<Vec<(i64, i64)>>,
}

impl FakeReminders {
    fn enable(&self, user_id: i64, guild_id: i64) {
        self.enabled
            .lock()
            .expect("enabled preferences")
            .insert((user_id, guild_id));
    }
}

#[async_trait]
impl PetSweepReminderPort for FakeReminders {
    async fn pet_enabled(&self, user_id: i64, guild_id: i64) -> Result<bool, String> {
        Ok(self
            .enabled
            .lock()
            .expect("enabled preferences")
            .contains(&(user_id, guild_id)))
    }

    async fn rearm_pet(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        self.rearmed
            .lock()
            .expect("rearmed reminders")
            .push((user_id, guild_id));
        Ok(())
    }

    async fn cancel_pet(&self, user_id: i64, guild_id: i64) -> Result<(), String> {
        self.cancelled
            .lock()
            .expect("cancelled reminders")
            .push((user_id, guild_id));
        Ok(())
    }
}

#[derive(Default)]
struct NeverFlavor {
    started: Notify,
}

#[async_trait]
impl PetSweepFlavorPort for NeverFlavor {
    async fn generate(&self, _event: PetFlavorEvent, _pet: Pet) -> String {
        self.started.notify_waiters();
        std::future::pending().await
    }
}

struct FakeDiscord {
    guild_id: i64,
    configured_channel_id: i64,
    channel: Option<i64>,
    outcomes: Mutex<VecDeque<Result<(), PetSweepDeliveryError>>>,
    channel_messages: Mutex<Vec<(i64, DiscordMessage)>>,
    dms: Mutex<Vec<(i64, DiscordMessage)>>,
    dm_error: Mutex<Option<String>>,
    brawl_probe: Option<(PathBuf, i64)>,
    observed_brawl_statuses: Mutex<Vec<String>>,
}

impl FakeDiscord {
    fn new(channel: Option<i64>) -> Self {
        Self {
            guild_id: GUILD,
            configured_channel_id: CHANNEL,
            channel,
            outcomes: Mutex::new(VecDeque::new()),
            channel_messages: Mutex::new(Vec::new()),
            dms: Mutex::new(Vec::new()),
            dm_error: Mutex::new(None),
            brawl_probe: None,
            observed_brawl_statuses: Mutex::new(Vec::new()),
        }
    }

    fn with_outcomes(
        channel: Option<i64>,
        outcomes: impl IntoIterator<Item = Result<(), PetSweepDeliveryError>>,
    ) -> Self {
        let fake = Self::new(channel);
        *fake.outcomes.lock().expect("delivery outcomes") = outcomes.into_iter().collect();
        fake
    }

    fn with_brawl_probe(mut self, path: &Path, brawl_id: i64) -> Self {
        self.brawl_probe = Some((path.to_path_buf(), brawl_id));
        self
    }

    fn with_dm_error(self, error: &str) -> Self {
        *self.dm_error.lock().expect("direct-message error") = Some(error.to_owned());
        self
    }

    fn messages(&self) -> Vec<(i64, DiscordMessage)> {
        self.channel_messages
            .lock()
            .expect("channel messages")
            .clone()
    }

    fn dms(&self) -> Vec<(i64, DiscordMessage)> {
        self.dms.lock().expect("direct messages").clone()
    }
}

impl FirstGamePoolGuildSource for FakeDiscord {
    fn live_guild_ids(&self) -> Result<Vec<i64>, String> {
        Ok(vec![self.guild_id])
    }
}

#[async_trait]
impl PetSweepDiscordPort for FakeDiscord {
    async fn cached_text_channel(
        &self,
        guild_id: i64,
        configured_channel_id: i64,
    ) -> Result<Option<i64>, String> {
        Ok(
            (guild_id == self.guild_id && configured_channel_id == self.configured_channel_id)
                .then_some(self.channel)
                .flatten(),
        )
    }

    async fn send_channel(
        &self,
        channel_id: i64,
        message: DiscordMessage,
    ) -> Result<(), PetSweepDeliveryError> {
        if let Some((path, brawl_id)) = &self.brawl_probe {
            let status = PetBrawlRepository::new(path)
                .get_brawl(*brawl_id, Some(GUILD))
                .expect("read brawl during delivery")
                .expect("brawl remains durable")
                .status;
            self.observed_brawl_statuses
                .lock()
                .expect("observed brawl statuses")
                .push(status);
        }
        self.channel_messages
            .lock()
            .expect("channel messages")
            .push((channel_id, message));
        self.outcomes
            .lock()
            .expect("delivery outcomes")
            .pop_front()
            .unwrap_or(Ok(()))
    }

    async fn send_dm(&self, user_id: i64, message: DiscordMessage) -> Result<(), String> {
        self.dms
            .lock()
            .expect("direct messages")
            .push((user_id, message));
        self.dm_error
            .lock()
            .expect("direct-message error")
            .clone()
            .map_or(Ok(()), Err)
    }
}

fn migrated_database() -> NamedTempFile {
    let database = NamedTempFile::new().expect("temporary SQLite database");
    initialize_or_migrate(database.path()).expect("migrate canonical SQLite schema");
    database
}

fn add_player(path: &Path, user_id: i64) {
    let players = PlayerRepository::new(path);
    players
        .add(&NewPlayer::new(
            user_id,
            format!("Pet Owner {user_id}"),
            Some(GUILD),
        ))
        .expect("register pet owner");
    players
        .update_balance(user_id, Some(GUILD), 1_000)
        .expect("fund pet owner");
}

fn adopt_and_resolve(path: &Path, user_id: i64, name: &str, adopted_at: i64, species: &str) -> Pet {
    add_player(path, user_id);
    let mut service = SqlitePetCommandService::new(
        path,
        SeededPetRandom::new(user_id.unsigned_abs()),
        FixedClock(adopted_at),
        20,
    );
    let adopted = service
        .adopt(user_id, Some(GUILD), name, "standard")
        .unwrap()
        .pet;
    PetRepository::new(path)
        .resolve_hatch_species(adopted.pet_id, Some(GUILD), species, adopted.hatched_at)
        .expect("resolve hatch species")
}

fn worker(
    path: &Path,
    discord: Arc<FakeDiscord>,
    reminders: Arc<FakeReminders>,
    assets: &TempDir,
) -> PetSweepWorker {
    let discord: Arc<dyn PetSweepDiscordPort> = discord;
    let reminders: Arc<dyn PetSweepReminderPort> = reminders;
    PetSweepWorker::with_ports(
        path,
        CHANNEL,
        20,
        discord,
        reminders,
        FilesystemPetAssets::new(assets.path()),
        PET_SWEEP_WAKE_INTERVAL,
    )
}

fn worker_with_flavor(
    path: &Path,
    discord: Arc<FakeDiscord>,
    reminders: Arc<FakeReminders>,
    assets: &TempDir,
    flavor: Arc<dyn PetSweepFlavorPort>,
) -> PetSweepWorker {
    let discord: Arc<dyn PetSweepDiscordPort> = discord;
    let reminders: Arc<dyn PetSweepReminderPort> = reminders;
    PetSweepWorker::with_ports_and_flavor(
        path,
        CHANNEL,
        20,
        discord,
        reminders,
        flavor,
        FilesystemPetAssets::new(assets.path()),
        PET_SWEEP_WAKE_INTERVAL,
    )
}

fn worker_context() -> (watch::Sender<bool>, WorkerContext) {
    let (sender, receiver) = watch::channel(false);
    (sender, WorkerContext::new(receiver))
}

fn pet_announcement_times(path: &Path, pet_id: i64) -> (Option<i64>, Option<i64>) {
    Connection::open(path)
        .expect("open pet database")
        .query_row(
            "SELECT hatch_announced_at,death_announced_at FROM pets WHERE pet_id=?1",
            params![pet_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read announcement stamps")
}

#[tokio::test]
async fn stale_brawls_precede_hatch_delivery_and_authored_bytes_are_sent() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let first = adopt_and_resolve(
        database.path(),
        101,
        "Blep",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    let second = adopt_and_resolve(
        database.path(),
        102,
        "Blop",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    let brawl = PetBrawlRepository::new(database.path())
        .create_brawl_atomic(
            Some(GUILD),
            CHANNEL,
            first.discord_id,
            second.discord_id,
            first.pet_id,
            now - 5,
            now - 1,
            0,
            0,
        )
        .expect("create expiring brawl");
    let assets = TempDir::new().expect("pet assets directory");
    let authored = b"GIF89a-authored-pet-card";
    std::fs::write(assets.path().join("common_cama_baby_happy.gif"), authored)
        .expect("write authored pet card");
    let discord =
        Arc::new(FakeDiscord::new(Some(CHANNEL)).with_brawl_probe(database.path(), brawl.brawl_id));
    let reminders = Arc::new(FakeReminders::default());
    let worker = worker(
        database.path(),
        Arc::clone(&discord),
        Arc::clone(&reminders),
        &assets,
    );
    let (_shutdown, context) = worker_context();

    worker.sweep_once(&context).await;

    assert!(
        discord
            .observed_brawl_statuses
            .lock()
            .expect("observed brawl statuses")
            .iter()
            .all(|status| status == "expired")
    );
    let messages = discord.messages();
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|(_, message)| {
        message.allowed_mentions == DiscordAllowedMentions::Default
            && message.response.attachments.len() == 1
            && message.response.attachments[0].bytes == authored
            && message.response.embeds[0]
                .image_url
                .as_deref()
                .is_some_and(|url| url.starts_with("attachment://"))
    }));
    assert!(
        pet_announcement_times(database.path(), first.pet_id)
            .0
            .is_some()
    );
    assert!(
        pet_announcement_times(database.path(), second.pet_id)
            .0
            .is_some()
    );
    assert_eq!(
        reminders.rearmed.lock().expect("rearmed reminders").len(),
        2
    );
}

#[tokio::test]
async fn mixed_live_and_foreign_pet_state_fails_closed_before_mutation_or_delivery() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let live = adopt_and_resolve(
        database.path(),
        111,
        "Live",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    let foreign = adopt_and_resolve(
        database.path(),
        112,
        "Foreign",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    Connection::open(database.path())
        .expect("open pet database")
        .execute(
            "UPDATE pets SET guild_id=?1 WHERE pet_id=?2",
            params![OTHER_GUILD, foreign.pet_id],
        )
        .expect("move pet into foreign guild fixture");
    let assets = TempDir::new().expect("pet assets directory");
    let discord = Arc::new(FakeDiscord::new(Some(CHANNEL)));
    let reminders = Arc::new(FakeReminders::default());
    let worker = worker(
        database.path(),
        Arc::clone(&discord),
        Arc::clone(&reminders),
        &assets,
    );
    let (_shutdown, context) = worker_context();

    worker.sweep_once(&context).await;

    assert_eq!(pet_announcement_times(database.path(), live.pet_id).0, None);
    assert_eq!(
        pet_announcement_times(database.path(), foreign.pet_id).0,
        None
    );
    assert!(discord.messages().is_empty());
    assert!(discord.dms().is_empty());
    assert!(
        reminders
            .rearmed
            .lock()
            .expect("rearmed reminders")
            .is_empty()
    );
    assert!(
        reminders
            .cancelled
            .lock()
            .expect("cancelled reminders")
            .is_empty()
    );
}

#[tokio::test]
async fn transient_hatch_is_isolated_unstamped_and_retried_after_restart() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let first = adopt_and_resolve(
        database.path(),
        201,
        "First",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    let second = adopt_and_resolve(
        database.path(),
        202,
        "Second",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    let assets = TempDir::new().expect("pet assets directory");
    let first_discord = Arc::new(FakeDiscord::with_outcomes(
        Some(CHANNEL),
        [
            Err(PetSweepDeliveryError::Transient("network reset".to_owned())),
            Ok(()),
        ],
    ));
    let reminders = Arc::new(FakeReminders::default());
    let first_worker = worker(
        database.path(),
        Arc::clone(&first_discord),
        Arc::clone(&reminders),
        &assets,
    );
    let (_shutdown, context) = worker_context();

    first_worker.sweep_once(&context).await;

    assert_eq!(
        pet_announcement_times(database.path(), first.pet_id).0,
        None
    );
    assert!(
        pet_announcement_times(database.path(), second.pet_id)
            .0
            .is_some()
    );
    assert_eq!(first_discord.messages().len(), 2);
    assert_eq!(
        reminders
            .rearmed
            .lock()
            .expect("rearmed reminders")
            .as_slice(),
        &[(second.discord_id, GUILD)]
    );

    let restarted_discord = Arc::new(FakeDiscord::new(Some(CHANNEL)));
    let restarted = worker(
        database.path(),
        Arc::clone(&restarted_discord),
        Arc::clone(&reminders),
        &assets,
    );
    restarted.sweep_once(&context).await;

    assert_eq!(restarted_discord.messages().len(), 1);
    assert!(
        pet_announcement_times(database.path(), first.pet_id)
            .0
            .is_some()
    );
}

#[tokio::test]
async fn cancellation_interrupts_cold_flavor_before_delivery_or_stamp() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let pet = adopt_and_resolve(
        database.path(),
        203,
        "Cancelled",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    let assets = TempDir::new().expect("pet assets directory");
    let discord = Arc::new(FakeDiscord::new(Some(CHANNEL)));
    let reminders = Arc::new(FakeReminders::default());
    let flavor = Arc::new(NeverFlavor::default());
    let flavor_port: Arc<dyn PetSweepFlavorPort> = flavor.clone();
    let worker = worker_with_flavor(
        database.path(),
        Arc::clone(&discord),
        Arc::clone(&reminders),
        &assets,
        flavor_port,
    );
    let (shutdown, context) = worker_context();
    let started = flavor.started.notified();
    let sweep = worker.sweep_once(&context);
    tokio::pin!(started, sweep);

    tokio::select! {
        () = &mut started => {}
        () = &mut sweep => panic!("sweep completed before flavor generation blocked"),
    }
    shutdown.send(true).expect("cancel worker during flavor");
    tokio::time::timeout(Duration::from_secs(1), &mut sweep)
        .await
        .expect("cancellation interrupts flavor generation");

    assert!(discord.messages().is_empty());
    assert_eq!(pet_announcement_times(database.path(), pet.pet_id).0, None);
    assert!(
        reminders
            .rearmed
            .lock()
            .expect("rearmed reminders")
            .is_empty()
    );
}

#[tokio::test]
async fn durable_eaten_death_survives_transient_then_forbidden_and_dms_opted_in_owner() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let pet = adopt_and_resolve(
        database.path(),
        301,
        "Dinner",
        now - EGG_HATCH_SECONDS - ADULT_AGE_SECONDS - 10,
        "common_cama",
    );
    Connection::open(database.path())
        .expect("open pet fixture")
        .execute(
            "UPDATE pets SET last_fed_at=?1,hunger_at_last_fed=100,dig_work_at=?1,
             hatch_announced_at=?1 WHERE pet_id=?2",
            params![now, pet.pet_id],
        )
        .expect("refresh adult pet anchors");
    let pet = PetRepository::new(database.path())
        .get_pet_by_id(pet.pet_id, Some(GUILD))
        .expect("read adult pet")
        .expect("adult pet exists");
    PetEatingRepository::new(database.path())
        .eat_adult_pet_atomic(EatAdultPetRequest {
            discord_id: pet.discord_id,
            guild_id: Some(GUILD),
            pet_id: pet.pet_id,
            expected_last_fed_at: pet.last_fed_at,
            expected_hunger: pet.hunger_at_last_fed,
            reward: 123,
            penalty_games: 4,
            now,
        })
        .expect("persist eaten outcome atomically");
    let assets = TempDir::new().expect("pet assets directory");
    let reminders = Arc::new(FakeReminders::default());
    reminders.enable(pet.discord_id, GUILD);
    let transient = Arc::new(FakeDiscord::with_outcomes(
        Some(CHANNEL),
        [Err(PetSweepDeliveryError::Transient(
            "Discord unavailable".to_owned(),
        ))],
    ));
    let first = worker(
        database.path(),
        Arc::clone(&transient),
        Arc::clone(&reminders),
        &assets,
    );
    let (_shutdown, context) = worker_context();

    first.sweep_once(&context).await;

    assert_eq!(pet_announcement_times(database.path(), pet.pet_id).1, None);
    assert!(
        reminders
            .cancelled
            .lock()
            .expect("cancellations")
            .is_empty()
    );
    assert!(transient.dms().is_empty());

    let forbidden = Arc::new(FakeDiscord::with_outcomes(
        Some(CHANNEL),
        [Err(PetSweepDeliveryError::Forbidden)],
    ));
    let restarted = worker(
        database.path(),
        Arc::clone(&forbidden),
        Arc::clone(&reminders),
        &assets,
    );
    restarted.sweep_once(&context).await;

    assert!(
        pet_announcement_times(database.path(), pet.pet_id)
            .1
            .is_some()
    );
    assert_eq!(
        reminders
            .cancelled
            .lock()
            .expect("cancellations")
            .as_slice(),
        &[(pet.discord_id, GUILD)]
    );
    let public = forbidden.messages();
    let dms = forbidden.dms();
    assert_eq!((public.len(), dms.len()), (1, 1));
    for message in [&public[0].1, &dms[0].1] {
        assert!(message.response.attachments.is_empty());
        let description = message.response.embeds[0]
            .description
            .as_deref()
            .expect("eaten outcome description");
        assert!(description.contains("123"));
        assert!(description.contains("4 remaining"));
        assert!(description.contains("1,103"));
    }
}

#[tokio::test]
async fn direct_eat_delivery_guard_suppresses_sweep_until_retry() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let pet = adopt_and_resolve(
        database.path(),
        351,
        "Guarded Dinner",
        now - EGG_HATCH_SECONDS - ADULT_AGE_SECONDS - 10,
        "common_cama",
    );
    Connection::open(database.path())
        .expect("open pet fixture")
        .execute(
            "UPDATE pets SET last_fed_at=?1,hunger_at_last_fed=100,dig_work_at=?1,
             hatch_announced_at=?1 WHERE pet_id=?2",
            params![now, pet.pet_id],
        )
        .expect("refresh adult pet anchors");
    let pet = PetRepository::new(database.path())
        .get_pet_by_id(pet.pet_id, Some(GUILD))
        .expect("read adult pet")
        .expect("adult pet exists");
    PetEatingRepository::new(database.path())
        .eat_adult_pet_atomic(EatAdultPetRequest {
            discord_id: pet.discord_id,
            guild_id: Some(GUILD),
            pet_id: pet.pet_id,
            expected_last_fed_at: pet.last_fed_at,
            expected_hunger: pet.hunger_at_last_fed,
            reward: 123,
            penalty_games: 4,
            now,
        })
        .expect("persist eaten outcome atomically");
    let assets = TempDir::new().expect("pet assets directory");
    let discord = Arc::new(FakeDiscord::new(Some(CHANNEL)));
    let reminders = Arc::new(FakeReminders::default());
    let worker = worker(
        database.path(),
        Arc::clone(&discord),
        Arc::clone(&reminders),
        &assets,
    );
    let (_shutdown, context) = worker_context();
    let guard = DirectDeathDeliveryGuard::new(database.path(), pet.pet_id);

    worker.sweep_once(&context).await;

    assert!(discord.messages().is_empty());
    assert_eq!(pet_announcement_times(database.path(), pet.pet_id).1, None);
    assert!(
        reminders
            .cancelled
            .lock()
            .expect("cancellations")
            .is_empty()
    );

    drop(guard);
    worker.sweep_once(&context).await;

    assert_eq!(discord.messages().len(), 1);
    assert!(
        pet_announcement_times(database.path(), pet.pet_id)
            .1
            .is_some()
    );
}

#[tokio::test]
async fn no_channel_death_marks_and_cancels_after_best_effort_native_tombstone_dm() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let pet = adopt_and_resolve(
        database.path(),
        401,
        "Memorial",
        now - EGG_HATCH_SECONDS - 100,
        "common_cama",
    );
    Connection::open(database.path())
        .expect("open pet fixture")
        .execute(
            "UPDATE pets SET died_at=?1,death_cause='starvation',hatch_announced_at=?1
             WHERE pet_id=?2",
            params![now - 1, pet.pet_id],
        )
        .expect("persist unannounced death");
    let assets = TempDir::new().expect("empty pet assets directory");
    let discord = Arc::new(FakeDiscord::new(None).with_dm_error("DMs are closed"));
    let reminders = Arc::new(FakeReminders::default());
    reminders.enable(pet.discord_id, GUILD);
    let worker = worker(
        database.path(),
        Arc::clone(&discord),
        Arc::clone(&reminders),
        &assets,
    );
    let (_shutdown, context) = worker_context();

    worker.sweep_once(&context).await;

    assert!(discord.messages().is_empty());
    let dms = discord.dms();
    assert_eq!(dms.len(), 1);
    let attachment = &dms[0].1.response.attachments[0];
    assert!(attachment.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(
        dms[0].1.response.embeds[0].image_url.as_deref(),
        Some(format!("attachment://{}", attachment.filename).as_str())
    );
    assert!(
        pet_announcement_times(database.path(), pet.pet_id)
            .1
            .is_some()
    );
    assert_eq!(
        reminders
            .cancelled
            .lock()
            .expect("cancellations")
            .as_slice(),
        &[(pet.discord_id, GUILD)]
    );
}

#[tokio::test]
async fn transient_refund_stays_durable_then_restart_posts_and_marks() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    Connection::open(database.path())
        .expect("open refund fixture")
        .execute(
            "INSERT INTO pet_refund_windows
                (guild_id,week_key,paid_at,total_paid,announcement_payload)
             VALUES (?1,'2026-W30',?2,30,
                '{\"payouts\":[{\"discord_id\":501,\"consumed_jc\":20,\"multiplier_pct\":150,\"amount\":30}],\"scaled_down\":false}')",
            params![GUILD, now],
        )
        .expect("persist refund announcement");
    let assets = TempDir::new().expect("pet assets directory");
    let reminders = Arc::new(FakeReminders::default());
    let transient = Arc::new(FakeDiscord::with_outcomes(
        Some(CHANNEL),
        [Err(PetSweepDeliveryError::Transient(
            "Discord timeout".to_owned(),
        ))],
    ));
    let first = worker(
        database.path(),
        Arc::clone(&transient),
        Arc::clone(&reminders),
        &assets,
    );
    let (_shutdown, context) = worker_context();

    first.sweep_once(&context).await;

    let announced = || {
        Connection::open(database.path())
            .expect("open refund database")
            .query_row(
                "SELECT announced_at FROM pet_refund_windows
                 WHERE guild_id=?1 AND week_key='2026-W30'",
                [GUILD],
                |row| row.get::<_, Option<i64>>(0),
            )
            .expect("read refund stamp")
    };
    assert_eq!(announced(), None);

    let restarted_discord = Arc::new(FakeDiscord::new(Some(CHANNEL)));
    let restarted = worker(
        database.path(),
        Arc::clone(&restarted_discord),
        reminders,
        &assets,
    );
    restarted.sweep_once(&context).await;

    assert!(announced().is_some());
    let messages = restarted_discord.messages();
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].1.response.embeds[0]
            .description
            .as_deref()
            .expect("refund copy")
            .contains("150%")
    );
}

#[tokio::test]
async fn production_flavor_honors_guild_ai_disabled_without_provider_call() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let pet = adopt_and_resolve(
        database.path(),
        601,
        "Quiet",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    GuildConfigRepository::new(database.path(), true)
        .set_ai_enabled(GUILD, false)
        .expect("disable guild AI");
    let provider = TestAiProvider::line("This line must not be requested.");
    let flavor = ProductionPetFlavorRuntime::new(
        database.path(),
        Some(test_ai_service(Arc::clone(&provider))),
        true,
    );

    let line = flavor.generate(PetFlavorEvent::Hatched, pet).await;

    assert!(fallbacks(PetFlavorEvent::Hatched).contains(&line.as_str()));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn production_flavor_uses_ai_and_caches_lifecycle_line() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let pet = adopt_and_resolve(
        database.path(),
        602,
        "Cache",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    GuildConfigRepository::new(database.path(), false)
        .set_ai_enabled(GUILD, true)
        .expect("enable guild AI");
    let generated = "Tiny hooves report for duty.";
    let provider = TestAiProvider::line(generated);
    let flavor = ProductionPetFlavorRuntime::new(
        database.path(),
        Some(test_ai_service(Arc::clone(&provider))),
        false,
    );

    let first = flavor.generate(PetFlavorEvent::Hatched, pet.clone()).await;
    let second = flavor.generate(PetFlavorEvent::Hatched, pet).await;

    assert_eq!((first.as_str(), second.as_str()), (generated, generated));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let requests = provider.requests.lock().expect("AI requests");
    assert_eq!(requests[0].max_tokens, 100);
    assert_eq!(requests[0].tools[0].name, LINE_TOOL_NAME);
    assert_eq!(requests[0].tools[0].additional_properties, Some(false));
}

#[tokio::test]
async fn production_flavor_failure_is_fail_soft_with_exact_fallback() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let pet = adopt_and_resolve(
        database.path(),
        603,
        "Fallback",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    let provider = TestAiProvider::failure();
    let flavor = ProductionPetFlavorRuntime::new(
        database.path(),
        Some(test_ai_service(Arc::clone(&provider))),
        true,
    );

    let line = flavor.generate(PetFlavorEvent::Hatched, pet).await;

    assert!(fallbacks(PetFlavorEvent::Hatched).contains(&line.as_str()));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn production_flavor_preserves_two_request_concurrency_bound() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let pets = [604, 605, 606]
        .into_iter()
        .map(|user_id| {
            adopt_and_resolve(
                database.path(),
                user_id,
                &format!("Concurrent {user_id}"),
                now - EGG_HATCH_SECONDS - 10,
                "common_cama",
            )
        })
        .collect::<Vec<_>>();
    let provider =
        TestAiProvider::delayed("A cheerful concurrent quip.", Duration::from_millis(40));
    let flavor = ProductionPetFlavorRuntime::new(
        database.path(),
        Some(test_ai_service(Arc::clone(&provider))),
        true,
    );

    let (first, second, third) = tokio::join!(
        flavor.generate(PetFlavorEvent::Hatched, pets[0].clone()),
        flavor.generate(PetFlavorEvent::Hatched, pets[1].clone()),
        flavor.generate(PetFlavorEvent::Hatched, pets[2].clone()),
    );

    assert_eq!(first, "A cheerful concurrent quip.");
    assert_eq!(second, first);
    assert_eq!(third, first);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert!(provider.max_active.load(Ordering::SeqCst) <= 2);
}

#[tokio::test]
async fn ai_flavored_hatch_retries_after_worker_restart_before_stamp() {
    let now = Utc::now().timestamp();
    let database = migrated_database();
    let pet = adopt_and_resolve(
        database.path(),
        607,
        "Restart",
        now - EGG_HATCH_SECONDS - 10,
        "common_cama",
    );
    let assets = TempDir::new().expect("pet assets directory");
    let reminders = Arc::new(FakeReminders::default());
    let provider = TestAiProvider::line("Restarted hooves are online.");
    let ai = test_ai_service(Arc::clone(&provider));
    let first_discord = Arc::new(FakeDiscord::with_outcomes(
        Some(CHANNEL),
        [Err(PetSweepDeliveryError::Transient(
            "first attempt failed".to_owned(),
        ))],
    ));
    let first_flavor: Arc<dyn PetSweepFlavorPort> = Arc::new(ProductionPetFlavorRuntime::new(
        database.path(),
        Some(Arc::clone(&ai)),
        true,
    ));
    let first = worker_with_flavor(
        database.path(),
        Arc::clone(&first_discord),
        Arc::clone(&reminders),
        &assets,
        first_flavor,
    );
    let (_shutdown, context) = worker_context();

    first.sweep_once(&context).await;

    assert_eq!(pet_announcement_times(database.path(), pet.pet_id).0, None);
    let restarted_discord = Arc::new(FakeDiscord::new(Some(CHANNEL)));
    let restarted_flavor: Arc<dyn PetSweepFlavorPort> = Arc::new(ProductionPetFlavorRuntime::new(
        database.path(),
        Some(ai),
        true,
    ));
    let restarted = worker_with_flavor(
        database.path(),
        Arc::clone(&restarted_discord),
        reminders,
        &assets,
        restarted_flavor,
    );
    restarted.sweep_once(&context).await;

    assert!(
        pet_announcement_times(database.path(), pet.pet_id)
            .0
            .is_some()
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let fields = &restarted_discord.messages()[0].1.response.embeds[0].fields;
    assert!(fields.iter().any(|field| {
        field.name == "💬 Cama chatter" && field.value == "Restarted hooves are online."
    }));
}

#[test]
fn production_spec_is_channel_gated_and_uses_exact_public_identity() {
    let database = migrated_database();
    let disabled = ApplicationConfig::from_lookup(|name| {
        (name == "DISCORD_BOT_TOKEN").then(|| "token".to_owned())
    })
    .expect("disabled pet config");
    let enabled = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("token".to_owned()),
        "PET_CHANNEL_ID" => Some(CHANNEL.to_string()),
        "PET_HUNGER_DECAY_PER_DAY" => Some("23".to_owned()),
        _ => None,
    })
    .expect("enabled pet config");
    assert!(disabled.channels.pet.is_none());
    assert_eq!(enabled.channels.pet, Some(CHANNEL));
    assert_eq!(enabled.values.pet_hunger_decay_per_day, 23);
    assert_eq!(PET_SWEEP_WORKER_NAME, "pet_sweep");

    let discord = Arc::new(SerenityDiscordTransport::new());
    let reminder_discord: Arc<dyn crate::discord_transport::DiscordTransport> = discord.clone();
    let reminders =
        crate::ReminderRegistrationProvider::new(database.path(), &enabled, reminder_discord);
    assert!(
        pet_sweep_worker_spec(
            database.path(),
            &disabled,
            Arc::clone(&discord),
            reminders.hooks(),
        )
        .is_none()
    );
    let spec =
        pet_sweep_worker_spec_with_ai(database.path(), &enabled, discord, reminders.hooks(), None)
            .expect("configured pet channel enables worker");
    assert_eq!(spec.name, PET_SWEEP_WORKER_NAME);
}

#[tokio::test]
async fn worker_loop_uses_exact_interval_and_honors_pre_cancelled_context() {
    assert_eq!(PET_SWEEP_WAKE_INTERVAL, Duration::from_secs(600));
    let database = migrated_database();
    let assets = TempDir::new().expect("pet assets directory");
    let discord = Arc::new(FakeDiscord::new(Some(CHANNEL)));
    let reminders = Arc::new(FakeReminders::default());
    let worker = worker(database.path(), discord, reminders, &assets);
    let (shutdown, context) = worker_context();
    shutdown.send(true).expect("cancel worker");

    worker.run(context).await.expect("clean cancellation");
}
