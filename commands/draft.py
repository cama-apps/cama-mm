"""
Draft commands for Immortal Draft mode: /setcaptain, /startdraft, /restartdraft
"""

import logging
import random
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

import math

from config import BET_LOCK_SECONDS, JOPACOIN_MIN_BET, LOBBY_READY_THRESHOLD
from domain.models.draft import SNAKE_DRAFT_ORDER, DraftPhase, DraftState
from rating_system import CamaRatingSystem
from domain.services.draft_service import DraftService
from services.draft_state_manager import DraftStateManager
from services.permissions import has_admin_permission
from utils.draft_embeds import MAX_NAME_LEN, format_player_row, format_roles
from utils.formatting import JOPACOIN_EMOTE, format_betting_display
from utils.interaction_safety import safe_defer

if TYPE_CHECKING:
    from services.lobby_manager_service import LobbyManagerService as LobbyManager
    from repositories.player_repository import PlayerRepository
    from services.match_service import MatchService

logger = logging.getLogger("cama_bot.commands.draft")


# ============================================================================
# Constants
# ============================================================================

DRAFT_POOL_SIZE = 10  # Number of players selected for draft
DRAFT_TOTAL_PICKS = 8  # Total picks (10 players - 2 captains)
BUTTON_LABEL_MAX_LENGTH = 80  # Discord button label limit
PRE_DRAFT_TIMEOUT = 300.0  # 5 minutes for pre-draft choices
DRAFTING_TIMEOUT = 600.0  # 10 minutes for player drafting


# ============================================================================
# Pre-Draft Choice Views
# ============================================================================


class WinnerChoiceView(discord.ui.View):
    """Winner of coinflip chooses: Side or Hero Pick Order."""

    def __init__(
        self,
        cog: "DraftCommands",
        guild_id: int,
        winner_id: int,
        timeout: float = PRE_DRAFT_TIMEOUT,
    ):
        super().__init__(timeout=timeout)
        self.cog = cog
        self.guild_id = guild_id
        self.winner_id = winner_id

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id != self.winner_id:
            await interaction.response.send_message(
                "Only the coinflip winner can make this choice.", ephemeral=True
            )
            return False
        return True

    @discord.ui.button(label="Choose Side", style=discord.ButtonStyle.primary, emoji="🗺️")
    async def choose_side(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self.cog.handle_winner_chose_side(interaction, self.guild_id)
        self.stop()

    @discord.ui.button(label="Choose Hero Pick Order", style=discord.ButtonStyle.primary, emoji="⚔️")
    async def choose_hero_pick(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self.cog.handle_winner_chose_hero_pick(interaction, self.guild_id)
        self.stop()

    async def on_timeout(self):
        await self.cog._handle_draft_timeout(self.guild_id)


class SideChoiceView(discord.ui.View):
    """Choose Radiant or Dire."""

    def __init__(
        self,
        cog: "DraftCommands",
        guild_id: int,
        chooser_id: int,
        is_winner: bool,
        timeout: float = PRE_DRAFT_TIMEOUT,
    ):
        super().__init__(timeout=timeout)
        self.cog = cog
        self.guild_id = guild_id
        self.chooser_id = chooser_id
        self.is_winner = is_winner  # True if coinflip winner, False if loser

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id != self.chooser_id:
            await interaction.response.send_message(
                "Only the designated captain can make this choice.", ephemeral=True
            )
            return False
        return True

    @discord.ui.button(label="Radiant", style=discord.ButtonStyle.success, emoji="🟢")
    async def choose_radiant(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self.cog.handle_side_choice(interaction, self.guild_id, "radiant", self.is_winner)
        self.stop()

    @discord.ui.button(label="Dire", style=discord.ButtonStyle.danger, emoji="🔴")
    async def choose_dire(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self.cog.handle_side_choice(interaction, self.guild_id, "dire", self.is_winner)
        self.stop()

    async def on_timeout(self):
        await self.cog._handle_draft_timeout(self.guild_id)


class HeroPickOrderView(discord.ui.View):
    """Choose First or Second hero pick in-game."""

    def __init__(
        self,
        cog: "DraftCommands",
        guild_id: int,
        chooser_id: int,
        is_winner: bool,
        timeout: float = PRE_DRAFT_TIMEOUT,
    ):
        super().__init__(timeout=timeout)
        self.cog = cog
        self.guild_id = guild_id
        self.chooser_id = chooser_id
        self.is_winner = is_winner

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id != self.chooser_id:
            await interaction.response.send_message(
                "Only the designated captain can make this choice.", ephemeral=True
            )
            return False
        return True

    @discord.ui.button(label="First Pick", style=discord.ButtonStyle.primary, emoji="1️⃣")
    async def choose_first(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self.cog.handle_hero_pick_choice(interaction, self.guild_id, "first", self.is_winner)
        self.stop()

    @discord.ui.button(label="Second Pick", style=discord.ButtonStyle.secondary, emoji="2️⃣")
    async def choose_second(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self.cog.handle_hero_pick_choice(interaction, self.guild_id, "second", self.is_winner)
        self.stop()

    async def on_timeout(self):
        await self.cog._handle_draft_timeout(self.guild_id)


class PlayerDraftOrderView(discord.ui.View):
    """Lower-rated captain chooses first or second in player draft."""

    def __init__(
        self,
        cog: "DraftCommands",
        guild_id: int,
        chooser_id: int,
        timeout: float = PRE_DRAFT_TIMEOUT,
    ):
        super().__init__(timeout=timeout)
        self.cog = cog
        self.guild_id = guild_id
        self.chooser_id = chooser_id

    async def interaction_check(self, interaction: discord.Interaction) -> bool:
        if interaction.user.id != self.chooser_id:
            await interaction.response.send_message(
                "Only the lower-rated captain can make this choice.", ephemeral=True
            )
            return False
        return True

    @discord.ui.button(label="Pick First", style=discord.ButtonStyle.primary, emoji="1️⃣")
    async def pick_first(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self.cog.handle_player_draft_order_choice(interaction, self.guild_id, "first")
        self.stop()

    @discord.ui.button(label="Pick Second", style=discord.ButtonStyle.secondary, emoji="2️⃣")
    async def pick_second(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self.cog.handle_player_draft_order_choice(interaction, self.guild_id, "second")
        self.stop()

    async def on_timeout(self):
        await self.cog._handle_draft_timeout(self.guild_id)


# ============================================================================
# Player Draft Views
# ============================================================================


class PlayerPickButton(discord.ui.Button):
    """Button to pick a player during draft."""

    def __init__(
        self,
        player_id: int,
        player_name: str,
        player_rating: float,
        player_roles: list[str] | None = None,
        row: int = 0,
    ):
        roles_str = format_roles(player_roles)
        label = f"{player_name} ({player_rating:.0f}) {roles_str}".strip()
        super().__init__(
            label=label[:BUTTON_LABEL_MAX_LENGTH],
            style=discord.ButtonStyle.primary,
            custom_id=f"draft_pick_{player_id}",
            row=row,
        )
        self.player_id = player_id

    async def callback(self, interaction: discord.Interaction):
        view: DraftingView = self.view
        await view.cog.handle_player_pick(interaction, view.guild_id, self.player_id)


class SidePreferenceButton(discord.ui.Button):
    """Button for players to indicate side preference."""

    def __init__(self, side: str, row: int = 4):
        if side == "radiant":
            super().__init__(
                label="Prefer Radiant",
                style=discord.ButtonStyle.success,
                emoji="🟢",
                custom_id="draft_pref_radiant",
                row=row,
            )
        else:
            super().__init__(
                label="Prefer Dire",
                style=discord.ButtonStyle.danger,
                emoji="🔴",
                custom_id="draft_pref_dire",
                row=row,
            )
        self.side = side

    async def callback(self, interaction: discord.Interaction):
        view: DraftingView = self.view
        await view.cog.handle_side_preference(interaction, view.guild_id, self.side)


class ClearPreferenceButton(discord.ui.Button):
    """Button to clear side preference."""

    def __init__(self, row: int = 4):
        super().__init__(
            label="Clear Preference",
            style=discord.ButtonStyle.secondary,
            emoji="❌",
            custom_id="draft_pref_clear",
            row=row,
        )

    async def callback(self, interaction: discord.Interaction):
        view: DraftingView = self.view
        await view.cog.handle_side_preference(interaction, view.guild_id, None)


class DraftingView(discord.ui.View):
    """View for the active drafting phase with player pick buttons."""

    def __init__(
        self,
        cog: "DraftCommands",
        guild_id: int,
        available_players: list,  # List of Player objects
        current_captain_id: int,
        timeout: float = DRAFTING_TIMEOUT,
    ):
        super().__init__(timeout=timeout)
        self.cog = cog
        self.guild_id = guild_id
        self.current_captain_id = current_captain_id

        # Add player pick buttons (up to 8 players, across rows 0-1)
        for i, player in enumerate(available_players[:8]):
            row = i // 4  # 4 buttons per row
            self.add_item(
                PlayerPickButton(
                    player_id=player.discord_id,
                    player_name=player.name,
                    player_rating=player.glicko_rating or 1500.0,
                    player_roles=player.preferred_roles,
                    row=row,
                )
            )

        # Add side preference buttons on row 4 (last row)
        self.add_item(SidePreferenceButton("radiant", row=4))
        self.add_item(SidePreferenceButton("dire", row=4))
        self.add_item(ClearPreferenceButton(row=4))

    async def on_timeout(self):
        await self.cog._handle_draft_timeout(self.guild_id)


# ============================================================================
# Draft Commands Cog
# ============================================================================


class DraftCommands(commands.Cog):
    """Commands for Immortal Draft captain-based team selection."""

    def __init__(
        self,
        bot: commands.Bot,
        player_repo: "PlayerRepository",
        lobby_manager: "LobbyManager",
        draft_state_manager: DraftStateManager,
        draft_service: DraftService,
        match_service: "MatchService | None" = None,
    ):
        self.bot = bot
        self.player_repo = player_repo
        self.lobby_manager = lobby_manager
        self.draft_state_manager = draft_state_manager
        self.draft_service = draft_service
        self.match_service = match_service

    # ========================================================================
    # /setcaptain command
    # ========================================================================

    @app_commands.command(
        name="setcaptain",
        description="Set yourself as eligible (or ineligible) to be a captain in Immortal Draft",
    )
    @app_commands.describe(
        eligible="Yes to be captain-eligible, No to opt out",
    )
    @app_commands.choices(
        eligible=[
            app_commands.Choice(name="Yes", value="yes"),
            app_commands.Choice(name="No", value="no"),
        ]
    )
    async def setcaptain(
        self,
        interaction: discord.Interaction,
        eligible: app_commands.Choice[str],
    ):
        """Toggle captain eligibility for Immortal Draft mode."""
        logger.info(
            f"Setcaptain command: User {interaction.user.id} ({interaction.user}) "
            f"setting eligible={eligible.value}"
        )

        # Check if user is registered
        player = self.player_repo.get_by_id(interaction.user.id)
        if not player:
            await interaction.response.send_message(
                "❌ You must be registered first. Use `/register` to sign up.",
                ephemeral=True,
            )
            return

        is_eligible = eligible.value == "yes"
        self.player_repo.set_captain_eligible(interaction.user.id, is_eligible)

        if is_eligible:
            await interaction.response.send_message(
                "✅ You are now **captain-eligible** for Immortal Draft!\n"
                "You may be selected as a captain when `/startdraft` is used.",
                ephemeral=True,
            )
        else:
            await interaction.response.send_message(
                "✅ You are no longer captain-eligible for Immortal Draft.",
                ephemeral=True,
            )

    # ========================================================================
    # /restartdraft command
    # ========================================================================

    @app_commands.command(
        name="restartdraft",
        description="Restart the current Immortal Draft (preserves lobby)",
    )
    async def restartdraft(
        self,
        interaction: discord.Interaction,
    ):
        """Restart an active draft. Only captains or admins can restart."""
        guild_id = interaction.guild_id
        user_id = interaction.user.id

        logger.info(
            f"Restartdraft command: User {user_id} ({interaction.user}) in guild {guild_id}"
        )

        state = self.draft_state_manager.get_state(guild_id)
        if not state:
            await interaction.response.send_message(
                "❌ No active draft to restart.",
                ephemeral=True,
            )
            return

        # Check if user is a captain or admin
        is_captain = user_id in (state.captain1_id, state.captain2_id)
        is_admin = False

        # Check admin permissions
        if hasattr(self.bot, "admin_user_ids"):
            is_admin = user_id in self.bot.admin_user_ids
        if not is_admin and interaction.guild:
            member = interaction.guild.get_member(user_id)
            if member and member.guild_permissions.administrator:
                is_admin = True

        if not is_captain and not is_admin:
            await interaction.response.send_message(
                "❌ Only captains or server admins can restart the draft.",
                ephemeral=True,
            )
            return

        # Clear any pending stakes if a match was created from the draft
        if self.match_service:
            pending_state = self.match_service.get_last_shuffle(guild_id)
            if pending_state and pending_state.get("is_draft"):
                # Clear stakes
                stake_service = getattr(self.bot, "stake_service", None)
                if stake_service:
                    stake_service.clear_stakes(guild_id, pending_state)
                # Clear pending match
                self.match_service.clear_last_shuffle(guild_id)

        # Clear the draft state
        self.draft_state_manager.clear_state(guild_id)

        # Get user name for the message
        user_name = interaction.user.display_name

        await interaction.response.send_message(
            f"🔄 **Draft Restarted** by {user_name}\n\n"
            "The lobby has been preserved. Use `/startdraft` to start a new draft.",
        )

        logger.info(
            f"Draft restarted by {interaction.user} (captain={is_captain}, admin={is_admin}) "
            f"in guild {guild_id}"
        )

    # ========================================================================
    # Sample/Debug Commands (Admin only)
    # ========================================================================

    @app_commands.command(
        name="sampledraftinprogress",
        description="[Admin] Show sample draft UI mid-draft for testing",
    )
    async def sampledraftinprogress(self, interaction: discord.Interaction):
        """Show a sample draft in progress for UI testing."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message("❌ Admin only command.", ephemeral=True)
            return

        # Create a mock draft state
        guild_id = interaction.guild_id or 0
        state = DraftState(guild_id=guild_id)

        # Use fake player IDs (negative) for the sample
        state.captain1_id = -101
        state.captain2_id = -102
        state.captain1_rating = 1650.0
        state.captain2_rating = 1580.0
        state.radiant_captain_id = -101
        state.dire_captain_id = -102
        state.coinflip_winner_id = -101
        state.radiant_hero_pick_order = 1
        state.dire_hero_pick_order = 2
        state.player_draft_first_captain_id = -102  # Lower rated picks first
        state.phase = DraftPhase.DRAFTING

        # Pool of 10 players (2 captains + 8 available), plus 2 excluded
        state.player_pool_ids = [-101, -102, -103, -104, -105, -106, -107, -108, -109, -110]
        state.excluded_player_ids = [-111, -112]

        # Captains are on their teams
        state.radiant_player_ids = [-101]
        state.dire_player_ids = [-102, -103]  # Dire already picked one

        # Some side preferences
        state.side_preferences = {-105: "radiant", -107: "dire"}

        state.current_pick_index = 1  # Second pick

        # Ensure fake players exist in DB for display
        await self._ensure_sample_players_exist()

        embed = await self._build_draft_embed(interaction.guild, state)

        # Get available players for buttons
        available_ids = state.available_player_ids
        available_players = self.player_repo.get_by_ids(available_ids)
        available_players.sort(key=lambda p: p.glicko_rating or 1500.0, reverse=True)

        view = DraftingView(
            cog=self,
            guild_id=guild_id,
            available_players=available_players,
            current_captain_id=state.current_captain_id,
        )

        await interaction.response.send_message(
            content="**[SAMPLE UI - Not a real draft]**",
            embed=embed,
            view=view,
        )

    @app_commands.command(
        name="sampledraftcomplete",
        description="[Admin] Show sample draft complete UI for testing",
    )
    async def sampledraftcomplete(self, interaction: discord.Interaction):
        """Show a sample completed draft for UI testing."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message("❌ Admin only command.", ephemeral=True)
            return

        # Create a mock completed draft state
        guild_id = interaction.guild_id or 0
        state = DraftState(guild_id=guild_id)

        state.captain1_id = -101
        state.captain2_id = -102
        state.captain1_rating = 1650.0
        state.captain2_rating = 1580.0
        state.radiant_captain_id = -101
        state.dire_captain_id = -102
        state.radiant_hero_pick_order = 1
        state.dire_hero_pick_order = 2
        state.phase = DraftPhase.COMPLETE

        # Full teams
        state.radiant_player_ids = [-101, -103, -105, -107, -109]
        state.dire_player_ids = [-102, -104, -106, -108, -110]
        state.excluded_player_ids = [-111, -112]

        # Ensure fake players exist
        await self._ensure_sample_players_exist()

        # Create mock pending_state for betting display
        now_ts = int(time.time())
        mock_pending_state = {
            "radiant_team_ids": state.radiant_player_ids,
            "dire_team_ids": state.dire_player_ids,
            "betting_mode": "pool",
            "bet_lock_until": now_ts + BET_LOCK_SECONDS,
            "shuffle_timestamp": now_ts,
            "blind_bets_result": {
                "created": 3,
                "total_radiant": 15,
                "total_dire": 12,
            },
        }

        embed = await self._build_draft_complete_embed(
            interaction.guild, state, pending_state=mock_pending_state
        )

        await interaction.response.send_message(
            content="**[SAMPLE UI - Not a real draft]**",
            embed=embed,
        )

    async def _ensure_sample_players_exist(self):
        """Create sample fake players if they don't exist."""
        sample_players = [
            (-101, "SampleCapt1", 1650.0, ["1", "2"]),
            (-102, "SampleCapt2", 1580.0, ["1", "3"]),
            (-103, "SamplePlayer3", 1720.0, ["2"]),
            (-104, "SamplePlayer4", 1540.0, ["3", "4"]),
            (-105, "SamplePlayer5", 1610.0, ["4", "5"]),
            (-106, "SamplePlayer6", 1490.0, ["5"]),
            (-107, "SamplePlayer7", 1555.0, ["1", "2", "3"]),
            (-108, "SamplePlayer8", 1480.0, ["4", "5"]),
            (-109, "SamplePlayer9", 1630.0, ["2", "3"]),
            (-110, "SamplePlayer10", 1520.0, ["3", "4", "5"]),
            (-111, "ExcludedPlayer1", 1450.0, ["5"]),
            (-112, "ExcludedPlayer2", 1410.0, ["4", "5"]),
        ]

        for pid, name, rating, roles in sample_players:
            existing = self.player_repo.get_by_id(pid)
            if not existing:
                try:
                    self.player_repo.add(
                        discord_id=pid,
                        discord_username=name,
                        initial_mmr=None,
                        glicko_rating=rating,
                        glicko_rd=100.0,
                        glicko_volatility=0.06,
                        preferred_roles=roles,
                    )
                except ValueError:
                    pass

    # ========================================================================
    # Core draft execution (reusable by /shuffle auto-redirect)
    # ========================================================================

    async def _execute_draft(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        lobby,
        force_random_captains: bool = False,
        specified_captain1_id: int | None = None,
        specified_captain2_id: int | None = None,
    ) -> bool:
        """
        Core draft logic, callable from /startdraft or /shuffle auto-redirect.

        Args:
            interaction: The Discord interaction
            guild_id: The guild ID
            lobby: The Lobby object
            force_random_captains: If True, picks any 2 random players as captains
                                   (ignoring captain eligibility)
            specified_captain1_id: Optional specified captain 1 ID
            specified_captain2_id: Optional specified captain 2 ID

        Returns:
            True if draft started successfully, False otherwise.
        """
        # Get all players including conditional ones
        lobby_player_ids = list(lobby.players) + list(lobby.conditional_players)

        # Get player ratings for captain selection
        players = self.player_repo.get_by_ids(lobby_player_ids)
        player_ratings = {p.discord_id: p.glicko_rating or 1500.0 for p in players}

        if force_random_captains:
            # Randomly select 2 players as captains (no eligibility check)
            if len(lobby_player_ids) < 2:
                await interaction.followup.send(
                    "❌ Not enough players to select captains.",
                    ephemeral=True,
                )
                return False
            selected_captains = random.sample(lobby_player_ids, 2)
            specified_captain1_id = selected_captains[0]
            specified_captain2_id = selected_captains[1]
            # All players are eligible when force_random_captains is True
            eligible_captain_ids = lobby_player_ids.copy()
        else:
            # Normal captain eligibility check
            eligible_captain_ids = self.player_repo.get_captain_eligible_players(lobby_player_ids)

            # If captains are specified, add them to eligible list if not already there
            if specified_captain1_id and specified_captain1_id not in eligible_captain_ids:
                eligible_captain_ids.append(specified_captain1_id)
            if specified_captain2_id and specified_captain2_id not in eligible_captain_ids:
                eligible_captain_ids.append(specified_captain2_id)

            # Check we have enough captains
            needed_captains = 2
            if specified_captain1_id:
                needed_captains -= 1
            if specified_captain2_id:
                needed_captains -= 1

            if len(eligible_captain_ids) < needed_captains + (1 if specified_captain1_id else 0) + (
                1 if specified_captain2_id else 0
            ):
                if needed_captains == 2:
                    await interaction.followup.send(
                        "❌ Not enough captain-eligible players in lobby.\n"
                        "Players can use `/setcaptain` to mark themselves as eligible, "
                        "or specify captains with the `captain1` and `captain2` options.",
                        ephemeral=True,
                    )
                else:
                    await interaction.followup.send(
                        f"❌ Need at least {needed_captains} more captain-eligible player(s) in lobby.\n"
                        "Players can use `/setcaptain` to mark themselves as eligible.",
                        ephemeral=True,
                    )
                return False

        # Select captains
        try:
            captain_pair = self.draft_service.select_captains(
                eligible_ids=eligible_captain_ids,
                player_ratings=player_ratings,
                specified_captain1=specified_captain1_id,
                specified_captain2=specified_captain2_id,
            )
        except ValueError as e:
            await interaction.followup.send(f"❌ {e}", ephemeral=True)
            return False

        # Get exclusion counts for player pool selection
        exclusion_counts = self.player_repo.get_exclusion_counts(lobby_player_ids)

        # Select player pool (10 players, captains always included)
        try:
            pool_result = self.draft_service.select_player_pool(
                lobby_player_ids=lobby_player_ids,
                exclusion_counts=exclusion_counts,
                forced_include_ids=[captain_pair.captain1_id, captain_pair.captain2_id],
                pool_size=DRAFT_POOL_SIZE,
            )
        except ValueError as e:
            await interaction.followup.send(f"❌ {e}", ephemeral=True)
            return False

        # Update exclusion counts for excluded players
        for excluded_id in pool_result.excluded_ids:
            self.player_repo.increment_exclusion_count(excluded_id)

        # Create draft state
        try:
            state = self.draft_state_manager.create_draft(guild_id)
        except ValueError as e:
            await interaction.followup.send(f"❌ {e}", ephemeral=True)
            return False

        # Initialize state
        state.player_pool_ids = pool_result.selected_ids
        state.excluded_player_ids = pool_result.excluded_ids
        state.captain1_id = captain_pair.captain1_id
        state.captain2_id = captain_pair.captain2_id
        state.captain1_rating = captain_pair.captain1_rating
        state.captain2_rating = captain_pair.captain2_rating
        state.draft_channel_id = interaction.channel_id

        # Perform coinflip
        coinflip_winner_id = self.draft_service.coinflip(
            captain_pair.captain1_id, captain_pair.captain2_id
        )
        state.coinflip_winner_id = coinflip_winner_id
        state.phase = DraftPhase.WINNER_CHOICE

        # Get captain names for display
        captain1_name = await self._get_member_name(interaction.guild, captain_pair.captain1_id)
        captain2_name = await self._get_member_name(interaction.guild, captain_pair.captain2_id)
        winner_name = (
            captain1_name if coinflip_winner_id == captain_pair.captain1_id else captain2_name
        )

        # Build coinflip result embed
        embed = discord.Embed(
            title="🎲 IMMORTAL DRAFT",
            color=discord.Color.gold(),
        )

        embed.add_field(
            name="👑 Captains",
            value=(
                f"**{captain1_name}** ({captain_pair.captain1_rating:.0f})\n"
                f"**{captain2_name}** ({captain_pair.captain2_rating:.0f})"
            ),
            inline=False,
        )

        embed.add_field(
            name="🎰 Coinflip Result",
            value=f"**{winner_name}** won the coinflip!",
            inline=False,
        )

        embed.add_field(
            name="Next Step",
            value=f"{winner_name}, choose whether to pick **Side** or **Hero Pick Order**.",
            inline=False,
        )

        if pool_result.excluded_ids:
            excluded_names = []
            for eid in pool_result.excluded_ids:
                name = await self._get_member_name(interaction.guild, eid)
                excluded_names.append(name)
            embed.add_field(
                name="📤 Excluded Players",
                value=", ".join(excluded_names),
                inline=False,
            )

        # Send with winner choice buttons
        view = WinnerChoiceView(self, guild_id, coinflip_winner_id)
        message = await interaction.followup.send(embed=embed, view=view)

        # Store message ID for later updates
        state.draft_message_id = message.id

        return True

    # ========================================================================
    # /startdraft command
    # ========================================================================

    @app_commands.command(
        name="startdraft",
        description="Start an Immortal Draft with captain-based player selection",
    )
    @app_commands.describe(
        captain1="(Optional) Specify first captain",
        captain2="(Optional) Specify second captain",
    )
    async def startdraft(
        self,
        interaction: discord.Interaction,
        captain1: discord.Member | None = None,
        captain2: discord.Member | None = None,
    ):
        """Start an Immortal Draft session."""
        guild_id = interaction.guild_id
        logger.info(
            f"Startdraft command: User {interaction.user.id} ({interaction.user}) "
            f"in guild {guild_id}, captain1={captain1}, captain2={captain2}"
        )

        await safe_defer(interaction)

        # Check for existing draft
        if self.draft_state_manager.has_active_draft(guild_id):
            await interaction.followup.send(
                "❌ A draft is already in progress. Use `/restartdraft` to restart it first.",
                ephemeral=True,
            )
            return

        # Check for pending match from shuffle
        if self.match_service:
            pending_match = self.match_service.get_last_shuffle(guild_id)
            if pending_match:
                await interaction.followup.send(
                    "❌ There's an active shuffled match that needs to be recorded! "
                    "Use `/record` first before starting a draft.",
                    ephemeral=True,
                )
                return

        # Check lobby
        lobby = self.lobby_manager.get_lobby()
        if not lobby:
            await interaction.followup.send(
                "❌ No active lobby. Use `/lobby` to create one first.",
                ephemeral=True,
            )
            return

        lobby_player_ids = list(lobby.players)
        if len(lobby_player_ids) < LOBBY_READY_THRESHOLD:
            await interaction.followup.send(
                f"❌ Need at least {LOBBY_READY_THRESHOLD} players in lobby. "
                f"Currently have {len(lobby_player_ids)}.",
                ephemeral=True,
            )
            return

        # Validate specified captains are in lobby
        specified_captain1_id = captain1.id if captain1 else None
        specified_captain2_id = captain2.id if captain2 else None

        if specified_captain1_id and specified_captain1_id not in lobby_player_ids:
            await interaction.followup.send(
                f"❌ {captain1.display_name} is not in the lobby.",
                ephemeral=True,
            )
            return

        if specified_captain2_id and specified_captain2_id not in lobby_player_ids:
            await interaction.followup.send(
                f"❌ {captain2.display_name} is not in the lobby.",
                ephemeral=True,
            )
            return

        if (
            specified_captain1_id
            and specified_captain2_id
            and specified_captain1_id == specified_captain2_id
        ):
            await interaction.followup.send(
                "❌ Cannot specify the same player as both captains.",
                ephemeral=True,
            )
            return

        # Execute draft with specified captains (normal captain eligibility check)
        await self._execute_draft(
            interaction,
            guild_id,
            lobby,
            force_random_captains=False,
            specified_captain1_id=specified_captain1_id,
            specified_captain2_id=specified_captain2_id,
        )

    # ========================================================================
    # Choice Handlers
    # ========================================================================

    async def handle_winner_chose_side(self, interaction: discord.Interaction, guild_id: int):
        """Handle when coinflip winner chooses to pick side."""
        state = self.draft_state_manager.get_state(guild_id)
        if not state:
            await interaction.response.send_message("❌ Draft not found.", ephemeral=True)
            return

        state.winner_choice_type = "side"
        state.phase = DraftPhase.WINNER_SIDE_CHOICE

        winner_name = await self._get_member_name(interaction.guild, state.coinflip_winner_id)

        embed = discord.Embed(
            title="🎲 IMMORTAL DRAFT",
            description=f"**{winner_name}** chose to pick **Side**.",
            color=discord.Color.blue(),
        )
        embed.add_field(
            name="Choose Your Side",
            value=f"{winner_name}, pick Radiant or Dire.",
            inline=False,
        )

        view = SideChoiceView(self, guild_id, state.coinflip_winner_id, is_winner=True)
        await interaction.response.edit_message(embed=embed, view=view)

    async def handle_winner_chose_hero_pick(self, interaction: discord.Interaction, guild_id: int):
        """Handle when coinflip winner chooses to pick hero order."""
        state = self.draft_state_manager.get_state(guild_id)
        if not state:
            await interaction.response.send_message("❌ Draft not found.", ephemeral=True)
            return

        state.winner_choice_type = "hero_pick"
        state.phase = DraftPhase.WINNER_HERO_CHOICE

        winner_name = await self._get_member_name(interaction.guild, state.coinflip_winner_id)

        embed = discord.Embed(
            title="🎲 IMMORTAL DRAFT",
            description=f"**{winner_name}** chose to pick **Hero Pick Order**.",
            color=discord.Color.blue(),
        )
        embed.add_field(
            name="Choose Hero Pick Order",
            value=f"{winner_name}, pick First or Second hero pick (in-game).",
            inline=False,
        )

        view = HeroPickOrderView(self, guild_id, state.coinflip_winner_id, is_winner=True)
        await interaction.response.edit_message(embed=embed, view=view)

    async def handle_side_choice(
        self,
        interaction: discord.Interaction,
        guild_id: int,
        side: str,
        is_winner: bool,
    ):
        """Handle side choice (Radiant/Dire)."""
        state = self.draft_state_manager.get_state(guild_id)
        if not state:
            await interaction.response.send_message("❌ Draft not found.", ephemeral=True)
            return

        chooser_id = interaction.user.id
        chooser_name = await self._get_member_name(interaction.guild, chooser_id)

        # Determine other captain (loser)
        loser_id = (
            state.captain2_id
            if state.coinflip_winner_id == state.captain1_id
            else state.captain1_id
        )
        loser_name = await self._get_member_name(interaction.guild, loser_id)

        if is_winner:
            state.winner_choice_value = side
            # Assign sides
            if side == "radiant":
                state.radiant_captain_id = chooser_id
                state.dire_captain_id = loser_id
            else:
                state.dire_captain_id = chooser_id
                state.radiant_captain_id = loser_id

            # Loser now picks hero pick order
            state.phase = DraftPhase.LOSER_CHOICE

            embed = discord.Embed(
                title="🎲 IMMORTAL DRAFT",
                description=f"**{chooser_name}** chose **{side.title()}**.",
                color=discord.Color.green() if side == "radiant" else discord.Color.red(),
            )
            embed.add_field(
                name="Choose Hero Pick Order",
                value=f"{loser_name}, pick First or Second hero pick (in-game).",
                inline=False,
            )

            view = HeroPickOrderView(self, guild_id, loser_id, is_winner=False)
            await interaction.response.edit_message(embed=embed, view=view)

        else:
            # Loser is choosing side (winner picked hero order first)
            state.loser_choice_value = side
            if side == "radiant":
                state.radiant_captain_id = chooser_id
                state.dire_captain_id = state.coinflip_winner_id
            else:
                state.dire_captain_id = chooser_id
                state.radiant_captain_id = state.coinflip_winner_id

            # Move to player draft order phase
            await self._show_player_draft_order_choice(interaction, guild_id, state)

    async def handle_hero_pick_choice(
        self,
        interaction: discord.Interaction,
        guild_id: int,
        pick_order: str,
        is_winner: bool,
    ):
        """Handle hero pick order choice (First/Second)."""
        state = self.draft_state_manager.get_state(guild_id)
        if not state:
            await interaction.response.send_message("❌ Draft not found.", ephemeral=True)
            return

        chooser_id = interaction.user.id
        chooser_name = await self._get_member_name(interaction.guild, chooser_id)

        # Determine other captain (loser)
        loser_id = (
            state.captain2_id
            if state.coinflip_winner_id == state.captain1_id
            else state.captain1_id
        )
        loser_name = await self._get_member_name(interaction.guild, loser_id)

        if is_winner:
            state.winner_choice_value = pick_order
            # Loser now picks side
            state.phase = DraftPhase.LOSER_CHOICE

            embed = discord.Embed(
                title="🎲 IMMORTAL DRAFT",
                description=f"**{chooser_name}** chose **{pick_order.title()} Pick** for heroes.",
                color=discord.Color.blue(),
            )
            embed.add_field(
                name="Choose Your Side",
                value=f"{loser_name}, pick Radiant or Dire.",
                inline=False,
            )

            view = SideChoiceView(self, guild_id, loser_id, is_winner=False)
            await interaction.response.edit_message(embed=embed, view=view)

        else:
            # Loser is choosing hero order (winner picked side first)
            state.loser_choice_value = pick_order
            # Move to player draft order phase
            await self._show_player_draft_order_choice(interaction, guild_id, state)

    async def _show_player_draft_order_choice(
        self,
        interaction: discord.Interaction,
        guild_id: int,
        state: DraftState,
    ):
        """Show player draft order choice to lower-rated captain."""
        # Assign hero pick order based on choices
        # winner_choice_type tells us what winner chose to pick
        # winner_choice_value is the winner's choice
        # loser_choice_value is the loser's choice

        if state.winner_choice_type == "side":
            # Winner picked side, loser picked hero order
            hero_pick_choice = state.loser_choice_value
        else:
            # Winner picked hero order, loser picked side
            hero_pick_choice = state.winner_choice_value

        # The captain who picked hero order chose first or second
        # We need to figure out who gets first pick
        if state.winner_choice_type == "hero_pick":
            # Winner chose hero order
            hero_order_chooser = state.coinflip_winner_id
        else:
            # Loser chose hero order (winner chose side)
            hero_order_chooser = (
                state.captain2_id
                if state.coinflip_winner_id == state.captain1_id
                else state.captain1_id
            )

        # Determine hero pick order
        if hero_pick_choice == "first":
            # The chooser gets first pick
            if hero_order_chooser == state.radiant_captain_id:
                state.radiant_hero_pick_order = 1
                state.dire_hero_pick_order = 2
            else:
                state.dire_hero_pick_order = 1
                state.radiant_hero_pick_order = 2
        else:
            # The chooser gets second pick
            if hero_order_chooser == state.radiant_captain_id:
                state.radiant_hero_pick_order = 2
                state.dire_hero_pick_order = 1
            else:
                state.dire_hero_pick_order = 2
                state.radiant_hero_pick_order = 1

        state.phase = DraftPhase.PLAYER_DRAFT_ORDER

        lower_captain_id = state.lower_rated_captain_id
        lower_captain_name = await self._get_member_name(interaction.guild, lower_captain_id)

        radiant_name = await self._get_member_name(interaction.guild, state.radiant_captain_id)
        dire_name = await self._get_member_name(interaction.guild, state.dire_captain_id)

        first_hero_team = "Radiant" if state.radiant_hero_pick_order == 1 else "Dire"

        embed = discord.Embed(
            title="🎲 IMMORTAL DRAFT - Pre-Draft Setup Complete",
            color=discord.Color.purple(),
        )

        embed.add_field(
            name="Teams",
            value=(f"🟢 **Radiant**: {radiant_name}\n🔴 **Dire**: {dire_name}"),
            inline=False,
        )

        embed.add_field(
            name="Hero Draft Order (In-Game)",
            value=f"**{first_hero_team}** picks first",
            inline=False,
        )

        embed.add_field(
            name="Player Draft Order",
            value=f"{lower_captain_name} (lower-rated) chooses first or second pick for player draft.",
            inline=False,
        )

        view = PlayerDraftOrderView(self, guild_id, lower_captain_id)
        await interaction.response.edit_message(embed=embed, view=view)

    async def handle_player_draft_order_choice(
        self,
        interaction: discord.Interaction,
        guild_id: int,
        order: str,
    ):
        """Handle player draft order choice."""
        state = self.draft_state_manager.get_state(guild_id)
        if not state:
            await interaction.response.send_message("❌ Draft not found.", ephemeral=True)
            return

        lower_captain_id = state.lower_rated_captain_id
        higher_captain_id = state.higher_rated_captain_id

        if order == "first":
            state.player_draft_first_captain_id = lower_captain_id
        else:
            state.player_draft_first_captain_id = higher_captain_id

        state.phase = DraftPhase.DRAFTING

        # Add captains to their teams
        state.radiant_player_ids.append(state.radiant_captain_id)
        state.dire_player_ids.append(state.dire_captain_id)

        # Show draft UI
        await self._show_draft_ui(interaction, guild_id, state)

    async def _show_draft_ui(
        self,
        interaction: discord.Interaction,
        guild_id: int,
        state: DraftState,
        is_edit: bool = True,
    ):
        """Show the drafting UI with player pick buttons."""
        embed = await self._build_draft_embed(interaction.guild, state)

        # Get available players for buttons, sorted by rating descending
        available_ids = state.available_player_ids
        available_players = self.player_repo.get_by_ids(available_ids)
        available_players.sort(key=lambda p: p.glicko_rating or 1500.0, reverse=True)

        # Create view with player buttons
        view = DraftingView(
            cog=self,
            guild_id=guild_id,
            available_players=available_players,
            current_captain_id=state.current_captain_id,
        )

        if is_edit:
            await interaction.response.edit_message(embed=embed, view=view)
        else:
            await interaction.edit_original_response(embed=embed, view=view)

    async def _build_draft_embed(
        self,
        guild: discord.Guild | None,
        state: DraftState,
    ) -> discord.Embed:
        """Build the draft status embed."""
        current_captain_id = state.current_captain_id
        current_captain_name = (
            await self._get_member_name(guild, current_captain_id) if current_captain_id else "N/A"
        )
        current_team = state.current_captain_team or "N/A"
        picks_remaining = state.picks_remaining_this_turn

        # Get all players for role display
        all_player_ids = state.radiant_player_ids + state.dire_player_ids
        all_players = {p.discord_id: p for p in self.player_repo.get_by_ids(all_player_ids)}

        radiant_captain_name = await self._get_member_name(guild, state.radiant_captain_id)
        dire_captain_name = await self._get_member_name(guild, state.dire_captain_id)

        # Helper to get roles as numbers for a player
        def get_role_nums(player_id: int) -> str:
            player = all_players.get(player_id)
            if player and player.preferred_roles:
                return format_roles(player.preferred_roles)
            return ""

        # Build team displays with aligned rows
        radiant_lines = []
        for pid in state.radiant_player_ids:
            is_cap = pid == state.radiant_captain_id
            player = all_players.get(pid)
            name = radiant_captain_name if is_cap else (player.name if player else f"P{pid}")
            rating = player.glicko_rating if player and player.glicko_rating else 1500.0
            roles = get_role_nums(pid)
            radiant_lines.append(format_player_row(is_cap, name, rating, roles))

        dire_lines = []
        for pid in state.dire_player_ids:
            is_cap = pid == state.dire_captain_id
            player = all_players.get(pid)
            name = dire_captain_name if is_cap else (player.name if player else f"P{pid}")
            rating = player.glicko_rating if player and player.glicko_rating else 1500.0
            roles = get_role_nums(pid)
            dire_lines.append(format_player_row(is_cap, name, rating, roles))

        radiant_display = "\n".join(radiant_lines) if radiant_lines else "Empty"
        dire_display = "\n".join(dire_lines) if dire_lines else "Empty"

        # Build available players display with roles and preferences, sorted by rating
        available_ids = state.available_player_ids
        available_players = self.player_repo.get_by_ids(available_ids)
        # Sort by rating descending
        available_players.sort(key=lambda p: p.glicko_rating or 1500.0, reverse=True)
        available_display = []
        for p in available_players:
            rating = p.glicko_rating or 1500.0
            roles = format_roles(p.preferred_roles)
            pref = state.side_preferences.get(p.discord_id)
            # Preference indicator at end
            if pref == "radiant":
                pref_indicator = " 🟢"
            elif pref == "dire":
                pref_indicator = " 🔴"
            else:
                pref_indicator = ""
            available_display.append(f"{p.name} ({rating:.0f}) {roles}{pref_indicator}")

        # Color based on current team
        if current_team == "radiant":
            color = discord.Color.green()
        elif current_team == "dire":
            color = discord.Color.red()
        else:
            color = discord.Color.gold()

        # Pick count indicator
        pick_text = (
            f"{picks_remaining} pick{'s' if picks_remaining != 1 else ''}"
            if picks_remaining > 0
            else "No picks"
        )

        # Build draft order visual with progress
        # Snake order: [0, 1, 1, 0, 0, 1, 1, 0] where 0 = first picker's team
        first_is_radiant = state.player_draft_first_captain_id == state.radiant_captain_id
        current_pick = state.current_pick_index

        order_parts = []
        for i, pick_team in enumerate(SNAKE_DRAFT_ORDER):
            if first_is_radiant:
                icon = "🟢" if pick_team == 0 else "🔴"
            else:
                icon = "🔴" if pick_team == 0 else "🟢"

            if i < current_pick:
                # Completed pick - grey dot
                order_parts.append("⚫")
            elif i == current_pick:
                # Current pick - highlight with brackets
                order_parts.append(f"[{icon}]")
            else:
                # Future pick
                order_parts.append(icon)

        draft_order_visual = " ".join(order_parts)

        embed = discord.Embed(
            title="⚔️ IMMORTAL DRAFT - Player Selection",
            description=f"**Draft Order:** {draft_order_visual}\n\n**{current_captain_name}** ({current_team.title()}) to pick! ({pick_text})",
            color=color,
        )

        embed.add_field(
            name=f"🟢 Radiant ({len(state.radiant_player_ids)}/5)",
            value=radiant_display,
            inline=True,
        )

        embed.add_field(
            name=f"🔴 Dire ({len(state.dire_player_ids)}/5)",
            value=dire_display,
            inline=True,
        )

        embed.add_field(
            name="\u200b",
            value="\u200b",
            inline=True,
        )

        embed.add_field(
            name=f"📋 Available ({len(available_display)})",
            value="\n".join(available_display) if available_display else "None",
            inline=False,
        )

        # Excluded players section
        if state.excluded_player_ids:
            excluded_players = self.player_repo.get_by_ids(state.excluded_player_ids)
            excluded_players.sort(key=lambda p: p.glicko_rating or 1500.0, reverse=True)
            excluded_display = []
            for p in excluded_players:
                rating = p.glicko_rating or 1500.0
                roles = format_roles(p.preferred_roles)
                excluded_display.append(f"{p.name} ({rating:.0f}) {roles}".strip())

            excluded_note = "Excluded players are prioritized in future games."
            embed.add_field(
                name=f"🚫 Excluded ({len(excluded_display)})",
                value="\n".join(excluded_display) + f"\n\n*{excluded_note}*",
                inline=False,
            )

        first_hero_team = "Radiant" if state.radiant_hero_pick_order == 1 else "Dire"
        embed.add_field(
            name="ℹ️ In-Game Hero Draft",
            value=f"**{first_hero_team}** picks first",
            inline=False,
        )

        embed.set_footer(
            text=f"Pick #{state.current_pick_index + 1}/{DRAFT_TOTAL_PICKS} | Click a player name to pick"
        )

        return embed

    async def _update_draft_message(
        self,
        guild: discord.Guild | None,
        channel_id: int,
        message_id: int,
        state: DraftState,
    ):
        """Update the draft message with current state."""
        try:
            channel = self.bot.get_channel(channel_id)
            if not channel:
                return

            message = await channel.fetch_message(message_id)
            if not message:
                return

            embed = await self._build_draft_embed(guild, state)

            if state.phase == DraftPhase.COMPLETE:
                # Draft complete - remove buttons
                await message.edit(embed=embed, view=None)
            else:
                # Still drafting - update with new buttons, sorted by rating
                available_ids = state.available_player_ids
                available_players = self.player_repo.get_by_ids(available_ids)
                available_players.sort(key=lambda p: p.glicko_rating or 1500.0, reverse=True)

                view = DraftingView(
                    cog=self,
                    guild_id=state.guild_id,
                    available_players=available_players,
                    current_captain_id=state.current_captain_id,
                )
                await message.edit(embed=embed, view=view)

        except Exception as e:
            logger.error(f"Failed to update draft message: {e}")

    # ========================================================================
    # Player Draft Handlers
    # ========================================================================

    async def handle_player_pick(
        self,
        interaction: discord.Interaction,
        guild_id: int,
        player_id: int,
    ):
        """Handle when a captain picks a player."""
        state = self.draft_state_manager.get_state(guild_id)
        if not state:
            await interaction.response.send_message("❌ Draft not found.", ephemeral=True)
            return

        if state.phase != DraftPhase.DRAFTING:
            await interaction.response.send_message(
                "❌ Draft is not in picking phase.", ephemeral=True
            )
            return

        # Check if it's the user's turn
        current_captain_id = state.current_captain_id
        if interaction.user.id != current_captain_id:
            captain_name = await self._get_member_name(interaction.guild, current_captain_id)
            await interaction.response.send_message(
                f"❌ It's not your turn! Waiting for **{captain_name}** to pick.",
                ephemeral=True,
            )
            return

        # Attempt to pick the player
        success = state.pick_player(player_id)
        if not success:
            await interaction.response.send_message(
                "❌ Cannot pick that player. They may already be picked.",
                ephemeral=True,
            )
            return

        picked_name = await self._get_member_name(interaction.guild, player_id)
        picker_team = "Radiant" if player_id in state.radiant_player_ids else "Dire"

        logger.info(
            f"Draft pick: {interaction.user} picked {picked_name} for {picker_team} "
            f"(pick #{state.current_pick_index}/{DRAFT_TOTAL_PICKS}, guild {guild_id})"
        )

        # Check if draft is complete
        if state.phase == DraftPhase.COMPLETE:
            # Create pending match for betting and recording
            match_created = await self._create_pending_match(guild_id, state)

            if not match_created:
                # Failed to create pending match - don't reset lobby, show error
                embed = discord.Embed(
                    title="⚠️ Draft Complete - Match Creation Failed",
                    description=(
                        "The draft completed but the match could not be created.\n"
                        "This may be a configuration issue. Please contact an admin.\n\n"
                        "The lobby has been preserved."
                    ),
                    color=discord.Color.orange(),
                )
                await interaction.response.edit_message(embed=embed, view=None)
                self.draft_state_manager.clear_state(guild_id)
                return

            # Get pending state for betting display
            pending_state = self.match_service.get_last_shuffle(guild_id)

            # Decay exclusion counts for included players (same as shuffle mode)
            included_player_ids = state.radiant_player_ids + state.dire_player_ids
            for pid in included_player_ids:
                self.player_repo.decay_exclusion_count(pid)

            # Save thread ID before resetting lobby
            lobby_service = getattr(self.bot, "lobby_service", None)
            thread_id = lobby_service.get_lobby_thread_id() if lobby_service else None

            # Reset lobby only after successful match creation
            self.lobby_manager.reset_lobby()

            embed = await self._build_draft_complete_embed(interaction.guild, state, pending_state)
            await interaction.response.edit_message(embed=embed, view=None)

            # Post to match thread and ping players
            await self._post_to_match_thread(state, embed, thread_id)

            # Clear draft state
            self.draft_state_manager.clear_state(guild_id)
            return

        # Update the draft UI
        await self._show_draft_ui(interaction, guild_id, state, is_edit=True)

    async def handle_side_preference(
        self,
        interaction: discord.Interaction,
        guild_id: int,
        side: str | None,
    ):
        """Handle when a player sets their side preference."""
        state = self.draft_state_manager.get_state(guild_id)
        if not state:
            await interaction.response.send_message("❌ Draft not found.", ephemeral=True)
            return

        if state.phase != DraftPhase.DRAFTING:
            await interaction.response.send_message(
                "❌ Draft is not in picking phase.", ephemeral=True
            )
            return

        player_id = interaction.user.id

        # Check if player is available (not yet picked)
        if player_id not in state.available_player_ids:
            await interaction.response.send_message(
                "❌ You have already been picked or are not in this draft.",
                ephemeral=True,
            )
            return

        # Set preference
        success = state.set_side_preference(player_id, side)
        if not success:
            await interaction.response.send_message(
                "❌ Could not set preference.",
                ephemeral=True,
            )
            return

        # Acknowledge with ephemeral message
        if side:
            await interaction.response.send_message(
                f"✅ Preference set to **{side.title()}**",
                ephemeral=True,
            )
        else:
            await interaction.response.send_message(
                "✅ Preference cleared",
                ephemeral=True,
            )

        # Update the draft message to show new preference
        if state.draft_message_id and state.draft_channel_id:
            await self._update_draft_message(
                interaction.guild,
                state.draft_channel_id,
                state.draft_message_id,
                state,
            )

    async def _create_pending_match(
        self,
        guild_id: int,
        state: DraftState,
    ) -> bool:
        """
        Create a pending match from draft result for betting and recording.

        Returns:
            True if pending match was created successfully, False otherwise.
        """
        if not self.match_service:
            logger.warning("No match_service available, skipping pending match creation")
            return False

        now_ts = int(time.time())

        # Determine first pick team based on hero pick order
        first_pick_team = "Radiant" if state.radiant_hero_pick_order == 1 else "Dire"

        # Calculate approximate team values for parity display
        radiant_players = self.player_repo.get_by_ids(state.radiant_player_ids)
        dire_players = self.player_repo.get_by_ids(state.dire_player_ids)

        radiant_value = sum(p.glicko_rating or 1500.0 for p in radiant_players)
        dire_value = sum(p.glicko_rating or 1500.0 for p in dire_players)
        value_diff = abs(radiant_value - dire_value)

        # Calculate win probability for stake pool using Glicko-2 expected outcome
        radiant_mean = radiant_value / len(radiant_players) if radiant_players else 1500.0
        dire_mean = dire_value / len(dire_players) if dire_players else 1500.0

        # Calculate RMS RD for each team
        radiant_rds = [p.glicko_rd or 350.0 for p in radiant_players]
        dire_rds = [p.glicko_rd or 350.0 for p in dire_players]
        radiant_rms_rd = math.sqrt(sum(rd**2 for rd in radiant_rds) / len(radiant_rds)) if radiant_rds else 350.0
        dire_rms_rd = math.sqrt(sum(rd**2 for rd in dire_rds) / len(dire_rds)) if dire_rds else 350.0

        # Calculate expected win probability
        radiant_win_prob = CamaRatingSystem.expected_outcome(
            radiant_mean, radiant_rms_rd, dire_mean, dire_rms_rd
        )

        # Create shuffle state dict compatible with match_service
        shuffle_state = {
            "radiant_team_ids": state.radiant_player_ids,
            "dire_team_ids": state.dire_player_ids,
            "excluded_player_ids": state.excluded_player_ids,
            "radiant_team": None,  # No Team objects for draft
            "dire_team": None,
            "radiant_roles": [],  # No role assignments for draft
            "dire_roles": [],
            "radiant_value": radiant_value,
            "dire_value": dire_value,
            "value_diff": value_diff,
            "first_pick_team": first_pick_team,
            "record_submissions": {},
            "shuffle_timestamp": now_ts,
            "bet_lock_until": now_ts + BET_LOCK_SECONDS,
            "shuffle_message_jump_url": None,
            "shuffle_message_id": state.draft_message_id,
            "shuffle_channel_id": state.draft_channel_id,
            "betting_mode": "pool",  # Default to pool mode for drafts
            "is_draft": True,  # Mark as draft for any special handling
            "stake_radiant_win_prob": radiant_win_prob,  # For stake pool calculations
        }

        # Create stakes for player stake pool (draft only)
        stake_service = getattr(self.bot, "stake_service", None)
        stake_result = None
        if stake_service and stake_service.is_enabled():
            stake_result = stake_service.create_stakes_for_draft(
                guild_id=guild_id,
                radiant_ids=state.radiant_player_ids,
                dire_ids=state.dire_player_ids,
                excluded_ids=state.excluded_player_ids,
                radiant_win_prob=radiant_win_prob,
                stake_time=now_ts,
            )
            shuffle_state["stake_pool_created"] = True
            shuffle_state["stake_result"] = stake_result
            logger.info(
                f"Created stake pool for draft: radiant_win_prob={radiant_win_prob:.2f}, "
                f"radiant_payout={stake_result.get('radiant_payout_if_win')}, "
                f"dire_payout={stake_result.get('dire_payout_if_win')}"
            )
        else:
            shuffle_state["stake_pool_created"] = False

        self.match_service.set_last_shuffle(guild_id, shuffle_state)
        self.match_service._persist_match_state(guild_id, shuffle_state)

        logger.info(
            f"Created pending match from draft for guild {guild_id}: "
            f"Radiant={state.radiant_player_ids}, Dire={state.dire_player_ids}"
        )
        return True

    async def _build_draft_complete_embed(
        self,
        guild: discord.Guild | None,
        state: DraftState,
        pending_state: dict | None = None,
    ) -> discord.Embed:
        """Build the draft complete embed."""
        # Get all players for role display
        all_player_ids = state.radiant_player_ids + state.dire_player_ids
        all_players = {p.discord_id: p for p in self.player_repo.get_by_ids(all_player_ids)}

        # Helper to get roles as numbers for a player
        def get_role_nums(player_id: int) -> str:
            player = all_players.get(player_id)
            if player and player.preferred_roles:
                return format_roles(player.preferred_roles)
            return ""

        radiant_captain_name = await self._get_member_name(guild, state.radiant_captain_id)
        dire_captain_name = await self._get_member_name(guild, state.dire_captain_id)

        # Calculate team rating totals
        radiant_sum = sum(
            (all_players.get(pid).glicko_rating or 1500.0)
            for pid in state.radiant_player_ids
            if all_players.get(pid)
        )
        dire_sum = sum(
            (all_players.get(pid).glicko_rating or 1500.0)
            for pid in state.dire_player_ids
            if all_players.get(pid)
        )
        value_diff = abs(radiant_sum - dire_sum)

        # Build team displays with aligned rows
        radiant_lines = []
        for pid in state.radiant_player_ids:
            is_cap = pid == state.radiant_captain_id
            player = all_players.get(pid)
            name = radiant_captain_name if is_cap else (player.name if player else f"P{pid}")
            rating = player.glicko_rating if player and player.glicko_rating else 1500.0
            roles = get_role_nums(pid)
            radiant_lines.append(format_player_row(is_cap, name, rating, roles))

        dire_lines = []
        for pid in state.dire_player_ids:
            is_cap = pid == state.dire_captain_id
            player = all_players.get(pid)
            name = dire_captain_name if is_cap else (player.name if player else f"P{pid}")
            rating = player.glicko_rating if player and player.glicko_rating else 1500.0
            roles = get_role_nums(pid)
            dire_lines.append(format_player_row(is_cap, name, rating, roles))

        radiant_display = "\n".join(radiant_lines) if radiant_lines else "Empty"
        dire_display = "\n".join(dire_lines) if dire_lines else "Empty"

        first_hero_team = "Radiant" if state.radiant_hero_pick_order == 1 else "Dire"
        first_pick_emoji = "🟢" if first_hero_team == "Radiant" else "🔴"

        embed = discord.Embed(
            title="⚔️ IMMORTAL DRAFT - Complete!",
            description=f"{first_pick_emoji} **{first_hero_team}** picks first in hero draft",
            color=discord.Color.gold(),
        )

        # Team fields with rating totals in header
        embed.add_field(
            name=f"🟢 Radiant ({radiant_sum:.0f})",
            value=radiant_display,
            inline=True,
        )

        embed.add_field(
            name=f"🔴 Dire ({dire_sum:.0f})",
            value=dire_display,
            inline=True,
        )

        embed.add_field(
            name="\u200b",
            value="\u200b",
            inline=True,
        )

        # Excluded players section
        if state.excluded_player_ids:
            excluded_players = self.player_repo.get_by_ids(state.excluded_player_ids)
            excluded_players.sort(key=lambda p: p.glicko_rating or 1500.0, reverse=True)
            excluded_display = []
            for p in excluded_players:
                rating = p.glicko_rating or 1500.0
                roles = format_roles(p.preferred_roles)
                excluded_display.append(f"{p.name} ({rating:.0f}) {roles}".strip())

            excluded_note = "Excluded players are prioritized in future games."
            embed.add_field(
                name=f"🚫 Excluded ({len(excluded_display)})",
                value="\n".join(excluded_display) + f"\n\n*{excluded_note}*",
                inline=False,
            )

        # Balance stats
        balance_info = f"**Value diff:** {value_diff:.0f}"
        embed.add_field(name="📊 Balance", value=balance_info, inline=False)

        # Betting info (if betting service available)
        betting_service = getattr(self.bot, "betting_service", None)
        if betting_service and pending_state:
            betting_mode = pending_state.get("betting_mode", "pool")

            # Betting instructions
            if betting_mode == "pool":
                betting_note = (
                    f"`/bet <Radiant/Dire> <amount>` (min {JOPACOIN_MIN_BET} {JOPACOIN_EMOTE}). "
                    "Pool betting: odds determined by bet distribution."
                )
            else:
                betting_note = (
                    f"`/bet <Radiant/Dire> <amount>` (min {JOPACOIN_MIN_BET} {JOPACOIN_EMOTE}). "
                    "1:1 payouts."
                )
            embed.add_field(name="📝 How to Bet", value=betting_note, inline=False)

            # Blind bets summary (if any)
            blind_bets = pending_state.get("blind_bets_result")
            if blind_bets and blind_bets.get("created", 0) > 0:
                blind_note = (
                    f"**Auto-liquidity:** {blind_bets['created']} players contributed blind bets\n"
                    f"🟢 Radiant: {blind_bets['total_radiant']} {JOPACOIN_EMOTE} | "
                    f"🔴 Dire: {blind_bets['total_dire']} {JOPACOIN_EMOTE}"
                )
                embed.add_field(name="🎲 Blind Bets", value=blind_note, inline=False)

            # Player Stake Pool (draft mode only) - Dual Pool System
            stake_service = getattr(self.bot, "stake_service", None)
            if stake_service and stake_service.is_enabled() and pending_state.get("stake_pool_created"):
                stake_result = pending_state.get("stake_result", {})
                radiant_win_prob = stake_result.get("radiant_win_prob", 0.5)
                dire_win_prob = 1.0 - radiant_win_prob
                radiant_auto = stake_result.get("radiant_auto", 25)
                dire_auto = stake_result.get("dire_auto", 25)
                pool_size = stake_result.get("pool_size", 50)
                radiant_mult = stake_result.get("initial_radiant_multiplier", 2.0)
                dire_mult = stake_result.get("initial_dire_multiplier", 2.0)
                excluded_count = len(state.excluded_player_ids)
                excluded_payout_radiant = stake_result.get("excluded_payout_if_radiant_wins", 10)
                excluded_payout_dire = stake_result.get("excluded_payout_if_dire_wins", 10)

                # Determine favored team
                if radiant_win_prob > 0.52:
                    radiant_label = f"🟢 Radiant ({radiant_win_prob*100:.0f}% favored)"
                    dire_label = f"🔴 Dire ({dire_win_prob*100:.0f}% underdog)"
                elif dire_win_prob > 0.52:
                    radiant_label = f"🟢 Radiant ({radiant_win_prob*100:.0f}% underdog)"
                    dire_label = f"🔴 Dire ({dire_win_prob*100:.0f}% favored)"
                else:
                    radiant_label = f"🟢 Radiant ({radiant_win_prob*100:.0f}%)"
                    dire_label = f"🔴 Dire ({dire_win_prob*100:.0f}%)"

                # Player Pool: auto-liquidity + optional player bets
                player_pool_note = (
                    f"**{pool_size} {JOPACOIN_EMOTE} auto-liquidity** (Glicko-weighted)\n"
                    f"{radiant_label}: {radiant_auto:.1f} {JOPACOIN_EMOTE} → **{radiant_mult:.2f}x**\n"
                    f"{dire_label}: {dire_auto:.1f} {JOPACOIN_EMOTE} → **{dire_mult:.2f}x**\n"
                    f"*Players: `/bet` on your team to add to pool*"
                )
                if excluded_count > 0:
                    player_pool_note += (
                        f"\nExcluded ({excluded_count}): **{excluded_payout_radiant}** if Rad / "
                        f"**{excluded_payout_dire}** if Dire"
                    )

                embed.add_field(name="🎯 Player Stake Pool", value=player_pool_note, inline=False)

            # Spectator Pool (draft mode only)
            spectator_pool_service = getattr(self.bot, "spectator_pool_service", None)
            if spectator_pool_service and pending_state.get("is_draft"):
                spectator_note = (
                    "Spectators: `/bet <team> <amount>`\n"
                    "90% to winning bettors, 10% to winning players\n"
                    "Pool: 🟢 0 JC | 🔴 0 JC"
                )
                embed.add_field(name="🎲 Spectator Pool", value=spectator_note, inline=False)

            # Current wagers
            guild_id = state.guild_id
            totals = betting_service.get_pot_odds(guild_id, pending_state=pending_state)
            lock_until = pending_state.get("bet_lock_until")
            wager_field_name, wager_field_value = format_betting_display(
                totals["radiant"], totals["dire"], betting_mode, lock_until
            )
            embed.add_field(name=wager_field_name, value=wager_field_value, inline=False)

        embed.set_footer(text="Use /record to record the match result when finished.")

        return embed

    # ========================================================================
    # Helper Methods
    # ========================================================================

    async def _post_to_match_thread(
        self, state: DraftState, embed: discord.Embed, thread_id: int | None = None
    ) -> None:
        """Post draft complete embed to match thread and ping players."""
        if not thread_id:
            return

        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            # Update thread name to show draft complete
            try:
                await thread.edit(name="🔒 Draft Complete - Awaiting Results")
            except discord.HTTPException:
                pass  # Rate limit on thread name changes

            # Post the draft complete embed
            await thread.send(embed=embed)

            # Ping all drafted players (both teams)
            all_player_ids = state.radiant_player_ids + state.dire_player_ids
            real_player_ids = [pid for pid in all_player_ids if pid > 0]
            if real_player_ids:
                mentions = " ".join(f"<@{pid}>" for pid in real_player_ids)
                await thread.send(f"{mentions}\nPlayers, please take your starting positions!")

            # Lock the thread
            try:
                await thread.edit(locked=True)
            except discord.HTTPException:
                pass

        except Exception as exc:
            logger.warning(f"Failed to post to match thread: {exc}")

    async def _handle_draft_timeout(self, guild_id: int) -> None:
        """Handle draft timeout by clearing state and updating the message."""
        state = self.draft_state_manager.get_state(guild_id)
        if not state:
            return

        logger.info(f"Draft timed out for guild {guild_id} in phase {state.phase.value}")

        # Clear the draft state
        self.draft_state_manager.clear_state(guild_id)

        # Try to update the message to show timeout
        if state.draft_message_id and state.draft_channel_id:
            try:
                channel = self.bot.get_channel(state.draft_channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(state.draft_channel_id)

                message = await channel.fetch_message(state.draft_message_id)

                embed = discord.Embed(
                    title="⏰ Draft Timed Out",
                    description=(
                        "The draft was automatically cancelled due to inactivity.\n\n"
                        "The lobby has been preserved. Use `/startdraft` to begin a new draft."
                    ),
                    color=discord.Color.orange(),
                )
                await message.edit(embed=embed, view=None)

            except Exception as exc:
                logger.warning(f"Failed to update message after draft timeout: {exc}")

    async def _get_member_name(self, guild: discord.Guild | None, user_id: int) -> str:
        """Get display name for a user ID."""
        if guild:
            member = guild.get_member(user_id)
            if member:
                return member.display_name
        # Fallback to username from player repo
        player = self.player_repo.get_by_id(user_id)
        if player:
            return player.name
        return f"Unknown ({user_id})"


async def setup(bot: commands.Bot):
    player_repo = getattr(bot, "player_repo", None)
    lobby_manager = getattr(bot, "lobby_manager", None)
    draft_state_manager = getattr(bot, "draft_state_manager", None)
    draft_service = getattr(bot, "draft_service", None)
    match_service = getattr(bot, "match_service", None)

    if player_repo is None:
        logger.warning("DraftCommands: player_repo not found on bot, skipping cog load")
        return
    if lobby_manager is None:
        logger.warning("DraftCommands: lobby_manager not found on bot, skipping cog load")
        return

    # Create draft services if not already on bot
    if draft_state_manager is None:
        draft_state_manager = DraftStateManager()
        bot.draft_state_manager = draft_state_manager
        logger.info("DraftCommands: Created DraftStateManager")

    if draft_service is None:
        draft_service = DraftService()
        bot.draft_service = draft_service
        logger.info("DraftCommands: Created DraftService")

    await bot.add_cog(
        DraftCommands(
            bot,
            player_repo,
            lobby_manager,
            draft_state_manager,
            draft_service,
            match_service,
        )
    )
