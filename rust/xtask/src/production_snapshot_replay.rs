//! Rust twins for `tests/test_production_snapshot_replay.py`.

use serde_json::{Value, json};

use super::snapshot_contract_support as support;

#[test]
fn test_retained_read_normalizes_dig_and_survey_api_values() -> support::TestResult {
    let (_directory, database) = support::fresh_database()?;
    support::apply_rust_snapshot_transitions(&database)?;

    let root = support::repository_root()?;
    let script = root.join("scripts/retained_python_snapshot_read.py");
    let output = support::run_python_script(&root, &script, &database)?;
    let encoded = support::output_text(&output, "retained Python repository read")?;
    let actual = support::parse_retained_python_read(&encoded)?;

    assert_eq!(actual, support::expected_retained_python_read());
    assert_eq!(
        actual["dig"]["tunnel"]["route_state"],
        support::ROUTE_STATE_CANONICAL
    );
    assert_eq!(actual["survey"]["recipient"]["answer_count"], 0);
    Ok(())
}

#[test]
fn test_retained_read_reports_missing_repository_field() -> support::TestResult {
    let root = support::repository_root()?;
    let output = support::run_python_code(
        &root,
        r#"from scripts import retained_python_snapshot_read as reader

class Guild:
    def __init__(self, _db_path):
        pass
    def get_config(self, _guild_id):
        return {"guild_id": reader.DIG_GUILD_ID, "league_id": 777, "auto_enrich_matches": 0, "ai_features_enabled": 1}

class Dig:
    def __init__(self, _db_path):
        pass
    def get_inventory(self, _discord_id, _guild_id):
        return [{"discord_id": reader.DIG_PLAYER_ID, "guild_id": reader.DIG_GUILD_ID, "item_type": reader.DIG_ITEM_TYPE, "queued": 1}]
    def get_tunnel(self, _discord_id, _guild_id):
        return {"discord_id": reader.DIG_PLAYER_ID, "guild_id": reader.DIG_GUILD_ID, "depth": 100}

reader.GuildConfigRepository = Guild
reader.DigRepository = Dig
try:
    reader.read_snapshot("unused-disposable-copy.db")
except RuntimeError as error:
    expected = "Python repository API blocker: DigRepository.get_tunnel sentinel row does not expose 'route_state'"
    assert str(error) == expected, (str(error), expected)
else:
    raise AssertionError("missing route_state was accepted")"#,
    )?;
    assert!(output.stdout.is_empty());
    Ok(())
}

#[test]
fn test_retained_reader_has_no_raw_sql_fallback() -> support::TestResult {
    let root = support::repository_root()?;
    let source = support::source_text(&root, "scripts/retained_python_snapshot_read.py")?;
    assert!(!source.contains("sqlite3"));
    assert!(!source.contains("SELECT "));
    assert!(source.contains("DigRepository"));
    assert!(source.contains("SurveyRepository"));
    Ok(())
}

#[test]
fn test_snapshot_parser_accepts_only_the_normalized_contract() -> support::TestResult {
    let (_directory, database) = support::fresh_database()?;
    support::apply_rust_snapshot_transitions(&database)?;

    let root = support::repository_root()?;
    let script = root.join("scripts/retained_python_snapshot_read.py");
    let output = support::run_python_script(&root, &script, &database)?;
    let encoded = support::output_text(&output, "retained Python repository read")?;
    assert_eq!(
        support::parse_retained_python_read(&encoded)?,
        support::expected_retained_python_read()
    );

    let mut malformed: Value = serde_json::from_str(&encoded)?;
    malformed["dig"]["tunnel"]["route_state"] = json!("{}");
    let error = support::parse_retained_python_read(&serde_json::to_string(&malformed)?)
        .expect_err("stale route JSON must not satisfy the normalized contract");
    assert!(
        error
            .to_string()
            .contains("retained Python repository read mismatch")
    );
    Ok(())
}
