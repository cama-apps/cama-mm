use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use cama_app::opendota_http::{OpenDotaHttpConfig, OpenDotaRuntimeServices};
use cama_db::schema_manager::initialize_or_migrate;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use crate::registration::{InteractionResponseError, Registry};

use super::*;

fn leaf_paths(options: &[CommandOptionSpec]) -> BTreeMap<String, Vec<String>> {
    fn visit(
        prefix: &mut Vec<String>,
        options: &[CommandOptionSpec],
        leaves: &mut BTreeMap<String, Vec<String>>,
    ) {
        for option in options {
            prefix.push(option.name.clone());
            if option.kind == CommandOptionKind::Subcommand {
                leaves.insert(prefix.join(" "), prefix.clone());
            } else if option.kind == CommandOptionKind::SubcommandGroup {
                visit(prefix, &option.options, leaves);
            }
            prefix.pop();
        }
    }

    let mut leaves = BTreeMap::new();
    visit(&mut Vec::new(), options, &mut leaves);
    leaves
}

#[test]
fn production_admin_tree_has_all_thirty_python_leaves() {
    let actual = leaf_paths(&admin_options(3_000.0));
    let expected = [
        "adjust rating",
        "adjust rd",
        "lowprio add",
        "lowprio remove",
        "lowprio status",
        "lowprio list",
        "moderation suspend",
        "moderation lift",
        "moderation status",
        "moderation list",
        "moderation history",
        "health",
        "addfake",
        "filllobbytest",
        "resetuser",
        "registeruser",
        "sync",
        "givecoin",
        "resetloancooldown",
        "resetbankruptcycooldown",
        "setrating",
        "recalibrate",
        "bumprd",
        "resetrecalibrationcooldown",
        "extendbetting",
        "correctmatch",
        "addsteamid",
        "removesteamid",
        "setprimarysteam",
        "seedherogrid",
    ];
    assert_eq!(actual.len(), 30);
    assert_eq!(
        actual.keys().cloned().collect::<BTreeSet<_>>(),
        expected.into_iter().map(str::to_owned).collect()
    );
}

#[test]
fn test_admin_payload_keeps_lowprio_group_discoverable() {
    let fixture = ProviderFixture::new();
    let registry = fixture.registry();
    let command = registry
        .commands()
        .find(|command| command.name == "admin")
        .expect("admin root command is registered");
    let lowprio = command
        .options
        .iter()
        .find(|option| option.name == "lowprio")
        .expect("lowprio group is discoverable under admin");

    assert_eq!(lowprio.kind, CommandOptionKind::SubcommandGroup);
    assert_eq!(
        lowprio
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        ["add", "remove", "status", "list"]
    );
    // CommandSpec deliberately has no default-member-permissions field: the
    // root is discoverable, while each handler enforces admin permissions at
    // dispatch time instead of applying a static Discord gate.
}

#[test]
fn correctmatch_contract_is_exact_and_required() {
    let options = admin_options(3_000.0);
    let command = options
        .iter()
        .find(|option| option.name == "correctmatch")
        .expect("correctmatch leaf");
    assert_eq!(command.kind, CommandOptionKind::Subcommand);
    assert_eq!(command.options.len(), 2);
    assert_eq!(command.options[0].name, "match_id");
    assert_eq!(command.options[0].kind, CommandOptionKind::Integer);
    assert!(command.options[0].required);
    assert_eq!(command.options[1].name, "correct_result");
    assert_eq!(command.options[1].kind, CommandOptionKind::String);
    assert!(command.options[1].required);
    assert_eq!(
        command.options[1].choices,
        vec![
            CommandOptionChoice::String {
                name: "Radiant".to_owned(),
                value: "radiant".to_owned(),
            },
            CommandOptionChoice::String {
                name: "Dire".to_owned(),
                value: "dire".to_owned(),
            },
        ]
    );
}

const GUILD: u64 = 42_001;
const ADMIN: u64 = 42_002;
const TARGET: u64 = 42_003;
const MANAGE_GUILD: u64 = 1 << 5;

#[derive(Default)]
struct RecordingResponder {
    acknowledged: AtomicBool,
    fail_defer: AtomicBool,
    defers: Mutex<Vec<bool>>,
    responses: Mutex<Vec<InteractionResponse>>,
}

impl RecordingResponder {
    fn last(&self) -> InteractionResponse {
        self.responses
            .lock()
            .expect("response lock")
            .last()
            .cloned()
            .expect("admin response")
    }
}

#[async_trait]
impl InteractionResponder for RecordingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.acknowledged.store(true, Ordering::Release);
        self.responses.lock().expect("response lock").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        if self.fail_defer.load(Ordering::Acquire) {
            return Err(InteractionResponseError::new(
                "simulated expired interaction",
            ));
        }
        self.acknowledged.store(true, Ordering::Release);
        self.defers.lock().expect("defer lock").push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.responses.lock().expect("response lock").push(response);
        Ok(())
    }

    fn response_is_done(&self) -> bool {
        self.acknowledged.load(Ordering::Acquire)
    }
}

struct RecordingDiscordControl {
    health_calls: AtomicUsize,
    sync_calls: AtomicUsize,
}

#[async_trait]
impl AdminDiscordControl for RecordingDiscordControl {
    async fn health(&self) -> AdminDiscordHealth {
        self.health_calls.fetch_add(1, Ordering::SeqCst);
        AdminDiscordHealth {
            guild_count: 3,
            latency_ms: Some(17),
        }
    }

    async fn sync_commands(&self) -> Result<AdminCommandSyncResult, String> {
        self.sync_calls.fetch_add(1, Ordering::SeqCst);
        Ok(AdminCommandSyncResult {
            command_count: 91,
            guild_count: 3,
        })
    }
}

#[derive(Default)]
struct RecordingLobbyControl {
    add_fake: Mutex<Vec<AdminFakeLobbyRequest>>,
    fill_fake: Mutex<Vec<AdminFakeLobbyRequest>>,
}

#[async_trait]
impl AdminLobbyControl for RecordingLobbyControl {
    async fn add_fake_users(
        &self,
        request: AdminFakeLobbyRequest,
    ) -> Result<AdminFakeLobbyResult, String> {
        self.add_fake.lock().expect("add-fake lock").push(request);
        Ok(AdminFakeLobbyResult {
            users_added: request.count,
            user_names: (1..=request.count)
                .map(|index| format!("FakeUser{index}"))
                .collect(),
            already_at_threshold: None,
        })
    }

    async fn fill_lobby(
        &self,
        request: AdminFakeLobbyRequest,
    ) -> Result<AdminFakeLobbyResult, String> {
        self.fill_fake.lock().expect("fill-fake lock").push(request);
        Ok(AdminFakeLobbyResult {
            users_added: 4,
            user_names: Vec::new(),
            already_at_threshold: None,
        })
    }

    async fn eject_suspended_player(
        &self,
        _request: AdminLobbyEjectionRequest,
    ) -> Result<AdminLobbyEjectionResult, String> {
        Ok(AdminLobbyEjectionResult::default())
    }
}

#[derive(Default)]
struct RecordingMatchControl {
    extensions: Mutex<Vec<AdminExtendBettingRequest>>,
    seeds: Mutex<Vec<AdminSeedHeroGridRequest>>,
}

#[async_trait]
impl AdminMatchControl for RecordingMatchControl {
    async fn extend_betting(
        &self,
        request: AdminExtendBettingRequest,
    ) -> Result<AdminExtendBettingResult, String> {
        self.extensions
            .lock()
            .expect("extension lock")
            .push(request);
        Ok(AdminExtendBettingResult {
            pending_match_id: request.pending_match_id,
            old_bet_lock_until: 1_000,
            new_bet_lock_until: 1_300,
            lobby_label: "All You Can Feed".to_owned(),
            jump_url: Some("https://discord.test/match".to_owned()),
            refreshed_routes: 2,
            refresh_failures: Vec::new(),
        })
    }

    async fn seed_hero_grid(
        &self,
        request: AdminSeedHeroGridRequest,
    ) -> Result<AdminSeedHeroGridResult, String> {
        self.seeds.lock().expect("seed lock").push(request);
        Ok(AdminSeedHeroGridResult {
            players_seeded: 20,
            matches_created: 30,
        })
    }
}

#[derive(Default)]
struct RecordingCorrectionControl {
    requests: Mutex<Vec<AdminCorrectMatchRequest>>,
}

#[async_trait]
impl AdminMatchCorrectionControl for RecordingCorrectionControl {
    async fn correct_match(
        &self,
        request: AdminCorrectMatchRequest,
    ) -> Result<AdminCorrectMatchResult, AdminCorrectMatchError> {
        self.requests.lock().expect("correction lock").push(request);
        Ok(AdminCorrectMatchResult {
            old_winning_team: AdminCorrectMatchSide::Radiant,
            new_winning_team: request.correct_result,
            correction_id: Some(7),
            players_affected: 10,
            ratings_updated: 10,
            openskill_matches_replayed: 3,
            bet_correction: None,
            replay_recovered: false,
        })
    }
}

struct ProviderFixture {
    database: NamedTempFile,
    provider: AdminRegistrationProvider,
    discord: Arc<RecordingDiscordControl>,
    lobby: Arc<RecordingLobbyControl>,
    matches: Arc<RecordingMatchControl>,
    corrections: Arc<RecordingCorrectionControl>,
}

impl ProviderFixture {
    fn new() -> Self {
        let database = NamedTempFile::new().expect("temporary admin database");
        initialize_or_migrate(database.path()).expect("migrate canonical admin database");
        let config = ApplicationConfig::from_lookup(|name| match name {
            "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
            "ADMIN_USER_IDS" => Some(ADMIN.to_string()),
            "ECONOMY_EVENTS_ENABLED" | "NEON_DEGEN_ENABLED" => Some("false".to_owned()),
            _ => None,
        })
        .expect("admin test configuration");
        let mut opendota_config = OpenDotaHttpConfig::new("http://127.0.0.1:9", None);
        opendota_config.request_timeout = Duration::from_millis(1);
        opendota_config.request_deadline = Duration::from_millis(1);
        opendota_config.retry_delays = vec![Duration::ZERO];
        let opendota = Arc::new(
            OpenDotaRuntimeServices::with_config(opendota_config)
                .expect("offline OpenDota composition"),
        );
        let discord = Arc::new(RecordingDiscordControl {
            health_calls: AtomicUsize::new(0),
            sync_calls: AtomicUsize::new(0),
        });
        let lobby = Arc::new(RecordingLobbyControl::default());
        let matches = Arc::new(RecordingMatchControl::default());
        let corrections = Arc::new(RecordingCorrectionControl::default());
        let transport = Arc::new(crate::SerenityDiscordTransport::new());
        let provider = AdminRegistrationProvider::new(
            database.path(),
            &config,
            opendota,
            AdminRuntimePorts {
                discord: transport,
                discord_control: discord.clone(),
                lobby: lobby.clone(),
                matches: matches.clone(),
                match_corrections: corrections.clone(),
            },
            UsageMonitor::default(),
        );
        Self {
            database,
            provider,
            discord,
            lobby,
            matches,
            corrections,
        }
    }

    fn registry(&self) -> Registry {
        let mut builder = RegistryBuilder::default();
        builder
            .add_provider(&self.provider)
            .expect("register production Admin provider");
        builder.build()
    }

    fn player(
        &self,
        user_id: u64,
        wins: i64,
        losses: i64,
        rating: Option<f64>,
        rd: Option<f64>,
        volatility: Option<f64>,
    ) {
        let connection =
            Connection::open(self.database.path()).expect("open Admin provider database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("use Python-compatible foreign-key policy");
        connection
            .execute(
                "INSERT INTO players (
                     discord_id,guild_id,discord_username,wins,losses,
                     glicko_rating,glicko_rd,glicko_volatility
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    i64::try_from(user_id).unwrap(),
                    i64::try_from(GUILD).unwrap(),
                    format!("Player {user_id}"),
                    wins,
                    losses,
                    rating,
                    rd,
                    volatility
                ],
            )
            .expect("seed Admin provider player");
    }

    fn rating(&self, user_id: u64) -> (Option<f64>, Option<f64>, Option<f64>) {
        Connection::open(self.database.path())
            .expect("open Admin provider database")
            .query_row(
                "SELECT glicko_rating,glicko_rd,glicko_volatility FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![
                    i64::try_from(user_id).unwrap(),
                    i64::try_from(GUILD).unwrap()
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load Admin provider rating")
    }

    async fn dispatch(
        &self,
        subcommand: &str,
        options: Vec<InteractionOption>,
        interaction_id: u64,
    ) -> Arc<RecordingResponder> {
        self.dispatch_request(admin_command(
            subcommand,
            options,
            interaction_id,
            ADMIN,
            Some(MANAGE_GUILD),
        ))
        .await
    }

    async fn dispatch_adjust(
        &self,
        subcommand: &str,
        options: Vec<InteractionOption>,
        interaction_id: u64,
        actor_id: u64,
        permissions: Option<u64>,
    ) -> Arc<RecordingResponder> {
        self.dispatch_request(admin_adjust_command(
            subcommand,
            options,
            interaction_id,
            actor_id,
            permissions,
        ))
        .await
    }

    async fn dispatch_request(&self, request: InteractionRequest) -> Arc<RecordingResponder> {
        let responder = Arc::new(RecordingResponder::default());
        self.registry()
            .command_handler("admin")
            .expect("Admin handler")
            .handle(request, responder.clone())
            .await
            .expect("Admin command succeeds");
        responder
    }
}

fn admin_command(
    subcommand: &str,
    options: Vec<InteractionOption>,
    interaction_id: u64,
    actor_id: u64,
    permissions: Option<u64>,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id,
        name: "admin".to_owned(),
        user_id: actor_id,
        user_display_name: "Admin".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(55),
        member_permissions: permissions,
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(options),
        }],
    }
}

fn admin_adjust_command(
    subcommand: &str,
    options: Vec<InteractionOption>,
    interaction_id: u64,
    actor_id: u64,
    permissions: Option<u64>,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id,
        name: "admin".to_owned(),
        user_id: actor_id,
        user_display_name: "Admin".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(55),
        member_permissions: permissions,
        options: vec![InteractionOption {
            name: "adjust".to_owned(),
            value: InteractionValue::SubcommandGroup(vec![InteractionOption {
                name: subcommand.to_owned(),
                value: InteractionValue::Subcommand(options),
            }]),
        }],
    }
}

fn integer_option(name: &str, value: i64) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::Integer(value),
    }
}

fn string_option(name: &str, value: &str) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::String(value.to_owned()),
    }
}

fn user_option(name: &str, id: u64, display_name: &str) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::User {
            id,
            display_name: Some(display_name.to_owned()),
            is_bot: Some(false),
        },
    }
}

fn number_option(name: &str, value: f64) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::Number(value),
    }
}

#[tokio::test]
async fn production_health_and_sync_use_live_discord_control() {
    let fixture = ProviderFixture::new();
    let health = fixture.dispatch("health", Vec::new(), 1).await.last();
    assert!(health.ephemeral);
    assert!(health.content.contains("**Status:** OK"));
    assert!(health.content.contains("**Discord latency:** 17 ms"));
    assert!(health.content.contains("**Guilds:** 3"));
    assert_eq!(fixture.discord.health_calls.load(Ordering::SeqCst), 1);

    let sync = fixture.dispatch("sync", Vec::new(), 2).await.last();
    assert_eq!(
        sync.content,
        "✅ Synced 91 command(s) to 3 guild(s) and globally."
    );
    assert!(sync.ephemeral);
    assert_eq!(fixture.discord.sync_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_resetuser_requires_admin() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 1, 2, Some(1_500.0), Some(200.0), Some(0.06));

    let non_admin = fixture
        .dispatch_request(admin_command(
            "resetuser",
            vec![user_option("user", TARGET, "Target")],
            3,
            90_001,
            Some(0),
        ))
        .await
        .last();
    assert!(non_admin.ephemeral);
    assert!(non_admin.content.contains("Admin only"));
    assert_eq!(fixture.rating(TARGET).0, Some(1_500.0));

    let admin = fixture
        .dispatch("resetuser", vec![user_option("user", TARGET, "Target")], 4)
        .await
        .last();
    assert!(admin.ephemeral);
    assert!(admin.content.contains("Reset"));
    let remaining = Connection::open(fixture.database.path())
        .expect("open reset-user database")
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM players WHERE discord_id=?1 AND guild_id=?2
             )",
            params![
                i64::try_from(TARGET).unwrap(),
                i64::try_from(GUILD).unwrap()
            ],
            |row| row.get::<_, bool>(0),
        )
        .expect("load reset-user result");
    assert!(!remaining);
}

#[test]
fn test_monitoring_snapshot_reports_ok_db_and_runtime_fields() {
    let database = NamedTempFile::new().expect("temporary monitoring database");
    initialize_or_migrate(database.path()).expect("migrate monitoring database");
    let usage = UsageMonitor::default();
    usage.record_command(Some("admin health"));
    let monitoring = AdminMonitoring {
        database_path: database.path().to_path_buf(),
        usage,
        started_at: Utc::now() - chrono::Duration::seconds(3_661),
        git_sha: "abc123".to_owned(),
    };
    let snapshot = monitoring.snapshot(AdminDiscordHealth {
        guild_count: 2,
        latency_ms: Some(123),
    });
    assert_eq!(snapshot.status, "ok");
    assert!(snapshot.db_ok);
    assert!(snapshot.db_latency_ms.is_some());
    assert_eq!(snapshot.git_sha, "abc123");
    assert_eq!(snapshot.guild_count, 2);
    assert_eq!(snapshot.discord_latency_ms, Some(123));
    assert_eq!(snapshot.usage.command_total, 1);
    assert!(snapshot.uptime.contains("1h"));
}

#[test]
fn test_monitoring_snapshot_degrades_on_db_failure() {
    let directory = tempfile::tempdir().expect("temporary monitoring directory");
    let monitoring = AdminMonitoring {
        database_path: directory.path().join("missing").join("missing.db"),
        usage: UsageMonitor::default(),
        started_at: Utc::now(),
        git_sha: "abc123".to_owned(),
    };
    let snapshot = monitoring.snapshot(AdminDiscordHealth {
        guild_count: 0,
        latency_ms: None,
    });
    assert_eq!(snapshot.status, "degraded");
    assert!(!snapshot.db_ok);
    assert!(!snapshot.reasons.is_empty());
}

#[test]
fn test_monitoring_snapshot_rejects_empty_sqlite_file() {
    let database = NamedTempFile::new().expect("empty SQLite file");
    let monitoring = AdminMonitoring {
        database_path: database.path().to_path_buf(),
        usage: UsageMonitor::default(),
        started_at: Utc::now(),
        git_sha: "abc123".to_owned(),
    };
    let snapshot = monitoring.snapshot(AdminDiscordHealth {
        guild_count: 0,
        latency_ms: None,
    });
    assert_eq!(snapshot.status, "degraded");
    assert!(!snapshot.db_ok);
    assert!(
        snapshot
            .reasons
            .iter()
            .any(|reason| reason.contains("DB probe failed"))
    );
}

#[test]
fn test_format_health_snapshot_includes_requested_fields() {
    let database = NamedTempFile::new().expect("temporary monitoring database");
    initialize_or_migrate(database.path()).expect("migrate monitoring database");
    let usage = UsageMonitor::default();
    usage.record_command(Some("admin health"));
    usage.record_api_request("opendota");
    let text = format_health_snapshot(
        &AdminMonitoring {
            database_path: database.path().to_path_buf(),
            usage,
            started_at: Utc::now(),
            git_sha: "abc123".to_owned(),
        }
        .snapshot(AdminDiscordHealth {
            guild_count: 0,
            latency_ms: None,
        }),
    );
    assert!(text.contains("**Status:** OK"));
    assert!(text.contains("**Uptime:**"));
    assert!(text.contains("**Started:**"));
    assert!(text.contains("**Git SHA:** `abc123`"));
    assert!(text.contains("**Python:**"));
    assert!(text.contains("**DB:** ok"));
    assert!(text.contains("**Commands:** 1 total"));
    assert!(text.contains("**API requests:** opendota: 1"));
}

#[test]
fn test_monitoring_service_prefers_git_sha_env() {
    assert_eq!(
        short_git_sha_with(Some("abcdef1234567890".to_owned()), || {
            panic!("git fallback must not run when GIT_SHA is nonempty")
        }),
        "abcdef123456"
    );
}

#[test]
fn test_monitoring_service_uses_git_command_when_env_missing() {
    let called = AtomicBool::new(false);
    assert_eq!(
        short_git_sha_with(None, || {
            called.store(true, Ordering::SeqCst);
            Some("123456789abc\n".to_owned())
        }),
        "123456789abc"
    );
    assert!(called.load(Ordering::SeqCst));
}

#[tokio::test]
async fn production_admin_routes_share_lobby_match_and_correction_controls() {
    let fixture = ProviderFixture::new();
    let addfake = fixture
        .dispatch("addfake", vec![integer_option("count", 3)], 10)
        .await
        .last();
    assert_eq!(
        addfake.content,
        "✅ Added 3 fake user(s): FakeUser1, FakeUser2, FakeUser3"
    );
    assert_eq!(
        fixture.lobby.add_fake.lock().expect("add-fake lock").len(),
        1
    );

    let extension = fixture
        .dispatch(
            "extendbetting",
            vec![
                integer_option("minutes", 5),
                integer_option("pending_match_id", 88),
            ],
            11,
        )
        .await
        .last();
    assert!(extension.content.contains("All You Can Feed / Match #88"));
    assert!(extension.content.contains("<t:1300:R>"));
    assert_eq!(
        fixture.matches.extensions.lock().expect("extension lock")[0],
        AdminExtendBettingRequest {
            guild_id: i64::try_from(GUILD).unwrap(),
            actor_id: i64::try_from(ADMIN).unwrap(),
            minutes: 5,
            pending_match_id: 88,
        }
    );

    let correction = fixture
        .dispatch(
            "correctmatch",
            vec![
                integer_option("match_id", 99),
                string_option("correct_result", "dire"),
            ],
            12,
        )
        .await
        .last();
    assert!(correction.content.contains("**Match #99 corrected!**"));
    assert_eq!(
        fixture
            .corrections
            .requests
            .lock()
            .expect("correction lock")[0],
        AdminCorrectMatchRequest {
            guild_id: i64::try_from(GUILD).unwrap(),
            actor_id: i64::try_from(ADMIN).unwrap(),
            match_id: 99,
            correct_result: AdminCorrectMatchSide::Dire,
        }
    );
}

#[tokio::test]
async fn test_addfake_works_when_defer_fails() {
    let fixture = ProviderFixture::new();
    let responder = Arc::new(RecordingResponder {
        fail_defer: AtomicBool::new(true),
        ..RecordingResponder::default()
    });

    fixture
        .registry()
        .command_handler("admin")
        .expect("Admin handler")
        .handle(
            admin_command(
                "addfake",
                vec![integer_option("count", 3)],
                77,
                ADMIN,
                Some(MANAGE_GUILD),
            ),
            responder.clone(),
        )
        .await
        .expect("mutation survives failed defer");

    assert_eq!(
        fixture.lobby.add_fake.lock().expect("add-fake lock").len(),
        1
    );
    assert!(
        responder
            .responses
            .lock()
            .expect("response lock")
            .is_empty(),
        "an expired interaction cannot receive a followup"
    );
}

#[tokio::test]
async fn production_registeruser_uses_shared_opendota_registration_and_migrated_sqlite() {
    let fixture = ProviderFixture::new();
    let response = fixture
        .dispatch(
            "registeruser",
            vec![
                user_option("user", TARGET, "Target Player"),
                integer_option("steam_id", 12_345),
                integer_option("mmr", 2_500),
            ],
            20,
        )
        .await
        .last();
    assert!(response.ephemeral);
    assert!(response.content.contains("✅ Registered <@42003>!"));
    let connection = Connection::open(fixture.database.path()).expect("inspect Admin database");
    let player: (String, i64, i64) = connection
        .query_row(
            "SELECT discord_username,initial_mmr,guild_id FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![
                i64::try_from(TARGET).unwrap(),
                i64::try_from(GUILD).unwrap()
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("registered player");
    assert_eq!(player, ("Target Player".to_owned(), 2_500, 42_001));
    assert_eq!(
        connection
            .query_row(
                "SELECT steam_id FROM player_steam_ids WHERE discord_id=?1 AND is_primary=1",
                [i64::try_from(TARGET).unwrap()],
                |row| row.get::<_, i64>(0),
            )
            .expect("primary Steam account"),
        12_345
    );
}

#[tokio::test]
async fn test_setinitialrating_happy_path() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 2, 0, Some(1_500.0), Some(100.0), Some(0.07));
    let response = fixture
        .dispatch(
            "setrating",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rating", 2_500.0),
            ],
            30,
        )
        .await
        .last();
    assert_eq!(
        fixture.rating(TARGET),
        (Some(2_500.0), Some(100.0), Some(0.07))
    );
    assert!(response.content.contains("Set initial rating"));
    assert!(response.content.contains("RD kept at 100.0"));
    assert!(response.ephemeral);
}

#[tokio::test]
async fn test_setinitialrating_uses_new_player_rd_when_rating_data_is_missing() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 0, 0, None, None, None);
    fixture
        .dispatch(
            "setrating",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rating", 1_500.0),
            ],
            31,
        )
        .await;
    assert_eq!(
        fixture.rating(TARGET),
        (Some(1_500.0), Some(350.0), Some(0.06))
    );
}

#[tokio::test]
async fn test_setinitialrating_rejects_too_many_games() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 1_000, 0, None, None, None);
    let response = fixture
        .dispatch(
            "setrating",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rating", 1_200.0),
            ],
            32,
        )
        .await
        .last();
    assert!(
        response
            .content
            .to_ascii_lowercase()
            .contains("too many games")
    );
    assert_eq!(fixture.rating(TARGET), (None, None, None));
}

#[tokio::test]
async fn test_adjust_rating_allows_many_games_and_preserves_rd() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 1_000, 0, Some(1_500.0), Some(110.0), Some(0.08));
    let response = fixture
        .dispatch_adjust(
            "rating",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rating", 2_400.0),
            ],
            33,
            ADMIN,
            Some(MANAGE_GUILD),
        )
        .await
        .last();
    assert_eq!(
        fixture.rating(TARGET),
        (Some(2_400.0), Some(110.0), Some(0.08))
    );
    assert!(response.content.contains("RD kept at 110.0"));
}

#[tokio::test]
async fn test_admin_rating_commands_allow_values_above_seed_scale_setinitialrating() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 0, 0, Some(3_100.0), Some(110.0), Some(0.08));
    fixture
        .dispatch(
            "setrating",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rating", 3_200.0),
            ],
            34,
        )
        .await;
    assert_eq!(fixture.rating(TARGET).0, Some(3_200.0));
}

#[tokio::test]
async fn test_admin_rating_commands_allow_values_above_seed_scale_adjust_rating() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 0, 0, Some(3_100.0), Some(110.0), Some(0.08));
    fixture
        .dispatch_adjust(
            "rating",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rating", 3_200.0),
            ],
            35,
            ADMIN,
            Some(MANAGE_GUILD),
        )
        .await;
    assert_eq!(fixture.rating(TARGET).0, Some(3_200.0));
}

#[tokio::test]
async fn test_admin_rating_commands_reject_values_above_sanity_ceiling_setinitialrating() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 0, 0, Some(1_500.0), Some(110.0), Some(0.08));
    let response = fixture
        .dispatch(
            "setrating",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rating", 6_001.0),
            ],
            36,
        )
        .await
        .last();
    assert!(
        response
            .content
            .to_ascii_lowercase()
            .contains("must be between")
    );
    assert_eq!(fixture.rating(TARGET).0, Some(1_500.0));
}

#[tokio::test]
async fn test_admin_rating_commands_reject_values_above_sanity_ceiling_adjust_rating() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 0, 0, Some(1_500.0), Some(110.0), Some(0.08));
    let response = fixture
        .dispatch_adjust(
            "rating",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rating", 6_001.0),
            ],
            37,
            ADMIN,
            Some(MANAGE_GUILD),
        )
        .await
        .last();
    assert!(
        response
            .content
            .to_ascii_lowercase()
            .contains("must be between")
    );
    assert_eq!(fixture.rating(TARGET).0, Some(1_500.0));
}

#[tokio::test]
async fn test_adjust_rd_preserves_rating_and_volatility() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 1_000, 0, Some(1_512.0), Some(95.0), Some(0.07));
    let response = fixture
        .dispatch_adjust(
            "rd",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rd", 180.0),
            ],
            38,
            ADMIN,
            Some(MANAGE_GUILD),
        )
        .await
        .last();
    assert_eq!(
        fixture.rating(TARGET),
        (Some(1_512.0), Some(180.0), Some(0.07))
    );
    assert!(response.content.contains("rating kept at 1512"));
}

#[tokio::test]
async fn test_adjust_rating_rejects_non_admin() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 0, 0, Some(1_500.0), Some(100.0), Some(0.07));
    let response = fixture
        .dispatch_adjust(
            "rating",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rating", 2_400.0),
            ],
            39,
            999_999,
            Some(0),
        )
        .await
        .last();
    assert!(response.content.contains("Admin only"));
    assert_eq!(fixture.rating(TARGET).0, Some(1_500.0));
}

#[tokio::test]
async fn test_adjust_rd_rejects_invalid_rd() {
    let fixture = ProviderFixture::new();
    fixture.player(TARGET, 0, 0, Some(1_500.0), Some(100.0), Some(0.07));
    let response = fixture
        .dispatch_adjust(
            "rd",
            vec![
                user_option("user", TARGET, "Target"),
                number_option("rd", 351.0),
            ],
            40,
            ADMIN,
            Some(MANAGE_GUILD),
        )
        .await
        .last();
    assert!(response.content.contains("RD must be between 0 and 350"));
    assert_eq!(fixture.rating(TARGET).1, Some(100.0));
}
