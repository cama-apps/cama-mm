"""Views and embed builder for the ``/dig gear`` loadout panel."""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING

import discord

from commands.dig_helpers._shared import _wrap
from services.dig_constants import format_relic_label
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer

if TYPE_CHECKING:
    from services.dig_service import DigService

logger = logging.getLogger("cama_bot.commands.dig")


def _build_gear_embed(
    loadout: dict,
    inventory: list[dict],
    damaged: list[dict],
    dig_service: DigService | None = None,
) -> discord.Embed:
    """Build the /dig gear embed: equipped slots + summary footer.

    When ``dig_service`` is provided, damaged equipped pieces also show
    their repair cost so the player can decide before clicking Repair.
    """
    embed = discord.Embed(title="Your Loadout", color=0x8B4513)

    def _slot_value(slot_name: str) -> str:
        piece = loadout.get(slot_name)
        if piece is None:
            return "_— Empty —_"
        line = f"**{piece['name']}** ({piece['durability']}/{piece['max_durability']})"
        if piece["durability"] <= 0:
            line += "\n**BROKEN — effects disabled until repaired.**"
        if piece.get("effect"):
            line += f"\n{piece['effect']}"
        if (
            dig_service is not None
            and piece["durability"] < piece["max_durability"]
        ):
            cost = dig_service.compute_repair_cost(
                piece["slot"], piece["tier"], piece.get("item_id"),
                piece["durability"], piece["max_durability"],
            )
            if cost > 0:
                line += f"\nRepair: {cost} {JOPACOIN_EMOTE}"
        return line

    embed.add_field(name="Weapon", value=_slot_value("weapon"), inline=False)
    embed.add_field(name="Armor",  value=_slot_value("armor"),  inline=False)
    embed.add_field(name="Boots",  value=_slot_value("boots"),  inline=False)
    embed.add_field(name="Amulet", value=_slot_value("amulet"), inline=False)

    relics = loadout.get("relics") or []
    cap = loadout.get("relic_cap")
    if relics:
        count = len(relics)
        header = f"{count}/{cap}" if cap is not None else str(count)
        relic_field_name = f"Relics ({header} equipped)"
        relic_value = "\n".join(
            f"• {format_relic_label(r.get('artifact_id', ''), with_stats=True)}"
            for r in relics
        )
        if cap is not None and count > cap:
            relic_value += f"\n⚠ Over cap — unequip {count - cap}."
    else:
        relic_field_name = f"Relics (0/{cap})" if cap is not None else "Relics"
        relic_value = "_None equipped_"
    embed.add_field(name=relic_field_name, value=relic_value, inline=False)

    inv_count = len(inventory)
    damaged_count = len(damaged)
    footer_parts = [f"{inv_count} owned"]
    if damaged_count:
        footer_parts.append(f"{damaged_count} damaged")
    footer_parts.append("Buy gear via /dig shop")
    embed.set_footer(text=" • ".join(footer_parts))
    return embed


class GearPanelView(discord.ui.View):
    """Top-level /dig gear panel: equip, unequip, repair via Selects."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        repair_all_cost: int = 0,
        has_damaged_gear: bool = False,
    ):
        super().__init__(timeout=180)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        # Surface the total repair cost on the button so players see the
        # bill before they click. Disable when nothing is damaged.
        if has_damaged_gear:
            if repair_all_cost > 0:
                self.repair_all_btn.label = f"Repair All ({repair_all_cost} JC)"
            else:
                self.repair_all_btn.label = "Repair All (Free)"
            self.repair_all_btn.disabled = False
        else:
            self.repair_all_btn.label = "Repair All"
            self.repair_all_btn.disabled = True

    async def _refresh(self, interaction: discord.Interaction) -> None:
        """Reload loadout + inventory and rebuild the panel embed in place."""
        loadout = await asyncio.to_thread(
            self.dig_service.get_loadout, self.user_id, self.guild_id
        )
        inventory = await asyncio.to_thread(
            self.dig_service.get_inventory_gear, self.user_id, self.guild_id
        )
        damaged = [g for g in inventory if g["durability"] < g["max_durability"]]
        total_cost = await asyncio.to_thread(
            self.dig_service.compute_repair_all_cost, self.user_id, self.guild_id,
        )
        embed = _build_gear_embed(loadout, inventory, damaged, self.dig_service)
        # Reset to the main panel buttons (in case we're being called from a sub-view).
        view = GearPanelView(
            self.dig_service, self.user_id, self.guild_id,
            repair_all_cost=total_cost, has_damaged_gear=bool(damaged),
        )
        await interaction.edit_original_response(embed=embed, view=view)

    def _check_owner(self, interaction: discord.Interaction) -> bool:
        return interaction.user.id == self.user_id

    @discord.ui.button(label="Equip", style=discord.ButtonStyle.primary, row=0)
    async def equip_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if not self._check_owner(interaction):
            await interaction.response.send_message("This isn't your gear panel.", ephemeral=True)
            return
        await safe_defer(interaction)
        inventory = await asyncio.to_thread(
            self.dig_service.get_inventory_gear, self.user_id, self.guild_id
        )
        relics = await asyncio.to_thread(
            self.dig_service.get_owned_relics, self.user_id, self.guild_id
        )
        unequipped_gear = [
            g for g in inventory
            if not g["equipped"] and g["durability"] > 0
        ]
        unequipped_relics = [r for r in relics if not r.get("equipped")]
        if not unequipped_gear and not unequipped_relics:
            await interaction.followup.send("Nothing to equip.", ephemeral=True)
            return
        view = GearSelectView(
            dig_service=self.dig_service,
            user_id=self.user_id,
            guild_id=self.guild_id,
            mode="equip",
            gear_items=unequipped_gear,
            relics=unequipped_relics,
            parent=self,
        )
        await interaction.edit_original_response(view=view)

    @discord.ui.button(label="Unequip", style=discord.ButtonStyle.secondary, row=0)
    async def unequip_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if not self._check_owner(interaction):
            await interaction.response.send_message("This isn't your gear panel.", ephemeral=True)
            return
        await safe_defer(interaction)
        inventory = await asyncio.to_thread(
            self.dig_service.get_inventory_gear, self.user_id, self.guild_id
        )
        relics = await asyncio.to_thread(
            self.dig_service.get_owned_relics, self.user_id, self.guild_id
        )
        equipped_gear = [g for g in inventory if g["equipped"]]
        equipped_relics = [r for r in relics if r.get("equipped")]
        if not equipped_gear and not equipped_relics:
            await interaction.followup.send("Nothing equipped to unequip.", ephemeral=True)
            return
        view = GearSelectView(
            dig_service=self.dig_service,
            user_id=self.user_id,
            guild_id=self.guild_id,
            mode="unequip",
            gear_items=equipped_gear,
            relics=equipped_relics,
            parent=self,
        )
        await interaction.edit_original_response(view=view)

    @discord.ui.button(label="Repair", style=discord.ButtonStyle.success, row=0)
    async def repair_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if not self._check_owner(interaction):
            await interaction.response.send_message("This isn't your gear panel.", ephemeral=True)
            return
        await safe_defer(interaction)
        inventory = await asyncio.to_thread(
            self.dig_service.get_inventory_gear, self.user_id, self.guild_id
        )
        damaged = [g for g in inventory if g["durability"] < g["max_durability"]]
        if not damaged:
            await interaction.followup.send("Nothing damaged to repair.", ephemeral=True)
            return
        view = GearSelectView(
            dig_service=self.dig_service,
            user_id=self.user_id,
            guild_id=self.guild_id,
            mode="repair",
            gear_items=damaged,
            relics=[],
            parent=self,
        )
        await interaction.edit_original_response(view=view)

    @discord.ui.button(label="Repair All", style=discord.ButtonStyle.danger, row=0)
    async def repair_all_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if not self._check_owner(interaction):
            await interaction.response.send_message("This isn't your gear panel.", ephemeral=True)
            return
        await safe_defer(interaction)
        # Run repair_all directly — the cost is shown in the result.
        result = _wrap(await asyncio.to_thread(
            self.dig_service.repair_all_gear, self.user_id, self.guild_id
        ))
        if not getattr(result, "success", True):
            await interaction.followup.send(
                getattr(result, "error", "Repair failed."), ephemeral=True
            )
        else:
            count = getattr(result, "repaired", 0)
            cost = getattr(result, "cost", 0)
            await interaction.followup.send(
                f"Repaired **{count}** piece(s) for **{cost}** {JOPACOIN_EMOTE}.",
                ephemeral=True,
            )
        await self._refresh(interaction)

    @discord.ui.button(label="Recycle Relics", style=discord.ButtonStyle.secondary, row=0)
    async def recycle_relics_btn(
        self, interaction: discord.Interaction, _btn: discord.ui.Button,
    ):
        if not self._check_owner(interaction):
            await interaction.response.send_message(
                "This isn't your gear panel.", ephemeral=True,
            )
            return
        await safe_defer(interaction)
        relics = await asyncio.to_thread(
            self.dig_service.get_owned_relics, self.user_id, self.guild_id,
        )
        candidates = [
            relic for relic in relics
            if not relic.get("equipped")
            and relic.get("recyclable")
            and relic.get("rarity") != "Legendary"
        ]
        viable_rarities = {
            rarity for rarity in {relic.get("rarity") for relic in candidates}
            if sum(relic.get("rarity") == rarity for relic in candidates) >= 3
        }
        candidates = [
            relic for relic in candidates
            if relic.get("rarity") in viable_rarities
        ]
        if not candidates:
            await interaction.followup.send(
                "You need three unequipped ordinary relics of the same rarity.",
                ephemeral=True,
            )
            return
        view = RecycleRelicView(
            dig_service=self.dig_service,
            user_id=self.user_id,
            guild_id=self.guild_id,
            relics=candidates,
            parent=self,
        )
        await interaction.edit_original_response(view=view)


class RecycleRelicView(discord.ui.View):
    """Select exactly three same-rarity relics for atomic recycling."""

    def __init__(
        self,
        *,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        relics: list[dict],
        parent: GearPanelView,
    ):
        super().__init__(timeout=180)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.parent = parent
        options = [
            discord.SelectOption(
                label=(
                    f"[{relic.get('rarity', 'Unknown')}] "
                    f"{relic.get('name', relic.get('id', '?'))}"
                )[:100],
                value=str(relic["db_id"]),
            )
            for relic in relics[:25]
            if relic.get("db_id") is not None
        ]
        self.select = discord.ui.Select(
            placeholder="Choose 3 relics of the same rarity",
            options=options,
            min_values=3,
            max_values=3,
        )
        self.select.callback = self._on_select
        self.add_item(self.select)

    async def _on_select(self, interaction: discord.Interaction):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel.", ephemeral=True)
            return
        await safe_defer(interaction)
        row_ids = [int(value) for value in self.select.values]
        result = _wrap(await asyncio.to_thread(
            self.dig_service.recycle_relics,
            self.user_id,
            self.guild_id,
            row_ids,
        ))
        if not getattr(result, "success", True):
            await interaction.followup.send(
                getattr(result, "error", "Recycling failed."), ephemeral=True,
            )
        else:
            await interaction.followup.send(
                "Recycled **3 "
                f"{getattr(result, 'source_rarity', '')}** relics into "
                f"**{getattr(result, 'relic_name', 'a relic')}** "
                f"({getattr(result, 'output_rarity', '')}).",
                ephemeral=True,
            )
        await self.parent._refresh(interaction)

    @discord.ui.button(label="Back", style=discord.ButtonStyle.secondary, row=1)
    async def back_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel.", ephemeral=True)
            return
        await safe_defer(interaction)
        await self.parent._refresh(interaction)


class GearSelectView(discord.ui.View):
    """Sub-view: dropdown of gear / relic items + Back button."""

    def __init__(
        self,
        *,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        mode: str,                 # "equip" | "unequip" | "repair"
        gear_items: list[dict],
        relics: list[dict],
        parent: GearPanelView,
    ):
        super().__init__(timeout=180)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.mode = mode
        self.parent = parent

        options: list[discord.SelectOption] = []
        for g in gear_items[:20]:
            slot_label = g["slot"].title()
            label = f"[{slot_label}] {g['name']} ({g['durability']}/{g['max_durability']})"
            if mode == "repair":
                cost = dig_service.compute_repair_cost(
                    g["slot"], g["tier"], g.get("item_id"),
                    g["durability"], g["max_durability"],
                )
                if cost > 0:
                    label += f" — {cost} JC"
            options.append(discord.SelectOption(
                label=label[:100],
                value=f"gear:{g['id']}",
                description=(g.get("effect") or "")[:100] or None,
            ))
        for r in relics[:25 - len(options)]:
            db_id = r.get("db_id")
            if db_id is None:
                # Skip malformed relic rows rather than encoding a string
                # artifact_id that would later silently parse to id=0.
                logger.warning("Relic %s missing db_id; skipping", r)
                continue
            options.append(discord.SelectOption(
                label=f"[Relic] {r.get('name', r.get('artifact_id', '?'))}"[:100],
                value=f"relic:{db_id}",
            ))
        if not options:
            options = [discord.SelectOption(label="(nothing here)", value="noop")]

        verb = {"equip": "Equip", "unequip": "Unequip", "repair": "Repair"}.get(mode, "Choose")
        self.select = discord.ui.Select(
            placeholder=f"{verb} which piece?",
            options=options,
            min_values=1,
            max_values=1,
        )
        self.select.callback = self._on_select
        self.add_item(self.select)

    async def _on_select(self, interaction: discord.Interaction):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel.", ephemeral=True)
            return
        await safe_defer(interaction)
        value = self.select.values[0]
        if value == "noop":
            await self.parent._refresh(interaction)
            return
        kind, _, raw = value.partition(":")
        try:
            target_id = int(raw)
        except ValueError:
            await interaction.followup.send("Invalid selection.", ephemeral=True)
            await self.parent._refresh(interaction)
            return

        gear_fns = {
            "equip": self.dig_service.equip_gear,
            "unequip": self.dig_service.unequip_gear,
            "repair": self.dig_service.repair_gear,
        }
        relic_fns = {
            "equip": self.dig_service.equip_relic_for_player,
            "unequip": self.dig_service.unequip_relic_for_player,
        }
        if kind == "gear":
            fn = gear_fns.get(self.mode)
            if fn is None:
                result = _wrap({"success": False, "error": "Action not supported."})
            else:
                result = _wrap(await asyncio.to_thread(
                    fn, self.user_id, self.guild_id, target_id,
                ))
        elif kind == "relic":
            fn = relic_fns.get(self.mode)
            if fn is None:
                result = _wrap({"success": False, "error": "Relics can't be repaired."})
            else:
                result = _wrap(await asyncio.to_thread(
                    fn, self.user_id, self.guild_id, target_id,
                ))
        else:
            result = _wrap({"success": False, "error": "Unknown selection."})

        if not getattr(result, "success", True):
            await interaction.followup.send(
                getattr(result, "error", "Action failed."), ephemeral=True
            )
        else:
            verb_past = {"equip": "Equipped", "unequip": "Unequipped", "repair": "Repaired"}.get(self.mode, "Done")
            cost = getattr(result, "cost", 0)
            cost_part = f" for {cost} {JOPACOIN_EMOTE}" if cost else ""
            await interaction.followup.send(f"{verb_past}{cost_part}.", ephemeral=True)
        await self.parent._refresh(interaction)

    @discord.ui.button(label="Back", style=discord.ButtonStyle.secondary, row=1)
    async def back_btn(self, interaction: discord.Interaction, _btn: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel.", ephemeral=True)
            return
        await safe_defer(interaction)
        await self.parent._refresh(interaction)
