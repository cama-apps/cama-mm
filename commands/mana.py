"""
Daily MTG Mana command.

Each player is assigned one mana land per day (reset at 4 AM PST).
The land is chosen automatically the first time /mana is run each day.
/mana all:True rolls everyone in the guild and shows a paginated list.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_gamba_channel
from services.mana_service import LAND_COLORS, LAND_EMOJIS, LAND_ORDER, get_today_pst
from utils.interaction_safety import safe_defer, safe_followup

if TYPE_CHECKING:
    from services.mana_service import ManaService

logger = logging.getLogger("cama_bot.commands.mana")

LAND_EMBED_COLORS: dict[str, int] = {
    "Island": 0x3498DB,
    "Mountain": 0xE74C3C,
    "Forest": 0x27AE60,
    "Plains": 0xF5F5DC,
    "Swamp": 0x2C3E50,
}

PAGE_SIZE = 12  # players per page on the guild board


# ---------------------------------------------------------------------------
# Paginated guild board view
# ---------------------------------------------------------------------------

class ManaAllView(discord.ui.View):
    """Paginated view for the guild mana board."""

    def __init__(self, pages: list[discord.Embed]):
        super().__init__(timeout=300)
        self.pages = pages
        self.current = 0
        self._sync_buttons()

    def _sync_buttons(self) -> None:
        self.prev_btn.disabled = self.current == 0
        self.next_btn.disabled = self.current >= len(self.pages) - 1

    @discord.ui.button(label="◀ Prev", style=discord.ButtonStyle.secondary)
    async def prev_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        self.current -= 1
        self._sync_buttons()
        await interaction.response.edit_message(embed=self.pages[self.current], view=self)

    @discord.ui.button(label="Next ▶", style=discord.ButtonStyle.secondary)
    async def next_btn(self, interaction: discord.Interaction, button: discord.ui.Button):
        self.current += 1
        self._sync_buttons()
        await interaction.response.edit_message(embed=self.pages[self.current], view=self)


# ---------------------------------------------------------------------------
# Command cog
# ---------------------------------------------------------------------------

class ManaCommands(commands.Cog):
    def __init__(self, bot: commands.Bot):
        self.bot = bot

    @app_commands.command(name="mana", description="Check your daily mana land assignment")
    @app_commands.describe(
        user="View another player's mana (optional)",
        all="Roll everyone's mana for today and show the full guild list",
    )
    @app_commands.checks.cooldown(rate=3, per=10)
    async def mana(
        self,
        interaction: discord.Interaction,
        user: discord.Member | None = None,
        all: bool = False,
    ):
        if not await require_gamba_channel(interaction):
            return

        await safe_defer(interaction, ephemeral=False)

        guild_id = interaction.guild.id if interaction.guild else None
        mana_service: ManaService = interaction.client.mana_service

        # --- Guild board: roll everyone then paginate ---
        if all:
            # Collect ash-fan IDs from the member cache (no extra API calls needed)
            ash_fan_ids: set[int] = set()
            if interaction.guild:
                for member in interaction.guild.members:
                    if any("ash" in role.name.lower() for role in member.roles):
                        ash_fan_ids.add(member.id)

            mana_service.assign_all_daily_mana(guild_id, ash_fan_ids=ash_fan_ids)
            rows = mana_service.mana_repo.get_all_mana(guild_id)

            # Build display-name lookup from guild cache
            member_lookup: dict[int, str] = {}
            if interaction.guild:
                for member in interaction.guild.members:
                    member_lookup[member.id] = member.display_name

            pages = _build_all_pages(rows, member_lookup)
            if len(pages) == 1:
                await safe_followup(interaction, embed=pages[0])
            else:
                view = ManaAllView(pages)
                await safe_followup(interaction, embed=pages[0], view=view)
            return

        # --- Single player ---
        target = user or interaction.user
        is_self = target.id == interaction.user.id

        if is_self and not mana_service.has_assigned_today(target.id, guild_id):
            is_ash_fan = (
                any("ash" in role.name.lower() for role in interaction.user.roles)
                if interaction.guild
                else False
            )
            result = mana_service.assign_daily_mana(target.id, guild_id, is_ash_fan=is_ash_fan)
            embed = _build_single_embed(target, result)
            await safe_followup(interaction, embed=embed)
            return

        current = mana_service.get_current_mana(target.id, guild_id)
        if current:
            embed = _build_single_embed(target, current)
        else:
            embed = _build_no_mana_embed(target)
        await safe_followup(interaction, embed=embed)


# ---------------------------------------------------------------------------
# Embed / page builders
# ---------------------------------------------------------------------------

def _build_all_pages(
    rows: list[dict],
    member_lookup: dict[int, str],
) -> list[discord.Embed]:
    """Build a list of embeds (one per page) for the guild mana board.

    Rows are sorted by land order then by display name.
    """
    today = get_today_pst()

    # Determine sort key for each land
    land_rank = {land: i for i, land in enumerate(LAND_ORDER)}

    def sort_key(row: dict):
        land = row.get("current_land") or "Unknown"
        name = member_lookup.get(row["discord_id"], "").lower()
        return (land_rank.get(land, 99), name)

    sorted_rows = sorted(rows, key=sort_key)

    # Build one line per player
    lines: list[str] = []
    for row in sorted_rows:
        land = row.get("current_land") or "Unknown"
        emoji = LAND_EMOJIS.get(land, "❓")
        did = row["discord_id"]
        name = member_lookup.get(did) or f"<@{did}>"
        lines.append(f"{emoji} **{land}** · {name}")

    # Split into pages
    chunks = [lines[i : i + PAGE_SIZE] for i in range(0, max(len(lines), 1), PAGE_SIZE)]

    today_count = sum(1 for r in rows if r.get("assigned_date") == today)
    total = len(rows)
    num_pages = len(chunks)

    pages: list[discord.Embed] = []
    for page_num, chunk in enumerate(chunks, start=1):
        embed = discord.Embed(
            title="🔮 Guild Mana Board",
            description="\n".join(chunk) if chunk else "No mana assigned yet.",
            color=0x9B59B6,
        )
        embed.set_footer(
            text=f"Page {page_num}/{num_pages} · {today_count}/{total} assigned today · Resets 4 AM PST"
        )
        pages.append(embed)

    return pages


def _build_single_embed(
    member: discord.Member | discord.User,
    mana: dict,
) -> discord.Embed:
    land = mana["land"]
    color_name = mana.get("color", LAND_COLORS.get(land, "Unknown"))
    emoji = mana.get("emoji", LAND_EMOJIS.get(land, "❓"))
    assigned_date = mana.get("assigned_date", "")

    today = get_today_pst()
    date_label = "Today" if assigned_date == today else assigned_date

    embed = discord.Embed(
        title=f"🔮 Daily Mana — {member.display_name}",
        color=LAND_EMBED_COLORS.get(land, 0x95A5A6),
    )
    embed.add_field(name="Land", value=f"{emoji} **{land}** · {color_name} Mana", inline=False)
    embed.add_field(name="Assigned", value=date_label, inline=True)
    if isinstance(member, discord.Member) and member.display_avatar:
        embed.set_thumbnail(url=member.display_avatar.url)
    return embed


def _build_no_mana_embed(member: discord.Member | discord.User) -> discord.Embed:
    return discord.Embed(
        title=f"🔮 Daily Mana — {member.display_name}",
        description="This player hasn't been assigned any mana yet.",
        color=0x95A5A6,
    )


async def setup(bot: commands.Bot):
    await bot.add_cog(ManaCommands(bot))
