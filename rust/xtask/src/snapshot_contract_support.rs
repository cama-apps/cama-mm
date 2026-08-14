//! Shared disposable SQLite and cross-language helpers for the production
//! snapshot contract tests.
//!
//! These helpers intentionally live with `xtask`: they are evidence probes,
//! not runtime repositories.  The tests still write through the real Rust
//! adapters, read through the retained Python repositories, and admit only a
//! migrated existing-schema database.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use cama_db::dig_inventory_repository::{BuyItemOutcome, BuyItemRequest, DigInventoryRepository};
use cama_db::dig_routes_repository::{DigRouteRepository, RouteStateWriteOutcome};
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_db::schema_manager::initialize_or_migrate;
use cama_db::survey::{SurveyQuestionType, SurveyRepository, SurveyTargetType};
use cama_domain::guild_config::GuildConfigStore;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OpenFlags, params, params_from_iter};
use serde_json::{Map, Value as JsonValue, json};
use tempfile::TempDir;

use super::parity_python;

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub const SMOKE_GUILD_ID: i64 = -8_888_888_888_888_888_888;
pub const DIG_PLAYER_ID: i64 = -8_888_888_888_888_888_870;
pub const SURVEY_GUILD_ID: i64 = 9_000_000_000_000_001;
pub const SURVEY_USER_ID: i64 = 9_000_000_000_000_002;
pub const SURVEY_TITLE: &str = "Rust disposable Survey recovery smoke";
pub const SURVEY_PROMPT: &str = "Can the Rust recovery worker deliver this clone-only sentinel?";
pub const SURVEY_CHANNEL_ID: i64 = 8_000_000_000_000_001;
pub const SURVEY_MESSAGE_ID: i64 = 8_000_000_000_000_002;
pub const ROUTE_STATE_REVERSED: &str = r#"{"status":"active","route_id":"shored_passage"}"#;
pub const ROUTE_STATE_CANONICAL: &str = r#"{"route_id":"shored_passage","status":"active"}"#;

/// The exact columns and reserved-row filters used by the Python A/B runner.
/// Keeping these as constants makes the Rust evidence query schema-aware
/// instead of comparing whole SQLite files or unscoped table counts.
pub const SCOPE_COLUMNS: &[(&str, &[&str])] = &[
    (
        "guild_config",
        &[
            "guild_id",
            "league_id",
            "auto_enrich_matches",
            "ai_features_enabled",
        ],
    ),
    (
        "dig_inventory",
        &["discord_id", "guild_id", "item_type", "queued"],
    ),
    (
        "tunnels",
        &["discord_id", "guild_id", "depth", "route_state"],
    ),
    (
        "surveys",
        &[
            "guild_id",
            "title",
            "status",
            "target_type",
            "registered_only",
        ],
    ),
    (
        "survey_questions",
        &["position", "prompt", "question_type", "required"],
    ),
    (
        "survey_recipients",
        &[
            "guild_id",
            "discord_id",
            "delivery_status",
            "attempt_count",
            "dm_channel_id",
            "dm_message_id",
            "ui_channel_id",
            "ui_message_id",
            "current_question_id",
            "in_review",
            "submitted_at",
            "receipt_checked_attempt",
        ],
    ),
    (
        "survey_answers",
        &[
            "guild_id",
            "recipient_id",
            "numeric_value",
            "text_value",
            "skipped",
        ],
    ),
];

pub fn repository_root() -> TestResult<PathBuf> {
    super::repository_root().map_err(Into::into)
}

pub fn fresh_database() -> TestResult<(TempDir, PathBuf)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("snapshot-contract.db");
    initialize_or_migrate(&path)?;
    Ok((directory, path))
}

fn seed_dig_fixture(path: &Path) -> TestResult {
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "foreign_keys", false)?;
    connection.execute(
        "INSERT INTO players (
             discord_id, guild_id, discord_username, jopacoin_balance,
             glicko_rating, glicko_rd, glicko_volatility
         ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL)",
        params![
            DIG_PLAYER_ID,
            SMOKE_GUILD_ID,
            "Rust snapshot contract miner",
            100_i64
        ],
    )?;
    connection.execute(
        "INSERT INTO tunnels (discord_id, guild_id, depth) VALUES (?1, ?2, 100)",
        params![DIG_PLAYER_ID, SMOKE_GUILD_ID],
    )?;
    Ok(())
}

/// Apply the bounded Rust-side transitions through production repository APIs.
pub fn apply_rust_snapshot_transitions(path: &Path) -> TestResult {
    seed_dig_fixture(path)?;

    let guild = GuildConfigRepository::new(path, false);
    guild.set_league_id(SMOKE_GUILD_ID, 777)?;
    guild.set_auto_enrich(SMOKE_GUILD_ID, false)?;
    guild.set_ai_enabled(SMOKE_GUILD_ID, true)?;

    let inventory = DigInventoryRepository::new(path);
    let purchase = inventory.buy_item_atomic(BuyItemRequest {
        discord_id: DIG_PLAYER_ID,
        guild_id: Some(SMOKE_GUILD_ID),
        item_type: "hard_hat",
        price: 20,
        inventory_limit: 10,
        auto_queue_if_available: true,
        created_at: 1_800_000_000,
    })?;
    if !matches!(
        purchase,
        BuyItemOutcome::Purchased {
            queued: true,
            cost: 20,
            balance_after: 80,
            ..
        }
    ) {
        return Err(format!("Rust snapshot Dig purchase mismatch: {purchase:?}").into());
    }

    let routes = DigRouteRepository::new(path);
    if routes.set_route_state(
        DIG_PLAYER_ID,
        Some(SMOKE_GUILD_ID),
        Some(ROUTE_STATE_REVERSED),
    )? != RouteStateWriteOutcome::Updated
    {
        return Err("Rust snapshot route transition did not update the tunnel".into());
    }

    let surveys = SurveyRepository::with_clock(path, || 1_800_000_000);
    let survey = surveys.create_survey(SURVEY_GUILD_ID, SURVEY_TITLE)?;
    let question = surveys.add_question(
        SURVEY_GUILD_ID,
        survey.survey_id,
        SURVEY_PROMPT,
        SurveyQuestionType::Nps,
        true,
    )?;
    surveys.open_survey(
        SURVEY_GUILD_ID,
        survey.survey_id,
        &[SURVEY_USER_ID],
        SurveyTargetType::Member,
        false,
    )?;
    let claimed = surveys.claim_deliveries(1, 300)?;
    let recipient = claimed
        .first()
        .ok_or("Rust snapshot Survey delivery was not claimable")?;
    if recipient.survey_id != survey.survey_id
        || recipient.current_question_id != Some(question.question_id)
    {
        return Err(format!("Rust snapshot Survey claim mismatch: {recipient:?}").into());
    }
    let sent = surveys.mark_delivery_sent(
        recipient.recipient_id,
        recipient.attempt_count,
        SURVEY_CHANNEL_ID,
        SURVEY_MESSAGE_ID,
    )?;
    if sent.delivery_status.as_str() != "sent"
        || sent.attempt_count != 1
        || sent.dm_channel_id != Some(SURVEY_CHANNEL_ID)
        || sent.dm_message_id != Some(SURVEY_MESSAGE_ID)
        || sent.ui_channel_id != Some(SURVEY_CHANNEL_ID)
        || sent.ui_message_id != Some(SURVEY_MESSAGE_ID)
        || sent.current_question_id != Some(question.question_id)
    {
        return Err(format!("Rust snapshot Survey delivery mismatch: {sent:?}").into());
    }
    Ok(())
}

pub fn run_python_code(root: &Path, code: &str) -> TestResult<Output> {
    let mut command = parity_python(root);
    let output = command
        .args(["-c", code])
        .output()
        .map_err(|error| format!("cannot run retained Python probe: {error}"))?;
    require_success(output, "retained Python probe")
}

pub fn run_python_script(root: &Path, script: &Path, database: &Path) -> TestResult<Output> {
    let mut command = parity_python(root);
    let output = command
        .arg(script)
        .arg(database)
        .output()
        .map_err(|error| format!("cannot run {}: {error}", script.display()))?;
    require_success(output, &script.display().to_string())
}

fn require_success(output: Output, description: &str) -> TestResult<Output> {
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "{description} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

pub fn output_text(output: &Output, description: &str) -> TestResult<String> {
    String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{description} emitted non-UTF-8 output: {error}").into())
}

fn scope_filter(table: &str) -> TestResult<(&'static str, Vec<Value>)> {
    match table {
        "guild_config" => Ok(("guild_id = ?1", vec![Value::Integer(SMOKE_GUILD_ID)])),
        "dig_inventory" | "tunnels" => Ok((
            "discord_id = ?1 AND guild_id = ?2",
            vec![
                Value::Integer(DIG_PLAYER_ID),
                Value::Integer(SMOKE_GUILD_ID),
            ],
        )),
        "surveys" => Ok((
            "guild_id = ?1 AND title = ?2",
            vec![
                Value::Integer(SURVEY_GUILD_ID),
                Value::Text(SURVEY_TITLE.to_owned()),
            ],
        )),
        "survey_questions" => Ok((
            "guild_id = ?1 AND survey_id IN (
                 SELECT survey_id FROM surveys WHERE guild_id = ?2 AND title = ?3
             )",
            vec![
                Value::Integer(SURVEY_GUILD_ID),
                Value::Integer(SURVEY_GUILD_ID),
                Value::Text(SURVEY_TITLE.to_owned()),
            ],
        )),
        "survey_recipients" => Ok((
            "guild_id = ?1 AND discord_id = ?2 AND survey_id IN (
                 SELECT survey_id FROM surveys WHERE guild_id = ?3 AND title = ?4
             )",
            vec![
                Value::Integer(SURVEY_GUILD_ID),
                Value::Integer(SURVEY_USER_ID),
                Value::Integer(SURVEY_GUILD_ID),
                Value::Text(SURVEY_TITLE.to_owned()),
            ],
        )),
        "survey_answers" => Ok((
            "guild_id = ?1 AND survey_id IN (
                 SELECT survey_id FROM surveys WHERE guild_id = ?2 AND title = ?3
             )",
            vec![
                Value::Integer(SURVEY_GUILD_ID),
                Value::Integer(SURVEY_GUILD_ID),
                Value::Text(SURVEY_TITLE.to_owned()),
            ],
        )),
        other => Err(format!("unknown snapshot scope table {other:?}").into()),
    }
}

fn json_from_value(value: ValueRef<'_>) -> JsonValue {
    match value {
        ValueRef::Null => JsonValue::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => json!(value),
        ValueRef::Text(value) => JsonValue::String(String::from_utf8_lossy(value).into_owned()),
        ValueRef::Blob(value) => {
            json!({"bytes_hex": value.iter().map(|byte| format!("{byte:02x}")).collect::<String>()})
        }
    }
}

fn canonical_value(table: &str, column: &str, value: JsonValue) -> JsonValue {
    if column == "route_state"
        && let JsonValue::String(raw) = value
    {
        return serde_json::from_str(&raw).unwrap_or(JsonValue::String(raw));
    }
    if table == "survey_questions"
        && column == "position"
        && let Some(number) = value.as_i64()
    {
        return json!(number);
    }
    if matches!(column, "current_question_id" | "question_id") && !value.is_null() {
        return JsonValue::String("<question>".to_owned());
    }
    value
}

fn canonical_json(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_owned(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(value) => value.to_string(),
        JsonValue::String(value) => serde_json::to_string(value).expect("JSON string encoding"),
        JsonValue::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        JsonValue::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| {
                        format!(
                            "{}:{}",
                            serde_json::to_string(key).expect("JSON key encoding"),
                            canonical_json(&values[key])
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

/// Read the same reserved projections as the Python A/B runner.
pub fn scope_snapshot(path: &Path) -> TestResult<JsonValue> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.pragma_update(None, "query_only", true)?;
    let mut snapshot = Map::new();
    for (table, columns) in SCOPE_COLUMNS {
        let (filter, parameters) = scope_filter(table)?;
        let select = columns.join(", ");
        let sql = format!("SELECT {select} FROM \"{table}\" WHERE {filter}");
        let mut statement = connection.prepare(&sql)?;
        let mut query = statement.query(params_from_iter(parameters.iter()))?;
        let mut rows = Vec::new();
        while let Some(row) = query.next()? {
            let mut projected = Map::new();
            for (index, column) in columns.iter().enumerate() {
                let value = canonical_value(table, column, json_from_value(row.get_ref(index)?));
                projected.insert((*column).to_owned(), value);
            }
            rows.push(JsonValue::Object(projected));
        }
        rows.sort_by_key(canonical_json);
        snapshot.insert(
            (*table).to_owned(),
            json!({"row_count": rows.len(), "rows": rows}),
        );
    }
    Ok(JsonValue::Object(snapshot))
}

fn table_rows<'a>(snapshot: &'a JsonValue, table: &str) -> TestResult<&'a Vec<JsonValue>> {
    snapshot
        .get(table)
        .and_then(|value| value.get("rows"))
        .and_then(JsonValue::as_array)
        .ok_or_else(|| format!("snapshot is missing {table}.rows").into())
}

pub fn scope_delta(before: &JsonValue, after: &JsonValue) -> TestResult<JsonValue> {
    let mut delta = Map::new();
    for (table, _) in SCOPE_COLUMNS {
        let before_rows = table_rows(before, table)?;
        let after_rows = table_rows(after, table)?;
        delta.insert(
            (*table).to_owned(),
            json!({
                "before_count": before_rows.len(),
                "after_count": after_rows.len(),
                "delta": after_rows.len() as i64 - before_rows.len() as i64,
                "before_rows": before_rows,
                "after_rows": after_rows,
            }),
        );
    }
    Ok(JsonValue::Object(delta))
}

pub fn expected_scope_after() -> JsonValue {
    json!({
        "guild_config": {
            "row_count": 1,
            "rows": [{
                "guild_id": SMOKE_GUILD_ID,
                "league_id": 777,
                "auto_enrich_matches": 0,
                "ai_features_enabled": 1
            }]
        },
        "dig_inventory": {
            "row_count": 1,
            "rows": [{
                "discord_id": DIG_PLAYER_ID,
                "guild_id": SMOKE_GUILD_ID,
                "item_type": "hard_hat",
                "queued": 1
            }]
        },
        "tunnels": {
            "row_count": 1,
            "rows": [{
                "discord_id": DIG_PLAYER_ID,
                "guild_id": SMOKE_GUILD_ID,
                "depth": 100,
                "route_state": {"route_id": "shored_passage", "status": "active"}
            }]
        },
        "surveys": {
            "row_count": 1,
            "rows": [{
                "guild_id": SURVEY_GUILD_ID,
                "title": SURVEY_TITLE,
                "status": "open",
                "target_type": "member",
                "registered_only": 0
            }]
        },
        "survey_questions": {
            "row_count": 1,
            "rows": [{
                "position": 1,
                "prompt": SURVEY_PROMPT,
                "question_type": "nps",
                "required": 1
            }]
        },
        "survey_recipients": {
            "row_count": 1,
            "rows": [{
                "guild_id": SURVEY_GUILD_ID,
                "discord_id": SURVEY_USER_ID,
                "delivery_status": "sent",
                "attempt_count": 1,
                "dm_channel_id": SURVEY_CHANNEL_ID,
                "dm_message_id": SURVEY_MESSAGE_ID,
                "ui_channel_id": SURVEY_CHANNEL_ID,
                "ui_message_id": SURVEY_MESSAGE_ID,
                "current_question_id": "<question>",
                "in_review": 0,
                "submitted_at": null,
                "receipt_checked_attempt": 1
            }]
        },
        "survey_answers": {"row_count": 0, "rows": []}
    })
}

pub fn expected_retained_python_read() -> JsonValue {
    json!({
        "guild_config": {
            "guild_id": SMOKE_GUILD_ID,
            "league_id": 777,
            "auto_enrich_matches": 0,
            "ai_features_enabled": 1
        },
        "dig": {
            "inventory": {
                "row_count": 1,
                "discord_id": DIG_PLAYER_ID,
                "guild_id": SMOKE_GUILD_ID,
                "item_type": "hard_hat",
                "queued": true
            },
            "tunnel": {
                "discord_id": DIG_PLAYER_ID,
                "guild_id": SMOKE_GUILD_ID,
                "depth": 100,
                "route_state": ROUTE_STATE_CANONICAL
            }
        },
        "survey": {
            "survey": {
                "guild_id": SURVEY_GUILD_ID,
                "title": SURVEY_TITLE,
                "status": "open",
                "target_type": "member",
                "registered_only": false
            },
            "question": {
                "position": 1,
                "prompt": SURVEY_PROMPT,
                "question_type": "nps",
                "required": true
            },
            "recipient": {
                "guild_id": SURVEY_GUILD_ID,
                "discord_id": SURVEY_USER_ID,
                "delivery_status": "sent",
                "attempt_count": 1,
                "dm_channel_id": SURVEY_CHANNEL_ID,
                "dm_message_id": SURVEY_MESSAGE_ID,
                "ui_channel_id": SURVEY_CHANNEL_ID,
                "ui_message_id": SURVEY_MESSAGE_ID,
                "current_question_is_first": true,
                "in_review": false,
                "submitted": false,
                "answer_count": 0
            }
        }
    })
}

/// The retained replay parser is intentionally exact: unknown keys, missing
/// keys, stale route JSON, or a raw repository-shaped response are rejected.
pub fn parse_retained_python_read(output: &str) -> TestResult<JsonValue> {
    let value: JsonValue = serde_json::from_str(output.trim())
        .map_err(|error| format!("retained Python repository read was not JSON: {error}"))?;
    if !value.is_object() {
        return Err(format!("retained Python repository read was not an object: {value}").into());
    }
    let expected = expected_retained_python_read();
    if value != expected {
        return Err(format!(
            "retained Python repository read mismatch: expected {expected}, found {value}"
        )
        .into());
    }
    Ok(value)
}

pub fn canonical_json_text(value: &JsonValue) -> String {
    canonical_json(value)
}

pub fn source_text(root: &Path, relative: &str) -> TestResult<String> {
    let path = root.join(relative);
    Ok(fs::read_to_string(&path)?)
}
