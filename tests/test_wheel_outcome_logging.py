"""Focused persistence tests for semantic wheel outcome history."""

import sqlite3
from unittest.mock import MagicMock

from commands.betting import _canonical_wheel_outcome_code
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID


def _wheel_row(player_repository, spin_id: int) -> sqlite3.Row:
    with sqlite3.connect(player_repository.db_path) as conn:
        conn.row_factory = sqlite3.Row
        row = conn.execute(
            "SELECT * FROM wheel_spins WHERE spin_id = ?",
            (spin_id,),
        ).fetchone()
    assert row is not None
    return row


def test_log_wheel_spin_old_call_remains_compatible(player_repository):
    player_repository.add(9101, "Legacy Spinner", TEST_GUILD_ID)

    spin_id = player_repository.log_wheel_spin(
        9101,
        TEST_GUILD_ID,
        25,
        1_700_000_000,
    )

    row = _wheel_row(player_repository, spin_id)
    assert row["result"] == 25
    assert row["outcome_code"] is None
    assert row["is_bonus"] == 0
    assert row["event_id"] is None
    assert row["outcome_metadata"] is None


def test_log_wheel_spin_stores_deterministic_metadata(player_repository):
    player_repository.add(9102, "Audited Spinner", TEST_GUILD_ID)

    spin_id = player_repository.log_wheel_spin(
        9102,
        TEST_GUILD_ID,
        0,
        1_700_000_001,
        outcome_code="LIGHTNING_BOLT",
        is_bonus=True,
        event_id="discord-interaction-42",
        outcome_metadata={"z": 2, "a": {"b": True}},
    )

    row = _wheel_row(player_repository, spin_id)
    assert row["outcome_code"] == "LIGHTNING_BOLT"
    assert row["is_bonus"] == 1
    assert row["event_id"] == "discord-interaction-42"
    assert row["outcome_metadata"] == '{"a":{"b":true},"z":2}'


def test_player_service_forwards_exact_wheel_fields():
    repo = MagicMock()
    repo.log_wheel_spin.return_value = 17
    service = PlayerService(repo)
    metadata = {"lightning_total": 40, "lightning_count": 2}

    spin_id = service.log_wheel_spin(
        9103,
        TEST_GUILD_ID,
        0,
        1_700_000_002,
        True,
        False,
        outcome_code="LIGHTNING_BOLT",
        is_bonus=True,
        event_id="event-17",
        outcome_metadata=metadata,
    )

    assert spin_id == 17
    repo.log_wheel_spin.assert_called_once_with(
        9103,
        TEST_GUILD_ID,
        0,
        1_700_000_002,
        True,
        False,
        outcome_code="LIGHTNING_BOLT",
        is_bonus=True,
        event_id="event-17",
        outcome_metadata=metadata,
    )


def test_canonical_codes_preserve_dynamic_wedge_identity():
    assert (
        _canonical_wheel_outcome_code(
            ("-92", -92, "#1a1a1a"),
            -92,
            is_bankrupt=False,
            is_golden=False,
        )
        == "BANKRUPT"
    )
    assert (
        _canonical_wheel_outcome_code(
            ("-390", -390, "#4a3000"),
            -390,
            is_bankrupt=False,
            is_golden=True,
        )
        == "OVEREXTENDED"
    )
    assert (
        _canonical_wheel_outcome_code(
            ("200", 200, "#fffacd"),
            200,
            is_bankrupt=False,
            is_golden=True,
        )
        == "CROWN"
    )
