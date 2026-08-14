//! Cama Rust process entrypoint.

mod herogrid_provider;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use cama_app::ai_http::production_ai_service_from_settings;
use cama_app::draft::DraftStateManager;
use cama_app::service_container::ServiceContainer;
use cama_db::audit_database;
use cama_runtime::inventory;
use cama_runtime::match_provider::production_betting_flavor;
use cama_runtime::process_lock::ProcessLock;
use cama_runtime::{
    AdminMatchCorrectionRuntime, AdminRegistrationProvider, AdminRuntimePorts,
    AdvancedStatsRegistrationProvider, ApplicationConfig, AskRegistrationProvider,
    BlameLukeRegistrationProvider, CompletedDatabaseAdmission, DatabaseAdmission, DigBonusRuntime,
    DigRegistrationProvider, DotaInfoRegistrationProvider, DraftRegistrationProvider,
    DuelRegistrationProvider, EnrichmentRegistrationProvider, GatewayEventObservers,
    GlobalInteractionHooks, HealthReporter, InfoRegistrationProvider, LobbyRegistrationProvider,
    LobbyRuntimeConfig, MafiaRegistrationProvider, ManaRegistrationProvider,
    MatchRegistrationProvider, PetRegistrationProvider, PlayerRegistrationProvider,
    PlayerTriviaRegistrationProvider, PredictionRegistrationProvider, PredictionRuntimePorts,
    ProfileRegistrationProvider, RatingAnalysisRegistrationProvider, RawReactionObservers,
    RegistryBuilder, ReminderRegistrationProvider, Runtime, ScoutRegistrationProvider,
    SerenityDiscordTransport, SerenityGateway, ShopRegistrationProvider, SqliteDatabaseAdmission,
    SurveyRegistrationProvider, TaxRegistrationProvider, TriviaRegistrationProvider, UsageMonitor,
    VanityTaxGatewayObserver, WrappedRegistrationProvider, check_health, dig_weather_worker_spec,
    duel_challenges_worker_spec, economy_events_worker_spec, first_game_pool_worker_spec,
    manashop_debt_worker_spec, pet_sweep_worker_spec_with_ai, prediction_digest_worker_spec,
    prediction_refresh_worker_spec, validate_production_registry,
};
use cama_runtime::{
    BettingRegistrationProvider, BettingRuntimeConfig, match_post_match_debrief_port,
    match_wager_refresh_port,
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::herogrid_provider::HeroGridRegistrationProvider;

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Serve,
    DatabaseCheck {
        path: PathBuf,
    },
    HealthCheck {
        path: PathBuf,
        maximum_age: Duration,
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
        Ok(Command::HealthCheck { path, maximum_age }) => run_health_check(path, maximum_age),
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
        Some("db-check") => parse_db_check(args),
        Some("health-check") => parse_health_check(args),
        Some(command) => Err(format!(
            "unknown command {command:?}; expected `serve`, `db-check`, `health-check`, `catalog-check`, or `inventory`"
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
    let database_admission = match SqliteDatabaseAdmission::with_migration_settings(
        application_config.migration_settings(),
    )
    .admit(&config.db_path)
    .await
    {
        Ok(report) => report,
        Err(error) => {
            error!(%error, "Rust database migration/admission refused startup");
            return ExitCode::from(1);
        }
    };
    info!(
        db_path = %database_admission.path.display(),
        applied_migrations = database_admission.applied_migrations,
        required_migrations = database_admission.required_migrations,
        newly_applied_migrations = database_admission.newly_applied_migrations,
        created_tables = database_admission.created_tables,
        rebuilt_tables = database_admission.rebuilt_tables,
        historical_extra_migrations = database_admission.historical_extra_migrations,
        "Rust database migration and compatibility admission complete"
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
    let discord_transport = Arc::new(SerenityDiscordTransport::new());
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
    ) {
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
        survey_provider.gateway_observer(),
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
    if let Err(error) = validate_production_registry(&registry) {
        error!(%error, "production command-tree contract refused startup");
        return ExitCode::from(1);
    }
    let manashop_debt_worker = manashop_debt_worker_spec(&config.db_path);
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
        db_path = %config.db_path.display(),
        registered_commands = registry.commands().len(),
        required_cutover_items = inventory::required_count(),
        "starting Rust Discord runtime"
    );
    let mut runtime = Runtime::new(
        config,
        registry,
        SerenityGateway::with_discord_transport(discord_transport),
        CompletedDatabaseAdmission::new(database_admission),
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
    .with_worker(survey_recovery_worker);
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
                "healthy=true status={:?} heartbeat_age_ms={} migrations={}/{} pid={}",
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
