"""
Advanced statistics commands: /advstats, /matchup
"""

import logging
from typing import Optional

import discord
from discord.ext import commands
from discord import app_commands

from repositories.interfaces import IPairingsRepository
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger('cama_bot.commands.advstats')


class AdvancedStatsCommands(commands.Cog):
    """Commands for viewing advanced player statistics."""

    def __init__(self, bot: commands.Bot, pairings_repo: IPairingsRepository, player_repo):
        self.bot = bot
        self.pairings_repo = pairings_repo
        self.player_repo = player_repo

    @app_commands.command(name="advstats", description="View advanced pairwise statistics")
    @app_commands.describe(
        user="Player to view stats for (defaults to yourself)",
        min_games="Minimum games together/against to show (default: 3)",
        limit="Number of players to show per category (default: 5, max: 15)",
    )
    async def advstats(
        self,
        interaction: discord.Interaction,
        user: Optional[discord.Member] = None,
        min_games: int = 3,
        limit: int = 5,
    ):
        """View pairwise statistics: best/worst teammates, best/worst matchups."""
        logger.info(
            "Advstats command: User %s requested stats for %s",
            interaction.user.id,
            user.id if user else "self",
        )
        if not await safe_defer(interaction, ephemeral=True):
            return

        target_id = user.id if user else interaction.user.id
        target_name = user.display_name if user else interaction.user.display_name

        # Clamp limit to reasonable bounds
        limit = max(1, min(limit, 15))

        # Verify player is registered
        player = self.player_repo.get_by_id(target_id)
        if not player:
            await safe_followup(
                interaction,
                content=f"{'That user is' if user else 'You are'} not registered. Use `/register` first.",
                ephemeral=True,
            )
            return

        embed = discord.Embed(
            title=f"Advanced Stats for {target_name}",
            color=discord.Color.purple(),
        )

        # Best Teammates
        best_teammates = self.pairings_repo.get_best_teammates(target_id, min_games=min_games, limit=limit)
        if best_teammates:
            lines = []
            for i, tm in enumerate(best_teammates, 1):
                teammate_name = self._get_player_mention(tm["teammate_id"])
                wins = tm["wins_together"]
                games = tm["games_together"]
                rate = tm["win_rate"] * 100
                lines.append(f"{i}. {teammate_name} - {rate:.0f}% ({wins}W/{games - wins}L)")
            embed.add_field(
                name="Best Teammates",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="Best Teammates", value="No winning records yet", inline=True)

        # Worst Teammates
        worst_teammates = self.pairings_repo.get_worst_teammates(target_id, min_games=min_games, limit=limit)
        if worst_teammates:
            lines = []
            for i, tm in enumerate(worst_teammates, 1):
                teammate_name = self._get_player_mention(tm["teammate_id"])
                wins = tm["wins_together"]
                games = tm["games_together"]
                rate = tm["win_rate"] * 100
                lines.append(f"{i}. {teammate_name} - {rate:.0f}% ({wins}W/{games - wins}L)")
            embed.add_field(
                name="Worst Teammates",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="Worst Teammates", value="No losing records yet", inline=True)

        # Spacer
        embed.add_field(name="\u200b", value="\u200b", inline=True)

        # Best Matchups (dominates)
        best_matchups = self.pairings_repo.get_best_matchups(target_id, min_games=min_games, limit=limit)
        if best_matchups:
            lines = []
            for i, m in enumerate(best_matchups, 1):
                opponent_name = self._get_player_mention(m["opponent_id"])
                wins = m["wins_against"]
                games = m["games_against"]
                rate = m["win_rate"] * 100
                lines.append(f"{i}. {opponent_name} - {rate:.0f}% ({wins}W/{games - wins}L)")
            embed.add_field(
                name="Dominates",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="Dominates", value="No winning matchups yet", inline=True)

        # Worst Matchups (struggles against)
        worst_matchups = self.pairings_repo.get_worst_matchups(target_id, min_games=min_games, limit=limit)
        if worst_matchups:
            lines = []
            for i, m in enumerate(worst_matchups, 1):
                opponent_name = self._get_player_mention(m["opponent_id"])
                wins = m["wins_against"]
                games = m["games_against"]
                rate = m["win_rate"] * 100
                lines.append(f"{i}. {opponent_name} - {rate:.0f}% ({wins}W/{games - wins}L)")
            embed.add_field(
                name="Struggles Against",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="Struggles Against", value="No losing matchups yet", inline=True)

        # Spacer for row alignment
        embed.add_field(name="\u200b", value="\u200b", inline=True)

        # Most Played With
        most_played_with = self.pairings_repo.get_most_played_with(target_id, min_games=min_games, limit=limit)
        if most_played_with:
            lines = []
            for i, tm in enumerate(most_played_with, 1):
                teammate_name = self._get_player_mention(tm["teammate_id"])
                wins = tm["wins_together"]
                games = tm["games_together"]
                rate = tm["win_rate"] * 100
                lines.append(f"{i}. {teammate_name} - {games}g ({rate:.0f}%)")
            embed.add_field(
                name="Most Played With",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="Most Played With", value="No data yet", inline=True)

        # Most Played Against
        most_played_against = self.pairings_repo.get_most_played_against(target_id, min_games=min_games, limit=limit)
        if most_played_against:
            lines = []
            for i, m in enumerate(most_played_against, 1):
                opponent_name = self._get_player_mention(m["opponent_id"])
                wins = m["wins_against"]
                games = m["games_against"]
                rate = m["win_rate"] * 100
                lines.append(f"{i}. {opponent_name} - {games}g ({rate:.0f}%)")
            embed.add_field(
                name="Most Played Against",
                value="\n".join(lines),
                inline=True,
            )
        else:
            embed.add_field(name="Most Played Against", value="No data yet", inline=True)

        # Spacer for row alignment
        embed.add_field(name="\u200b", value="\u200b", inline=True)

        # Evenly Matched Teammates (50% win rate)
        even_teammates = self.pairings_repo.get_evenly_matched_teammates(target_id, min_games=min_games, limit=limit)
        if even_teammates:
            lines = []
            for i, tm in enumerate(even_teammates, 1):
                teammate_name = self._get_player_mention(tm["teammate_id"])
                wins = tm["wins_together"]
                games = tm["games_together"]
                lines.append(f"{i}. {teammate_name} ({wins}W/{games - wins}L)")
            embed.add_field(
                name="Evenly Matched (Teammates)",
                value="\n".join(lines),
                inline=True,
            )

        # Evenly Matched Opponents (50% win rate)
        even_opponents = self.pairings_repo.get_evenly_matched_opponents(target_id, min_games=min_games, limit=limit)
        if even_opponents:
            lines = []
            for i, m in enumerate(even_opponents, 1):
                opponent_name = self._get_player_mention(m["opponent_id"])
                wins = m["wins_against"]
                games = m["games_against"]
                lines.append(f"{i}. {opponent_name} ({wins}W/{games - wins}L)")
            embed.add_field(
                name="Evenly Matched (Opponents)",
                value="\n".join(lines),
                inline=True,
            )

        # Get totals for footer
        counts = self.pairings_repo.get_pairing_counts(target_id, min_games=min_games)
        footer_parts = [f"Min {min_games} games"]
        if counts["unique_teammates"] > 0 or counts["unique_opponents"] > 0:
            footer_parts.append(f"{counts['unique_teammates']} teammates, {counts['unique_opponents']} opponents tracked")
        embed.set_footer(text=" | ".join(footer_parts))
        await interaction.followup.send(embed=embed)

    def _get_player_mention(self, discord_id: int) -> str:
        """Get a mention string for a player, falling back to username if needed."""
        if discord_id and discord_id > 0:
            return f"<@{discord_id}>"
        # Fallback to stored username
        player = self.player_repo.get_by_id(discord_id)
        return player.name if player else f"Unknown ({discord_id})"

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
        if not await safe_defer(interaction, ephemeral=True):
            return

        if player1.id == player2.id:
            await safe_followup(
                interaction,
                content="Cannot compare a player with themselves!",
                ephemeral=True,
            )
            return

        # Verify both players are registered
        p1 = self.player_repo.get_by_id(player1.id)
        p2 = self.player_repo.get_by_id(player2.id)

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

        h2h = self.pairings_repo.get_head_to_head(player1.id, player2.id)

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
            embed.add_field(name="As Opponents", value="Never played against each other", inline=False)

        await interaction.followup.send(embed=embed)


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    pairings_repo = getattr(bot, 'pairings_repo', None)
    player_repo = getattr(bot, 'player_repo', None)

    if pairings_repo is None or player_repo is None:
        logger.warning("advstats cog: pairings_repo or player_repo not available, skipping")
        return

    await bot.add_cog(AdvancedStatsCommands(bot, pairings_repo, player_repo))
