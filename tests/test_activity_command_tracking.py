"""Using any slash command records the invoking user's command time, which
keeps them active for the lottery (read live, no daily job)."""

from types import SimpleNamespace

import pytest

from repositories.player_repository import PlayerRepository
from tests.conftest import TEST_GUILD_ID

LOTTERY_DAYS = 14


@pytest.fixture
def bot_module():
    import bot as bot_module

    return bot_module


@pytest.fixture
def repo(repo_db_path):
    return PlayerRepository(repo_db_path)


def _interaction(user_id: int, guild_id: int | None):
    guild = SimpleNamespace(id=guild_id) if guild_id is not None else None
    return SimpleNamespace(user=SimpleNamespace(id=user_id), guild=guild)


@pytest.mark.asyncio
async def test_command_marks_user_active(bot_module, repo, monkeypatch):
    monkeypatch.setattr(bot_module, "ACTIVITY_TRACKING_ENABLED", True)
    monkeypatch.setattr(bot_module.bot, "player_repo", repo, raising=False)
    repo.add(discord_id=111, discord_username="U", guild_id=TEST_GUILD_ID)
    assert not repo.is_active_for_lottery(111, TEST_GUILD_ID, LOTTERY_DAYS)

    await bot_module._record_command_activity(_interaction(111, TEST_GUILD_ID))

    assert repo.is_active_for_lottery(111, TEST_GUILD_ID, LOTTERY_DAYS)


@pytest.mark.asyncio
async def test_dm_command_records_under_guild_zero(bot_module, repo, monkeypatch):
    monkeypatch.setattr(bot_module, "ACTIVITY_TRACKING_ENABLED", True)
    monkeypatch.setattr(bot_module.bot, "player_repo", repo, raising=False)
    repo.add(discord_id=111, discord_username="U", guild_id=None)

    await bot_module._record_command_activity(_interaction(111, None))

    assert repo.is_active_for_lottery(111, None, LOTTERY_DAYS)


@pytest.mark.asyncio
async def test_disabled_flag_skips_recording(bot_module, repo, monkeypatch):
    monkeypatch.setattr(bot_module, "ACTIVITY_TRACKING_ENABLED", False)
    monkeypatch.setattr(bot_module.bot, "player_repo", repo, raising=False)
    repo.add(discord_id=222, discord_username="U2", guild_id=TEST_GUILD_ID)

    await bot_module._record_command_activity(_interaction(222, TEST_GUILD_ID))

    assert not repo.is_active_for_lottery(222, TEST_GUILD_ID, LOTTERY_DAYS)
