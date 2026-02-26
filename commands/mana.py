"""
Daily MTG Mana command.

Each player may claim one mana land per day (reset at 4 AM PST).
The land type is randomly selected via weighted probabilities derived
from player attributes (balance, rating, degen score, tips, etc.).
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from services.mana_service import LAND_COLORS, LAND_EMOJIS, get_today_pst
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from services.mana_service import ManaService

logger = logging.getLogger("cama_bot.commands.mana")

# Embed colors keyed by land
LAND_EMBED_COLORS: dict[str, int] = {
    "Island": 0x3498DB,   # Blue
    "Mountain": 0xE74C3C,  # Red
    "Forest": 0x27AE60,   # Green
    "Plains": 0xF5F5DC,   # Off-white / beige
    "Swamp": 0x2C3E50,    # Dark grey/black
}


class ClaimManaView(discord.ui.View):
    """View shown when a player hasn't yet claimed today's mana."""

    def __init__(self, cog: "ManaCommands", interaction: discord.Interaction, is_ash_fan: bool):
        super().__init__(timeout=300)
        self.cog = cog
        self.original_interaction = interaction
        self.is_ash_fan = is_ash_fan

    @discord.ui.button(label="Claim Today's Mana", style=discord.ButtonStyle.primary, emoji="🔮")
    async def claim(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.original_interaction.user.id:
            await interaction.response.send_message(
                "This isn't your mana to claim.", ephemeral=True
            )
            return

        await interaction.response.defer()

        guild_id = interaction.guild.id if interaction.guild else None
        mana_service: ManaService = interaction.client.mana_service

        try:
            result = mana_service.assign_daily_mana(
                interaction.user.id, guild_id, is_ash_fan=self.is_ash_fan
            )
        except ValueError:
            # Already assigned (race condition or double-click)
            current = mana_service.get_current_mana(interaction.user.id, guild_id)
            if current:
                embed = _build_single_embed(interaction.user, current, already_assigned=True)
                await interaction.followup.edit_message(
                    interaction.message.id, embed=embed, view=None
                )
            return

        embed = _build_single_embed(interaction.user, result, already_assigned=True)
        self.stop()
        await interaction.followup.edit_message(interaction.message.id, embed=embed, view=None)


class ManaCommands(commands.Cog):
    def __init__(self, bot: commands.Bot):
        self.bot = bot

    @app_commands.command(name="mana", description="Check your daily mana land assignment")
    @app_commands.describe(
        user="View another player's mana (optional)",
        all="Show all guild members' current mana",
    )
    @app_commands.checks.cooldown(rate=3, per=10)
    async def mana(
        self,
        interaction: discord.Interaction,
        user: discord.Member | None = None,
        all: bool = False,
    ):
        if not await GLOBAL_RATE_LIMITER.check(interaction):
            return

        await safe_defer(interaction, ephemeral=False)

        guild_id = interaction.guild.id if interaction.guild else None
        mana_service: ManaService = interaction.client.mana_service

        # --- All mode: guild leaderboard ---
        if all:
            rows = mana_service.mana_repo.get_all_mana(guild_id)
            embed = _build_all_embed(rows, guild_id)
            await safe_followup(interaction, embed=embed)
            return

        # --- Single player mode ---
        target = user or interaction.user
        is_self = (target.id == interaction.user.id)

        is_ash_fan = False
        if is_self and interaction.guild:
            is_ash_fan = any("ash" in role.name.lower() for role in interaction.user.roles)

        current = mana_service.get_current_mana(target.id, guild_id)
        assigned_today = mana_service.has_assigned_today(target.id, guild_id)

        if assigned_today and current:
            # Already assigned — show result, no button
            embed = _build_single_embed(target, current, already_assigned=True)
            await safe_followup(interaction, embed=embed)
        elif is_self and not assigned_today:
            # Own mana, not yet claimed — show placeholder embed + claim button
            embed = _build_unclaimed_embed(interaction.user)
            view = ClaimManaView(self, interaction, is_ash_fan)
            await safe_followup(interaction, embed=embed, view=view)
        else:
            # Viewing someone else who hasn't claimed yet
            if current:
                embed = _build_single_embed(target, current, already_assigned=False)
            else:
                embed = _build_never_assigned_embed(target)
            await safe_followup(interaction, embed=embed)


# ---------------------------------------------------------------------------
# Embed builders
# ---------------------------------------------------------------------------

def _build_single_embed(
    member: discord.Member | discord.User,
    mana: dict,
    *,
    already_assigned: bool,
) -> discord.Embed:
    land = mana["land"]
    color_name = mana.get("color", LAND_COLORS.get(land, "Unknown"))
    emoji = mana.get("emoji", LAND_EMOJIS.get(land, "❓"))
    assigned_date = mana.get("assigned_date", "")

    embed_color = LAND_EMBED_COLORS.get(land, 0x95A5A6)
    today = get_today_pst()
    date_label = "Today" if assigned_date == today else assigned_date

    embed = discord.Embed(
        title=f"🔮 Daily Mana — {member.display_name}",
        color=embed_color,
    )
    embed.add_field(
        name="Land",
        value=f"{emoji} **{land}** · {color_name} Mana",
        inline=False,
    )
    embed.add_field(name="Assigned", value=date_label, inline=True)
    if isinstance(member, discord.Member) and member.display_avatar:
        embed.set_thumbnail(url=member.display_avatar.url)
    return embed


def _build_unclaimed_embed(member: discord.Member | discord.User) -> discord.Embed:
    embed = discord.Embed(
        title=f"🔮 Daily Mana — {member.display_name}",
        description="You haven't claimed today's mana yet.\nClick below to discover your land!",
        color=0x95A5A6,
    )
    if isinstance(member, discord.Member) and member.display_avatar:
        embed.set_thumbnail(url=member.display_avatar.url)
    return embed


def _build_never_assigned_embed(member: discord.Member | discord.User) -> discord.Embed:
    embed = discord.Embed(
        title=f"🔮 Daily Mana — {member.display_name}",
        description="This player hasn't claimed any mana yet.",
        color=0x95A5A6,
    )
    return embed


def _build_all_embed(rows: list[dict], guild_id: int | None) -> discord.Embed:
    embed = discord.Embed(
        title="🔮 Guild Mana Board",
        color=0x9B59B6,
    )
    today = get_today_pst()

    if not rows:
        embed.description = "No mana has been assigned in this guild yet."
        return embed

    # Group by land
    by_land: dict[str, list[dict]] = {}
    for row in rows:
        land = row.get("current_land") or "Unknown"
        by_land.setdefault(land, []).append(row)

    lines: list[str] = []
    for land in ("Island", "Mountain", "Forest", "Plains", "Swamp"):
        if land not in by_land:
            continue
        emoji = LAND_EMOJIS.get(land, "❓")
        color_name = LAND_COLORS.get(land, "")
        members = by_land[land]
        count = len(members)
        label = f"{emoji} **{land}** ({color_name}) — {count}"
        lines.append(label)

    # Today vs older
    today_count = sum(1 for r in rows if r.get("assigned_date") == today)
    old_count = len(rows) - today_count

    embed.description = "\n".join(lines)
    embed.set_footer(text=f"Claimed today: {today_count} · Previous days: {old_count}")
    return embed


async def setup(bot: commands.Bot):
    await bot.add_cog(ManaCommands(bot))
