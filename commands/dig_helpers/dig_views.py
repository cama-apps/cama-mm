"""Confirmation views for paid digs, sabotage, and tunnel abandonment."""

from __future__ import annotations

from typing import TYPE_CHECKING

import discord

from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer

if TYPE_CHECKING:
    from services.dig_service import DigService


class PaidDigView(discord.ui.View):
    """Simple confirm/cancel for paid digs."""

    def __init__(self, dig_service: DigService, user_id: int, guild_id: int | None, cost: int):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.cost = cost
        self.value: bool | None = None

    @discord.ui.button(label="Confirm", style=discord.ButtonStyle.green)
    async def confirm(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your dig.", ephemeral=True)
            return
        self.value = True
        self.stop()
        await safe_defer(interaction)

    @discord.ui.button(label="Cancel", style=discord.ButtonStyle.grey)
    async def cancel(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your dig.", ephemeral=True)
            return
        self.value = False
        self.stop()
        await safe_defer(interaction)


class ConfirmSabotageView(discord.ui.View):
    """Dark-themed confirmation for sabotage actions."""

    def __init__(self, user_id: int, target: discord.Member, cost: int, damage_range: str):
        super().__init__(timeout=30)
        self.user_id = user_id
        self.target = target
        self.cost = cost
        self.damage_range = damage_range
        self.value: bool | None = None

    def build_embed(self) -> discord.Embed:
        embed = discord.Embed(
            title="Confirm Sabotage",
            description=(
                f"**Target:** {self.target.display_name}\n"
                f"**Cost:** {self.cost} {JOPACOIN_EMOTE}\n"
                f"**Potential damage:** {self.damage_range} blocks\n\n"
                "Are you sure? If they have a trap set, you could take damage instead."
            ),
            color=0x2C2F33,
        )
        return embed

    @discord.ui.button(label="Sabotage", style=discord.ButtonStyle.danger)
    async def confirm(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your sabotage.", ephemeral=True)
            return
        self.value = True
        self.stop()
        await safe_defer(interaction)

    @discord.ui.button(label="Cancel", style=discord.ButtonStyle.grey)
    async def cancel(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your sabotage.", ephemeral=True)
            return
        self.value = False
        self.stop()
        await safe_defer(interaction)


class ConfirmAbandonView(discord.ui.View):
    """Confirm tunnel abandonment with refund info."""

    def __init__(self, user_id: int, refund: int):
        super().__init__(timeout=60)
        self.user_id = user_id
        self.refund = refund
        self.value: bool | None = None

    @discord.ui.button(label="Abandon Tunnel", style=discord.ButtonStyle.danger)
    async def confirm(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your tunnel.", ephemeral=True)
            return
        self.value = True
        self.stop()
        await safe_defer(interaction)

    @discord.ui.button(label="Cancel", style=discord.ButtonStyle.grey)
    async def cancel(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your tunnel.", ephemeral=True)
            return
        self.value = False
        self.stop()
        await safe_defer(interaction)
