"""
Admin commands: maintenance helpers and testing utilities.
"""

import logging
import random
import time

import discord
from discord import app_commands
from discord.ext import commands

from config import ADMIN_RATING_ADJUSTMENT_MAX_GAMES
from services.permissions import has_admin_permission
from utils.formatting import ROLE_EMOJIS, format_betting_display
from utils.interaction_safety import safe_defer, safe_followup
from utils.rate_limiter import GLOBAL_RATE_LIMITER

logger = logging.getLogger("cama_bot.commands.admin")

# Module-level tracking: shared across all AdminCommands instances
# This prevents duplicate responses even if the command is registered multiple times
_processed_interactions = set()


class AdminCommands(commands.Cog):
    """Admin-only slash commands."""

    def __init__(
        self,
        bot: commands.Bot,
        lobby_service,
        player_repo,
        loan_service=None,
        bankruptcy_service=None,
        recalibration_service=None,
    ):
        self.bot = bot
        self.lobby_service = lobby_service
        self.player_repo = player_repo
        self.loan_service = loan_service
        self.bankruptcy_service = bankruptcy_service
        self.recalibration_service = recalibration_service

    @app_commands.command(
        name="addfake", description="Add fake users to lobby for testing (Admin only)"
    )
    @app_commands.describe(
        count="Number of fake users to add (1-10)",
        captain_eligible="Make fake users captain-eligible for Immortal Draft testing",
    )
    async def addfake(
        self,
        interaction: discord.Interaction,
        count: int = 1,
        captain_eligible: bool = False,
    ):
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

        # Track if we can respond - continue processing even if defer fails
        can_respond = await safe_defer(interaction, ephemeral=True)

        if not has_admin_permission(interaction):
            if can_respond:
                await safe_followup(
                    interaction,
                    content="❌ Admin only! You need Administrator or Manage Server permissions.",
                    ephemeral=True,
                )
            return

        if count < 1 or count > 10:
            if can_respond:
                await safe_followup(
                    interaction,
                    content="❌ Count must be between 1 and 10.",
                    ephemeral=True,
                )
            return

        lobby = self.lobby_service.get_or_create_lobby()
        current = lobby.get_player_count()
        if current + count > self.lobby_service.max_players:
            if can_respond:
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

        # Find highest existing fake user index to continue from there
        lobby = self.lobby_service.get_lobby()
        existing_fake_ids = [pid for pid in lobby.players if pid < 0]
        next_index = max([-pid for pid in existing_fake_ids], default=0) + 1

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

            # Set captain eligibility if requested
            if captain_eligible:
                self.player_repo.set_captain_eligible(fake_id, True)

            success, _ = self.lobby_service.join_lobby(fake_id)
            if success:
                fake_users_added.append(fake_name)

        # Update lobby message if it exists
        lobby = self.lobby_service.get_lobby()  # Get fresh lobby state
        message_id = self.lobby_service.get_lobby_message_id()
        channel_id = self.lobby_service.get_lobby_channel_id()
        if message_id and channel_id and lobby:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                message = await channel.fetch_message(message_id)
                embed = self.lobby_service.build_lobby_embed(lobby)
                if embed:
                    await message.edit(embed=embed)
            except Exception as exc:
                logger.warning(f"Failed to refresh lobby message after addfake: {exc}")

        if can_respond:
            captain_note = " (captain-eligible)" if captain_eligible else ""
            await safe_followup(
                interaction,
                content=(
                    f"✅ Added {len(fake_users_added)} fake user(s){captain_note}: "
                    + ", ".join(fake_users_added)
                ),
                ephemeral=True,
            )
        logger.info(f"addfake completed: added {len(fake_users_added)} fake users")

    @app_commands.command(
        name="filllobbytest",
        description="Fill remaining lobby spots with fake users for testing (Admin only)",
    )
    @app_commands.describe(
        captain_eligible="Make fake users captain-eligible for Immortal Draft testing",
    )
    async def filllobbytest(
        self,
        interaction: discord.Interaction,
        captain_eligible: bool = False,
    ):
        """Fill lobby to ready threshold with fake users."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message("❌ Admin only command.", ephemeral=True)
            return

        await interaction.response.defer(ephemeral=True)

        lobby = self.lobby_service.get_or_create_lobby()
        current = lobby.get_player_count()
        ready_threshold = self.lobby_service.ready_threshold

        if current >= ready_threshold:
            await interaction.followup.send(
                f"✅ Lobby already has {current}/{ready_threshold} players.",
                ephemeral=True,
            )
            return

        needed = ready_threshold - current
        if needed > 10:
            needed = 10  # Cap at 10 per call for safety

        role_choices = list(ROLE_EMOJIS.keys())

        # Find highest existing fake user index
        existing_fake_ids = [pid for pid in lobby.players if pid < 0]
        next_index = max([-pid for pid in existing_fake_ids], default=0) + 1

        fake_users_added = []
        for _ in range(needed):
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

            if captain_eligible:
                self.player_repo.set_captain_eligible(fake_id, True)

            success, _ = self.lobby_service.join_lobby(fake_id)
            if success:
                fake_users_added.append(fake_name)

        # Update lobby message if it exists
        lobby = self.lobby_service.get_lobby()
        message_id = self.lobby_service.get_lobby_message_id()
        channel_id = self.lobby_service.get_lobby_channel_id()
        if message_id and channel_id and lobby:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                message = await channel.fetch_message(message_id)
                embed = self.lobby_service.build_lobby_embed(lobby)
                if embed:
                    await message.edit(embed=embed)
            except Exception as exc:
                logger.warning(f"Failed to refresh lobby message after filllobbytest: {exc}")

        captain_note = " (captain-eligible)" if captain_eligible else ""
        await interaction.followup.send(
            f"✅ Added {len(fake_users_added)} fake user(s){captain_note} to fill lobby.",
            ephemeral=True,
        )
        logger.info(f"filllobbytest completed: added {len(fake_users_added)} fake users")

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

    @app_commands.command(name="givecoin", description="Give jopacoin to a user (Admin only)")
    @app_commands.describe(
        user="The user to give coins to",
        amount="Amount to give (can be negative to take)",
    )
    async def givecoin(self, interaction: discord.Interaction, user: discord.Member, amount: int):
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
    async def resetloancooldown(self, interaction: discord.Interaction, user: discord.Member):
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

        # Reset the cooldown by setting last_loan_at to 0 (epoch = no cooldown)
        # Note: Can't use None because COALESCE in upsert keeps old value
        self.loan_service.loan_repo.upsert_state(
            discord_id=user.id,
            last_loan_at=0,
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
        name="resetbankruptcycooldown",
        description="Reset a user's bankruptcy cooldown (Admin only)",
    )
    @app_commands.describe(user="The user whose bankruptcy cooldown to reset")
    async def resetbankruptcycooldown(self, interaction: discord.Interaction, user: discord.Member):
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

        # Get current state
        state = self.bankruptcy_service.bankruptcy_repo.get_state(user.id)

        if not state:
            await interaction.response.send_message(
                f"ℹ️ {user.mention} has no bankruptcy history to reset.",
                ephemeral=True,
            )
            return

        # Reset cooldown AND clear penalty games (without incrementing bankruptcy count)
        self.bankruptcy_service.bankruptcy_repo.reset_cooldown_only(
            discord_id=user.id,
            last_bankruptcy_at=0,  # Far in the past = no cooldown
            penalty_games_remaining=0,  # Clear penalty games
        )

        await interaction.response.send_message(
            f"✅ Reset bankruptcy for {user.mention}. Cooldown and penalty games cleared.",
            ephemeral=True,
        )
        logger.info(
            f"Admin {interaction.user.id} ({interaction.user}) reset bankruptcy (cooldown + penalty) for "
            f"{user.id} ({user})"
        )

    @app_commands.command(name="setinitialrating", description="Set initial rating for a player")
    @app_commands.describe(
        user="Player to adjust (must have few games)",
        rating="Initial rating (0-3000)",
    )
    async def setinitialrating(
        self, interaction: discord.Interaction, user: discord.Member, rating: float
    ):
        """Admin command to set initial rating for low-game players."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        if rating < 0 or rating > 3000:
            await interaction.response.send_message(
                "❌ Rating must be between 0 and 3000.",
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

        games = 0
        if hasattr(self.player_repo, "get_game_count"):
            games = self.player_repo.get_game_count(user.id)
        if games >= ADMIN_RATING_ADJUSTMENT_MAX_GAMES:
            await interaction.response.send_message(
                "❌ Player has too many games for initial rating adjustment.",
                ephemeral=True,
            )
            return

        # Keep existing volatility if available
        vol = 0.06
        rating_data = self.player_repo.get_glicko_rating(user.id)
        if rating_data:
            _current_rating, _current_rd, current_vol = rating_data
            if current_vol is not None:
                vol = current_vol

        rd_reset = 300.0
        self.player_repo.update_glicko_rating(user.id, rating, rd_reset, vol)

        await interaction.response.send_message(
            f"✅ Set initial rating for {user.mention} to {rating} (RD reset to {rd_reset}).",
            ephemeral=True,
        )
        logger.info(
            f"Admin {interaction.user.id} ({interaction.user}) set initial rating for "
            f"{user.id} ({user}) to {rating} with RD={rd_reset}"
        )

    @app_commands.command(
        name="recalibrate", description="Reset rating uncertainty for a player (Admin only)"
    )
    @app_commands.describe(user="The player to recalibrate")
    async def recalibrate(
        self, interaction: discord.Interaction, user: discord.Member
    ):
        """Admin command to recalibrate a player's rating uncertainty."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        if not self.recalibration_service:
            await interaction.response.send_message(
                "❌ Recalibration service not available.",
                ephemeral=True,
            )
            return

        result = self.recalibration_service.can_recalibrate(user.id)
        if not result["allowed"]:
            reason = result["reason"]
            if reason == "not_registered":
                await interaction.response.send_message(
                    f"❌ {user.mention} is not registered.",
                    ephemeral=True,
                )
            elif reason == "no_rating":
                await interaction.response.send_message(
                    f"❌ {user.mention} has no Glicko rating.",
                    ephemeral=True,
                )
            elif reason == "insufficient_games":
                games_played = result.get("games_played", 0)
                min_games = result.get("min_games", 5)
                await interaction.response.send_message(
                    f"❌ {user.mention} must play at least {min_games} games before recalibrating. "
                    f"Current: {games_played} games.",
                    ephemeral=True,
                )
            elif reason == "on_cooldown":
                cooldown_ends = result.get("cooldown_ends_at")
                await interaction.response.send_message(
                    f"❌ {user.mention} is on recalibration cooldown. "
                    f"Can recalibrate again <t:{cooldown_ends}:R>.",
                    ephemeral=True,
                )
            else:
                await interaction.response.send_message(
                    f"❌ Cannot recalibrate: {reason}",
                    ephemeral=True,
                )
            return

        # Execute recalibration
        recal_result = self.recalibration_service.recalibrate(user.id)
        if not recal_result["success"]:
            await interaction.response.send_message(
                f"❌ Recalibration failed: {recal_result.get('reason', 'unknown error')}",
                ephemeral=True,
            )
            return

        old_rd = recal_result["old_rd"]
        new_rd = recal_result["new_rd"]
        rating = recal_result["old_rating"]
        total_recals = recal_result["total_recalibrations"]
        cooldown_ends = recal_result["cooldown_ends_at"]

        await interaction.response.send_message(
            f"✅ Recalibrated {user.mention}!\n"
            f"• Rating: **{rating:.0f}** (unchanged)\n"
            f"• RD: {old_rd:.1f} → **{new_rd:.0f}** (high uncertainty)\n"
            f"• Total recalibrations: {total_recals}\n"
            f"• Next recalibration available: <t:{cooldown_ends}:R>",
            ephemeral=True,
        )
        logger.info(
            f"Admin {interaction.user.id} ({interaction.user}) recalibrated "
            f"{user.id} ({user}): rating={rating:.0f}, RD {old_rd:.1f} -> {new_rd:.0f}"
        )

    @app_commands.command(
        name="resetrecalibrationcooldown", description="Reset a user's recalibration cooldown (Admin only)"
    )
    @app_commands.describe(user="The user whose recalibration cooldown to reset")
    async def resetrecalibrationcooldown(
        self, interaction: discord.Interaction, user: discord.Member
    ):
        """Admin command to reset a user's recalibration cooldown."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        if not self.recalibration_service:
            await interaction.response.send_message(
                "❌ Recalibration service not available.",
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

        result = self.recalibration_service.reset_cooldown(user.id)
        if not result["success"]:
            reason = result["reason"]
            if reason == "no_recalibration_history":
                await interaction.response.send_message(
                    f"ℹ️ {user.mention} has no recalibration history to reset.",
                    ephemeral=True,
                )
            else:
                await interaction.response.send_message(
                    f"❌ Failed to reset cooldown: {reason}",
                    ephemeral=True,
                )
            return

        await interaction.response.send_message(
            f"✅ Reset recalibration cooldown for {user.mention}. They can now recalibrate.",
            ephemeral=True,
        )
        logger.info(
            f"Admin {interaction.user.id} ({interaction.user}) reset recalibration cooldown for "
            f"{user.id} ({user})"
        )

    @app_commands.command(
        name="extendbetting",
        description="Extend the betting window for the current match (Admin only)",
    )
    @app_commands.describe(minutes="Number of minutes to extend betting (1-60)")
    async def extendbetting(self, interaction: discord.Interaction, minutes: int):
        """Admin command to extend the betting window for an active match."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        if minutes < 1 or minutes > 60:
            await interaction.response.send_message(
                "❌ Extension must be between 1 and 60 minutes.",
                ephemeral=True,
            )
            return

        # Get match_service from bot
        match_service = getattr(self.bot, "match_service", None)
        if not match_service:
            await interaction.response.send_message(
                "❌ Match service not available.",
                ephemeral=True,
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Check for pending match
        pending_state = match_service.get_last_shuffle(guild_id)
        if not pending_state:
            await interaction.response.send_message(
                "❌ No active match to extend betting for.",
                ephemeral=True,
            )
            return

        current_lock = pending_state.get("bet_lock_until")
        if not current_lock:
            await interaction.response.send_message(
                "❌ No betting window found for the current match.",
                ephemeral=True,
            )
            return

        # Calculate new lock time: extend from max(current_lock, now)
        now_ts = int(time.time())
        base_time = max(current_lock, now_ts)
        new_lock_until = base_time + (minutes * 60)

        # Update state
        pending_state["bet_lock_until"] = new_lock_until
        match_service.set_last_shuffle(guild_id, pending_state)
        match_service._persist_match_state(guild_id, pending_state)

        # Cancel existing and reschedule betting reminder tasks
        match_cog = self.bot.get_cog("MatchCommands")
        if match_cog:
            match_cog._cancel_betting_tasks(guild_id)
            # Schedule new reminders with the updated lock time
            await match_cog._schedule_betting_reminders(guild_id, new_lock_until)

        # Update the shuffle embed if we can find it
        message_id = pending_state.get("shuffle_message_id")
        channel_id = pending_state.get("shuffle_channel_id")
        embed_updated = False

        if message_id and channel_id:
            try:
                channel = self.bot.get_channel(channel_id)
                if channel:
                    message = await channel.fetch_message(message_id)
                    if message and message.embeds:
                        embed = message.embeds[0].copy()

                        # Find and update the betting field
                        betting_service = getattr(self.bot, "betting_service", None)
                        totals = {"radiant": 0, "dire": 0}
                        betting_mode = pending_state.get("betting_mode", "pool")

                        if betting_service:
                            totals = betting_service.get_pot_odds(
                                guild_id, pending_state=pending_state
                            )

                        new_field_name, new_field_value = format_betting_display(
                            totals["radiant"], totals["dire"], betting_mode, new_lock_until
                        )

                        # Find and replace the betting field (usually the last field or has "Wagers" in name)
                        new_fields = []
                        for field in embed.fields:
                            if (
                                "Wagers" in field.name
                                or "Current Wagers" in field.name
                                or "Pool" in field.name
                            ):
                                new_fields.append(
                                    discord.EmbedField(
                                        name=new_field_name, value=new_field_value, inline=False
                                    )
                                )
                            else:
                                new_fields.append(field)

                        embed.clear_fields()
                        for field in new_fields:
                            embed.add_field(name=field.name, value=field.value, inline=field.inline)

                        await message.edit(embed=embed)
                        embed_updated = True
            except Exception as exc:
                logger.warning(f"Failed to update shuffle embed after extending betting: {exc}")

        # Send public announcement
        jump_url = pending_state.get("shuffle_message_jump_url", "")
        jump_link = f" [View match]({jump_url})" if jump_url else ""

        await interaction.response.send_message(
            f"⏰ **Betting window extended by {minutes} minute(s)!** "
            f"Closes <t:{new_lock_until}:R>.{jump_link}"
        )

        status_note = " (embed updated)" if embed_updated else ""
        logger.info(
            f"Admin {interaction.user.id} ({interaction.user}) extended betting by {minutes} min "
            f"for guild {guild_id}. New lock: {new_lock_until}{status_note}"
        )


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    # Use player_repo directly from bot for admin operations
    player_repo = getattr(bot, "player_repo", None)
    loan_service = getattr(bot, "loan_service", None)
    bankruptcy_service = getattr(bot, "bankruptcy_service", None)
    recalibration_service = getattr(bot, "recalibration_service", None)

    # Check if cog is already loaded
    if "AdminCommands" in [cog.__class__.__name__ for cog in bot.cogs.values()]:
        logger.warning("AdminCommands cog is already loaded, skipping duplicate registration")
        return

    await bot.add_cog(
        AdminCommands(
            bot, lobby_service, player_repo, loan_service, bankruptcy_service, recalibration_service
        )
    )

    # Log command registration
    admin_commands = [
        cmd.name
        for cmd in bot.tree.walk_commands()
        if cmd.name
        in [
            "addfake",
            "resetuser",
            "sync",
            "givecoin",
            "resetloancooldown",
            "resetbankruptcycooldown",
            "setinitialrating",
            "extendbetting",
            "recalibrate",
            "resetrecalibrationcooldown",
        ]
    ]
    logger.info(
        f"AdminCommands cog loaded. Registered commands: {admin_commands}. "
        f"Total addfake commands found: {len([c for c in bot.tree.walk_commands() if c.name == 'addfake'])}"
    )
