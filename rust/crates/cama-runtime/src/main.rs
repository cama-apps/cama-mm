//! Cama Rust process entrypoint.

mod herogrid_provider;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cama_app::ai_http::production_ai_service_from_settings;
use cama_app::draft::{DraftStateManager, SqliteDraftStatePersistence};
use cama_app::service_container::ServiceContainer;
use cama_db::{
    audit_database,
    schema_manager::{MigrationSettings, initialize_or_migrate_with_settings},
};
use cama_runtime::gateway::{GatewayError, GatewaySession};
use cama_runtime::inventory;
use cama_runtime::match_provider::production_betting_flavor;
use cama_runtime::process_lock::ProcessLock;
use cama_runtime::registration::InteractionResponseError;
use cama_runtime::{
    AdminMatchCorrectionRuntime, AdminRegistrationProvider, AdminRuntimePorts,
    AdvancedStatsRegistrationProvider, ApplicationConfig, AskRegistrationProvider,
    BlameLukeRegistrationProvider, CommandSpec, DigBonusRuntime, DigRegistrationProvider,
    DiscordToken, DotaInfoRegistrationProvider, DraftRegistrationProvider,
    DuelRegistrationProvider, EnrichmentRegistrationProvider, GatewayEventObservers,
    GatewaySessionEnd, GatewayTransport, GlobalInteractionHooks, HealthReporter,
    InfoRegistrationProvider, InteractionAllowedMentions, InteractionHandler,
    InteractionHandlerError, InteractionRequest, InteractionResponder, InteractionResponse,
    LifecycleEvent, LobbyRegistrationProvider, LobbyRuntimeConfig, MafiaRegistrationProvider,
    ManaRegistrationProvider, MatchRegistrationProvider, PetRegistrationProvider,
    PlayerRegistrationProvider, PlayerTriviaRegistrationProvider, PredictionRegistrationProvider,
    PredictionRuntimePorts, ProfileRegistrationProvider, RatingAnalysisRegistrationProvider,
    RawReactionObservers, RegistryBuilder, ReminderRegistrationProvider, Runtime, RuntimeConfig,
    ScoutRegistrationProvider, SerenityDiscordTransport, SerenityGateway, ShopRegistrationProvider,
    SqliteDatabaseInitializer, SurveyRegistrationProvider, TaxRegistrationProvider,
    TriviaRegistrationProvider, UsageMonitor, VanityTaxGatewayObserver,
    WrappedRegistrationProvider, check_health, curfew_sweep_worker_spec, dig_weather_worker_spec,
    duel_challenges_worker_spec, economy_events_worker_spec, first_game_pool_worker_spec,
    manashop_debt_worker_spec, pet_sweep_worker_spec_with_ai, prediction_digest_worker_spec,
    prediction_refresh_worker_spec, validate_production_registry,
};
use cama_runtime::{
    BettingRegistrationProvider, BettingRuntimeConfig, match_post_match_debrief_port,
    match_wager_refresh_port,
};
use rusqlite::Connection;
use tokio::sync::oneshot;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::herogrid_provider::HeroGridRegistrationProvider;

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Serve,
    DatabaseCheck {
        path: PathBuf,
    },
    DatabaseAdmit {
        path: PathBuf,
        source: PathBuf,
    },
    HealthCheck {
        path: PathBuf,
        maximum_age: Duration,
    },
    HealthSmoke {
        path: PathBuf,
    },
    Inventory,
    CatalogCheck {
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    initialize_logging();
    match parse_command(env::args().skip(1)) {
        Ok(Command::Serve) => run_serve().await,
        Ok(Command::DatabaseCheck { path }) => run_db_check(path),
        Ok(Command::DatabaseAdmit { path, source }) => run_db_admit(path, source),
        Ok(Command::HealthCheck { path, maximum_age }) => run_health_check(path, maximum_age),
        Ok(Command::HealthSmoke { path }) => run_health_smoke(path).await,
        Ok(Command::Inventory) => {
            print_inventory();
            ExitCode::SUCCESS
        }
        Ok(Command::CatalogCheck { path }) => run_catalog_check(path),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(64)
        }
    }
}

fn initialize_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(false)
        .json()
        .flatten_event(true)
        .with_current_span(false)
        .with_span_list(false)
        .try_init();
}

fn parse_command(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    match args.next().as_deref() {
        None | Some("serve") => {
            reject_remaining(args, "serve")?;
            Ok(Command::Serve)
        }
        Some("inventory") => {
            reject_remaining(args, "inventory")?;
            Ok(Command::Inventory)
        }
        Some("catalog-check") => parse_catalog_check(args),
        Some("db-admit") => parse_db_admit(args),
        Some("db-check") => parse_db_check(args),
        Some("health-check") => parse_health_check(args),
        Some("health-smoke") => parse_health_smoke(args),
        Some(command) => Err(format!(
            "unknown command {command:?}; expected `serve`, `db-admit`, `db-check`, `health-check`, `health-smoke`, `catalog-check`, or `inventory`"
        )),
    }
}

fn parse_catalog_check(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--path" => {
                explicit_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--path requires a path".to_owned())?,
                ));
            }
            _ => return Err(format!("unexpected catalog-check argument {argument:?}")),
        }
    }
    Ok(Command::CatalogCheck {
        path: explicit_path
            .unwrap_or_else(|| PathBuf::from(cama_app::dotabase_sqlite::PRODUCTION_DOTABASE_PATH)),
    })
}

fn reject_remaining(mut args: impl Iterator<Item = String>, command: &str) -> Result<(), String> {
    args.next().map_or(Ok(()), |argument| {
        Err(format!("unexpected {command} argument {argument:?}"))
    })
}

fn parse_db_check(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--db-path" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--db-path requires a path".to_owned())?;
                explicit_path = Some(PathBuf::from(value));
            }
            _ => return Err(format!("unexpected db-check argument {argument:?}")),
        }
    }

    let path = explicit_path
        .or_else(|| env::var_os("DB_PATH").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("cama_shuffle.db"));
    Ok(Command::DatabaseCheck { path })
}

fn parse_db_admit(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    let mut source = None;
    let mut confirmation_count = 0;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--db-path" => {
                explicit_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--db-path requires a path".to_owned())?,
                ));
            }
            "--source-db" => {
                source = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--source-db requires a path".to_owned())?,
                ));
            }
            "--disposable-copy" => confirmation_count += 1,
            _ => return Err(format!("unexpected db-admit argument {argument:?}")),
        }
    }
    if confirmation_count != 1 {
        return Err("db-admit requires exactly one --disposable-copy".to_owned());
    }
    Ok(Command::DatabaseAdmit {
        path: explicit_path.ok_or_else(|| "db-admit requires --db-path".to_owned())?,
        source: source.ok_or_else(|| "db-admit requires --source-db".to_owned())?,
    })
}

fn parse_health_check(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    let mut maximum_age = cama_runtime::DEFAULT_MAX_HEARTBEAT_AGE;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--db-path" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--db-path requires a path".to_owned())?;
                explicit_path = Some(PathBuf::from(value));
            }
            "--max-age-seconds" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--max-age-seconds requires a positive integer".to_owned())?;
                let seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--max-age-seconds requires a positive integer".to_owned())?;
                if seconds == 0 {
                    return Err("--max-age-seconds requires a positive integer".to_owned());
                }
                maximum_age = Duration::from_secs(seconds);
            }
            _ => return Err(format!("unexpected health-check argument {argument:?}")),
        }
    }

    let path = explicit_path
        .or_else(|| env::var_os("DB_PATH").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("cama_shuffle.db"));
    Ok(Command::HealthCheck { path, maximum_age })
}

fn parse_health_smoke(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut explicit_path = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--db-path" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--db-path requires a path".to_owned())?;
                explicit_path = Some(PathBuf::from(value));
            }
            _ => return Err(format!("unexpected health-smoke argument {argument:?}")),
        }
    }

    Ok(Command::HealthSmoke {
        path: explicit_path
            .or_else(|| env::var_os("DB_PATH").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("cama_shuffle.db")),
    })
}

const HEALTH_SMOKE_COMMAND: &str = "health-smoke";
const HEALTH_SMOKE_RESPONSE: &str = "health-smoke Discord response recorded";

#[derive(Debug)]
struct HealthSmokeCommand {
    database_path: PathBuf,
}

#[async_trait]
impl InteractionHandler for HealthSmokeCommand {
    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        if request.root_name() != HEALTH_SMOKE_COMMAND {
            return Err(InteractionHandlerError::Handler(format!(
                "unexpected health-smoke command {:?}",
                request.root_name()
            )));
        }
        let path = self.database_path.clone();
        tokio::task::spawn_blocking(move || {
            let connection = Connection::open(&path)
                .map_err(|error| format!("health-smoke database write open failed: {error}"))?;
            connection
                .execute(
                    "INSERT INTO app_kv (guild_id, key, value)
                     VALUES (0, '__rust_health_smoke__', 'loopback-discord-write')
                     ON CONFLICT(guild_id, key) DO UPDATE SET value=excluded.value",
                    [],
                )
                .map_err(|error| format!("health-smoke database write failed: {error}"))?;
            Ok::<(), String>(())
        })
        .await
        .map_err(|error| {
            InteractionHandlerError::Handler(format!(
                "health-smoke database write task failed: {error}"
            ))
        })??;
        responder
            .respond(
                InteractionResponse::message(HEALTH_SMOKE_RESPONSE)
                    .ephemeral()
                    .without_mentions(),
            )
            .await
            .map_err(|error| InteractionHandlerError::Handler(error.to_string()))
    }
}

#[derive(Debug, Default)]
struct HealthSmokeResponder {
    response: Mutex<Option<InteractionResponse>>,
}

impl HealthSmokeResponder {
    fn recorded_response(&self) -> Result<InteractionResponse, GatewayError> {
        self.response
            .lock()
            .map_err(|_| GatewayError::Fatal("health-smoke response lock poisoned".to_owned()))?
            .clone()
            .ok_or_else(|| {
                GatewayError::Fatal("health-smoke command produced no Discord response".to_owned())
            })
    }
}

#[async_trait]
impl InteractionResponder for HealthSmokeResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        let mut recorded = self
            .response
            .lock()
            .map_err(|_| InteractionResponseError::new("health-smoke response lock poisoned"))?;
        if recorded.is_some() {
            return Err(InteractionResponseError::new(
                "health-smoke command responded more than once",
            ));
        }
        *recorded = Some(response);
        Ok(())
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        Err(InteractionResponseError::new(
            "health-smoke command unexpectedly deferred",
        ))
    }

    async fn followup(
        &self,
        _response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        Err(InteractionResponseError::new(
            "health-smoke command unexpectedly sent a follow-up",
        ))
    }
}

/// A network-free gateway used only by the image-level health smoke.
///
/// This drives the same lifecycle and health reporter as `serve`, but never
/// constructs a Serenity client or opens a Discord socket. The command-tree
/// registration and typed interaction response are the loopback Discord seam;
/// the command's accompanying `app_kv` transaction proves that a
/// current-schema runtime write is possible while the process is healthy.
#[derive(Debug)]
struct HealthSmokeGateway {
    responder: Arc<HealthSmokeResponder>,
}

#[async_trait]
impl GatewayTransport for HealthSmokeGateway {
    async fn run_session(
        &mut self,
        session: GatewaySession,
    ) -> Result<GatewaySessionEnd, GatewayError> {
        let _ = session.events.send(LifecycleEvent::Ready {
            bot_user_id: 0,
            guild_count: 0,
        });
        let _ = session
            .events
            .send(LifecycleEvent::GatewayShardStageChanged {
                shard_id: 0,
                connected: true,
                stage: "loopback".to_owned(),
            });
        let handler = session
            .registry
            .command_handler(HEALTH_SMOKE_COMMAND)
            .ok_or_else(|| {
                GatewayError::Fatal("health-smoke command is not registered".to_owned())
            })?;
        handler
            .handle(
                InteractionRequest::Command {
                    interaction_id: 1,
                    name: HEALTH_SMOKE_COMMAND.to_owned(),
                    user_id: 1,
                    user_display_name: "health-smoke".to_owned(),
                    guild_id: Some(1),
                    channel_id: Some(1),
                    member_permissions: None,
                    options: Vec::new(),
                },
                self.responder.clone(),
            )
            .await
            .map_err(|error| GatewayError::Fatal(error.to_string()))?;
        let response = self.responder.recorded_response()?;
        if response.content != HEALTH_SMOKE_RESPONSE
            || !response.ephemeral
            || response.allowed_mentions != InteractionAllowedMentions::None
            || !response.embeds.is_empty()
            || !response.attachments.is_empty()
            || !response.components.is_empty()
        {
            return Err(GatewayError::Fatal(
                "health-smoke Discord response did not preserve its typed contract".to_owned(),
            ));
        }
        let command_count = session.registry.commands().len();
        let _ = session.events.send(LifecycleEvent::CommandsRegistered {
            command_count,
            component_route_count: session.registry.component_routes().len(),
            synchronized: command_count == 1,
        });

        let mut shutdown = session.shutdown;
        loop {
            if *shutdown.borrow() {
                return Ok(GatewaySessionEnd::Shutdown);
            }
            if shutdown.changed().await.is_err() || *shutdown.borrow() {
                return Ok(GatewaySessionEnd::Shutdown);
            }
        }
    }
}

async fn run_health_smoke(path: PathBuf) -> ExitCode {
    let result = run_health_smoke_inner(path).await;
    match result {
        Ok(()) => {
            println!(
                "health_smoke=passed gateway=loopback discord_write=interaction_response database_write=app_kv"
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("health-smoke failed: {error}");
            ExitCode::from(1)
        }
    }
}

async fn run_health_smoke_inner(path: PathBuf) -> Result<(), String> {
    let database_path = if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .map_err(|error| format!("could not resolve current directory: {error}"))?
            .join(path)
    };
    let health_reporter = HealthReporter::initialize(&database_path)
        .map_err(|error| format!("could not initialize health reporter: {error}"))?;
    let config = RuntimeConfig {
        token: DiscordToken::parse("health-smoke-token")
            .map_err(|error| format!("could not construct smoke token: {error}"))?,
        db_path: database_path.clone(),
        reconnect_initial: Duration::ZERO,
        reconnect_max: Duration::ZERO,
        rust_cutover_candidate: false,
    };
    let mut registry = RegistryBuilder::default();
    registry
        .command(CommandSpec {
            name: HEALTH_SMOKE_COMMAND.to_owned(),
            description: "Verify the loopback Discord and database write seams".to_owned(),
            options: Vec::new(),
            handler: Arc::new(HealthSmokeCommand {
                database_path: database_path.clone(),
            }),
        })
        .map_err(|error| format!("could not register health-smoke command: {error}"))?;
    let responder = Arc::new(HealthSmokeResponder::default());
    let database_initialization = SqliteDatabaseInitializer::default()
        .initialize(&database_path)
        .await
        .map_err(|error| format!("could not initialize database schema: {error}"))?;
    let runtime = Runtime::new(
        config,
        registry.build(),
        HealthSmokeGateway {
            responder: Arc::clone(&responder),
        },
        database_initialization,
    );
    let health_events = runtime.events().subscribe();
    let health_task = tokio::spawn(async move { health_reporter.run(health_events).await });
    let (shutdown_sender, shutdown_receiver) = oneshot::channel();
    let runtime_task = tokio::spawn(async move {
        runtime
            .run_until(async move {
                let _ = shutdown_receiver.await;
            })
            .await
    });

    let startup_result = async {
        for _ in 0..120 {
            if let Ok(report) = check_health(&database_path, Duration::from_secs(30))
                && report.snapshot.is_healthy()
            {
                    let value: String = tokio::task::spawn_blocking({
                        let path = database_path.clone();
                        move || {
                            let connection = Connection::open(path)
                                .map_err(|error| format!("could not verify smoke write: {error}"))?;
                            connection
                                .query_row(
                                    "SELECT value FROM app_kv WHERE guild_id=0 AND key='__rust_health_smoke__'",
                                    [],
                                    |row| row.get(0),
                                )
                                .map_err(|error| format!("smoke write was not committed: {error}"))
                        }
                    })
                    .await
                    .map_err(|error| format!("smoke write verification task failed: {error}"))??;
                    if value != "loopback-discord-write" {
                        return Err(format!("unexpected smoke write value {value:?}"));
                    }
                    return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Err("runtime did not reach a healthy ready state within 3 seconds".to_owned())
    }
    .await;

    let _ = shutdown_sender.send(());
    let runtime_result = runtime_task
        .await
        .map_err(|error| format!("runtime task failed: {error}"))?
        .map_err(|error| format!("runtime stopped with an error: {error}"));
    let health_result = health_task
        .await
        .map_err(|error| format!("health reporter task failed: {error}"))?
        .map_err(|error| format!("health reporter stopped with an error: {error}"));
    startup_result?;
    runtime_result?;
    health_result?;
    if check_health(&database_path, Duration::from_secs(30)).is_ok() {
        return Err("health-check accepted the stopped runtime marker".to_owned());
    }
    Ok(())
}

async fn run_serve() -> ExitCode {
    let application_config = match ApplicationConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            error!(%error, "invalid Rust runtime configuration");
            return ExitCode::from(64);
        }
    };
    let config = application_config.runtime.clone();
    let _process_lock = match acquire_runtime_lock(&config.db_path) {
        Ok(process_lock) => process_lock,
        Err(error) => {
            error!(%error, "Rust runtime process lock refused startup");
            return ExitCode::from(1);
        }
    };
    let health_reporter = match HealthReporter::initialize(&config.db_path) {
        Ok(reporter) => reporter,
        Err(error) => {
            error!(%error, "could not initialize runtime health reporting");
            return ExitCode::from(1);
        }
    };
    let database_initialization = match SqliteDatabaseInitializer::with_migration_settings(
        application_config.migration_settings(),
    )
    .initialize(&config.db_path)
    .await
    {
        Ok(report) => report,
        Err(error) => {
            error!(%error, "Rust database schema initialization refused startup");
            return ExitCode::from(1);
        }
    };
    info!(
        db_path = %database_initialization.path.display(),
        applied_migrations = database_initialization.applied_migrations,
        required_migrations = database_initialization.required_migrations,
        newly_applied_migrations = database_initialization.newly_applied_migrations,
        created_tables = database_initialization.created_tables,
        rebuilt_tables = database_initialization.rebuilt_tables,
        "Rust database schema initialization and integrity check complete"
    );

    // Build the concrete OpenDota transport before any gateway connection.
    // Construction is offline, but a TLS/executor failure is fatal because
    // serving without this production dependency would silently regress the
    // Python lift-and-shift contract.
    let usage_monitor = UsageMonitor::default();
    let opendota = match application_config.opendota_services() {
        Ok(services) => Arc::new(services.with_request_observer({
            let usage_monitor = usage_monitor.clone();
            move |provider| usage_monitor.record_api_request(provider)
        })),
        Err(error) => {
            error!(%error, "OpenDota transport construction refused startup");
            return ExitCode::from(1);
        }
    };

    // Construct the same typed service graph before connecting. Concrete
    // command providers attach these components to `RegistryBuilder` as each
    // production interaction adapter is ported.
    let production_ai_service = match application_config
        .llm
        .selected_api_key
        .as_ref()
        .filter(|key| !key.expose().is_empty())
    {
        Some(key) => match production_ai_service_from_settings(
            application_config.llm.model.clone(),
            key.expose().to_owned(),
            application_config.values.ai_timeout_seconds,
            application_config.values.ai_max_tokens,
            &config.db_path,
        ) {
            Ok(service) => Some(service),
            Err(error) => {
                error!(%error, "AI provider construction refused startup");
                return ExitCode::from(1);
            }
        },
        None => None,
    };
    let mut service_options =
        application_config.service_container_options(Some(Arc::clone(&opendota)));
    service_options.production_ai_service = production_ai_service.clone();
    let mut service_container = ServiceContainer::new(&config.db_path, service_options);
    service_container.initialize();
    let vanity_tax_service = match service_container.components() {
        Ok(components) => Arc::clone(&components.vanity_tax_service),
        Err(error) => {
            error!(%error, "could not resolve vanity-tax runtime service");
            return ExitCode::from(1);
        }
    };
    let discord_transport = Arc::new(SerenityDiscordTransport::with_gamba_channel_id(
        application_config.channels.gamba,
    ));
    let draft_states = Arc::new(DraftStateManager::default());
    let lobby_provider = match LobbyRegistrationProvider::new(
        &config.db_path,
        match LobbyRuntimeConfig::from_application_config(&application_config) {
            Ok(config) => config,
            Err(error) => {
                error!(%error, "lobby runtime configuration refused startup");
                return ExitCode::from(1);
            }
        },
        Arc::clone(&draft_states),
        discord_transport.clone(),
    ) {
        Ok(provider) => provider,
        Err(error) => {
            error!(%error, "lobby runtime construction refused startup");
            return ExitCode::from(1);
        }
    };
    let registration_provider = PlayerRegistrationProvider::new(
        &config.db_path,
        Arc::clone(&opendota),
        discord_transport.clone(),
        &application_config,
    );
    let dota_info_provider = match DotaInfoRegistrationProvider::production() {
        Ok(provider) => provider,
        Err(error) => {
            error!(%error, "bundled Dotabase catalog refused startup");
            return ExitCode::from(1);
        }
    };
    let enrichment_provider = match EnrichmentRegistrationProvider::new(
        &config.db_path,
        &application_config,
        Arc::clone(&opendota),
    ) {
        Ok(provider) => provider,
        Err(error) => {
            error!(%error, "enrichment runtime construction refused startup");
            return ExitCode::from(1);
        }
    };
    let match_provider = match MatchRegistrationProvider::new(
        &config.db_path,
        &application_config,
        Arc::clone(&vanity_tax_service),
        lobby_provider.match_lobby_port(),
        enrichment_provider.recorded_match_discovery(),
        discord_transport.clone(),
    ) {
        Ok(provider) => provider,
        Err(error) => {
            error!(%error, "match runtime construction refused startup");
            return ExitCode::from(1);
        }
    };
    let betting_provider = BettingRegistrationProvider::with_runtime_config_and_vanity_tax(
        config.db_path.clone(),
        BettingRuntimeConfig::from_application_config(&application_config),
        discord_transport.clone(),
        vanity_tax_service.clone() as Arc<dyn cama_runtime::betting_provider::BettingVanityTaxPort>,
    );
    betting_provider.set_wager_refresh_port(match_wager_refresh_port(match_provider.clone()));
    if let Err(error) = match_provider
        .attach_post_match_debrief(match_post_match_debrief_port(betting_provider.clone()))
    {
        error!(%error, "match/betting post-match debrief composition refused startup");
        return ExitCode::from(1);
    }
    if let Err(error) = match_provider.attach_betting_flavor(production_betting_flavor(
        &config.db_path,
        &application_config,
        production_ai_service.clone(),
    )) {
        error!(%error, "match/betting flavor composition refused startup");
        return ExitCode::from(1);
    }
    let draft_provider = match DraftRegistrationProvider::new_with_reminder_scheduler_and_neon(
        &config.db_path,
        &application_config,
        lobby_provider.match_lobby_port(),
        Arc::clone(&draft_states),
        discord_transport.clone(),
        Arc::new(match_provider.clone()),
        registration_provider.draft_neon_observer(),
    )
    .and_then(|provider| {
        provider.with_persistence(Arc::new(SqliteDraftStatePersistence::new(&config.db_path)))
    }) {
        Ok(provider) => provider,
        Err(error) => {
            error!(%error, "draft runtime construction refused startup");
            return ExitCode::from(1);
        }
    };
    let admin_match_correction = match AdminMatchCorrectionRuntime::new(
        &config.db_path,
        &application_config,
        match_provider.correction_reward_control(),
        Arc::clone(&vanity_tax_service),
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            error!(%error, "admin match-correction runtime construction refused startup");
            return ExitCode::from(1);
        }
    };
    let admin_provider = AdminRegistrationProvider::new(
        &config.db_path,
        &application_config,
        Arc::clone(&opendota),
        AdminRuntimePorts {
            discord: discord_transport.clone(),
            discord_control: discord_transport.clone(),
            lobby: lobby_provider.admin_control(),
            matches: match_provider.admin_control(),
            match_corrections: admin_match_correction.control(),
        },
        usage_monitor.clone(),
    );
    let reminder_provider = ReminderRegistrationProvider::new(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
    );
    if let Err(error) = match_provider.attach_reminder_hooks(reminder_provider.hooks()) {
        error!(%error, "match reminder composition refused startup");
        return ExitCode::from(1);
    }
    let pet_provider = PetRegistrationProvider::new(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
        reminder_provider.hooks(),
        production_ai_service.clone(),
    );
    let duel_provider = DuelRegistrationProvider::new(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
        production_ai_service.clone(),
    );
    let mafia_provider = MafiaRegistrationProvider::new(
        &config.db_path,
        &application_config,
        Arc::clone(&vanity_tax_service),
        discord_transport.clone(),
    );
    let prediction_provider = PredictionRegistrationProvider::from_ports(
        &config.db_path,
        &application_config,
        Arc::clone(&vanity_tax_service),
        PredictionRuntimePorts {
            command_discord: discord_transport.clone(),
            market_discord: discord_transport.clone(),
            gamba_guilds: discord_transport.clone(),
            discord: discord_transport.clone(),
            ai: production_ai_service.clone(),
        },
    );
    let trivia_provider = match TriviaRegistrationProvider::new(
        &config.db_path,
        &application_config,
        Arc::clone(&vanity_tax_service),
        discord_transport.clone(),
        Some(reminder_provider.hooks()),
    ) {
        Ok(provider) => provider,
        Err(error) => {
            error!(%error, "trivia runtime construction refused startup");
            return ExitCode::from(1);
        }
    };
    let dig_bonus_runtime = DigBonusRuntime::from_application_config(
        &config.db_path,
        &application_config,
        trivia_provider.catalog(),
        betting_provider.clone(),
        discord_transport.clone(),
    );
    let dig_provider = match DigRegistrationProvider::production(
        &config.db_path,
        &application_config,
        Arc::clone(&vanity_tax_service),
        discord_transport.clone(),
        Some(reminder_provider.hooks()),
        production_ai_service.clone(),
        Arc::new(dig_bonus_runtime.clone()),
    ) {
        Ok(provider) => provider,
        Err(error) => {
            error!(%error, "Dig runtime construction refused startup");
            return ExitCode::from(1);
        }
    };
    let mana_provider = ManaRegistrationProvider::new(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
    );
    let player_trivia_provider = PlayerTriviaRegistrationProvider::new(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
    );
    let info_provider = InfoRegistrationProvider::new(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
    );
    let rating_analysis_provider =
        match RatingAnalysisRegistrationProvider::new(&config.db_path, &application_config) {
            Ok(provider) => provider,
            Err(error) => {
                error!(%error, "rating-analysis runtime construction refused startup");
                return ExitCode::from(1);
            }
        };
    let blame_luke_provider = BlameLukeRegistrationProvider::new(&config.db_path);
    let tax_provider = TaxRegistrationProvider::new(
        &config.db_path,
        &application_config,
        Arc::clone(&vanity_tax_service),
    );
    let scout_provider =
        match ScoutRegistrationProvider::new(&config.db_path, Arc::clone(&draft_states)) {
            Ok(provider) => provider,
            Err(error) => {
                error!(%error, "Scout runtime construction refused startup");
                return ExitCode::from(1);
            }
        };
    let wrapped_provider = WrappedRegistrationProvider::new(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
    );
    let shop_provider = match ShopRegistrationProvider::new(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
        production_ai_service.clone(),
    ) {
        Ok(provider) => provider,
        Err(error) => {
            error!(%error, "Shop runtime construction refused startup");
            return ExitCode::from(1);
        }
    };
    let survey_provider =
        match SurveyRegistrationProvider::new(&config.db_path, discord_transport.clone()) {
            Ok(provider) => provider,
            Err(error) => {
                error!(%error, "survey runtime construction refused startup");
                return ExitCode::from(1);
            }
        };
    if let Err(error) =
        lobby_provider.set_join_observer(registration_provider.lobby_join_observer())
    {
        error!(%error, "registration lobby-notification composition refused startup");
        return ExitCode::from(1);
    }
    let gateway_observers = GatewayEventObservers::new(vec![
        Arc::new(VanityTaxGatewayObserver::new(Arc::clone(
            &vanity_tax_service,
        ))),
        lobby_provider.gateway_observer(),
        match_provider.gateway_observer(),
        admin_match_correction.gateway_observer(),
        reminder_provider.gateway_observer(),
        duel_provider.gateway_observer(),
        betting_provider.gateway_observer(),
        mafia_provider.gateway_observer(),
        registration_provider.region_backfill_observer(),
        trivia_provider.gateway_observer(),
        dig_provider.gateway_observer(),
        blame_luke_provider.gateway_observer(),
        survey_provider.gateway_observer(),
        draft_provider.gateway_observer(),
    ]);
    let raw_reaction_observers =
        RawReactionObservers::new(vec![lobby_provider.raw_reaction_observer()]);
    let global_interaction_hooks = GlobalInteractionHooks::new(usage_monitor);

    let mut registry = RegistryBuilder::default();
    if let Err(error) = registry.add_provider(&HeroGridRegistrationProvider::new(
        &config.db_path,
        Arc::clone(&draft_states),
    )) {
        error!(%error, "could not register Hero Grid command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&scout_provider) {
        error!(%error, "could not register Scout command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&wrapped_provider) {
        error!(%error, "could not register Wrapped command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&shop_provider) {
        error!(%error, "could not register Shop command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&survey_provider) {
        error!(%error, "could not register survey command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&registration_provider) {
        error!(%error, "could not register player registration command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&ProfileRegistrationProvider::new(
        &config.db_path,
        Arc::clone(&opendota),
    )) {
        error!(%error, "could not register profile command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&dota_info_provider) {
        error!(%error, "could not register Dota information command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&enrichment_provider) {
        error!(%error, "could not register enrichment command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&reminder_provider) {
        error!(%error, "could not register reminder command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&pet_provider) {
        error!(%error, "could not register conditional Pet command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&duel_provider) {
        error!(%error, "could not register duel command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&mafia_provider) {
        error!(%error, "could not register Mafia command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&prediction_provider) {
        error!(%error, "could not register prediction command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&trivia_provider) {
        error!(%error, "could not register trivia command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&dig_bonus_runtime) {
        error!(%error, "could not register Dig bonus component provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&dig_provider) {
        error!(%error, "could not register Dig command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&mana_provider) {
        error!(%error, "could not register mana command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&player_trivia_provider) {
        error!(%error, "could not register Player Trivia command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&info_provider) {
        error!(%error, "could not register information command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&rating_analysis_provider) {
        error!(%error, "could not register rating-analysis command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&blame_luke_provider) {
        error!(%error, "could not register Blame Luke command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&tax_provider) {
        error!(%error, "could not register Tax Man command provider");
        return ExitCode::from(1);
    }
    if let Err(error) =
        registry.add_provider(&AdvancedStatsRegistrationProvider::new(&config.db_path))
    {
        error!(%error, "could not register advanced-statistics command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&lobby_provider) {
        error!(%error, "could not register lobby command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&match_provider) {
        error!(%error, "could not register match command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&betting_provider) {
        error!(%error, "could not register betting command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&draft_provider) {
        error!(%error, "could not register draft command provider");
        return ExitCode::from(1);
    }
    if let Err(error) = registry.add_provider(&admin_provider) {
        error!(%error, "could not register admin command provider");
        return ExitCode::from(1);
    }
    if let Some(ai_service) = production_ai_service.as_ref() {
        let ask_provider = AskRegistrationProvider::new(
            &config.db_path,
            Arc::clone(ai_service),
            application_config.values.ai_features_enabled,
            application_config.values.ai_rate_limit_requests,
            application_config.values.ai_rate_limit_window,
        );
        if let Err(error) = registry.add_provider(&ask_provider) {
            error!(%error, "could not register ask command provider");
            return ExitCode::from(1);
        }
    }
    if inventory::global_command_sync_allowed(
        application_config.runtime.rust_cutover_candidate,
        inventory::required_count(),
    ) {
        registry.enable_global_command_sync();
    }
    let registry = registry.build();
    if let Err(error) =
        validate_production_registry(&registry, application_config.channels.pet.is_some())
    {
        error!(%error, "production command-tree contract refused startup");
        return ExitCode::from(1);
    }
    let manashop_debt_worker =
        manashop_debt_worker_spec(&config.db_path, discord_transport.clone());
    let duel_challenges_worker = duel_challenges_worker_spec(
        &config.db_path,
        application_config.channels.duel,
        Arc::clone(&discord_transport),
        production_ai_service.clone(),
        application_config.values.ai_features_enabled,
    );
    let first_game_pool_worker = first_game_pool_worker_spec(
        &config.db_path,
        application_config.values.first_game_pool_daily_amount,
        discord_transport.clone(),
        lobby_provider.first_game_pool_display(),
    );
    let economy_events_worker = economy_events_worker_spec(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
        discord_transport.clone(),
        discord_transport.clone(),
    );
    let prediction_refresh_worker = prediction_refresh_worker_spec(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
    );
    let prediction_digest_worker = prediction_digest_worker_spec(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
        discord_transport.clone(),
    );
    let dig_weather_worker = dig_weather_worker_spec(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
    );
    let curfew_sweep_worker = curfew_sweep_worker_spec(
        lobby_provider.curfew_service(),
        lobby_provider.live_lobby_service(),
        discord_transport.clone(),
        lobby_provider.curfew_lobby_display(),
        discord_transport.clone(),
    );
    let survey_recovery_worker = survey_provider.recovery_worker_spec();
    let mafia_phase_worker = mafia_provider.worker_spec(discord_transport.clone());
    let betting_view_timeout_worker = betting_provider.timeout_worker();
    let pet_sweep_worker = pet_sweep_worker_spec_with_ai(
        &config.db_path,
        &application_config,
        discord_transport.clone(),
        reminder_provider.hooks(),
        production_ai_service.clone(),
    );
    info!(
        runtime = "rust",
        git_sha = %env::var("GIT_SHA").unwrap_or_else(|_| "unknown".to_owned()),
        db_path = %config.db_path.display(),
        registered_commands = registry.commands().len(),
        required_cutover_items = inventory::required_count(),
        "starting Rust Discord runtime"
    );
    let mut runtime = Runtime::new(
        config,
        registry,
        SerenityGateway::with_discord_transport(discord_transport),
        database_initialization,
    )
    .with_gateway_event_observers(gateway_observers)
    .with_global_interaction_hooks(global_interaction_hooks)
    .with_raw_reaction_observers(raw_reaction_observers)
    .with_worker(manashop_debt_worker)
    .with_worker(duel_challenges_worker)
    .with_worker(prediction_refresh_worker)
    .with_worker(prediction_digest_worker)
    .with_worker(dig_weather_worker)
    .with_worker(mafia_phase_worker)
    .with_worker(betting_view_timeout_worker)
    .with_worker(survey_recovery_worker)
    .with_worker(curfew_sweep_worker);
    if let Some(first_game_pool_worker) = first_game_pool_worker {
        runtime = runtime.with_worker(first_game_pool_worker);
    }
    if let Some(economy_events_worker) = economy_events_worker {
        runtime = runtime.with_worker(economy_events_worker);
    }
    if let Some(pet_sweep_worker) = pet_sweep_worker {
        runtime = runtime.with_worker(pet_sweep_worker);
    }
    let health_events = runtime.events().subscribe();
    let (health_failure_sender, health_failure_receiver) = tokio::sync::oneshot::channel();
    let health_task = tokio::spawn(async move {
        let result = health_reporter.run(health_events).await;
        if let Err(error) = &result {
            let _ = health_failure_sender.send(error.to_string());
        }
        result
    });
    let mut lifecycle = runtime.events().subscribe();
    tokio::spawn(async move {
        while let Ok(event) = lifecycle.recv().await {
            info!(?event, "runtime lifecycle");
        }
    });

    let runtime_result = runtime
        .run_until(async move {
            tokio::select! {
                () = shutdown_signal() => {}
                health_failure = health_failure_receiver => match health_failure {
                    Ok(error) => error!(%error, "runtime health reporter failed"),
                    Err(_) => error!("runtime health reporter stopped unexpectedly"),
                }
            }
        })
        .await;
    match health_task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            error!(%error, "runtime health reporter stopped with an error");
            return ExitCode::from(1);
        }
        Err(error) => {
            error!(%error, "runtime health reporter task panicked");
            return ExitCode::from(1);
        }
    }

    match runtime_result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "Rust Discord runtime stopped with an error");
            ExitCode::from(1)
        }
    }
}

fn run_health_check(path: PathBuf, maximum_age: Duration) -> ExitCode {
    match check_health(&path, maximum_age) {
        Ok(report) => {
            println!(
                "healthy=true runtime={} revision={} status={:?} heartbeat_age_ms={} migrations={}/{} pid={}",
                report.snapshot.runtime,
                report.snapshot.git_sha,
                report.snapshot.status,
                report.heartbeat_age.as_millis(),
                report.snapshot.applied_migrations,
                report.snapshot.required_migrations,
                report.snapshot.pid,
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("unhealthy: {error}");
            ExitCode::from(1)
        }
    }
}

fn acquire_runtime_lock(database_path: &std::path::Path) -> Result<ProcessLock, String> {
    let lock_path = ProcessLock::path_for_database(database_path);
    ProcessLock::try_acquire(&lock_path).map_err(|error| error.to_string())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    error!(%error, "Ctrl-C signal handler failed");
                }
            }
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        error!(%error, "Ctrl-C signal handler failed");
    }
}

fn run_db_check(path: PathBuf) -> ExitCode {
    let audit = match audit_database(&path) {
        Ok(audit) => audit,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    println!(
        "schema_compatible={} migrations={}/{} historical_extras={} journal_mode={} quick_check={} foreign_keys={} user_version={}",
        audit.is_compatible(),
        audit.applied_migration_count,
        audit.required_migration_count,
        audit.extra_historical_migrations.len(),
        audit.journal_mode,
        audit.quick_check,
        if audit.foreign_keys_enabled {
            "on"
        } else {
            "off"
        },
        audit.user_version,
    );

    if audit.is_compatible() {
        ExitCode::SUCCESS
    } else {
        for issue in audit.issues() {
            eprintln!("incompatible: {issue}");
        }
        ExitCode::from(2)
    }
}

fn sqlite_namespace(path: &Path) -> [PathBuf; 4] {
    let path_text = path.as_os_str().to_string_lossy();
    [
        path.to_path_buf(),
        PathBuf::from(format!("{path_text}-wal")),
        PathBuf::from(format!("{path_text}-shm")),
        PathBuf::from(format!("{path_text}-journal")),
    ]
}

fn paths_share_existing_identity(left: &Path, right: &Path) -> Result<bool, String> {
    if left == right {
        return Ok(true);
    }
    let left_metadata = match fs::metadata(left) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("could not inspect {left:?}: {error}")),
    };
    let right_metadata = match fs::metadata(right) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("could not inspect {right:?}: {error}")),
    };
    if fs::canonicalize(left).map_err(|error| format!("could not resolve {left:?}: {error}"))?
        == fs::canonicalize(right)
            .map_err(|error| format!("could not resolve {right:?}: {error}"))?
    {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if left_metadata.dev() == right_metadata.dev()
            && left_metadata.ino() == right_metadata.ino()
        {
            return Ok(true);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (left_metadata, right_metadata);
    }
    Ok(false)
}

fn verify_disposable_database(source: &Path, candidate: &Path) -> Result<(), String> {
    if fs::symlink_metadata(source)
        .map_err(|error| format!("could not inspect source database: {error}"))?
        .file_type()
        .is_symlink()
        || fs::symlink_metadata(candidate)
            .map_err(|error| format!("could not inspect disposable database: {error}"))?
            .file_type()
            .is_symlink()
    {
        return Err("db-admit refuses symbolic-link database paths".to_owned());
    }
    let source = fs::canonicalize(source)
        .map_err(|error| format!("could not resolve source database: {error}"))?;
    let candidate = fs::canonicalize(candidate)
        .map_err(|error| format!("could not resolve disposable database: {error}"))?;
    let source_namespace = sqlite_namespace(&source);
    let candidate_namespace = sqlite_namespace(&candidate);
    for candidate_path in &candidate_namespace {
        match fs::symlink_metadata(candidate_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(
                    "db-admit refuses symbolic links in the disposable SQLite namespace".to_owned(),
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect disposable SQLite namespace path {candidate_path:?}: {error}"
                ));
            }
        }
        for source_path in &source_namespace {
            if paths_share_existing_identity(candidate_path, source_path)? {
                return Err("db-admit refuses overlap with the source SQLite namespace".to_owned());
            }
        }
    }
    Ok(())
}

fn run_db_admit(path: PathBuf, source: PathBuf) -> ExitCode {
    if let Err(error) = verify_disposable_database(&source, &path) {
        eprintln!("database admission refused: {error}");
        return ExitCode::from(1);
    }
    let migration = match initialize_or_migrate_with_settings(&path, &MigrationSettings::from_env())
    {
        Ok(migration) => migration,
        Err(error) => {
            eprintln!("database admission failed: {error}");
            return ExitCode::from(1);
        }
    };
    let audit = match audit_database(&path) {
        Ok(audit) => audit,
        Err(error) => {
            eprintln!("database admission audit failed: {error}");
            return ExitCode::from(1);
        }
    };
    println!(
        "schema_compatible={} migrations={}/{} newly_applied={} created_tables={} rebuilt_tables={} historical_extras={} journal_mode={} quick_check={} foreign_keys={}",
        audit.is_compatible(),
        audit.applied_migration_count,
        audit.required_migration_count,
        migration.newly_applied.len(),
        migration.created_tables.len(),
        migration.rebuilt_tables.len(),
        migration.historical_extra_migrations.len(),
        audit.journal_mode,
        audit.quick_check,
        if audit.foreign_keys_enabled {
            "on"
        } else {
            "off"
        },
    );
    if audit.is_compatible() {
        ExitCode::SUCCESS
    } else {
        for issue in audit.issues() {
            eprintln!("incompatible: {issue}");
        }
        ExitCode::from(2)
    }
}

fn print_inventory() {
    println!(
        "wired={} required={}",
        inventory::wired_count(),
        inventory::required_count()
    );
    for (category, items) in [
        ("extension", inventory::PYTHON_EXTENSIONS),
        ("gateway-event", inventory::PYTHON_GATEWAY_EVENTS),
        ("ready-recovery", inventory::PYTHON_READY_RECOVERY_ACTIONS),
        ("background-task", inventory::PYTHON_BACKGROUND_TASKS),
        ("external-provider", inventory::EXTERNAL_PROVIDERS),
        ("configuration", inventory::CONFIGURATION_DOMAINS),
    ] {
        for item in items {
            println!(
                "{category}\t{:?}\t{}\t{}",
                item.status, item.python_name, item.rust_boundary
            );
        }
    }
}

fn run_catalog_check(path: PathBuf) -> ExitCode {
    match DotaInfoRegistrationProvider::from_path(&path) {
        Ok(_) => {
            println!("compatible Dotabase catalog: {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("incompatible Dotabase catalog {}: {error}", path.display());
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn default_and_explicit_serve_select_the_gateway_runtime() {
        assert_eq!(parse_command(std::iter::empty()), Ok(Command::Serve));
        assert_eq!(
            parse_command(["serve".to_owned()].into_iter()),
            Ok(Command::Serve)
        );
    }

    #[test]
    fn db_check_accepts_an_explicit_path() {
        assert_eq!(
            parse_command(
                ["db-check", "--db-path", "/tmp/test.db"]
                    .map(str::to_owned)
                    .into_iter()
            ),
            Ok(Command::DatabaseCheck {
                path: PathBuf::from("/tmp/test.db")
            })
        );
    }

    #[test]
    fn db_admit_requires_an_explicit_distinct_copy_contract() {
        assert_eq!(
            parse_command(
                [
                    "db-admit",
                    "--db-path",
                    "/tmp/disposable.db",
                    "--source-db",
                    "/tmp/live.db",
                    "--disposable-copy",
                ]
                .map(str::to_owned)
                .into_iter()
            ),
            Ok(Command::DatabaseAdmit {
                path: PathBuf::from("/tmp/disposable.db"),
                source: PathBuf::from("/tmp/live.db"),
            })
        );
        assert!(
            parse_command(
                [
                    "db-admit",
                    "--db-path",
                    "/tmp/disposable.db",
                    "--source-db",
                    "/tmp/live.db",
                ]
                .map(str::to_owned)
                .into_iter()
            )
            .is_err()
        );
    }

    #[test]
    fn db_admit_refuses_the_source_and_accepts_a_distinct_file() {
        let directory = tempdir().expect("temporary admission directory");
        let source = directory.path().join("live.db");
        let candidate = directory.path().join("candidate.db");
        fs::write(&source, b"source").expect("write source identity fixture");
        fs::write(&candidate, b"candidate").expect("write candidate identity fixture");

        assert!(verify_disposable_database(&source, &source).is_err());
        verify_disposable_database(&source, &candidate).expect("distinct file is disposable");
    }

    #[cfg(unix)]
    #[test]
    fn db_admit_refuses_a_hard_link_to_the_source() {
        let directory = tempdir().expect("temporary admission directory");
        let source = directory.path().join("live.db");
        let candidate = directory.path().join("candidate.db");
        fs::write(&source, b"source").expect("write source identity fixture");
        fs::hard_link(&source, &candidate).expect("create hard-link fixture");

        assert!(
            verify_disposable_database(&source, &candidate)
                .expect_err("hard-linked live DB must be refused")
                .contains("source SQLite namespace")
        );
    }

    #[cfg(unix)]
    #[test]
    fn db_admit_refuses_a_candidate_sidecar_linked_to_the_source_namespace() {
        let directory = tempdir().expect("temporary admission directory");
        let source = directory.path().join("live.db");
        let candidate = directory.path().join("candidate.db");
        let candidate_wal = PathBuf::from(format!("{}-wal", candidate.display()));
        fs::write(&source, b"source").expect("write source identity fixture");
        fs::write(&candidate, b"candidate").expect("write candidate identity fixture");
        fs::hard_link(&source, &candidate_wal).expect("create sidecar hard-link fixture");

        assert!(
            verify_disposable_database(&source, &candidate)
                .expect_err("candidate sidecar linked to live DB must be refused")
                .contains("source SQLite namespace")
        );
    }

    #[test]
    fn health_check_accepts_path_and_maximum_age() {
        assert_eq!(
            parse_command(
                [
                    "health-check",
                    "--db-path",
                    "/tmp/test.db",
                    "--max-age-seconds",
                    "45",
                ]
                .map(str::to_owned)
                .into_iter()
            ),
            Ok(Command::HealthCheck {
                path: PathBuf::from("/tmp/test.db"),
                maximum_age: Duration::from_secs(45),
            })
        );
        assert!(
            parse_command(
                ["health-check", "--max-age-seconds", "0"]
                    .map(str::to_owned)
                    .into_iter()
            )
            .is_err()
        );
    }

    #[test]
    fn health_smoke_accepts_an_explicit_database_path() {
        assert_eq!(
            parse_command(
                ["health-smoke", "--db-path", "/tmp/health-smoke.db"]
                    .map(str::to_owned)
                    .into_iter()
            ),
            Ok(Command::HealthSmoke {
                path: PathBuf::from("/tmp/health-smoke.db")
            })
        );
        assert!(
            parse_command(
                ["health-smoke", "--unexpected"]
                    .map(str::to_owned)
                    .into_iter()
            )
            .is_err()
        );
    }

    #[test]
    fn catalog_check_accepts_an_explicit_path() {
        assert_eq!(
            parse_command(
                ["catalog-check", "--path", "/tmp/dotabase.db"]
                    .map(str::to_owned)
                    .into_iter()
            ),
            Ok(Command::CatalogCheck {
                path: PathBuf::from("/tmp/dotabase.db")
            })
        );
    }

    #[test]
    fn runtime_rejects_unknown_commands_and_extra_arguments() {
        assert!(parse_command(["wat".to_owned()].into_iter()).is_err());
        assert!(parse_command(["serve".to_owned(), "unexpected".to_owned()].into_iter()).is_err());
    }

    #[test]
    fn serve_process_lock_rejects_overlap_and_releases_on_drop() {
        let directory = tempdir().expect("temporary database directory");
        let database_path = directory.path().join("cama.db");
        let first = acquire_runtime_lock(&database_path).expect("first runtime owns lock");
        assert!(acquire_runtime_lock(&database_path).is_err());
        drop(first);
        acquire_runtime_lock(&database_path).expect("replacement owns released lock");
    }
}
