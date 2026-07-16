"""Slash-command activity: commands are noted in memory during the day and
rolled up once per game-day (4 AM PST) into lottery eligibility."""

import datetime
from types import SimpleNamespace

import pytest

from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID

LOTTERY_DAYS = 14


@pytest.fixture
def bot_module():
    import bot as bot_module

    bot_module._pending_command_activity.clear()
    yield bot_module
    bot_module._pending_command_activity.clear()


@pytest.fixture
def repo(repo_db_path):
    return PlayerRepository(repo_db_path)


def _interaction(user_id: int, guild_id: int | None):
    guild = SimpleNamespace(id=guild_id) if guild_id is not None else None
    return SimpleNamespace(user=SimpleNamespace(id=user_id), guild=guild)


def test_note_records_command_user(bot_module, monkeypatch):
    monkeypatch.setattr(bot_module, "ACTIVITY_TRACKING_ENABLED", True)
    bot_module._note_command_activity(_interaction(111, TEST_GUILD_ID))
    assert (TEST_GUILD_ID, 111) in bot_module._pending_command_activity


def test_note_skips_when_disabled(bot_module, monkeypatch):
    monkeypatch.setattr(bot_module, "ACTIVITY_TRACKING_ENABLED", False)
    bot_module._note_command_activity(_interaction(111, TEST_GUILD_ID))
    assert not bot_module._pending_command_activity


def test_rollup_marks_users_active_and_drains(bot_module, repo, monkeypatch):
    monkeypatch.setattr(bot_module, "ACTIVITY_TRACKING_ENABLED", True)
    monkeypatch.setattr(bot_module.bot, "player_repo", repo, raising=False)
    repo.add(discord_id=111, discord_username="U", guild_id=TEST_GUILD_ID)
    assert not repo.is_active_for_lottery(111, TEST_GUILD_ID, LOTTERY_DAYS)

    bot_module._note_command_activity(_interaction(111, TEST_GUILD_ID))
    processed = bot_module._run_activity_rollup()

    assert processed == 1
    assert repo.is_active_for_lottery(111, TEST_GUILD_ID, LOTTERY_DAYS)
    assert not bot_module._pending_command_activity  # drained


def test_rollup_unregistered_user_is_noop(bot_module, repo, monkeypatch):
    monkeypatch.setattr(bot_module, "ACTIVITY_TRACKING_ENABLED", True)
    monkeypatch.setattr(bot_module.bot, "player_repo", repo, raising=False)
    bot_module._note_command_activity(_interaction(999, TEST_GUILD_ID))
    bot_module._run_activity_rollup()
    assert not repo.is_active_for_lottery(999, TEST_GUILD_ID, LOTTERY_DAYS)


def test_rollup_empty_set_is_noop(bot_module, repo, monkeypatch):
    monkeypatch.setattr(bot_module.bot, "player_repo", repo, raising=False)
    assert bot_module._run_activity_rollup() == 0


def test_seconds_until_next_rollup_targets_noon_utc(bot_module):
    # 10:00 UTC -> 2h until the 12:00 UTC (4 AM PST) rollover.
    before = datetime.datetime(2026, 7, 15, 10, 0, tzinfo=datetime.UTC)
    assert bot_module._seconds_until_next_activity_rollup(before) == 2 * 3600
    # 12:30 UTC -> rolls to the next day's 12:00 UTC (23.5h away).
    after = datetime.datetime(2026, 7, 15, 12, 30, tzinfo=datetime.UTC)
    assert bot_module._seconds_until_next_activity_rollup(after) == 23.5 * 3600
