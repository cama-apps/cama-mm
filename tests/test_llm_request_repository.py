"""Repository tests for metadata-only LLM request telemetry."""

import sqlite3

import pytest

from repositories.llm_request_repository import LLMRequestRepository


@pytest.fixture
def llm_request_repository(repo_db_path):
    return LLMRequestRepository(repo_db_path)


def test_records_success_and_failure_metadata(llm_request_repository):
    success_id = llm_request_repository.record_attempt(
        feature="dig",
        operation="narration",
        provider="groq",
        model="openai/gpt-oss-20b",
        success=True,
        latency_ms=123.6,
        prompt_tokens=100,
        completion_tokens=25,
        total_tokens=125,
        created_at=1_000,
    )
    failure_id = llm_request_repository.record_attempt(
        feature="ask",
        operation="sql_generation",
        provider="cerebras",
        model="gemma-4-31b",
        success=False,
        latency_ms=50,
        error_type="RateLimitError",
        created_at=1_001,
    )

    assert failure_id > success_id
    with sqlite3.connect(llm_request_repository.db_path) as conn:
        conn.row_factory = sqlite3.Row
        rows = conn.execute(
            "SELECT * FROM llm_request_attempts ORDER BY attempt_id"
        ).fetchall()

    assert dict(rows[0]) == {
        "attempt_id": success_id,
        "feature": "dig",
        "operation": "narration",
        "provider": "groq",
        "model": "openai/gpt-oss-20b",
        "success": 1,
        "latency_ms": 124,
        "prompt_tokens": 100,
        "completion_tokens": 25,
        "total_tokens": 125,
        "error_type": None,
        "created_at": 1_000,
    }
    assert rows[1]["success"] == 0
    assert rows[1]["error_type"] == "RateLimitError"
    assert rows[1]["prompt_tokens"] is None


def test_usage_summary_groups_attempts_and_filters_time(llm_request_repository):
    attempts = [
        ("dig", "narration", "groq", "model-a", True, 100, 10, 5, 15, 100),
        ("dig", "narration", "groq", "model-a", False, 300, None, None, None, 110),
        ("ask", "sql", "groq", "model-b", True, 50, 20, 4, 24, 120),
        ("dig", "narration", "groq", "model-a", True, 500, 30, 8, 38, 200),
    ]
    for feature, operation, provider, model, success, latency, prompt, completion, total, ts in attempts:
        llm_request_repository.record_attempt(
            feature=feature,
            operation=operation,
            provider=provider,
            model=model,
            success=success,
            latency_ms=latency,
            prompt_tokens=prompt,
            completion_tokens=completion,
            total_tokens=total,
            error_type=None if success else "TimeoutError",
            created_at=ts,
        )

    summary = llm_request_repository.get_usage_summary(since=100, until=200)

    assert [row["feature"] for row in summary] == ["dig", "ask"]
    assert summary[0] == {
        "feature": "dig",
        "operation": "narration",
        "provider": "groq",
        "model": "model-a",
        "attempt_count": 2,
        "success_count": 1,
        "failure_count": 1,
        "avg_latency_ms": 200.0,
        "prompt_tokens": 10,
        "completion_tokens": 5,
        "total_tokens": 15,
        "first_attempt_at": 100,
        "last_attempt_at": 110,
    }


def test_prune_before_is_bounded_and_oldest_first(llm_request_repository):
    for created_at in (30, 10, 20, 40):
        llm_request_repository.record_attempt(
            feature="dig",
            operation="narration",
            provider="groq",
            model="model-a",
            success=True,
            latency_ms=1,
            created_at=created_at,
        )

    assert llm_request_repository.prune_before(35, limit=2) == 2
    with sqlite3.connect(llm_request_repository.db_path) as conn:
        timestamps = [
            row[0]
            for row in conn.execute(
                "SELECT created_at FROM llm_request_attempts ORDER BY created_at"
            )
        ]
    assert timestamps == [30, 40]
    assert llm_request_repository.prune_before(35, limit=2) == 1
    assert llm_request_repository.prune_before(35, limit=2) == 0


@pytest.mark.parametrize(
    ("overrides", "message"),
    [
        ({"feature": "  "}, "feature must not be empty"),
        ({"latency_ms": -1}, "latency_ms must be non-negative"),
        ({"total_tokens": -1}, "total_tokens must be non-negative"),
    ],
)
def test_record_attempt_rejects_invalid_metadata(
    llm_request_repository,
    overrides,
    message,
):
    values = {
        "feature": "dig",
        "operation": "narration",
        "provider": "groq",
        "model": "model-a",
        "success": True,
        "latency_ms": 1,
    }
    values.update(overrides)

    with pytest.raises(ValueError, match=message):
        llm_request_repository.record_attempt(**values)


def test_migration_adds_audit_indexes_without_sensitive_columns(repo_db_path):
    with sqlite3.connect(repo_db_path) as conn:
        columns = {
            row[1] for row in conn.execute("PRAGMA table_info(llm_request_attempts)")
        }
        indexes = {
            row[1] for row in conn.execute("PRAGMA index_list(llm_request_attempts)")
        }

    assert {"prompt", "response", "api_key", "guild_id", "discord_id"}.isdisjoint(columns)
    assert {
        "idx_llm_attempts_created",
        "idx_llm_attempts_workload",
        "idx_llm_attempts_provider_model",
    }.issubset(indexes)
