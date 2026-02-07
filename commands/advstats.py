"""
Pairwise statistics commands: /matchup, /rebuildpairings
"""

import asyncio
import logging

import discord
from discord import app_commands
from discord.ext import commands

from repositories.interfaces import IPairingsRepository
from services.permissions import has_admin_permission
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.advstats")


class AdvancedStatsCommands(commands.Cog):
    """Commands for viewing pairwise player statistics."""

    def __init__(
        self,
        bot: commands.Bot,
        pairings_repo: IPairingsRepository,
        player_repo,
        *,
        flavor_text_service=None,
        guild_config_service=None,
    ):
        self.bot = bot
        self.pairings_repo = pairings_repo
        self.player_repo = player_repo
        self.flavor_text_service = flavor_text_service
        self.guild_config_service = guild_config_service

    @app_commands.command(name="matchup", description="View head-to-head stats between two players")
    @app_commands.describe(
        player1="First player",
        player2="Second player",
    )
    async def matchup(
        self,
        interaction: discord.Interaction,
        player1: discord.Member,
        player2: discord.Member,
    ):
        """View detailed head-to-head statistics between two players."""
        logger.info(
            "Matchup command: %s vs %s requested by %s",
            player1.id,
            player2.id,
            interaction.user.id,
        )
        if not await safe_defer(interaction, ephemeral=False):
            return

        if player1.id == player2.id:
            await safe_followup(
                interaction,
                content="Cannot compare a player with themselves!",
                ephemeral=True,
            )
            return

        # Verify both players are registered
        guild_id = interaction.guild.id if interaction.guild else None
        p1 = await asyncio.to_thread(self.player_repo.get_by_id, player1.id, guild_id)
        p2 = await asyncio.to_thread(self.player_repo.get_by_id, player2.id, guild_id)

        if not p1:
            await safe_followup(
                interaction,
                content=f"{player1.display_name} is not registered.",
                ephemeral=True,
            )
            return
        if not p2:
            await safe_followup(
                interaction,
                content=f"{player2.display_name} is not registered.",
                ephemeral=True,
            )
            return

        h2h = await asyncio.to_thread(self.pairings_repo.get_head_to_head, player1.id, player2.id)

        embed = discord.Embed(
            title=f"{player1.display_name} vs {player2.display_name}",
            color=discord.Color.orange(),
        )

        if not h2h:
            embed.description = "No games played together or against each other yet."
            await interaction.followup.send(embed=embed)
            return

        # Together stats
        games_together = h2h["games_together"]
        wins_together = h2h["wins_together"]
        if games_together > 0:
            together_rate = (wins_together / games_together) * 100
            embed.add_field(
                name="As Teammates",
                value=f"{games_together} games, {wins_together} wins ({together_rate:.0f}%)",
                inline=False,
            )
        else:
            embed.add_field(name="As Teammates", value="Never played together", inline=False)

        # Against stats
        games_against = h2h["games_against"]
        p1_wins = 0
        p2_wins = 0
        if games_against > 0:
            # Determine who is player1 in canonical order
            p1_canonical = h2h["player1_id"]
            if player1.id == p1_canonical:
                p1_wins = h2h["player1_wins_against"]
                p2_wins = games_against - p1_wins
            else:
                p2_wins = h2h["player1_wins_against"]
                p1_wins = games_against - p2_wins

            embed.add_field(
                name="As Opponents",
                value=(
                    f"{games_against} games\n"
                    f"{player1.display_name}: {p1_wins} wins\n"
                    f"{player2.display_name}: {p2_wins} wins"
                ),
                inline=False,
            )
        else:
            embed.add_field(
                name="As Opponents", value="Never played against each other", inline=False
            )

        # AI insight for matchup - disabled

        await interaction.followup.send(embed=embed)

    @app_commands.command(
        name="rebuildpairings", description="Rebuild pairwise stats from match history (Admin only)"
    )
    async def rebuildpairings(self, interaction: discord.Interaction):
        """Admin command to rebuild all pairwise statistics from match history."""
        logger.info(
            "Rebuildpairings command: User %s (%s)",
            interaction.user.id,
            interaction.user,
        )

        if not await safe_defer(interaction, ephemeral=True):
            return

        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="This command is admin-only.",
                ephemeral=True,
            )
            return

        try:
            count = await asyncio.to_thread(self.pairings_repo.rebuild_all_pairings)
            await safe_followup(
                interaction,
                content=f"Rebuilt pairwise statistics. {count} pairings calculated from match history.",
                ephemeral=True,
            )
            logger.info(f"Rebuildpairings: rebuilt {count} pairings")
        except Exception as e:
            logger.error(f"Error rebuilding pairings: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content=f"Error rebuilding pairings: {e}",
                ephemeral=True,
            )


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    pairings_repo = getattr(bot, "pairings_repo", None)
    player_repo = getattr(bot, "player_repo", None)
    flavor_text_service = getattr(bot, "flavor_text_service", None)
    guild_config_service = getattr(bot, "guild_config_service", None)

    if pairings_repo is None or player_repo is None:
        logger.warning("advstats cog: pairings_repo or player_repo not available, skipping")
        return

    await bot.add_cog(
        AdvancedStatsCommands(
            bot,
            pairings_repo,
            player_repo,
            flavor_text_service=flavor_text_service,
            guild_config_service=guild_config_service,
        )
    )
