//! Rust-side architecture contracts corresponding to Python's static layer checks.
//!
//! These tests deliberately exercise the Rust workspace boundaries rather than
//! importing or re-running the Python checks.  Source-shape assertions here are
//! limited to contracts that cannot be observed through a public API (crate
//! dependency direction, module exports, and the blocking boundary); database
//! tests below also execute the real migrated-schema adapters.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cama_db::autobet_investments::AutobetInvestmentRepository;
use cama_db::core_repositories::PlayerRepository;
use cama_db::dig_relic_recycling::DigRelicRecyclingRepository;
use cama_db::loan_repository::LoanRepository;
use cama_db::mafia_repository::MafiaRepository;
use cama_db::match_recording_repository::MatchRecordingRepository;
use cama_db::schema_manager::initialize_or_migrate;
use cama_db::soft_avoid_repository::SoftAvoidRepository;
use tempfile::NamedTempFile;

use super::repository_root;

fn root() -> PathBuf {
    repository_root().expect("workspace root")
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn production_source(relative: &str) -> String {
    let source = read(relative);
    source
        .split_once("#[cfg(test)]")
        .map_or(source.clone(), |(production, _)| production.to_owned())
}

fn cargo_workspace_dependencies(relative: &str) -> BTreeSet<String> {
    let contents = read(relative);
    let mut in_dependencies = false;
    let mut dependencies = BTreeSet::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dependencies = trimmed == "[dependencies]";
            continue;
        }
        if !in_dependencies || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, _)) = trimmed.split_once('=') else {
            continue;
        };
        if name.trim().starts_with("cama-") {
            dependencies.insert(name.trim().to_owned());
        }
    }
    dependencies
}

fn rust_source_files(relative: &str) -> Vec<PathBuf> {
    fn visit(directory: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        {
            let entry = entry.expect("directory entry");
            let path = entry.path();
            if path.is_dir() {
                visit(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    visit(&root().join(relative), &mut files);
    files.sort();
    files
}

fn root_module_paths(relative: &str) -> BTreeMap<String, PathBuf> {
    let root_path = root().join(relative);
    let source = fs::read_to_string(&root_path).expect("crate root");
    let source_directory = root_path.parent().expect("crate source directory");
    let mut path_override = None;
    let mut modules = BTreeMap::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(path) = trimmed
            .strip_prefix("#[path = \"")
            .and_then(|value| value.strip_suffix("\"]"))
        {
            path_override = Some(path.to_owned());
            continue;
        }
        if let Some(name) = trimmed
            .strip_prefix("pub mod ")
            .and_then(|value| value.strip_suffix(';'))
        {
            let path = path_override.take().map_or_else(
                || source_directory.join(format!("{name}.rs")),
                |path| source_directory.join(path),
            );
            modules.insert(name.to_owned(), path);
        } else if !trimmed.starts_with("//") && !trimmed.is_empty() {
            path_override = None;
        }
    }
    modules
}

#[test]
fn domain_root_is_a_declaration_only_initializer() {
    let source = read("rust/crates/cama-domain/src/lib.rs");
    let declarations: Vec<_> = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//!"))
        .collect();
    assert!(!declarations.is_empty());
    assert!(declarations.iter().all(|line| line.starts_with("pub mod ")));
    assert!(!source.contains("cama_db"));
    assert!(!source.contains("cama_runtime"));
}

#[test]
fn domain_layer_has_no_infrastructure_dependencies() {
    let dependencies = cargo_workspace_dependencies("rust/crates/cama-domain/Cargo.toml");
    assert!(
        dependencies.is_empty(),
        "domain dependencies: {dependencies:?}"
    );
    for file in rust_source_files("rust/crates/cama-domain/src") {
        let source = fs::read_to_string(&file).expect("domain source");
        for forbidden in [
            "cama_db",
            "cama_app",
            "cama_runtime",
            "rusqlite",
            "serenity",
        ] {
            assert!(
                !source.contains(forbidden),
                "domain file {} reaches outward layer {forbidden}",
                file.display()
            );
        }
    }
}

#[test]
fn crate_dependency_dag_has_only_inward_edges() {
    let expected = [
        ("rust/crates/cama-domain/Cargo.toml", BTreeSet::new()),
        (
            "rust/crates/cama-db/Cargo.toml",
            BTreeSet::from(["cama-domain".to_owned()]),
        ),
        (
            "rust/crates/cama-app/Cargo.toml",
            BTreeSet::from(["cama-db".to_owned(), "cama-domain".to_owned()]),
        ),
        (
            "rust/crates/cama-runtime/Cargo.toml",
            BTreeSet::from([
                "cama-app".to_owned(),
                "cama-db".to_owned(),
                "cama-domain".to_owned(),
            ]),
        ),
    ];
    for (manifest, allowed) in expected {
        assert_eq!(
            cargo_workspace_dependencies(manifest),
            allowed,
            "layer edge drift in {manifest}"
        );
    }
}

#[test]
fn crate_roots_resolve_every_public_module_export() {
    for (crate_name, root_file) in [
        ("cama-domain", "rust/crates/cama-domain/src/lib.rs"),
        ("cama-db", "rust/crates/cama-db/src/lib.rs"),
        ("cama-app", "rust/crates/cama-app/src/lib.rs"),
        ("cama-runtime", "rust/crates/cama-runtime/src/lib.rs"),
    ] {
        let modules = root_module_paths(root_file);
        assert!(!modules.is_empty(), "{crate_name} exports no modules");
        for (module, path) in modules {
            assert!(
                path.is_file(),
                "{crate_name} exports `{module}` from missing {}",
                path.display()
            );
        }
    }
}

#[test]
fn application_crate_has_no_runtime_or_discord_transport_dependency() {
    let dependencies = read("rust/crates/cama-app/Cargo.toml");
    assert!(!dependencies.contains("cama-runtime"));
    assert!(!dependencies.contains("serenity"));
    assert!(!dependencies.contains("discord_transport"));
    for file in rust_source_files("rust/crates/cama-app/src") {
        let source = fs::read_to_string(&file).expect("application source");
        assert!(
            !source.contains("cama_runtime"),
            "{} imports runtime",
            file.display()
        );
        assert!(
            !source.contains("discord_transport"),
            "{} imports transport",
            file.display()
        );
    }
}

#[test]
fn runtime_sqlite_providers_cross_a_blocking_boundary() {
    let provider_files = [
        "advstats_provider.rs",
        "admin_provider.rs",
        "ask_provider.rs",
        "betting_provider.rs",
        "blame_luke_provider.rs",
        "dig_provider.rs",
        "draft_provider.rs",
        "duel_provider.rs",
        "enrichment_provider.rs",
        "herogrid_provider.rs",
        "info_provider.rs",
        "lobby_provider.rs",
        "mafia_provider.rs",
        "mana_provider.rs",
        "match_provider.rs",
        "pet_provider.rs",
        "player_trivia_provider.rs",
        "prediction_provider.rs",
        "profile_provider.rs",
        "rating_analysis_provider.rs",
        "registration_provider.rs",
        "scout_provider.rs",
        "shop_provider.rs",
        "survey_provider.rs",
        "tax_provider.rs",
        "trivia_provider.rs",
        "wrapped_provider.rs",
    ];
    for file in provider_files {
        let relative = format!("rust/crates/cama-runtime/src/{file}");
        let source = read(&relative);
        assert!(
            source.contains("async fn"),
            "{file} has no async entrypoint"
        );
        assert!(
            source.contains("spawn_blocking"),
            "{file} has async SQLite-facing code without spawn_blocking"
        );
    }
}

#[test]
fn synchronous_sqlite_adapters_are_marked_for_blocking_callers() {
    for file in [
        "ai_query_sqlite.rs",
        "economy_event_sqlite.rs",
        "pet_sqlite.rs",
        "reminder_sqlite.rs",
    ] {
        let relative = format!("rust/crates/cama-app/src/{file}");
        let source = read(&relative);
        assert!(
            !source.contains("async fn"),
            "{file} exposes async SQLite work"
        );
        assert!(
            source.contains("spawn_blocking") || source.contains("blocking pool"),
            "{file} does not document its runtime blocking boundary"
        );
    }
}

#[test]
fn db_repositories_use_the_existing_schema_connection() {
    let repository_files = [
        "core_repositories.rs",
        "loan.rs",
        "mafia_repository.rs",
        "match_recording.rs",
        "prediction_resolution.rs",
        "rating_history.rs",
        "soft_avoid.rs",
    ];
    for file in repository_files {
        let relative = format!("rust/crates/cama-db/src/{file}");
        let source = production_source(&relative);
        assert!(
            source.contains("open_runtime_connection"),
            "{file} bypasses the shared existing-schema connection policy"
        );
        assert!(
            !source.contains("CREATE TABLE"),
            "{file} creates runtime schema"
        );
    }
}

#[test]
fn guild_normalization_is_one_zero_sentinel_convention() {
    let repository_files = [
        "core_repositories.rs",
        "dig_relic_recycling.rs",
        "loan.rs",
        "mafia_repository.rs",
        "match_recording.rs",
        "soft_avoid.rs",
    ];
    for file in repository_files {
        let source = production_source(&format!("rust/crates/cama-db/src/{file}"));
        assert!(
            source.contains("Some(guild_id) => guild_id"),
            "{file} changes Some guild IDs"
        );
        assert!(
            source.contains("None => 0"),
            "{file} changes the None guild sentinel"
        );
    }
}

#[test]
fn db_writes_use_typed_transactions_not_manual_sql() {
    let mut transactional_files = 0;
    for file in rust_source_files("rust/crates/cama-db/src") {
        if file.file_name().is_some_and(|name| name == "tests.rs") {
            continue;
        }
        let relative = file
            .strip_prefix(root().join("rust/crates/cama-db/src"))
            .expect("repository relative path");
        let relative = format!("rust/crates/cama-db/src/{}", relative.display());
        let source = production_source(&relative);
        if source.contains("TransactionBehavior::Immediate") {
            transactional_files += 1;
            assert!(
                source.contains("transaction.commit()"),
                "{relative} never commits typed transaction"
            );
        }
        assert!(
            !source.contains("BEGIN IMMEDIATE;"),
            "{relative} embeds manual BEGIN SQL"
        );
    }
    assert!(
        transactional_files >= 20,
        "too few typed repository transactions: {transactional_files}"
    );
}

#[test]
fn application_services_do_not_reach_runtime_modules() {
    for file in rust_source_files("rust/crates/cama-app/src") {
        let source = fs::read_to_string(&file).expect("application source");
        assert!(
            !source.contains("cama_runtime"),
            "{} reaches runtime",
            file.display()
        );
        assert!(
            !source.contains("crate::discord_transport"),
            "{} reaches Discord transport",
            file.display()
        );
        assert!(
            !source.contains("crate::serenity_transport"),
            "{} reaches Serenity transport",
            file.display()
        );
    }
}

#[test]
fn optional_guild_normalization_has_canonical_runtime_behavior() {
    assert_eq!(PlayerRepository::normalize_guild_id(None), 0);
    assert_eq!(LoanRepository::normalize_guild_id(None), 0);
    assert_eq!(SoftAvoidRepository::normalize_guild_id(None), 0);
    assert_eq!(DigRelicRecyclingRepository::normalize_guild_id(None), 0);
    assert_eq!(MafiaRepository::normalize_guild_id(None), 0);
    assert_eq!(MatchRecordingRepository::normalize_guild_id(None), 0);
    for guild_id in [-42, 0, 42, i64::MAX] {
        assert_eq!(
            PlayerRepository::normalize_guild_id(Some(guild_id)),
            guild_id
        );
        assert_eq!(LoanRepository::normalize_guild_id(Some(guild_id)), guild_id);
        assert_eq!(
            MafiaRepository::normalize_guild_id(Some(guild_id)),
            guild_id
        );
    }
}

#[test]
fn migrated_schema_and_atomic_write_policy_are_explicit() {
    let file = NamedTempFile::new().expect("temporary migrated database");
    initialize_or_migrate(file.path()).expect("initialize existing-schema fixture");
    let audit = cama_db::audit_database(file.path()).expect("audit migrated database");
    assert!(
        audit.is_compatible(),
        "migrated database is not compatible: {:?}",
        audit.issues()
    );
    assert_eq!(audit.journal_mode.to_ascii_lowercase(), "wal");

    let investments = AutobetInvestmentRepository::new(file.path());
    for target_id in 27..32 {
        assert_eq!(
            investments
                .set(None, 17, target_id, "long", 10)
                .expect("atomic position write"),
            (target_id - 26) * 10
        );
    }
    assert!(investments.set(None, 17, 32, "short", 1).is_err());
    let rows = investments.list(None, 17).expect("read committed position");
    assert_eq!(
        rows.len(),
        5,
        "failed atomic write leaked a partial position"
    );
    assert_eq!(rows[0].guild_id, 0);
}
