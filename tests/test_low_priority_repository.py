"""Focused repository tests for configurable low-priority assignments."""

import pytest

from domain.low_priority_constants import LOW_PRIORITY_REQUIRED_WINS
from repositories.low_priority_repository import LowPriorityRepository
from tests.conftest import TEST_GUILD_ID


def test_assignment_defaults_to_three_wins_without_pending_watermark(repo_db_path):
    repo = LowPriorityRepository(repo_db_path)

    state = repo.set_low_priority(
        101,
        TEST_GUILD_ID,
        set_by=901,
        reason="default assignment",
    )

    assert state.wins_required == LOW_PRIORITY_REQUIRED_WINS
    assert state.wins_remaining == LOW_PRIORITY_REQUIRED_WINS
    assert state.start_pending_match_id is None
    assert repo.get_state(101, TEST_GUILD_ID) == state


def test_reassignment_replaces_win_count_and_pending_watermark(repo_db_path):
    repo = LowPriorityRepository(repo_db_path)
    repo.set_low_priority(
        101,
        TEST_GUILD_ID,
        set_by=901,
        reason="first assignment",
        wins_required=5,
        start_pending_match_id=41,
    )
    repo.clear_low_priority(
        101,
        TEST_GUILD_ID,
        removed_by=902,
        reason="reviewed",
    )

    replaced = repo.set_low_priority(
        101,
        TEST_GUILD_ID,
        set_by=903,
        reason="replacement",
        wins_required=20,
        start_pending_match_id=84,
    )

    assert replaced.active is True
    assert replaced.wins_required == 20
    assert replaced.wins_remaining == 20
    assert replaced.start_pending_match_id == 84
    assert replaced.reason == "replacement"
    assert replaced.set_by == 903
    assert replaced.removed_by is None
    assert replaced.removed_reason is None
    assert repo.list_active(TEST_GUILD_ID) == [replaced]


@pytest.mark.parametrize("wins_required", [0, 21, -1, True, 1.5])
def test_assignment_rejects_invalid_required_wins(repo_db_path, wins_required):
    repo = LowPriorityRepository(repo_db_path)

    with pytest.raises(ValueError, match="wins_required"):
        repo.set_low_priority(
            101,
            TEST_GUILD_ID,
            set_by=901,
            reason=None,
            wins_required=wins_required,
        )

    assert repo.get_state(101, TEST_GUILD_ID) is None


@pytest.mark.parametrize("watermark", [-1, True, 1.5])
def test_assignment_rejects_invalid_pending_match_watermark(repo_db_path, watermark):
    repo = LowPriorityRepository(repo_db_path)

    with pytest.raises(ValueError, match="start_pending_match_id"):
        repo.set_low_priority(
            101,
            TEST_GUILD_ID,
            set_by=901,
            reason=None,
            start_pending_match_id=watermark,
        )

    assert repo.get_state(101, TEST_GUILD_ID) is None
