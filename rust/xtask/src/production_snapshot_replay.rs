//! One-way Rust twins for `tests/test_production_snapshot_replay.py`.
//!
//! The retained Python tests remain part of the Python behavior inventory, but
//! Rust cutover is intentionally one-way: these twins prove that production
//! Rust repositories normalize and mutate a disposable Python-era SQLite
//! shape. They do not require Python to read post-cutover Rust writes.

use rusqlite::Connection;
use serde_json::{Value, json};

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
    let root = support::repository_root()?;
    let replay_source = support::source_text(&root, "scripts/production_snapshot_replay.py")?;
    assert!(replay_source.contains("referral_snapshot_smoke"));
    assert!(!replay_source.contains("python_post_rust_retained_repository_read"));
    Ok(())
}

#[test]
fn test_referral_subprocess_failure_reports_attempt_and_partial_deltas() -> support::TestResult {
    let root = support::repository_root()?;
    let output = support::run_python_code(
        &root,
        r#"import json
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from scripts import production_snapshot_replay as replay

with TemporaryDirectory() as directory:
    source = Path(directory) / "production.db"
    source.touch()
    source = source.resolve()
    phase = {"value": "before"}
    source_facts = {
        "sha256": "source",
        "size_bytes": 1,
        "migration_count": 1,
        "quick_check": "ok",
        "integrity_check": ["ok"],
        "integrity_ok": True,
    }

    def fake_inspect(path):
        if path == source:
            return dict(source_facts)
        return {
            **source_facts,
            "sha256": f"copy-{phase['value']}",
            "phase": phase["value"],
        }

    repository_counts = dict(replay.EXPECTED_SMOKE_DELTAS)
    partial_referral_counts = dict(repository_counts)
    partial_referral_counts["matches"] = 1
    count_results = iter([
        {},
        repository_counts,
        repository_counts,
        partial_referral_counts,
    ])

    def fake_run_step(name, command, *, cwd, timeout_seconds, report):
        del command, cwd, timeout_seconds
        if name == "rust_db_check":
            return {"stdout": "schema_compatible=true"}
        if name == "rust_repository_smoke":
            return {"stdout": "shared_repository_roundtrip=ok"}
        if name == "rust_referral_settlement_smoke":
            phase["value"] = "referral_failed"
            step = {
                "name": name,
                "status": "failed",
                "returncode": 1,
                "stdout": "",
                "stderr": "injected failure after a partial write",
            }
            report["steps"].append(step)
            raise replay.StepFailure(step)
        return {"stdout": ""}

    replay.inspect_database = fake_inspect
    replay.table_row_counts = lambda path: next(count_results)
    replay.sentinel_digests = lambda path: {
        table: {"row_count": 1, "sha256": table}
        for table in replay.EXPECTED_SMOKE_DELTAS
    }
    replay.run_step = fake_run_step
    replay._python_executable = lambda argument, root: "python"
    status, report = replay.run(SimpleNamespace(
        source=source,
        report=None,
        python=None,
        cargo="cargo",
        timeout_seconds=1,
    ))
    print(json.dumps({"status": status, "report": report}, sort_keys=True))"#,
    )?;
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["status"], 1);
    assert_eq!(result["report"]["status"], "failed");
    assert_eq!(
        result["report"]["referral_settlement"],
        json!({
            "status": "failed",
            "attempted": true,
            "settlement_marker": false,
            "expected_table_deltas": {
                "economy_ledger_entries": 8,
                "match_participants": 2,
                "match_predictions": 1,
                "matches": 1,
                "player_pairings": 1,
                "players": 6,
                "referrals": 1
            },
            "table_deltas": {
                "matches": {"before": 0, "after": 1, "delta": 1}
            }
        })
    );
    assert_eq!(
        result["report"]["disposable_copy"]["after_rust"]["phase"],
        "referral_failed"
    );
    assert_eq!(result["report"]["source"]["unchanged"], true);
    Ok(())
}
