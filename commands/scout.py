"""
Scout command: /scout

Generates hero scouting reports for Dota 2 players showing:
- Hero stats aggregated across players
- Win/loss record per hero (color-coded)
- Ban frequency (heroes banned when players are in game)
- Pagination for viewing more heroes
"""

import asyncio
import logging
import re
from typing import NamedTuple

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_guild
from utils.drawing import draw_scout_report
from utils.embeds import COLOR_BLUE
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.scout")

# Heroes per page
HEROES_PER_PAGE = 10

# Discord's hard limit on an embed field's value length.
EMBED_FIELD_LIMIT = 1024


class TeamContext(NamedTuple):
    """Players resolved from active match/draft/lobby context.

    When the source has assigned teams (post-shuffle, or a draft with teams
    picked) ``split`` is True and ``radiant``/``dire`` are populated. For an
    open lobby or an unassigned draft pool there is no team split: ``split``
    is False and only ``flat`` carries the players.
    """

    radiant: list[int]
    dire: list[int]
    flat: list[int]
    source_label: str | None
    split: bool
    filtered_prefix: str


class ScoutView(discord.ui.View):
    """Paginated view for scout reports."""

    def __init__(
        self,
        all_heroes: list[dict],
        player_names: list[str],
        player_count: int,
        total_matches: int,
        title: str,
        timeout: int = 840,
    ):
        super().__init__(timeout=timeout)
        self.all_heroes = all_heroes
        self.player_names = player_names
        self.player_count = player_count
        self.total_matches = total_matches
        self.title = title
        self.current_page = 0
        self.total_pages = max(1, (len(all_heroes) + HEROES_PER_PAGE - 1) // HEROES_PER_PAGE)
        self.message: discord.Message | None = None
        self._update_buttons()

    def _update_buttons(self):
        """Update button states based on current page."""
        self.prev_button.disabled = self.current_page == 0
        self.next_button.disabled = self.current_page >= self.total_pages - 1

    def _get_page_heroes(self) -> list[dict]:
        """Get heroes for current page."""
        start = self.current_page * HEROES_PER_PAGE
        end = start + HEROES_PER_PAGE
        return self.all_heroes[start:end]

    async def _generate_embed_and_file(self) -> tuple[discord.Embed, discord.File]:
        """Generate embed and image file for current page."""
        page_heroes = self._get_page_heroes()
        scout_data = {
            "player_count": self.player_count,
            "total_matches": self.total_matches,
            "heroes": page_heroes,
        }

        image_bytes = await asyncio.to_thread(
            draw_scout_report,
            scout_data=scout_data,
            player_names=self.player_names,
            title=self.title,
        )

        file = discord.File(image_bytes, filename="scout_report.png")

        start_hero = self.current_page * HEROES_PER_PAGE + 1
        end_hero = min((self.current_page + 1) * HEROES_PER_PAGE, len(self.all_heroes))

        embed = discord.Embed(
            title=self.title,
            description=f"{self.player_count} players | Heroes {start_hero}-{end_hero} of {len(self.all_heroes)}",
            color=discord.Color.gold(),
        )
        embed.set_image(url="attachment://scout_report.png")
        embed.set_footer(
            text=f"Page {self.current_page + 1}/{self.total_pages} | Tot=W+L+B | CR=Tot/Games | WR=W/(W+L)"
        )

        return embed, file

    async def on_timeout(self):
        """Delete the message when the view times out."""
        if self.message:
            try:
                await self.message.delete()
                logger.info("Scout message deleted on timeout")
            except discord.NotFound:
                logger.debug("Scout message was already deleted")
            except discord.HTTPException as e:
                logger.warning(f"Failed to delete scout message: {e}")

    @discord.ui.button(label="◀ Prev", style=discord.ButtonStyle.secondary)
    async def prev_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        """Go to previous page."""
        if self.current_page > 0:
            self.current_page -= 1
            self._update_buttons()

            embed, file = await self._generate_embed_and_file()
            await interaction.response.edit_message(embed=embed, attachments=[file], view=self)
        else:
            # Already at first page - acknowledge interaction to avoid Discord error
            await interaction.response.defer()

    @discord.ui.button(label="Next ▶", style=discord.ButtonStyle.secondary)
    async def next_button(self, interaction: discord.Interaction, button: discord.ui.Button):
        """Go to next page."""
        if self.current_page < self.total_pages - 1:
            self.current_page += 1
            self._update_buttons()

            embed, file = await self._generate_embed_and_file()
            await interaction.response.edit_message(embed=embed, attachments=[file], view=self)
        else:
            # Already at last page - acknowledge interaction to avoid Discord error
            await interaction.response.defer()


class ScoutCommands(commands.Cog):
    """Commands for player scouting."""

    scout = app_commands.Group(name="scout", description="Player scouting tools")

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

    def _resolve_team_context(self, guild_id: int | None) -> TeamContext:
        """
        Resolve players from active match/draft/lobby context, keeping teams split.

        Priority: pending match (post-shuffle) -> active draft -> open lobby.

        Args:
            guild_id: Guild ID for context lookup

        Returns:
            A TeamContext. When no context is found every list is empty and
            ``source_label`` is None.
        """
        # Priority 1: Pending match (post-shuffle)
        if self.match_service:
            try:
                last_shuffle = self.match_service.get_last_shuffle(guild_id)
                if last_shuffle:
                    radiant = list(last_shuffle.radiant_team_ids or [])
                    dire = list(last_shuffle.dire_team_ids or [])
                    if radiant or dire:
                        return TeamContext(
                            radiant, dire, radiant + dire, "Active Match", True, ""
                        )
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
                        return TeamContext(
                            radiant, dire, radiant + dire, "Draft", True, "Draft "
                        )

                    # If draft not yet assigned teams, use player pool
                    if draft_state.player_pool_ids:
                        pool = list(draft_state.player_pool_ids)
                        return TeamContext([], [], pool, "Draft Pool", False, "")
            except Exception:
                logger.debug("Failed to check draft state", exc_info=True)

        # Priority 3: Active lobby
        lobby = self.lobby_manager.get_lobby(guild_id=guild_id)
        if lobby and lobby.players:
            player_ids = list(lobby.players)
            if lobby.conditional_players:
                player_ids.extend(lobby.conditional_players)
            return TeamContext([], [], player_ids, "Lobby", False, "")

        return TeamContext([], [], [], None, False, "")

    def _resolve_player_context(
        self, guild_id: int | None, team_filter: str | None = None
    ) -> tuple[list[int], str | None]:
        """
        Resolve players from active match/lobby context.

        Thin wrapper over :meth:`_resolve_team_context` that flattens the result
        to the ``(player_ids, source_label)`` contract used by ``/scout report``.

        Args:
            guild_id: Guild ID for context lookup
            team_filter: Optional "radiant" or "dire" to filter to specific team

        Returns:
            (player_ids, source_label)
        """
        ctx = self._resolve_team_context(guild_id)
        if not ctx.flat:
            return [], None
        if ctx.split and team_filter == "radiant":
            return list(ctx.radiant), f"{ctx.filtered_prefix}Radiant"
        if ctx.split and team_filter == "dire":
            return list(ctx.dire), f"{ctx.filtered_prefix}Dire"
        return list(ctx.flat), ctx.source_label

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

    def _build_link_lines(
        self,
        discord_ids: list[int],
        name_map: dict[int, str],
        steam_map: dict[int, list[int]],
    ) -> list[str]:
        """
        Build one display line per player listing their Dotabuff profile link(s).

        Players with no linked Steam account are still listed, with a note.

        Args:
            discord_ids: Players to render, in display order
            name_map: discord_id -> display name
            steam_map: discord_id -> Steam32 IDs (primary first)

        Returns:
            One Markdown line per player.
        """
        lines: list[str] = []
        for did in discord_ids:
            name = name_map.get(did) or f"<@{did}>"
            steam_ids = steam_map.get(did) or []
            if not steam_ids:
                lines.append(f"**{name}** — no linked Steam account")
                continue
            if len(steam_ids) == 1:
                labels = ["Dotabuff"]
            else:
                labels = [f"Dotabuff {i}" for i in range(1, len(steam_ids) + 1)]
            links = " · ".join(
                f"[{label}](https://www.dotabuff.com/players/{sid})"
                for label, sid in zip(labels, steam_ids)
            )
            lines.append(f"**{name}** — {links}")
        return lines

    def _add_player_field(
        self, embed: discord.Embed, name: str, lines: list[str]
    ) -> None:
        """
        Add player lines to ``embed`` under ``name``.

        Splits into multiple fields ("name", "name (2)", ...) when the joined
        text would exceed Discord's per-field character limit, so a large flat
        list (e.g. a full lobby of smurf-heavy players) never overflows.
        """
        if not lines:
            embed.add_field(name=name, value="—", inline=False)
            return
        chunks: list[list[str]] = [[]]
        for line in lines:
            current = chunks[-1]
            joined_len = sum(len(item) + 1 for item in current) + len(line)
            if current and joined_len > EMBED_FIELD_LIMIT:
                chunks.append([line])
            else:
                current.append(line)
        for index, chunk in enumerate(chunks):
            field_name = name if index == 0 else f"{name} ({index + 1})"
            embed.add_field(name=field_name, value="\n".join(chunk), inline=False)

    @scout.command(
        name="report",
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
    @require_guild
    async def report(
        self,
        interaction: discord.Interaction,
        players: str | None = None,
        team: app_commands.Choice[str] | None = None,
    ):
        """Generate a visual hero scouting report."""
        if not await safe_defer(interaction):
            return

        guild_id = interaction.guild.id
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
            player_ids, source_label = await asyncio.to_thread(
                self._resolve_player_context,
                guild_id,
                team_value,
            )

        if not player_ids:
            await safe_followup(
                interaction,
                content="No players found. Use `@mentions` or specify a `team` (radiant/dire) during an active match.",
            )
            return

        # Fetch ALL hero data (no limit) for pagination
        scout_data = await asyncio.to_thread(
            self.match_service.get_scout_data,
            player_ids,
            guild_id,
            limit=100,
        )

        if not scout_data.get("heroes"):
            await safe_followup(
                interaction,
                content="No enriched match data found for these players. "
                "Matches need to be enriched with `/enrich match` or `/enrich discover` first.",
            )
            return

        # Build player names list
        players_obj = await asyncio.to_thread(self.player_service.get_by_ids, player_ids, guild_id)
        player_name_map = {p.discord_id: p.name for p in players_obj}
        player_names = []
        for pid in player_ids:
            if pid in player_name_map:
                player_names.append(player_name_map[pid])

        # Generate paginated view
        try:
            title = f"SCOUT: {source_label}" if source_label else "SCOUT REPORT"
            all_heroes = scout_data.get("heroes", [])

            view = ScoutView(
                all_heroes=all_heroes,
                player_names=player_names,
                player_count=len(player_ids),
                total_matches=scout_data.get("total_matches", 0),
                title=title,
            )

            embed, file = await view._generate_embed_and_file()
            message = await safe_followup(interaction, embed=embed, file=file, view=view)

            if message:
                view.message = message

        except Exception as e:
            logger.error(f"Error generating scout report: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content="Failed to generate scout report. Please try again.",
            )

    @scout.command(
        name="links",
        description="List Dotabuff profile links for players in the current game",
    )
    @app_commands.describe(
        players="@mention players to look up (optional)",
        team="Team to list from active match: radiant or dire",
    )
    @app_commands.choices(
        team=[
            app_commands.Choice(name="Radiant", value="radiant"),
            app_commands.Choice(name="Dire", value="dire"),
        ]
    )
    @require_guild
    async def links(
        self,
        interaction: discord.Interaction,
        players: str | None = None,
        team: app_commands.Choice[str] | None = None,
    ):
        """List Dotabuff profile links for players in the current game."""
        if not await safe_defer(interaction):
            return

        guild_id = interaction.guild.id
        team_value = team.value if team else None

        radiant_ids: list[int] = []
        dire_ids: list[int] = []
        flat_ids: list[int] = []
        source_label: str | None = None
        two_teams = False

        if players:
            # Explicit @mentions: render a flat list, no team split.
            mentioned_ids = self._parse_mentions(players)
            if mentioned_ids:
                flat_ids = list(dict.fromkeys(mentioned_ids))
                source_label = f"{len(flat_ids)} Player{'s' if len(flat_ids) > 1 else ''}"

        if not flat_ids:
            ctx = await asyncio.to_thread(self._resolve_team_context, guild_id)
            if ctx.split and team_value == "radiant":
                flat_ids, source_label = list(ctx.radiant), f"{ctx.filtered_prefix}Radiant"
            elif ctx.split and team_value == "dire":
                flat_ids, source_label = list(ctx.dire), f"{ctx.filtered_prefix}Dire"
            elif ctx.split:
                radiant_ids, dire_ids = list(ctx.radiant), list(ctx.dire)
                source_label, two_teams = ctx.source_label, True
            else:
                flat_ids, source_label = list(ctx.flat), ctx.source_label

        if not (flat_ids or radiant_ids or dire_ids):
            await safe_followup(
                interaction,
                content="No players found. Use `@mentions` or specify a `team` (radiant/dire) during an active match.",
            )
            return

        all_ids = list(dict.fromkeys(radiant_ids + dire_ids + flat_ids))
        steam_map = await asyncio.to_thread(self.player_service.get_steam_ids_bulk, all_ids)
        players_obj = await asyncio.to_thread(self.player_service.get_by_ids, all_ids, guild_id)
        name_map = {p.discord_id: p.name for p in players_obj}

        embed = discord.Embed(
            title=f"Dotabuff Links — {source_label}" if source_label else "Dotabuff Links",
            color=COLOR_BLUE,
        )
        if two_teams:
            self._add_player_field(
                embed, "Radiant", self._build_link_lines(radiant_ids, name_map, steam_map)
            )
            self._add_player_field(
                embed, "Dire", self._build_link_lines(dire_ids, name_map, steam_map)
            )
        else:
            self._add_player_field(
                embed, "Players", self._build_link_lines(flat_ids, name_map, steam_map)
            )

        await safe_followup(interaction, embed=embed)


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
