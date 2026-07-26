"""Interactive button panel for /pet status.

The view is rebuilt after every action so button labels (owned counts,
disabled states) stay current. Owner-only: other users get an ephemeral nudge
to adopt their own cama.
"""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING

import discord

from domain.pet_constants import FOOD_ITEMS, SALT_LICK, food_cost
from services import error_codes

if TYPE_CHECKING:
    from commands.pet import PetCommands

logger = logging.getLogger("cama_bot.commands.pet.views")

VIEW_TIMEOUT_SECONDS = 180


class PetStatusView(discord.ui.View):
    def __init__(
        self,
        cog: PetCommands,
        owner_id: int,
        guild_id: int | None,
        *,
        supplies: dict[str, int] | None,
        species_id: str,
        can_feed: bool,
    ):
        super().__init__(timeout=VIEW_TIMEOUT_SECONDS)
        self.cog = cog
        self.owner_id = owner_id
        self.guild_id = guild_id
        self.message: discord.Message | None = None
        supplies = supplies or {}
        for item_id, food in FOOD_ITEMS.items():
            owned = supplies.get(item_id, 0)
            self.add_item(
                _FeedButton(item_id, food, owned, enabled=can_feed and owned > 0)
            )
        for item_id, food in FOOD_ITEMS.items():
            self.add_item(_BuyButton(item_id, food, food_cost(species_id, item_id)))
        self.add_item(_SaltLickButton(food_cost(species_id, SALT_LICK.item_id)))

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id != self.owner_id:
            await interaction.response.send_message(
                "That's not your cama. Adopt your own with `/pet adopt`!",
                ephemeral=True,
            )
            return False
        return True

    async def on_timeout(self) -> None:
        for item in self.children:
            item.disabled = True
        if self.message is not None:
            try:
                await self.message.edit(view=self)
            except discord.HTTPException:
                pass

    async def refresh(self, interaction: discord.Interaction) -> None:
        """Re-derive status and swap in a fresh embed, art, and view."""
        embed, file, view = await self.cog.compose_status(
            self.owner_id,
            self.guild_id,
            owner_name=interaction.user.display_name,
        )
        self.stop()
        kwargs = {"embed": embed, "view": view, "attachments": [file] if file else []}
        if interaction.response.is_done():
            message = await interaction.edit_original_response(**kwargs)
        else:
            await interaction.response.edit_message(**kwargs)
            message = await interaction.original_response()
        if view is not None:  # no view when the pet just died (memorial embed)
            view.message = message


class _FeedButton(discord.ui.Button):
    def __init__(self, item_id: str, food, owned: int, *, enabled: bool):
        super().__init__(
            label=f"Feed {food.display_name} ×{owned}",
            emoji=food.emoji,
            style=discord.ButtonStyle.success,
            disabled=not enabled,
            row=0,
        )
        self.item_id = item_id

    async def callback(self, interaction: discord.Interaction) -> None:
        view: PetStatusView = self.view
        result = await asyncio.to_thread(
            view.cog.pet_service.feed, view.owner_id, view.guild_id, self.item_id
        )
        if not result.success:
            await interaction.response.send_message(f"❌ {result.error}", ephemeral=True)
            return
        # Refresh consumes the component response (edit_message on the panel);
        # the spat notice goes out as a followup so the panel stays live.
        spat = result.value.spat
        pet_name = result.value.pet.name
        await view.refresh(interaction)
        if spat:
            await interaction.followup.send(
                f"💢 **{pet_name}** spat the "
                f"{FOOD_ITEMS[self.item_id].display_name} straight back at you. "
                "The temperament of legends.",
                ephemeral=True,
            )


class _BuyButton(discord.ui.Button):
    def __init__(self, item_id: str, food, cost: int):
        super().__init__(
            label=f"Buy {food.display_name} ({cost})",
            style=discord.ButtonStyle.secondary,
            row=1,
        )
        self.item_id = item_id

    async def callback(self, interaction: discord.Interaction) -> None:
        view: PetStatusView = self.view
        result = await asyncio.to_thread(
            view.cog.pet_service.buy, view.owner_id, view.guild_id, self.item_id, 1
        )
        if not result.success:
            await interaction.response.send_message(f"❌ {result.error}", ephemeral=True)
            return
        await view.refresh(interaction)


class _SaltLickButton(discord.ui.Button):
    def __init__(self, cost: int):
        super().__init__(
            label=f"Salt Lick ({cost})",
            emoji=SALT_LICK.emoji,
            style=discord.ButtonStyle.primary,
            row=1,
        )

    async def callback(self, interaction: discord.Interaction) -> None:
        view: PetStatusView = self.view
        result = await asyncio.to_thread(
            view.cog.pet_service.buy,
            view.owner_id,
            view.guild_id,
            SALT_LICK.item_id,
            1,
        )
        if not result.success:
            if result.error_code == error_codes.ALREADY_PAMPERED:
                await interaction.response.send_message(
                    "🧂 Already pampered. There is such a thing as too much salt.",
                    ephemeral=True,
                )
            else:
                await interaction.response.send_message(
                    f"❌ {result.error}", ephemeral=True
                )
            return
        await view.refresh(interaction)
