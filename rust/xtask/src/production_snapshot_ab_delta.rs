//! Rust twins for `tests/test_production_snapshot_ab_delta.py`.

use serde_json::Value;

use super::snapshot_contract_support as support;

#[test]
fn test_route_state_projection_is_json_order_independent() -> support::TestResult {
    let (_directory, database) = support::fresh_database()?;
    support::apply_rust_snapshot_transitions(&database)?;

    let snapshot = support::scope_snapshot(&database)?;
    let projected = snapshot
        .get("tunnels")
        .and_then(|table| table.get("rows"))
        .ok_or("Rust tunnel projection is missing rows")?;
    let expected = support::expected_scope_after()["tunnels"]["rows"].clone();
    assert_eq!(*projected, expected);

    // Confirm that the same reversed-key storage value is normalized by the
    // retained Python projection, rather than merely by the Rust assertion.
    let root = support::repository_root()?;
    let projection_probe = format!(
        r#"import json
from scripts.production_snapshot_ab_delta import _project_rows
rows = [{{"discord_id": {player}, "guild_id": {guild}, "depth": 100, "route_state": '{{"status":"active","route_id":"shored_passage"}}'}}]
print(json.dumps(_project_rows("tunnels", rows), sort_keys=True, separators=(",", ":")))"#,
        player = support::DIG_PLAYER_ID,
        guild = support::SMOKE_GUILD_ID,
    );
    let output = support::run_python_code(&root, &projection_probe)?;
    let python_projection: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(python_projection, expected);
    Ok(())
}

#[test]
fn test_scope_delta_reports_schema_aware_rows_and_counts() -> support::TestResult {
    let (_directory, database) = support::fresh_database()?;
    let before = support::scope_snapshot(&database)?;

    // A different guild is deliberately present.  The bounded scope must not
    // count it when calculating the reserved sentinel delta.
    let unrelated = cama_db::guild_config_repository::GuildConfigRepository::new(&database, false);
    cama_domain::guild_config::GuildConfigStore::set_league_id(&unrelated, 42, 999)?;

    support::apply_rust_snapshot_transitions(&database)?;
    let after = support::scope_snapshot(&database)?;
    assert_eq!(after, support::expected_scope_after());

    let delta = support::scope_delta(&before, &after)?;
    for (table, _) in support::SCOPE_COLUMNS {
        assert_eq!(
            delta[table]["before_count"], 0,
            "unexpected preexisting {table}"
        );
    }
    assert_eq!(delta["guild_config"]["delta"], 1);
    assert_eq!(delta["survey_answers"]["delta"], 0);
    assert_eq!(
        delta["survey_recipients"]["after_rows"][0]["current_question_id"],
        "<question>"
    );
    Ok(())
}

#[test]
fn test_expected_contract_is_stable_json() -> support::TestResult {
    let expected = support::expected_scope_after();
    let encoded = support::canonical_json_text(&expected);
    assert!(!encoded.contains("created_at"));
    assert!(!encoded.contains("updated_at"));
    assert!(encoded.contains("route_id"));
    assert!(encoded.contains("delivery_status"));

    let root = support::repository_root()?;
    let output = support::run_python_code(
        &root,
        r#"import json
from scripts.production_snapshot_ab_delta import expected_scope_after
print(json.dumps(expected_scope_after(), sort_keys=True, separators=(",", ":")))"#,
    )?;
    let python_expected: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(support::canonical_json_text(&python_expected), encoded);
    Ok(())
}

#[test]
fn test_python_write_helper_uses_retained_repositories_for_transitions() -> support::TestResult {
    let root = support::repository_root()?;
    let source = support::source_text(&root, "scripts/python_snapshot_ab_write.py")?;
    for required in [
        "GuildConfigRepository",
        "DigRepository",
        "SurveyRepository",
        "set_league_id",
        "atomic_auto_buy_items",
        "mark_delivery_sent",
    ] {
        assert!(
            source.contains(required),
            "writer lost retained API {required}"
        );
    }

    let (_directory, database) = support::fresh_database()?;
    let script = root.join("scripts/python_snapshot_ab_write.py");
    let output = support::run_python_script(&root, &script, &database)?;
    let stdout = support::output_text(&output, "Python snapshot writer")?;
    assert!(stdout.contains("python_snapshot_ab_write=ok"));
    assert_eq!(
        support::scope_snapshot(&database)?,
        support::expected_scope_after()
    );
    Ok(())
}
