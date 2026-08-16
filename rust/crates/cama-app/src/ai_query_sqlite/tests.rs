use std::sync::Arc;

use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, params};

use super::*;
use crate::ai_services::{GeneratedSql, SQLQueryService, SqlGenerationPort, SqlGenerationRequest};

struct FixedSql {
    sql: &'static str,
}

impl SqlGenerationPort for FixedSql {
    fn generate_sql(&self, _request: &SqlGenerationRequest) -> Option<GeneratedSql> {
        Some(GeneratedSql {
            sql: self.sql.to_owned(),
            explanation: "fixture".to_owned(),
        })
    }
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("cama.db");
    initialize_or_migrate_with_settings(&path, &MigrationSettings::default())
        .expect("migrate temporary database");
    let connection = Connection::open(&path).expect("open fixture");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match production foreign-key mode");
    connection
        .execute(
            "INSERT INTO players
                 (discord_id, guild_id, discord_username, wins, jopacoin_balance)
             VALUES (?1, 11, 'Guild Eleven', 9, 100),
                    (?2, 22, 'Guild Twenty Two', 99, 200)",
            params![101_i64, 202_i64],
        )
        .expect("insert cross-guild players");
    (directory, path)
}

#[test]
fn migrated_schema_metadata_and_parameterized_read_match_python_contract() {
    let (_directory, path) = fixture();
    let repository = SqliteAIQueryRepository::new(&path);
    let snapshot = repository
        .get_schema_metadata()
        .expect("read schema metadata");
    let players = snapshot
        .tables
        .iter()
        .find(|table| table.name == "players")
        .expect("players metadata");
    assert!(
        players
            .columns
            .iter()
            .any(|column| column.name == "guild_id")
    );

    let rows = repository
        .execute_readonly(
            "SELECT discord_username FROM players WHERE discord_id = ?",
            &[Value::Integer(101)],
            25,
        )
        .expect("parameterized read");
    assert_eq!(
        rows[0].get("discord_username"),
        Some(&Value::Text("Guild Eleven".to_owned()))
    );
}

#[test]
fn guild_scoped_temp_views_hide_other_guild_without_trusting_generated_sql() {
    let (_directory, path) = fixture();
    let repository = SqliteAIQueryRepository::new(&path);
    let rows = repository
        .execute_readonly_guild_scoped(
            "SELECT discord_username, wins FROM players ORDER BY wins DESC",
            Some(11),
            &[],
            25,
        )
        .expect("guild-scoped query");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("discord_username"),
        Some(&Value::Text("Guild Eleven".to_owned()))
    );
}

#[test]
fn sql_query_service_executes_validated_projection_through_real_adapter() {
    let (_directory, path) = fixture();
    let repository: Arc<dyn AIQueryRepository> = Arc::new(SqliteAIQueryRepository::new(&path));
    let service = SQLQueryService::new(repository);
    let result = service.query(
        &FixedSql {
            sql: "SELECT discord_username, wins FROM players ORDER BY wins DESC",
        },
        None,
        Some(11),
        "who wins the most?",
        Some(101),
    );
    assert!(result.success, "{:?}", result.error);
    assert_eq!(result.row_count, 1);
    assert_eq!(
        result.results[0].get("discord_username"),
        Some(&Value::Text("Guild Eleven".to_owned()))
    );
}

#[test]
fn missing_database_and_write_sql_fail_without_creating_or_mutating() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("missing.db");
    let missing_repository = SqliteAIQueryRepository::new(&missing);
    assert!(missing_repository.get_all_tables().is_err());
    assert!(!missing.exists());

    let (_fixture_directory, path) = fixture();
    let repository = SqliteAIQueryRepository::new(&path);
    assert_eq!(
        repository.execute_readonly("UPDATE players SET wins = 0", &[], 25),
        Err(QueryRepositoryError::ReadOnlyViolation)
    );
    let wins: i64 = Connection::open(path)
        .expect("reopen fixture")
        .query_row(
            "SELECT wins FROM players WHERE discord_id = 101",
            [],
            |row| row.get(0),
        )
        .expect("read unchanged wins");
    assert_eq!(wins, 9);
}
