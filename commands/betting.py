"""
Betting commands for jopacoin wagers.
"""

import logging
from datetime import datetime
from typing import Optional

import discord
from discord.ext import commands
from discord import app_commands

from services.betting_service import BettingService
from services.match_service import MatchService
from services.player_service import PlayerService
from config import JOPACOIN_MIN_BET
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer
from utils.rate_limiter import GLOBAL_RATE_LIMITER


logger = logging.getLogger("cama_bot.commands.betting")


class BettingCommands(commands.Cog):
    """Slash commands to place and view wagers."""

    def __init__(
        self,
        bot: commands.Bot,
        betting_service: BettingService,
        match_service: MatchService,
        player_service: PlayerService,
    ):
        self.bot = bot
        self.betting_service = betting_service
        self.match_service = match_service
        self.player_service = player_service

    async def _update_shuffle_message_wagers(self, guild_id: Optional[int]) -> None:
        """
        Refresh the shuffle message's wager field with current totals.
        """
        pending_state = self.match_service.get_last_shuffle(guild_id)
        if not pending_state:
            return

        message_info = self.match_service.get_shuffle_message_info(guild_id)
        message_id = message_info.get("message_id") if message_info else None
        channel_id = message_info.get("channel_id") if message_info else None
        if not message_id or not channel_id:
            return

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
            totals = self.betting_service.get_pot_odds(guild_id, pending_state=pending_state)
            lock_until = pending_state.get("bet_lock_until")
            lock_text = f"Closes <t:{int(lock_until)}:R>" if lock_until else "No active match"
            totals_text = (
                f"Radiant: {totals['radiant']} {JOPACOIN_EMOTE} | "
                f"Dire: {totals['dire']} {JOPACOIN_EMOTE}\n{lock_text}"
            )

            embed_dict = embed.to_dict()
            fields = embed_dict.get("fields", [])
            updated = False
            for field in fields:
                if field.get("name") == "💰 Current Wagers":
                    field["value"] = totals_text
                    updated = True
                    break
            if not updated:
                fields.append({"name": "💰 Current Wagers", "value": totals_text, "inline": False})
            embed_dict["fields"] = fields

            new_embed = discord.Embed.from_dict(embed_dict)
            await message.edit(embed=new_embed, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to update shuffle wagers: {exc}", exc_info=True)

    async def _send_betting_reminder(
        self,
        guild_id: Optional[int],
        *,
        reminder_type: str,
        lock_until: Optional[int],
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
        if not message_id or not channel_id:
            return

        try:
            channel = self.bot.get_channel(channel_id)
            if channel is None:
                channel = await self.bot.fetch_channel(channel_id)
            if channel is None:
                return

            message = await channel.fetch_message(message_id)
            if not message:
                return

            totals = self.betting_service.get_pot_odds(guild_id, pending_state=pending_state)
            totals_text = (
                f"Radiant: {totals['radiant']} {JOPACOIN_EMOTE} | "
                f"Dire: {totals['dire']} {JOPACOIN_EMOTE}"
            )

            if reminder_type == "warning":
                if not lock_until:
                    return
                content = (
                    f"⏰ **5 minutes remaining until betting closes!** (<t:{int(lock_until)}:R>)\n\n"
                    f"Current bets:\n{totals_text}"
                )
            elif reminder_type == "closed":
                content = (
                    "🔒 **Betting is now closed!**\n\n"
                    f"Final bets:\n{totals_text}"
                )
            else:
                return

            await message.reply(content, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to send betting reminder: {exc}", exc_info=True)

    @app_commands.command(
        name="bet",
        description="Place a jopacoin bet on the current match (check balance with /balance)",
    )
    @app_commands.describe(
        team="Radiant or Dire",
        amount="Amount of jopacoin to wager (view balance with /balance)",
    )
    @app_commands.choices(
        team=[
            app_commands.Choice(name="Radiant", value="radiant"),
            app_commands.Choice(name="Dire", value="dire"),
        ]
    )
    async def bet(self, interaction: discord.Interaction, team: app_commands.Choice[str], amount: int):
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
            await interaction.followup.send(f"Minimum bet is {JOPACOIN_MIN_BET} {JOPACOIN_EMOTE}.", ephemeral=True)
            return

        pending_state = self.match_service.get_last_shuffle(guild_id)
        try:
            self.betting_service.place_bet(guild_id, user_id, team.value, amount, pending_state)
        except ValueError as exc:
            await interaction.followup.send(f"❌ {exc}", ephemeral=True)
            return

        await self._update_shuffle_message_wagers(guild_id)

        await interaction.followup.send(f"✅ Bet placed: {amount} {JOPACOIN_EMOTE} on {team.name}.", ephemeral=True)

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
        bet = self.betting_service.get_pending_bet(guild_id, interaction.user.id, pending_state=pending_state)
        if not bet:
            await interaction.followup.send("You have no active bets.", ephemeral=True)
            return

        await interaction.followup.send(
            f"Active bet: {bet['amount']} {JOPACOIN_EMOTE} on {bet['team_bet_on'].title()} (placed at {datetime.utcfromtimestamp(bet['bet_time']).strftime('%H:%M UTC')})",
            ephemeral=True,
        )

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
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/balance` again.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return
        
        balance = self.player_service.get_balance(interaction.user.id)
        await interaction.followup.send(
            f"💰 {interaction.user.mention} has {balance} jopacoin.", ephemeral=True
        )

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

    await bot.add_cog(
        BettingCommands(bot, betting_service, match_service, player_service)
    )

