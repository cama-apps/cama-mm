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


from services.dig_data.items import _PICKAXE_TIERS_DEF

# Stone is tier 1: the first paid upgrade from the Wooden starter.
STONE_TIER = _PICKAXE_TIERS_DEF[1]
STONE_COST = STONE_TIER.jc_cost
STONE_DEPTH = STONE_TIER.depth_required


class TestUpgradePickaxeToTier:
    def test_buy_next_tier_succeeds(self, dig_service, dig_repo, player_repository, guild_id):
        _register(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(10001, guild_id, depth=30, pickaxe_tier=0)

        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 1)
        assert result["success"]
        assert result["tier"] == 1

    def test_buy_next_tier_debits_exact_cost_and_grants_tier(
        self, dig_service, dig_repo, player_repository, guild_id
    ):
        """A successful upgrade debits exactly the tier's JC cost and equips the
        new pickaxe tier in both the tunnel column and the equipped weapon."""
        start_balance = 10_000
        _register(player_repository, balance=start_balance)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(10001, guild_id, depth=STONE_DEPTH, pickaxe_tier=0)
        assert STONE_COST > 0, "this test only proves the debit if the tier costs JC"

        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 1)
        assert result["success"]
        assert result["tier"] == 1
        assert result["cost"] == STONE_COST

        # Balance debited by exactly the upgrade cost — nothing more, nothing less.
        assert (
            player_repository.get_balance(10001, guild_id) == start_balance - STONE_COST
        )
        # Tier 1 is actually granted: both the legacy tunnel column and the
        # equipped weapon row reflect Stone.
        assert dig_repo.get_tunnel(10001, guild_id)["pickaxe_tier"] == 1
        assert dig_repo.get_equipped_gear(10001, guild_id)["weapon"]["tier"] == 1

    def test_buy_next_tier_insufficient_funds_rejected_debits_nothing(
        self, dig_service, dig_repo, player_repository, guild_id
    ):
        """An upgrade the player can't afford is rejected and debits nothing,
        leaving the pickaxe tier unchanged."""
        poor_balance = STONE_COST - 1
        _register(player_repository, balance=poor_balance)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(10001, guild_id, depth=STONE_DEPTH, pickaxe_tier=0)

        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 1)
        assert not result["success"]
        # Balance untouched and the pickaxe tier is NOT advanced.
        assert player_repository.get_balance(10001, guild_id) == poor_balance
        assert dig_repo.get_tunnel(10001, guild_id)["pickaxe_tier"] == 0

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
        starter_weapon = dig_repo.get_equipped_gear(10001, guild_id)["weapon"]["id"]
        dig_repo.unequip_gear(starter_weapon)
        tier_two_weapon = dig_repo.add_gear(10001, guild_id, "weapon", 2)
        dig_repo.equip_gear(tier_two_weapon, 10001, guild_id, "weapon")

        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 2)
        assert not result["success"]
        assert "already" in result["error"].lower()

    def test_broken_high_tier_weapon_cannot_be_replaced_by_lower_tier(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        _register(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(
            10001, guild_id, depth=300, prestige_level=5, pickaxe_tier=7,
        )
        starter_id = dig_repo.get_equipped_gear(10001, guild_id)["weapon"]["id"]
        dig_repo.unequip_gear(starter_id)
        broken_id = dig_repo.add_gear(
            10001, guild_id, "weapon", 7, durability=0,
        )
        dig_repo.equip_gear(broken_id, 10001, guild_id, "weapon")
        balance_before = player_repository.get_balance(10001, guild_id)

        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 1)

        assert not result["success"]
        assert "already" in result["error"].lower()
        assert player_repository.get_balance(10001, guild_id) == balance_before
        assert dig_repo.get_equipped_gear(10001, guild_id)["weapon"]["id"] == broken_id

    def test_shop_does_not_reoffer_pickaxe_tiers_below_broken_weapon(
        self, dig_service, dig_repo, player_repository, guild_id,
    ):
        _register(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "TestTunnel")
        dig_repo.update_tunnel(
            10001, guild_id, depth=300, prestige_level=5, pickaxe_tier=7,
        )
        starter_id = dig_repo.get_equipped_gear(10001, guild_id)["weapon"]["id"]
        dig_repo.unequip_gear(starter_id)
        broken_id = dig_repo.add_gear(
            10001, guild_id, "weapon", 7, durability=0,
        )
        dig_repo.equip_gear(broken_id, 10001, guild_id, "weapon")

        shop = dig_service.get_shop(10001, guild_id)

        assert shop["pickaxe_upgrades"] == []

    def test_no_tunnel_rejected(self, dig_service, player_repository, guild_id):
        _register(player_repository)
        result = dig_service.upgrade_pickaxe_to_tier(10001, guild_id, 1)
        assert not result["success"]
