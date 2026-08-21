//! Offline replay of the read-only Dotabase contract.
//!
//! The checked-in SQL is intentionally a small, sanitized catalog. It keeps
//! production table/column/relationship shapes while avoiding a copy of the
//! large upstream asset. The live_network_used flag is false, so this
//! evidence does not close the external-service readiness gate.

use std::collections::BTreeSet;
use std::path::PathBuf;

use cama_app::dota_info::DotaInfoSource;
use cama_app::dotabase_sqlite::DotabaseSqliteSource;
use cama_app::hero_lookup::{HeroImageSize, HeroLookup, HeroRoleClass};
use cama_app::trivia_data::{TriviaDataRegistry, TriviaDataSource};
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

mod support;
use support::sha256_hex;

const FIXTURE_MANIFEST: &str = include_str!("fixtures/dotabase/manifest.json");
const CATALOG_SQL: &str = include_str!("fixtures/dotabase/sanitized_catalog.sql");

fn materialize_catalog() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("create Dotabase replay directory");
    let path = directory.path().join("dotabase.db");
    let connection = Connection::open(&path).expect("create Dotabase replay database");
    connection
        .execute_batch(CATALOG_SQL)
        .expect("materialize sanitized Dotabase catalog");
    drop(connection);
    (directory, path)
}

#[test]
fn dotabase_fixture_manifest_is_hashed_and_redacted() {
    let manifest: Value = serde_json::from_str(FIXTURE_MANIFEST).expect("fixture manifest JSON");
    assert_eq!(manifest["schema"], "cama.external-service-recording/v1");
    assert_eq!(manifest["provider"], "dotabase_sqlite");
    assert_eq!(manifest["schema_version"], "dotabase-compatible-7.8.11");
    assert_eq!(manifest["provenance"]["live_network_used"], false);
    assert!(FIXTURE_MANIFEST.contains("sanitized_contract_fixture"));
    assert!(!FIXTURE_MANIFEST.contains("Authorization"));
    assert!(!FIXTURE_MANIFEST.contains("Bearer"));

    let body = manifest["contracts"][0]["body"]
        .as_str()
        .expect("catalog body name");
    assert_eq!(body, "sanitized_catalog.sql");
    let expected = manifest["contracts"][0]["sha256"]
        .as_str()
        .expect("catalog body hash");
    assert_eq!(sha256_hex(CATALOG_SQL.as_bytes()), expected);

    let contracts = manifest["contracts"]
        .as_array()
        .expect("Dotabase fixture contracts");
    let ids = contracts
        .iter()
        .map(|contract| contract["id"].as_str().expect("contract id"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ids,
        BTreeSet::from(["catalog-success", "missing-asset", "schema-mismatch"])
    );
}

#[test]
fn dotabase_success_recording_replays_through_public_adapters() {
    let (_directory, path) = materialize_catalog();
    let original = std::fs::read(&path).expect("read replay database before adapter calls");
    let source = DotabaseSqliteSource::new(&path);

    let registry = TriviaDataRegistry::new(source.clone());
    let heroes = registry.load_heroes().expect("recorded heroes");
    assert_eq!(heroes.len(), 2);
    assert_eq!(heroes[0].localized_name, "Anti-Mage");
    assert_eq!(heroes[0].damage_min_at_level_one, Some(53));

    let abilities = registry.load_abilities().expect("recorded abilities");
    assert_eq!(abilities.len(), 2);
    let blink = abilities
        .iter()
        .find(|ability| ability.localized_name == "Blink")
        .expect("Blink ability");
    assert_eq!(blink.hero_name.as_deref(), Some("Anti-Mage"));
    assert_eq!(
        blink.icon_url.as_deref(),
        Some(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/abilities/antimage_blink.png",
        )
    );
    assert_eq!(registry.load_items().expect("recorded items").len(), 2);
    assert_eq!(
        registry
            .load_voicelines()
            .expect("recorded voicelines")
            .len(),
        2
    );
    assert_eq!(registry.load_facets().expect("recorded facets").len(), 3);

    let source = registry.source();
    assert_eq!(source.all_heroes().expect("hero choices").len(), 2);
    assert_eq!(source.all_abilities().expect("ability choices").len(), 2);
    let anti_mage = source
        .hero_by_name("am")
        .expect("hero alias lookup")
        .expect("Anti-Mage alias");
    assert_eq!(anti_mage.localized_name, "Anti-Mage");
    assert_eq!(anti_mage.abilities, ["Blink"]);
    assert_eq!(anti_mage.talents, ["Hidden Talent"]);
    let pudge = source
        .hero_by_name("Pudge")
        .expect("hero lookup")
        .expect("Pudge");
    assert_eq!(pudge.talents.len(), 8);
    assert_eq!(pudge.facets.len(), 2);
    let meat_hook = source
        .ability_by_name("meat hook")
        .expect("ability lookup")
        .expect("Meat Hook");
    assert_eq!(meat_hook.localized_name, "Meat Hook");
    assert_eq!(meat_hook.damage_type.as_deref(), Some("Pure"));

    let lookup = HeroLookup::new(&path);
    assert_eq!(lookup.color(1), Some(0x78_40_94));
    assert_eq!(lookup.roles(1), ["Carry", "Escape", "Nuker"]);
    assert_eq!(lookup.classify_role(1), HeroRoleClass::Core);
    assert_eq!(
        lookup.image_url(1, HeroImageSize::Full).as_deref(),
        Some(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/antimage.png",
        )
    );
    assert_eq!(
        std::fs::read(&path).expect("read replay database after adapter calls"),
        original,
        "read-only Dotabase adapters must not mutate the fixture",
    );
}

#[test]
fn dotabase_missing_asset_recording_fails_without_creating_database() {
    let directory = tempfile::tempdir().expect("create missing-asset directory");
    let path = directory.path().join("missing-dotabase.db");
    let source = DotabaseSqliteSource::new(&path);

    let error = source.heroes().expect_err("missing asset must fail closed");
    assert!(error.to_string().contains("unable to open database file"));
    assert!(!path.exists(), "read-only source must not create an asset");
}

#[test]
fn dotabase_schema_mismatch_recording_fails_at_first_required_query() {
    let directory = tempfile::tempdir().expect("create schema-mismatch directory");
    let path = directory.path().join("incomplete-dotabase.db");
    let connection = Connection::open(&path).expect("create incomplete catalog");
    connection
        .execute_batch(
            "PRAGMA user_version = 70000;
             CREATE TABLE heroes (id INTEGER PRIMARY KEY, localized_name TEXT);",
        )
        .expect("materialize incomplete schema");
    drop(connection);

    let source = DotabaseSqliteSource::new(&path);
    let error = source
        .heroes()
        .expect_err("incomplete schema must fail closed");
    assert!(error.to_string().contains("no such column"));
    assert!(path.exists(), "schema mismatch must not delete the asset");
}
