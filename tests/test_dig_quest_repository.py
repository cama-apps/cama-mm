"""Tests for DigQuestRepository persistence layer."""

import pytest

from repositories.dig_quest_repository import DigQuestRepository, QuestState
from tests.conftest import TEST_GUILD_ID


@pytest.fixture
def quest_repo(repo_db_path):
    return DigQuestRepository(repo_db_path)


def test_get_state_returns_empty_for_unknown_player(quest_repo):
    state = quest_repo.get_state(discord_id=1, guild_id=TEST_GUILD_ID)
    assert state.active_quest_id is None
    assert state.active_quest_step is None
    assert state.completed_quests == ()


def test_set_active_inserts_new_row(quest_repo):
    state = quest_repo.set_active(
        discord_id=1, guild_id=TEST_GUILD_ID, quest_id="agh_trial", step=1,
    )
    assert state.active_quest_id == "agh_trial"
    assert state.active_quest_step == 1
    assert state.completed_quests == ()
    assert state.last_updated_at is not None


def test_set_active_advances_step(quest_repo):
    quest_repo.set_active(1, TEST_GUILD_ID, "agh_trial", 1)
    state = quest_repo.set_active(1, TEST_GUILD_ID, "agh_trial", 2)
    assert state.active_quest_step == 2


def test_set_active_refuses_to_overwrite_different_quest(quest_repo):
    quest_repo.set_active(1, TEST_GUILD_ID, "agh_trial", 3)
    with pytest.raises(ValueError):
        quest_repo.set_active(1, TEST_GUILD_ID, "necropolis", 1)


def test_complete_quest_clears_active_and_appends_completed(quest_repo):
    quest_repo.set_active(1, TEST_GUILD_ID, "agh_trial", 5)
    state = quest_repo.complete_quest(1, TEST_GUILD_ID, "agh_trial")
    assert state.active_quest_id is None
    assert state.active_quest_step is None
    assert "agh_trial" in state.completed_quests


def test_complete_quest_is_idempotent(quest_repo):
    quest_repo.set_active(1, TEST_GUILD_ID, "agh_trial", 5)
    quest_repo.complete_quest(1, TEST_GUILD_ID, "agh_trial")
    state = quest_repo.complete_quest(1, TEST_GUILD_ID, "agh_trial")
    assert state.completed_quests.count("agh_trial") == 1


def test_complete_quest_then_start_new_quest(quest_repo):
    quest_repo.set_active(1, TEST_GUILD_ID, "agh_trial", 5)
    quest_repo.complete_quest(1, TEST_GUILD_ID, "agh_trial")
    state = quest_repo.set_active(1, TEST_GUILD_ID, "necropolis", 1)
    assert state.active_quest_id == "necropolis"
    assert state.active_quest_step == 1
    assert "agh_trial" in state.completed_quests


def test_guild_isolation(quest_repo):
    """Same player, two guilds — quests track independently."""
    quest_repo.set_active(1, TEST_GUILD_ID, "agh_trial", 3)
    quest_repo.complete_quest(1, TEST_GUILD_ID, "agh_trial")

    other_guild = TEST_GUILD_ID + 1
    state = quest_repo.get_state(1, other_guild)
    assert state.active_quest_id is None
    assert state.completed_quests == ()
    # In the other guild, the quest is still available.
    state = quest_repo.set_active(1, other_guild, "agh_trial", 1)
    assert state.active_quest_id == "agh_trial"


def test_abandon_active_clears_quest_but_keeps_completed_list(quest_repo):
    quest_repo.set_active(1, TEST_GUILD_ID, "agh_trial", 5)
    quest_repo.complete_quest(1, TEST_GUILD_ID, "agh_trial")
    quest_repo.set_active(1, TEST_GUILD_ID, "necropolis", 2)

    state = quest_repo.abandon_active(1, TEST_GUILD_ID)
    assert state.active_quest_id is None
    assert state.active_quest_step is None
    assert "agh_trial" in state.completed_quests


def test_quest_state_helpers():
    s = QuestState(
        discord_id=1, guild_id=0,
        active_quest_id="x", active_quest_step=2,
        completed_quests=("a", "b"),
    )
    assert s.is_active("x")
    assert not s.is_active("y")
    assert s.has_completed("a")
    assert not s.has_completed("z")


def test_guild_id_none_normalizes_to_zero(quest_repo):
    """guild_id=None should be normalized; subsequent reads with 0 see the row."""
    quest_repo.set_active(1, None, "agh_trial", 1)
    state = quest_repo.get_state(1, 0)
    assert state.active_quest_id == "agh_trial"
