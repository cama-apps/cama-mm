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

from config import BOMB_POT_CHANCE, ENRICHMENT_RETRY_DELAYS, JOPACOIN_MIN_BET
from services.flavor_text_service import FlavorEvent
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
from utils.interaction_safety import safe_defer
from utils.pin_helpers import safe_unpin_all_bot_messages
from utils.rate_limiter import GLOBAL_RATE_LIMITER

logger = logging.getLogger("cama_bot.commands.match")


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
        flavor_text_service=None,
    ):
        self.bot = bot
        self.lobby_service = lobby_service
        self.match_service = match_service
        self.player_service = player_service
        self.guild_config_service = guild_config_service
        # bankruptcy_repo kept for embed builders that need bankruptcy status display
        self.bankruptcy_repo = bankruptcy_repo
        self.flavor_text_service = flavor_text_service
        # Track scheduled betting reminder tasks per guild for cleanup
        self._betting_tasks_by_guild = {}

    async def _send_neon_result(self, interaction: discord.Interaction, neon_result) -> None:
        """Send a NeonResult to the channel, auto-deleting after 60s."""
        try:
            if neon_result is None:
                return
            msg = None
            if neon_result.gif_file:
                gif_file = discord.File(neon_result.gif_file, filename="jopat_terminal.gif")
                if neon_result.text_block:
                    msg = await interaction.channel.send(neon_result.text_block, file=gif_file)
                else:
                    msg = await interaction.channel.send(file=gif_file)
            elif neon_result.text_block:
                msg = await interaction.channel.send(neon_result.text_block)
            elif neon_result.footer_text:
                msg = await interaction.channel.send(neon_result.footer_text)
            # Auto-delete after 60 seconds
            if msg:
                asyncio.create_task(self._delete_neon_after(msg, 60))
        except Exception as exc:
            logger.debug(f"Failed to send neon result: {exc}")

    @staticmethod
    async def _delete_neon_after(msg, delay: float) -> None:
        """Delete a message after a delay, ignoring errors."""
        try:
            await asyncio.sleep(delay)
            await msg.delete()
        except Exception:
            pass

    async def _update_channel_message_closed(self, reason: str = "Match Aborted") -> None:
        """Update the channel message embed to show lobby/match is closed."""
        message_id = self.lobby_service.get_lobby_message_id()
        channel_id = self.lobby_service.get_lobby_channel_id()
        if not message_id or not channel_id:
            return

        try:
            channel = self.bot.get_channel(channel_id)
            if not channel:
                channel = await self.bot.fetch_channel(channel_id)
            message = await channel.fetch_message(message_id)

            embed = discord.Embed(
                title=f"🚫 {reason}",
                description="This lobby has been closed.",
                color=discord.Color.dark_grey(),
            )
            await message.edit(embed=embed, view=None)
        except Exception as exc:
            logger.warning(f"Failed to update channel message as closed: {exc}")

    async def _lock_lobby_thread(
        self,
        guild_id: int | None,
        shuffle_embed: discord.Embed = None,
        included_player_ids: list[int] | None = None,
    ) -> None:
        """Lock the lobby thread when shuffle occurs and post shuffle results."""
        thread_id = self.lobby_service.get_lobby_thread_id()
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

        # Store thread shuffle message ID for betting updates
        if thread_shuffle_msg:
            self.match_service.set_shuffle_message_info(
                guild_id,
                message_id=None,
                channel_id=None,
                thread_message_id=thread_shuffle_msg.id,
                thread_id=thread_id,
            )

    async def _finalize_lobby_thread(
        self, guild_id: int | None, winning_result: str, *, thread_id: int | None = None
    ) -> None:
        """Post results to lobby thread and archive it."""
        # Use provided thread_id, or try to get from pending state / lobby_service
        if not thread_id:
            pending_state = self.match_service.get_last_shuffle(guild_id)
            thread_id = pending_state.get("thread_shuffle_thread_id") if pending_state else None
        if not thread_id:
            # Fallback to lobby_service in case it's still available
            thread_id = self.lobby_service.get_lobby_thread_id()
        if not thread_id:
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

    async def _abort_lobby_thread(self, guild_id: int | None) -> None:
        """Archive the lobby thread when match is aborted."""
        # Get thread_id from pending state (must be called before clear_last_shuffle)
        pending_state = self.match_service.get_last_shuffle(guild_id)
        thread_id = pending_state.get("thread_shuffle_thread_id") if pending_state else None
        if not thread_id:
            thread_id = self.lobby_service.get_lobby_thread_id()
        if not thread_id:
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

    def _get_notable_winner(
        self,
        match_id: int,
        winning_ids: list[int],
        exclude_id: int | None = None,
    ) -> tuple[int | None, dict]:
        """
        Pick the most notable winner for PMA flavor text.

        Args:
            match_id: The match ID to look up
            winning_ids: List of winning player discord IDs
            exclude_id: Optional discord_id to exclude (e.g., already mentioned in bet flavor)

        Returns: (discord_id, event_details) or (None, {})

        Priority:
        1. Underdog hero (won with <45% expected prob)
        2. Biggest rating gainer
        3. Random winner (fallback)
        """
        if not winning_ids:
            return None, {}

        # Filter out excluded player if specified
        available_winners = (
            [w for w in winning_ids if w != exclude_id] if exclude_id else winning_ids
        )
        if not available_winners:
            return None, {}  # All winners excluded

        # Get rating history for this match
        rating_data = self.match_service.get_rating_history_for_match(match_id)
        winner_data = [r for r in rating_data if r["discord_id"] in available_winners]

        if not winner_data:
            # Fallback: random winner, no details
            return random.choice(available_winners), {}

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
        rating_system="Rating system for team balancing (experimental)",
    )
    @app_commands.choices(
        rating_system=[
            app_commands.Choice(name="Glicko-2 (default)", value="glicko"),
            app_commands.Choice(name="OpenSkill (experimental)", value="openskill"),
            app_commands.Choice(name="Jopacoin Balance", value="jopacoin"),
        ]
    )
    async def shuffle(
        self,
        interaction: discord.Interaction,
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

        pending_match = self.match_service.get_last_shuffle(guild_id)
        if pending_match:
            jump_url = pending_match.get("shuffle_message_jump_url")
            message_text = "❌ There's an active match that needs to be recorded!"
            if jump_url:
                message_text += f" [View match]({jump_url}) then use `/record` first."
            else:
                message_text += " Use `/record` first."
            await interaction.followup.send(message_text, ephemeral=True)
            return

        # Check for active draft
        draft_state_manager = getattr(self.bot, "draft_state_manager", None)
        if draft_state_manager and draft_state_manager.has_active_draft(guild_id):
            state = draft_state_manager.get_state(guild_id)
            user_id = interaction.user.id

            # Check if user can restart (captain or admin)
            is_captain = state and user_id in (state.captain1_id, state.captain2_id)
            is_admin = user_id in getattr(self.bot, "admin_user_ids", set())
            if not is_admin and interaction.guild:
                member = interaction.guild.get_member(user_id)
                if member and member.guild_permissions.administrator:
                    is_admin = True

            # Only show restart option to captains and admins
            if is_captain or is_admin:
                msg = (
                    "❌ There's an active Immortal Draft in progress! "
                    "Use `/restartdraft` to restart it or complete the draft first."
                )
            else:
                msg = (
                    "❌ There's an active Immortal Draft in progress! "
                    "Please wait for it to complete."
                )

            await interaction.followup.send(msg, ephemeral=True)
            return

        lobby = self.lobby_service.get_lobby()
        if not lobby:
            await interaction.followup.send(
                "❌ No active lobby. Use `/lobby` to create one!", ephemeral=True
            )
            return

        regular_count = lobby.get_player_count()
        conditional_count = lobby.get_conditional_count()
        total_count = lobby.get_total_count()

        if total_count < 10:
            await interaction.followup.send(
                f"❌ Need at least 10 players in lobby. Currently {total_count}/10 "
                f"({regular_count} regular, {conditional_count} conditional).",
                ephemeral=True,
            )
            return

        # 15 or more players → force Immortal Draft with auto-selected captains
        if total_count >= 15:
            await interaction.followup.send(
                f"⚔️ **{total_count} players signed up** — starting Immortal Draft!\n"
                f"_(Shuffle limited to 14 players)_"
            )
            draft_cog = self.bot.get_cog("DraftCommands")
            if draft_cog:
                await draft_cog._execute_draft(
                    interaction, guild_id, lobby, force_random_captains=True
                )
            else:
                await interaction.followup.send(
                    "❌ Draft system not available. Please contact an admin.",
                    ephemeral=True,
                )
            return

        # Build the player list for shuffling
        # Priority: regular players first, then fill with conditional if needed
        player_ids, players = self.lobby_service.get_lobby_players(lobby, guild_id)
        conditional_player_ids_included = []
        excluded_conditional_ids = []

        # Always get conditional players to track who wasn't included
        all_conditional_ids, all_conditional_players = self.lobby_service.get_conditional_players(lobby, guild_id)

        if regular_count < 10:
            # Need to include some conditional players to reach exactly 10
            # Regular players should ALWAYS play when there are fewer than 10 of them
            # Only conditional players can fill the remaining slots
            # Sort conditional players by rating/RD priority (same as exclusion logic)
            # Higher RD = more uncertain = lower priority, higher rating = higher priority
            def priority_key(player):
                rating = player.glicko_rating if player.glicko_rating else 1500.0
                rd = player.glicko_rd if player.glicko_rd else 350.0
                # Higher rating and lower RD = higher priority
                return (rating, -rd)

            # Pair conditional players with their IDs for sorting
            conditional_pairs = list(zip(all_conditional_ids, all_conditional_players))
            conditional_pairs.sort(key=lambda x: priority_key(x[1]), reverse=True)

            # Take exactly enough conditional players to reach 10 (no more)
            # This ensures the shuffler gets exactly 10 players and excludes none,
            # guaranteeing all regular players are included
            slots_available = 10 - regular_count
            for cid, cplayer in conditional_pairs[:slots_available]:
                player_ids.append(cid)
                players.append(cplayer)
                conditional_player_ids_included.append(cid)

            # All remaining conditional players are excluded
            excluded_conditional_ids = [cid for cid, _ in conditional_pairs[slots_available:]]
        else:
            # 10+ regular players means all conditional players are excluded
            excluded_conditional_ids = list(all_conditional_ids)
        # `guild` and `guild_id` already computed before the match check
        mode = "pool"  # betting_mode.value if betting_mode else "pool"
        rs = rating_system.value if rating_system else "glicko"
        try:
            result = await asyncio.to_thread(
                functools.partial(self.match_service.shuffle_players,
                    player_ids, guild_id=guild_id, betting_mode=mode, rating_system=rs)
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

        # Determine if this is a bomb pot match (~10% chance)
        is_bomb_pot = random.random() < BOMB_POT_CHANCE

        # Store bomb pot status in pending state and persist to DB
        pending_state = self.match_service.get_last_shuffle(guild_id)
        if pending_state:
            pending_state["is_bomb_pot"] = is_bomb_pot
            # Persist to DB so bomb pot survives bot restart
            self.match_service._persist_match_state(guild_id, pending_state)

        if is_bomb_pot:
            logger.info(f"💣 BOMB POT triggered for guild {guild_id}")

        # Create auto-liquidity blind bets for pool mode
        blind_bets_result = None
        if mode == "pool":
            betting_service = getattr(self.bot, "betting_service", None)
            if betting_service:
                try:
                    # Get IDs and timestamp from the saved pending state
                    pending_state = self.match_service.get_last_shuffle(guild_id)
                    blind_bets_result = await asyncio.to_thread(
                        functools.partial(betting_service.create_auto_blind_bets,
                            guild_id=guild_id,
                            radiant_ids=pending_state["radiant_team_ids"],
                            dire_ids=pending_state["dire_team_ids"],
                            shuffle_timestamp=pending_state["shuffle_timestamp"],
                            is_bomb_pot=is_bomb_pot,
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
                                neon = getattr(self.bot, "neon_degen_service", None)
                                if neon:
                                    pool_total = blind_bets_result['total_radiant'] + blind_bets_result['total_dire']
                                    bomb_result = await neon.on_bomb_pot(
                                        guild_id, pool_total, blind_bets_result['created']
                                    )
                                    if bomb_result:
                                        await self._send_neon_result(interaction, bomb_result)
                            except Exception as e:
                                logger.debug(f"neon on_bomb_pot error: {e}")
                except Exception as exc:
                    logger.warning(f"Failed to create blind bets: {exc}", exc_info=True)

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

        # Build embed title with bomb pot banner if applicable
        if is_bomb_pot:
            embed_title = "💣 BOMB POT 💣 Balanced Team Shuffle"
            embed_color = discord.Color.orange()
        else:
            embed_title = "Balanced Team Shuffle"
            embed_color = discord.Color.blue()

        embed = discord.Embed(title=embed_title, color=embed_color)
        first_pick_emoji = "🟢" if first_pick_team == "Radiant" else "🔴"
        embed.add_field(
            name=f"🟢 Radiant ({radiant_sum:.0f})  |  🔴 Dire ({dire_sum:.0f})",
            value=f"{first_pick_emoji} **First Pick: {first_pick_team}**\n\n"
            + "\n".join(head_to_head),
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
            f"**Balanced with:** {rating_system_display}\n"
            f"**Goodness score:** {goodness_display} (lower = better)\n"
            f"**Value diff:** {value_diff:.0f}\n"
            f"**Off-role players:** Radiant: {radiant_off}, Dire: {dire_off} (Total: {radiant_off + dire_off})"
        )
        # Build excluded list - combine regular excluded + excluded conditional players
        all_excluded_names = []
        if excluded_ids:
            for pid in excluded_ids:
                player_obj = self.player_service.get_player(pid, guild_id)
                if player_obj:
                    display_name = get_player_display_name(player_obj, discord_id=pid, guild=guild)
                    all_excluded_names.append(display_name)
                else:
                    all_excluded_names.append(f"Unknown({pid})")

        # Add excluded conditional players with frogling emoji
        if excluded_conditional_ids:
            for pid in excluded_conditional_ids:
                player_obj = self.player_service.get_player(pid, guild_id)
                if player_obj:
                    display_name = get_player_display_name(player_obj, discord_id=pid, guild=guild)
                    all_excluded_names.append(f"{FROGLING_EMOTE} {display_name}")
                else:
                    all_excluded_names.append(f"{FROGLING_EMOTE} Unknown({pid})")

            # Give conditional players half the exclusion count bonus
            # (jopacoin bonus is awarded at record time in match_service)
            for pid in excluded_conditional_ids:
                self.player_service.increment_exclusion_count_half(pid, guild_id)

            # Store excluded conditional IDs in shuffle state for jopacoin bonus at record time
            pending_state = self.match_service.get_last_shuffle(guild_id)
            if pending_state:
                pending_state["excluded_conditional_player_ids"] = excluded_conditional_ids
                self.match_service.set_last_shuffle(guild_id, pending_state)

        if all_excluded_names:
            balance_info += f"\n**Excluded:** {', '.join(all_excluded_names)}"

        # Show conditional players who were pulled into the shuffle
        if conditional_player_ids_included:
            conditional_names = []
            for pid in conditional_player_ids_included:
                player_obj = self.player_service.get_player(pid, guild_id)
                if player_obj:
                    display_name = get_player_display_name(player_obj, discord_id=pid, guild=guild)
                    conditional_names.append(display_name)
                else:
                    conditional_names.append(f"Unknown({pid})")
            balance_info += f"\n{FROGLING_EMOTE} **Pulled from conditional:** {', '.join(conditional_names)}"
        embed.add_field(name="📊 Balance", value=balance_info, inline=False)

        # Betting instructions (mode-aware)
        if mode == "pool":
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

        # Show blind bet summary if any were created
        if blind_bets_result and blind_bets_result["created"] > 0:
            if is_bomb_pot:
                blind_note = (
                    f"💣 **BOMB POT:** All 10 players ante'd in! (10% + 10 {JOPACOIN_EMOTE} ante)\n"
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

        # Current wagers display
        betting_service = getattr(self.bot, "betting_service", None)
        totals = {"radiant": 0, "dire": 0}
        lock_until = None
        if betting_service:
            pending_state = self.match_service.get_last_shuffle(guild_id)
            totals = betting_service.get_pot_odds(guild_id, pending_state=pending_state)
            lock_until = pending_state.get("bet_lock_until") if pending_state else None

        wager_field_name, wager_field_value = format_betting_display(
            totals["radiant"], totals["dire"], mode, lock_until
        )
        embed.add_field(name=wager_field_name, value=wager_field_value, inline=False)

        # Add match quality indicators to footer (subtle display)
        glicko_prob = result.get("glicko_radiant_win_prob", 0.5)
        os_prob = result.get("openskill_radiant_win_prob", 0.5)
        embed.set_footer(text=f"{glicko_prob:.2f} {os_prob:.2f}")

        # Post shuffle embed to the lobby channel (dedicated channel where embed lives)
        lobby_channel_id = self.lobby_service.get_lobby_channel_id()
        message = None
        if lobby_channel_id:
            try:
                channel = self.bot.get_channel(lobby_channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(lobby_channel_id)
                message = await channel.send(embed=embed)
            except Exception as exc:
                logger.warning(f"Failed to post shuffle to lobby channel: {exc}")

        # Also post to command channel if different from lobby channel (dedicated channel setup)
        command_channel_id = interaction.channel.id if interaction.channel else None
        if command_channel_id and command_channel_id != lobby_channel_id:
            try:
                await interaction.channel.send(embed=embed)
            except Exception as exc:
                logger.warning(f"Failed to post shuffle to command channel: {exc}")

        # Send ephemeral confirmation to user
        await interaction.followup.send("✅ Teams shuffled!", ephemeral=True)

        # Save the shuffle message link so pending-match prompts can point to it
        # Capture origin_channel_id before reset_lobby clears it (needed for betting reminders)
        try:
            origin_channel_id = self.lobby_service.get_origin_channel_id()
            if message:
                jump_url = message.jump_url if hasattr(message, "jump_url") else None
                self.match_service.set_shuffle_message_info(
                    guild_id,
                    message_id=message.id,
                    channel_id=message.channel.id if message.channel else None,
                    jump_url=jump_url,
                    origin_channel_id=origin_channel_id,
                )
        except Exception as exc:
            logger.warning(f"Failed to store shuffle message URL: {exc}", exc_info=True)

        # Schedule betting reminders (5-minute warning and close) if applicable
        pending_state = self.match_service.get_last_shuffle(guild_id)
        bet_lock_until = pending_state.get("bet_lock_until") if pending_state else None
        await self._schedule_betting_reminders(guild_id, bet_lock_until)

        # Lock lobby thread and post shuffle results there too
        included_ids = []
        if pending_state:
            included_ids = pending_state.get("radiant_team_ids", []) + pending_state.get(
                "dire_team_ids", []
            )
        await self._lock_lobby_thread(
            guild_id,
            shuffle_embed=embed,
            included_player_ids=included_ids,
        )
        # Unpin from the lobby channel (may be dedicated channel, not interaction channel)
        lobby_channel_id = self.lobby_service.get_lobby_channel_id()
        lobby_channel = None
        if lobby_channel_id:
            try:
                lobby_channel = self.bot.get_channel(lobby_channel_id)
                if not lobby_channel:
                    lobby_channel = await self.bot.fetch_channel(lobby_channel_id)
            except Exception:
                lobby_channel = interaction.channel
        else:
            lobby_channel = interaction.channel
        await safe_unpin_all_bot_messages(lobby_channel, self.bot.user)
        await asyncio.to_thread(self.lobby_service.reset_lobby)

        # Clear lobby rally cooldowns
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
        is_admin = has_admin_permission(interaction)
        if result.value == "abort":
            if is_admin:
                await self._finalize_abort(interaction, guild_id, admin_override=True)
                return
            try:
                submission = await asyncio.to_thread(
                    functools.partial(self.match_service.add_abort_submission,
                        guild_id, interaction.user.id, is_admin=False)
                )
            except ValueError as exc:
                await interaction.followup.send(f"❌ {exc}", ephemeral=True)
                return
            if not submission["is_ready"]:
                min_subs = self.match_service.MIN_NON_ADMIN_SUBMISSIONS
                await interaction.followup.send(
                    f"✅ Abort request recorded. Non-admin submissions: {submission['non_admin_count']}/{min_subs} "
                    f"(admins do not count toward the minimum).\nRequires {min_subs} abort confirmations.",
                    ephemeral=True,
                )
                return
            await self._finalize_abort(interaction, guild_id, admin_override=False)
            return
        try:
            submission = await asyncio.to_thread(
                self.match_service.add_record_submission,
                guild_id, interaction.user.id, result.value, is_admin
            )
        except ValueError as exc:
            await interaction.followup.send(f"❌ {exc}", ephemeral=True)
            return

        if not submission["is_ready"]:
            vote_counts = submission.get("vote_counts", {"radiant": 0, "dire": 0})
            min_subs = self.match_service.MIN_NON_ADMIN_SUBMISSIONS
            admin_note = " (admins do not count toward the minimum)" if is_admin else ""
            confirmations_text = f"🟢 Radiant: {vote_counts['radiant']}/{min_subs} | 🔴 Dire: {vote_counts['dire']}/{min_subs}"
            await interaction.followup.send(
                f"✅ Result recorded for {result.name}.{admin_note}\n{confirmations_text}\nRequires {min_subs} confirmations.",
                ephemeral=True,
            )
            return

        winning_result = submission["result"]

        # Save thread_id before record_match clears the pending state
        pending_state = self.match_service.get_last_shuffle(guild_id)
        thread_id_for_finalize = (
            pending_state.get("thread_shuffle_thread_id") if pending_state else None
        )

        try:
            record_result = await asyncio.to_thread(
                functools.partial(self.match_service.record_match,
                    winning_result, guild_id=guild_id, dotabuff_match_id=dotabuff_match_id)
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

        distributions = record_result.get("bet_distributions", {})
        winners = distributions.get("winners", [])
        losers = distributions.get("losers", [])
        distribution_text = ""
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

            for uid, user_bets in winners_by_user.items():
                total_payout = sum(b["payout"] for b in user_bets)
                multiplier = user_bets[0].get("multiplier")  # Same for all bets in pool mode

                if len(user_bets) == 1:
                    # Single bet - original display
                    bet = user_bets[0]
                    leverage = bet.get("leverage", 1) or 1
                    leverage_text = f" at {leverage}x" if leverage > 1 else ""
                    if multiplier:
                        distribution_lines.append(
                            f"<@{uid}> won {total_payout} {JOPACOIN_EMOTE} "
                            f"(bet {bet['amount']}{leverage_text}, {multiplier:.2f}x)"
                        )
                    else:
                        distribution_lines.append(
                            f"<@{uid}> won {total_payout} {JOPACOIN_EMOTE} "
                            f"(bet {bet['amount']}{leverage_text})"
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
                            f"<@{uid}> won {total_payout} {JOPACOIN_EMOTE} "
                            f"(bets: {bets_str}, {multiplier:.2f}x)"
                        )
                    else:
                        distribution_lines.append(
                            f"<@{uid}> won {total_payout} {JOPACOIN_EMOTE} (bets: {bets_str})"
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
            distribution_text = "\n" + "\n".join(distribution_lines)

        # Add stake distribution info (draft mode only)
        stake_distributions = record_result.get("stake_distributions", {})
        if stake_distributions.get("enabled") and stake_distributions.get("winners"):
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

            distribution_text += stake_text

        # Generate AI flavor text — pick targets synchronously, fire LLM calls in parallel
        ai_flavor = None
        match_flavor = None
        notable_bettor = None  # Track for exclusion from match flavor
        if self.flavor_text_service:
            try:
                # --- Synchronous: pick notable bettor ---
                flavor_event = None
                event_details = {}
                if winners or losers:
                    leveraged_losers = [
                        entry
                        for entry in losers
                        if (entry.get("leverage", 1) or 1) > 1 and not entry.get("refunded")
                    ]
                    if leveraged_losers:
                        notable_bettor = max(
                            leveraged_losers, key=lambda e: e.get("effective_bet", e["amount"])
                        )
                        flavor_event = FlavorEvent.LEVERAGE_LOSS
                        event_details = {
                            "amount": notable_bettor["amount"],
                            "leverage": notable_bettor.get("leverage", 1),
                            "effective_loss": notable_bettor.get(
                                "effective_bet", notable_bettor["amount"]
                            ),
                            "team": notable_bettor.get("team", "unknown"),
                        }
                    elif winners:
                        notable_bettor = max(winners, key=lambda e: e.get("payout", 0))
                        flavor_event = FlavorEvent.BET_WON
                        event_details = {
                            "amount": notable_bettor["amount"],
                            "payout": notable_bettor.get("payout", 0),
                            "leverage": notable_bettor.get("leverage", 1),
                            "team": notable_bettor.get("team", "unknown"),
                            "multiplier": notable_bettor.get("multiplier"),
                        }

                # --- Synchronous: pick notable winner ---
                bettor_id = notable_bettor["discord_id"] if notable_bettor else None
                notable_winner = None
                match_details = {}
                if record_result.get("winning_player_ids"):
                    notable_winner, match_details = self._get_notable_winner(
                        match_id=record_result["match_id"],
                        winning_ids=record_result["winning_player_ids"],
                        exclude_id=bettor_id,
                    )

                # --- Parallel: fire both LLM calls concurrently ---
                async def _gen_bet_flavor():
                    if not (notable_bettor and flavor_event):
                        return None
                    result = await self.flavor_text_service.generate_event_flavor(
                        guild_id=guild_id,
                        event=flavor_event,
                        discord_id=notable_bettor["discord_id"],
                        event_details=event_details,
                    )
                    if result:
                        return f"<@{notable_bettor['discord_id']}> {result}"
                    return None

                async def _gen_match_flavor():
                    if not notable_winner:
                        return None
                    result = await self.flavor_text_service.generate_event_flavor(
                        guild_id=guild_id,
                        event=FlavorEvent.MATCH_WIN,
                        discord_id=notable_winner,
                        event_details=match_details,
                    )
                    if result:
                        return f"<@{notable_winner}> {result}"
                    return None

                ai_flavor, match_flavor = await asyncio.gather(
                    _gen_bet_flavor(), _gen_match_flavor()
                )
            except Exception as e:
                logger.warning(f"Failed to generate AI flavor: {e}")

        if ai_flavor:
            distribution_text += f"\n\n💬 {ai_flavor}"
        if match_flavor:
            distribution_text += f"\n\n🎮 {match_flavor}"

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
        await interaction.followup.send(message, ephemeral=False)

        # Finalize lobby thread with results (use saved thread_id since pending state is cleared)
        await self._finalize_lobby_thread(
            guild_id, winning_result, thread_id=thread_id_for_finalize
        )

        # Neon Degen Terminal hook (match recorded footer / streak)
        from services.neon_degen_service import NeonDegenService
        _neon_svc = getattr(self.bot, "neon_degen_service", None)
        neon = _neon_svc if isinstance(_neon_svc, NeonDegenService) else None
        if neon:
            try:
                async def _send_neon_msg(neon_result):
                    if neon_result is None:
                        return
                    msg = None
                    if neon_result.gif_file:
                        gif_file = discord.File(neon_result.gif_file, filename="jopat_terminal.gif")
                        if neon_result.text_block:
                            msg = await interaction.channel.send(neon_result.text_block, file=gif_file)
                        else:
                            msg = await interaction.channel.send(file=gif_file)
                    elif neon_result.text_block:
                        msg = await interaction.channel.send(neon_result.text_block)
                    elif neon_result.footer_text:
                        msg = await interaction.channel.send(neon_result.footer_text)
                    if msg:
                        async def _delete_after(m, delay):
                            try:
                                await asyncio.sleep(delay)
                                await m.delete()
                            except Exception:
                                pass
                        asyncio.create_task(_delete_after(msg, 60))

                neon_result = await neon.on_match_recorded(
                    guild_id,
                    streak_data=record_result.get("notable_streak"),
                )
                await _send_neon_msg(neon_result)

                # Wire on_bet_settled + on_leverage_loss for losers (max ONE per match)
                neon_sent = neon_result is not None
                if not neon_sent and losers:
                    for entry in losers:
                        if entry.get("refunded"):
                            continue
                        loser_id = entry["discord_id"]
                        leverage = entry.get("leverage", 1) or 1
                        amount = entry.get("effective_bet", entry["amount"])
                        new_bal = self.player_service.get_balance(loser_id, guild_id)

                        # on_leverage_loss: 5x leverage into debt
                        if leverage >= 5 and new_bal < 0:
                            lr = await neon.on_leverage_loss(
                                loser_id, guild_id, amount, leverage, new_bal
                            )
                            if lr:
                                await _send_neon_msg(lr)
                                neon_sent = True
                                break

                        # on_bet_settled: hit MAX_DEBT or zero
                        from config import MAX_DEBT as _MAX_DEBT
                        if new_bal <= -_MAX_DEBT or new_bal == 0:
                            sr = await neon.on_bet_settled(
                                loser_id, guild_id, won=False, payout=0, new_balance=new_bal
                            )
                            if sr:
                                await _send_neon_msg(sr)
                                neon_sent = True
                                break

                # Wire on_degen_milestone for losers
                if not neon_sent and losers:
                    for entry in losers:
                        if entry.get("refunded"):
                            continue
                        loser_id = entry["discord_id"]
                        degen_score = neon._get_degen_score(loser_id, guild_id)
                        if degen_score is not None and degen_score >= 90:
                            mr = await neon.on_degen_milestone(loser_id, guild_id, degen_score)
                            if mr:
                                await _send_neon_msg(mr)
                                break

                # Easter egg hooks: games milestones
                easter_data = record_result.get("easter_egg_data", {})
                events_fired = []  # Track events for simultaneous detection

                for milestone in easter_data.get("games_milestones", []):
                    gm_result = await neon.on_games_milestone(
                        milestone["discord_id"],
                        guild_id,
                        milestone["total_games"],
                    )
                    if gm_result:
                        events_fired.append("games_milestone")
                    await _send_neon_msg(gm_result)

                # Easter egg hooks: win streak records
                for streak_rec in easter_data.get("win_streak_records", []):
                    ws_result = await neon.on_win_streak_record(
                        streak_rec["discord_id"],
                        guild_id,
                        streak_rec["current_streak"],
                        streak_rec["previous_best"],
                    )
                    if ws_result:
                        events_fired.append("win_streak_record")
                    await _send_neon_msg(ws_result)

                # Easter egg hooks: rivalries detected
                for rivalry in easter_data.get("rivalries_detected", []):
                    rv_result = await neon.on_rivalry_detected(
                        guild_id,
                        rivalry["player1_id"],
                        rivalry["player2_id"],
                        rivalry["games_together"],
                        rivalry["winrate_vs"],
                    )
                    if rv_result:
                        events_fired.append("rivalry_detected")
                    await _send_neon_msg(rv_result)

                # Check for simultaneous events
                if len(events_fired) >= 2:
                    sim_result = await neon.on_simultaneous_events(guild_id, events_fired)
                    await _send_neon_msg(sim_result)

            except Exception as exc:
                logger.debug(f"Neon match hook failed: {exc}")

        # Trigger auto-discovery in background if enabled
        match_id = record_result.get("match_id")
        if match_id:
            asyncio.create_task(
                self._trigger_auto_discovery(guild_id, match_id, interaction.channel)
            )

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
                auto_enrich = self.guild_config_service.is_auto_enrich_enabled(guild_id)
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

                # Run discovery in executor to avoid blocking
                loop = asyncio.get_event_loop()
                result = await loop.run_in_executor(
                    None, discovery_service.discover_match, match_id
                )

                status = result.get("status")

                if status == "discovered":
                    # Success - send enriched embed
                    await self._send_enrichment_result(channel, match_id, result)
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
            match_data = self.match_service.get_match_by_id(match_id)
            participants = self.match_service.get_match_participants(match_id)

            if match_data and participants:
                radiant = [p for p in participants if p.get("side") == "radiant"]
                dire = [p for p in participants if p.get("side") == "dire"]

                embed = create_enriched_match_embed(
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
                )

                await channel.send(
                    f"📊 Match #{match_id} auto-enriched ({confidence:.0%} confidence)",
                    embed=embed,
                )
            else:
                # Fallback to simple message
                await channel.send(
                    f"📊 Match #{match_id} auto-enriched! "
                    f"(Dota Match ID: {valve_match_id}, {confidence:.0%} confidence)"
                )
        except Exception as exc:
            logger.warning(f"Failed to send enrichment result: {exc}")

    async def _finalize_abort(
        self, interaction: discord.Interaction, guild_id: int | None, admin_override: bool
    ):
        betting_service = getattr(self.bot, "betting_service", None)
        pending_state = self.match_service.get_last_shuffle(guild_id)
        if betting_service and pending_state:
            try:
                betting_service.refund_pending_bets(guild_id, pending_state)
            except Exception as exc:
                logger.error(f"Error refunding pending bets on abort: {exc}", exc_info=True)
        # Cancel any pending betting reminders
        self._cancel_betting_tasks(guild_id)

        # Update channel message to show closed and archive thread
        await self._update_channel_message_closed("Match Aborted")
        await self._abort_lobby_thread(guild_id)

        self.match_service.clear_last_shuffle(guild_id)
        await safe_unpin_all_bot_messages(interaction.channel, self.bot.user)
        await asyncio.to_thread(self.lobby_service.reset_lobby)

        # Clear lobby rally cooldowns
        from bot import clear_lobby_rally_cooldowns
        clear_lobby_rally_cooldowns(guild_id or 0)

        await interaction.followup.send(
            "✅ Match aborted. You can create a new lobby.", ephemeral=False
        )

    async def _schedule_betting_reminders(
        self, guild_id: int | None, bet_lock_until: int | None
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

        tasks = []

        # 5-minute warning (only if more than 5 minutes remain)
        warn_delay = seconds_until_close - 300
        if warn_delay > 0:
            tasks.append(
                asyncio.create_task(
                    self._run_bet_reminder_after_delay(
                        delay_seconds=warn_delay,
                        guild_id=guild_id,
                        reminder_type="warning",
                        lock_until=bet_lock_until,
                    )
                )
            )

        # Close reminder
        tasks.append(
            asyncio.create_task(
                self._run_bet_reminder_after_delay(
                    delay_seconds=seconds_until_close,
                    guild_id=guild_id,
                    reminder_type="closed",
                    lock_until=bet_lock_until,
                )
            )
        )

        self._register_betting_tasks(guild_id, tasks)

    async def _run_bet_reminder_after_delay(
        self,
        *,
        delay_seconds: int,
        guild_id: int | None,
        reminder_type: str,
        lock_until: int | None,
    ) -> None:
        """Sleep for delay_seconds then send the requested reminder, if still relevant."""
        try:
            await asyncio.sleep(delay_seconds)
            # Ensure pending state still matches the expected lock time
            state = self.match_service.get_last_shuffle(guild_id)
            if not state:
                return
            current_lock = state.get("bet_lock_until")
            if not current_lock or (lock_until and current_lock != lock_until):
                return

            betting_cog = self.bot.get_cog("BettingCommands")
            if betting_cog is None or not hasattr(betting_cog, "_send_betting_reminder"):
                return

            await betting_cog._send_betting_reminder(
                guild_id,
                reminder_type=reminder_type,
                lock_until=lock_until,
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
        normalized = self.match_service._normalize_guild_id(guild_id)  # type: ignore[attr-defined]
        self._betting_tasks_by_guild[normalized] = tasks

    def _cancel_betting_tasks(self, guild_id: int | None) -> None:
        """Cancel any scheduled betting reminder tasks for the guild."""
        normalized = self.match_service._normalize_guild_id(guild_id)  # type: ignore[attr-defined]
        tasks = self._betting_tasks_by_guild.pop(normalized, [])
        for task in tasks:
            try:
                task.cancel()
            except Exception:
                continue


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    match_service = getattr(bot, "match_service", None)
    player_service = getattr(bot, "player_service", None)
    guild_config_service = getattr(bot, "guild_config_service", None)
    bankruptcy_repo = getattr(bot, "bankruptcy_repo", None)
    flavor_text_service = getattr(bot, "flavor_text_service", None)
    await bot.add_cog(
        MatchCommands(
            bot,
            lobby_service,
            match_service,
            player_service,
            guild_config_service=guild_config_service,
            bankruptcy_repo=bankruptcy_repo,
            flavor_text_service=flavor_text_service,
        )
    )
