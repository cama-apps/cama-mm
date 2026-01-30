"""
Hero Grid command: /herogrid

Generates a player x hero grid visualization showing hero pool overlap.
Circle size = games played, circle color = win rate.
"""

import logging

import discord
from discord import app_commands
from discord.ext import commands

from utils.drawing import draw_hero_grid
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.herogrid")


class HeroGridCommands(commands.Cog):
    """Commands for hero grid visualization."""

    def __init__(self, bot: commands.Bot, match_repo, player_repo, lobby_manager):
        self.bot = bot
        self.match_repo = match_repo
        self.player_repo = player_repo
        self.lobby_manager = lobby_manager

    @app_commands.command(
        name="herogrid",
        description="Generate a player x hero grid showing hero pools and win rates",
    )
    @app_commands.describe(
        source="Player source: auto picks lobby if available, otherwise all players",
        min_games="Minimum games on a hero for it to appear (default: 2)",
        limit="Maximum number of players to include",
    )
    @app_commands.choices(
        source=[
            app_commands.Choice(name="Auto (lobby if available)", value="auto"),
            app_commands.Choice(name="Current Lobby", value="lobby"),
            app_commands.Choice(name="All Players", value="all"),
        ]
    )
    async def herogrid(
        self,
        interaction: discord.Interaction,
        source: app_commands.Choice[str] | None = None,
        min_games: int = 2,
        limit: int | None = None,
    ):
        """Generate a player x hero grid image."""
        if not await safe_defer(interaction):
            return

        source_value = source.value if source else "auto"

        # Determine player list
        player_ids = []
        used_lobby = False

        if source_value in ("auto", "lobby"):
            lobby = self.lobby_manager.get_lobby()
            if lobby and lobby.players:
                player_ids = list(lobby.players)
                if lobby.conditional_players:
                    player_ids.extend(lobby.conditional_players)
                used_lobby = True

        if not player_ids and source_value == "lobby":
            await safe_followup(
                interaction,
                content="No active lobby found. Use `source: All Players` to show all players.",
            )
            return

        if not player_ids:
            # Fallback to all players with enriched data (pre-sorted by games desc)
            enriched_players = self.match_repo.get_players_with_enriched_data()
            player_ids = [p["discord_id"] for p in enriched_players]

        if not player_ids:
            await safe_followup(
                interaction,
                content="No players with enriched match data found.",
            )
            return

        # Apply limit
        if limit is not None and limit > 0:
            player_ids = player_ids[:limit]

        # Clamp min_games
        min_games = max(1, min(min_games, 10))

        # Fetch grid data
        grid_data = self.match_repo.get_multi_player_hero_stats(player_ids)

        if not grid_data:
            await safe_followup(
                interaction,
                content="No enriched hero data found for the selected players.",
            )
            return

        # Build player names dict (preserving order)
        players = self.player_repo.get_by_ids(player_ids)
        player_names = {}
        for p in players:
            player_names[p.discord_id] = p.name

        # Include any player_ids that weren't found in repo with fallback names
        for pid in player_ids:
            if pid not in player_names:
                player_names[pid] = f"User {pid}"

        # Generate image
        try:
            grid_title = "Hero Grid: Lobby" if used_lobby else "Hero Grid"

            image_bytes = draw_hero_grid(
                grid_data=grid_data,
                player_names=player_names,
                min_games=min_games,
                title=grid_title,
            )
            file = discord.File(image_bytes, filename="hero_grid.png")

            embed = discord.Embed(
                title=grid_title,
                description=f"{len(player_ids)} players | min {min_games} games per hero",
                color=discord.Color.blue(),
            )
            embed.set_image(url="attachment://hero_grid.png")
            embed.set_footer(
                text="Circle size = games played | Color = win rate (green \u226560%, yellow \u226540%, red <40%)"
            )

            await safe_followup(interaction, embed=embed, file=file)
        except Exception as e:
            logger.error(f"Error generating hero grid: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content="Failed to generate hero grid image. Please try again.",
            )


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    match_repo = getattr(bot, "match_repo", None)
    player_repo = getattr(bot, "player_repo", None)
    lobby_manager = getattr(bot, "lobby_manager", None)

    if match_repo is None:
        logger.warning("HeroGridCommands: match_repo not found on bot, skipping cog load")
        return
    if player_repo is None:
        logger.warning("HeroGridCommands: player_repo not found on bot, skipping cog load")
        return
    if lobby_manager is None:
        logger.warning("HeroGridCommands: lobby_manager not found on bot, skipping cog load")
        return

    await bot.add_cog(HeroGridCommands(bot, match_repo, player_repo, lobby_manager))
