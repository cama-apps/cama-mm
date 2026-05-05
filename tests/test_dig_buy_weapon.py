"""/dig buy adds weapon/pickaxe upgrades and enforces sequential tiers."""

import pytest

from repositories.dig_repository import DigRepository
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repository, discord_id=10001, guild_id=12345, balance=10_000):
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


class TestUpgradePickaxeToTier:
    def test_buy_next_tier_succeeds(self, dig_service, dig_repo, player_repository, guild_id):
        _register(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(10001, guild_id, depth=30, pickaxe_tier=0)

        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 1)
        assert result["success"]
        assert result["tier"] == 1

    def test_skip_tier_rejected(self, dig_service, dig_repo, player_repository, guild_id):
        _register(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(10001, guild_id, depth=80, pickaxe_tier=0)

        # Try to jump straight to Diamond (tier 3) with Wooden equipped.
        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 3)
        assert not result["success"]
        # Error references the actual next tier name (Stone Pickaxe).
        assert "stone" in result["error"].lower()

    def test_buy_current_or_lower_tier_rejected(self, dig_service, dig_repo, player_repository, guild_id):
        _register(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(10001, guild_id, depth=80, pickaxe_tier=2)

        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 2)
        assert not result["success"]
        assert "already" in result["error"].lower()

    def test_no_tunnel_rejected(self, dig_service, player_repository, guild_id):
        _register(player_repository)
        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 1)
        assert not result["success"]
