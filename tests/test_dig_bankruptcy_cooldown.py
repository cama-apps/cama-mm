"""The dig cooldown is a flat 1 hour for every player.

The base FREE_DIG_COOLDOWN is 1h and applies uniformly — bankrupt players no
longer get a separate half-off (that discount existed only to bring them to ~1h
off the old 2h base; now everyone is at 1h). Injury / stamina / curse / mana
modifiers still stack on top.
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
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


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


class TestFlatCooldown:
    def test_base_cooldown_is_one_hour(self):
        assert FREE_DIG_COOLDOWN == 3600

    def test_bankrupt_player_uses_flat_cooldown(
        self, dig_service_with_bankruptcy, bankruptcy_repo, guild_id, monkeypatch,
    ):
        """A bankrupt player gets the same flat 1h as everyone else — the old
        half-off discount is gone."""
        bankruptcy_repo.upsert_state(
            10001, guild_id,
            last_bankruptcy_at=int(time.time()),
            penalty_games_remaining=5,
        )
        now = 1_000_000
        monkeypatch.setattr(time, "time", lambda: now)
        tunnel = _build_tunnel_dict(10001, guild_id, last_dig_at=now)

        remaining = dig_service_with_bankruptcy._get_cooldown_remaining(tunnel)
        assert remaining == FREE_DIG_COOLDOWN

    def test_non_bankrupt_player_uses_flat_cooldown(
        self, dig_service_with_bankruptcy, guild_id, monkeypatch,
    ):
        now = 1_000_000
        monkeypatch.setattr(time, "time", lambda: now)
        tunnel = _build_tunnel_dict(10001, guild_id, last_dig_at=now)

        remaining = dig_service_with_bankruptcy._get_cooldown_remaining(tunnel)
        assert remaining == FREE_DIG_COOLDOWN

    def test_injury_override_is_not_halved(
        self, dig_service_with_bankruptcy, bankruptcy_repo, guild_id, monkeypatch,
    ):
        """An injured player (even a bankrupt one) gets the full injury cooldown;
        there is no bankruptcy halving anymore."""
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
        assert remaining == INJURY_SLOW_COOLDOWN


class TestCooldownMalformedInjuryState:
    """A corrupt injury_state JSON string must not crash the cooldown read.

    _get_cooldown_remaining runs on essentially every /dig and status
    check. A malformed/truncated injury_state (partial write, manual DB
    edit, schema drift) must degrade to "no injury", not raise
    JSONDecodeError — the two sibling injury readers already guard this.
    Regression guard for the unguarded json.loads.
    """

    def test_malformed_injury_state_does_not_crash(
        self, dig_service_no_bankruptcy, guild_id, monkeypatch,
    ):
        now = 1_000_000
        monkeypatch.setattr(time, "time", lambda: now)
        tunnel = _build_tunnel_dict(10001, guild_id, last_dig_at=now)
        tunnel["injury_state"] = "{not valid json"

        # Must not raise; malformed state is treated as "no injury".
        remaining = dig_service_no_bankruptcy._get_cooldown_remaining(tunnel)
        assert remaining == FREE_DIG_COOLDOWN
