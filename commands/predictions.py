"""
Discord commands for prediction markets.
"""

import logging
import re
import time

import discord
from discord import app_commands
from discord.ext import commands

from services.permissions import has_admin_permission
from services.prediction_service import PredictionService
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.predictions")

JOPACOIN_EMOTE = "<:jopacoin:1375713705702318192>"


def parse_duration(duration_str: str) -> int:
    """
    Parse a duration string like "1h", "3d", "30m", "1mo" into seconds from now.

    Returns Unix timestamp when the duration ends.
    """
    duration_str = duration_str.strip().lower()

    # Match patterns like "1h", "3d", "30m", "2w", "1mo"
    match = re.match(r"^(\d+)\s*(mo|[mhdw])$", duration_str)
    if not match:
        raise ValueError(
            "Invalid duration format. Use: 30m, 1h, 3d, 1w, 1mo (minutes, hours, days, weeks, months)"
        )

    value = int(match.group(1))
    unit = match.group(2)

    multipliers = {
        "m": 60,  # minutes
        "h": 3600,  # hours
        "d": 86400,  # days
        "w": 604800,  # weeks
        "mo": 2592000,  # months (30 days)
    }

    seconds = value * multipliers[unit]

    # Minimum 1 minute, maximum 6 months
    if seconds < 60:
        raise ValueError("Minimum duration is 1 minute.")
    if seconds > 180 * 86400:
        raise ValueError("Maximum duration is 6 months.")

    return int(time.time()) + seconds


class PredictionBetModal(discord.ui.Modal):
    """Modal for entering bet amount on a prediction."""

    amount = discord.ui.TextInput(
        label="Bet Amount (numbers only)",
        placeholder="e.g., 10",
        min_length=1,
        max_length=10,
        required=True,
    )

    def __init__(
        self,
        prediction_service: PredictionService,
        prediction_id: int,
        position: str,
        question: str,
        current_odds: float,
        balance: int,
        cog: "PredictionCommands",
    ):
        super().__init__(title=f"Bet {position.upper()} ({balance} available)")
        self.prediction_service = prediction_service
        self.prediction_id = prediction_id
        self.position = position
        self.question = question
        self.current_odds = current_odds
        self.balance = balance
        self.cog = cog

        # Update placeholder with hint
        self.amount.placeholder = f"Enter 1-{balance}"

    async def on_submit(self, interaction: discord.Interaction):
        try:
            amount = int(self.amount.value)
        except ValueError:
            await interaction.response.send_message(
                "Invalid amount. Please enter a number.", ephemeral=True
            )
            return

        try:
            result = self.prediction_service.place_bet(
                prediction_id=self.prediction_id,
                discord_id=interaction.user.id,
                position=self.position,
                amount=amount,
            )

            # Get prediction for thread posting
            pred = self.prediction_service.get_prediction(self.prediction_id)
            new_odds = result["odds"][self.position]

            # Acknowledge the modal (no visible response)
            await interaction.response.defer()

            # Post public activity message in thread
            if pred and pred.get("thread_id"):
                await self.cog.post_bet_activity(
                    thread_id=pred["thread_id"],
                    user=interaction.user,
                    amount=amount,
                    position=self.position,
                    new_odds=new_odds,
                    is_addition=result.get("bet_count", 1) > 1,
                )

            # Update the embed
            if pred and pred.get("embed_message_id") and pred.get("thread_id"):
                await self.cog.update_prediction_embed(
                    thread_id=pred["thread_id"],
                    message_id=pred["embed_message_id"],
                    prediction_id=self.prediction_id,
                )

        except ValueError as e:
            await interaction.response.send_message(f"❌ {e}", ephemeral=True)
        except Exception as e:
            logger.exception(f"Error placing prediction bet: {e}")
            await interaction.response.send_message(
                "An error occurred while placing your bet.", ephemeral=True
            )


class PersistentPredictionView(discord.ui.View):
    """
    Persistent view that handles all prediction button interactions.

    Registered once on bot startup and handles buttons by parsing custom_id.
    """

    def __init__(self, cog: "PredictionCommands"):
        super().__init__(timeout=None)
        self.cog = cog

    @discord.ui.button(
        label="Bet YES",
        emoji="✅",
        style=discord.ButtonStyle.success,
        custom_id="prediction:bet:yes",
    )
    async def bet_yes(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_bet(interaction, "yes")

    @discord.ui.button(
        label="Bet NO",
        emoji="❌",
        style=discord.ButtonStyle.danger,
        custom_id="prediction:bet:no",
    )
    async def bet_no(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_bet(interaction, "no")

    @discord.ui.button(
        label="My Position",
        emoji="👤",
        style=discord.ButtonStyle.secondary,
        custom_id="prediction:position",
    )
    async def my_position(self, interaction: discord.Interaction, button: discord.ui.Button):
        await self._handle_position(interaction)

    def _get_prediction_id_from_message(self, message: discord.Message) -> int | None:
        """Extract prediction ID from the embed in the message."""
        if not message.embeds:
            return None
        embed = message.embeds[0]
        if embed.title and "PREDICTION #" in embed.title:
            # Extract ID from title like "🔮 PREDICTION #42"
            import re
            match = re.search(r"#(\d+)", embed.title)
            if match:
                return int(match.group(1))
        return None

    async def _handle_bet(self, interaction: discord.Interaction, position: str):
        """Handle bet button press."""
        prediction_id = self._get_prediction_id_from_message(interaction.message)
        if not prediction_id:
            await interaction.response.send_message(
                "Could not find prediction ID.", ephemeral=True
            )
            return

        pred = self.cog.prediction_service.get_prediction(prediction_id)
        if not pred:
            await interaction.response.send_message(
                "Prediction not found.", ephemeral=True
            )
            return

        if pred["status"] != "open":
            await interaction.response.send_message(
                "Betting is closed for this prediction.", ephemeral=True
            )
            return

        # Check if close time has passed
        now = int(time.time())
        if now >= pred["closes_at"]:
            await interaction.response.send_message(
                "Betting period has ended.", ephemeral=True
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        player = self.cog.player_service.get_player(interaction.user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You must be registered to bet. Use `/register` first.",
                ephemeral=True,
            )
            return

        balance = player.jopacoin_balance or 0
        if balance <= 0:
            await interaction.response.send_message(
                "You have no jopacoins to bet. Win some matches first!",
                ephemeral=True,
            )
            return

        odds_info = self.cog.prediction_service.get_odds(prediction_id)
        current_odds = odds_info["odds"].get(position, 0)

        modal = PredictionBetModal(
            prediction_service=self.cog.prediction_service,
            prediction_id=prediction_id,
            position=position,
            question=pred["question"],
            current_odds=current_odds,
            balance=balance,
            cog=self.cog,
        )
        await interaction.response.send_modal(modal)

    async def _handle_position(self, interaction: discord.Interaction):
        """Handle position button press."""
        prediction_id = self._get_prediction_id_from_message(interaction.message)
        if not prediction_id:
            await interaction.response.send_message(
                "Could not find prediction ID.", ephemeral=True
            )
            return

        position = self.cog.prediction_service.get_user_position(
            prediction_id, interaction.user.id
        )

        if not position:
            await interaction.response.send_message(
                "You haven't bet on this prediction yet.", ephemeral=True
            )
            return

        pred = self.cog.prediction_service.get_prediction(prediction_id)
        odds = pred.get("odds", {}) if pred else {}
        current_odds = odds.get(position["position"], 0)
        potential_win = int(position["total_amount"] * current_odds)

        await interaction.response.send_message(
            f"**Your Position:**\n"
            f"Side: **{position['position'].upper()}**\n"
            f"Amount: **{position['total_amount']}** {JOPACOIN_EMOTE}\n"
            f"Current odds: **{current_odds:.2f}x**\n"
            f"Potential win: **{potential_win}** {JOPACOIN_EMOTE}",
            ephemeral=True,
        )


class PredictionCommands(commands.Cog):
    """Commands for prediction markets."""

    def __init__(
        self,
        bot: commands.Bot,
        prediction_service: PredictionService,
        player_service,
    ):
        self.bot = bot
        self.prediction_service = prediction_service
        self.player_service = player_service

    def _create_prediction_embed(self, pred: dict) -> discord.Embed:
        """Create the embed for a prediction."""
        status_emoji = {
            "open": "🔮",
            "locked": "🔒",
            "resolved": "✅" if pred.get("outcome") == "yes" else "❌",
            "cancelled": "🚫",
        }

        emoji = status_emoji.get(pred["status"], "🔮")
        title = f"{emoji} PREDICTION #{pred['prediction_id']}"

        # Color based on status
        colors = {
            "open": 0x3498DB,  # Blue
            "locked": 0xF39C12,  # Orange
            "resolved": 0x2ECC71 if pred.get("outcome") == "yes" else 0xE74C3C,
            "cancelled": 0x95A5A6,  # Gray
        }
        color = colors.get(pred["status"], 0x3498DB)

        embed = discord.Embed(
            title=title,
            description=f'"{pred["question"]}"',
            color=color,
        )

        # Creator and timing
        embed.add_field(
            name="Created by",
            value=f"<@{pred['creator_id']}>",
            inline=True,
        )

        if pred["status"] == "open":
            embed.add_field(
                name="Closes",
                value=f"<t:{pred['closes_at']}:R>",
                inline=True,
            )
        elif pred["status"] == "resolved":
            outcome = pred.get("outcome", "").upper()
            embed.add_field(
                name="Outcome",
                value=f"**{outcome}**",
                inline=True,
            )

        # Odds section
        yes_total = pred.get("yes_total", 0)
        no_total = pred.get("no_total", 0)
        total_pool = yes_total + no_total
        odds = pred.get("odds", {"yes": 0, "no": 0})

        odds_text = ""
        if total_pool > 0:
            yes_odds = odds.get("yes", 0)
            no_odds = odds.get("no", 0)
            yes_pct = round(100 * yes_total / total_pool)
            no_pct = 100 - yes_pct
            odds_text = (
                f"✅ **YES**: {yes_pct}% ({yes_odds:.2f}x) — "
                f"{yes_total} {JOPACOIN_EMOTE}, {pred.get('yes_count', 0)} bettors\n"
                f"❌ **NO**: {no_pct}% ({no_odds:.2f}x) — "
                f"{no_total} {JOPACOIN_EMOTE}, {pred.get('no_count', 0)} bettors"
            )
        else:
            odds_text = "No bets yet - be the first!"

        embed.add_field(
            name="📊 Current Odds",
            value=odds_text,
            inline=False,
        )

        embed.add_field(
            name="Total Pool",
            value=f"{total_pool} {JOPACOIN_EMOTE}",
            inline=True,
        )

        # Resolution votes if applicable
        if pred["status"] == "locked":
            votes = self.prediction_service.get_resolution_votes(pred["prediction_id"])
            yes_votes = votes.get("yes", 0)
            no_votes = votes.get("no", 0)
            needed = self.prediction_service.MIN_RESOLUTION_VOTES

            if yes_votes > 0 or no_votes > 0:
                embed.add_field(
                    name="🗳️ Resolution Votes",
                    value=(
                        f"YES: {yes_votes}/{needed} | NO: {no_votes}/{needed}\n"
                        f"Use `/predictionresolve {pred['prediction_id']} <YES|NO>` to vote"
                    ),
                    inline=False,
                )

        return embed

    def _create_prediction_view(self) -> PersistentPredictionView:
        """Create the persistent prediction view with betting buttons."""
        return PersistentPredictionView(self)


    async def post_bet_activity(
        self,
        thread_id: int,
        user: discord.User,
        amount: int,
        position: str,
        new_odds: float,
        is_addition: bool = False,
    ):
        """Post bet activity message to thread."""
        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            emoji = "✅" if position == "yes" else "❌"
            action = "added" if is_addition else "bet"

            await thread.send(
                f"{emoji} **{user.display_name}** {action} **{amount}** {JOPACOIN_EMOTE} "
                f"on {position.upper()} → odds now {new_odds:.2f}x"
            )
        except Exception as e:
            logger.warning(f"Failed to post bet activity: {e}")

    async def update_prediction_embed(
        self, thread_id: int, message_id: int, prediction_id: int
    ):
        """Update the prediction embed in the thread."""
        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            message = await thread.fetch_message(message_id)

            pred = self.prediction_service.get_prediction(prediction_id)
            if not pred:
                return

            embed = self._create_prediction_embed(pred)
            await message.edit(embed=embed)
        except Exception as e:
            logger.warning(f"Failed to update prediction embed: {e}")

    @app_commands.command(name="prediction", description="Create a new prediction market")
    @app_commands.describe(
        question="The question to predict on",
        closes_in="When betting closes (e.g., 1h, 3d, 1w, 1mo)",
    )
    async def prediction(
        self, interaction: discord.Interaction, question: str, closes_in: str
    ):
        """Create a new prediction market."""
        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None

        # Check if user is registered
        player = self.player_service.get_player(interaction.user.id, guild_id)
        if not player:
            await safe_followup(
                interaction,
                content="You must be registered to create predictions. Use `/register` first.",
            )
            return

        try:
            closes_at = parse_duration(closes_in)
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {e}")
            return

        try:
            result = self.prediction_service.create_prediction(
                guild_id=guild_id,
                creator_id=interaction.user.id,
                question=question,
                closes_at=closes_at,
                channel_id=interaction.channel_id,
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {e}")
            return

        prediction_id = result["prediction_id"]

        # Send acknowledgment via followup (ephemeral)
        await safe_followup(
            interaction,
            content=f"✅ Prediction #{prediction_id} created! Setting up thread...",
            ephemeral=True,
        )

        # Send channel message (regular message has guild info for thread creation)
        channel_msg = await interaction.channel.send(
            f"🔮 **New Prediction** by {interaction.user.mention}\n"
            f'"{question}"\n'
            f"Closes <t:{closes_at}:R> • Click thread below to bet",
        )

        # Create thread from message
        try:
            thread_name = f"🔮 Prediction: {question[:50]}..." if len(question) > 50 else f"🔮 Prediction: {question}"
            thread = await channel_msg.create_thread(name=thread_name)

            # Get prediction with odds
            pred = self.prediction_service.get_prediction(prediction_id)

            # Create and send embed in thread
            embed = self._create_prediction_embed(pred)
            view = self._create_prediction_view()

            embed_msg = await thread.send(embed=embed, view=view)

            # Try to pin the embed message
            try:
                await embed_msg.pin()
            except discord.Forbidden:
                pass

            # Update prediction with Discord IDs
            self.prediction_service.update_discord_ids(
                prediction_id=prediction_id,
                thread_id=thread.id,
                embed_message_id=embed_msg.id,
                channel_message_id=channel_msg.id,
            )

        except discord.Forbidden:
            await safe_followup(
                interaction,
                content="⚠️ Prediction created but couldn't create thread. "
                "Missing 'Create Public Threads' permission.",
            )
        except Exception as e:
            logger.exception(f"Error creating prediction thread: {e}")

    @app_commands.command(name="predictionresolve", description="Vote to resolve a prediction")
    @app_commands.describe(
        prediction_id="The prediction ID to resolve",
        outcome="The outcome (YES or NO)",
    )
    @app_commands.choices(
        outcome=[
            app_commands.Choice(name="YES", value="yes"),
            app_commands.Choice(name="NO", value="no"),
        ]
    )
    async def resolve(
        self,
        interaction: discord.Interaction,
        prediction_id: int,
        outcome: app_commands.Choice[str],
    ):
        """Vote to resolve a prediction."""
        await safe_defer(interaction)

        # Check if user has admin permissions (for immediate resolution)
        is_admin = has_admin_permission(interaction)

        try:
            vote_result = self.prediction_service.add_resolution_vote(
                prediction_id=prediction_id,
                user_id=interaction.user.id,
                outcome=outcome.value,
                is_admin=is_admin,
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {e}")
            return

        pred = self.prediction_service.get_prediction(prediction_id)

        if vote_result["can_resolve"]:
            # Resolve the prediction
            try:
                settlement = self.prediction_service.resolve(
                    prediction_id=prediction_id,
                    outcome=outcome.value,
                    resolved_by=interaction.user.id,
                )

                # Build resolution message
                winners = settlement.get("winners", [])
                losers = settlement.get("losers", [])

                outcome_emoji = "✅" if outcome.value == "yes" else "❌"
                result_lines = [f"🏆 **Prediction Resolved!**\n{outcome_emoji} **Answer: {outcome.value.upper()}**\n"]

                if winners:
                    result_lines.append("**Winners:**")
                    for w in winners[:10]:
                        result_lines.append(
                            f"• <@{w['discord_id']}>: +{w['profit']} "
                            f"(bet {w['amount']}, won {w['payout']})"
                        )
                    if len(winners) > 10:
                        result_lines.append(f"...and {len(winners) - 10} more")

                if losers:
                    result_lines.append("\n**Losers:**")
                    for l in losers[:10]:
                        result_lines.append(f"• <@{l['discord_id']}>: -{l['amount']}")
                    if len(losers) > 10:
                        result_lines.append(f"...and {len(losers) - 10} more")

                result_lines.append(
                    f"\nTotal pool: **{settlement['total_pool']}** {JOPACOIN_EMOTE}"
                )

                await safe_followup(interaction, content="\n".join(result_lines))

                # Post to thread, lock, and archive
                if pred and pred.get("thread_id"):
                    try:
                        thread = self.bot.get_channel(pred["thread_id"])
                        if thread:
                            await thread.send("\n".join(result_lines))
                            # Lock and archive thread
                            try:
                                await thread.edit(locked=True, archived=True)
                            except discord.Forbidden:
                                # Missing Manage Threads permission - just archive
                                await thread.edit(archived=True)
                            # Update the channel message to show it's resolved
                            if pred.get("channel_message_id") and thread.parent:
                                try:
                                    channel_msg = await thread.parent.fetch_message(
                                        pred["channel_message_id"]
                                    )
                                    outcome_emoji = "✅" if outcome.value == "yes" else "❌"
                                    await channel_msg.edit(
                                        content=f"{outcome_emoji} **Prediction Resolved: {outcome.value.upper()}**\n"
                                        f'"{pred.get("question", "")}"\n'
                                        f"See thread for results"
                                    )
                                except Exception as e:
                                    logger.warning(f"Failed to update channel message: {e}")
                            # Update the close message to show voting is complete
                            if pred.get("close_message_id"):
                                try:
                                    close_msg = await thread.fetch_message(
                                        pred["close_message_id"]
                                    )
                                    outcome_emoji = "✅" if outcome.value == "yes" else "❌"
                                    await close_msg.edit(
                                        content=f"{outcome_emoji} **Voting complete** — Resolved: **{outcome.value.upper()}**"
                                    )
                                except Exception as e:
                                    logger.warning(f"Failed to update close message: {e}")
                    except Exception as e:
                        logger.warning(f"Failed to post resolution to thread: {e}")

                # Update embed
                if pred and pred.get("embed_message_id") and pred.get("thread_id"):
                    await self.update_prediction_embed(
                        thread_id=pred["thread_id"],
                        message_id=pred["embed_message_id"],
                        prediction_id=prediction_id,
                    )

            except ValueError as e:
                await safe_followup(interaction, content=f"❌ Resolution failed: {e}")
        else:
            # Just a vote, not enough to resolve yet
            count = vote_result.get(f"{outcome.value}_count", 0)
            needed = vote_result["votes_needed"]

            await safe_followup(
                interaction,
                content=f"🗳️ Your vote for **{outcome.value.upper()}** has been recorded!\n"
                f"Votes for {outcome.value.upper()}: {count}/{needed}",
            )

            # Update embed with vote counts
            if pred and pred.get("embed_message_id") and pred.get("thread_id"):
                await self.update_prediction_embed(
                    thread_id=pred["thread_id"],
                    message_id=pred["embed_message_id"],
                    prediction_id=prediction_id,
                )

    @app_commands.command(name="predictioncancel", description="Cancel a prediction (admin only)")
    @app_commands.describe(prediction_id="The prediction ID to cancel")
    async def cancel(self, interaction: discord.Interaction, prediction_id: int):
        """Cancel a prediction and refund all bets (admin only)."""
        await safe_defer(interaction)

        # Check admin permissions (ADMIN_USER_IDS or Discord Administrator/Manage Server)
        if not has_admin_permission(interaction):
            await safe_followup(
                interaction, content="❌ Only admins can cancel predictions."
            )
            return

        try:
            result = self.prediction_service.cancel_by_admin(
                prediction_id=prediction_id,
                admin_id=interaction.user.id,
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {e}")
            return

        refunded = result.get("refunded", [])
        total = result.get("total_refunded", 0)

        await safe_followup(
            interaction,
            content=f"🚫 **Prediction #{prediction_id} cancelled**\n"
            f"Refunded **{total}** {JOPACOIN_EMOTE} to {len(refunded)} player(s).",
        )

        # Post to thread, lock, and archive
        pred = self.prediction_service.get_prediction(prediction_id)
        if pred and pred.get("thread_id"):
            try:
                thread = self.bot.get_channel(pred["thread_id"])
                if thread:
                    await thread.send(
                        f"🚫 **CANCELLED** by <@{interaction.user.id}>\n"
                        f"All bets have been refunded."
                    )
                    # Lock and archive thread
                    try:
                        await thread.edit(locked=True, archived=True)
                    except discord.Forbidden:
                        # Missing Manage Threads permission - just archive
                        await thread.edit(archived=True)
                    # Update the channel message to show it's cancelled
                    if pred.get("channel_message_id") and thread.parent:
                        try:
                            channel_msg = await thread.parent.fetch_message(
                                pred["channel_message_id"]
                            )
                            await channel_msg.edit(
                                content=f"🚫 **Prediction Cancelled**\n"
                                f'"{pred.get("question", "")}"\n'
                                f"All bets refunded"
                            )
                        except Exception as e:
                            logger.warning(f"Failed to update channel message: {e}")
            except Exception as e:
                logger.warning(f"Failed to post cancellation to thread: {e}")

    @app_commands.command(name="predictionclose", description="Close betting early (admin only)")
    @app_commands.describe(prediction_id="The prediction ID to close")
    async def closebet(self, interaction: discord.Interaction, prediction_id: int):
        """Close betting on a prediction early (admin only)."""
        await safe_defer(interaction)

        if not has_admin_permission(interaction):
            await safe_followup(
                interaction, content="❌ Only admins can close betting early."
            )
            return

        try:
            result = self.prediction_service.close_betting_early(prediction_id)
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {e}")
            return

        votes_needed = self.prediction_service.MIN_RESOLUTION_VOTES
        await safe_followup(
            interaction,
            content=f"🔒 **Betting closed** on prediction #{prediction_id}\n"
            f"Resolution voting is now open ({votes_needed} votes or 1 admin to resolve).\n"
            f"Use `/predictionresolve {prediction_id} <YES|NO>` to vote.",
        )

        # Post to thread and update thread name
        pred = self.prediction_service.get_prediction(prediction_id)
        if pred and pred.get("thread_id"):
            try:
                thread = self.bot.get_channel(pred["thread_id"])
                if thread:
                    close_msg = await thread.send(
                        f"🔒 **Betting closed early** by <@{interaction.user.id}>\n"
                        f"Vote to resolve ({votes_needed} votes or 1 admin needed):\n"
                        f"`/predictionresolve {prediction_id} YES` or `/predictionresolve {prediction_id} NO`"
                    )
                    # Save the close message ID so we can edit it when resolved
                    self.prediction_service.update_discord_ids(
                        prediction_id=prediction_id,
                        close_message_id=close_msg.id,
                    )
                    # Update thread name to show it's closed
                    question = pred.get("question", "")[:40]
                    await thread.edit(name=f"🔒 Closed: {question}")
                    # Update the embed
                    if pred.get("embed_message_id"):
                        await self.update_prediction_embed(
                            thread_id=pred["thread_id"],
                            message_id=pred["embed_message_id"],
                            prediction_id=prediction_id,
                        )
                    # Update the channel message to show it's closed
                    if pred.get("channel_message_id") and thread.parent:
                        try:
                            channel_msg = await thread.parent.fetch_message(
                                pred["channel_message_id"]
                            )
                            await channel_msg.edit(
                                content=f"🔒 **Prediction Closed** by {interaction.user.mention}\n"
                                f'"{pred.get("question", "")}"\n'
                                f"Betting has ended • Resolution voting in progress"
                            )
                        except Exception as e:
                            logger.warning(f"Failed to update channel message: {e}")
            except Exception as e:
                logger.warning(f"Failed to post close message to thread: {e}")

    @app_commands.command(name="predictions", description="List predictions")
    @app_commands.describe(
        show_all="Show all predictions including resolved/cancelled",
        limit="Number of predictions to show (default 10)",
    )
    async def predictions(
        self,
        interaction: discord.Interaction,
        show_all: bool = False,
        limit: int = 10,
    ):
        """List predictions."""
        await safe_defer(interaction)

        guild_id = interaction.guild.id if interaction.guild else None
        limit = max(1, min(limit, 25))  # Cap between 1 and 25

        # Always auto-lock expired predictions first
        self.prediction_service.check_and_lock_expired(guild_id)

        if show_all:
            # Get all predictions
            all_preds = []
            for s in ["open", "locked", "resolved", "cancelled"]:
                all_preds.extend(
                    self.prediction_service.prediction_repo.get_predictions_by_status(
                        guild_id, s
                    )
                )
            preds = sorted(all_preds, key=lambda p: p.get("created_at", 0), reverse=True)
        else:
            # Active only (open + locked)
            preds = self.prediction_service.get_active_predictions(guild_id)

        # Enrich with odds info
        for pred in preds:
            totals = self.prediction_service.prediction_repo.get_prediction_totals(
                pred["prediction_id"]
            )
            pred.update(totals)
            pred["odds"] = self.prediction_service.calculate_odds(
                totals["yes_total"], totals["no_total"]
            )
            pred["total_pool"] = totals["yes_total"] + totals["no_total"]

        if not preds:
            msg = "No predictions found." if show_all else "No active predictions."
            await safe_followup(interaction, content=msg)
            return

        title = "🔮 All Predictions" if show_all else "🔮 Active Predictions"
        embed = discord.Embed(
            title=title,
            color=0x3498DB,
        )

        for pred in preds[:limit]:
            pool = pred.get('total_pool', 0)
            yes_total = pred.get("yes_total", 0)
            odds = pred.get("odds", {})

            if pred["status"] == "resolved":
                # Resolved predictions - show outcome with winner/loser summary
                outcome = pred.get("outcome", "?")
                outcome_emoji = "✅" if outcome == "yes" else "❌"

                # Get resolution summary
                summary = self.prediction_service.prediction_repo.get_resolution_summary(
                    pred["prediction_id"]
                )
                winners = summary["winners"]
                losers = summary["losers"]

                # Build winner/loser display - show all users
                if winners:
                    winner_parts = [f"<@{w['discord_id']}> +{w['profit']}" for w in winners[:5]]
                    if len(winners) > 5:
                        winner_parts.append(f"+{len(winners) - 5} more")
                    winner_text = ", ".join(winner_parts)
                else:
                    winner_text = "none"

                if losers:
                    loser_parts = [f"<@{l['discord_id']}> -{l['bet']}" for l in losers[:5]]
                    if len(losers) > 5:
                        loser_parts.append(f"+{len(losers) - 5} more")
                    loser_text = ", ".join(loser_parts)
                else:
                    loser_text = "none"

                value = (
                    f"{outcome_emoji} **Answer: {outcome.upper()}** | Pool: {pool} {JOPACOIN_EMOTE}\n"
                    f"🏆 {winner_text}\n💀 {loser_text}"
                )

                embed.add_field(
                    name=f"✨ #{pred['prediction_id']}: {pred['question'][:50]}",
                    value=value,
                    inline=False,
                )
            elif pred["status"] == "cancelled":
                # Cancelled - show refund info
                value = f"🚫 **Cancelled** | Bets refunded"

                embed.add_field(
                    name=f"🚫 #{pred['prediction_id']}: {pred['question'][:50]}",
                    value=value,
                    inline=False,
                )
            elif pred["status"] == "locked":
                # Locked - awaiting resolution
                yes_pct = round(100 * yes_total / pool) if pool > 0 else 50
                no_pct = 100 - yes_pct
                value = (
                    f"⏳ **Awaiting resolution** | Pool: {pool} {JOPACOIN_EMOTE}\n"
                    f"YES {yes_pct}% / NO {no_pct}%"
                )

                embed.add_field(
                    name=f"🔒 #{pred['prediction_id']}: {pred['question'][:50]}",
                    value=value,
                    inline=False,
                )
            elif pred["status"] == "open":
                # Open - show odds and time
                yes_pct = round(100 * yes_total / pool) if pool > 0 else 50
                no_pct = 100 - yes_pct
                yes_odds = odds.get("yes", 0)
                no_odds = odds.get("no", 0)

                if pool > 0:
                    value = (
                        f"YES {yes_pct}% ({yes_odds:.2f}x) / NO {no_pct}% ({no_odds:.2f}x)\n"
                        f"Pool: {pool} {JOPACOIN_EMOTE} • Closes <t:{pred['closes_at']}:R>"
                    )
                else:
                    value = f"No bets yet • Closes <t:{pred['closes_at']}:R>"

                embed.add_field(
                    name=f"🟢 #{pred['prediction_id']}: {pred['question'][:50]}",
                    value=value,
                    inline=False,
                )

        if len(preds) > 10:
            embed.set_footer(text=f"Showing 10 of {len(preds)} predictions")

        await safe_followup(interaction, embed=embed)

    @app_commands.command(name="mypredictions", description="View your prediction positions")
    @app_commands.describe(history="Show resolved predictions instead of active ones")
    async def mypredictions(
        self,
        interaction: discord.Interaction,
        history: bool = False,
    ):
        """View user's prediction positions."""
        await safe_defer(interaction, ephemeral=True)

        if history:
            guild_id = interaction.guild_id
            positions = self.prediction_service.get_user_resolved_positions(
                interaction.user.id, guild_id
            )

            if not positions:
                await safe_followup(
                    interaction, content="You don't have any resolved predictions yet."
                )
                return

            embed = discord.Embed(
                title="🔮 Your Prediction History",
                color=0x9B59B6,
            )

            total_wagered = 0
            total_profit = 0

            for pos in positions[:15]:
                bet_emoji = "✅" if pos["position"] == "yes" else "❌"
                won = pos["position"] == pos["outcome"]
                result_emoji = "🏆" if won else "💀"

                amount = pos["total_amount"]
                payout = pos["payout"] or 0
                profit = payout - amount if won else -amount

                total_wagered += amount
                total_profit += profit

                profit_str = f"+{profit}" if profit > 0 else str(profit)

                embed.add_field(
                    name=f"{result_emoji} #{pos['prediction_id']}: {pos['question'][:40]}",
                    value=(
                        f"{bet_emoji} Bet **{pos['position'].upper()}** - "
                        f"{amount} {JOPACOIN_EMOTE} → {profit_str} {JOPACOIN_EMOTE}\n"
                        f"Outcome: **{pos['outcome'].upper()}** • "
                        f"<t:{pos['resolved_at']}:R>"
                    ),
                    inline=False,
                )

            # Summary footer
            profit_str = f"+{total_profit}" if total_profit > 0 else str(total_profit)
            embed.set_footer(text=f"Total wagered: {total_wagered} | Net P/L: {profit_str}")

            await safe_followup(interaction, embed=embed)
        else:
            guild_id = interaction.guild_id
            positions = self.prediction_service.get_user_active_positions(
                interaction.user.id, guild_id
            )

            if not positions:
                await safe_followup(
                    interaction,
                    content="You don't have any active prediction positions.",
                )
                return

            embed = discord.Embed(
                title="🔮 Your Active Positions",
                color=0x3498DB,
            )

            for pos in positions[:15]:
                emoji = "✅" if pos["position"] == "yes" else "❌"
                status_emoji = {"open": "🟢", "locked": "🔒"}.get(pos["status"], "❓")

                # Get current odds for this prediction
                odds_info = self.prediction_service.get_odds(pos["prediction_id"])
                current_odds = odds_info["odds"].get(pos["position"], 0)
                pool = odds_info["total_pool"]
                yes_total = odds_info["yes_total"]
                pct = round(100 * yes_total / pool) if pool > 0 else 50
                my_pct = pct if pos["position"] == "yes" else 100 - pct

                potential = int(pos["total_amount"] * current_odds) if current_odds > 0 else 0

                embed.add_field(
                    name=f"{status_emoji} #{pos['prediction_id']}: {pos['question'][:40]}",
                    value=(
                        f"{emoji} **{pos['position'].upper()}** @ {my_pct}% ({current_odds:.2f}x)\n"
                        f"Stake: {pos['total_amount']} {JOPACOIN_EMOTE} → "
                        f"Potential: {potential} {JOPACOIN_EMOTE}\n"
                        f"{'Closes' if pos['status'] == 'open' else 'Closed'} <t:{pos['closes_at']}:R>"
                    ),
                    inline=False,
                )

            await safe_followup(interaction, embed=embed)

async def setup(bot: commands.Bot):
    prediction_service = getattr(bot, "prediction_service", None)
    if prediction_service is None:
        raise RuntimeError("Prediction service not registered on bot.")
    player_service = getattr(bot, "player_service", None)
    if player_service is None:
        raise RuntimeError("Player service not registered on bot.")

    cog = PredictionCommands(bot, prediction_service, player_service)
    await bot.add_cog(cog)

    # Register persistent view for prediction buttons
    bot.add_view(PersistentPredictionView(cog))
