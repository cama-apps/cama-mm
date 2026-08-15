//! Existing-schema persistence for metadata-only LLM provider attempts.
//!
//! Prompt/response bodies, user or guild identifiers, and credentials are not
//! represented by this API, so callers cannot accidentally persist them.

use std::path::{Path, PathBuf};

use rusqlite::params;
use thiserror::Error;

use crate::open_runtime_connection;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LlmAttemptRecord {
    pub feature: String,
    pub operation: String,
    pub provider: String,
    pub model: String,
    pub success: bool,
    pub latency_ms: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub error_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LlmUsageSummary {
    pub feature: String,
    pub operation: String,
    pub provider: String,
    pub model: String,
    pub attempt_count: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub avg_latency_ms: f64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub first_attempt_at: i64,
    pub last_attempt_at: i64,
}

#[derive(Clone, Debug)]
pub struct LlmRequestRepository {
    path: PathBuf,
}

impl LlmRequestRepository {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn record_attempt(&self, attempt: &LlmAttemptRecord) -> Result<i64, LlmRequestError> {
        self.record_attempt_at(attempt, None)
    }

    /// Persist an attempt at an injected timestamp for replay/import tests.
    /// Production callers pass `None`, matching Python's current-time default.
    pub fn record_attempt_at(
        &self,
        attempt: &LlmAttemptRecord,
        created_at: Option<i64>,
    ) -> Result<i64, LlmRequestError> {
        validate_attempt(attempt)?;
        let connection = open_runtime_connection(&self.path)?;
        connection.execute(
            "INSERT INTO llm_request_attempts (
                feature, operation, provider, model, success, latency_ms,
                prompt_tokens, completion_tokens, total_tokens, error_type,
                created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                       COALESCE(?, CAST(strftime('%s', 'now') AS INTEGER)))",
            params![
                attempt.feature.trim(),
                attempt.operation.trim(),
                attempt.provider.trim(),
                attempt.model.trim(),
                i64::from(attempt.success),
                attempt.latency_ms,
                attempt.prompt_tokens,
                attempt.completion_tokens,
                attempt.total_tokens,
                attempt
                    .error_type
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty()),
                created_at,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn get_usage_summary(
        &self,
        since: Option<i64>,
        until: Option<i64>,
        limit: i64,
    ) -> Result<Vec<LlmUsageSummary>, LlmRequestError> {
        if limit <= 0 {
            return Err(LlmRequestError::Invalid("limit must be positive"));
        }
        let connection = open_runtime_connection(&self.path)?;
        let mut statement = connection.prepare(
            "SELECT feature, operation, provider, model,
                    COUNT(*) AS attempt_count,
                    SUM(success) AS success_count,
                    COUNT(*) - SUM(success) AS failure_count,
                    ROUND(AVG(latency_ms), 2) AS avg_latency_ms,
                    SUM(prompt_tokens), SUM(completion_tokens), SUM(total_tokens),
                    MIN(created_at), MAX(created_at)
               FROM llm_request_attempts
              WHERE (?1 IS NULL OR created_at >= ?1)
                AND (?2 IS NULL OR created_at < ?2)
              GROUP BY feature, operation, provider, model
              ORDER BY attempt_count DESC, feature, operation, provider, model
              LIMIT ?3",
        )?;
        let rows = statement.query_map(params![since, until, limit], |row| {
            Ok(LlmUsageSummary {
                feature: row.get(0)?,
                operation: row.get(1)?,
                provider: row.get(2)?,
                model: row.get(3)?,
                attempt_count: row.get(4)?,
                success_count: row.get(5)?,
                failure_count: row.get(6)?,
                avg_latency_ms: row.get(7)?,
                prompt_tokens: row.get(8)?,
                completion_tokens: row.get(9)?,
                total_tokens: row.get(10)?,
                first_attempt_at: row.get(11)?,
                last_attempt_at: row.get(12)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn prune_before(
        &self,
        cutoff_created_at: i64,
        limit: i64,
    ) -> Result<usize, LlmRequestError> {
        if limit <= 0 {
            return Err(LlmRequestError::Invalid("limit must be positive"));
        }
        let connection = open_runtime_connection(&self.path)?;
        connection
            .execute(
                "DELETE FROM llm_request_attempts
                 WHERE attempt_id IN (
                    SELECT attempt_id FROM llm_request_attempts
                    WHERE created_at < ?
                    ORDER BY created_at, attempt_id
                    LIMIT ?
                 )",
                params![cutoff_created_at, limit],
            )
            .map_err(Into::into)
    }
}

fn validate_attempt(attempt: &LlmAttemptRecord) -> Result<(), LlmRequestError> {
    for (label, value) in [
        ("feature", attempt.feature.as_str()),
        ("operation", attempt.operation.as_str()),
        ("provider", attempt.provider.as_str()),
        ("model", attempt.model.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(LlmRequestError::Invalid(label));
        }
    }
    if attempt.latency_ms < 0
        || [
            attempt.prompt_tokens,
            attempt.completion_tokens,
            attempt.total_tokens,
        ]
        .into_iter()
        .flatten()
        .any(|tokens| tokens < 0)
    {
        return Err(LlmRequestError::Invalid(
            "latency and token counts must be nonnegative",
        ));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum LlmRequestError {
    #[error("SQLite LLM request repository error: {0}")]
    SQLite(#[from] rusqlite::Error),
    #[error("invalid LLM attempt metadata: {0}")]
    Invalid(&'static str),
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;

    fn fixture() -> (NamedTempFile, LlmRequestRepository) {
        let file = NamedTempFile::new().expect("temporary database");
        let connection = Connection::open(file.path()).expect("open database");
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE llm_request_attempts (
                    attempt_id INTEGER PRIMARY KEY AUTOINCREMENT,
                    feature TEXT NOT NULL CHECK(length(trim(feature)) > 0),
                    operation TEXT NOT NULL CHECK(length(trim(operation)) > 0),
                    provider TEXT NOT NULL CHECK(length(trim(provider)) > 0),
                    model TEXT NOT NULL CHECK(length(trim(model)) > 0),
                    success INTEGER NOT NULL CHECK(success IN (0, 1)),
                    latency_ms INTEGER NOT NULL CHECK(latency_ms >= 0),
                    prompt_tokens INTEGER CHECK(prompt_tokens >= 0),
                    completion_tokens INTEGER CHECK(completion_tokens >= 0),
                    total_tokens INTEGER CHECK(total_tokens >= 0),
                    error_type TEXT,
                    created_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s', 'now') AS INTEGER))
                 );
                 CREATE INDEX idx_llm_attempts_created
                     ON llm_request_attempts(created_at);
                 CREATE INDEX idx_llm_attempts_workload
                     ON llm_request_attempts(feature, operation, created_at);
                 CREATE INDEX idx_llm_attempts_provider_model
                     ON llm_request_attempts(provider, model, created_at);",
            )
            .expect("create schema");
        let repository = LlmRequestRepository::new(file.path());
        (file, repository)
    }

    fn record() -> LlmAttemptRecord {
        LlmAttemptRecord {
            feature: "dig.flavor".to_owned(),
            operation: "tool_call".to_owned(),
            provider: "groq".to_owned(),
            model: "qwen/qwen3.6-27b".to_owned(),
            success: true,
            latency_ms: 123,
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            total_tokens: Some(14),
            error_type: None,
        }
    }

    #[test]
    fn records_only_typed_operational_metadata() {
        let (file, repository) = fixture();
        assert_eq!(repository.record_attempt(&record()).expect("record"), 1);
        let connection = Connection::open(file.path()).expect("reopen database");
        let row = connection
            .query_row(
                "SELECT feature, operation, provider, model, success, latency_ms,
                        prompt_tokens, completion_tokens, total_tokens, error_type
                 FROM llm_request_attempts",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, Option<String>>(9)?,
                    ))
                },
            )
            .expect("attempt row");
        assert_eq!(
            row,
            (
                "dig.flavor".to_owned(),
                "tool_call".to_owned(),
                "groq".to_owned(),
                "qwen/qwen3.6-27b".to_owned(),
                1,
                123,
                10,
                4,
                14,
                None,
            )
        );
    }

    #[test]
    fn validation_and_bounded_pruning_match_existing_schema_contract() {
        let (file, repository) = fixture();
        let mut invalid = record();
        invalid.feature = "  ".to_owned();
        assert!(matches!(
            repository.record_attempt(&invalid),
            Err(LlmRequestError::Invalid("feature"))
        ));
        repository.record_attempt(&record()).expect("first row");
        repository.record_attempt(&record()).expect("second row");
        let connection = Connection::open(file.path()).expect("reopen database");
        connection
            .execute("UPDATE llm_request_attempts SET created_at = 10", [])
            .expect("age rows");
        drop(connection);
        assert_eq!(repository.prune_before(11, 1).expect("prune"), 1);
        assert_eq!(repository.prune_before(11, 10).expect("prune"), 1);
        assert!(matches!(
            repository.prune_before(11, 0),
            Err(LlmRequestError::Invalid("limit must be positive"))
        ));
    }

    #[test]
    fn test_records_success_and_failure_metadata() {
        let (file, repository) = fixture();
        let success = LlmAttemptRecord {
            feature: "dig".to_owned(),
            operation: "narration".to_owned(),
            provider: "groq".to_owned(),
            model: "openai/gpt-oss-20b".to_owned(),
            success: true,
            latency_ms: 124,
            prompt_tokens: Some(100),
            completion_tokens: Some(25),
            total_tokens: Some(125),
            error_type: None,
        };
        let success_id = repository
            .record_attempt_at(&success, Some(1_000))
            .expect("success attempt");
        let failure = LlmAttemptRecord {
            feature: "ask".to_owned(),
            operation: "sql_generation".to_owned(),
            provider: "cerebras".to_owned(),
            model: "gemma-4-31b".to_owned(),
            success: false,
            latency_ms: 50,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            error_type: Some("RateLimitError".to_owned()),
        };
        let failure_id = repository
            .record_attempt_at(&failure, Some(1_001))
            .expect("failure attempt");
        assert!(failure_id > success_id);

        let connection = Connection::open(file.path()).expect("reopen database");
        let success_row = connection
            .query_row(
                "SELECT feature,operation,provider,model,success,latency_ms,
                        prompt_tokens,completion_tokens,total_tokens,error_type,created_at
                   FROM llm_request_attempts WHERE attempt_id=?1",
                [success_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, i64>(10)?,
                    ))
                },
            )
            .expect("success row");
        assert_eq!(
            success_row,
            (
                "dig".to_owned(),
                "narration".to_owned(),
                "groq".to_owned(),
                "openai/gpt-oss-20b".to_owned(),
                1,
                124,
                Some(100),
                Some(25),
                Some(125),
                None,
                1_000,
            )
        );
        let failure_row: (i64, Option<String>, Option<i64>) = connection
            .query_row(
                "SELECT success,error_type,prompt_tokens
                   FROM llm_request_attempts WHERE attempt_id=?1",
                [failure_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("failure row");
        assert_eq!(failure_row, (0, Some("RateLimitError".to_owned()), None));
    }

    #[test]
    fn test_usage_summary_groups_attempts_and_filters_time() {
        let (_file, repository) = fixture();
        for (feature, operation, model, success, latency, prompt, completion, total, at) in [
            (
                "dig",
                "narration",
                "model-a",
                true,
                100,
                Some(10),
                Some(5),
                Some(15),
                100,
            ),
            (
                "dig",
                "narration",
                "model-a",
                false,
                300,
                None,
                None,
                None,
                110,
            ),
            (
                "ask",
                "sql",
                "model-b",
                true,
                50,
                Some(20),
                Some(4),
                Some(24),
                120,
            ),
            (
                "dig",
                "narration",
                "model-a",
                true,
                500,
                Some(30),
                Some(8),
                Some(38),
                200,
            ),
        ] {
            repository
                .record_attempt_at(
                    &LlmAttemptRecord {
                        feature: feature.to_owned(),
                        operation: operation.to_owned(),
                        provider: "groq".to_owned(),
                        model: model.to_owned(),
                        success,
                        latency_ms: latency,
                        prompt_tokens: prompt,
                        completion_tokens: completion,
                        total_tokens: total,
                        error_type: (!success).then(|| "TimeoutError".to_owned()),
                    },
                    Some(at),
                )
                .expect("attempt");
        }

        let summary = repository
            .get_usage_summary(Some(100), Some(200), 100)
            .expect("usage summary");
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].feature, "dig");
        assert_eq!(summary[0].operation, "narration");
        assert_eq!(summary[0].provider, "groq");
        assert_eq!(summary[0].model, "model-a");
        assert_eq!(summary[0].attempt_count, 2);
        assert_eq!(summary[0].success_count, 1);
        assert_eq!(summary[0].failure_count, 1);
        assert_eq!(summary[0].avg_latency_ms, 200.0);
        assert_eq!(summary[0].prompt_tokens, Some(10));
        assert_eq!(summary[0].completion_tokens, Some(5));
        assert_eq!(summary[0].total_tokens, Some(15));
        assert_eq!(summary[0].first_attempt_at, 100);
        assert_eq!(summary[0].last_attempt_at, 110);
        assert_eq!(summary[1].feature, "ask");
    }

    #[test]
    fn test_prune_before_is_bounded_and_oldest_first() {
        let (file, repository) = fixture();
        for created_at in [30, 10, 20, 40] {
            repository
                .record_attempt_at(&record(), Some(created_at))
                .expect("attempt");
        }
        assert_eq!(repository.prune_before(35, 2).expect("first prune"), 2);
        let connection = Connection::open(file.path()).expect("reopen database");
        let timestamps = connection
            .prepare("SELECT created_at FROM llm_request_attempts ORDER BY created_at")
            .expect("timestamp query")
            .query_map([], |row| row.get::<_, i64>(0))
            .expect("timestamp rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("timestamps");
        assert_eq!(timestamps, [30, 40]);
        drop(connection);
        assert_eq!(repository.prune_before(35, 2).expect("second prune"), 1);
        assert_eq!(repository.prune_before(35, 2).expect("final prune"), 0);
    }

    #[test]
    fn test_record_attempt_rejects_invalid_metadata_overrides0_feature_must_not_be_empty() {
        let (_file, repository) = fixture();
        let mut invalid = record();
        invalid.feature = "  ".to_owned();
        assert!(matches!(
            repository.record_attempt(&invalid),
            Err(LlmRequestError::Invalid("feature"))
        ));
    }

    #[test]
    fn test_record_attempt_rejects_invalid_metadata_overrides1_latency_ms_must_be_non_negative() {
        let (_file, repository) = fixture();
        let mut invalid = record();
        invalid.latency_ms = -1;
        assert!(matches!(
            repository.record_attempt(&invalid),
            Err(LlmRequestError::Invalid(
                "latency and token counts must be nonnegative"
            ))
        ));
    }

    #[test]
    fn test_record_attempt_rejects_invalid_metadata_overrides2_total_tokens_must_be_non_negative() {
        let (_file, repository) = fixture();
        let mut invalid = record();
        invalid.total_tokens = Some(-1);
        assert!(matches!(
            repository.record_attempt(&invalid),
            Err(LlmRequestError::Invalid(
                "latency and token counts must be nonnegative"
            ))
        ));
    }

    #[test]
    fn test_migration_adds_audit_indexes_without_sensitive_columns() {
        let (file, _repository) = fixture();
        let connection = Connection::open(file.path()).expect("reopen database");
        let columns = connection
            .prepare("PRAGMA table_info(llm_request_attempts)")
            .expect("column query")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("column rows")
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .expect("columns");
        for forbidden in ["prompt", "response", "api_key", "guild_id", "discord_id"] {
            assert!(!columns.contains(forbidden));
        }
        let indexes = connection
            .prepare("PRAGMA index_list(llm_request_attempts)")
            .expect("index query")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("index rows")
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .expect("indexes");
        for expected in [
            "idx_llm_attempts_created",
            "idx_llm_attempts_workload",
            "idx_llm_attempts_provider_model",
        ] {
            assert!(indexes.contains(expected));
        }
    }
}
