import sqlite3

import pytest

from domain.models.duel import DuelChallenge, DuelStatus


def test_duel_model_reads_sqlite_row(repo_db_path):
    conn = sqlite3.connect(repo_db_path)
    conn.row_factory = sqlite3.Row
    row = conn.execute(
        """
        SELECT 7 AS challenge_id, 42 AS guild_id, 9 AS channel_id,
               NULL AS message_id, 1 AS challenger_id, 2 AS recipient_id,
               501 AS wager, 'pending' AS status, NULL AS trial_type,
               1400.0 AS challenger_glicko, 80.0 AS challenger_rd,
               1500.0 AS recipient_glicko, 70.0 AS recipient_rd,
               100 AS created_at, 200 AS expires_at, 120 AS next_reminder_at,
               NULL AS responded_at, NULL AS resolved_at,
               NULL AS winner_id, NULL AS resolution_actor_id
        """
    ).fetchone()
    challenge = DuelChallenge.from_row(row)
    assert challenge.status is DuelStatus.PENDING
    assert challenge.trial_type is None
    assert challenge.decline_penalty == 251


def test_duel_schema_enforces_wager_and_status(repo_db_path):
    conn = sqlite3.connect(repo_db_path)
    columns = {row[1] for row in conn.execute("PRAGMA table_info(duel_challenges)")}
    assert {"challenge_id", "guild_id", "message_id", "next_reminder_at"} <= columns
    indexes = {row[1] for row in conn.execute("PRAGMA index_list(duel_challenges)")}
    assert {
        "idx_duel_guild_status",
        "idx_duel_challenger_history",
        "idx_duel_recipient_history",
        "idx_duel_due_expiry",
        "idx_duel_due_reminder",
    } <= indexes

    values = (42, 9, 1, 2, 499, "pending", 1400.0, 80.0, 1500.0, 70.0, 100, 200)
    with pytest.raises(sqlite3.IntegrityError):
        conn.execute(
            """
            INSERT INTO duel_challenges (
                guild_id, channel_id, challenger_id, recipient_id, wager, status,
                challenger_glicko, challenger_rd, recipient_glicko, recipient_rd,
                created_at, expires_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            values,
        )
