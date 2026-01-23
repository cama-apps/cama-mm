"""Tests for player registration MMR override fallback."""

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
            }
        )


def test_register_player_uses_override(monkeypatch):
    repo = FakeRepo()
    service = PlayerService(repo)

    # Monkeypatch OpenDotaAPI to avoid network calls and force ValueError if used
    class DummyAPI:
        def get_player_data(self, _steam_id):
            raise AssertionError("Should not call OpenDota when mmr_override is provided")

    monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: DummyAPI())

    result = service.register_player(
        discord_id=123,
        discord_username="user#123",
        steam_id=42,
        mmr_override=4000,
    )

    assert repo.add_calls, "Repo add should be called"
    added = repo.add_calls[0]
    assert added["initial_mmr"] == 4000
    assert result["mmr"] == 4000


def test_register_player_requires_mmr_if_no_override(monkeypatch):
    repo = FakeRepo()
    service = PlayerService(repo)

    class DummyAPI:
        def get_player_data(self, _steam_id):
            return {}

        def get_player_mmr(self, _steam_id):
            return None

    monkeypatch.setattr("services.player_service.OpenDotaAPI", lambda: DummyAPI())

    with pytest.raises(ValueError):
        service.register_player(discord_id=1, discord_username="u", steam_id=2)

