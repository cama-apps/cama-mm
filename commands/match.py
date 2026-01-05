"""
Match commands: /shuffle and /record.
"""

import asyncio
import logging
import time

import discord
from discord import app_commands
from discord.ext import commands

from config import JOPACOIN_MIN_BET
from services.lobby_service import LobbyService
from services.match_discovery_service import MatchDiscoveryService
from services.match_service import MatchService
from services.permissions import has_admin_permission
from utils.embeds import create_enriched_match_embed
from utils.formatting import (
    JOPACOIN_EMOTE,
    ROLE_EMOJIS,
    ROLE_NAMES,
    format_betting_display,
    get_player_display_name,
)
from utils.interaction_safety import safe_defer
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
        guild_config_repo=None,
        player_repo=None,
        match_repo=None,
        bankruptcy_repo=None,
    ):
        self.bot = bot
        self.lobby_service = lobby_service
        self.match_service = match_service
        self.player_service = player_service
        self.guild_config_repo = guild_config_repo
        self.player_repo = player_repo
        self.match_repo = match_repo
        self.bankruptcy_repo = bankruptcy_repo
        # Track scheduled betting reminder tasks per guild for cleanup
        self._betting_tasks_by_guild = {}

    async def _safe_unpin(
        self, channel: discord.abc.Messageable | None, message_id: int | None
    ) -> None:
        """Unpin the lobby message safely, tolerating missing perms or missing message."""
        if not channel or not message_id:
            return
        try:
            message = await channel.fetch_message(message_id)
        except Exception as exc:
            logger.warning(f"Failed to fetch lobby message for unpin: {exc}")
            return

        try:
            await message.unpin(reason="Cama lobby closed")
        except discord.Forbidden:
            logger.warning("Cannot unpin lobby message: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to unpin lobby message: {exc}")

    def _format_team_lines(self, team, roles, player_ids, players, guild):
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
            lines.append(f"{role_emoji} {name_part} ({role_name}) [{rating}]{warn}")
        return lines

    @app_commands.command(name="shuffle", description="Create balanced teams from lobby")
    @app_commands.describe(
        betting_mode="Betting mode: House (1:1 fixed odds) or Pool (user-determined odds)",
    )
    @app_commands.choices(
        betting_mode=[
            app_commands.Choice(name="House (1:1)", value="house"),
            app_commands.Choice(name="Pool (user odds)", value="pool"),
        ]
    )
    async def shuffle(
        self,
        interaction: discord.Interaction,
        betting_mode: app_commands.Choice[str] = None,
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

        lobby = self.lobby_service.get_lobby()
        if not lobby:
            await interaction.followup.send(
                "❌ No active lobby. Use `/lobby` to create one!", ephemeral=True
            )
            return

        if lobby.get_player_count() < 10:
            await interaction.followup.send(
                f"❌ Need at least 10 players in lobby. Currently {lobby.get_player_count()}/10.",
                ephemeral=True,
            )
            return

        player_ids, players = self.lobby_service.get_lobby_players(lobby)
        # `guild` and `guild_id` already computed before the match check
        mode = betting_mode.value if betting_mode else "house"
        try:
            result = self.match_service.shuffle_players(
                player_ids, guild_id=guild_id, betting_mode=mode
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
        radiant_sum = sum(
            player.get_value(self.match_service.use_glicko) for player in radiant_team.players
        )
        dire_sum = sum(
            player.get_value(self.match_service.use_glicko) for player in dire_team.players
        )
        first_pick_team = result["first_pick_team"]
        excluded_ids = result["excluded_ids"]

        radiant_lines = self._format_team_lines(
            radiant_team, radiant_roles, player_ids, players, guild
        )
        dire_lines = self._format_team_lines(dire_team, dire_roles, player_ids, players, guild)

        # Sort by role number for cleaner view
        radiant_sorted = sorted(zip(radiant_roles, radiant_lines), key=lambda x: int(x[0]))
        dire_sorted = sorted(zip(dire_roles, dire_lines), key=lambda x: int(x[0]))

        head_to_head = []
        for (_r_role, r_line), (_d_role, d_line) in zip(radiant_sorted, dire_sorted):
            head_to_head.append(f"{r_line}  |  {d_line}")

        embed = discord.Embed(title="Balanced Team Shuffle", color=discord.Color.blue())
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
        balance_info = (
            f"**Goodness score:** {goodness_display} (lower = better)\n"
            f"**Value diff:** {value_diff:.0f}\n"
            f"**Off-role players:** Radiant: {radiant_off}, Dire: {dire_off} (Total: {radiant_off + dire_off})"
        )
        if excluded_ids:
            excluded_names = []
            for pid in excluded_ids:
                player_obj = self.player_service.get_player(pid)
                if player_obj:
                    display_name = get_player_display_name(player_obj, discord_id=pid, guild=guild)
                    excluded_names.append(display_name)
                else:
                    excluded_names.append(f"Unknown({pid})")
            balance_info += f"\n**Excluded:** {', '.join(excluded_names)}"
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

        message = await interaction.followup.send(embed=embed)

        # Save the shuffle message link so pending-match prompts can point to it
        try:
            if message:
                jump_url = message.jump_url if hasattr(message, "jump_url") else None
                self.match_service.set_shuffle_message_info(
                    guild_id,
                    message_id=message.id,
                    channel_id=message.channel.id if message.channel else None,
                    jump_url=jump_url,
                )
        except Exception as exc:
            logger.warning(f"Failed to store shuffle message URL: {exc}", exc_info=True)

        # Schedule betting reminders (5-minute warning and close) if applicable
        pending_state = self.match_service.get_last_shuffle(guild_id)
        bet_lock_until = pending_state.get("bet_lock_until") if pending_state else None
        await self._schedule_betting_reminders(guild_id, bet_lock_until)

        # Reset lobby after shuffle
        await self._safe_unpin(interaction.channel, self.lobby_service.get_lobby_message_id())
        self.lobby_service.reset_lobby()

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
                submission = self.match_service.add_abort_submission(
                    guild_id, interaction.user.id, is_admin=False
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
            submission = self.match_service.add_record_submission(
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
        try:
            record_result = self.match_service.record_match(
                winning_result, guild_id=guild_id, dotabuff_match_id=dotabuff_match_id
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
        Trigger auto-discovery for a match in the background.

        Checks if auto_enrich is enabled, then attempts to find and enrich
        the Dota 2 match ID using player match histories from OpenDota.
        """
        try:
            # Check if auto_enrich is enabled for this guild
            if self.guild_config_repo:
                auto_enrich = self.guild_config_repo.get_auto_enrich(guild_id or 0)
                if not auto_enrich:
                    logger.debug(f"Auto-enrich disabled for guild {guild_id}, skipping discovery")
                    return

            # Need repos to create discovery service
            if not self.match_repo or not self.player_repo:
                logger.warning("Cannot auto-discover: missing match_repo or player_repo")
                return

            # Small delay to let OpenDota parse the match (matches take time to appear)
            await asyncio.sleep(60)  # Wait 1 minute before trying

            # Run discovery in executor to avoid blocking
            discovery_service = MatchDiscoveryService(self.match_repo, self.player_repo)

            loop = asyncio.get_event_loop()
            result = await loop.run_in_executor(None, discovery_service.discover_match, match_id)

            if result.get("status") == "discovered":
                valve_match_id = result.get("valve_match_id")
                confidence = result.get("confidence", 0)
                logger.info(
                    f"Auto-discovered match {match_id} -> valve_match_id={valve_match_id} "
                    f"(confidence: {confidence:.0%})"
                )
                if channel:
                    try:
                        # Fetch enriched match data for embed
                        match_data = self.match_repo.get_match(match_id)
                        participants = self.match_repo.get_match_participants(match_id)

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
                    except Exception:
                        pass  # Ignore if we can't send message
            else:
                logger.debug(
                    f"Auto-discovery for match {match_id}: {result.get('status')} "
                    f"(best confidence: {result.get('confidence', 0):.0%})"
                )

        except Exception as exc:
            logger.error(f"Error in auto-discovery for match {match_id}: {exc}", exc_info=True)

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
        self.match_service.clear_last_shuffle(guild_id)
        await self._safe_unpin(interaction.channel, self.lobby_service.get_lobby_message_id())
        self.lobby_service.reset_lobby()
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
    guild_config_repo = getattr(bot, "guild_config_repo", None)
    player_repo = getattr(bot, "player_repo", None)
    match_repo = getattr(bot, "match_repo", None)
    bankruptcy_repo = getattr(bot, "bankruptcy_repo", None)
    await bot.add_cog(
        MatchCommands(
            bot,
            lobby_service,
            match_service,
            player_service,
            guild_config_repo=guild_config_repo,
            player_repo=player_repo,
            match_repo=match_repo,
            bankruptcy_repo=bankruptcy_repo,
        )
    )
