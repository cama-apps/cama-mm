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
