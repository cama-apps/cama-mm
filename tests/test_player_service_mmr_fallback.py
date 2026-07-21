"""Tests for player registration's OpenDota MMR fallback chain."""

import pytest

from opendota_integration import OpenDotaAPI
from services.player_service import PlayerService
from tests.conftest import TEST_GUILD_ID


class FakeRepo:
    def __init__(self):
        self.add_calls = []

    def get_by_id(self, _discord_id, _guild_id):
        return None

    def get_steam_id_owner(self, _steam_id):
        return None

    def add_steam_id(self, discord_id, steam_id, is_primary=False):
        pass

    def add(
        self,
        discord_id: int,
        discord_username: str,
        guild_id: int,
        dotabuff_url: str | None = None,
        steam_id: int | None = None,
        initial_mmr: int | None = None,
        preferred_roles=None,
        main_role=None,
        glicko_rating=None,
        glicko_rd=None,
        glicko_volatility=None,
        os_mu=None,
        os_sigma=None,
    ):
        self.add_calls.append(
            {
                "discord_id": discord_id,
                "discord_username": discord_username,
                "guild_id": guild_id,
                "dotabuff_url": dotabuff_url,
                "steam_id": steam_id,
                "initial_mmr": initial_mmr,
                "glicko_rating": glicko_rating,
                "glicko_rd": glicko_rd,
                "glicko_volatility": glicko_volatility,
                "os_mu": os_mu,
                "os_sigma": os_sigma,
            }
        )


def test_register_player_fallback_to_current_mmr(monkeypatch):
    repo = FakeRepo()
    service = PlayerService(repo)
    api_calls = {"player_data": 0, "mmr_from_data": 0}

    class DummyAPI:
        def get_player_data(self, _steam_id):
            api_calls["player_data"] += 1
            return {"mmr_estimate": {"estimate": 5200}}

        def get_player_mmr_from_data(self, _player_data):
            api_calls["mmr_from_data"] += 1
            return None

        def get_player_mmr(self, _steam_id):
            raise AssertionError("registration must not refetch the player payload")

        def get_player_counts(self, _steam_id):
            return None

    monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: DummyAPI())

    result = service.register_player(
        discord_id=1,
        discord_username="user#1",
        guild_id=TEST_GUILD_ID,
        steam_id=123,
    )

    assert repo.add_calls, "Repo add should be called"
    added = repo.add_calls[0]
    assert added["initial_mmr"] == 5200
    assert result["mmr"] == 5200
    assert api_calls == {"player_data": 1, "mmr_from_data": 1}


def test_register_player_raises_when_no_mmr_anywhere(monkeypatch):
    repo = FakeRepo()
    service = PlayerService(repo)

    class DummyAPI:
        def get_player_data(self, _steam_id):
            return {"mmr_estimate": {"estimate": None}}

        def get_player_mmr_from_data(self, _player_data):
            return None

    monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: DummyAPI())

    with pytest.raises(ValueError):
        service.register_player(discord_id=2, discord_username="user#2", guild_id=TEST_GUILD_ID, steam_id=456)


@pytest.mark.parametrize(
    ("player_data", "expected_mmr"),
    [
        (
            {
                "mmr_estimate": {"estimate": "5200"},
                "computed_mmr": 4800,
                "solo_competitive_rank": 4200,
            },
            5200,
        ),
        ({"computed_mmr": "4800"}, 4800),
        ({"rank_tier": 80, "leaderboard_rank": 500, "computed_mmr": 4000}, 6500),
        ({"solo_competitive_rank": "4200"}, 4200),
        ({"rank_tier": 80, "leaderboard_rank": None}, 5500),
        ({}, None),
        (None, None),
    ],
)
def test_get_player_mmr_from_data_preserves_fallback_chain(player_data, expected_mmr):
    api = object.__new__(OpenDotaAPI)

    assert api.get_player_mmr_from_data(player_data) == expected_mmr
