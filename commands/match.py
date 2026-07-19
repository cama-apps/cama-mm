"""
Match commands: /shuffle and /record.
"""

import asyncio
import functools
import logging
import random
import time

import discord
from discord import app_commands
from discord.ext import commands

from config import (
    BET_LAST_CALL_OFFSET,
    BET_REMINDER_OFFSETS,
    BOMB_POT_CHANCE,
    ENRICHMENT_RETRY_DELAYS,
    FIRST_GAME_BONUS,
    JOPACOIN_MIN_BET,
    OPENSKILL_SHUFFLE_CHANCE,
    STREAMING_BONUS,
)
from services.jopat_post_match import JopatPostMatchContext
from services.lobby_service import LobbyService
from services.match_discovery_service import MatchDiscoveryService
from services.match_service import MatchService
from services.permissions import has_admin_permission
from utils.embeds import create_enriched_match_embed
from utils.formatting import (
    FROGLING_EMOTE,
    JOPACOIN_EMOTE,
    ROLE_EMOJIS,
    ROLE_NAMES,
    format_betting_display,
    get_player_display_name,
)
from utils.guild import normalize_guild_id
from utils.interaction_safety import safe_defer, update_lobby_message_closed
from utils.match_views import EnrichedMatchView
from utils.neon_helpers import _delete_after as _neon_delete_after
from utils.neon_helpers import get_neon_service, send_neon_result
from utils.pin_helpers import safe_unpin_all_bot_messages
from utils.rate_limiter import GLOBAL_RATE_LIMITER
from utils.region import REGION_NAMES, resolve_region, summarize_region
from utils.streaming import get_streaming_player_ids

logger = logging.getLogger("cama_bot.commands.match")

# Strong references to fire-and-forget tasks. The event loop only holds
# tasks weakly, so an unreferenced task can be garbage-collected mid-run.
_background_tasks: set[asyncio.Task] = set()


def _retain_background_task(task: asyncio.Task) -> asyncio.Task:
    """Hold a strong reference to a fire-and-forget task until it finishes."""
    _background_tasks.add(task)
    task.add_done_callback(_background_tasks.discard)
    return task

DISCORD_MESSAGE_MAX_CHARS = 2000


def _chunk_discord_content(
    content: str, max_chars: int = DISCORD_MESSAGE_MAX_CHARS
) -> list[str]:
    """Split message content at natural boundaries within Discord's limit."""
    if max_chars <= 0:
        raise ValueError("max_chars must be positive")

    remaining = content.strip()
    chunks: list[str] = []

    while len(remaining) > max_chars:
        split_at = remaining.rfind("\n", 0, max_chars + 1)
        separator_length = 1

        if split_at <= 0:
            split_at = remaining.rfind(" ", 0, max_chars + 1)

        if split_at <= 0:
            split_at = max_chars
            separator_length = 0

        chunks.append(remaining[:split_at].rstrip())
        remaining = remaining[split_at + separator_length :].lstrip()

    if remaining:
        chunks.append(remaining)
    return chunks


def priority_key(player) -> tuple[float, float]:
    """Priority key for conditional player selection: higher rating first,
    then lower RD."""
    rating = player.glicko_rating if player.glicko_rating else 1500.0
    rd = player.glicko_rd if player.glicko_rd else 350.0
    return (rating, -rd)


def select_players_for_shuffle(
    regular_player_ids: list[int],
    regular_players: list,
    conditional_player_ids: list[int],
    conditional_players: list,
) -> tuple[list[int], list, list[int], list[int]]:
    """Pick the shuffle roster, filling from the conditional queue (highest
    priority first) when the regular queue is short of 10.

    Returns ``(player_ids, players, included_conditional_ids,
    excluded_conditional_ids)``.
    """
    player_ids = list(regular_player_ids)
    players = list(regular_players)
    included_conditional_ids: list[int] = []
    excluded_conditional_ids: list[int] = []

    regular_count = len(regular_player_ids)
    if regular_count < 10:
        conditional_pairs = list(zip(conditional_player_ids, conditional_players))
        conditional_pairs.sort(key=lambda x: priority_key(x[1]), reverse=True)

        slots_available = 10 - regular_count
        for cid, cplayer in conditional_pairs[:slots_available]:
            player_ids.append(cid)
            players.append(cplayer)
            included_conditional_ids.append(cid)

        excluded_conditional_ids = [cid for cid, _ in conditional_pairs[slots_available:]]
    else:
        excluded_conditional_ids = list(conditional_player_ids)

    return player_ids, players, included_conditional_ids, excluded_conditional_ids


class MatchCommands(commands.Cog):
    """Slash commands for shuffling teams and recording results."""

    def __init__(
        self,
        bot: commands.Bot,
        lobby_service: LobbyService,
        match_service: MatchService,
        player_service,
        *,
        guild_config_service=None,
        bankruptcy_repo=None,
    ):
        self.bot = bot
        self.lobby_service = lobby_service
        self.match_service = match_service
        self.player_service = player_service
        self.guild_config_service = guild_config_service
        # bankruptcy_repo kept for embed builders that need bankruptcy status display
        self.bankruptcy_repo = bankruptcy_repo
        # Track scheduled betting reminder tasks per guild for cleanup
        self._betting_tasks_by_guild = {}

    async def _update_channel_message_closed(
        self, reason: str = "Match Aborted", guild_id: int | None = None
    ) -> None:
        """Update the channel message embed to show lobby/match is closed."""
        await update_lobby_message_closed(self.bot, self.lobby_service, reason, guild_id=guild_id)

    async def _lock_lobby_thread(
        self,
        guild_id: int | None,
        shuffle_embed: discord.Embed = None,
        included_player_ids: list[int] | None = None,
        pending_match_id: int | None = None,
    ) -> None:
        """Lock the lobby thread when shuffle occurs and post shuffle results."""
        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id, guild_id=guild_id
        )
        if not thread_id:
            return

        thread_shuffle_msg = None
        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            # Update thread name to show shuffled state
            try:
                await thread.edit(name="🔒 Shuffled - Awaiting Results")
            except discord.HTTPException:
                pass  # Rate limit on thread name changes

            # Post shuffle embed to thread
            if shuffle_embed:
                thread_shuffle_msg = await thread.send(embed=shuffle_embed)
            else:
                thread_shuffle_msg = await thread.send(
                    "🔀 **Teams have been shuffled!**\nUse `/record` to record the match result."
                )

            # Ping included players in thread (subscribes them to thread notifications)
            if included_player_ids:
                # Filter out fake users (negative IDs)
                real_player_ids = [pid for pid in included_player_ids if pid > 0]
                if real_player_ids:
                    mentions = " ".join(f"<@{pid}>" for pid in real_player_ids)
                    await thread.send(f"{mentions}\nPlayers, take your starting positions")

            # Lock the thread so users can't post or use buttons
            try:
                await thread.edit(locked=True)
            except discord.Forbidden:
                pass
        except Exception as exc:
            logger.warning(f"Failed to lock lobby thread: {exc}")

        # Store thread info even when posting failed so /record can still
        # rename + archive the thread (thread_message_id stays unset then).
        await asyncio.to_thread(
            self.match_service.set_shuffle_message_info,
            guild_id,
            message_id=None,
            channel_id=None,
            thread_message_id=thread_shuffle_msg.id if thread_shuffle_msg else None,
            thread_id=thread_id,
            pending_match_id=pending_match_id,
        )

    async def _finalize_lobby_thread(
        self, guild_id: int | None, winning_result: str, *,
        thread_id: int | None = None,
        pending_match_id: int | None = None,
    ) -> None:
        """Post results to lobby thread and archive it."""
        # Use provided thread_id only - do NOT fallback to lobby_service
        # as that could return a different match's thread in concurrent match scenarios
        if not thread_id:
            pending_state = await asyncio.to_thread(
                self.match_service.get_last_shuffle,
                guild_id,
                pending_match_id=pending_match_id,
            )
            thread_id = pending_state.thread_shuffle_thread_id if pending_state else None
        if not thread_id:
            # No thread_id means we can't safely update any thread
            logger.debug("No thread_id available for finalize_lobby_thread")
            return

        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            # Post result summary
            winner = "Radiant" if winning_result == "radiant" else "Dire"
            await thread.send(f"🏆 **Match Complete - {winner} Victory!**")

            # Wait before archiving so players can see the result
            await asyncio.sleep(15)

            # Update thread name and archive
            try:
                await thread.edit(name=f"✅ Match Complete - {winner} Won", archived=True)
            except discord.Forbidden:
                await thread.edit(archived=True)
        except Exception as exc:
            logger.warning(f"Failed to finalize lobby thread: {exc}")

    async def _abort_lobby_thread(self, guild_id: int | None, pending_match_id: int | None = None) -> None:
        """Archive the lobby thread when match is aborted."""
        # Get thread_id from pending state (must be called before clear_last_shuffle)
        # Do NOT fallback to lobby_service as that could return a different match's thread
        pending_state = await asyncio.to_thread(
            self.match_service.get_last_shuffle, guild_id, pending_match_id
        )
        thread_id = pending_state.thread_shuffle_thread_id if pending_state else None
        if not thread_id:
            # No thread_id means we can't safely update any thread
            logger.debug("No thread_id available for abort_lobby_thread")
            return

        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            await thread.send("🚫 **Match Aborted** - All bets have been refunded.")

            try:
                await thread.edit(name="🚫 Match Aborted", archived=True)
            except discord.Forbidden:
                await thread.edit(archived=True)
        except Exception as exc:
            logger.warning(f"Failed to abort lobby thread: {exc}")

    def _format_team_lines(self, team, roles, player_ids, players, guild, balancing_system="glicko"):
        """Return formatted lines with roles and ratings for a team."""
        lines = []
        rating_system = self.match_service.rating_system
        name_to_id = {pl.name: pid for pid, pl in zip(player_ids, players)}

        for player, role in zip(team.players, roles):
            pid = name_to_id.get(player.name)
            display_name = (
                get_player_display_name(player, discord_id=pid, guild=guild) if pid else player.name
            )
            is_on_role = player.preferred_roles and role in player.preferred_roles
            role_emoji = ROLE_EMOJIS.get(role, "")
            role_name = ROLE_NAMES.get(role, role)
            rating = (
                rating_system.rating_to_display(player.glicko_rating)
                if player.glicko_rating
                else "N/A"
            )

            name_part = f"**{display_name}**" if is_on_role else display_name
            warn = "" if is_on_role else " ⚠️"
            if balancing_system == "jopacoin":
                lines.append(f"{role_emoji} {name_part} ({role_name}) [{rating}] 💰 {player.jopacoin_balance}{warn}")
            else:
                lines.append(f"{role_emoji} {name_part} ({role_name}) [{rating}]{warn}")
        return lines

    def _format_region_mix(self, label: str, team) -> str:
        """Return a compact region summary for one shuffled side."""
        votes = [resolve_region(player) for player in team.players]
        use = votes.count("USE")
        usw = votes.count("USW")
        unset = len(votes) - use - usw
        parts = []
        if usw:
            parts.append(f"{REGION_NAMES['USW']}: {usw}")
        if use:
            parts.append(f"{REGION_NAMES['USE']}: {use}")
        if unset:
            parts.append(f"Unset: {unset}")
        return f"**{label}:** " + (", ".join(parts) if parts else REGION_NAMES["USW"])

    def _get_notable_winner(
        self,
        match_id: int,
        winning_ids: list[int],
    ) -> tuple[int | None, dict]:
        """
        Pick the most notable winner for post-match commentary.

        Args:
            match_id: The match ID to look up
            winning_ids: List of winning player discord IDs

        Returns: (discord_id, event_details) or (None, {})

        Priority:
        1. Underdog hero (won with <45% expected prob)
        2. Biggest rating gainer
        3. Random winner (fallback)
        """
        if not winning_ids:
            return None, {}

        # Get rating history for this match
        rating_data = self.match_service.get_rating_history_for_match(match_id)
        winner_data = [r for r in rating_data if r["discord_id"] in winning_ids]

        if not winner_data:
            # Fallback: random winner, no details
            return random.choice(winning_ids), {}

        # Check for underdog (lowest expected win probability who won)
        underdog = min(
            winner_data,
            key=lambda x: x.get("expected_team_win_prob") or 0.5,
        )
        underdog_prob = underdog.get("expected_team_win_prob") or 0.5
        if underdog_prob < 0.45:
            rating_change = (underdog.get("rating") or 0) - (underdog.get("rating_before") or 0)
            return underdog["discord_id"], {
                "rating_change": rating_change,
                "expected_win_prob": underdog_prob,
                "is_underdog": True,
            }

        # Otherwise: biggest rating gainer
        best_gainer = max(
            winner_data,
            key=lambda x: (x.get("rating") or 0) - (x.get("rating_before") or 0),
        )
        rating_change = (best_gainer.get("rating") or 0) - (best_gainer.get("rating_before") or 0)
        return best_gainer["discord_id"], {
            "rating_change": rating_change,
            "expected_win_prob": best_gainer.get("expected_team_win_prob"),
            "is_big_gainer": True,
        }

    @app_commands.command(name="shuffle", description="Create balanced teams from lobby")
    @app_commands.describe(
        mode="Team-shuffle mode",
        rating_system="Rating system for team balancing (experimental)",
    )
    @app_commands.choices(
        mode=[
            app_commands.Choice(name="Balanced (default)", value="balanced"),
            app_commands.Choice(name="Region Split (US West vs US East)", value="region"),
        ],
        rating_system=[
            app_commands.Choice(name="Glicko-2 (default)", value="glicko"),
            app_commands.Choice(name="OpenSkill (experimental)", value="openskill"),
            app_commands.Choice(name="Jopacoin Balance", value="jopacoin"),
        ]
    )
    async def shuffle(
        self,
        interaction: discord.Interaction,
        mode: app_commands.Choice[str] | None = None,
        rating_system: app_commands.Choice[str] | None = None,
    ):
        logger.info(f"Shuffle command: User {interaction.user.id} ({interaction.user})")
        guild = interaction.guild if hasattr(interaction, "guild") else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="shuffle",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=2,
            per_seconds=30,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/shuffle` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        guild_id = guild.id if guild else None

        # Acquire shuffle lock to prevent race conditions
        lobby_manager = self.lobby_service.lobby_manager

        # Check for stale lock (>60s) and release if needed
        await asyncio.to_thread(lobby_manager._check_stale_lock, guild_id)

        shuffle_lock = await asyncio.to_thread(lobby_manager.get_shuffle_lock, guild_id)
        try:
            await asyncio.wait_for(shuffle_lock.acquire(), timeout=0.5)
        except TimeoutError:
            await interaction.followup.send(
                "A shuffle is already in progress. Please wait for it to complete.",
                ephemeral=True,
            )
            return

        await asyncio.to_thread(lobby_manager.record_lock_acquired, guild_id)
        try:
            await self._execute_shuffle(interaction, guild, guild_id, rating_system, mode)
        finally:
            await asyncio.to_thread(lobby_manager.clear_lock_time, guild_id)
            shuffle_lock.release()

    async def _validate_shuffle_preconditions(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Pre-flight checks for ``/shuffle``. Sends the error message to the user
        and returns ``None`` if anything fails; otherwise returns the active lobby."""
        player_match = await asyncio.to_thread(
            self.match_service.state_service.get_pending_match_for_player,
            guild_id,
            interaction.user.id,
        )
        if player_match:
            pending_match_id = player_match.get("pending_match_id")
            jump_url = player_match.get("shuffle_message_jump_url")
            message_text = f"❌ You're already in a pending match (Match #{pending_match_id})!"
            if jump_url:
                message_text += f" [View your match]({jump_url}) and use `/record` to complete it first."
            else:
                message_text += " Use `/record` to complete it first."
            await interaction.followup.send(message_text, ephemeral=True)
            return None

        draft_state_manager = getattr(self.bot, "draft_state_manager", None)
        if draft_state_manager and await asyncio.to_thread(
            draft_state_manager.has_active_draft, guild_id
        ):
            state = await asyncio.to_thread(draft_state_manager.get_state, guild_id)
            user_id = interaction.user.id

            is_captain = state and user_id in (state.captain1_id, state.captain2_id)
            is_admin = user_id in getattr(self.bot, "admin_user_ids", set())
            if not is_admin and interaction.guild:
                member = interaction.guild.get_member(user_id)
                if member and member.guild_permissions.administrator:
                    is_admin = True

            if is_captain or is_admin:
                msg = (
                    "❌ There's an active Immortal Draft in progress! "
                    "Use `/draft restart` to restart it or complete the draft first."
                )
            else:
                msg = (
                    "❌ There's an active Immortal Draft in progress! "
                    "Please wait for it to complete."
                )
            await interaction.followup.send(msg, ephemeral=True)
            return None

        lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)
        if not lobby:
            await interaction.followup.send(
                "❌ No active lobby. Use `/lobby` to create one!", ephemeral=True
            )
            return None

        user_id = interaction.user.id
        is_in_lobby = (
            user_id in getattr(lobby, "players", set())
            or user_id in getattr(lobby, "conditional_players", set())
        )
        if not is_in_lobby and not has_admin_permission(interaction):
            await interaction.followup.send(
                "❌ Only admins or players in the current lobby can shuffle.",
                ephemeral=True,
            )
            return None

        regular_count = lobby.get_player_count()
        conditional_count = lobby.get_conditional_count()
        total_count = lobby.get_total_count()
        if total_count < 10:
            await interaction.followup.send(
                f"❌ Need at least 10 players in lobby. Currently {total_count}/10 "
                f"({regular_count} regular, {conditional_count} conditional).",
                ephemeral=True,
            )
            return None

        return lobby

    async def _select_shuffle_roster(
        self, lobby, guild_id: int | None
    ) -> tuple[list[int], list, list[int], list[int]]:
        """Pick exactly 10 players for the shuffle, filling from the conditional
        queue when the regular queue is short. Returns ``(player_ids, players,
        conditional_included, excluded_conditional_ids)``."""
        player_ids, players = await asyncio.to_thread(
            self.lobby_service.get_lobby_players, lobby, guild_id
        )

        all_conditional_ids, all_conditional_players = await asyncio.to_thread(
            self.lobby_service.get_conditional_players,
            lobby,
            guild_id,
        )

        return select_players_for_shuffle(
            player_ids,
            players,
            all_conditional_ids,
            all_conditional_players,
        )

    async def _execute_shuffle(
        self,
        interaction: discord.Interaction,
        guild: discord.Guild | None,
        guild_id: int | None,
        rating_system: app_commands.Choice[str] | None,
        shuffle_mode: app_commands.Choice[str] | None = None,
    ):
        """Execute the shuffle logic. Called within the shuffle lock."""
        lobby = await self._validate_shuffle_preconditions(interaction, guild_id)
        if lobby is None:
            return

        regular_count = lobby.get_player_count()

        # 15 or more regular (non-conditional) players → force Immortal Draft
        if regular_count >= 15:
            draft_cog = self.bot.get_cog("DraftCommands")
            if not draft_cog:
                await interaction.followup.send(
                    "❌ Draft system not available. Please contact an admin.",
                )
                return

            try:
                # _execute_draft sends its own specific error on every failure
                # path, so inspecting the return value here would only let us
                # add a misleading duplicate. The except below covers the
                # unexpected-exception path.
                await draft_cog._execute_draft(interaction, guild_id, lobby)
            except Exception as e:
                logger.error(f"Draft auto-redirect failed: {e}", exc_info=True)
                # Clean up any partially-created draft state so it doesn't block
                # future shuffles/drafts. _execute_draft already clears on failure,
                # but guard here too in case the raise path changes.
                if hasattr(draft_cog, "draft_state_manager"):
                    await asyncio.to_thread(draft_cog.draft_state_manager.clear_state, guild_id)
                await interaction.followup.send(
                    "❌ Immortal Draft failed to start. Error has been logged. "
                    "Use `/shuffle` or `/draft start` to try again."
                )
            return

        (
            player_ids,
            players,
            conditional_player_ids_included,
            excluded_conditional_ids,
        ) = await self._select_shuffle_roster(lobby, guild_id)

        # Check if any of the players to be shuffled are already in a pending match
        pending_player_ids = await asyncio.to_thread(
            self.match_service.state_service.get_all_pending_player_ids, guild_id
        )
        players_in_pending = set(player_ids) & pending_player_ids
        if players_in_pending:
            # Get player names for the error message
            blocked_names = []
            for pid in players_in_pending:
                player_obj = await asyncio.to_thread(self.player_service.get_player, pid, guild_id)
                if player_obj:
                    display_name = get_player_display_name(player_obj, discord_id=pid, guild=guild)
                    blocked_names.append(display_name)
            blocked_list = ", ".join(blocked_names[:5])  # Show max 5 names
            if len(players_in_pending) > 5:
                blocked_list += f" and {len(players_in_pending) - 5} more"
            await interaction.followup.send(
                f"❌ Cannot shuffle: {len(players_in_pending)} players are already in a pending match: {blocked_list}\n"
                f"They must complete their current match with `/record` first.",
                ephemeral=True,
            )
            return

        # `guild` and `guild_id` already computed before the match check
        mode = "pool"  # house mode retired; new matches always use pool betting
        team_mode = shuffle_mode.value if shuffle_mode else "balanced"
        rs = rating_system.value if rating_system else "glicko"
        if rating_system is None and random.random() < OPENSKILL_SHUFFLE_CHANCE:
            rs = "openskill"

        is_openskill_shuffle = rs == "openskill"

        try:
            result = await asyncio.to_thread(
                functools.partial(self.match_service.shuffle_players,
                    player_ids, guild_id=guild_id, betting_mode=mode, rating_system=rs,
                    shuffle_mode=team_mode,
                    excluded_conditional_ids=excluded_conditional_ids)
            )
        except ValueError as exc:
            logger.warning(f"Shuffle validation error: {exc}", exc_info=True)
            await interaction.followup.send(f"❌ {exc}", ephemeral=True)
            return
        except Exception as exc:
            logger.error(f"Shuffle error: {exc}", exc_info=True)
            await interaction.followup.send(
                "❌ Unexpected error while shuffling. Please try again.", ephemeral=True
            )
            return

        radiant_team = result["radiant_team"]
        dire_team = result["dire_team"]
        radiant_roles = result["radiant_roles"]
        dire_roles = result["dire_roles"]
        value_diff = result["value_diff"]
        goodness_score = result.get("goodness_score")
        # Use actual rating system (may have fallen back from openskill to glicko)
        rs = result.get("balancing_rating_system", rs)
        is_openskill_shuffle = is_openskill_shuffle and rs == "openskill"
        # Sum of raw ratings (without off-role multipliers) for display
        use_os = rs == "openskill"
        use_jc = rs == "jopacoin"
        radiant_sum = sum(
            player.get_value(self.match_service.use_glicko, use_openskill=use_os, use_jopacoin=use_jc)
            for player in radiant_team.players
        )
        dire_sum = sum(
            player.get_value(self.match_service.use_glicko, use_openskill=use_os, use_jopacoin=use_jc)
            for player in dire_team.players
        )
        first_pick_team = result["first_pick_team"]
        excluded_ids = result["excluded_ids"]
        pending_match_id = result.get("pending_match_id")

        # Determine if this is a bomb pot match (~10% chance)
        is_bomb_pot = random.random() < BOMB_POT_CHANCE

        # Store bomb pot status in pending state and persist to DB
        pending_state = await asyncio.to_thread(
            self.match_service.get_last_shuffle,
            guild_id,
            pending_match_id=pending_match_id,
        )
        if pending_state:
            pending_state.is_bomb_pot = is_bomb_pot
            pending_state.is_openskill_shuffle = is_openskill_shuffle
            # Persist to DB so bomb pot and openskill shuffle survive bot restart
            await asyncio.to_thread(
                self.match_service._persist_match_state, guild_id, pending_state
            )

        if is_bomb_pot:
            logger.info(f"💣 BOMB POT triggered for guild {guild_id}")
        if is_openskill_shuffle:
            logger.info(f"⚗️ OPENSKILL SHUFFLE triggered for guild {guild_id}")

        # Create auto-liquidity blind bets for pool mode
        blind_bets_result = None
        spectator_bets_result = None
        if mode == "pool":
            betting_service = getattr(self.bot, "betting_service", None)
            if betting_service:
                try:
                    # Get IDs and timestamp from the saved pending state
                    logger.debug(f"Creating blind bets for pending_match_id={pending_match_id}")
                    pending_state = await asyncio.to_thread(
                        self.match_service.get_last_shuffle,
                        guild_id,
                        pending_match_id=pending_match_id,
                    )
                    state_pmid = pending_state.pending_match_id if pending_state else None
                    logger.debug(f"Blind bets: state={pending_state is not None}, state_pmid={state_pmid}")
                    blind_bets_result = await asyncio.to_thread(
                        functools.partial(betting_service.create_auto_blind_bets,
                            guild_id=guild_id,
                            radiant_ids=pending_state.radiant_team_ids,
                            dire_ids=pending_state.dire_team_ids,
                            shuffle_timestamp=pending_state.shuffle_timestamp,
                            is_bomb_pot=is_bomb_pot,
                            pending_match_id=pending_state.pending_match_id,
                        )
                    )
                    spectator_bets_result = await asyncio.to_thread(
                        functools.partial(
                            betting_service.create_auto_spectator_bets,
                            guild_id=guild_id,
                            radiant_ids=pending_state.radiant_team_ids,
                            dire_ids=pending_state.dire_team_ids,
                            shuffle_timestamp=pending_state.shuffle_timestamp,
                            pending_match_id=pending_state.pending_match_id,
                        )
                    )
                    if blind_bets_result["created"] > 0:
                        logger.info(
                            f"Created {blind_bets_result['created']} blind bets: "
                            f"Radiant={blind_bets_result['total_radiant']}, "
                            f"Dire={blind_bets_result['total_dire']}"
                            f"{' (BOMB POT)' if is_bomb_pot else ''}"
                        )
                        # Neon Degen Terminal: Bomb pot easter egg
                        if is_bomb_pot:
                            try:
                                neon = get_neon_service(self.bot)
                                if neon:
                                    pool_total = blind_bets_result['total_radiant'] + blind_bets_result['total_dire']
                                    bomb_result = await neon.on_bomb_pot(
                                        guild_id, pool_total, blind_bets_result['created']
                                    )
                                    if bomb_result:
                                        await send_neon_result(interaction, bomb_result)
                            except Exception as e:
                                logger.debug(f"neon on_bomb_pot error: {e}")
                    if spectator_bets_result["created"] > 0:
                        logger.info(
                            f"Created {spectator_bets_result['created']} spectator auto-wagers: "
                            f"Radiant={spectator_bets_result['total_radiant']}, "
                            f"Dire={spectator_bets_result['total_dire']}"
                        )
                except Exception as exc:
                    logger.warning(f"Failed to create automatic bets: {exc}", exc_info=True)

        # Streaming bonus: award +1 JC to all lobby players (including excluded) who are Go Live
        # Awarded at both shuffle and record time (intentional: rewards continuous streaming)
        streaming_bonus_names = []
        if guild and hasattr(guild, "get_member") and STREAMING_BONUS > 0:
            all_lobby_ids = list(player_ids) + list(excluded_conditional_ids)
            streaming_ids = get_streaming_player_ids(guild, all_lobby_ids)
            if streaming_ids:
                betting_svc = getattr(self.bot, "betting_service", None)
                if betting_svc:
                    await asyncio.to_thread(
                        betting_svc.award_streaming_bonus, list(streaming_ids), guild_id
                    )
                for sid in streaming_ids:
                    player_obj = await asyncio.to_thread(
                        self.player_service.get_player, sid, guild_id
                    )
                    if player_obj:
                        streaming_bonus_names.append(
                            get_player_display_name(player_obj, discord_id=sid, guild=guild)
                        )
                logger.info(
                    f"Streaming bonus (+{STREAMING_BONUS} JC) at shuffle: {streaming_ids}"
                )

        radiant_lines = self._format_team_lines(
            radiant_team, radiant_roles, player_ids, players, guild, balancing_system=rs
        )
        dire_lines = self._format_team_lines(
            dire_team, dire_roles, player_ids, players, guild, balancing_system=rs
        )

        # Sort by role number for cleaner view
        radiant_sorted = sorted(zip(radiant_roles, radiant_lines), key=lambda x: int(x[0]))
        dire_sorted = sorted(zip(dire_roles, dire_lines), key=lambda x: int(x[0]))

        head_to_head = []
        for (_r_role, r_line), (_d_role, d_line) in zip(radiant_sorted, dire_sorted):
            head_to_head.append(f"{r_line}  |  {d_line}")

        # Build embed title with match ID and bomb pot banner if applicable
        match_label = f"Match #{pending_match_id} — " if pending_match_id else ""
        if is_bomb_pot:
            title_mode = "Region Team Shuffle" if team_mode == "region" else "Balanced Team Shuffle"
            embed_title = f"💣 BOMB POT 💣 {match_label}{title_mode}"
            embed_color = discord.Color.orange()
        else:
            title_mode = "Region Team Shuffle" if team_mode == "region" else "Balanced Team Shuffle"
            embed_title = f"{match_label}{title_mode}"
            embed_color = discord.Color.blue()

        embed = discord.Embed(title=embed_title, color=embed_color)
        first_pick_emoji = "🟢" if first_pick_team == "Radiant" else "🔴"
        embed.add_field(
            name=f"🟢 Radiant ({radiant_sum:.0f})  |  🔴 Dire ({dire_sum:.0f})",
            value=f"{first_pick_emoji} **First Pick: {first_pick_team}**\n\n"
            + "\n".join(head_to_head),
            inline=False,
        )

        embed.add_field(
            name="🌎 Recommended Server",
            value=summarize_region(radiant_team.players + dire_team.players),
            inline=False,
        )

        if team_mode == "region":
            embed.add_field(
                name="🌎 Region Split",
                value=(
                    f"{self._format_region_mix('Radiant', radiant_team)}\n"
                    f"{self._format_region_mix('Dire', dire_team)}"
                ),
                inline=False,
            )

        radiant_off = radiant_team.get_off_role_count()
        dire_off = dire_team.get_off_role_count()
        goodness_display = f"{goodness_score:.1f}" if goodness_score is not None else "N/A"

        # Show which rating system was used for balancing
        balancing_system = result.get("balancing_rating_system", "glicko")
        if balancing_system == "jopacoin":
            rating_system_display = "💰 Jopacoin Balance"
        elif balancing_system == "openskill":
            rating_system_display = "⚗️ OpenSkill (experimental)"
        else:
            rating_system_display = "📊 Glicko-2"

        balance_info = (
            f"**Shuffle mode:** {'Region Split' if team_mode == 'region' else 'Balanced'}\n"
            f"**Balanced with:** {rating_system_display}\n"
            f"**Goodness score:** {goodness_display} (lower = better)\n"
            f"**Value diff:** {value_diff:.0f}\n"
            f"**Off-role players:** Radiant: {radiant_off}, Dire: {dire_off} (Total: {radiant_off + dire_off})"
        )
        # Build excluded list - combine regular excluded + excluded conditional players
        all_excluded_names = []
        if excluded_ids:
            for pid in excluded_ids:
                player_obj = await asyncio.to_thread(self.player_service.get_player, pid, guild_id)
                if player_obj:
                    display_name = get_player_display_name(player_obj, discord_id=pid, guild=guild)
                    all_excluded_names.append(display_name)
                else:
                    all_excluded_names.append(f"Unknown({pid})")

        # Add excluded conditional players with frogling emoji
        if excluded_conditional_ids:
            for pid in excluded_conditional_ids:
                player_obj = await asyncio.to_thread(self.player_service.get_player, pid, guild_id)
                if player_obj:
                    display_name = get_player_display_name(player_obj, discord_id=pid, guild=guild)
                    all_excluded_names.append(f"{FROGLING_EMOTE} {display_name}")
                else:
                    all_excluded_names.append(f"{FROGLING_EMOTE} Unknown({pid})")

        if all_excluded_names:
            balance_info += f"\n**Excluded:** {', '.join(all_excluded_names)}"

        # Show conditional players who were pulled into the shuffle
        if conditional_player_ids_included:
            conditional_names = []
            for pid in conditional_player_ids_included:
                player_obj = await asyncio.to_thread(self.player_service.get_player, pid, guild_id)
                if player_obj:
                    display_name = get_player_display_name(player_obj, discord_id=pid, guild=guild)
                    conditional_names.append(display_name)
                else:
                    conditional_names.append(f"Unknown({pid})")
            balance_info += f"\n{FROGLING_EMOTE} **Pulled from conditional:** {', '.join(conditional_names)}"
        if streaming_bonus_names:
            balance_info += f"\n📺 **Streaming bonus (+{STREAMING_BONUS} {JOPACOIN_EMOTE}):** {', '.join(streaming_bonus_names)}"
        embed.add_field(name="📊 Balance", value=balance_info, inline=False)

        # Betting instructions (house mode retired; all matches use pool betting)
        betting_note = (
            f"`/bet <Radiant/Dire> <amount>` (min {JOPACOIN_MIN_BET} {JOPACOIN_EMOTE}). "
            "Pool betting: odds determined by bet distribution."
        )
        embed.add_field(name="📝 How to Bet", value=betting_note, inline=False)

        # Show blind bet summary if any were created
        if blind_bets_result and blind_bets_result["created"] > 0:
            if is_bomb_pot:
                blind_pct = f"{blind_bets_result.get('percentage', 0) * 100:g}%"
                blind_note = (
                    f"💣 **BOMB POT:** All 10 players ante'd in! ({blind_pct} + 10 {JOPACOIN_EMOTE} ante)\n"
                    f"🟢 Radiant: {blind_bets_result['total_radiant']} {JOPACOIN_EMOTE} | "
                    f"🔴 Dire: {blind_bets_result['total_dire']} {JOPACOIN_EMOTE}\n"
                    f"_+1 bonus {JOPACOIN_EMOTE} for ALL players this match!_"
                )
                embed.add_field(name="💣 Bomb Pot Stakes", value=blind_note, inline=False)
            else:
                blind_note = (
                    f"**Auto-liquidity:** {blind_bets_result['created']} players contributed blind bets\n"
                    f"🟢 Radiant: {blind_bets_result['total_radiant']} {JOPACOIN_EMOTE} | "
                    f"🔴 Dire: {blind_bets_result['total_dire']} {JOPACOIN_EMOTE}"
                )
                embed.add_field(name="🎲 Blind Bets", value=blind_note, inline=False)

        if spectator_bets_result and spectator_bets_result["created"] > 0:
            spectator_pct = f"{spectator_bets_result.get('percentage', 0) * 100:g}%"
            spectator_note = (
                f"**Rich spectator auto-wagers:** {spectator_bets_result['created']} spectators wagered {spectator_pct} of net worth\n"
                f"🟢 Radiant: {spectator_bets_result['total_radiant']} {JOPACOIN_EMOTE} | "
                f"🔴 Dire: {spectator_bets_result['total_dire']} {JOPACOIN_EMOTE}"
            )
            embed.add_field(name="💸 Spectator Auto-Wagers", value=spectator_note, inline=False)

        # Current wagers display
        betting_service = getattr(self.bot, "betting_service", None)
        totals = {"radiant": 0, "dire": 0}
        lock_until = None
        seed_radiant = seed_dire = seed_bonus = 0
        if betting_service:
            pending_state = await asyncio.to_thread(
                self.match_service.get_last_shuffle,
                guild_id,
                pending_match_id=pending_match_id,
            )
            totals = await asyncio.to_thread(
                betting_service.get_pot_odds, guild_id, pending_state=pending_state
            )
            lock_until = pending_state.bet_lock_until if pending_state else None
            if pending_state:
                seed_radiant = pending_state.bet_seed_radiant
                seed_dire = pending_state.bet_seed_dire
                seed_bonus = pending_state.bet_seed_bonus

        wager_field_name, wager_field_value = format_betting_display(
            totals["radiant"],
            totals["dire"],
            mode,
            lock_until,
            seed_radiant=seed_radiant,
            seed_dire=seed_dire,
            seed_bonus=seed_bonus,
        )
        embed.add_field(name=wager_field_name, value=wager_field_value, inline=False)

        # Add match quality indicators to footer (subtle display)
        glicko_prob = result.get("glicko_radiant_win_prob", 0.5)
        os_prob = result.get("openskill_radiant_win_prob", 0.5)
        if pending_match_id:
            embed.set_footer(text=f"Match #{pending_match_id} | {glicko_prob:.2f} {os_prob:.2f}")
        else:
            embed.set_footer(text=f"{glicko_prob:.2f} {os_prob:.2f}")

        await self._finalize_shuffle(
            interaction=interaction,
            guild_id=guild_id,
            embed=embed,
            pending_match_id=pending_match_id,
        )

    async def _finalize_shuffle(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        embed: discord.Embed,
        pending_match_id: int | None,
    ) -> None:
        """Post the shuffle embed, persist its location, schedule reminders,
        lock the lobby thread, unpin, and reset the lobby."""
        lobby_channel_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_channel_id, guild_id=guild_id
        )
        message = None
        if lobby_channel_id:
            try:
                channel = self.bot.get_channel(lobby_channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(lobby_channel_id)
                message = await channel.send(embed=embed)
            except Exception as exc:
                logger.warning(f"Failed to post shuffle to lobby channel: {exc}")

        command_channel_id = interaction.channel.id if interaction.channel else None
        cmd_message = None
        if command_channel_id and command_channel_id != lobby_channel_id:
            try:
                cmd_message = await interaction.channel.send(embed=embed)
            except Exception as exc:
                logger.warning(f"Failed to post shuffle to command channel: {exc}")

        await interaction.followup.send("✅ Teams shuffled!", ephemeral=True)

        # Capture origin_channel_id before reset_lobby clears it (betting reminders need it).
        try:
            origin_channel_id = await asyncio.to_thread(
                self.lobby_service.get_origin_channel_id, guild_id=guild_id
            )
            if message or cmd_message:
                jump_url = message.jump_url if message and hasattr(message, "jump_url") else None
                await asyncio.to_thread(
                    self.match_service.set_shuffle_message_info,
                    guild_id,
                    message_id=message.id if message else None,
                    channel_id=message.channel.id if message and message.channel else None,
                    jump_url=jump_url,
                    origin_channel_id=origin_channel_id,
                    pending_match_id=pending_match_id,
                    cmd_message_id=cmd_message.id if cmd_message else None,
                    cmd_channel_id=cmd_message.channel.id if cmd_message and cmd_message.channel else None,
                )
        except Exception as exc:
            logger.warning(f"Failed to store shuffle message URL: {exc}", exc_info=True)

        pending_state = await asyncio.to_thread(
            self.match_service.get_last_shuffle,
            guild_id,
            pending_match_id=pending_match_id,
        )
        bet_lock_until = pending_state.bet_lock_until if pending_state else None
        await self._schedule_betting_reminders(guild_id, bet_lock_until, pending_match_id=pending_match_id)

        included_ids = []
        if pending_state:
            included_ids = list(pending_state.radiant_team_ids) + list(pending_state.dire_team_ids)
        await self._lock_lobby_thread(
            guild_id,
            shuffle_embed=embed,
            included_player_ids=included_ids,
            pending_match_id=pending_match_id,
        )

        lobby_channel = None
        if lobby_channel_id:
            try:
                lobby_channel = self.bot.get_channel(lobby_channel_id)
                if not lobby_channel:
                    lobby_channel = await self.bot.fetch_channel(lobby_channel_id)
            except Exception as e:
                logger.debug("Failed to fetch lobby channel, falling back to interaction channel: %s", e)
                lobby_channel = interaction.channel
        else:
            lobby_channel = interaction.channel
        await safe_unpin_all_bot_messages(lobby_channel, self.bot.user)
        await asyncio.to_thread(self.lobby_service.reset_lobby, guild_id)

        from bot import clear_lobby_rally_cooldowns
        clear_lobby_rally_cooldowns(guild_id or 0)

    @app_commands.command(
        name="record",
        description="Record a match result or abort the match",
    )
    @app_commands.describe(
        result="Match result: Radiant won, Dire won, or Abort",
        dotabuff_match_id="Dotabuff match ID (optional)",
    )
    @app_commands.choices(
        result=[
            app_commands.Choice(name="Radiant Won", value="radiant"),
            app_commands.Choice(name="Dire Won", value="dire"),
            app_commands.Choice(name="Abort Match", value="abort"),
        ]
    )
    async def record(
        self,
        interaction: discord.Interaction,
        result: app_commands.Choice[str],
        dotabuff_match_id: str = None,
    ):
        guild = interaction.guild if hasattr(interaction, "guild") else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="record",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=3,
            per_seconds=30,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/record` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        logger.info(
            f"Record command: User {interaction.user.id} ({interaction.user.name}) "
            f"result={result.value} match_id={dotabuff_match_id}"
        )

        guild_id = (
            interaction.guild.id if hasattr(interaction, "guild") and interaction.guild else None
        )

        # Auto-detect which pending match the voter is in (for concurrent match support)
        # This determines which match's votes to update
        pending_state = None
        all_pending = await asyncio.to_thread(
            self.match_service.state_service.get_all_pending_matches, guild_id
        )

        if len(all_pending) == 0:
            await interaction.followup.send("❌ No pending match to record.", ephemeral=True)
            return
        elif len(all_pending) == 1:
            # Single match - use it (backward compatible)
            pending_state = all_pending[0]
        else:
            # Multiple matches - find the one the voter is in. Abort votes are
            # allowed for the full shuffled lobby, including excluded players.
            if result.value == "abort":
                player_match = await asyncio.to_thread(
                    self.match_service.get_pending_match_for_abort_voter,
                    guild_id,
                    interaction.user.id,
                )
            else:
                player_match = await asyncio.to_thread(
                    self.match_service.state_service.get_pending_match_for_player,
                    guild_id,
                    interaction.user.id,
                )
            if player_match:
                pending_state = player_match
            else:
                # Voter not in any match - they can't vote
                match_ids = ", ".join(f"#{m.pending_match_id}" for m in all_pending)
                if result.value == "abort":
                    voter_context = "you're not in any shuffled lobby"
                    vote_target = "Only players in the shuffled lobby can vote to abort."
                else:
                    voter_context = "you're not a participant in any of them"
                    vote_target = "Only match participants can vote on the result."
                await interaction.followup.send(
                    f"❌ Multiple pending matches exist ({match_ids}) but {voter_context}. "
                    f"{vote_target}",
                    ephemeral=True,
                )
                return

        pending_match_id = pending_state.pending_match_id
        logger.info(f"Recording for pending_match_id={pending_match_id}")

        is_admin = has_admin_permission(interaction)
        if result.value == "abort":
            if is_admin:
                await self._finalize_abort(interaction, guild_id, pending_match_id=pending_match_id)
                return
            try:
                submission = await asyncio.to_thread(
                    functools.partial(self.match_service.add_abort_submission,
                        guild_id, interaction.user.id, is_admin=False, pending_match_id=pending_match_id)
                )
            except ValueError as exc:
                await interaction.followup.send(f"❌ {exc}", ephemeral=True)
                return
            if not submission["is_ready"]:
                min_subs = self.match_service.MIN_ABORT_SUBMISSIONS
                match_id_note = f" (Match #{pending_match_id})" if pending_match_id else ""
                await interaction.followup.send(
                    f"✅ Abort request recorded{match_id_note}. Non-admin submissions: {submission['non_admin_count']}/{min_subs} "
                    f"(admins do not count toward the minimum).\nRequires {min_subs} abort confirmations.",
                    ephemeral=True,
                )
                return
            await self._finalize_abort(interaction, guild_id, pending_match_id=pending_match_id)
            return
        try:
            submission = await asyncio.to_thread(
                functools.partial(self.match_service.add_record_submission,
                    guild_id, interaction.user.id, result.value, is_admin, pending_match_id=pending_match_id)
            )
        except ValueError as exc:
            await interaction.followup.send(f"❌ {exc}", ephemeral=True)
            return

        if not submission["is_ready"]:
            vote_counts = submission.get("vote_counts", {"radiant": 0, "dire": 0})
            min_subs = self.match_service.MIN_NON_ADMIN_SUBMISSIONS
            admin_note = " (admins do not count toward the minimum)" if is_admin else ""
            match_id_note = f" (Match #{pending_match_id})" if pending_match_id else ""
            confirmations_text = f"🟢 Radiant: {vote_counts['radiant']}/{min_subs} | 🔴 Dire: {vote_counts['dire']}/{min_subs}"
            await interaction.followup.send(
                f"✅ Result recorded for {result.name}{match_id_note}.{admin_note}\n{confirmations_text}\nRequires {min_subs} confirmations.",
                ephemeral=True,
            )
            return

        winning_result = submission["result"]

        # Save thread_id before record_match clears the pending state
        # Use the specific pending_match_id we've been working with
        thread_id_for_finalize = (
            pending_state.thread_shuffle_thread_id if pending_state else None
        )

        # Check first-game-of-night BEFORE recording (0 matches since boundary = first game)
        is_first_game = False
        if FIRST_GAME_BONUS > 0:
            try:
                is_first_game = await asyncio.to_thread(
                    self.match_service.is_first_game_of_night, guild_id
                )
            except Exception:
                logger.warning("Failed to check first game of night", exc_info=True)

        try:
            record_result = await asyncio.to_thread(
                functools.partial(self.match_service.record_match,
                    winning_result, guild_id=guild_id, dotabuff_match_id=dotabuff_match_id,
                    pending_match_id=pending_match_id)
            )
        except ValueError as exc:
            await interaction.followup.send(f"❌ {exc}", ephemeral=True)
            return
        except Exception as exc:
            logger.error(f"Error recording match: {exc}", exc_info=True)
            await interaction.followup.send(
                "❌ Unexpected error recording match. Please try again.", ephemeral=True
            )
            return
        finally:
            # Cancel any pending betting reminders when recording completes (success or failure)
            self._cancel_betting_tasks(guild_id)

        # Finalize lobby thread as soon as the match is committed. Everything
        # below this point is presentation/notification work and may fail
        # independently of the persisted match result.
        _retain_background_task(
            asyncio.create_task(
                self._finalize_lobby_thread(
                    guild_id, winning_result,
                    thread_id=thread_id_for_finalize,
                    pending_match_id=pending_match_id,
                )
            )
        )

        distributions = record_result.get("bet_distributions", {})
        winners = distributions.get("winners", [])
        losers = distributions.get("losers", [])

        # Assemble the result message body from focused formatting steps.
        distribution_text = self._format_bet_distribution(
            winners, losers, distributions.get("bankruptcy_penalties", {})
        )
        distribution_text += self._format_stake_distribution(record_result)
        distribution_text += self._format_streak_bonus(distributions)
        distribution_text += await self._award_streaming_bonus_at_record(
            guild, guild_id, record_result
        )
        distribution_text += await self._award_first_game_bonus(
            guild_id, is_first_game, record_result
        )

        admin_override = (
            is_admin
            and submission["non_admin_count"] < self.match_service.MIN_NON_ADMIN_SUBMISSIONS
        )
        winning_team_name = "Radiant Won" if winning_result == "radiant" else "Dire Won"
        vote_counts = submission.get("vote_counts", {"radiant": 0, "dire": 0})
        confirmations_text = (
            f" (🟢 {vote_counts['radiant']} vs 🔴 {vote_counts['dire']})"
            if not admin_override
            else ""
        )
        message = f"✅ Match recorded — {winning_team_name}{confirmations_text}.{distribution_text}"
        await self._send_record_announcement(interaction, message)

        self._spawn_curse_flames(interaction, guild, guild_id, winners, losers, record_result)

        await self._run_neon_match_hooks(interaction, guild_id, winners, losers, record_result)

        # Trigger auto-discovery in background if enabled
        match_id = record_result.get("match_id")
        if match_id:
            _retain_background_task(
                asyncio.create_task(
                    self._trigger_auto_discovery(guild_id, match_id, interaction.channel)
                )
            )

    async def _send_record_announcement(
        self,
        interaction: discord.Interaction,
        result_message: str,
    ) -> None:
        """Send the reliable result details, chunked when needed."""
        for content in _chunk_discord_content(result_message):
            await interaction.followup.send(content, ephemeral=False)

    def _format_bet_distribution(
        self, winners: list[dict], losers: list[dict],
        bankruptcy_penalties: dict[int, int] | None = None,
    ) -> str:
        """Format the winners/losers payout breakdown for the record message.

        Returns a leading-newline block, or an empty string when there are no bets.
        """
        distribution_lines: list[str] = []

        # Group winners by user (supports multiple bets per user)
        if winners:
            distribution_lines.append("🏆 Winners:")
            winners_by_user: dict[int, list[dict]] = {}
            for entry in winners:
                uid = entry["discord_id"]
                if uid not in winners_by_user:
                    winners_by_user[uid] = []
                winners_by_user[uid].append(entry)

            penalties = bankruptcy_penalties or {}
            for uid, user_bets in winners_by_user.items():
                total_payout = sum(b["payout"] for b in user_bets)
                multiplier = user_bets[0].get("multiplier")  # Same for all bets in pool mode

                # Bankruptcy debuff (per-player): show the net the player kept,
                # plus a concise note of the deduction.
                penalty = int(penalties.get(uid, 0))
                shown = total_payout - penalty
                pen_note = f" (−{penalty} {JOPACOIN_EMOTE} bankruptcy)" if penalty > 0 else ""

                if len(user_bets) == 1:
                    # Single bet - original display
                    bet = user_bets[0]
                    leverage = bet.get("leverage", 1) or 1
                    leverage_text = f" at {leverage}x" if leverage > 1 else ""
                    if multiplier:
                        distribution_lines.append(
                            f"<@{uid}> won {shown} {JOPACOIN_EMOTE} "
                            f"(bet {bet['amount']}{leverage_text}, {multiplier:.2f}x){pen_note}"
                        )
                    else:
                        distribution_lines.append(
                            f"<@{uid}> won {shown} {JOPACOIN_EMOTE} "
                            f"(bet {bet['amount']}{leverage_text}){pen_note}"
                        )
                else:
                    # Multiple bets - show breakdown
                    bet_parts = []
                    for b in user_bets:
                        lev = b.get("leverage", 1) or 1
                        if lev > 1:
                            bet_parts.append(f"{b['amount']}@{lev}x")
                        else:
                            bet_parts.append(str(b["amount"]))
                    bets_str = "+".join(bet_parts)
                    if multiplier:
                        distribution_lines.append(
                            f"<@{uid}> won {shown} {JOPACOIN_EMOTE} "
                            f"(bets: {bets_str}, {multiplier:.2f}x){pen_note}"
                        )
                    else:
                        distribution_lines.append(
                            f"<@{uid}> won {shown} {JOPACOIN_EMOTE} (bets: {bets_str}){pen_note}"
                        )

        # Group losers by user (supports multiple bets per user)
        if losers:
            # Calculate total lost by losing side (use effective_bet when available)
            total_lost = sum(
                entry.get("effective_bet", entry["amount"])
                for entry in losers
                if not entry.get("refunded")
            )
            distribution_lines.append(f"😞 Losers (total: {total_lost} {JOPACOIN_EMOTE}):")

            losers_by_user: dict[int, list[dict]] = {}
            for entry in losers:
                uid = entry["discord_id"]
                if uid not in losers_by_user:
                    losers_by_user[uid] = []
                losers_by_user[uid].append(entry)

            for uid, user_bets in losers_by_user.items():
                # Check if all refunded (pool edge case)
                all_refunded = all(b.get("refunded") for b in user_bets)
                if all_refunded:
                    total_refunded = sum(b["amount"] for b in user_bets)
                    distribution_lines.append(
                        f"<@{uid}> refunded {total_refunded} {JOPACOIN_EMOTE} (no winners on opposing side)"
                    )
                elif len(user_bets) == 1:
                    # Single bet - original display
                    bet = user_bets[0]
                    leverage = bet.get("leverage", 1) or 1
                    leverage_text = f" at {leverage}x" if leverage > 1 else ""
                    distribution_lines.append(
                        f"<@{uid}> lost {bet['amount']} {JOPACOIN_EMOTE}{leverage_text}"
                    )
                else:
                    # Multiple bets - show breakdown
                    bet_parts = []
                    total_base = 0
                    for b in user_bets:
                        lev = b.get("leverage", 1) or 1
                        total_base += b["amount"]
                        if lev > 1:
                            bet_parts.append(f"{b['amount']}@{lev}x")
                        else:
                            bet_parts.append(str(b["amount"]))
                    bets_str = "+".join(bet_parts)
                    distribution_lines.append(f"<@{uid}> lost {bets_str} {JOPACOIN_EMOTE}")

        if distribution_lines:
            return "\n" + "\n".join(distribution_lines)
        return ""

    def _format_stake_distribution(self, record_result: dict) -> str:
        """Format draft-mode stake pool payouts. Returns "" when not applicable."""
        stake_distributions = record_result.get("stake_distributions", {})
        if not (stake_distributions.get("enabled") and stake_distributions.get("winners")):
            return ""

        stake_winners = stake_distributions.get("winners", [])
        payout_per_winner = stake_distributions.get("payout_per_winner", 0)
        total_payout = stake_distributions.get("total_payout", 0)

        # Count team winners vs excluded
        team_winners = [w for w in stake_winners if not w.get("is_excluded")]
        excluded_winners = [w for w in stake_winners if w.get("is_excluded")]

        stake_text = f"\n\n🎯 **Stake Pool Payouts:** +{payout_per_winner} {JOPACOIN_EMOTE} each"
        if team_winners:
            stake_text += f"\n  Winners ({len(team_winners)}): "
            stake_text += ", ".join(f"<@{w['discord_id']}>" for w in team_winners)
        if excluded_winners:
            stake_text += f"\n  Excluded ({len(excluded_winners)}): "
            stake_text += ", ".join(f"<@{w['discord_id']}>" for w in excluded_winners)
        stake_text += f"\n  **Total minted:** {total_payout} {JOPACOIN_EMOTE}"
        return stake_text

    def _format_streak_bonus(self, distributions: dict) -> str:
        """Format daily-play streak bonuses (per actual player, not bench/excluded).

        Bonus tier comes from the unified STREAKS schedule (see services/dig_constants.py).
        """
        streaks = distributions.get("streaks", {})
        streak_lines = [
            f"<@{uid}>: +{info['bonus']} {JOPACOIN_EMOTE} ({info['days']}d)"
            for uid, info in streaks.items()
            if info.get("bonus", 0) > 0
        ]
        if streak_lines:
            return "\n\n🔥 **Streak Bonus:**\n" + "\n".join(streak_lines)
        return ""

    async def _award_streaming_bonus_at_record(
        self,
        guild: discord.Guild | None,
        guild_id: int | None,
        record_result: dict,
    ) -> str:
        """Award +1 JC to match participants who are Go Live.

        Awarded at both shuffle and record time (intentional: rewards continuous
        streaming). Returns the formatted streaming-bonus line, or "".
        """
        if not (guild and hasattr(guild, "get_member") and STREAMING_BONUS > 0):
            return ""

        all_participant_ids = list(record_result.get("winning_player_ids", [])) + list(
            record_result.get("losing_player_ids", [])
        )
        streaming_ids = get_streaming_player_ids(guild, all_participant_ids)
        if not streaming_ids:
            return ""

        betting_svc = getattr(self.bot, "betting_service", None)
        if betting_svc:
            await asyncio.to_thread(
                betting_svc.award_streaming_bonus, list(streaming_ids), guild_id
            )
        streamer_mentions = ", ".join(f"<@{sid}>" for sid in streaming_ids)
        logger.info(f"Streaming bonus (+{STREAMING_BONUS} JC) at record: {streaming_ids}")
        return (
            f"\n📺 Streaming bonus (+{STREAMING_BONUS} {JOPACOIN_EMOTE}): {streamer_mentions}"
        )

    async def _award_first_game_bonus(
        self,
        guild_id: int | None,
        is_first_game: bool,
        record_result: dict,
    ) -> str:
        """Award the first-game-of-the-night bonus to all lobby participants
        (including excluded). Returns the formatted bonus line, or "".
        """
        if not (is_first_game and FIRST_GAME_BONUS > 0):
            return ""

        all_ids = list(set(
            list(record_result.get("winning_player_ids", []))
            + list(record_result.get("losing_player_ids", []))
            + list(record_result.get("excluded_player_ids", []))
            + list(record_result.get("excluded_conditional_player_ids", []))
        ))
        betting_svc = getattr(self.bot, "betting_service", None)
        if not (betting_svc and all_ids):
            return ""

        await asyncio.to_thread(betting_svc.award_first_game_bonus, all_ids, guild_id)
        logger.info(f"First game bonus (+{FIRST_GAME_BONUS} JC) awarded to {all_ids}")
        return (
            f"\n🌙 First game of the night! (+{FIRST_GAME_BONUS} {JOPACOIN_EMOTE} each)"
        )

    def _spawn_curse_flames(
        self,
        interaction: discord.Interaction,
        guild: discord.Guild | None,
        guild_id: int | None,
        winners: list[dict],
        losers: list[dict],
        record_result: dict,
    ) -> None:
        """Fire Witch's Curse hooks per cursed engager (no-ops fast for non-cursed)."""
        curse_service = getattr(self.bot, "curse_service", None)
        if curse_service is None or interaction.channel is None:
            return

        from services.curse_service import spawn_curse_flame

        channel = interaction.channel

        def _resolve_name(uid: int) -> str | None:
            if guild is not None and hasattr(guild, "get_member"):
                member = guild.get_member(uid)
                if member is not None:
                    return member.display_name
            return None

        for uid in record_result.get("winning_player_ids", []) or []:
            spawn_curse_flame(
                curse_service,
                channel,
                target_id=uid,
                guild_id=guild_id,
                system="match",
                outcome="win",
                event_context={"team": "winner"},
                target_display_name=_resolve_name(uid),
            )
        for uid in record_result.get("losing_player_ids", []) or []:
            spawn_curse_flame(
                curse_service,
                channel,
                target_id=uid,
                guild_id=guild_id,
                system="match",
                outcome="loss",
                event_context={"team": "loser"},
                target_display_name=_resolve_name(uid),
            )
        seen_betters: set[int] = set()
        for entry in winners or []:
            uid = entry.get("discord_id")
            if uid is None or uid in seen_betters:
                continue
            seen_betters.add(uid)
            spawn_curse_flame(
                curse_service,
                channel,
                target_id=uid,
                guild_id=guild_id,
                system="bet",
                outcome="win",
                event_context={"side": "winner"},
                target_display_name=_resolve_name(uid),
            )
        for entry in losers or []:
            uid = entry.get("discord_id")
            if uid is None or uid in seen_betters:
                continue
            seen_betters.add(uid)
            spawn_curse_flame(
                curse_service,
                channel,
                target_id=uid,
                guild_id=guild_id,
                system="bet",
                outcome="loss",
                event_context={"side": "loser"},
                target_display_name=_resolve_name(uid),
            )

    async def _run_neon_match_hooks(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        winners: list[dict],
        losers: list[dict],
        record_result: dict,
    ) -> None:
        """Run prioritized Neon/JOPA-T hooks for a recorded match.

        At most one neon event is sent per match.
        """
        neon = get_neon_service(self.bot)
        if not neon:
            return

        try:
            neon_sent = False

            # Headline: a big match-bet win (rare, payout-scaled). The top payout
            # takes the slot; an underdog upset (winning side was the minority
            # stake) is flagged so the GIF banner reads accordingly.
            if winners:
                top = max(winners, key=lambda w: w.get("payout", 0) or 0)
                top_payout = top.get("payout", 0) or 0
                if top_payout and not top.get("refunded"):
                    win_stake = sum((w.get("amount", 0) or 0) for w in winners)
                    lose_stake = sum((entry.get("amount", 0) or 0) for entry in losers)
                    flavor = "underdog" if (win_stake and lose_stake > win_stake) else "top_dog"
                    bw = await neon.on_big_win(
                        top["discord_id"], guild_id,
                        source="match", payout=top_payout, flavor=flavor,
                    )
                    if bw:
                        await send_neon_result(interaction, bw)
                        neon_sent = True

            # Wire on_bet_settled + on_leverage_loss for losers (max ONE per match)
            if not neon_sent and losers:
                for entry in losers:
                    if entry.get("refunded"):
                        continue
                    loser_id = entry["discord_id"]
                    leverage = entry.get("leverage", 1) or 1
                    amount = entry.get("effective_bet", entry["amount"])
                    new_bal = await asyncio.to_thread(
                        self.player_service.get_balance, loser_id, guild_id
                    )

                    # on_leverage_loss: 5x leverage into debt
                    if leverage >= 5 and new_bal < 0:
                        lr = await neon.on_leverage_loss(
                            loser_id, guild_id, amount, leverage, new_bal
                        )
                        if lr:
                            await send_neon_result(interaction, lr)
                            neon_sent = True
                            break

                    # on_bet_settled: hit MAX_DEBT or zero
                    from config import MAX_DEBT as _MAX_DEBT
                    if new_bal <= -_MAX_DEBT or new_bal == 0:
                        sr = await neon.on_bet_settled(
                            loser_id, guild_id, won=False, new_balance=new_bal
                        )
                        if sr:
                            await send_neon_result(interaction, sr)
                            neon_sent = True
                            break

            # Wire on_degen_milestone for losers
            if not neon_sent and losers:
                for entry in losers:
                    if entry.get("refunded"):
                        continue
                    loser_id = entry["discord_id"]
                    degen_score = await asyncio.to_thread(
                        neon._get_degen_score, loser_id, guild_id
                    )
                    if degen_score is not None and degen_score >= 90:
                        mr = await neon.on_degen_milestone(loser_id, guild_id, degen_score)
                        if mr:
                            await send_neon_result(interaction, mr)
                            neon_sent = True
                            break

            # Easter egg hooks: at most ONE additional neon event
            if not neon_sent:
                easter_data = record_result.get("easter_egg_data", {})

                for milestone in easter_data.get("games_milestones", []):
                    if neon_sent:
                        break
                    gm_result = await neon.on_games_milestone(
                        milestone["discord_id"],
                        guild_id,
                        milestone["total_games"],
                    )
                    if gm_result:
                        await send_neon_result(interaction, gm_result)
                        neon_sent = True

                for streak_rec in easter_data.get("win_streak_records", []):
                    if neon_sent:
                        break
                    ws_result = await neon.on_win_streak_record(
                        streak_rec["discord_id"],
                        guild_id,
                        streak_rec["current_streak"],
                        streak_rec["previous_best"],
                    )
                    if ws_result:
                        await send_neon_result(interaction, ws_result)
                        neon_sent = True

                for rivalry in easter_data.get("rivalries_detected", []):
                    if neon_sent:
                        break
                    rv_result = await neon.on_rivalry_detected(
                        guild_id,
                        rivalry["player1_id"],
                        rivalry["player2_id"],
                        rivalry["games_together"],
                        rivalry["winrate_vs"],
                    )
                    if rv_result:
                        await send_neon_result(interaction, rv_result)
                        neon_sent = True

            # Consolidated chance-gated debrief runs only after rarer, higher-impact
            # events decline their slot. Plain match/bet flavor does not bypass this.
            if not neon_sent:
                eligible_losers = [
                    entry for entry in losers if not entry.get("refunded")
                ]
                notable_loser = max(
                    eligible_losers,
                    key=lambda entry: (
                        entry.get("leverage", 1) or 1,
                        entry.get("effective_bet", entry.get("amount", 0)) or 0,
                    ),
                    default=None,
                )
                top_bet_winner = max(
                    winners,
                    key=lambda entry: entry.get("payout", 0) or 0,
                    default=None,
                )

                winner_id = None
                match_details: dict = {}
                if record_result.get("winning_player_ids"):
                    winner_id, match_details = await asyncio.to_thread(
                        self._get_notable_winner,
                        match_id=record_result["match_id"],
                        winning_ids=record_result["winning_player_ids"],
                    )
                streak_data = record_result.get("notable_streak") or {}
                streak = int(streak_data.get("streak", 0) or 0)
                if streak and not streak_data.get("is_win", False):
                    streak = -abs(streak)

                # Keep every fact attached to the player it describes. Select the
                # strongest candidate instead of blending unrelated bettors,
                # rating changes, and streaks into one misleading prompt.
                candidates: list[
                    tuple[JopatPostMatchContext, int | None, int | None]
                ] = []
                if top_bet_winner:
                    candidates.append(
                        (
                            JopatPostMatchContext(
                                payout=top_bet_winner.get("payout", 0) or 0,
                            ),
                            top_bet_winner.get("discord_id"),
                            None,
                        )
                    )
                if notable_loser:
                    candidates.append(
                        (
                            JopatPostMatchContext(
                                loss=notable_loser.get(
                                    "effective_bet", notable_loser.get("amount", 0)
                                )
                                or 0,
                                leverage=notable_loser.get("leverage", 1) or 1,
                            ),
                            None,
                            notable_loser.get("discord_id"),
                        )
                    )
                streak_player_id = streak_data.get("discord_id")
                if streak and streak_player_id is not None:
                    candidates.append(
                        (
                            JopatPostMatchContext(streak=streak),
                            streak_player_id if streak > 0 else None,
                            streak_player_id if streak < 0 else None,
                        )
                    )
                if winner_id is not None:
                    candidates.append(
                        (
                            JopatPostMatchContext(
                                rating_change=match_details.get("rating_change"),
                                expected_win_prob=match_details.get(
                                    "expected_win_prob"
                                ),
                            ),
                            winner_id,
                            None,
                        )
                    )
                if not candidates:
                    candidates.append((JopatPostMatchContext(), None, None))

                context, context_winner_id, context_loser_id = max(
                    candidates,
                    key=lambda candidate: neon._post_match_chance(candidate[0]),
                )
                debrief = await neon.on_post_match_debrief(
                    guild_id,
                    context,
                    winner_id=context_winner_id,
                    loser_id=context_loser_id,
                )
                if debrief:
                    await send_neon_result(interaction, debrief)

        except Exception as exc:
            logger.debug(f"Neon match hook failed: {exc}")

    async def _trigger_auto_discovery(
        self,
        guild_id: int | None,
        match_id: int,
        channel: discord.abc.Messageable | None,
    ) -> None:
        """
        Trigger auto-discovery for a match in the background with exponential backoff.

        Checks if auto_enrich is enabled, then attempts to find and enrich
        the Dota 2 match ID using player match histories from OpenDota.
        Uses exponential backoff retry logic: attempts at 1s, 5s, 20s, 60s, 180s.
        """
        try:
            # Check if auto_enrich is enabled for this guild
            if self.guild_config_service:
                auto_enrich = await asyncio.to_thread(
                    self.guild_config_service.is_auto_enrich_enabled, guild_id
                )
                if not auto_enrich:
                    logger.debug(f"Auto-enrich disabled for guild {guild_id}, skipping discovery")
                    return

            # Get repos from bot for MatchDiscoveryService (which needs direct repo access)
            match_repo = getattr(self.bot, "match_repo", None)
            player_repo = getattr(self.bot, "player_repo", None)
            if not match_repo or not player_repo:
                logger.warning("Cannot auto-discover: missing match_repo or player_repo")
                return

            # Create discovery service once
            discovery_service = MatchDiscoveryService(
                match_repo, player_repo, match_service=self.match_service
            )

            # Exponential backoff retry loop
            delays = ENRICHMENT_RETRY_DELAYS  # [1, 5, 20, 60, 180]
            total_attempts = len(delays)

            for attempt, delay in enumerate(delays, 1):
                await asyncio.sleep(delay)

                # Run discovery in thread to avoid blocking
                result = await asyncio.to_thread(
                    discovery_service.discover_match, match_id, guild_id
                )

                status = result.get("status")

                if status == "discovered":
                    # Success - send enriched embed
                    await self._send_enrichment_result(channel, match_id, result, guild_id)
                    return

                if status in ("low_confidence", "no_candidates"):
                    # OpenDota hasn't parsed yet - retry with next delay
                    logger.info(
                        f"Match {match_id}: OpenDota not ready (attempt {attempt}/{total_attempts}), "
                        f"status={status}, confidence={result.get('confidence', 0):.0%}"
                    )
                    continue

                if status == "validation_failed":
                    # Validation failed - likely wrong match, don't retry
                    logger.warning(
                        f"Match {match_id}: Validation failed: {result.get('validation_error')}"
                    )
                    return

                # Other errors (no_steam_ids, not_found, etc.) - don't retry
                logger.debug(
                    f"Match {match_id}: Discovery status={status}, not retrying"
                )
                return

            # Exhausted all retries
            logger.warning(
                f"Match {match_id}: Gave up after {total_attempts} attempts "
                f"(total wait: {sum(delays)}s)"
            )

        except Exception as exc:
            logger.error(f"Error in auto-discovery for match {match_id}: {exc}", exc_info=True)

    async def _send_enrichment_result(
        self,
        channel: discord.abc.Messageable | None,
        match_id: int,
        result: dict,
        guild_id: int | None = None,
    ) -> None:
        """Send enriched match embed to channel after successful discovery."""
        valve_match_id = result.get("valve_match_id")
        confidence = result.get("confidence", 0)

        logger.info(
            f"Auto-discovered match {match_id} -> valve_match_id={valve_match_id} "
            f"(confidence: {confidence:.0%})"
        )

        if not channel:
            return

        try:
            # Fetch enriched match data for embed
            match_data = await asyncio.to_thread(
                self.match_service.get_match_by_id, match_id, guild_id
            )
            participants = await asyncio.to_thread(
                self.match_service.get_match_participants, match_id, guild_id
            )

            if match_data and participants:
                radiant = [p for p in participants if p.get("side") == "radiant"]
                dire = [p for p in participants if p.get("side") == "dire"]

                embed = await asyncio.to_thread(
                    create_enriched_match_embed,
                    match_id=match_id,
                    valve_match_id=valve_match_id,
                    duration_seconds=match_data.get("duration_seconds"),
                    radiant_score=match_data.get("radiant_score"),
                    dire_score=match_data.get("dire_score"),
                    winning_team=match_data.get("winning_team", 1),
                    radiant_participants=radiant,
                    dire_participants=dire,
                    bankruptcy_repo=self.bankruptcy_repo,
                    lobby_type=match_data.get("lobby_type", "shuffle"),
                    guild_id=guild_id,
                )

                enrichment_data = await asyncio.to_thread(
                    self.match_service.get_enrichment_data,
                    match_id,
                    guild_id,
                )
                view = EnrichedMatchView(embed, enrichment_data, match_id)
                msg = await channel.send(
                    f"📊 Match #{match_id} auto-enriched ({confidence:.0%} confidence)",
                    embed=embed,
                    view=view,
                )
                view.message = msg
            else:
                # Fallback to simple message
                await channel.send(
                    f"📊 Match #{match_id} auto-enriched! "
                    f"(Dota Match ID: {valve_match_id}, {confidence:.0%} confidence)"
                )
            # Trigger MVP compliments for winners
            if match_data and participants:
                try:
                    neon = get_neon_service(self.bot)
                    if neon:
                        winning_team = match_data.get("winning_team", 1)
                        winning_side = "radiant" if winning_team == 1 else "dire"
                        winners = [p for p in participants if p.get("side") == winning_side]
                        losers = [p for p in participants if p.get("side") != winning_side]
                        mvp_results = await neon.on_match_enriched(
                            guild_id, winners, losers
                        )
                        for neon_result in mvp_results:
                            if neon_result and neon_result.text_block:
                                mvp_msg = await channel.send(neon_result.text_block)
                                _retain_background_task(
                                    asyncio.create_task(_neon_delete_after(mvp_msg, 60))
                                )
                except Exception as exc:
                    logger.debug(f"MVP compliment failed: {exc}")
        except Exception as exc:
            logger.warning(f"Failed to send enrichment result: {exc}")

    async def _finalize_abort(
        self,
        interaction: discord.Interaction,
        guild_id: int | None,
        pending_match_id: int | None = None,
    ):
        betting_service = getattr(self.bot, "betting_service", None)
        pending_state = await asyncio.to_thread(
            self.match_service.state_service.get_last_shuffle, guild_id, pending_match_id
        )
        if betting_service and pending_state:
            try:
                await asyncio.to_thread(
                    betting_service.refund_pending_bets,
                    guild_id,
                    pending_state,
                    pending_match_id=pending_match_id,
                )
            except Exception as exc:
                logger.error(f"Error refunding pending bets on abort: {exc}", exc_info=True)
        # Cancel any pending betting reminders
        self._cancel_betting_tasks(guild_id)

        # Update channel message to show closed and archive thread
        await self._update_channel_message_closed("Match Aborted", guild_id=guild_id)
        await self._abort_lobby_thread(guild_id, pending_match_id)

        # Clear only the specific pending match (not all of them)
        await asyncio.to_thread(
            self.match_service.state_service.clear_last_shuffle, guild_id, pending_match_id
        )
        await safe_unpin_all_bot_messages(interaction.channel, self.bot.user)

        # Don't reset lobby on abort - players can still queue for next game
        # Only reset lobby after successful shuffle to clear the player list

        # Clear lobby rally cooldowns
        from bot import clear_lobby_rally_cooldowns
        clear_lobby_rally_cooldowns(guild_id or 0)

        match_id_note = f" (Match #{pending_match_id})" if pending_match_id else ""
        await interaction.followup.send(
            f"✅ Match aborted{match_id_note}. Bets have been refunded.", ephemeral=False
        )

    async def _schedule_betting_reminders(
        self, guild_id: int | None, bet_lock_until: int | None,
        pending_match_id: int | None = None,
    ) -> None:
        """
        Schedule betting reminder tasks (5-minute warning and close) for the current shuffle.
        """
        if not bet_lock_until:
            return

        betting_cog = self.bot.get_cog("BettingCommands")
        if betting_cog is None or not hasattr(betting_cog, "_send_betting_reminder"):
            logger.debug("BettingCommands not loaded; skipping betting reminders.")
            return

        now_ts = int(time.time())
        seconds_until_close = bet_lock_until - now_ts
        if seconds_until_close <= 0:
            return

        # Build the escalating reminder schedule: plain "warning" reminders at
        # each configured offset before lock, the AI "last call" at its offset,
        # and the "closed" notice at lock. De-dupe offsets (last call wins) and
        # skip any offset that is already past for this window.
        offset_types: dict[int, str] = {}
        for off in BET_REMINDER_OFFSETS:
            if 0 < off < seconds_until_close:
                offset_types.setdefault(off, "warning")
        if 0 < BET_LAST_CALL_OFFSET < seconds_until_close:
            offset_types[BET_LAST_CALL_OFFSET] = "last_call"

        # The smallest active "warning" offset (closest to lock; 5 min by
        # default) is the enriched tier: it gets the AI persona flavor line and
        # the lopsided-pool underdog ping. Larger warnings stay terse.
        warning_offsets = [off for off, rtype in offset_types.items() if rtype == "warning"]
        final_warning_offset = min(warning_offsets) if warning_offsets else None

        tasks = [
            asyncio.create_task(
                self._run_bet_reminder_after_delay(
                    delay_seconds=seconds_until_close - off,
                    guild_id=guild_id,
                    reminder_type=rtype,
                    lock_until=bet_lock_until,
                    pending_match_id=pending_match_id,
                    is_final_warning=(rtype == "warning" and off == final_warning_offset),
                )
            )
            for off, rtype in sorted(offset_types.items(), reverse=True)
        ]

        # Close reminder at lock (also flips the embed to a closed state).
        tasks.append(
            asyncio.create_task(
                self._run_bet_reminder_after_delay(
                    delay_seconds=seconds_until_close,
                    guild_id=guild_id,
                    reminder_type="closed",
                    lock_until=bet_lock_until,
                    pending_match_id=pending_match_id,
                )
            )
        )

        self._register_betting_tasks(guild_id, tasks)

        reminder_svc = getattr(self.bot, "reminder_service", None)
        if reminder_svc and guild_id is not None:
            _retain_background_task(
                asyncio.create_task(
                    reminder_svc.notify_betting_subscribers(self.bot, guild_id, bet_lock_until)
                )
            )

    async def _run_bet_reminder_after_delay(
        self,
        *,
        delay_seconds: int,
        guild_id: int | None,
        reminder_type: str,
        lock_until: int | None,
        pending_match_id: int | None = None,
        is_final_warning: bool = False,
    ) -> None:
        """Sleep for delay_seconds then send the requested reminder, if still relevant."""
        try:
            await asyncio.sleep(delay_seconds)
            # Ensure pending state still matches the expected lock time
            state = await asyncio.to_thread(
                self.match_service.get_last_shuffle,
                guild_id,
                pending_match_id=pending_match_id,
            )
            if not state:
                return
            current_lock = state.bet_lock_until
            if not current_lock or (lock_until and current_lock != lock_until):
                return

            betting_cog = self.bot.get_cog("BettingCommands")
            if betting_cog is None or not hasattr(betting_cog, "_send_betting_reminder"):
                return

            await betting_cog._send_betting_reminder(
                guild_id,
                reminder_type=reminder_type,
                lock_until=lock_until,
                pending_match_id=pending_match_id,
                is_final_warning=is_final_warning,
            )
        except asyncio.CancelledError:
            # Task was cancelled because match ended/aborted
            return
        except Exception as exc:
            logger.warning(
                f"Failed to run betting reminder ({reminder_type}): {exc}", exc_info=True
            )

    def _register_betting_tasks(self, guild_id: int | None, tasks) -> None:
        """Store reminder tasks for the guild and cancel any existing tasks first."""
        self._cancel_betting_tasks(guild_id)
        normalized = normalize_guild_id(guild_id)
        self._betting_tasks_by_guild[normalized] = tasks

    def _cancel_betting_tasks(self, guild_id: int | None) -> None:
        """Cancel any scheduled betting reminder tasks for the guild."""
        normalized = normalize_guild_id(guild_id)
        tasks = self._betting_tasks_by_guild.pop(normalized, [])
        for task in tasks:
            try:
                task.cancel()
            except Exception as e:
                logger.debug("Failed to cancel betting task: %s", e)
                continue


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    match_service = getattr(bot, "match_service", None)
    player_service = getattr(bot, "player_service", None)
    guild_config_service = getattr(bot, "guild_config_service", None)
    bankruptcy_repo = getattr(bot, "bankruptcy_repo", None)
    await bot.add_cog(
        MatchCommands(
            bot,
            lobby_service,
            match_service,
            player_service,
            guild_config_service=guild_config_service,
            bankruptcy_repo=bankruptcy_repo,
        )
    )
