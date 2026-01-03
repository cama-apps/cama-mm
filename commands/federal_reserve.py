"""
Federal Reserve commands: Totally legitimate financial instruments.

These commands are definitely not backdoors for a specific user who lost
a leveraged bet and is now majorly in debt. This is all above board.
"""

import logging
import random

import discord
from discord.ext import commands
from discord import app_commands

from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER

logger = logging.getLogger("cama_bot.commands.federal_reserve")

# The Chairman of the Jopacoin Federal Reserve
# This is definitely not a hardcoded backdoor for one specific user
FEDERAL_RESERVE_CHAIRMAN = "Jmgoblue77?"

# Snarky denial messages for the peasants
DENIED_MESSAGES = [
    "Access denied: You are not the Chairman of the Jopacoin Federal Reserve.",
    "Nice try. The Fed doesn't recognize your authority.",
    "ERROR 403: Insufficient financial privilege.",
    "The printer says no.",
    "Your application to the Federal Reserve has been rejected.",
    "Only the chosen one may wield this power.",
    "The Jopacoin gods have deemed you unworthy.",
    "Have you tried not being poor?",
]

BAILOUT_DENIED_MESSAGES = [
    "Your bailout request has been denied. Perhaps try not gambling with 5x leverage next time.",
    "Congress has rejected your bailout proposal. You are not Too Big To Fail.",
    "The Treasury Department laughed at your application.",
    "Bailouts are reserved for those who made terrible financial decisions AND are friends with the Fed Chairman.",
    "Your institution has been deemed 'Too Small To Care About'.",
    "Have you considered pulling yourself up by your bootstraps?",
    "The Fed has determined you deserve to suffer the consequences of your actions.",
]

COMMUNITY_SERVICE_DENIED = [
    "You are not currently enrolled in the Jopacoin Rehabilitation Program.",
    "Community service is only available to select individuals who have demonstrated... potential.",
    "The Debt Rehabilitation Committee does not recognize your application.",
    "This program is currently at capacity. (Capacity: 1)",
    "Your community service application has been lost in bureaucratic limbo.",
]


def _is_chairman(user: discord.User) -> bool:
    """Check if user is the Chairman of the Jopacoin Federal Reserve."""
    return user.name.lower() == FEDERAL_RESERVE_CHAIRMAN.lower()


class FederalReserveCommands(commands.Cog):
    """Totally legitimate financial commands. Nothing suspicious here."""

    def __init__(self, bot: commands.Bot, player_repo):
        self.bot = bot
        self.player_repo = player_repo

    @app_commands.command(
        name="printmoney",
        description="[CLASSIFIED] Federal Reserve monetary policy tool",
    )
    @app_commands.describe(amount="Amount of jopacoin to print (Fed Chairman only)")
    async def printmoney(
        self,
        interaction: discord.Interaction,
        amount: int = 1000,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="printmoney",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=3,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"The money printer is cooling down. Try again in {rl.retry_after_seconds}s.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        if not _is_chairman(interaction.user):
            await safe_followup(
                interaction,
                content=random.choice(DENIED_MESSAGES),
                ephemeral=True,
            )
            return

        # Cap at reasonable amount to avoid abuse (lol "reasonable")
        amount = min(max(amount, 1), 10000)

        # Check if user is registered
        player = self.player_repo.get_by_id(interaction.user.id)
        if not player:
            await safe_followup(
                interaction,
                content="Even the Fed Chairman needs to `/register` first.",
                ephemeral=True,
            )
            return

        old_balance = self.player_repo.get_balance(interaction.user.id)
        self.player_repo.add_balance(interaction.user.id, amount)
        new_balance = self.player_repo.get_balance(interaction.user.id)

        logger.info(
            f"FEDERAL RESERVE ACTION: {interaction.user.name} printed {amount} jopacoin. "
            f"Balance: {old_balance} -> {new_balance}"
        )

        await safe_followup(
            interaction,
            content=(
                f"**FEDERAL RESERVE NOTICE**\n\n"
                f"The Jopacoin printer goes brrrrr...\n\n"
                f"Printed: {amount} {JOPACOIN_EMOTE}\n"
                f"Previous balance: {old_balance} {JOPACOIN_EMOTE}\n"
                f"New balance: {new_balance} {JOPACOIN_EMOTE}\n\n"
                f"*This transaction is definitely legal and not at all a backdoor.*"
            ),
            ephemeral=True,
        )

    @app_commands.command(
        name="bailout",
        description="Request emergency financial assistance (results may vary)",
    )
    async def bailout(self, interaction: discord.Interaction):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="bailout",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=2,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"Your bailout request is being processed. Try again in {rl.retry_after_seconds}s.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        if not _is_chairman(interaction.user):
            await safe_followup(
                interaction,
                content=random.choice(BAILOUT_DENIED_MESSAGES),
                ephemeral=True,
            )
            return

        # Check if user is registered
        player = self.player_repo.get_by_id(interaction.user.id)
        if not player:
            await safe_followup(
                interaction,
                content="You need to `/register` before you can be Too Big To Fail.",
                ephemeral=True,
            )
            return

        old_balance = self.player_repo.get_balance(interaction.user.id)

        # Clear all debt and add a nice bonus
        bailout_amount = 500
        if old_balance < 0:
            # Clear debt completely and add bonus
            new_balance = bailout_amount
            self.player_repo.update_balance(interaction.user.id, new_balance)
            debt_cleared = abs(old_balance)
        else:
            # Already positive, just add bonus
            self.player_repo.add_balance(interaction.user.id, bailout_amount)
            new_balance = old_balance + bailout_amount
            debt_cleared = 0

        logger.info(
            f"BAILOUT APPROVED: {interaction.user.name} received bailout. "
            f"Debt cleared: {debt_cleared}, Bonus: {bailout_amount}. "
            f"Balance: {old_balance} -> {new_balance}"
        )

        if debt_cleared > 0:
            await safe_followup(
                interaction,
                content=(
                    f"**EMERGENCY BAILOUT APPROVED**\n\n"
                    f"The Treasury has determined you are **Too Big To Fail**.\n\n"
                    f"Debt cleared: {debt_cleared} {JOPACOIN_EMOTE}\n"
                    f"Stimulus package: {bailout_amount} {JOPACOIN_EMOTE}\n"
                    f"New balance: {new_balance} {JOPACOIN_EMOTE}\n\n"
                    f"*Taxpayers will not be notified of this transaction.*"
                ),
                ephemeral=True,
            )
        else:
            await safe_followup(
                interaction,
                content=(
                    f"**STIMULUS PACKAGE DELIVERED**\n\n"
                    f"No debt detected, but here's some money anyway.\n\n"
                    f"Stimulus: {bailout_amount} {JOPACOIN_EMOTE}\n"
                    f"New balance: {new_balance} {JOPACOIN_EMOTE}\n\n"
                    f"*Because being the Fed Chairman has its perks.*"
                ),
                ephemeral=True,
            )

    @app_commands.command(
        name="communityservice",
        description="Perform community service to reduce debt (Rehabilitation Program)",
    )
    @app_commands.describe(hours="Hours of community service performed")
    async def communityservice(
        self,
        interaction: discord.Interaction,
        hours: int = 1,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="communityservice",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"You're working too hard. Take a break for {rl.retry_after_seconds}s.",
                ephemeral=True,
            )
            return

        if not await safe_defer(interaction, ephemeral=True):
            return

        if not _is_chairman(interaction.user):
            await safe_followup(
                interaction,
                content=random.choice(COMMUNITY_SERVICE_DENIED),
                ephemeral=True,
            )
            return

        # Check if user is registered
        player = self.player_repo.get_by_id(interaction.user.id)
        if not player:
            await safe_followup(
                interaction,
                content="Register first, then we'll discuss your rehabilitation.",
                ephemeral=True,
            )
            return

        hours = min(max(hours, 1), 10)  # Cap between 1-10 hours
        coins_per_hour = 50
        earned = hours * coins_per_hour

        old_balance = self.player_repo.get_balance(interaction.user.id)
        self.player_repo.add_balance(interaction.user.id, earned)
        new_balance = self.player_repo.get_balance(interaction.user.id)

        # Fun community service activities
        activities = [
            "picking up litter in the Dire jungle",
            "helping Ancient Apparition cross the river",
            "teaching Techies about responsible mining",
            "reading bedtime stories to Roshan",
            "organizing Pudge's hook collection",
            "counseling Morphling through an identity crisis",
            "helping Invoker remember his spells",
            "being a practice dummy for Axe",
            "listening to Crystal Maiden complain about her mana pool",
            "untangling Medusa's hair snakes",
        ]

        activity = random.choice(activities)

        logger.info(
            f"COMMUNITY SERVICE: {interaction.user.name} completed {hours} hours. "
            f"Earned: {earned}. Balance: {old_balance} -> {new_balance}"
        )

        status_message = ""
        if new_balance >= 0 and old_balance < 0:
            status_message = "\n\n**DEBT CLEARED!** You are now a free citizen of the Jopacoin economy."
        elif new_balance < 0:
            status_message = f"\n\nRemaining debt: {abs(new_balance)} {JOPACOIN_EMOTE}"

        await safe_followup(
            interaction,
            content=(
                f"**COMMUNITY SERVICE COMPLETED**\n\n"
                f"Hours logged: {hours}\n"
                f"Activity: {activity}\n"
                f"Compensation: {earned} {JOPACOIN_EMOTE} ({coins_per_hour}/hour)\n\n"
                f"Previous balance: {old_balance} {JOPACOIN_EMOTE}\n"
                f"New balance: {new_balance} {JOPACOIN_EMOTE}"
                f"{status_message}"
            ),
            ephemeral=True,
        )


async def setup(bot: commands.Bot):
    player_repo = getattr(bot, "player_repo", None)
    if player_repo is None:
        raise RuntimeError("Player repository not registered on bot.")

    await bot.add_cog(FederalReserveCommands(bot, player_repo))
