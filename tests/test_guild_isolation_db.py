"""Guild isolation tests for database.py and match_repository.py.

Verifies that guild-scoped operations in guild 1 do NOT bleed into
identical discord_id rows in guild 2.

Covers the methods fixed in the review sweep:
  - Database.delete_player (was missing guild_id filter)
  - Database.get_exclusion_counts (was missing guild_id filter)
  - Database.consume_pending_match by-id branch (was missing guild_id guard)
  - MatchRepository.update_pending_match (was missing guild_id guard)
"""

from __future__ import annotations

import json

import pytest

from database import Database
from repositories.match_repository import MatchRepository
from tests.conftest import TEST_GUILD_ID, TEST_GUILD_ID_SECONDARY

GUILD_A = TEST_GUILD_ID
GUILD_B = TEST_GUILD_ID_SECONDARY
SHARED_DISCORD_ID = 555_000_000


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _add_player(db: Database, discord_id: int, guild_id: int) -> None:
    db.add_player(
        discord_id=discord_id,
        discord_username=f"player_{discord_id}_{guild_id}",
        guild_id=guild_id,
    )


def _player_exists(db: Database, discord_id: int, guild_id: int) -> bool:
    with db.connection() as conn:
        row = conn.execute(
            "SELECT 1 FROM players WHERE discord_id = ? AND guild_id = ?",
            (discord_id, guild_id),
        ).fetchone()
    return row is not None


# ---------------------------------------------------------------------------
# #1 / #2: delete_player — guild isolation
# ---------------------------------------------------------------------------


def test_delete_player_does_not_touch_other_guild(test_db_with_schema):
    """Deleting a player in guild A must leave the same discord_id in guild B untouched."""
    db: Database = test_db_with_schema
    _add_player(db, SHARED_DISCORD_ID, GUILD_A)
    _add_player(db, SHARED_DISCORD_ID, GUILD_B)

    deleted = db.delete_player(SHARED_DISCORD_ID, guild_id=GUILD_A)

    assert deleted is True
    assert not _player_exists(db, SHARED_DISCORD_ID, GUILD_A), "row in guild A should be gone"
    assert _player_exists(db, SHARED_DISCORD_ID, GUILD_B), "row in guild B must survive"


def test_delete_player_returns_false_when_guild_mismatch(test_db_with_schema):
    """delete_player returns False when the player exists only in a different guild."""
    db: Database = test_db_with_schema
    _add_player(db, SHARED_DISCORD_ID, GUILD_B)

    result = db.delete_player(SHARED_DISCORD_ID, guild_id=GUILD_A)

    assert result is False
    assert _player_exists(db, SHARED_DISCORD_ID, GUILD_B), "guild B row must be untouched"


# ---------------------------------------------------------------------------
# #2: get_exclusion_counts — guild isolation
# ---------------------------------------------------------------------------


def _set_exclusion_count(db: Database, discord_id: int, guild_id: int, count: int) -> None:
    with db.connection() as conn:
        conn.execute(
            "UPDATE players SET exclusion_count = ? WHERE discord_id = ? AND guild_id = ?",
            (count, discord_id, guild_id),
        )


def test_get_exclusion_counts_scoped_to_guild(test_db_with_schema):
    """get_exclusion_counts with guild_id=GUILD_A must not return counts from GUILD_B rows."""
    db: Database = test_db_with_schema
    _add_player(db, SHARED_DISCORD_ID, GUILD_A)
    _add_player(db, SHARED_DISCORD_ID, GUILD_B)
    _set_exclusion_count(db, SHARED_DISCORD_ID, GUILD_A, 3)
    _set_exclusion_count(db, SHARED_DISCORD_ID, GUILD_B, 99)

    counts_a = db.get_exclusion_counts([SHARED_DISCORD_ID], guild_id=GUILD_A)
    counts_b = db.get_exclusion_counts([SHARED_DISCORD_ID], guild_id=GUILD_B)

    assert counts_a[SHARED_DISCORD_ID] == 3, "guild A count must be 3, not guild B's 99"
    assert counts_b[SHARED_DISCORD_ID] == 99


# ---------------------------------------------------------------------------
# #3: consume_pending_match by-id — guild isolation
# ---------------------------------------------------------------------------


def _insert_pending_match(db: Database, guild_id: int) -> int:
    with db.connection() as conn:
        cur = conn.execute(
            "INSERT INTO pending_matches (guild_id, payload) VALUES (?, ?)",
            (guild_id, json.dumps({"guild": guild_id})),
        )
        return cur.lastrowid


def test_consume_pending_match_by_id_guild_guard(test_db_with_schema):
    """Consuming a pending_match_id that belongs to guild B must return None when guild A is given."""
    db: Database = test_db_with_schema
    match_id_b = _insert_pending_match(db, GUILD_B)

    result = db.consume_pending_match(guild_id=GUILD_A, pending_match_id=match_id_b)

    assert result is None, "cross-guild consume must return None"

    # Row must still exist in guild B
    with db.connection() as conn:
        row = conn.execute(
            "SELECT 1 FROM pending_matches WHERE pending_match_id = ?", (match_id_b,)
        ).fetchone()
    assert row is not None, "guild B match row must survive the rejected consume"


# ---------------------------------------------------------------------------
# #4: MatchRepository.update_pending_match — guild guard
# ---------------------------------------------------------------------------


@pytest.fixture
def match_repo(repo_db_path):
    return MatchRepository(repo_db_path)


def test_update_pending_match_guild_guard(match_repo):
    """update_pending_match with the wrong guild_id must be a no-op."""
    match_id = match_repo.save_pending_match(GUILD_A, {"original": True})

    # Attempt to update it while claiming guild B ownership
    match_repo.update_pending_match(match_id, {"tampered": True}, guild_id=GUILD_B)

    # Read back — payload must be unchanged
    matches = match_repo.get_pending_matches(GUILD_A)
    assert len(matches) == 1
    payload = matches[0]
    assert payload.get("original") is True, "payload must not have been overwritten"
    assert "tampered" not in payload


def test_update_pending_match_correct_guild_succeeds(match_repo):
    """update_pending_match with the correct guild_id applies the update."""
    match_id = match_repo.save_pending_match(GUILD_A, {"original": True})

    match_repo.update_pending_match(match_id, {"updated": True}, guild_id=GUILD_A)

    matches = match_repo.get_pending_matches(GUILD_A)
    assert len(matches) == 1
    assert matches[0].get("updated") is True
