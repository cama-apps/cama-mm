"""Metadata-only persistence for LLM request telemetry."""

from __future__ import annotations

import time

from repositories.base_repository import BaseRepository
from repositories.interfaces import ILLMRequestRepository


class LLMRequestRepository(BaseRepository, ILLMRequestRepository):
    """Store and summarize actual LLM provider attempts.

    This repository deliberately accepts only operational metadata. Prompt and
    response bodies, user identifiers, guild identifiers, and API credentials
    do not belong in this audit trail.
    """

    def record_attempt(
        self,
        *,
        feature: str,
        operation: str,
        provider: str,
        model: str,
        success: bool,
        latency_ms: int | float,
        prompt_tokens: int | None = None,
        completion_tokens: int | None = None,
        total_tokens: int | None = None,
        error_type: str | None = None,
        created_at: int | None = None,
    ) -> int:
        """Persist one LLM provider attempt and return its identifier."""
        feature = self._required_label(feature, "feature")
        operation = self._required_label(operation, "operation")
        provider = self._required_label(provider, "provider")
        model = self._required_label(model, "model")
        latency = self._nonnegative_integer(latency_ms, "latency_ms")
        prompt = self._optional_nonnegative_integer(prompt_tokens, "prompt_tokens")
        completion = self._optional_nonnegative_integer(
            completion_tokens,
            "completion_tokens",
        )
        total = self._optional_nonnegative_integer(total_tokens, "total_tokens")
        timestamp = int(time.time()) if created_at is None else int(created_at)
        safe_error_type = None if error_type is None else str(error_type).strip() or None

        with self.connection() as conn:
            cursor = conn.execute(
                """
                INSERT INTO llm_request_attempts (
                    feature, operation, provider, model, success, latency_ms,
                    prompt_tokens, completion_tokens, total_tokens, error_type,
                    created_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    feature,
                    operation,
                    provider,
                    model,
                    int(bool(success)),
                    latency,
                    prompt,
                    completion,
                    total,
                    safe_error_type,
                    timestamp,
                ),
            )
            return int(cursor.lastrowid)

    def get_usage_summary(
        self,
        *,
        since: int | None = None,
        until: int | None = None,
        limit: int = 100,
    ) -> list[dict]:
        """Aggregate attempts for identifying the busiest LLM workloads."""
        bounded_limit = self._positive_limit(limit)
        clauses: list[str] = []
        params: list[int] = []
        if since is not None:
            clauses.append("created_at >= ?")
            params.append(int(since))
        if until is not None:
            clauses.append("created_at < ?")
            params.append(int(until))
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""

        with self.connection() as conn:
            rows = conn.execute(
                f"""
                SELECT
                    feature,
                    operation,
                    provider,
                    model,
                    COUNT(*) AS attempt_count,
                    SUM(success) AS success_count,
                    COUNT(*) - SUM(success) AS failure_count,
                    ROUND(AVG(latency_ms), 2) AS avg_latency_ms,
                    SUM(prompt_tokens) AS prompt_tokens,
                    SUM(completion_tokens) AS completion_tokens,
                    SUM(total_tokens) AS total_tokens,
                    MIN(created_at) AS first_attempt_at,
                    MAX(created_at) AS last_attempt_at
                FROM llm_request_attempts
                {where}
                GROUP BY feature, operation, provider, model
                ORDER BY attempt_count DESC, feature, operation, provider, model
                LIMIT ?
                """,
                (*params, bounded_limit),
            ).fetchall()
        return [dict(row) for row in rows]

    def prune_before(self, cutoff_created_at: int, *, limit: int = 10_000) -> int:
        """Delete a bounded batch of oldest attempts before the cutoff."""
        bounded_limit = self._positive_limit(limit)
        with self.connection() as conn:
            cursor = conn.execute(
                """
                DELETE FROM llm_request_attempts
                WHERE attempt_id IN (
                    SELECT attempt_id
                    FROM llm_request_attempts
                    WHERE created_at < ?
                    ORDER BY created_at, attempt_id
                    LIMIT ?
                )
                """,
                (int(cutoff_created_at), bounded_limit),
            )
            return max(int(cursor.rowcount), 0)

    @staticmethod
    def _required_label(value: str, field: str) -> str:
        label = str(value).strip()
        if not label:
            raise ValueError(f"{field} must not be empty")
        return label

    @staticmethod
    def _nonnegative_integer(value: int | float, field: str) -> int:
        number = round(float(value))
        if number < 0:
            raise ValueError(f"{field} must be non-negative")
        return number

    @classmethod
    def _optional_nonnegative_integer(cls, value: int | None, field: str) -> int | None:
        if value is None:
            return None
        return cls._nonnegative_integer(value, field)

    @staticmethod
    def _positive_limit(limit: int) -> int:
        bounded_limit = int(limit)
        if bounded_limit <= 0:
            raise ValueError("limit must be positive")
        return bounded_limit
