"""
Scout command: /scout

Generates hero scouting reports for Dota 2 players showing:
- Top 10 most played heroes (aggregated across players)
- Win/loss record per hero (color-coded)
- Ban frequency (heroes banned when players are in game)
- Primary role for each hero
"""

import asyncio
import logging
import re

import discord
from discord import app_commands
from discord.ext import commands

from utils.drawing import draw_scout_report
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.scout")


class ScoutCommands(commands.Cog):
    """Commands for player scouting."""

    def __init__(
        self,
        bot: commands.Bot,
        match_service,
        player_service,
        lobby_manager,
    ):
        self.bot = bot
        self.match_service = match_service
        self.player_service = player_service
        self.lobby_manager = lobby_manager

    def _resolve_player_context(
        self, guild_id: int | None, team_filter: str | None = None
    ) -> tuple[list[int], str | None]:
        """
        Resolve players from active match/lobby context.

        Args:
            guild_id: Guild ID for context lookup
            team_filter: Optional "radiant" or "dire" to filter to specific team

        Returns:
            (player_ids, source_label)
        """
        # Priority 1: Pending match (post-shuffle)
        if self.match_service:
            try:
                last_shuffle = self.match_service.get_last_shuffle(guild_id)
                if last_shuffle:
                    radiant_ids = last_shuffle.get("radiant_team_ids") or []
                    dire_ids = last_shuffle.get("dire_team_ids") or []

                    if radiant_ids or dire_ids:
                        if team_filter == "radiant":
                            return list(radiant_ids), "Radiant"
                        elif team_filter == "dire":
                            return list(dire_ids), "Dire"
                        else:
                            # Return both teams if no filter
                            return list(radiant_ids) + list(dire_ids), "Active Match"
            except Exception:
                logger.debug("Failed to check pending match state", exc_info=True)

        # Priority 2: Active draft
        draft_state_manager = getattr(self.bot, "draft_state_manager", None)
        if draft_state_manager:
            try:
                draft_state = draft_state_manager.get_state(guild_id)
                if draft_state:
                    radiant = list(draft_state.radiant_player_ids or [])
                    dire = list(draft_state.dire_player_ids or [])

                    if radiant or dire:
                        if team_filter == "radiant":
                            return radiant, "Draft Radiant"
                        elif team_filter == "dire":
                            return dire, "Draft Dire"
                        else:
                            return radiant + dire, "Draft"

                    # If draft not yet assigned teams, use player pool
                    if draft_state.player_pool_ids:
                        return list(draft_state.player_pool_ids), "Draft Pool"
            except Exception:
                logger.debug("Failed to check draft state", exc_info=True)

        # Priority 3: Active lobby
        lobby = self.lobby_manager.get_lobby()
        if lobby and lobby.players:
            player_ids = list(lobby.players)
            if lobby.conditional_players:
                player_ids.extend(lobby.conditional_players)
            return player_ids, "Lobby"

        return [], None

    def _parse_mentions(self, text: str) -> list[int]:
        """
        Parse Discord user mentions from text.

        Args:
            text: String potentially containing <@123456> or <@!123456> mentions

        Returns:
            List of Discord user IDs
        """
        # Match both <@123456> and <@!123456> formats
        pattern = r"<@!?(\d+)>"
        matches = re.findall(pattern, text)
        return [int(m) for m in matches]

    @app_commands.command(
        name="scout",
        description="Generate hero scouting report for players",
    )
    @app_commands.describe(
        players="@mention players to scout (optional)",
        team="Team to scout from active match: radiant or dire",
    )
    @app_commands.choices(
        team=[
            app_commands.Choice(name="Radiant", value="radiant"),
            app_commands.Choice(name="Dire", value="dire"),
        ]
    )
    async def scout(
        self,
        interaction: discord.Interaction,
        players: str | None = None,
        team: app_commands.Choice[str] | None = None,
    ):
        """Generate a visual hero scouting report."""
        if not await safe_defer(interaction):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        team_value = team.value if team else None

        # Determine player IDs
        player_ids: list[int] = []
        source_label: str | None = None

        if players:
            # Parse @mentions from the input string
            mentioned_ids = self._parse_mentions(players)
            if mentioned_ids:
                player_ids = list(set(mentioned_ids))
                source_label = f"{len(player_ids)} Player{'s' if len(player_ids) > 1 else ''}"

        if not player_ids:
            # Use context resolution with optional team filter
            player_ids, source_label = self._resolve_player_context(guild_id, team_value)

        if not player_ids:
            await safe_followup(
                interaction,
                content="No players found. Use `@mentions` or specify a `team` (radiant/dire) during an active match.",
            )
            return

        # Fetch scout data
        scout_data = self.match_service.get_scout_data(player_ids, guild_id, limit=10)

        if not scout_data.get("heroes"):
            await safe_followup(
                interaction,
                content="No enriched match data found for these players. "
                "Matches need to be enriched with `/enrichmatch` or `/autodiscover` first.",
            )
            return

        # Build player names list
        players_obj = self.player_service.get_by_ids(player_ids, guild_id)
        player_name_map = {p.discord_id: p.name for p in players_obj}
        player_names = []
        for pid in player_ids:
            if pid in player_name_map:
                player_names.append(player_name_map[pid])

        # Generate image (run in thread pool to avoid blocking on HTTP image fetches)
        try:
            title = f"SCOUT: {source_label}" if source_label else "SCOUT REPORT"

            # Run image generation in thread pool since it may fetch hero images from CDN
            image_bytes = await asyncio.to_thread(
                draw_scout_report,
                scout_data=scout_data,
                player_names=player_names,
                title=title,
            )

            file = discord.File(image_bytes, filename="scout_report.png")

            embed = discord.Embed(
                title=title,
                description=f"{len(player_ids)} players | Top 10 heroes",
                color=discord.Color.gold(),
            )
            embed.set_image(url="attachment://scout_report.png")
            embed.set_footer(
                text="W-L = Wins-Losses | B:N = Times Banned | # = Position"
            )

            await safe_followup(interaction, embed=embed, file=file)
        except Exception as e:
            logger.error(f"Error generating scout report: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content="Failed to generate scout report. Please try again.",
            )


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    match_service = getattr(bot, "match_service", None)
    player_service = getattr(bot, "player_service", None)
    lobby_manager = getattr(bot, "lobby_manager", None)

    if not match_service:
        logger.warning("ScoutCommands: match_service not found on bot, skipping cog load")
        return
    if not player_service:
        logger.warning("ScoutCommands: player_service not found on bot, skipping cog load")
        return
    if not lobby_manager:
        logger.warning("ScoutCommands: lobby_manager not found on bot, skipping cog load")
        return

    await bot.add_cog(ScoutCommands(bot, match_service, player_service, lobby_manager))
