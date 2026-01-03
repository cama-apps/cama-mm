"""
Betting commands for jopacoin wagers.
"""

import logging
from typing import Optional

import discord
from discord.ext import commands
from discord import app_commands

from services.betting_service import BettingService
from services.match_service import MatchService
from services.player_service import PlayerService
from config import JOPACOIN_MIN_BET, LEVERAGE_TIERS, GARNISHMENT_PERCENTAGE, MAX_DEBT
from utils.formatting import JOPACOIN_EMOTE, format_betting_display
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
            betting_mode = pending_state.get("betting_mode", "house")

            field_name, field_value = format_betting_display(
                totals["radiant"], totals["dire"], betting_mode, lock_until
            )

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
            betting_mode = pending_state.get("betting_mode", "house")

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
            await interaction.followup.send(f"Minimum bet is {JOPACOIN_MIN_BET} {JOPACOIN_EMOTE}.", ephemeral=True)
            return

        lev = leverage.value if leverage else 1
        effective_bet = amount * lev

        pending_state = self.match_service.get_last_shuffle(guild_id)
        try:
            self.betting_service.place_bet(guild_id, user_id, team.value, amount, pending_state, leverage=lev)
        except ValueError as exc:
            await interaction.followup.send(f"❌ {exc}", ephemeral=True)
            return

        await self._update_shuffle_message_wagers(guild_id)

        # Build response message
        betting_mode = pending_state.get("betting_mode", "house") if pending_state else "house"
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
        bets = self.betting_service.get_pending_bets(guild_id, interaction.user.id, pending_state=pending_state)
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
            if leverage > 1:
                bet_lines.append(
                    f"{i}. {bet['amount']} {JOPACOIN_EMOTE} at {leverage}x "
                    f"(effective: {effective} {JOPACOIN_EMOTE}) — {time_str}"
                )
            else:
                bet_lines.append(f"{i}. {bet['amount']} {JOPACOIN_EMOTE} — {time_str}")

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
        betting_mode = pending_state.get("betting_mode", "house") if pending_state else "house"
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
                    f"\nIf you win: ~{potential_payout} {JOPACOIN_EMOTE} ({odds_ratio:.1f}:1 odds)"
                )
        elif betting_mode == "house":
            # House mode: 1:1 payout
            potential_payout = total_effective * 2
            base_msg += f"\n\nIf you win: {potential_payout} {JOPACOIN_EMOTE} (1:1 odds)"

        await interaction.followup.send(base_msg, ephemeral=True)

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

        if balance >= 0:
            await interaction.followup.send(
                f"{interaction.user.mention} has {balance} {JOPACOIN_EMOTE}.", ephemeral=True
            )
        else:
            # Show debt information
            garnishment_pct = int(GARNISHMENT_PERCENTAGE * 100)

            await interaction.followup.send(
                f"{interaction.user.mention} has **{balance}** {JOPACOIN_EMOTE} (in debt)\n"
                f"Garnishment: {garnishment_pct}% of winnings go to debt repayment",
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

