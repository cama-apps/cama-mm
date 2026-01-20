"""
Betting commands for jopacoin wagers.
"""

from __future__ import annotations

import logging
import random
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

if TYPE_CHECKING:
    from services.flavor_text_service import FlavorTextService

from services.flavor_text_service import FlavorEvent

from config import (
    BANKRUPTCY_PENALTY_RATE,
    GARNISHMENT_PERCENTAGE,
    JOPACOIN_MIN_BET,
    LOAN_FEE_RATE,
    MAX_DEBT,
)
from config import DISBURSE_MIN_FUND
from services.bankruptcy_service import BankruptcyService
from services.betting_service import BettingService
from services.disburse_service import DisburseService
from services.gambling_stats_service import GamblingStatsService
from services.loan_service import LoanService
from services.match_service import MatchService
from services.permissions import has_admin_permission
from services.player_service import PlayerService
from utils.drawing import draw_gamba_chart
from utils.formatting import JOPACOIN_EMOTE, TOMBSTONE_EMOJI, format_betting_display
from utils.interaction_safety import safe_defer
from utils.rate_limiter import GLOBAL_RATE_LIMITER

logger = logging.getLogger("cama_bot.commands.betting")


# Snarky messages for those who don't deserve bankruptcy
BANKRUPTCY_DENIED_MESSAGES = [
    "You're not actually in debt. Nice try, freeloader.",
    "Bankruptcy is for degenerates who lost it all. You still have coins.",
    "You're trying to declare bankruptcy while being solvent? The audacity.",
    "ERROR: Wealth detected. Cannot process bankruptcy request.",
    "Your application for financial ruin has been denied. You're too rich.",
    "Sorry, this service is exclusively for people who made terrible decisions.",
    "The Jopacoin Bankruptcy Court rejects your attempt to game the system.",
    "Imagine trying to go bankrupt when you have money. Couldn't be you.",
]

BANKRUPTCY_COOLDOWN_MESSAGES = [
    "You already declared bankruptcy recently. The court isn't buying it again so soon.",
    "Nice try, but your credit score hasn't recovered from the last bankruptcy.",
    "The Jopacoin Financial Recovery Board says you need to wait longer.",
    "Bankruptcy addiction is real. Seek help. And try again later.",
    "One bankruptcy per week, please. We have standards.",
    "Your previous bankruptcy paperwork hasn't even finished processing yet.",
    "The judge remembers you. Come back when they've forgotten.",
]

BANKRUPTCY_SUCCESS_MESSAGES = [
    "Congratulations on your complete financial ruin. Your debt has been erased, but at what cost?",
    "The court has granted your bankruptcy. Your ancestors weep.",
    "Chapter 7 approved. Your jopacoin legacy dies here.",
    "Debt cleared. Dignity? Also cleared. For the next {games} games, you'll earn only {rate}% of win bonuses.",
    "The Jopacoin Federal Reserve takes note of another fallen gambler. Debt erased.",
    "Your bankruptcy filing has been accepted. The house always wins, but at least you don't owe it anymore.",
    "Financial rock bottom achieved. Welcome to the Bankruptcy Hall of Shame.",
    "Your debt of {debt} jopacoin has been forgiven. You're now starting from almost nothing. Again.",
]

LOAN_SUCCESS_MESSAGES = [
    "The bank approves your request. {amount} {emote} deposited. You now owe {owed}. Good luck.",
    "Money acquired. Dignity sacrificed. {amount} {emote} in, {owed} to repay. The cycle continues.",
    "Loan approved. {amount} {emote} hits your account. Don't spend it all in one bet. (You will.)",
    "The Jopacoin Lending Co. smiles upon you. {amount} {emote} granted. {fee} {emote} goes to charity.",
    "Fresh jopacoin, fresh start, same gambling addiction. {amount} {emote} received.",
]

LOAN_DENIED_COOLDOWN_MESSAGES = [
    "You just took a loan! The bank needs time to process your crippling debt.",
    "One loan every 3 days. We have to pretend we're responsible lenders.",
    "Your loan application is on cooldown. Maybe reflect on your choices.",
    "The Jopacoin Bank says: 'Come back later, we're still counting your last loan's fees.'",
]

LOAN_DENIED_DEBT_MESSAGES = [
    "You're already too deep in debt. Even we have standards.",
    "Loan denied. Your credit is worse than your gambling decisions.",
    "The bank has reviewed your finances and respectfully declined to make things worse.",
    "ERROR: Maximum debt capacity reached. Try bankruptcy first.",
]

# Special messages for peak degen behavior: taking a loan while already in debt
NEGATIVE_LOAN_MESSAGES = [
    "You... you took out a loan while already in debt. The money went straight to your creditors. "
    "You're now even MORE in debt. Congratulations, you absolute degenerate.",
    "LEGENDARY MOVE: Borrowing money just to owe MORE money. "
    "Your financial advisor has left the country. True degen behavior.",
    "The loan was approved and immediately garnished. You gained nothing but more debt and our respect. "
    "This is galaxy-brain degeneracy.",
    "You borrowed {amount} {emote} while broke. Net result: deeper in the hole. "
    "The degen energy radiating from this decision is immeasurable.",
    "This is advanced degeneracy. You can't even gamble with this money because you're still negative. "
    "But you did it anyway. We're impressed and horrified.",
]


class BettingCommands(commands.Cog):
    """Slash commands to place and view wagers."""

    def __init__(
        self,
        bot: commands.Bot,
        betting_service: BettingService,
        match_service: MatchService,
        player_service: PlayerService,
        bankruptcy_service: BankruptcyService | None = None,
        gambling_stats_service: GamblingStatsService | None = None,
        loan_service: LoanService | None = None,
        disburse_service: DisburseService | None = None,
        flavor_text_service: FlavorTextService | None = None,
    ):
        self.bot = bot
        self.betting_service = betting_service
        self.match_service = match_service
        self.player_service = player_service
        self.bankruptcy_service = bankruptcy_service
        self.flavor_text_service = flavor_text_service
        self.gambling_stats_service = gambling_stats_service
        self.loan_service = loan_service
        self.disburse_service = disburse_service

    async def _update_shuffle_message_wagers(self, guild_id: int | None) -> None:
        """
        Refresh the shuffle message's wager field with current totals.
        Updates both the main channel message and the thread copy.
        """
        pending_state = self.match_service.get_last_shuffle(guild_id)
        if not pending_state:
            return

        # Get betting display info
        totals = self.betting_service.get_pot_odds(guild_id, pending_state=pending_state)
        lock_until = pending_state.get("bet_lock_until")
        betting_mode = pending_state.get("betting_mode", "pool")
        field_name, field_value = format_betting_display(
            totals["radiant"], totals["dire"], betting_mode, lock_until
        )

        # Update main channel message
        message_info = self.match_service.get_shuffle_message_info(guild_id)
        message_id = message_info.get("message_id") if message_info else None
        channel_id = message_info.get("channel_id") if message_info else None
        if message_id and channel_id:
            await self._update_embed_betting_field(channel_id, message_id, field_name, field_value)

        # Update thread message if it exists
        thread_message_id = pending_state.get("thread_shuffle_message_id")
        thread_id = pending_state.get("thread_shuffle_thread_id")
        if thread_message_id and thread_id:
            await self._update_embed_betting_field(thread_id, thread_message_id, field_name, field_value)

    async def _update_embed_betting_field(
        self, channel_id: int, message_id: int, field_name: str, field_value: str
    ) -> None:
        """Helper to update the betting field in an embed message."""
        try:
            channel = self.bot.get_channel(channel_id)
            if channel is None:
                channel = await self.bot.fetch_channel(channel_id)
            if channel is None:
                return

            message = await channel.fetch_message(message_id)
            if not message or not message.embeds:
                return

            embed = message.embeds[0]
            embed_dict = embed.to_dict()
            fields = embed_dict.get("fields", [])

            # Known wager field names to look for
            wager_field_names = {"💰 Pool Betting", "💰 House Betting (1:1)", "💰 Betting"}

            # Find and update wager field, remove duplicates
            updated = False
            new_fields = []
            for field in fields:
                fname = field.get("name", "")
                if fname in wager_field_names:
                    if not updated:
                        # Update the first matching wager field
                        field["name"] = field_name
                        field["value"] = field_value
                        new_fields.append(field)
                        updated = True
                    # Skip duplicates (don't add them to new_fields)
                else:
                    new_fields.append(field)

            if not updated:
                new_fields.append({"name": field_name, "value": field_value, "inline": False})
            embed_dict["fields"] = new_fields

            new_embed = discord.Embed.from_dict(embed_dict)
            await message.edit(embed=new_embed, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to update shuffle wagers: {exc}", exc_info=True)

    async def _send_betting_reminder(
        self,
        guild_id: int | None,
        *,
        reminder_type: str,
        lock_until: int | None,
    ) -> None:
        """
        Send a reminder message replying to the shuffle embed with current bet totals.

        reminder_type: "warning" (5 minutes left) or "closed" (betting closed).
        """
        pending_state = self.match_service.get_last_shuffle(guild_id)
        if not pending_state:
            return

        message_info = self.match_service.get_shuffle_message_info(guild_id)
        message_id = message_info.get("message_id") if message_info else None
        channel_id = message_info.get("channel_id") if message_info else None
        thread_message_id = message_info.get("thread_message_id") if message_info else None
        thread_id = message_info.get("thread_id") if message_info else None

        totals = self.betting_service.get_pot_odds(guild_id, pending_state=pending_state)
        betting_mode = pending_state.get("betting_mode", "pool")

        # Format bets with odds for pool mode
        _, totals_text = format_betting_display(
            totals["radiant"], totals["dire"], betting_mode, lock_until=None
        )
        mode_label = "Pool" if betting_mode == "pool" else "House (1:1)"

        if reminder_type == "warning":
            if not lock_until:
                return
            content = (
                f"⏰ **5 minutes remaining until betting closes!** (<t:{int(lock_until)}:R>)\n"
                f"Mode: {mode_label}\n\n"
                f"Current bets:\n{totals_text}"
            )
        elif reminder_type == "closed":
            content = (
                f"🔒 **Betting is now closed!**\n"
                f"Mode: {mode_label}\n\n"
                f"Final bets:\n{totals_text}"
            )
        else:
            return

        # Post to channel
        if message_id and channel_id:
            try:
                channel = self.bot.get_channel(channel_id)
                if channel is None:
                    channel = await self.bot.fetch_channel(channel_id)
                if channel:
                    message = await channel.fetch_message(message_id)
                    if message:
                        await message.reply(content, allowed_mentions=discord.AllowedMentions.none())
            except Exception as exc:
                logger.warning(f"Failed to send betting reminder to channel: {exc}", exc_info=True)

        # Post to thread
        if thread_message_id and thread_id:
            try:
                thread = self.bot.get_channel(thread_id)
                if thread is None:
                    thread = await self.bot.fetch_channel(thread_id)
                if thread:
                    thread_message = await thread.fetch_message(thread_message_id)
                    if thread_message:
                        await thread_message.reply(content, allowed_mentions=discord.AllowedMentions.none())
            except Exception as exc:
                logger.warning(f"Failed to send betting reminder to thread: {exc}", exc_info=True)

    @app_commands.command(
        name="bet",
        description="Place a jopacoin bet on the current match (check balance with /balance)",
    )
    @app_commands.describe(
        team="Radiant or Dire",
        amount="Amount of jopacoin to wager (view balance with /balance)",
        leverage="Leverage multiplier (2x, 3x, 5x) - can cause debt!",
    )
    @app_commands.choices(
        team=[
            app_commands.Choice(name="Radiant", value="radiant"),
            app_commands.Choice(name="Dire", value="dire"),
        ],
        leverage=[
            app_commands.Choice(name="None (1x)", value=1),
            app_commands.Choice(name="2x", value=2),
            app_commands.Choice(name="3x", value=3),
            app_commands.Choice(name="5x", value=5),
        ],
    )
    async def bet(
        self,
        interaction: discord.Interaction,
        team: app_commands.Choice[str],
        amount: int,
        leverage: app_commands.Choice[int] = None,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="bet",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=20,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/bet` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return
        guild_id = interaction.guild.id if interaction.guild else None
        user_id = interaction.user.id

        if amount < JOPACOIN_MIN_BET:
            await interaction.followup.send(
                f"Minimum bet is {JOPACOIN_MIN_BET} {JOPACOIN_EMOTE}.", ephemeral=True
            )
            return

        pending_state = self.match_service.get_last_shuffle(guild_id)
        if not pending_state:
            await interaction.followup.send("❌ No active match to bet on.", ephemeral=True)
            return

        # Check if this is draft mode (dual pool betting)
        is_draft = pending_state.get("is_draft", False)
        if is_draft:
            # Dual pool system: route to player pool or spectator pool
            radiant_ids = pending_state.get("radiant_ids", [])
            dire_ids = pending_state.get("dire_ids", [])
            is_participant = user_id in radiant_ids or user_id in dire_ids

            if is_participant:
                # Player pool: can only bet on their own team
                user_team = "radiant" if user_id in radiant_ids else "dire"
                if team.value != user_team:
                    await interaction.followup.send(
                        f"❌ As a participant, you can only bet on your own team ({user_team.title()}).",
                        ephemeral=True,
                    )
                    return

                # Leverage not supported for player pool bets
                if leverage and leverage.value > 1:
                    await interaction.followup.send(
                        "❌ Leverage is not supported for player pool bets.",
                        ephemeral=True,
                    )
                    return

                stake_service = getattr(self.bot, "stake_service", None)
                if not stake_service:
                    await interaction.followup.send(
                        "❌ Player pool betting is not available.", ephemeral=True
                    )
                    return

                result = stake_service.place_player_bet(
                    guild_id, user_id, team.value, amount, pending_state
                )
                if not result.get("success"):
                    await interaction.followup.send(
                        f"❌ {result.get('error', 'Unknown error')}", ephemeral=True
                    )
                    return

                await interaction.followup.send(
                    f"Player pool bet placed: {amount} {JOPACOIN_EMOTE} on {team.name}\n"
                    f"Current multiplier: {result.get('new_multiplier', 0):.2f}x\n"
                    f"New balance: {result.get('new_balance', 0)} {JOPACOIN_EMOTE}",
                    ephemeral=True,
                )
            else:
                # Spectator pool: parimutuel with player cut
                # Leverage not supported for spectator pool bets
                if leverage and leverage.value > 1:
                    await interaction.followup.send(
                        "❌ Leverage is not supported for spectator pool bets.",
                        ephemeral=True,
                    )
                    return

                spectator_pool_service = getattr(self.bot, "spectator_pool_service", None)
                if not spectator_pool_service:
                    await interaction.followup.send(
                        "❌ Spectator pool betting is not available.", ephemeral=True
                    )
                    return

                result = spectator_pool_service.place_bet(
                    guild_id, user_id, team.value, amount, pending_state
                )
                if not result.get("success"):
                    await interaction.followup.send(
                        f"❌ {result.get('error', 'Unknown error')}", ephemeral=True
                    )
                    return

                pool_totals = result.get("pool_totals", {})
                await interaction.followup.send(
                    f"Spectator pool bet placed: {amount} {JOPACOIN_EMOTE} on {team.name}\n"
                    f"Pool totals: Radiant {pool_totals.get('radiant', 0)} JC | Dire {pool_totals.get('dire', 0)} JC\n"
                    f"Note: 90% to winners, 10% to winning players",
                    ephemeral=True,
                )
        else:
            # Legacy betting mode (non-draft)
            lev = leverage.value if leverage else 1
            effective_bet = amount * lev

            try:
                self.betting_service.place_bet(
                    guild_id, user_id, team.value, amount, pending_state, leverage=lev
                )
            except ValueError as exc:
                await interaction.followup.send(f"❌ {exc}", ephemeral=True)
                return

            await self._update_shuffle_message_wagers(guild_id)

            # Build response message
            betting_mode = pending_state.get("betting_mode", "pool") if pending_state else "pool"
            pool_warning = ""
            if betting_mode == "pool":
                pool_warning = "\n⚠️ Pool mode: odds may shift as more bets come in. Use `/mybets` to check current EV."

            if lev > 1:
                await interaction.followup.send(
                    f"Bet placed: {amount} {JOPACOIN_EMOTE} on {team.name} at {lev}x leverage "
                    f"(effective: {effective_bet} {JOPACOIN_EMOTE}).{pool_warning}",
                    ephemeral=True,
                )
            else:
                await interaction.followup.send(
                    f"Bet placed: {amount} {JOPACOIN_EMOTE} on {team.name}.{pool_warning}",
                    ephemeral=True,
                )

    @app_commands.command(name="mybets", description="Show your active bets")
    async def mybets(self, interaction: discord.Interaction):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="mybets",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=10,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/mybets` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        pending_state = self.match_service.get_last_shuffle(guild_id)
        bets = self.betting_service.get_pending_bets(
            guild_id, interaction.user.id, pending_state=pending_state
        )
        if not bets:
            await interaction.followup.send("You have no active bets.", ephemeral=True)
            return

        # Calculate totals across all bets
        total_amount = sum(b["amount"] for b in bets)
        total_effective = sum(b["amount"] * (b.get("leverage", 1) or 1) for b in bets)
        team_name = bets[0]["team_bet_on"].title()  # All bets are on the same team

        # Build message with each bet enumerated
        bet_lines = []
        for i, bet in enumerate(bets, 1):
            leverage = bet.get("leverage", 1) or 1
            effective = bet["amount"] * leverage
            time_str = f"<t:{int(bet['bet_time'])}:t>"
            is_blind = bet.get("is_blind", 0)
            auto_tag = " (auto)" if is_blind else ""
            if leverage > 1:
                bet_lines.append(
                    f"{i}. {bet['amount']} {JOPACOIN_EMOTE} at {leverage}x "
                    f"(effective: {effective} {JOPACOIN_EMOTE}){auto_tag} — {time_str}"
                )
            else:
                bet_lines.append(f"{i}. {bet['amount']} {JOPACOIN_EMOTE}{auto_tag} — {time_str}")

        # Header with totals
        if len(bets) == 1:
            header = f"**Active bet on {team_name}:**"
        else:
            header = f"**Active bets on {team_name}** ({len(bets)} bets):"

        # Show total if multiple bets
        if len(bets) > 1:
            if total_amount != total_effective:
                bet_lines.append(
                    f"\n**Total:** {total_amount} {JOPACOIN_EMOTE} "
                    f"(effective: {total_effective} {JOPACOIN_EMOTE})"
                )
            else:
                bet_lines.append(f"\n**Total:** {total_amount} {JOPACOIN_EMOTE}")

        base_msg = header + "\n" + "\n".join(bet_lines)

        # Add EV info for pool mode
        betting_mode = pending_state.get("betting_mode", "pool") if pending_state else "pool"
        if betting_mode == "pool":
            totals = self.betting_service.get_pot_odds(guild_id, pending_state=pending_state)
            total_pool = totals["radiant"] + totals["dire"]
            my_team_total = totals[bets[0]["team_bet_on"]]

            if my_team_total > 0 and total_pool > 0:
                my_share = total_effective / my_team_total
                potential_payout = int(total_pool * my_share)
                other_team = "dire" if bets[0]["team_bet_on"] == "radiant" else "radiant"
                odds_ratio = totals[other_team] / my_team_total if my_team_total > 0 else 0

                base_msg += (
                    f"\n\n📊 **Current Pool Odds** (may change):"
                    f"\nTotal pool: {total_pool} {JOPACOIN_EMOTE}"
                    f"\nYour team ({team_name}): {my_team_total} {JOPACOIN_EMOTE}"
                    f"\nIf you win: ~{potential_payout} {JOPACOIN_EMOTE} ({odds_ratio:.2f}:1 odds)"
                )
        elif betting_mode == "house":
            # House mode: 1:1 payout
            potential_payout = total_effective * 2
            base_msg += f"\n\nIf you win: {potential_payout} {JOPACOIN_EMOTE} (1:1 odds)"

        await interaction.followup.send(base_msg, ephemeral=True)

    @app_commands.command(name="bets", description="Show all bets in the current pool")
    async def bets(self, interaction: discord.Interaction):
        """View all bets in the current pool."""
        if not has_admin_permission(interaction):
            guild = interaction.guild if interaction.guild else None
            rl_gid = guild.id if guild else 0
            rl = GLOBAL_RATE_LIMITER.check(
                scope="bets",
                guild_id=rl_gid,
                user_id=interaction.user.id,
                limit=1,
                per_seconds=60,
            )
            if not rl.allowed:
                await interaction.response.send_message(
                    f"⏳ Please wait {rl.retry_after_seconds}s before using `/bets` again.",
                    ephemeral=True,
                )
                return

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        pending_state = self.match_service.get_last_shuffle(guild_id)
        if not pending_state:
            await interaction.followup.send("No active match to show bets for.", ephemeral=True)
            return

        all_bets = self.betting_service.get_all_pending_bets(guild_id, pending_state=pending_state)
        if not all_bets:
            await interaction.followup.send("No bets placed yet.", ephemeral=True)
            return

        # Get current odds
        totals = self.betting_service.get_pot_odds(guild_id, pending_state=pending_state)
        total_pool = totals["radiant"] + totals["dire"]
        radiant_mult = total_pool / totals["radiant"] if totals["radiant"] > 0 else None
        dire_mult = total_pool / totals["dire"] if totals["dire"] > 0 else None

        # Build embed
        embed = discord.Embed(
            title=f"📊 Pool Bets ({len(all_bets)} bets)",
            color=discord.Color.gold(),
        )

        # Current odds header
        lock_until = pending_state.get("bet_lock_until")
        radiant_odds_str = f"{radiant_mult:.2f}x" if radiant_mult else "—"
        dire_odds_str = f"{dire_mult:.2f}x" if dire_mult else "—"
        odds_text = (
            f"🟢 Radiant: {totals['radiant']} {JOPACOIN_EMOTE} ({radiant_odds_str}) | "
            f"🔴 Dire: {totals['dire']} {JOPACOIN_EMOTE} ({dire_odds_str})"
        )
        if lock_until:
            odds_text += f"\nBetting closes <t:{lock_until}:R>"
        embed.add_field(name="Current Odds", value=odds_text, inline=False)

        # Group bets by team
        radiant_bets = [b for b in all_bets if b["team_bet_on"] == "radiant"]
        dire_bets = [b for b in all_bets if b["team_bet_on"] == "dire"]

        # Check if betting is still open and if user is admin
        is_admin = has_admin_permission(interaction)
        betting_open = lock_until and int(time.time()) < lock_until
        show_names = is_admin or not betting_open

        # Format bet line helper
        def format_bet_line(bet: dict, index: int) -> str:
            leverage = bet.get("leverage", 1) or 1
            is_blind = bet.get("is_blind", 0)
            odds_at_placement = bet.get("odds_at_placement")

            # Base amount - hide names for non-admins while betting is open
            if show_names:
                line = f"<@{bet['discord_id']}> • {bet['amount']}"
            else:
                line = f"Bettor #{index} • {bet['amount']}"

            # Auto tag
            if is_blind:
                line += " (auto)"

            # Leverage notation
            if leverage > 1:
                effective = bet["amount"] * leverage
                line += f" at {leverage}x → {effective} eff"

            # Odds at placement
            if odds_at_placement:
                line += f" • {odds_at_placement:.2f}x"

            return line

        # Radiant bets section
        if radiant_bets:
            radiant_lines = [format_bet_line(b, i + 1) for i, b in enumerate(radiant_bets)]
            # Truncate if too long
            radiant_text = "\n".join(radiant_lines[:15])
            if len(radiant_bets) > 15:
                radiant_text += f"\n... +{len(radiant_bets) - 15} more"
            embed.add_field(
                name=f"🟢 Radiant Bets ({len(radiant_bets)})",
                value=radiant_text or "None",
                inline=False,
            )

        # Dire bets section
        if dire_bets:
            dire_lines = [format_bet_line(b, i + 1) for i, b in enumerate(dire_bets)]
            dire_text = "\n".join(dire_lines[:15])
            if len(dire_bets) > 15:
                dire_text += f"\n... +{len(dire_bets) - 15} more"
            embed.add_field(
                name=f"🔴 Dire Bets ({len(dire_bets)})",
                value=dire_text or "None",
                inline=False,
            )

        # Pool summary
        radiant_pct = (totals["radiant"] / total_pool * 100) if total_pool > 0 else 0
        dire_pct = (totals["dire"] / total_pool * 100) if total_pool > 0 else 0
        summary_text = (
            f"**Total:** {total_pool} {JOPACOIN_EMOTE} effective\n"
            f"Radiant: {totals['radiant']} ({radiant_pct:.0f}%) | Dire: {totals['dire']} ({dire_pct:.0f}%)"
        )
        embed.add_field(name="Pool Summary", value=summary_text, inline=False)

        await interaction.followup.send(embed=embed, ephemeral=True)

    @app_commands.command(name="balance", description="Check your jopacoin balance")
    async def balance(self, interaction: discord.Interaction):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="balance",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=10,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"Please wait {rl.retry_after_seconds}s before using `/balance` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        user_id = interaction.user.id
        balance = self.player_service.get_balance(user_id)

        # Check for bankruptcy penalty
        penalty_info = ""
        if self.bankruptcy_service:
            state = self.bankruptcy_service.get_state(user_id)
            if state.penalty_games_remaining > 0:
                penalty_rate_pct = int(BANKRUPTCY_PENALTY_RATE * 100)
                penalty_info = (
                    f"\n**Bankruptcy penalty:** {penalty_rate_pct}% win bonus "
                    f"for {state.penalty_games_remaining} more game(s)"
                )

        # Check for loan info
        loan_info = ""
        if self.loan_service:
            loan_state = self.loan_service.get_state(user_id)
            # Show outstanding loan prominently
            if loan_state.has_outstanding_loan:
                loan_info = (
                    f"\n⚠️ **Outstanding loan:** {loan_state.outstanding_total} {JOPACOIN_EMOTE} "
                    f"(repaid after next match)"
                )
            if loan_state.total_loans_taken > 0:
                loan_info += f"\n**Loans taken:** {loan_state.total_loans_taken} (fees paid: {loan_state.total_fees_paid})"
            if loan_state.is_on_cooldown and loan_state.cooldown_ends_at:
                import time
                remaining = loan_state.cooldown_ends_at - int(time.time())
                hours = remaining // 3600
                minutes = (remaining % 3600) // 60
                loan_info += f"\n**Loan cooldown:** {hours}h {minutes}m remaining"

        if balance >= 0:
            await interaction.followup.send(
                f"{interaction.user.mention} has {balance} {JOPACOIN_EMOTE}.{penalty_info}{loan_info}",
                ephemeral=True,
            )
        else:
            # Show debt information
            garnishment_pct = int(GARNISHMENT_PERCENTAGE * 100)

            await interaction.followup.send(
                f"{interaction.user.mention} has **{balance}** {JOPACOIN_EMOTE} (in debt)\n"
                f"Garnishment: {garnishment_pct}% of winnings go to debt repayment{penalty_info}{loan_info}\n\n"
                f"Use `/bankruptcy` to clear your debt (with penalties).\n"
                f"Use `/loan` to borrow more jopacoin (with a fee).",
                ephemeral=True,
            )

    @app_commands.command(name="paydebt", description="Help another player pay off their debt")
    @app_commands.describe(
        player="Player whose debt to pay",
        amount="Amount of jopacoin to pay toward their debt",
    )
    async def paydebt(
        self,
        interaction: discord.Interaction,
        player: discord.Member,
        amount: int,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="paydebt",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=10,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"Please wait {rl.retry_after_seconds}s before using `/paydebt` again.",
                ephemeral=True,
            )
            return

        # Always public since helping another player
        if not await safe_defer(interaction, ephemeral=False):
            return

        try:
            result = self.player_service.player_repo.pay_debt_atomic(
                from_discord_id=interaction.user.id,
                to_discord_id=player.id,
                amount=amount,
            )

            await interaction.followup.send(
                f"{interaction.user.mention} paid {result['amount_paid']} {JOPACOIN_EMOTE} "
                f"toward {player.mention}'s debt!",
                ephemeral=False,
            )
        except ValueError as exc:
            await interaction.followup.send(f"{exc}", ephemeral=True)

    @app_commands.command(
        name="bankruptcy",
        description="Declare bankruptcy to clear your debt (once per week, with penalties)",
    )
    async def bankruptcy(self, interaction: discord.Interaction):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="bankruptcy",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=2,
            per_seconds=30,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"The bankruptcy court requires you to wait {rl.retry_after_seconds}s "
                "before filing again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        if not self.bankruptcy_service:
            await interaction.followup.send("Bankruptcy service is not available.", ephemeral=True)
            return

        user_id = interaction.user.id

        # Check if player is registered
        player = self.player_service.get_player(user_id)
        if not player:
            await interaction.followup.send(
                "You need to `/register` before you can declare bankruptcy. "
                "Though maybe that's a good sign you shouldn't gamble.",
                ephemeral=True,
            )
            return

        # Check if bankruptcy is allowed
        check = self.bankruptcy_service.can_declare_bankruptcy(user_id)

        if not check["allowed"]:
            if check["reason"] == "not_in_debt":
                message = random.choice(BANKRUPTCY_DENIED_MESSAGES)
                balance = check.get("balance", 0)
                await interaction.followup.send(
                    f"{interaction.user.mention} tried to declare bankruptcy...\n\n"
                    f"{message}\n\nTheir balance: {balance} {JOPACOIN_EMOTE}",
                    ephemeral=False,
                )
                return
            elif check["reason"] == "on_cooldown":
                message = random.choice(BANKRUPTCY_COOLDOWN_MESSAGES)
                cooldown_ends = check.get("cooldown_ends_at")
                cooldown_str = f"<t:{cooldown_ends}:R>" if cooldown_ends else "soon"
                await interaction.followup.send(
                    f"{interaction.user.mention} tried to declare bankruptcy again...\n\n"
                    f"{message}\n\nThey can file again {cooldown_str}.",
                    ephemeral=False,
                )
                return

        # Declare bankruptcy
        result = self.bankruptcy_service.declare_bankruptcy(user_id)

        if not result["success"]:
            await interaction.followup.send(
                "Something went wrong with your bankruptcy filing. The universe is cruel.",
                ephemeral=True,
            )
            return

        # Format success message
        message = random.choice(BANKRUPTCY_SUCCESS_MESSAGES).format(
            debt=result["debt_cleared"],
            games=result["penalty_games"],
            rate=int(result["penalty_rate"] * 100),
        )

        # Try to get AI-generated flavor text
        ai_flavor = None
        if self.flavor_text_service:
            guild_id = interaction.guild.id if interaction.guild else None
            try:
                ai_flavor = await self.flavor_text_service.generate_event_flavor(
                    guild_id=guild_id,
                    event=FlavorEvent.BANKRUPTCY_DECLARED,
                    discord_id=user_id,
                    event_details={
                        "debt_cleared": result["debt_cleared"],
                        "penalty_games": result["penalty_games"],
                        "penalty_rate": result["penalty_rate"],
                    },
                )
            except Exception as e:
                logger.warning(f"Failed to generate AI flavor for bankruptcy: {e}")

        penalty_rate_pct = int(result["penalty_rate"] * 100)
        flavor_line = f"\n\n*{ai_flavor}*" if ai_flavor else ""
        await interaction.followup.send(
            f"**{interaction.user.mention} HAS DECLARED BANKRUPTCY**\n\n"
            f"{message}{flavor_line}\n\n"
            f"**Details:**\n"
            f"Debt cleared: {result['debt_cleared']} {JOPACOIN_EMOTE}\n"
            f"Penalty: {penalty_rate_pct}% win bonus for the next {result['penalty_games']} games\n"
            f"New balance: 0 {JOPACOIN_EMOTE}",
            ephemeral=False,
        )

    @app_commands.command(name="gambastats", description="View your gambling statistics and degen score")
    @app_commands.describe(user="Player to view stats for (defaults to yourself)")
    async def gambastats(
        self,
        interaction: discord.Interaction,
        user: discord.Member | None = None,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="gambastats",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=3,
            per_seconds=15,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/gambastats` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        if not self.gambling_stats_service:
            await interaction.followup.send("Gambling stats service is not available.", ephemeral=True)
            return

        target_user = user or interaction.user
        stats = self.gambling_stats_service.get_player_stats(target_user.id)

        if not stats:
            await interaction.followup.send(
                f"{target_user.display_name} has no betting history yet.",
                ephemeral=True,
            )
            return

        # Build embed
        degen = stats.degen_score
        pnl_color = 0x57F287 if stats.net_pnl >= 0 else 0xED4245  # Green or red

        embed = discord.Embed(
            title=f"Gamba Stats for {target_user.display_name}",
            color=pnl_color,
        )

        # Degen score header
        flavor_text = " • ".join(degen.flavor_texts) if degen.flavor_texts else degen.tagline
        embed.description = (
            f"**Degen Score: {degen.total}** {degen.emoji} {degen.title}\n"
            f"*{flavor_text}*"
        )

        # Core stats
        pnl_str = f"+{stats.net_pnl}" if stats.net_pnl >= 0 else str(stats.net_pnl)
        roi_str = f"+{stats.roi:.1%}" if stats.roi >= 0 else f"{stats.roi:.1%}"

        # Get current balance
        player = self.player_service.get_player(target_user.id)
        balance = player.jopacoin_balance if player else 0

        embed.add_field(
            name="Performance",
            value=(
                f"**Balance:** {balance} {JOPACOIN_EMOTE}\n"
                f"**Net P&L:** {pnl_str} {JOPACOIN_EMOTE}\n"
                f"**ROI:** {roi_str}\n"
                f"**Record:** {stats.wins}W-{stats.losses}L ({stats.win_rate:.0%})"
            ),
            inline=True,
        )

        embed.add_field(
            name="Volume",
            value=(
                f"**Total Bets:** {stats.total_bets}\n"
                f"**Wagered:** {stats.total_wagered} {JOPACOIN_EMOTE}\n"
                f"**Avg Bet:** {stats.avg_bet_size:.1f} {JOPACOIN_EMOTE}"
            ),
            inline=True,
        )

        # Leverage distribution
        lev_parts = []
        for lev in [1, 2, 3, 5]:
            count = stats.leverage_distribution.get(lev, 0)
            if count > 0:
                pct = count / stats.total_bets * 100
                lev_parts.append(f"{lev}×({pct:.0f}%)")
        lev_str = " ".join(lev_parts) if lev_parts else "None"

        # Streaks
        streak_emoji = "🔥" if stats.current_streak > 0 else "💀" if stats.current_streak < 0 else "➖"
        streak_val = abs(stats.current_streak)
        streak_type = "W" if stats.current_streak >= 0 else "L"

        embed.add_field(
            name="Risk Profile",
            value=(
                f"**Leverage:** {lev_str}\n"
                f"**Streak:** {streak_emoji} {streak_type}{streak_val}\n"
                f"**Best/Worst:** W{stats.best_streak} / L{abs(stats.worst_streak)}"
            ),
            inline=True,
        )

        # Extremes
        peak_str = f"+{stats.peak_pnl}" if stats.peak_pnl > 0 else str(stats.peak_pnl)
        trough_str = str(stats.trough_pnl)
        biggest_win_str = f"+{stats.biggest_win}" if stats.biggest_win > 0 else "None"
        biggest_loss_str = str(stats.biggest_loss) if stats.biggest_loss < 0 else "None"

        embed.add_field(
            name="Extremes",
            value=(
                f"**Peak:** {peak_str} {JOPACOIN_EMOTE}\n"
                f"**Trough:** {trough_str} {JOPACOIN_EMOTE}\n"
                f"**Best Win:** {biggest_win_str} {JOPACOIN_EMOTE}\n"
                f"**Worst Loss:** {biggest_loss_str} {JOPACOIN_EMOTE}"
            ),
            inline=True,
        )

        # Paper hands - now highlighted since removed from degen score
        if stats.matches_played > 0:
            paper_rate = stats.paper_hands_count / stats.matches_played * 100
            paper_emoji = "📄" if paper_rate >= 30 else "🤔" if paper_rate >= 10 else "💎"
            embed.add_field(
                name=f"{paper_emoji} Paper Hands",
                value=(
                    f"Played: {stats.matches_played} matches\n"
                    f"No self-bet: {stats.paper_hands_count} ({paper_rate:.0f}%)"
                ),
                inline=True,
            )

        # Loan stats if available
        if self.loan_service:
            loan_state = self.loan_service.get_state(target_user.id)
            if loan_state.total_loans_taken > 0 or loan_state.has_outstanding_loan:
                loan_lines = [
                    f"**Taken:** {loan_state.total_loans_taken}",
                    f"**Fees Paid:** {loan_state.total_fees_paid} {JOPACOIN_EMOTE}",
                ]
                if loan_state.has_outstanding_loan:
                    loan_lines.append(f"⚠️ **Owed:** {loan_state.outstanding_total} {JOPACOIN_EMOTE}")
                embed.add_field(
                    name="🏦 Loans",
                    value="\n".join(loan_lines),
                    inline=True,
                )

        # Bankruptcy penalty if active
        if self.bankruptcy_service:
            bankruptcy_state = self.bankruptcy_service.get_state(target_user.id)
            if bankruptcy_state.penalty_games_remaining > 0:
                penalty_rate_pct = int(BANKRUPTCY_PENALTY_RATE * 100)
                embed.add_field(
                    name=f"{TOMBSTONE_EMOJI} Bankruptcy Penalty",
                    value=f"{penalty_rate_pct}% win bonus for {bankruptcy_state.penalty_games_remaining} more game(s)",
                    inline=True,
                )

        await interaction.followup.send(embed=embed)

    @app_commands.command(name="gambachart", description="View your gambling history as a chart")
    @app_commands.describe(user="Player to view chart for (defaults to yourself)")
    async def gambachart(
        self,
        interaction: discord.Interaction,
        user: discord.Member | None = None,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="gambachart",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=2,
            per_seconds=20,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/gambachart` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        if not self.gambling_stats_service:
            await interaction.followup.send("Gambling stats service is not available.", ephemeral=True)
            return

        target_user = user or interaction.user
        stats = self.gambling_stats_service.get_player_stats(target_user.id)

        if not stats:
            await interaction.followup.send(
                f"{target_user.display_name} has no betting history yet.",
                ephemeral=True,
            )
            return

        # Get P&L series for chart
        pnl_series = self.gambling_stats_service.get_cumulative_pnl_series(target_user.id)

        # Generate chart
        degen = stats.degen_score
        chart_image = draw_gamba_chart(
            username=target_user.display_name,
            degen_score=degen.total,
            degen_title=degen.title,
            degen_emoji=degen.emoji,
            pnl_series=pnl_series,
            stats={
                "total_bets": stats.total_bets,
                "win_rate": stats.win_rate,
                "net_pnl": stats.net_pnl,
                "roi": stats.roi,
            },
        )

        file = discord.File(chart_image, filename="gamba_chart.png")
        await interaction.followup.send(file=file)

    @app_commands.command(name="gambaleaderboard", description="View server gambling leaderboard")
    @app_commands.describe(limit="Number of entries per section (default 5)")
    async def gambaleaderboard(
        self,
        interaction: discord.Interaction,
        limit: int = 5,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="gambaleaderboard",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=2,
            per_seconds=30,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/gambaleaderboard` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=False):
            return

        if not self.gambling_stats_service:
            await interaction.followup.send("Gambling stats service is not available.", ephemeral=True)
            return

        guild_id = interaction.guild.id if interaction.guild else None
        limit = max(1, min(limit, 10))  # Clamp between 1 and 10

        leaderboard = self.gambling_stats_service.get_leaderboard(guild_id, limit=limit)

        if not leaderboard.top_earners and not leaderboard.hall_of_degen:
            await interaction.followup.send(
                "No gambling data yet! Players need at least 3 settled bets to appear.",
                ephemeral=True,
            )
            return

        embed = discord.Embed(
            title="🏆 GAMBA LEADERBOARD",
            color=0xFFD700,  # Gold
        )

        # Helper to get username with tombstone if bankrupt
        async def get_name(discord_id: int) -> str:
            try:
                member = interaction.guild.get_member(discord_id) if interaction.guild else None
                if member:
                    name = member.display_name
                else:
                    user = await self.bot.fetch_user(discord_id)
                    name = user.display_name if user else f"User {discord_id}"
            except Exception:
                name = f"User {discord_id}"

            # Add tombstone if player has active bankruptcy penalty
            if self.bankruptcy_service:
                state = self.bankruptcy_service.get_state(discord_id)
                if state.penalty_games_remaining > 0:
                    return f"{TOMBSTONE_EMOJI} {name}"
            return name

        # Top earners
        if leaderboard.top_earners:
            lines = []
            for i, entry in enumerate(leaderboard.top_earners, 1):
                name = await get_name(entry.discord_id)
                pnl_str = f"+{entry.net_pnl}" if entry.net_pnl >= 0 else str(entry.net_pnl)
                lines.append(f"{i}. **{name}** {pnl_str} {JOPACOIN_EMOTE} ({entry.win_rate:.0%} WR)")
            embed.add_field(
                name="💰 Top Earners",
                value="\n".join(lines),
                inline=False,
            )

        # Down bad
        if leaderboard.down_bad:
            lines = []
            for i, entry in enumerate(leaderboard.down_bad, 1):
                name = await get_name(entry.discord_id)
                lines.append(f"{i}. **{name}** {entry.net_pnl} {JOPACOIN_EMOTE} ({entry.win_rate:.0%} WR)")
            embed.add_field(
                name="📉 Down Bad",
                value="\n".join(lines),
                inline=False,
            )

        # Hall of degen
        if leaderboard.hall_of_degen:
            lines = []
            degen_emojis = ["🥱", "🎰", "🔥", "💀", "🎪", "👑"]
            for i, entry in enumerate(leaderboard.hall_of_degen, 1):
                name = await get_name(entry.discord_id)
                # Get emoji based on score
                score = entry.degen_score or 0
                emoji_idx = min(score // 20, 5)
                emoji = degen_emojis[emoji_idx]
                title = entry.degen_title or "Unknown"
                lines.append(f"{i}. **{name}** {score} {emoji} {title}")
            embed.add_field(
                name="👑 Hall of Degen",
                value="\n".join(lines),
                inline=False,
            )

        # Server totals
        avg_degen_str = f"{leaderboard.avg_degen_score:.0f}" if leaderboard.avg_degen_score else "N/A"
        embed.add_field(
            name="📊 Server Totals",
            value=(
                f"Total Wagered: {leaderboard.total_wagered} {JOPACOIN_EMOTE} | "
                f"Bets: {leaderboard.total_bets} | "
                f"Avg Degen: {avg_degen_str}\n"
                f"Bankruptcies: {leaderboard.total_bankruptcies} | "
                f"Loans: {leaderboard.total_loans}"
            ),
            inline=False,
        )

        await interaction.followup.send(embed=embed)

    @app_commands.command(name="loan", description="Borrow jopacoin (with a fee)")
    @app_commands.describe(amount="Amount to borrow (max 100)")
    async def loan(
        self,
        interaction: discord.Interaction,
        amount: int,
    ):
        """Take out a loan. You receive the full amount but owe amount + fee."""
        if not self.loan_service:
            await interaction.response.send_message(
                "Loan service is not available.", ephemeral=True
            )
            return

        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if registered
        if not self.player_service.get_player(user_id):
            await interaction.response.send_message(
                "You need to `/register` before taking loans.", ephemeral=True
            )
            return

        # Check eligibility
        check = self.loan_service.can_take_loan(user_id, amount)

        if not check["allowed"]:
            if check["reason"] == "has_outstanding_loan":
                await interaction.response.send_message(
                    f"You already have an outstanding loan of **{check['outstanding_total']}** {JOPACOIN_EMOTE} "
                    f"(principal: {check['outstanding_principal']}, fee: {check['outstanding_fee']}).\n\n"
                    "Repay it by playing in a match first!",
                )
                return
            elif check["reason"] == "on_cooldown":
                remaining = check["cooldown_ends_at"] - int(__import__("time").time())
                hours = remaining // 3600
                minutes = (remaining % 3600) // 60
                # Try AI flavor, fallback to static message
                msg = None
                if self.flavor_text_service:
                    try:
                        msg = await self.flavor_text_service.generate_event_flavor(
                            guild_id=guild_id,
                            event=FlavorEvent.LOAN_COOLDOWN,
                            discord_id=user_id,
                            event_details={
                                "cooldown_remaining_hours": hours,
                                "cooldown_remaining_minutes": minutes,
                                "requested_amount": amount,
                            },
                        )
                    except Exception as e:
                        logger.warning(f"Failed to generate AI flavor for loan cooldown: {e}")
                if not msg:
                    msg = random.choice(LOAN_DENIED_COOLDOWN_MESSAGES)
                await interaction.response.send_message(
                    f"{msg}\n\n⏳ Cooldown ends in **{hours}h {minutes}m**.",
                )
                return
            elif check["reason"] == "exceeds_debt_limit":
                # Try AI flavor, fallback to static message
                msg = None
                if self.flavor_text_service:
                    try:
                        msg = await self.flavor_text_service.generate_event_flavor(
                            guild_id=guild_id,
                            event=FlavorEvent.LOAN_DENIED_DEBT,
                            discord_id=user_id,
                            event_details={
                                "current_balance": check["current_balance"],
                                "requested_amount": amount,
                                "max_debt": MAX_DEBT,
                            },
                        )
                    except Exception as e:
                        logger.warning(f"Failed to generate AI flavor for loan denied: {e}")
                if not msg:
                    msg = random.choice(LOAN_DENIED_DEBT_MESSAGES)
                await interaction.response.send_message(
                    f"{msg}\n\nCurrent balance: **{check['current_balance']}** {JOPACOIN_EMOTE}",
                )
                return
            elif check["reason"] == "exceeds_max":
                await interaction.response.send_message(
                    f"Maximum loan amount is **{check['max_amount']}** {JOPACOIN_EMOTE}.",
                )
                return
            elif check["reason"] == "invalid_amount":
                await interaction.response.send_message(
                    "Loan amount must be positive.",
                )
                return

        # Take the loan
        result = self.loan_service.take_loan(user_id, amount, guild_id)

        if not result["success"]:
            await interaction.response.send_message(
                "Failed to process loan. Please try again.", ephemeral=True
            )
            return

        fee_pct = int(LOAN_FEE_RATE * 100)

        # Try to get AI-generated flavor text
        ai_flavor = None
        if self.flavor_text_service:
            event_type = (
                FlavorEvent.NEGATIVE_LOAN
                if result.get("was_negative_loan")
                else FlavorEvent.LOAN_TAKEN
            )
            try:
                ai_flavor = await self.flavor_text_service.generate_event_flavor(
                    guild_id=guild_id,
                    event=event_type,
                    discord_id=user_id,
                    event_details={
                        "amount": result["amount"],
                        "fee": result["fee"],
                        "total_owed": result["total_owed"],
                        "new_balance": result["new_balance"],
                        "total_loans_taken": result["total_loans_taken"],
                        "was_negative_loan": result.get("was_negative_loan", False),
                    },
                )
            except Exception as e:
                logger.warning(f"Failed to generate AI flavor for loan: {e}")

        # Check if this was a negative loan (peak degen behavior)
        if result.get("was_negative_loan"):
            # Use AI flavor as main message if available, otherwise fallback to static
            if ai_flavor:
                msg = ai_flavor
            else:
                msg = random.choice(NEGATIVE_LOAN_MESSAGES).format(
                    amount=result["amount"],
                    emote=JOPACOIN_EMOTE,
                )
            embed = discord.Embed(
                title="🎪 LEGENDARY DEGEN MOVE 🎪",
                description=msg,
                color=0x9B59B6,  # Purple for peak degen
            )
            embed.add_field(
                name="The Damage",
                value=(
                    f"Borrowed: **{result['amount']}** {JOPACOIN_EMOTE}\n"
                    f"Fee ({fee_pct}%): **{result['fee']}** {JOPACOIN_EMOTE}\n"
                    f"Total Owed: **{result['total_owed']}** {JOPACOIN_EMOTE}\n"
                    f"New Balance: **{result['new_balance']}** {JOPACOIN_EMOTE}"
                ),
                inline=False,
            )
            embed.add_field(
                name="⚠️ Repayment",
                value="You will repay the full amount **after your next match**.",
                inline=False,
            )
            embed.set_footer(
                text="Loan #{} | Go bet it all, you beautiful degen".format(
                    result["total_loans_taken"]
                )
            )
        else:
            # Use AI flavor as main message if available, otherwise fallback to static
            if ai_flavor:
                msg = ai_flavor
            else:
                msg = random.choice(LOAN_SUCCESS_MESSAGES).format(
                    amount=result["amount"],
                    owed=result["total_owed"],
                    fee=result["fee"],
                    emote=JOPACOIN_EMOTE,
                )
            embed = discord.Embed(
                title="🏦 Loan Approved",
                description=msg,
                color=0x2ECC71,  # Green
            )
            embed.add_field(
                name="Details",
                value=(
                    f"Borrowed: **{result['amount']}** {JOPACOIN_EMOTE}\n"
                    f"Fee ({fee_pct}%): **{result['fee']}** {JOPACOIN_EMOTE}\n"
                    f"Total Owed: **{result['total_owed']}** {JOPACOIN_EMOTE}\n"
                    f"New Balance: **{result['new_balance']}** {JOPACOIN_EMOTE}"
                ),
                inline=False,
            )
            embed.add_field(
                name="📅 Repayment",
                value="You will repay the full amount **after your next match**.",
                inline=False,
            )
            embed.set_footer(
                text=f"Loan #{result['total_loans_taken']} | Fee donated to Gambling Addiction Nonprofit"
            )

        await interaction.response.send_message(embed=embed)

    @app_commands.command(name="nonprofit", description="View the Gambling Addiction Nonprofit fund")
    async def nonprofit(self, interaction: discord.Interaction):
        """View how much has been collected for the nonprofit."""
        if not self.loan_service:
            await interaction.response.send_message(
                "Loan service is not available.", ephemeral=True
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        total = self.loan_service.get_nonprofit_fund(guild_id)

        embed = discord.Embed(
            title="💝 Jopacoin Nonprofit for Gambling Addiction",
            description=(
                "All loan fees are donated to help those with negative balance.\n\n"
                "*\"We're here to help... by taking a cut of every loan.\"*"
            ),
            color=0xE91E63,  # Pink
        )
        embed.add_field(
            name="Available Funds",
            value=f"**{total}** {JOPACOIN_EMOTE}",
            inline=False,
        )

        # Show status based on fund level
        if total >= DISBURSE_MIN_FUND:
            status_value = f"Ready for disbursement! (min: {DISBURSE_MIN_FUND})"
        else:
            status_value = f"Collecting... ({total}/{DISBURSE_MIN_FUND} needed)"

        embed.add_field(
            name="Status",
            value=status_value,
            inline=True,
        )

        # Show last disbursement info if available
        if self.disburse_service:
            last_disburse = self.disburse_service.get_last_disbursement(guild_id)
            if last_disburse:
                import datetime

                dt = datetime.datetime.fromtimestamp(
                    last_disburse["disbursed_at"], tz=datetime.timezone.utc
                )
                time_str = f"<t:{last_disburse['disbursed_at']}:R>"

                # Format recipients
                recipients = last_disburse["recipients"]
                if recipients:
                    # Show up to 3 recipients
                    recipient_strs = []
                    for discord_id, amount in recipients[:3]:
                        recipient_strs.append(f"<@{discord_id}>: +{amount}")
                    if len(recipients) > 3:
                        recipient_strs.append(f"+{len(recipients) - 3} more")
                    recipients_text = "\n".join(recipient_strs)
                else:
                    recipients_text = "No recipients"

                method_labels = {
                    "even": "Even Split",
                    "proportional": "Proportional",
                    "neediest": "Neediest First",
                }
                method_label = method_labels.get(
                    last_disburse["method"], last_disburse["method"]
                )

                embed.add_field(
                    name="Last Disbursement",
                    value=(
                        f"**{last_disburse['total_amount']}** {JOPACOIN_EMOTE} "
                        f"via {method_label}\n{time_str}\n{recipients_text}"
                    ),
                    inline=False,
                )

        embed.set_footer(text="Use /disburse propose to start a distribution vote!")

        await interaction.response.send_message(embed=embed)

    @app_commands.command(
        name="disburse", description="Propose or manage nonprofit fund distribution"
    )
    @app_commands.describe(action="Action to perform")
    @app_commands.choices(
        action=[
            app_commands.Choice(name="propose", value="propose"),
            app_commands.Choice(name="status", value="status"),
            app_commands.Choice(name="reset", value="reset"),
            app_commands.Choice(name="votes", value="votes"),
        ]
    )
    async def disburse(
        self,
        interaction: discord.Interaction,
        action: app_commands.Choice[str] | None = None,
    ):
        """Propose, view, or reset nonprofit fund distribution voting."""
        if not self.disburse_service:
            await interaction.response.send_message(
                "Disbursement service is not available.", ephemeral=True
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        action_value = action.value if action else "status"

        if action_value == "propose":
            await self._disburse_propose(interaction, guild_id)
        elif action_value == "status":
            await self._disburse_status(interaction, guild_id)
        elif action_value == "reset":
            await self._disburse_reset(interaction, guild_id)
        elif action_value == "votes":
            await self._disburse_votes(interaction, guild_id)

    async def _disburse_propose(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Create a new disbursement proposal."""
        can, reason = self.disburse_service.can_propose(guild_id)
        if not can:
            if reason == "active_proposal_exists":
                await interaction.response.send_message(
                    "A disbursement vote is already active. Use `/disburse status` to see it.",
                    ephemeral=True,
                )
            elif reason.startswith("insufficient_fund:"):
                parts = reason.split(":")
                current = int(parts[1])
                needed = int(parts[2])
                await interaction.response.send_message(
                    f"Insufficient funds. Current: **{current}** {JOPACOIN_EMOTE}, "
                    f"minimum required: **{needed}** {JOPACOIN_EMOTE}",
                    ephemeral=True,
                )
            elif reason == "no_debtors":
                await interaction.response.send_message(
                    "No players with negative balance to receive funds.", ephemeral=True
                )
            else:
                await interaction.response.send_message(
                    f"Cannot create proposal: {reason}", ephemeral=True
                )
            return

        try:
            proposal = self.disburse_service.create_proposal(guild_id)
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return

        # Create embed and view
        embed = self._create_disburse_embed(proposal)
        view = DisburseVoteView(self.disburse_service, self)

        await interaction.response.send_message(embed=embed, view=view)

        # Store message ID for updates
        msg = await interaction.original_response()
        self.disburse_service.set_proposal_message(
            guild_id, msg.id, interaction.channel_id
        )

    async def _disburse_status(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Show current proposal status, replacing the old message to keep it visible."""
        proposal = self.disburse_service.get_proposal(guild_id)
        if not proposal:
            await interaction.response.send_message(
                "No active disbursement proposal. Use `/disburse propose` to create one.",
                ephemeral=True,
            )
            return

        # Delete the old message if it exists (to avoid it getting lost in chat)
        if proposal.message_id and proposal.channel_id:
            try:
                old_channel = self.bot.get_channel(proposal.channel_id)
                if old_channel:
                    old_message = await old_channel.fetch_message(proposal.message_id)
                    if old_message:
                        await old_message.delete()
            except discord.errors.NotFound:
                pass  # Message already deleted
            except Exception as e:
                logger.warning(f"Failed to delete old disburse message: {e}")

        # Send new message with embed and voting buttons
        embed = self._create_disburse_embed(proposal)
        view = DisburseVoteView(self.disburse_service, self)
        await interaction.response.send_message(embed=embed, view=view)

        # Update stored message reference to point to the new message
        msg = await interaction.original_response()
        self.disburse_service.set_proposal_message(
            guild_id, msg.id, interaction.channel_id
        )

    async def _disburse_reset(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Reset (cancel) the active proposal. Admin only."""
        # Check admin
        if interaction.user.id not in self.bot.ADMIN_USER_IDS:
            await interaction.response.send_message(
                "Only admins can reset disbursement proposals.", ephemeral=True
            )
            return

        success = self.disburse_service.reset_proposal(guild_id)
        if success:
            await interaction.response.send_message(
                "Disbursement proposal has been reset.", ephemeral=False
            )
        else:
            await interaction.response.send_message(
                "No active proposal to reset.", ephemeral=True
            )

    async def _disburse_votes(
        self, interaction: discord.Interaction, guild_id: int | None
    ):
        """Show detailed voting information with voter identities. Admin only."""
        # Check admin
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "Only admins can view detailed voting information.", ephemeral=True
            )
            return

        proposal = self.disburse_service.get_proposal(guild_id)
        if not proposal:
            await interaction.response.send_message(
                "No active disbursement proposal. Use `/disburse status` to check.",
                ephemeral=True,
            )
            return

        # Create admin-only embed with voter details
        embed = self._create_disburse_votes_embed(proposal)
        await interaction.response.send_message(embed=embed, ephemeral=True)

    def _create_disburse_embed(self, proposal) -> discord.Embed:
        """Create embed for disbursement proposal."""
        votes = proposal.votes
        total_votes = proposal.total_votes
        quorum = proposal.quorum_required
        progress = proposal.quorum_progress

        embed = discord.Embed(
            title="💝 Nonprofit Fund Disbursement Vote",
            description=(
                f"Vote on how to distribute **{proposal.fund_amount}** {JOPACOIN_EMOTE}.\n\n"
                "Click a button below to vote!"
            ),
            color=0xE91E63,  # Pink
        )

        # Voting options with counts
        embed.add_field(
            name="📊 Even Split",
            value=f"Split equally to debtors\n**{votes['even']}** votes",
            inline=True,
        )
        embed.add_field(
            name="📈 Proportional",
            value=f"More debt = more funds\n**{votes['proportional']}** votes",
            inline=True,
        )
        embed.add_field(
            name="🎯 Neediest First",
            value=f"All to most indebted\n**{votes['neediest']}** votes",
            inline=True,
        )
        embed.add_field(
            name="💸 Stimulus",
            value=f"Even split to non-top-3\n**{votes['stimulus']}** votes",
            inline=True,
        )

        # Progress bar
        bar_length = 20
        filled = int(progress * bar_length)
        bar = "█" * filled + "░" * (bar_length - filled)
        embed.add_field(
            name="Quorum Progress",
            value=f"`{bar}` {total_votes}/{quorum} ({int(progress * 100)}%)",
            inline=False,
        )

        if proposal.quorum_reached:
            embed.add_field(
                name="✅ Quorum Reached!",
                value="The next vote will trigger automatic disbursement.",
                inline=False,
            )

        embed.set_footer(text="Ties are broken in favor of Even Split")

        return embed

    def _create_disburse_votes_embed(self, proposal) -> discord.Embed:
        """Create admin-only embed showing detailed voter information."""
        votes = proposal.votes
        total_votes = proposal.total_votes
        quorum = proposal.quorum_required
        progress = proposal.quorum_progress

        embed = discord.Embed(
            title="🔍 Disbursement Vote Details (Admin Only)",
            description=f"Fund Amount: **{proposal.fund_amount}** {JOPACOIN_EMOTE}",
            color=0x9C27B0,  # Purple (admin color)
        )

        # Proposal info
        embed.add_field(
            name="📋 Proposal Status",
            value=(
                f"**Quorum:** {total_votes}/{quorum} ({int(progress * 100)}%)\n"
                f"**Status:** {'✅ Ready' if proposal.quorum_reached else '⏳ Voting'}"
            ),
            inline=False,
        )

        # Vote breakdown
        vote_lines = []
        for method in ["even", "proportional", "neediest", "stimulus"]:
            count = votes[method]
            pct = (count / total_votes * 100) if total_votes > 0 else 0
            label = self.disburse_service.METHOD_LABELS[method]
            vote_lines.append(f"**{label}:** {count} ({pct:.0f}%)")

        embed.add_field(
            name="📊 Vote Breakdown",
            value="\n".join(vote_lines),
            inline=False,
        )

        # Individual votes
        guild_id = proposal.guild_id if proposal.guild_id != 0 else None
        individual_votes = self.disburse_service.disburse_repo.get_individual_votes(guild_id)

        if individual_votes:
            voter_lines = []
            for vote in individual_votes:
                discord_id = vote["discord_id"]
                method = vote["vote_method"]
                method_label = self.disburse_service.METHOD_LABELS.get(method, method)
                voter_lines.append(f"• <@{discord_id}> → {method_label}")

            voters_text = "\n".join(voter_lines)
        else:
            voters_text = "*No votes yet*"

        # Truncate if too long (Discord field limit is 1024 chars)
        if len(voters_text) > 1024:
            voters_text = voters_text[:1021] + "..."

        embed.add_field(
            name="👥 Individual Votes",
            value=voters_text,
            inline=False,
        )

        embed.set_footer(text="This information is only visible to you")

        return embed

    async def update_disburse_message(self, guild_id: int | None):
        """Update the disbursement proposal message with current vote counts."""
        proposal = self.disburse_service.get_proposal(guild_id)
        if not proposal or not proposal.message_id or not proposal.channel_id:
            return

        try:
            channel = self.bot.get_channel(proposal.channel_id)
            if not channel:
                return

            message = await channel.fetch_message(proposal.message_id)
            if not message:
                return

            embed = self._create_disburse_embed(proposal)
            await message.edit(embed=embed)
        except discord.errors.NotFound:
            pass
        except Exception as e:
            logger.warning(f"Failed to update disburse message: {e}")


class DisburseVoteView(discord.ui.View):
    """Persistent view for disbursement voting."""

    def __init__(self, disburse_service: DisburseService, cog: "BettingCommands"):
        super().__init__(timeout=None)  # Persistent - no timeout
        self.disburse_service = disburse_service
        self.cog = cog

    async def _handle_vote(
        self, interaction: discord.Interaction, method: str, label: str
    ):
        """Handle a vote button press."""
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if user is registered
        player = self.cog.player_service.get_player(interaction.user.id)
        if not player:
            await interaction.response.send_message(
                "You must be registered to vote. Use `/register` first.",
                ephemeral=True,
            )
            return

        # Check for active proposal
        proposal = self.disburse_service.get_proposal(guild_id)
        if not proposal:
            await interaction.response.send_message(
                "This vote has ended or been reset.", ephemeral=True
            )
            return

        try:
            result = self.disburse_service.add_vote(
                guild_id, interaction.user.id, method
            )
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return

        # Check if quorum reached and execute
        if result["quorum_reached"]:
            # Execute disbursement
            try:
                disbursement = self.disburse_service.execute_disbursement(guild_id)

                # Build result message
                if disbursement["total_disbursed"] == 0:
                    result_msg = disbursement.get(
                        "message", "No funds were distributed."
                    )
                else:
                    recipients = disbursement["distributions"]
                    recipient_lines = []
                    for discord_id, amount in recipients[:10]:
                        recipient_lines.append(f"<@{discord_id}>: +{amount}")
                    if len(recipients) > 10:
                        recipient_lines.append(f"...and {len(recipients) - 10} more")

                    result_msg = (
                        f"**{disbursement['total_disbursed']}** {JOPACOIN_EMOTE} "
                        f"distributed via **{disbursement['method_label']}** to "
                        f"{disbursement['recipient_count']} player(s):\n"
                        + "\n".join(recipient_lines)
                    )

                # Send result as new message
                embed = discord.Embed(
                    title="💝 Disbursement Complete!",
                    description=result_msg,
                    color=0x00FF00,  # Green
                )
                await interaction.response.send_message(embed=embed)

                # Disable buttons on the original message
                try:
                    if proposal.message_id and proposal.channel_id:
                        channel = self.cog.bot.get_channel(proposal.channel_id)
                        if channel:
                            msg = await channel.fetch_message(proposal.message_id)
                            # Create disabled view
                            disabled_view = discord.ui.View(timeout=None)
                            for item in self.children:
                                if isinstance(item, discord.ui.Button):
                                    new_btn = discord.ui.Button(
                                        label=item.label,
                                        emoji=item.emoji,
                                        style=discord.ButtonStyle.secondary,
                                        disabled=True,
                                        custom_id=item.custom_id,
                                    )
                                    disabled_view.add_item(new_btn)
                            await msg.edit(view=disabled_view)
                except Exception as e:
                    logger.warning(f"Failed to disable vote buttons: {e}")

            except ValueError as e:
                await interaction.response.send_message(
                    f"Disbursement failed: {e}", ephemeral=True
                )
        else:
            # Just acknowledge the vote
            await interaction.response.send_message(
                f"Your vote for **{label}** has been recorded! "
                f"({result['total_votes']}/{result['quorum_required']} for quorum)",
                ephemeral=True,
            )

            # Update the embed
            await self.cog.update_disburse_message(guild_id)

    @discord.ui.button(
        label="Even Split",
        emoji="📊",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:even",
    )
    async def vote_even(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "even", "Even Split")

    @discord.ui.button(
        label="Proportional",
        emoji="📈",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:proportional",
    )
    async def vote_proportional(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "proportional", "Proportional")

    @discord.ui.button(
        label="Neediest First",
        emoji="🎯",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:neediest",
    )
    async def vote_neediest(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "neediest", "Neediest First")

    @discord.ui.button(
        label="Stimulus",
        emoji="💸",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:stimulus",
    )
    async def vote_stimulus(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "stimulus", "Stimulus")


async def setup(bot: commands.Bot):
    betting_service = getattr(bot, "betting_service", None)
    if betting_service is None:
        raise RuntimeError("Betting service not registered on bot.")
    match_service = getattr(bot, "match_service", None)
    if match_service is None:
        raise RuntimeError("Match service not registered on bot.")
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("Player service not registered on bot.")
    bankruptcy_service = getattr(bot, "bankruptcy_service", None)
    gambling_stats_service = getattr(bot, "gambling_stats_service", None)
    loan_service = getattr(bot, "loan_service", None)
    disburse_service = getattr(bot, "disburse_service", None)
    flavor_text_service = getattr(bot, "flavor_text_service", None)
    # bankruptcy_service, gambling_stats_service, loan_service, disburse_service, flavor_text_service are optional

    cog = BettingCommands(
        bot,
        betting_service,
        match_service,
        player_service,
        bankruptcy_service,
        gambling_stats_service,
        loan_service,
        disburse_service,
        flavor_text_service,
    )
    await bot.add_cog(cog)

    # Register persistent view for disbursement voting
    if disburse_service:
        bot.add_view(DisburseVoteView(disburse_service, cog))
