use super::*;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use cama_db::economy_event_repository::{EventDraft, EventEffects};

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(1);

struct TestDatabase {
    path: PathBuf,
}

impl TestDatabase {
    fn new() -> Self {
        let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cama-service-container-{}-{sequence}.db",
            std::process::id()
        ));
        cama_db::schema_manager::initialize_or_migrate(&path)
            .expect("initialize canonical Rust schema");
        Self { path }
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let candidate = PathBuf::from(format!("{}{suffix}", self.path.display()));
            match fs::remove_file(candidate) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove disposable SQLite fixture: {error}"),
            }
        }
    }
}

fn reference_container() -> ServiceContainer {
    ServiceContainer::new(
        "/private/tmp/cama-service-container-reference-only.db",
        ServiceContainerOptions::default(),
    )
}

fn assert_same(left: &Arc<ComponentNode>, right: &Arc<ComponentNode>) {
    assert!(Arc::ptr_eq(left, right));
}

#[test]
fn test_initialize_is_idempotent() {
    let mut container = reference_container();
    container.initialize();
    let first = Arc::clone(
        container
            .components()
            .expect("initialized components")
            .player_service
            .component(),
    );

    container.initialize();
    let second = container
        .components()
        .expect("initialized components")
        .player_service
        .component();
    assert_same(&first, second);
}

#[test]
fn test_in_memory_container_uses_one_normalized_database() {
    let mut container = ServiceContainer::new(":memory:", ServiceContainerOptions::default());
    container.initialize();
    let components = container.components().expect("initialized components");

    assert!(components.database_runtime.is_normalized_shared_memory());
    assert_ne!(container.db_path(), ":memory:");
    assert_eq!(container.db_path(), components.database_runtime.db_path());
    assert_eq!(
        components.player_repository.db_path(),
        components.database_runtime.db_path()
    );
    assert_eq!(
        components.match_repository.db_path(),
        components.database_runtime.db_path()
    );
    assert_eq!(components.player_repository.player_ids(0), Some(Vec::new()));
    assert!(Arc::ptr_eq(
        components
            .database_runtime
            .memory
            .as_ref()
            .expect("runtime memory catalog"),
        components
            .player_repository
            .memory
            .as_ref()
            .expect("repository memory catalog")
    ));
}

#[test]
fn bot_composition_monitoring_uses_the_container_normalized_database() {
    let mut container = ServiceContainer::new(":memory:", ServiceContainerOptions::default());
    container.initialize();
    let components = container.components().expect("initialized components");

    // The production composition constructs every database-backed observer
    // from this normalized path after initialization. Keep the monitoring
    // source tied to the same runtime reference rather than the caller's
    // pre-normalized `:memory:` spelling.
    assert_ne!(container.db_path(), ":memory:");
    assert_eq!(container.db_path(), components.database_runtime.db_path());
    assert_eq!(container.db_path(), components.player_repository.db_path());
    assert_eq!(container.db_path(), components.match_repository.db_path());
}

#[test]
fn test_duel_repository_and_service_are_initialized_once() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    let repository = Arc::clone(&components.duel_repository);
    let service = Arc::clone(&components.duel_service);

    container.initialize();
    let components = container.components().expect("initialized components");
    assert!(Arc::ptr_eq(&repository, &components.duel_repository));
    assert!(Arc::ptr_eq(&service, &components.duel_service));
    assert!(Arc::ptr_eq(
        components.duel_service.repository(),
        &components.duel_repository
    ));
}

#[test]
fn test_vanity_tax_enforcement_survives_container_restart() {
    let database = TestDatabase::new();
    let member = VanityMember {
        discord_id: 7,
        nickname: Some("Skater".to_owned()),
    };
    let mut first = ServiceContainer::new(&database.path, ServiceContainerOptions::default());
    first.initialize();
    let first_service = &first
        .components()
        .expect("initialized components")
        .vanity_tax_service;
    first_service
        .refresh_guild(123, std::slice::from_ref(&member))
        .expect("refresh first container");
    first_service
        .set_manual_taxation(123, 7, true, 42)
        .expect("persist manual taxation");

    let mut second = ServiceContainer::new(&database.path, ServiceContainerOptions::default());
    second.initialize();
    let second_service = &second
        .components()
        .expect("initialized components")
        .vanity_tax_service;
    second_service
        .refresh_guild(123, &[member])
        .expect("refresh restarted container");
    assert_eq!(second_service.calculate_tax(7, 123, 100), 10);
}

#[test]
fn test_configured_vanity_tax_rate_reaches_concrete_service_without_basis_point_rounding() {
    let database = TestDatabase::new();
    let mut container = ServiceContainer::new(
        &database.path,
        ServiceContainerOptions {
            vanity_tax_rate: 0.123_456,
            ..ServiceContainerOptions::default()
        },
    );
    container.initialize();
    let service = &container
        .components()
        .expect("initialized components")
        .vanity_tax_service;
    service.update_member(123, 7, None);
    assert_eq!(service.calculate_tax(7, 123, 10_000), 1_234);
}

#[test]
fn test_initialized_flag() {
    let mut container = reference_container();
    assert!(!container.initialized());
    container.initialize();
    assert!(container.initialized());
}

#[test]
fn test_constructor_has_single_optional_llm_transport() {
    assert_eq!(OPTIONAL_API_KEY_FIELDS, ["llm_api_key"]);
    assert!(
        REMOVED_CONSTRUCTOR_FIELDS
            .iter()
            .all(|name| !OPTIONAL_API_KEY_FIELDS.contains(name))
    );
    let options = ServiceContainerOptions {
        llm_api_key: Some("sentinel-key".to_owned()),
        ..ServiceContainerOptions::default()
    };
    let container = ServiceContainer::new("unused.db", options);
    assert_eq!(
        container.options.llm_api_key.as_deref(),
        Some("sentinel-key")
    );
}

#[test]
fn test_concrete_opendota_transport_is_retained_for_runtime_consumers() {
    let opendota = Arc::new(
        crate::opendota_http::OpenDotaRuntimeServices::from_lookup(|name| match name {
            "OPENDOTA_API_KEY" => Some("container-secret".to_owned()),
            "ENRICHMENT_RETRY_DELAYS" => Some("0,0,0".to_owned()),
            _ => None,
        })
        .expect("offline OpenDota transport construction"),
    );
    let mut container = ServiceContainer::new(
        "unused.db",
        ServiceContainerOptions {
            opendota: Some(Arc::clone(&opendota)),
            ..ServiceContainerOptions::default()
        },
    );

    container.initialize();

    let retained = container
        .components()
        .expect("initialized components")
        .opendota
        .as_ref()
        .expect("concrete OpenDota transport");
    assert!(Arc::ptr_eq(retained, &opendota));
    let debug = format!("{retained:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("container-secret"));
}

#[test]
fn test_economy_controller_has_no_static_recovery_switch() {
    assert!(!REMOVED_CONSTRUCTOR_FIELDS.is_empty());
    assert!(REMOVED_CONSTRUCTOR_FIELDS.contains(&"economy_recovery_mode"));
    let database = TestDatabase::new();
    let mut container = ServiceContainer::new(
        &database.path,
        ServiceContainerOptions {
            economy_events_enabled: true,
            ..ServiceContainerOptions::default()
        },
    );
    container.initialize();
    let components = container.components().expect("initialized components");
    assert!(components.disburse_service.voting_enabled());
    let policy = components
        .economy_event_service
        .ensure_policy(123, 1_900_000_000)
        .expect("persist normal policy");
    assert_eq!(policy.mode, PolicyMode::Normal);
}

#[test]
fn test_severe_persisted_edict_dynamically_disables_reserve_voting() {
    let database = TestDatabase::new();
    let mut container = ServiceContainer::new(
        &database.path,
        ServiceContainerOptions {
            economy_events_enabled: true,
            ..ServiceContainerOptions::default()
        },
    );
    container.initialize();
    let now = 1_900_000_000;
    let event_date = DateTime::<Utc>::from_timestamp(now, 0)
        .expect("valid timestamp")
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    let effects = EventEffects {
        reserve_voting_disabled: true,
        ..EventEffects::default()
    };
    container
        .components()
        .expect("initialized components")
        .economy_event_service
        .repository
        .activate_event_atomic(
            Some(123),
            &EventDraft {
                event_date,
                name: "Ravage".to_owned(),
                hero: "Tidehunter".to_owned(),
                direction: EventDirection::Deflationary,
                severity: 3,
                target_effect_jc: 0,
                forecast_flow_jc: 0,
                expected_effect_jc: 0,
                monetary_stock_before: 0,
                effects,
                announcement: "A tidal shock hits the economy.".to_owned(),
                starts_at: now - 1,
                ends_at: now + 60,
                created_at: now,
            },
        )
        .expect("persist severe edict");

    assert_eq!(
        container
            .components()
            .expect("initialized components")
            .disburse_service
            .can_propose(123, now)
            .expect("evaluate proposal gate"),
        ProposalEligibility::Blocked(ProposalBlock::EconomyEdict)
    );
}

#[test]
fn test_llm_api_key_is_passed_to_ai_service() {
    let mut container = ServiceContainer::new(
        "unused.db",
        ServiceContainerOptions {
            llm_api_key: Some("sentinel-key".to_owned()),
            ..ServiceContainerOptions::default()
        },
    );
    container.initialize();
    let components = container.components().expect("initialized components");
    let ai = components
        .ai_service
        .as_ref()
        .expect("configured AI service");
    assert_eq!(ai.api_key(), "sentinel-key");
    assert_eq!(ai.model(), "groq/qwen/qwen3.6-27b");
    assert_eq!(ai.timeout_seconds(), 15.0);
    assert_eq!(ai.max_tokens(), 500);
    assert!(!format!("{ai:?}").contains("sentinel-key"));
    assert!(Arc::ptr_eq(
        ai.request_repository(),
        &components.llm_request_repository
    ));
}

#[test]
fn configured_production_ai_service_is_retained_by_shared_graph() {
    use std::time::Duration;

    use crate::ai_http::{OpenAiCompatibleEndpoint, OpenAiCompatibleProviderLoader};
    use crate::ai_services::{AIService, ProviderConfig, ProviderCredential, ProviderName};

    let provider_config = ProviderConfig::new(
        "groq/qwen/qwen3.6-27b",
        ProviderCredential::new("sentinel-key").expect("credential"),
        Duration::from_secs(3),
        500,
    )
    .expect("provider config");
    let loader = Arc::new(OpenAiCompatibleProviderLoader::new(
        OpenAiCompatibleEndpoint::for_provider(ProviderName::Groq).expect("endpoint"),
    ));
    let production = Arc::new(AIService::new(provider_config, loader));
    let mut container = ServiceContainer::new(
        "unused.db",
        ServiceContainerOptions {
            llm_api_key: Some("sentinel-key".to_owned()),
            production_ai_service: Some(Arc::clone(&production)),
            ..ServiceContainerOptions::default()
        },
    );
    container.initialize();
    let ai = container
        .components()
        .expect("initialized components")
        .ai_service
        .as_ref()
        .expect("AI service");
    assert!(Arc::ptr_eq(
        ai.production().expect("production AI adapter"),
        &production
    ));
    assert!(!format!("{ai:?}").contains("sentinel-key"));
}

#[test]
fn test_dig_llm_hard_switch_preserves_other_ai_services() {
    let mut container = ServiceContainer::new(
        "unused.db",
        ServiceContainerOptions {
            llm_api_key: Some("sentinel-key".to_owned()),
            dig_llm_enabled: false,
            ..ServiceContainerOptions::default()
        },
    );
    container.initialize();
    let components = container.components().expect("initialized components");
    assert!(components.ai_service.is_some());
    assert!(components.component("sql_query_service").is_some());
    assert!(components.component("dig_flavor_service").is_none());
    assert!(!components.neon_degen_service.dig_llm_enabled());
}

#[test]
fn test_expose_to_bot_sets_supported_attributes_by_identity() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    let exposure = container.expose_to_bot().expect("expose components");
    let expected = BOT_EXPOSURE_NAMES.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(
        exposure
            .attributes()
            .keys()
            .copied()
            .collect::<BTreeSet<_>>(),
        expected
    );
    for name in BOT_EXPOSURE_NAMES {
        match (
            exposure
                .attributes()
                .get(name)
                .expect("published attribute"),
            components
                .component_slot(name)
                .expect("registered component"),
        ) {
            (Some(exposed), Some(component)) => assert_same(exposed, component),
            (None, None) => {}
            pair => panic!("exposure identity mismatch for {name}: {pair:?}"),
        }
    }
    for removed in [
        "ADMIN_USER_IDS",
        "ai_service",
        "buff_repo",
        "db",
        "guild_config_repo",
        "survey_repo",
        "tax_repo",
        "tip_repository",
    ] {
        assert!(!exposure.attributes().contains_key(removed));
    }
    for optional in [
        "dig_flavor_service",
        "sql_query_service",
        "flavor_text_service",
    ] {
        assert!(matches!(exposure.attributes().get(optional), Some(None)));
    }
}

#[test]
fn test_duel_flavor_service_is_always_exposed() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    let exposure = container.expose_to_bot().expect("expose components");
    let exposed = exposure
        .attributes()
        .get("duel_flavor_service")
        .and_then(Option::as_ref)
        .expect("duel flavor exposure");
    assert_same(exposed, components.duel_flavor_service.component());
    assert!(components.duel_flavor_service.ai_service().is_none());
}

#[test]
fn test_duel_flavor_service_has_optional_ai_and_guild_config() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    assert_eq!(
        components.duel_flavor_service.ai_service().is_some(),
        components.ai_service.is_some()
    );
    assert!(Arc::ptr_eq(
        components.duel_flavor_service.guild_config_repository(),
        &components.guild_config_repository
    ));
}

#[test]
fn test_betting_service_has_garnishment() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    assert_same(
        components.betting_service.garnishment_service(),
        components
            .component("garnishment_service")
            .expect("garnishment component"),
    );
}

#[test]
fn test_betting_service_has_bankruptcy() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    assert_same(
        components.betting_service.bankruptcy_service(),
        components
            .component("bankruptcy_service")
            .expect("bankruptcy component"),
    );
}

#[test]
fn test_match_service_has_betting() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    assert_same(
        components.match_service.betting_service(),
        components.betting_service.component(),
    );
}

#[test]
fn test_match_service_has_state_service() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    assert_same(
        components.match_service.state_service(),
        components
            .component("match_state_service")
            .expect("match-state component"),
    );
}

#[test]
fn test_dig_service_receives_pet_service_via_constructor() {
    let mut container = ServiceContainer::new(
        "unused.db",
        ServiceContainerOptions {
            pet_enabled: true,
            ..ServiceContainerOptions::default()
        },
    );
    container.initialize();
    let components = container.components().expect("initialized components");
    assert_same(
        components
            .dig_service
            .pet_service()
            .expect("configured pet service"),
        components.component("pet_service").expect("pet component"),
    );
}

#[test]
fn test_lobby_service_has_state_service() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    assert_same(
        components.lobby_service.match_state_service(),
        components
            .component("match_state_service")
            .expect("match-state component"),
    );
}

#[test]
fn test_survey_service_has_survey_repository() {
    let mut container = reference_container();
    container.initialize();
    let components = container.components().expect("initialized components");
    assert!(Arc::ptr_eq(
        components.survey_service.repository(),
        &components.survey_repository
    ));
}
