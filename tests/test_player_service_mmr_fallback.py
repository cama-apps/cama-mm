"""Tests for player registration MMR fallback chain (OpenDota -> current_mmr/4 -> error)."""

import pytest

from services.player_service import PlayerService


class FakeRepo:
    def __init__(self):
        self.add_calls = []

    def get_by_id(self, _discord_id):
        return None

    def add(
        self,
        discord_id: int,
        discord_username: str,
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

    class DummyAPI:
        def get_player_data(self, _steam_id):
            return {"mmr_estimate": {"estimate": 5200}}

        def get_player_mmr(self, _steam_id):
            return None

    monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: DummyAPI())

    result = service.register_player(
        discord_id=1,
        discord_username="user#1",
        steam_id=123,
    )

    assert repo.add_calls, "Repo add should be called"
    added = repo.add_calls[0]
    assert added["initial_mmr"] == 5200
    assert result["mmr"] == 5200


def test_register_player_raises_when_no_mmr_anywhere(monkeypatch):
    repo = FakeRepo()
    service = PlayerService(repo)

    class DummyAPI:
        def get_player_data(self, _steam_id):
            return {"mmr_estimate": {"estimate": None}}

        def get_player_mmr(self, _steam_id):
            return None

    monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: DummyAPI())

    with pytest.raises(ValueError):
        service.register_player(discord_id=2, discord_username="user#2", steam_id=456)

