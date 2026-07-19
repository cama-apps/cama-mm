"""Tests for SoftAvoidService."""

from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

from repositories.soft_avoid_repository import SoftAvoidRepository
from services.soft_avoid_service import SoftAvoidService
from tests.conftest import TEST_GUILD_ID


def test_create_or_reactivate_avoid_delegates(repo_db_path):
    repo = SoftAvoidRepository(repo_db_path)
    service = SoftAvoidService(repo)

    avoid = service.create_or_reactivate_avoid(
        guild_id=TEST_GUILD_ID,
        avoider_id=100,
        avoided_id=200,
        games=3,
    )
    assert avoid.avoider_discord_id == 100
    assert avoid.avoided_discord_id == 200
    assert avoid.games_remaining == 3


def test_purchase_avoid_delegates():
    repo = MagicMock()
    expected = SimpleNamespace(success=True)
    repo.purchase_avoid.return_value = expected
    service = SoftAvoidService(repo)

    result = service.purchase_avoid(
        guild_id=TEST_GUILD_ID,
        avoider_id=100,
        avoided_id=200,
        cost=250,
        games=10,
    )

    assert result is expected
    repo.purchase_avoid.assert_called_once_with(
        guild_id=TEST_GUILD_ID,
        avoider_id=100,
        avoided_id=200,
        cost=250,
        games=10,
    )


def test_create_or_reactivate_rejects_self_avoid(repo_db_path):
    service = SoftAvoidService(SoftAvoidRepository(repo_db_path))
    with pytest.raises(ValueError, match="same player"):
        service.create_or_reactivate_avoid(TEST_GUILD_ID, 100, 100, games=1)


def test_get_active_avoids_for_players_none_guild_matches_zero(repo_db_path):
    repo = SoftAvoidRepository(repo_db_path)
    service = SoftAvoidService(repo)
    service.create_or_reactivate_avoid(None, 10, 20, games=2)

    via_none = service.get_active_avoids_for_players(None, [10, 20])
    via_zero = service.get_active_avoids_for_players(0, [10, 20])
    assert len(via_none) == 1
    assert via_zero[0].avoider_discord_id == via_none[0].avoider_discord_id
