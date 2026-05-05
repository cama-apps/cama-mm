"""Bankruptcy halves the dig cooldown.

The base FREE_DIG_COOLDOWN was lowered to 2h in this patch; bankrupt players
get an additional halving (so ~1h base) before stamina/injury modifiers stack
on top.
"""

import time

import pytest

from repositories.bankruptcy_repository import BankruptcyRepository
from repositories.dig_repository import DigRepository
from services.dig_constants import FREE_DIG_COOLDOWN
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def bankruptcy_repo(repo_db_path):
    return BankruptcyRepository(repo_db_path)


@pytest.fixture
def dig_service_with_bankruptcy(dig_repo, player_repository, bankruptcy_repo, monkeypatch):
    svc = DigService(
        dig_repo, player_repository, bankruptcy_repo=bankruptcy_repo,
    )
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


@pytest.fixture
def dig_service_no_bankruptcy(dig_repo, player_repository, monkeypatch):
    """Without a bankruptcy_repo wired — _is_bankrupt should be a no-op."""
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repository, discord_id=10001, guild_id=12345, balance=100):
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"P{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 3:
        player_repository.update_balance(discord_id, guild_id, balance)


def _build_tunnel_dict(discord_id, guild_id, last_dig_at):
    """Minimal tunnel dict for _get_cooldown_remaining."""
    return {
        "discord_id": discord_id,
        "guild_id": guild_id,
        "last_dig_at": last_dig_at,
        "stamina": 100,
        "injury_state": None,
        "color": None,
    }


class TestBankruptcyCooldown:
    def test_base_cooldown_is_two_hours(self):
        # Sanity check the constant change is in effect.
        assert FREE_DIG_COOLDOWN == 7200

    def test_bankrupt_player_cooldown_halved(
        self, dig_service_with_bankruptcy, bankruptcy_repo, guild_id, monkeypatch,
    ):
        # Mark the player as bankrupt (penalty_games_remaining > 0). Use
        # upsert_state since adjust_penalty_games requires a pre-existing row.
        bankruptcy_repo.upsert_state(
            10001, guild_id,
            last_bankruptcy_at=int(time.time()),
            penalty_games_remaining=5,
        )

        now = 1_000_000
        monkeypatch.setattr(time, "time", lambda: now)
        tunnel = _build_tunnel_dict(10001, guild_id, last_dig_at=now)

        remaining = dig_service_with_bankruptcy._get_cooldown_remaining(tunnel)
        # Halved base is 3600 (1h).
        assert remaining == 3600

    def test_non_bankrupt_player_uses_full_cooldown(
        self, dig_service_with_bankruptcy, guild_id, monkeypatch,
    ):
        now = 1_000_000
        monkeypatch.setattr(time, "time", lambda: now)
        tunnel = _build_tunnel_dict(10001, guild_id, last_dig_at=now)

        remaining = dig_service_with_bankruptcy._get_cooldown_remaining(tunnel)
        assert remaining == FREE_DIG_COOLDOWN

    def test_no_bankruptcy_repo_means_no_halving(
        self, dig_service_no_bankruptcy, guild_id, monkeypatch,
    ):
        now = 1_000_000
        monkeypatch.setattr(time, "time", lambda: now)
        tunnel = _build_tunnel_dict(10001, guild_id, last_dig_at=now)

        remaining = dig_service_no_bankruptcy._get_cooldown_remaining(tunnel)
        assert remaining == FREE_DIG_COOLDOWN

    def test_bankruptcy_halve_survives_injury_override(
        self, dig_service_with_bankruptcy, bankruptcy_repo, guild_id, monkeypatch,
    ):
        """An injured bankrupt player still gets the halving — injury wipes
        the base + mutation bonus, but bankruptcy halves the result LAST so
        the discount survives. Regression guard for the order-of-ops bug."""
        import json as _json

        from services.dig_constants import INJURY_SLOW_COOLDOWN

        bankruptcy_repo.upsert_state(
            10001, guild_id,
            last_bankruptcy_at=int(time.time()),
            penalty_games_remaining=5,
        )
        now = 1_000_000
        monkeypatch.setattr(time, "time", lambda: now)
        tunnel = _build_tunnel_dict(10001, guild_id, last_dig_at=now)
        tunnel["injury_state"] = _json.dumps({"type": "slower_cooldown"})

        remaining = dig_service_with_bankruptcy._get_cooldown_remaining(tunnel)
        # Injury sets cooldown = INJURY_SLOW_COOLDOWN; bankruptcy halves it.
        assert remaining == INJURY_SLOW_COOLDOWN // 2
