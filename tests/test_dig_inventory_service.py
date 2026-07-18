"""Item shop / inventory / trap / insurance behavior for the dig minigame.

Covers ``DigInventoryService``: buying, queueing and using consumables,
inventory listing, trap setting (one free per game day), and 24h sabotage
insurance. Tests assert balance/state changes that would break if the
charge-and-mutate paths regressed.
"""

import time

import pytest

import services.dig_inventory_service as inv_module
from repositories.dig_repository import DigRepository
from services.dig_constants import ITEM_PRICES, MAX_INVENTORY_SIZE
from services.dig_inventory_service import DigInventoryService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def inv_service(dig_repo, player_repository):
    # Built the way DigService.__init__ wires it: dig_repo + player_repo.
    return DigInventoryService(dig_repo, player_repository)


def _register_player(player_repository, discord_id=10001, guild_id=12345, balance=100):
    """Register a player and set their JC balance (default starting balance is 3)."""
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


def _fixed_game_date(monkeypatch, date_str):
    """Pin the game date so trap-per-day logic is deterministic."""
    monkeypatch.setattr(inv_module, "_get_game_date", lambda: date_str)


# ─────────────────────────────────────────────────────────────────────────
# buy_item
# ─────────────────────────────────────────────────────────────────────────


class TestBuyItem:
    """Buying debits exactly the item price and adds one inventory row."""

    def test_buy_item_debits_balance_and_adds_inventory(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """A successful buy charges ITEM_PRICES[type] and inserts the item."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")

        result = inv_service.buy_item(10001, guild_id, "dynamite")

        assert result["success"]
        assert result["cost"] == ITEM_PRICES["dynamite"]
        # balance_after must reflect the real debit, not just the input.
        assert result["balance_after"] == 100 - ITEM_PRICES["dynamite"]
        assert player_repository.get_balance(10001, guild_id) == 100 - ITEM_PRICES["dynamite"]
        inventory = dig_repo.get_inventory(10001, guild_id)
        assert len(inventory) == 1
        assert inventory[0]["item_type"] == "dynamite"
        assert inventory[0]["queued"] == 0
        assert result["queued"] is False
        # The returned item_id must point at the row that was actually inserted.
        assert result["item_id"] == inventory[0]["id"]

    @pytest.mark.parametrize(
        ("item_type", "cost"),
        [
            ("tempered_whetstone", 60),
            ("warding_salts", 50),
            ("rescue_line", 40),
        ],
    )
    def test_buy_boss_preparation_item(
        self,
        inv_service,
        dig_repo,
        player_repository,
        guild_id,
        item_type,
        cost,
    ):
        _register_player(player_repository, balance=200)
        dig_repo.create_tunnel(10001, guild_id, "T")

        result = inv_service.buy_item(10001, guild_id, item_type)

        assert result["success"], result
        assert result["cost"] == cost
        assert result["queued"] is False

    def test_buy_hard_hat_auto_queues(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """Hard Hat is active on the next dig without a separate /dig use."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")

        result = inv_service.buy_item(10001, guild_id, "hard_hat")

        assert result["success"]
        assert result["queued"] is True
        queued = dig_repo.get_queued_items(10001, guild_id)
        assert [q["id"] for q in queued] == [result["item_id"]]
        assert queued[0]["item_type"] == "hard_hat"

    def test_buy_torch_auto_queues(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """Torch is active on the next dig without a separate /dig use."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")

        result = inv_service.buy_item(10001, guild_id, "torch")

        assert result["success"]
        assert result["queued"] is True
        queued = dig_repo.get_queued_items(10001, guild_id)
        assert [q["id"] for q in queued] == [result["item_id"]]
        assert queued[0]["item_type"] == "torch"

    def test_buy_duplicate_auto_queue_item_stores_reserve(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """A second Hard Hat is not queued while one is already queued."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")
        first = inv_service.buy_item(10001, guild_id, "hard_hat")
        assert first["queued"] is True

        second = inv_service.buy_item(10001, guild_id, "hard_hat")

        assert second["success"]
        assert second["queued"] is False
        queued = dig_repo.get_queued_items(10001, guild_id)
        assert [q["id"] for q in queued] == [first["item_id"]]
        assert len(dig_repo.get_inventory(10001, guild_id)) == 2

    def test_buy_streak_charm_adds_passive_item(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """Streak Charm is bought through normal inventory storage."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")

        result = inv_service.buy_item(10001, guild_id, "streak_charm")

        assert result["success"]
        assert result["cost"] == ITEM_PRICES["streak_charm"]
        inventory = dig_repo.get_inventory(10001, guild_id)
        assert len(inventory) == 1
        assert inventory[0]["item_type"] == "streak_charm"
        assert result["queued"] is False

    def test_buy_item_unknown_type_rejected(self, inv_service, player_repository, guild_id):
        """An item type not in the price list is rejected before any charge."""
        _register_player(player_repository, balance=100)

        result = inv_service.buy_item(10001, guild_id, "made_up_item")

        assert not result["success"]
        assert "Unknown item type" in result["error"]
        assert player_repository.get_balance(10001, guild_id) == 100

    def test_buy_item_requires_tunnel(self, inv_service, player_repository, guild_id):
        """Buying without a tunnel fails and does not debit the player."""
        _register_player(player_repository, balance=100)

        result = inv_service.buy_item(10001, guild_id, "dynamite")

        assert not result["success"]
        assert "tunnel" in result["error"].lower()
        assert player_repository.get_balance(10001, guild_id) == 100

    def test_buy_item_insufficient_balance_rejected(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """Too little JC blocks the purchase and leaves balance untouched."""
        # void_bait costs 20; give the player 19.
        _register_player(player_repository, balance=19)
        dig_repo.create_tunnel(10001, guild_id, "T")

        result = inv_service.buy_item(10001, guild_id, "void_bait")

        assert not result["success"]
        assert "19 JC" in result["error"]
        assert player_repository.get_balance(10001, guild_id) == 19
        assert dig_repo.get_inventory(10001, guild_id) == []

    def test_buy_item_exact_balance_succeeds(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """balance == price is enough (the check is balance < price)."""
        _register_player(player_repository, balance=ITEM_PRICES["lantern"])
        dig_repo.create_tunnel(10001, guild_id, "T")

        result = inv_service.buy_item(10001, guild_id, "lantern")

        assert result["success"]
        assert player_repository.get_balance(10001, guild_id) == 0

    def test_buy_item_inventory_full_rejected(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """A full inventory blocks new purchases without charging."""
        _register_player(player_repository, balance=500)
        dig_repo.create_tunnel(10001, guild_id, "T")
        for _ in range(MAX_INVENTORY_SIZE):
            dig_repo.add_item(10001, guild_id, "torch")
        balance_before = player_repository.get_balance(10001, guild_id)

        result = inv_service.buy_item(10001, guild_id, "dynamite")

        assert not result["success"]
        assert "full" in result["error"].lower()
        assert player_repository.get_balance(10001, guild_id) == balance_before
        assert len(dig_repo.get_inventory(10001, guild_id)) == MAX_INVENTORY_SIZE


# ─────────────────────────────────────────────────────────────────────────
# use_item / queue_item
# ─────────────────────────────────────────────────────────────────────────


class TestUseItem:
    """use_item queues an owned, not-yet-queued consumable for the next dig."""

    def test_use_item_queues_owned_item(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """Using an owned item flips its queued flag in the inventory table."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        item_id = dig_repo.add_item(10001, guild_id, "dynamite")

        result = inv_service.use_item(10001, guild_id, "dynamite")

        assert result["success"]
        assert result["queued"] is True
        queued = dig_repo.get_queued_items(10001, guild_id)
        assert [q["id"] for q in queued] == [item_id]

    def test_use_item_unknown_type_rejected(self, inv_service, player_repository, guild_id):
        """A non-consumable type is rejected up front."""
        _register_player(player_repository)

        result = inv_service.use_item(10001, guild_id, "not_a_consumable")

        assert not result["success"]
        assert "Unknown item type" in result["error"]

    def test_use_streak_charm_rejected_as_passive(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """Streak Charm cannot be queued because it triggers automatically."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.add_item(10001, guild_id, "streak_charm")

        result = inv_service.use_item(10001, guild_id, "streak_charm")

        assert not result["success"]
        assert "passive" in result["error"]
        assert dig_repo.get_queued_items(10001, guild_id) == []

    def test_use_item_requires_tunnel(self, inv_service, player_repository, guild_id):
        """No tunnel means nothing to dig with — use_item fails."""
        _register_player(player_repository)

        result = inv_service.use_item(10001, guild_id, "dynamite")

        assert not result["success"]
        assert "tunnel" in result["error"].lower()

    def test_use_item_not_owned_rejected(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """Queueing an item the player doesn't own fails."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        # Inventory holds a torch, not the dynamite the player asks to use.
        dig_repo.add_item(10001, guild_id, "torch")

        result = inv_service.use_item(10001, guild_id, "dynamite")

        assert not result["success"]
        assert "don't have" in result["error"]

    def test_use_item_already_queued_rejected(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """The same item type cannot be queued twice."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.add_item(10001, guild_id, "dynamite")
        first = inv_service.use_item(10001, guild_id, "dynamite")
        assert first["success"]

        second = inv_service.use_item(10001, guild_id, "dynamite")

        assert not second["success"]
        assert "already queued" in second["error"]

    def test_use_item_queues_only_one_of_a_duplicate_pair(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """With two copies owned, use_item queues exactly one (not both)."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.add_item(10001, guild_id, "dynamite")
        dig_repo.add_item(10001, guild_id, "dynamite")

        result = inv_service.use_item(10001, guild_id, "dynamite")

        assert result["success"]
        # Only one of the two duplicate rows should be queued.
        assert len(dig_repo.get_queued_items(10001, guild_id)) == 1

    def test_only_one_boss_preparation_item_can_be_queued(
        self,
        inv_service,
        dig_repo,
        player_repository,
        guild_id,
    ):
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.add_item(10001, guild_id, "tempered_whetstone")
        dig_repo.add_item(10001, guild_id, "rescue_line")
        assert inv_service.use_item(
            10001,
            guild_id,
            "tempered_whetstone",
        )["success"]

        result = inv_service.use_item(10001, guild_id, "rescue_line")

        assert not result["success"]
        assert "one boss preparation" in result["error"].lower()

    def test_queue_item_by_id_sets_flag(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """queue_item(item_id) directly flips the queued flag for that row."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        item_id = dig_repo.add_item(10001, guild_id, "torch")

        result = inv_service.queue_item(10001, guild_id, item_id)

        assert result["success"]
        assert [q["id"] for q in dig_repo.get_queued_items(10001, guild_id)] == [item_id]


# ─────────────────────────────────────────────────────────────────────────
# get_inventory
# ─────────────────────────────────────────────────────────────────────────


class TestGetInventory:
    """get_inventory shapes rows for the embed layer with queued status."""

    def test_get_inventory_empty_without_tunnel(self, inv_service, player_repository, guild_id):
        """No tunnel yields an empty list rather than an error."""
        _register_player(player_repository)
        assert inv_service.get_inventory(10001, guild_id) == []

    def test_get_inventory_empty_with_tunnel_no_items(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """A tunnel with no items still yields an empty list."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        assert inv_service.get_inventory(10001, guild_id) == []

    def test_get_inventory_marks_queued_item(
        self, inv_service, dig_repo, player_repository, guild_id
    ):
        """A queued item shows queued=True; an un-queued one shows False."""
        _register_player(player_repository)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.add_item(10001, guild_id, "dynamite")
        dig_repo.add_item(10001, guild_id, "torch")
        inv_service.use_item(10001, guild_id, "dynamite")

        listing = inv_service.get_inventory(10001, guild_id)

        by_type = {row["type"]: row for row in listing}
        assert by_type["dynamite"]["queued"] is True
        assert by_type["torch"]["queued"] is False
        # Display fields are populated from CONSUMABLE_ITEMS.
        assert by_type["dynamite"]["name"] == "Dynamite"
        assert by_type["dynamite"]["description"]


# ─────────────────────────────────────────────────────────────────────────
# set_trap
# ─────────────────────────────────────────────────────────────────────────


class TestSetTrap:
    """First trap of the game day is free; the next one is paid."""

    def test_first_trap_of_day_is_free(
        self, inv_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """A fresh tunnel's first trap costs 0 and leaves balance untouched."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")
        _fixed_game_date(monkeypatch, "2026-05-16")

        result = inv_service.set_trap(10001, guild_id)

        assert result["success"]
        assert result["cost"] == 0
        assert player_repository.get_balance(10001, guild_id) == 100
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["trap_active"] == 1
        assert tunnel["trap_date"] == "2026-05-16"

    def test_second_trap_same_day_is_paid(
        self, inv_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """After the free trap is consumed and cleared, the next one is charged."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")
        _fixed_game_date(monkeypatch, "2026-05-16")
        inv_service.set_trap(10001, guild_id)
        # Clear the active trap so a second one can be set the same day.
        dig_repo.update_tunnel(10001, guild_id, trap_active=0)

        result = inv_service.set_trap(10001, guild_id)

        assert result["success"]
        # depth 0 -> cost = 5 + 0 // 25 = 5
        assert result["cost"] == 5
        assert player_repository.get_balance(10001, guild_id) == 95

    def test_paid_trap_scales_with_depth(
        self, inv_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Paid trap cost rises with depth: 5 + depth // 25."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(10001, guild_id, depth=60)
        _fixed_game_date(monkeypatch, "2026-05-16")
        inv_service.set_trap(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, trap_active=0)

        result = inv_service.set_trap(10001, guild_id)

        # depth 60 -> 5 + 60 // 25 = 5 + 2 = 7
        assert result["cost"] == 7
        assert player_repository.get_balance(10001, guild_id) == 93

    def test_new_day_resets_free_trap(
        self, inv_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Crossing into a new game date makes the next trap free again."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")
        _fixed_game_date(monkeypatch, "2026-05-16")
        inv_service.set_trap(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, trap_active=0)

        # Next game day: trap_date no longer matches -> free trap resets.
        _fixed_game_date(monkeypatch, "2026-05-17")
        result = inv_service.set_trap(10001, guild_id)

        assert result["success"]
        assert result["cost"] == 0
        assert player_repository.get_balance(10001, guild_id) == 100

    def test_set_trap_rejected_when_trap_already_active(
        self, inv_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """An already-active trap blocks setting another."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")
        _fixed_game_date(monkeypatch, "2026-05-16")
        inv_service.set_trap(10001, guild_id)

        result = inv_service.set_trap(10001, guild_id)

        assert not result["success"]
        assert "active trap" in result["error"]

    def test_set_trap_requires_tunnel(self, inv_service, player_repository, guild_id):
        """No tunnel means no trap."""
        _register_player(player_repository)

        result = inv_service.set_trap(10001, guild_id)

        assert not result["success"]
        assert "tunnel" in result["error"].lower()

    def test_paid_trap_insufficient_balance_rejected(
        self, inv_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Too little JC for a paid trap fails without changing balance."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")
        _fixed_game_date(monkeypatch, "2026-05-16")
        inv_service.set_trap(10001, guild_id)  # free one
        dig_repo.update_tunnel(10001, guild_id, trap_active=0)
        # Drain balance below the 5 JC paid-trap cost.
        player_repository.update_balance(10001, guild_id, 4)

        result = inv_service.set_trap(10001, guild_id)

        assert not result["success"]
        assert "4 JC" in result["error"]
        assert player_repository.get_balance(10001, guild_id) == 4
        # Trap state must not have flipped on a failed paid attempt.
        assert dig_repo.get_tunnel(10001, guild_id)["trap_active"] == 0


# ─────────────────────────────────────────────────────────────────────────
# buy_insurance
# ─────────────────────────────────────────────────────────────────────────


class TestBuyInsurance:
    """Insurance charges 5 + depth // 25 and sets a 24h protection window."""

    def test_buy_insurance_charges_and_sets_window(
        self, inv_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """A successful buy debits the cost and sets insured_until = now + 24h."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")
        monkeypatch.setattr(time, "time", lambda: 1_000_000)

        result = inv_service.buy_insurance(10001, guild_id)

        assert result["success"]
        # depth 0 -> 5 + 0 // 25 = 5
        assert result["cost"] == 5
        assert result["expires_at"] == 1_000_000 + 86400
        assert player_repository.get_balance(10001, guild_id) == 95
        assert dig_repo.get_tunnel(10001, guild_id)["insured_until"] == 1_000_000 + 86400

    def test_buy_insurance_cost_scales_with_depth(
        self, inv_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Insurance cost grows with depth: 5 + depth // 25."""
        _register_player(player_repository, balance=100)
        dig_repo.create_tunnel(10001, guild_id, "T")
        dig_repo.update_tunnel(10001, guild_id, depth=75)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)

        result = inv_service.buy_insurance(10001, guild_id)

        # depth 75 -> 5 + 75 // 25 = 5 + 3 = 8
        assert result["cost"] == 8
        assert player_repository.get_balance(10001, guild_id) == 92

    def test_buy_insurance_requires_tunnel(self, inv_service, player_repository, guild_id):
        """No tunnel means no insurance to buy."""
        _register_player(player_repository, balance=100)

        result = inv_service.buy_insurance(10001, guild_id)

        assert not result["success"]
        assert "tunnel" in result["error"].lower()
        assert player_repository.get_balance(10001, guild_id) == 100

    def test_buy_insurance_insufficient_balance_rejected(
        self, inv_service, dig_repo, player_repository, guild_id, monkeypatch
    ):
        """Too little JC blocks the purchase and leaves the tunnel uninsured."""
        _register_player(player_repository, balance=4)
        dig_repo.create_tunnel(10001, guild_id, "T")
        monkeypatch.setattr(time, "time", lambda: 1_000_000)

        result = inv_service.buy_insurance(10001, guild_id)

        assert not result["success"]
        assert "4 JC" in result["error"]
        assert player_repository.get_balance(10001, guild_id) == 4
        assert dig_repo.get_tunnel(10001, guild_id)["insured_until"] is None
