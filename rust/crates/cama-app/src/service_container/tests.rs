use super::*;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

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

#[test]
fn test_initialize_is_idempotent_and_flags_initialization() {
    let database = TestDatabase::new();
    let mut container = ServiceContainer::new(&database.path, ServiceContainerOptions::default());
    assert!(!container.initialized());
    assert!(matches!(
        container.components(),
        Err(ContainerError::NotInitialized)
    ));
    container.initialize();
    assert!(container.initialized());
    let first = Arc::clone(
        &container
            .components()
            .expect("components")
            .vanity_tax_service,
    );
    container.initialize();
    let second = &container
        .components()
        .expect("components")
        .vanity_tax_service;
    assert!(Arc::ptr_eq(&first, second));
}

#[test]
fn test_in_memory_container_uses_a_unique_shared_cache_database() {
    let mut first = ServiceContainer::new(":memory:", ServiceContainerOptions::default());
    let mut second = ServiceContainer::new(":memory:", ServiceContainerOptions::default());
    first.initialize();
    second.initialize();
    assert!(first.db_path().starts_with("file:cama-rust-container-"));
    assert!(first.db_path().ends_with("?mode=memory&cache=shared"));
    assert_ne!(first.db_path(), second.db_path());
}

#[test]
fn test_options_debug_redacts_llm_api_key() {
    let options = ServiceContainerOptions {
        llm_api_key: Some("secret-key".to_owned()),
        ..ServiceContainerOptions::default()
    };
    let debug = format!("{options:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("secret-key"));
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
    assert_eq!(second_service.calculate_tax(7, 123, 100), 20);
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
