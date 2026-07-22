"""Inventory and defense actions for the dig minigame.

Covers:
- Consumable item buy / queue / use / list
- Trap setting (one free per game day, paid thereafter)
- 24h sabotage insurance

All operations either read tunnel rows or run through
``dig_repo.atomic_tunnel_balance_update`` to keep balance + tunnel state
in lockstep. Pure delegation target — no orchestration helpers needed.
"""

from __future__ import annotations

import time
from typing import TYPE_CHECKING

from services.dig_constants import (
    BOSS_PREP_ITEM_IDS,
    CONSUMABLE_ITEMS,
    ITEM_PRICES,
    MAX_INVENTORY_SIZE,
)

if TYPE_CHECKING:
    from repositories.dig_repository import DigRepository
    from repositories.player_repository import PlayerRepository


def _ok(**kwargs) -> dict:
    """Return a standard success result. Mirrors DigService._ok."""
    result = {"success": True, "error": None}
    result.update(kwargs)
    if "depth_after" in result and "depth" not in result:
        result["depth"] = result["depth_after"]
    return result


def _error(msg: str) -> dict:
    """Return a standard error result."""
    return {"success": False, "error": msg}


def _get_game_date() -> str:
    """Get current game date (resets at 4 AM PST). Imported lazily so tests can mock."""
    from utils.game_date import get_game_date
    return get_game_date()


AUTO_QUEUE_ON_BUY = frozenset({"hard_hat", "torch"})


def _get_queued_items_for_tunnel(
    dig_repo: DigRepository, discord_id: int, guild_id
) -> list[dict]:
    """Get items queued for next dig from inventory table."""
    items = dig_repo.get_queued_items(discord_id, guild_id)
    return [{"type": i.get("item_type"), "id": i.get("id")} for i in items]


class DigInventoryService:
    """Item shop / inventory / trap / insurance actions."""

    def __init__(
        self,
        dig_repo: DigRepository,
        player_repo: PlayerRepository,
    ) -> None:
        self.dig_repo = dig_repo
        self.player_repo = player_repo

    # ------------------------------------------------------------------
    # Items
    # ------------------------------------------------------------------

    def use_item(self, discord_id: int, guild_id, item_type: str) -> dict:
        """Queue an item for next dig."""
        if item_type not in CONSUMABLE_ITEMS:
            return _error(f"Unknown item type: {item_type}")
        if item_type == "streak_charm":
            return _error("Streak Charm is passive and triggers automatically.")

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return _error("You don't have a tunnel.")

        tunnel = dict(tunnel)

        # Check inventory
        inventory = self.dig_repo.get_inventory(discord_id, guild_id)
        has_item = any(i.get("item_type") == item_type for i in inventory)
        if not has_item:
            return _error(f"You don't have a {CONSUMABLE_ITEMS[item_type]['name']}.")

        # Check not already queued
        queued = _get_queued_items_for_tunnel(self.dig_repo, discord_id, guild_id)
        if (
            item_type in BOSS_PREP_ITEM_IDS
            and any(q.get("type") in BOSS_PREP_ITEM_IDS for q in queued)
        ):
            return _error("Only one boss preparation item can be queued at a time.")
        if any(q.get("type") == item_type for q in queued):
            return _error(f"{CONSUMABLE_ITEMS[item_type]['name']} is already queued.")

        # Find the first non-queued item of this type and queue it
        for inv_item in inventory:
            if inv_item.get("item_type") == item_type and not inv_item.get("queued"):
                self.dig_repo.queue_item(inv_item["id"])
                break

        return _ok(
            item=CONSUMABLE_ITEMS[item_type]["name"],
            queued=True,
        )

    def queue_item(self, discord_id: int, guild_id, item_id: int) -> dict:
        """Queue a specific inventory item by its database id."""
        inventory = self.dig_repo.get_inventory(discord_id, guild_id)
        target = next(
            (row for row in inventory if int(row.get("id", 0)) == int(item_id)),
            None,
        )
        if target is None:
            return _error("That item is not in your inventory.")
        item_type = target.get("item_type")
        queued = _get_queued_items_for_tunnel(
            self.dig_repo,
            discord_id,
            guild_id,
        )
        if (
            item_type in BOSS_PREP_ITEM_IDS
            and any(q.get("type") in BOSS_PREP_ITEM_IDS for q in queued)
        ):
            return _error("Only one boss preparation item can be queued at a time.")
        # Duplicate-type guard (mirrors use_item): a second queued copy of the
        # same type would be consumed with zero extra effect.
        if any(q.get("type") == item_type for q in queued):
            item_name = CONSUMABLE_ITEMS.get(item_type, {}).get("name", "That item")
            return _error(f"{item_name} is already queued.")
        self.dig_repo.queue_item(item_id)
        return _ok(queued=True)

    def buy_item(self, discord_id: int, guild_id, item_type: str) -> dict:
        """Buy an item from the shop."""
        if item_type not in ITEM_PRICES:
            return _error(f"Unknown item type: {item_type}")

        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return _error("You don't have a tunnel. Dig first!")

        # Check inventory capacity
        inventory = self.dig_repo.get_inventory(discord_id, guild_id)
        if len(inventory) >= MAX_INVENTORY_SIZE:
            return _error(f"Inventory full ({MAX_INVENTORY_SIZE} items max).")

        queued = _get_queued_items_for_tunnel(self.dig_repo, discord_id, guild_id)
        auto_queue = (
            item_type in AUTO_QUEUE_ON_BUY
            and not any(q.get("type") == item_type for q in queued)
        )

        price = ITEM_PRICES[item_type]
        balance = self.player_repo.get_balance(discord_id, guild_id)
        if balance < price:
            return _error(f"Costs {price} JC but you only have {balance} JC.")

        # Debit + inventory insert commit together so a crash can't leave
        # the player charged with no item added to inventory.
        item_id = self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=-price,
            add_inventory_item=item_type,
        )
        if auto_queue and item_id is not None:
            self.dig_repo.queue_item(item_id)

        item_name = CONSUMABLE_ITEMS.get(item_type, {}).get("name", item_type)

        return _ok(
            item=item_name,
            item_id=item_id,
            queued=auto_queue,
            cost=price,
            balance_after=balance - price,
        )

    def ensure_auto_buy_items(
        self, discord_id: int, guild_id, item_types: list[str] | tuple[str, ...]
    ) -> list[dict]:
        """Ensure selected auto-buy items are queued for the imminent dig.

        Existing reserve inventory is queued first. Only if no reserve exists do
        we buy a new copy. Failures are reported but do not block the dig.
        """
        selected_types = [
            item_type for item_type in item_types if item_type in AUTO_QUEUE_ON_BUY
        ]
        if not selected_types:
            return []

        queued = _get_queued_items_for_tunnel(
            self.dig_repo, discord_id, guild_id
        )
        queued_types = {item.get("type") for item in queued}
        inventory = self.dig_repo.get_inventory(discord_id, guild_id)
        inventory_size = len(inventory)
        balance: int | None = None
        tunnel_checked = False
        tunnel_exists = False

        results: list[dict] = []
        queue_item_ids: list[int] = []
        purchases: list[tuple[str, int]] = []
        purchase_result_indexes: list[int] = []
        for item_type in selected_types:
            item_name = CONSUMABLE_ITEMS[item_type]["name"]
            if item_type in queued_types:
                results.append({
                    "type": item_type,
                    "item": item_name,
                    "status": "already_queued",
                    "cost": 0,
                })
                continue

            reserve = next(
                (
                    item for item in inventory
                    if item.get("item_type") == item_type and not item.get("queued")
                ),
                None,
            )
            if reserve is not None:
                queue_item_ids.append(reserve["id"])
                reserve["queued"] = 1
                queued_types.add(item_type)
                results.append({
                    "type": item_type,
                    "item": item_name,
                    "status": "queued_from_inventory",
                    "cost": 0,
                })
                continue

            if inventory_size >= MAX_INVENTORY_SIZE:
                results.append({
                    "type": item_type,
                    "item": item_name,
                    "status": "skipped_inventory_full",
                    "cost": 0,
                })
                continue

            price = ITEM_PRICES[item_type]
            if balance is None:
                balance = self.player_repo.get_balance(discord_id, guild_id)
            if balance < price:
                results.append({
                    "type": item_type,
                    "item": item_name,
                    "status": "skipped_insufficient_balance",
                    "cost": price,
                })
                continue

            if not tunnel_checked:
                tunnel_exists = (
                    self.dig_repo.get_tunnel(discord_id, guild_id) is not None
                )
                tunnel_checked = True
            if not tunnel_exists:
                results.append({
                    "type": item_type,
                    "item": item_name,
                    "status": "skipped_error",
                    "cost": price,
                    "error": "You don't have a tunnel. Dig first!",
                })
                continue

            purchase_result_indexes.append(len(results))
            purchases.append((item_type, price))
            results.append({
                "type": item_type,
                "item": item_name,
                "status": "purchased",
                "cost": price,
            })
            balance -= price
            inventory_size += 1
            queued_types.add(item_type)

        purchased_ids = self.dig_repo.atomic_auto_buy_items(
            discord_id,
            guild_id,
            queue_item_ids=queue_item_ids,
            purchases=purchases,
        )
        for result_index, item_id in zip(
            purchase_result_indexes, purchased_ids, strict=True
        ):
            results[result_index]["item_id"] = item_id

        return results

    def get_inventory(self, discord_id: int, guild_id) -> list[dict]:
        """Return inventory items with names and queued status."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return []

        tunnel = dict(tunnel)
        items = self.dig_repo.get_inventory(discord_id, guild_id)
        queued = _get_queued_items_for_tunnel(self.dig_repo, discord_id, guild_id)
        queued_types = {q.get("type") for q in queued}

        result = []
        for item in items:
            itype = item.get("item_type", "unknown")
            info = CONSUMABLE_ITEMS.get(itype, {})
            result.append({
                "type": itype,
                "name": info.get("name", itype),
                "description": info.get("description", ""),
                "queued": itype in queued_types,
            })

        return result

    # ------------------------------------------------------------------
    # Defense
    # ------------------------------------------------------------------

    def set_trap(self, discord_id: int, guild_id) -> dict:
        """Set a trap on your tunnel."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return _error("You don't have a tunnel.")

        tunnel = dict(tunnel)

        if tunnel.get("trap_active"):
            return _error("You already have an active trap.")

        today = _get_game_date()
        trap_date = tunnel.get("trap_date")
        trap_free_today = tunnel.get("trap_free_today", 0) or 0

        cost = 0
        if trap_date != today:
            # Reset free trap for new day
            trap_free_today = 0

        if trap_free_today > 0:
            # Already used free trap today — pay
            cost = 5 + (tunnel.get("depth", 0) // 25)
            balance = self.player_repo.get_balance(discord_id, guild_id)
            if balance < cost:
                return _error(f"Trap costs {cost} JC but you only have {balance} JC.")

        # Debit (if any) + trap fields commit together.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=-cost if cost else 0,
            tunnel_updates={
                "trap_active": 1,
                "trap_free_today": trap_free_today + 1,
                "trap_date": today,
            },
        )

        return _ok(cost=cost, message="Trap set!")

    def buy_insurance(self, discord_id: int, guild_id) -> dict:
        """Buy 24h sabotage insurance."""
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return _error("You don't have a tunnel.")

        depth = tunnel["depth"] if tunnel else 0
        cost = 5 + depth // 25
        balance = self.player_repo.get_balance(discord_id, guild_id)
        if balance < cost:
            return _error(f"Insurance costs {cost} JC but you only have {balance} JC.")

        now = int(time.time())
        # Debit + insurance window set together: the old two-step flow could
        # leave the player charged with no insurance applied.
        self.dig_repo.atomic_tunnel_balance_update(
            discord_id, guild_id,
            balance_delta=-cost,
            tunnel_updates={"insured_until": now + 86400},  # 24h
        )

        return _ok(cost=cost, expires_at=now + 86400)
