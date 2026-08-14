//! Rust twins for `tests/test_production_snapshot_ab_delta.py`.

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::snapshot_contract_support as support;

fn assert_report_path_guard(report_suffix: &str) -> support::TestResult {
    let root = support::repository_root()?;
    let suffix = serde_json::to_string(report_suffix)?;
    let probe = format!(
        r#"import json
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from scripts import production_snapshot_ab_delta as ab

with TemporaryDirectory() as directory:
    source = Path(directory) / "production.db"
    source.touch()
    inspections = []

    def unexpected_inspection(path):
        inspections.append(str(path))
        raise AssertionError("source inspection must not start")

    ab.inspect_database = unexpected_inspection
    try:
        ab.run(SimpleNamespace(
            source=source,
            report=Path(str(source) + {suffix}),
            python=None,
            cargo="cargo",
            timeout_seconds=1,
        ))
    except ValueError as error:
        result = {{"error": str(error), "inspections": inspections}}
    else:
        raise AssertionError("unsafe report path was accepted")
    print(json.dumps(result, sort_keys=True))"#
    );
    let output = support::run_python_code(&root, &probe)?;
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        result["error"],
        "report path must not be the source database or its sidecar"
    );
    assert_eq!(result["inspections"], json!([]));
    Ok(())
}

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

#[test]
fn test_report_path_rejects_source_database_before_inspection() -> support::TestResult {
    assert_report_path_guard("")
}

#[test]
fn test_report_path_rejects_source_wal_before_inspection() -> support::TestResult {
    assert_report_path_guard("-wal")
}

#[test]
fn test_report_path_rejects_source_shm_before_inspection() -> support::TestResult {
    assert_report_path_guard("-shm")
}

#[test]
fn test_referral_snapshot_live_core_is_one_way_and_idempotent() -> support::TestResult {
    let (_directory, database) = support::fresh_database()?;
    support::apply_rust_snapshot_transitions(&database)?;

    let snapshot = support::scope_snapshot(&database)?;
    assert_eq!(snapshot, support::expected_scope_after());
    assert_eq!(snapshot["players"]["row_count"], 3);
    assert_eq!(snapshot["referrals"]["row_count"], 1);
    assert_eq!(snapshot["matches"]["row_count"], 1);
    assert_eq!(snapshot["economy_ledger_entries"]["row_count"], 2);

    let connection = Connection::open(&database)?;
    let (match_count, participant_count, ledger_count, context_count): (i64, i64, i64, i64) =
        connection.query_row(
            "SELECT
                 (SELECT COUNT(*) FROM matches
                  WHERE guild_id = ?1 AND pending_match_id = ?2),
                 (SELECT COUNT(*) FROM match_participants
                  WHERE guild_id = ?1 AND match_id = (
                      SELECT match_id FROM matches
                      WHERE guild_id = ?1 AND pending_match_id = ?2
                  )),
                 (SELECT COUNT(*) FROM economy_ledger_entries
                  WHERE guild_id = ?1 AND source = 'referral_reward'),
                 (SELECT COUNT(*) FROM economy_ledger_context)",
            params![
                support::REFERRAL_GUILD_ID,
                support::REFERRAL_PENDING_MATCH_ID
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    assert_eq!(
        (match_count, participant_count, ledger_count, context_count),
        (1, 2, 2, 0)
    );

    let referral: (i64, i64, i64) = connection.query_row(
        "SELECT rewarded_match_id, reward_amount, rewarded_at
         FROM referrals WHERE guild_id = ?1 AND referred_id = ?2",
        params![support::REFERRAL_GUILD_ID, support::REFERRED_ID],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(referral.1, 100);
    assert_eq!(referral.2, support::REFERRAL_REWARDED_AT);

    let balances: Vec<(i64, i64, i64, i64)> = connection
        .prepare(
            "SELECT discord_id, jopacoin_balance, wins, losses FROM players
             WHERE guild_id = ?1 ORDER BY discord_id",
        )?
        .query_map([support::REFERRAL_GUILD_ID], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        balances,
        vec![
            (support::REFERRER_ID, 110, 0, 0),
            (support::REFERRED_ID, 120, 1, 0),
            (support::REFERRAL_OPPONENT_ID, 30, 0, 1),
        ]
    );
    let other_guild_counts: (i64, i64, i64) = connection.query_row(
        "SELECT
             (SELECT COUNT(*) FROM referrals WHERE guild_id = ?1),
             (SELECT COUNT(*) FROM matches
              WHERE guild_id = ?1 AND pending_match_id = ?2),
             (SELECT COUNT(*) FROM economy_ledger_entries
              WHERE guild_id = ?1 AND source = 'referral_reward')",
        params![
            support::REFERRAL_OTHER_GUILD_ID,
            support::REFERRAL_PENDING_MATCH_ID
        ],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(other_guild_counts, (0, 0, 0));

    let jc_changes: String = connection.query_row(
        "SELECT jc_changes FROM matches
         WHERE guild_id = ?1 AND pending_match_id = ?2",
        params![
            support::REFERRAL_GUILD_ID,
            support::REFERRAL_PENDING_MATCH_ID
        ],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<Value>(&jc_changes)?,
        json!({
            support::REFERRED_ID.to_string(): {"referral": 100},
            support::REFERRER_ID.to_string(): {"referral": 100},
        })
    );

    let root = support::repository_root()?;
    let replay_source = support::source_text(&root, "scripts/production_snapshot_replay.py")?;
    assert!(!replay_source.contains("python_post_rust_retained_repository_read"));
    assert!(!replay_source.contains("post_rust_python"));
    Ok(())
}
