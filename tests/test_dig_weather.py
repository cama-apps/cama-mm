"""Layer weather: daily rolls, weather effects on digs, and modifier interactions."""

import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import FREE_DIG_COOLDOWN_SECONDS
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register_player(player_repository, discord_id=10001, guild_id=12345, balance=100):
    """Helper to register a player with balance."""
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 3:
        player_repository.update_balance(discord_id, guild_id, balance)
    return discord_id


@pytest.mark.real_weather
class TestLayerWeather:
    """Tests for the daily layer weather system."""

    def _setup_and_second_dig(self, dig_service, player_repository, monkeypatch):
        """Helper: register, first dig, then second dig to trigger weather."""
        # Restore real weather effects (fixture stubs them out)
        monkeypatch.setattr(dig_service, "_get_weather_effects", DigService._get_weather_effects.__get__(dig_service))
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, 12345)  # first dig (early return, no weather)
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        dig_service.dig(10001, 12345)  # second dig triggers weather

    def test_weather_rolled_on_dig(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Weather should be rolled lazily when a dig reaches the main flow."""
        self._setup_and_second_dig(dig_service, player_repository, monkeypatch)

        today = dig_service._get_game_date()
        weather = dig_repo.get_weather(guild_id, today)
        assert len(weather) == 2, "Should roll exactly 2 weather events"

    def test_weather_targets_populated_layer(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """At least one weather event should target a layer with active players."""
        self._setup_and_second_dig(dig_service, player_repository, monkeypatch)

        today = dig_service._get_game_date()
        weather = dig_repo.get_weather(guild_id, today)
        layers_hit = {w["layer_name"] for w in weather}
        # Dirt should be targeted since the player is in Dirt
        assert "Dirt" in layers_hit

    def test_weather_stable_within_day(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Weather should not re-roll on subsequent digs the same day."""
        self._setup_and_second_dig(dig_service, player_repository, monkeypatch)

        today = dig_service._get_game_date()
        weather1 = dig_repo.get_weather(guild_id, today)

        # Third dig same day
        t = 1_000_000 + 2 * FREE_DIG_COOLDOWN_SECONDS + 2
        monkeypatch.setattr(time, "time", lambda: t)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        weather2 = dig_repo.get_weather(guild_id, today)
        assert weather1 == weather2, "Weather should not change within the same day"

    def test_get_weather_returns_info(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """get_weather() should return displayable weather info."""
        self._setup_and_second_dig(dig_service, player_repository, monkeypatch)

        weather = dig_service.get_weather(guild_id)
        assert len(weather) == 2
        for w in weather:
            assert "name" in w
            assert "description" in w
            assert "layer" in w
            assert "effects" in w

    def test_weather_effects_in_dig_result(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Dig result should include weather info when player is in an affected layer."""
        # Restore real _get_weather_effects (fixture stubs it out)
        monkeypatch.setattr(dig_service, "_get_weather_effects", DigService._get_weather_effects.__get__(dig_service))

        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Force weather on Dirt layer
        today = dig_service._get_game_date()
        dig_repo.set_weather(guild_id, today, "Dirt", "earthworm_migration")
        # Clear the other weather entries to control the test
        dig_repo.set_weather(guild_id, today, "Stone", "mineral_vein")

        dig_repo.update_tunnel(10001, guild_id, depth=5)

        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert result.get("weather") is not None
        assert result["weather"]["name"] == "Earthworm Migration"
