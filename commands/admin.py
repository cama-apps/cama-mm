"""
Admin commands: maintenance helpers and testing utilities.
"""

import logging
import random

import discord
from discord import app_commands
from discord.ext import commands

from services.permissions import has_admin_permission
from utils.formatting import ROLE_EMOJIS
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER

logger = logging.getLogger("cama_bot.commands.admin")

# Module-level tracking: shared across all AdminCommands instances
# This prevents duplicate responses even if the command is registered multiple times
_processed_interactions = set()


class AdminCommands(commands.Cog):
    """Admin-only slash commands."""

    def __init__(self, bot: commands.Bot, lobby_service, player_repo, loan_service=None, bankruptcy_service=None):
        self.bot = bot
        self.lobby_service = lobby_service
        self.player_repo = player_repo
        self.loan_service = loan_service
        self.bankruptcy_service = bankruptcy_service

    @app_commands.command(
        name="addfake", description="Add fake users to lobby for testing (Admin only)"
    )
    @app_commands.describe(count="Number of fake users to add (1-10)")
    async def addfake(self, interaction: discord.Interaction, count: int = 1):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="addfake",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=2,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/addfake` again.",
                ephemeral=True,
            )
            return

        # Response guard: Check if this interaction has already been processed (module-level tracking)
        interaction_key = f"{interaction.id}_{interaction.user.id}"
        if interaction_key in _processed_interactions:
            logger.warning(
                f"addfake command called multiple times for interaction {interaction.id} "
                f"by user {interaction.user.id} ({interaction.user}) - already processed"
            )
            return

        # Mark interaction as being processed
        _processed_interactions.add(interaction_key)

        # Clean up old entries (keep only last 1000 to prevent memory leak)
        if len(_processed_interactions) > 1000:
            # Remove oldest entries (simple approach: clear half)
            _processed_interactions.clear()
            # Note: We clear entirely to avoid complexity, interactions expire after 15 minutes anyway

        logger.info(
            f"addfake command invoked by user {interaction.user.id} ({interaction.user}) "
            f"with count={count}"
        )

        # Check if defer succeeds - if False, another handler already processed this
        defer_success = await safe_defer(interaction, ephemeral=True)
        if not defer_success:
            logger.warning(
                f"addfake: Failed to defer interaction {interaction.id} - "
                f"likely already processed by another handler"
            )
            return

        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        if count < 1 or count > 10:
            await safe_followup(
                interaction,
                content="❌ Count must be between 1 and 10.",
                ephemeral=True,
            )
            return

        lobby = self.lobby_service.get_or_create_lobby()
        current = lobby.get_player_count()
        if current + count > self.lobby_service.max_players:
            await safe_followup(
                interaction,
                content=(
                    f"❌ Adding {count} users would exceed {self.lobby_service.max_players} players. "
                    f"Currently {current}/{self.lobby_service.max_players}."
                ),
                ephemeral=True,
            )
            return

        fake_users_added = []
        role_choices = list(ROLE_EMOJIS.keys())
        next_index = 1

        for _ in range(count):
            fake_id = -next_index
            fake_name = f"FakeUser{next_index}"
            next_index += 1

            existing = self.player_repo.get_by_id(fake_id)
            if not existing:
                rating = random.randint(1000, 2000)
                rd = random.uniform(50, 350)
                vol = 0.06
                num_roles = random.randint(1, min(5, len(role_choices)))
                roles = random.sample(role_choices, k=num_roles)
                try:
                    self.player_repo.add(
                        discord_id=fake_id,
                        discord_username=fake_name,
                        initial_mmr=None,
                        glicko_rating=rating,
                        glicko_rd=rd,
                        glicko_volatility=vol,
                        preferred_roles=roles,
                    )
                except ValueError:
                    pass

            lobby.add_player(fake_id)
            fake_users_added.append(fake_name)

        # Update lobby message if it exists
        message_id = self.lobby_service.get_lobby_message_id()
        if message_id:
            try:
                channel = interaction.channel
                message = await channel.fetch_message(message_id)
                embed = self.lobby_service.build_lobby_embed(lobby)
                if embed:
                    await message.edit(embed=embed)
            except Exception as exc:
                logger.warning(f"Failed to refresh lobby message after addfake: {exc}")

        await safe_followup(
            interaction,
            content=(
                f"✅ Added {len(fake_users_added)} fake user(s): " + ", ".join(fake_users_added)
            ),
            ephemeral=True,
        )

    @app_commands.command(
        name="resetuser", description="Reset a specific user's account (Admin only)"
    )
    @app_commands.describe(user="The user whose account to reset")
    async def resetuser(self, interaction: discord.Interaction, user: discord.Member):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="resetuser",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=2,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/resetuser` again.",
                ephemeral=True,
            )
            return

        await safe_defer(interaction, ephemeral=True)

        if not has_admin_permission(interaction):
            await safe_followup(
                interaction,
                content="❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        player = self.player_repo.get_by_id(user.id)
        if not player:
            await safe_followup(
                interaction,
                content=f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        deleted = self.player_repo.delete(user.id)
        if deleted:
            await safe_followup(
                interaction,
                content=f"✅ Reset {user.mention}'s account. They can register again.",
                ephemeral=True,
            )
            try:
                await user.send(
                    f"Your account was reset by an administrator ({interaction.user.mention}). You can register again with `/register`."
                )
            except Exception:
                pass
        else:
            await safe_followup(
                interaction,
                content=f"❌ Failed to reset {user.mention}'s account.",
                ephemeral=True,
            )

    @app_commands.command(name="sync", description="Force sync commands (Admin only)")
    async def sync(self, interaction: discord.Interaction):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="sync",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=1,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/sync` again.",
                ephemeral=True,
            )
            return

        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        await safe_defer(interaction, ephemeral=True)
        try:
            synced_count = 0
            for guild in self.bot.guilds:
                synced = await self.bot.tree.sync(guild=guild)
                synced_count += len(synced)
            synced_global = await self.bot.tree.sync()
            total = synced_count + len(synced_global)
            await safe_followup(
                interaction,
                content=f"✅ Synced {total} command(s) to {len(self.bot.guilds)} guild(s) and globally.",
                ephemeral=True,
            )
        except Exception as exc:
            logger.error(f"Error syncing commands: {exc}", exc_info=True)
            await safe_followup(
                interaction,
                content=f"❌ Error syncing commands: {exc}",
                ephemeral=True,
            )

    @app_commands.command(
        name="givecoin", description="Give jopacoin to a user (Admin only)"
    )
    @app_commands.describe(
        user="The user to give coins to",
        amount="Amount to give (can be negative to take)",
    )
    async def givecoin(
        self, interaction: discord.Interaction, user: discord.Member, amount: int
    ):
        """Admin command to give or take jopacoin from a user."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        player = self.player_repo.get_by_id(user.id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        old_balance = self.player_repo.get_balance(user.id)
        self.player_repo.add_balance(user.id, amount)
        new_balance = self.player_repo.get_balance(user.id)

        action = "gave" if amount >= 0 else "took"
        abs_amount = abs(amount)

        await interaction.response.send_message(
            f"✅ {action.title()} **{abs_amount}** jopacoin {'to' if amount >= 0 else 'from'} {user.mention}\n"
            f"Balance: {old_balance} → {new_balance}",
            ephemeral=True,
        )
        logger.info(
            f"Admin {interaction.user.id} ({interaction.user}) {action} {abs_amount} jopacoin "
            f"{'to' if amount >= 0 else 'from'} {user.id} ({user}). Balance: {old_balance} → {new_balance}"
        )

    @app_commands.command(
        name="resetloancooldown", description="Reset a user's loan cooldown (Admin only)"
    )
    @app_commands.describe(user="The user whose loan cooldown to reset")
    async def resetloancooldown(
        self, interaction: discord.Interaction, user: discord.Member
    ):
        """Admin command to reset a user's loan cooldown."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        if not self.loan_service:
            await interaction.response.send_message(
                "❌ Loan service not available.",
                ephemeral=True,
            )
            return

        player = self.player_repo.get_by_id(user.id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        # Get current state
        state = self.loan_service.get_state(user.id)

        # Reset the cooldown by setting last_loan_at to None
        self.loan_service.loan_repo.upsert_state(
            discord_id=user.id,
            last_loan_at=None,
            total_loans_taken=state.total_loans_taken,
            total_fees_paid=state.total_fees_paid,
            negative_loans_taken=state.negative_loans_taken,
            outstanding_principal=state.outstanding_principal,
            outstanding_fee=state.outstanding_fee,
        )

        await interaction.response.send_message(
            f"✅ Reset loan cooldown for {user.mention}. They can now take a new loan.",
            ephemeral=True,
        )
        logger.info(
            f"Admin {interaction.user.id} ({interaction.user}) reset loan cooldown for "
            f"{user.id} ({user})"
        )

    @app_commands.command(
        name="resetbankruptcycooldown", description="Reset a user's bankruptcy cooldown (Admin only)"
    )
    @app_commands.describe(user="The user whose bankruptcy cooldown to reset")
    async def resetbankruptcycooldown(
        self, interaction: discord.Interaction, user: discord.Member
    ):
        """Admin command to reset a user's bankruptcy cooldown."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        if not self.bankruptcy_service:
            await interaction.response.send_message(
                "❌ Bankruptcy service not available.",
                ephemeral=True,
            )
            return

        player = self.player_repo.get_by_id(user.id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        # Get current state and reset the cooldown
        state = self.bankruptcy_service.bankruptcy_repo.get_state(user.id)
        penalty_games = state["penalty_games_remaining"] if state else 0

        # Reset the cooldown by setting last_bankruptcy_at to None (or 0)
        self.bankruptcy_service.bankruptcy_repo.upsert_state(
            discord_id=user.id,
            last_bankruptcy_at=0,  # Far in the past = no cooldown
            penalty_games_remaining=penalty_games,
        )

        await interaction.response.send_message(
            f"✅ Reset bankruptcy cooldown for {user.mention}. They can now declare bankruptcy again.",
            ephemeral=True,
        )
        logger.info(
            f"Admin {interaction.user.id} ({interaction.user}) reset bankruptcy cooldown for "
            f"{user.id} ({user})"
        )


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    # Use player_repo directly from bot for admin operations
    player_repo = getattr(bot, "player_repo", None)
    loan_service = getattr(bot, "loan_service", None)
    bankruptcy_service = getattr(bot, "bankruptcy_service", None)

    # Check if cog is already loaded
    if "AdminCommands" in [cog.__class__.__name__ for cog in bot.cogs.values()]:
        logger.warning("AdminCommands cog is already loaded, skipping duplicate registration")
        return

    await bot.add_cog(AdminCommands(bot, lobby_service, player_repo, loan_service, bankruptcy_service))

    # Log command registration
    admin_commands = [
        cmd.name for cmd in bot.tree.walk_commands() if cmd.name in ["addfake", "resetuser", "sync", "givecoin", "resetloancooldown", "resetbankruptcycooldown"]
    ]
    logger.info(
        f"AdminCommands cog loaded. Registered commands: {admin_commands}. "
        f"Total addfake commands found: {len([c for c in bot.tree.walk_commands() if c.name == 'addfake'])}"
    )
