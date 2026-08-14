//! One-way Rust twins for `tests/test_production_snapshot_replay.py`.
//!
//! The retained Python tests remain part of the Python behavior inventory, but
//! Rust cutover is intentionally one-way: these twins prove that production
//! Rust repositories normalize and mutate a disposable Python-era SQLite
//! shape. They do not require Python to read post-cutover Rust writes.

use rusqlite::Connection;
use serde_json::json;

use super::snapshot_contract_support as support;

#[test]
fn test_rust_snapshot_normalizes_dig_and_survey_api_values() -> support::TestResult {
    let (_directory, database) = support::fresh_database()?;
    let before = support::scope_snapshot(&database)?;
    support::apply_rust_snapshot_transitions(&database)?;
    let after = support::scope_snapshot(&database)?;

    assert_eq!(after, support::expected_scope_after());
    let delta = support::scope_delta(&before, &after)?;
    assert_eq!(delta["dig_inventory"]["delta"], 1);
    assert_eq!(
        after["tunnels"]["rows"][0]["route_state"],
        json!({"route_id": "shored_passage", "status": "active"})
    );
    assert_eq!(after["survey_answers"]["row_count"], 0);
    Ok(())
}

#[test]
fn test_rust_snapshot_reports_missing_repository_field() -> support::TestResult {
    let (_directory, database) = support::fresh_database()?;
    support::apply_rust_snapshot_transitions(&database)?;
    Connection::open(&database)?.execute(
        "ALTER TABLE tunnels RENAME COLUMN route_state TO missing_route_state",
        [],
    )?;

    let error = support::scope_snapshot(&database)
        .expect_err("missing production projection column must fail closed");
    assert!(error.to_string().contains("route_state"), "{error}");
    Ok(())
}

#[test]
fn test_python_seed_helper_uses_repositories_for_transitions() -> support::TestResult {
    let root = support::repository_root()?;
    let source = support::source_text(&root, "scripts/python_snapshot_ab_write.py")?;
    let (_, transitions) = source
        .split_once("def write_snapshot")
        .ok_or("Python snapshot writer is missing write_snapshot")?;

    assert!(transitions.contains("GuildConfigRepository"));
    assert!(transitions.contains("DigRepository"));
    assert!(transitions.contains("SurveyRepository"));
    assert!(transitions.contains("_seed_dig_fixture(db_path)"));
    assert!(!transitions.contains("connection.execute("));
    Ok(())
}

#[test]
fn test_snapshot_parser_accepts_only_the_normalized_rust_contract() -> support::TestResult {
    let (_directory, database) = support::fresh_database()?;
    support::apply_rust_snapshot_transitions(&database)?;
    let actual = support::scope_snapshot(&database)?;
    let encoded = serde_json::to_string(&actual)?;
    assert_eq!(
        support::parse_expected_scope_after(&encoded)?,
        support::expected_scope_after()
    );

    let mut malformed = actual;
    malformed["tunnels"]["rows"][0]["route_state"] = json!({});
    let error = support::parse_expected_scope_after(&serde_json::to_string(&malformed)?)
        .expect_err("stale route JSON must not satisfy the normalized Rust contract");
    assert!(error.to_string().contains("Rust snapshot mismatch"));
    Ok(())
}
