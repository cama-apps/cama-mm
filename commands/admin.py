"""
Admin commands: maintenance helpers and testing utilities.
"""

import asyncio
import functools
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
        player_service,
        loan_service=None,
        bankruptcy_service=None,
        recalibration_service=None,
        match_service=None,
    ):
        self.bot = bot
        self.lobby_service = lobby_service
        self.player_service = player_service
        self.loan_service = loan_service
        self.bankruptcy_service = bankruptcy_service
        self.recalibration_service = recalibration_service
        self.match_service = match_service

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

        lobby = await asyncio.to_thread(self.lobby_service.get_or_create_lobby)
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

        role_choices = list(ROLE_EMOJIS.keys())

        # guild_id for fake users - use None for global (they're not guild-specific)
        addfake_guild_id = interaction.guild.id if interaction.guild else None

        def _add_fake_users():
            fake_users_added = []
            # Find highest existing fake user index to continue from there
            lobby_snap = self.lobby_service.get_lobby()
            existing_fake_ids = [pid for pid in lobby_snap.players if pid < 0]
            next_index = max([-pid for pid in existing_fake_ids], default=0) + 1

            for _ in range(count):
                fake_id = -next_index
                fake_name = f"FakeUser{next_index}"
                next_index += 1

                existing = self.player_service.get_player(fake_id, addfake_guild_id)
                if not existing:
                    rating = random.randint(1000, 2000)
                    rd = random.uniform(50, 350)
                    vol = 0.06
                    num_roles = random.randint(1, min(5, len(role_choices)))
                    roles = random.sample(role_choices, k=num_roles)
                    try:
                        self.player_service.add_fake_player(
                            discord_id=fake_id,
                            discord_username=fake_name,
                            guild_id=addfake_guild_id,
                            glicko_rating=rating,
                            glicko_rd=rd,
                            glicko_volatility=vol,
                            preferred_roles=roles,
                        )
                    except ValueError:
                        pass

                # Set captain eligibility if requested
                if captain_eligible:
                    self.player_service.set_captain_eligible(fake_id, addfake_guild_id, True)

                success, _ = self.lobby_service.join_lobby(fake_id)
                if success:
                    fake_users_added.append(fake_name)

            return fake_users_added

        fake_users_added = await asyncio.to_thread(_add_fake_users)

        # Update lobby message if it exists
        lobby = self.lobby_service.get_lobby()  # In-memory read
        message_id = self.lobby_service.get_lobby_message_id()
        channel_id = self.lobby_service.get_lobby_channel_id()
        if message_id and channel_id and lobby:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                message = await channel.fetch_message(message_id)
                embed = await asyncio.to_thread(
                    self.lobby_service.build_lobby_embed, lobby, addfake_guild_id
                )
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

        lobby = await asyncio.to_thread(self.lobby_service.get_or_create_lobby)
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

        # guild_id for fake users
        fill_guild_id = interaction.guild.id if interaction.guild else None

        def _fill_lobby():
            fake_users_added = []
            # Find highest existing fake user index
            existing_fake_ids = [pid for pid in lobby.players if pid < 0]
            next_index = max([-pid for pid in existing_fake_ids], default=0) + 1

            for _ in range(needed):
                fake_id = -next_index
                fake_name = f"FakeUser{next_index}"
                next_index += 1

                existing = self.player_service.get_player(fake_id, fill_guild_id)
                if not existing:
                    rating = random.randint(1000, 2000)
                    rd = random.uniform(50, 350)
                    vol = 0.06
                    num_roles = random.randint(1, min(5, len(role_choices)))
                    roles = random.sample(role_choices, k=num_roles)
                    try:
                        self.player_service.add_fake_player(
                            discord_id=fake_id,
                            discord_username=fake_name,
                            guild_id=fill_guild_id,
                            glicko_rating=rating,
                            glicko_rd=rd,
                            glicko_volatility=vol,
                            preferred_roles=roles,
                        )
                    except ValueError:
                        pass

                if captain_eligible:
                    self.player_service.set_captain_eligible(fake_id, fill_guild_id, True)

                success, _ = self.lobby_service.join_lobby(fake_id)
                if success:
                    fake_users_added.append((fake_name, fake_id))
                    # Backdate join time for readycheck testing variety
                    import time as _time
                    offsets = [60, 5 * 60, 15 * 60, 30 * 60, 3600, 2 * 3600, 4 * 3600, 8 * 3600, 12 * 3600, 86400]
                    offset = offsets[(len(fake_users_added) - 1) % len(offsets)]
                    lobby_ref = self.lobby_service.get_lobby()
                    if lobby_ref and fake_id in lobby_ref.player_join_times:
                        lobby_ref.player_join_times[fake_id] = _time.time() - offset

            # Persist backdated join times to DB
            if fake_users_added:
                self.lobby_service.lobby_manager._persist_lobby()

            return [name for name, _ in fake_users_added]

        fake_users_added = await asyncio.to_thread(_fill_lobby)

        # Update lobby message if it exists
        lobby = self.lobby_service.get_lobby()  # In-memory read
        message_id = self.lobby_service.get_lobby_message_id()
        channel_id = self.lobby_service.get_lobby_channel_id()
        if message_id and channel_id and lobby:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                message = await channel.fetch_message(message_id)
                embed = await asyncio.to_thread(
                    self.lobby_service.build_lobby_embed, lobby, fill_guild_id
                )
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

        guild_id = interaction.guild.id if interaction.guild else None
        player = await asyncio.to_thread(self.player_service.get_player, user.id, guild_id)
        if not player:
            await safe_followup(
                interaction,
                content=f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        deleted = await asyncio.to_thread(self.player_service.delete_player, user.id, guild_id)
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

    @app_commands.command(
        name="registeruser", description="Register another user as a player (Admin only)"
    )
    @app_commands.describe(
        user="The user to register",
        steam_id="Steam32 ID (found in Dotabuff URL)",
        mmr="Optional MMR override (0-12000)",
    )
    async def registeruser(
        self,
        interaction: discord.Interaction,
        user: discord.Member,
        steam_id: int,
        mmr: int = None,
    ):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="registeruser",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=5,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/registeruser` again.",
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

        # Get player_service from bot
        player_service = getattr(self.bot, "player_service", None)
        if not player_service:
            await safe_followup(
                interaction,
                content="❌ Player service not available.",
                ephemeral=True,
            )
            return

        # Validate MMR if provided
        if mmr is not None and (mmr < 0 or mmr > 12000):
            await safe_followup(
                interaction,
                content="❌ MMR must be between 0 and 12000.",
                ephemeral=True,
            )
            return

        try:
            result = await asyncio.to_thread(
                functools.partial(
                    player_service.register_player,
                    discord_id=user.id,
                    discord_username=str(user),
                    steam_id=steam_id,
                    mmr_override=mmr,
                )
            )
            await safe_followup(
                interaction,
                content=(
                    f"✅ Registered {user.mention}!\n"
                    f"Cama Rating: {result['cama_rating']} ({result['uncertainty']:.0f}% uncertainty)\n"
                    f"They can use `/setroles` to set their preferred roles."
                ),
                ephemeral=True,
            )
        except ValueError as e:
            await safe_followup(
                interaction,
                content=f"❌ {str(e)}",
                ephemeral=True,
            )
        except Exception as e:
            logger.error(
                f"Error in registeruser command for user {user.id}: {str(e)}", exc_info=True
            )
            await safe_followup(
                interaction,
                content="❌ Unexpected error registering user. Check logs.",
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

    @app_commands.command(name="givecoin", description="Give jopacoin to a user or nonprofit (Admin only)")
    @app_commands.describe(
        user="The user to give coins to (leave empty if targeting nonprofit)",
        amount="Amount to give (can be negative to take)",
        nonprofit="Give to the nonprofit fund instead of a user",
    )
    async def givecoin(
        self,
        interaction: discord.Interaction,
        amount: int,
        user: discord.Member | None = None,
        nonprofit: bool = False,
    ):
        """Admin command to give or take jopacoin from a user or nonprofit fund."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        # Validate target - must specify exactly one
        if nonprofit and user:
            await interaction.response.send_message(
                "❌ Cannot specify both a user and nonprofit. Choose one target.",
                ephemeral=True,
            )
            return

        if not nonprofit and not user:
            await interaction.response.send_message(
                "❌ Must specify either a user or set nonprofit=True.",
                ephemeral=True,
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Handle nonprofit fund target
        if nonprofit:
            if not self.loan_service:
                await interaction.response.send_message(
                    "❌ Loan service not available.",
                    ephemeral=True,
                )
                return
            old_balance = await asyncio.to_thread(
                self.loan_service.get_nonprofit_fund, guild_id
            )

            if amount >= 0:
                await asyncio.to_thread(
                    self.loan_service.add_to_nonprofit_fund, guild_id, amount
                )
                new_balance = await asyncio.to_thread(
                    self.loan_service.get_nonprofit_fund, guild_id
                )
                await interaction.response.send_message(
                    f"✅ Added **{amount}** jopacoin to the nonprofit fund\n"
                    f"Fund balance: {old_balance} → {new_balance}",
                    ephemeral=True,
                )
            else:
                # Taking from nonprofit
                abs_amount = abs(amount)
                if old_balance < abs_amount:
                    await interaction.response.send_message(
                        f"❌ Nonprofit fund only has {old_balance} jopacoin. Cannot take {abs_amount}.",
                        ephemeral=True,
                    )
                    return
                await asyncio.to_thread(
                    self.loan_service.subtract_from_nonprofit_fund, guild_id, abs_amount
                )
                new_balance = await asyncio.to_thread(
                    self.loan_service.get_nonprofit_fund, guild_id
                )
                await interaction.response.send_message(
                    f"✅ Took **{abs_amount}** jopacoin from the nonprofit fund\n"
                    f"Fund balance: {old_balance} → {new_balance}",
                    ephemeral=True,
                )

            logger.info(
                f"Admin {interaction.user.id} ({interaction.user}) modified nonprofit fund by {amount}. "
                f"Balance: {old_balance} → {new_balance}"
            )
            return

        # Handle user target (original behavior)
        player = await asyncio.to_thread(self.player_service.get_player, user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        old_balance = await asyncio.to_thread(
            self.player_service.get_balance, user.id, guild_id
        )
        new_balance = await asyncio.to_thread(
            self.player_service.adjust_balance, user.id, guild_id, amount
        )

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

        guild_id = interaction.guild.id if interaction.guild else None
        player = await asyncio.to_thread(self.player_service.get_player, user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        # Reset the cooldown via service method
        await asyncio.to_thread(
            self.loan_service.reset_loan_cooldown, user.id, guild_id
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

        guild_id = interaction.guild.id if interaction.guild else None
        player = await asyncio.to_thread(self.player_service.get_player, user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        # Reset cooldown AND clear penalty games via service method
        reset = await asyncio.to_thread(
            self.bankruptcy_service.reset_cooldown, user.id, guild_id
        )

        if not reset:
            await interaction.response.send_message(
                f"ℹ️ {user.mention} has no bankruptcy history to reset.",
                ephemeral=True,
            )
            return

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

        guild_id = interaction.guild.id if interaction.guild else None
        player = await asyncio.to_thread(self.player_service.get_player, user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        games = await asyncio.to_thread(
            self.player_service.get_game_count, user.id, guild_id
        )
        if games >= ADMIN_RATING_ADJUSTMENT_MAX_GAMES:
            await interaction.response.send_message(
                "❌ Player has too many games for initial rating adjustment.",
                ephemeral=True,
            )
            return

        # Keep existing RD and volatility if available
        rd = 300.0
        vol = 0.06
        rating_data = await asyncio.to_thread(
            self.player_service.get_glicko_rating, user.id, guild_id
        )
        if rating_data:
            _current_rating, current_rd, current_vol = rating_data
            if current_rd is not None:
                rd = current_rd
            if current_vol is not None:
                vol = current_vol

        await asyncio.to_thread(
            self.player_service.update_glicko_rating, user.id, guild_id, rating, rd, vol
        )

        await interaction.response.send_message(
            f"✅ Set initial rating for {user.mention} to {rating} (RD kept at {rd:.1f}).",
            ephemeral=True,
        )
        logger.info(
            f"Admin {interaction.user.id} ({interaction.user}) set initial rating for "
            f"{user.id} ({user}) to {rating} with RD={rd:.1f}"
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

        guild_id = interaction.guild.id if interaction.guild else None
        result = await asyncio.to_thread(
            self.recalibration_service.can_recalibrate, user.id, guild_id
        )
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
        recal_result = await asyncio.to_thread(
            self.recalibration_service.recalibrate, user.id, guild_id
        )
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

        guild_id = interaction.guild.id if interaction.guild else None
        player = await asyncio.to_thread(self.player_service.get_player, user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        result = await asyncio.to_thread(
            self.recalibration_service.reset_cooldown, user.id, guild_id
        )
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
        pending_state = await asyncio.to_thread(match_service.get_last_shuffle, guild_id)
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
        match_service.set_last_shuffle(guild_id, pending_state)  # In-memory only
        await asyncio.to_thread(
            match_service._persist_match_state, guild_id, pending_state
        )

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
                            totals = await asyncio.to_thread(
                                functools.partial(
                                    betting_service.get_pot_odds,
                                    guild_id,
                                    pending_state=pending_state,
                                )
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

    @app_commands.command(
        name="correctmatch",
        description="Correct an incorrectly recorded match result (Admin only)",
    )
    @app_commands.describe(
        match_id="The match ID to correct",
        correct_result="The correct winning team",
    )
    @app_commands.choices(
        correct_result=[
            app_commands.Choice(name="Radiant", value="radiant"),
            app_commands.Choice(name="Dire", value="dire"),
        ]
    )
    async def correctmatch(
        self,
        interaction: discord.Interaction,
        match_id: int,
        correct_result: app_commands.Choice[str],
    ):
        """Admin command to correct an incorrectly recorded match result.

        This reverses all effects of the original recording and re-applies
        them with the correct winning team, including:
        - Win/loss counters
        - Glicko-2 and OpenSkill ratings
        - Bet payouts
        - Pairings statistics
        """
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        await safe_defer(interaction, ephemeral=True)

        # Get match_service from bot
        match_service = getattr(self.bot, "match_service", None)
        if not match_service:
            await safe_followup(
                interaction,
                content="❌ Match service not available.",
                ephemeral=True,
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None

        try:
            result = await asyncio.to_thread(
                functools.partial(
                    match_service.correct_match_result,
                    match_id=match_id,
                    new_winning_team=correct_result.value,
                    guild_id=guild_id,
                    corrected_by=interaction.user.id,
                )
            )

            # Build response message
            old_team = result["old_winning_team"].title()
            new_team = result["new_winning_team"].title()
            players_affected = result["players_affected"]
            ratings_updated = result["ratings_updated"]

            response_lines = [
                f"✅ **Match #{match_id} corrected!**",
                f"• Result changed: {old_team} → **{new_team}**",
                f"• Players affected: {players_affected}",
                f"• Ratings updated: {ratings_updated}",
            ]

            # Add bet correction info if applicable
            bet_info = result.get("bet_correction", {})
            if bet_info:
                bets_affected = bet_info.get("bets_affected", 0)
                if bets_affected > 0:
                    old_winners = bet_info.get("old_winners_reversed", 0)
                    new_winners = bet_info.get("new_winners_paid", 0)
                    response_lines.append(
                        f"• Bets corrected: {bets_affected} "
                        f"({old_winners} reversed, {new_winners} paid)"
                    )

            response_lines.append(
                f"\n*Correction ID: {result.get('correction_id', 'N/A')}*"
            )

            await safe_followup(
                interaction,
                content="\n".join(response_lines),
                ephemeral=True,
            )

            logger.info(
                f"Admin {interaction.user.id} ({interaction.user}) corrected match {match_id}: "
                f"{old_team} -> {new_team}"
            )

        except ValueError as e:
            await safe_followup(
                interaction,
                content=f"❌ {str(e)}",
                ephemeral=True,
            )
        except Exception as e:
            logger.error(
                f"Error correcting match {match_id}: {str(e)}", exc_info=True
            )
            await safe_followup(
                interaction,
                content=f"❌ Unexpected error correcting match: {str(e)}",
                ephemeral=True,
            )


    # --- Multi-Steam ID Admin Commands ---

    @app_commands.command(
        name="adminaddsteamid",
        description="Add a Steam ID to a player's account (Admin only)",
    )
    @app_commands.describe(
        user="The user to add the Steam ID to",
        steam_id="Steam32 ID to add",
        set_primary="Set as primary account (default: False)",
    )
    async def adminaddsteamid(
        self,
        interaction: discord.Interaction,
        user: discord.Member,
        steam_id: int,
        set_primary: bool = False,
    ):
        """Admin command to add a Steam ID to any player's account."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        player = await asyncio.to_thread(self.player_service.get_player, user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        # Validate steam_id
        if steam_id <= 0 or steam_id > 2**32:
            await interaction.response.send_message(
                "❌ Invalid Steam ID.",
                ephemeral=True,
            )
            return

        try:
            current_ids = await asyncio.to_thread(self.player_service.get_steam_ids, user.id)
            is_first = len(current_ids) == 0
            await asyncio.to_thread(
                functools.partial(
                    self.player_service.add_steam_id,
                    user.id,
                    steam_id,
                    is_primary=set_primary or is_first,
                )
            )

            primary_note = " (set as primary)" if set_primary or is_first else ""
            await interaction.response.send_message(
                f"✅ Added Steam ID `{steam_id}` to {user.mention}'s account{primary_note}.",
                ephemeral=True,
            )
            logger.info(
                f"Admin {interaction.user.id} ({interaction.user}) added steam_id {steam_id} "
                f"to {user.id} ({user}), primary={set_primary or is_first}"
            )
        except ValueError as e:
            await interaction.response.send_message(
                f"❌ {str(e)}",
                ephemeral=True,
            )

    @app_commands.command(
        name="adminremovesteamid",
        description="Remove a Steam ID from a player's account (Admin only)",
    )
    @app_commands.describe(
        user="The user to remove the Steam ID from",
        steam_id="Steam32 ID to remove",
    )
    async def adminremovesteamid(
        self,
        interaction: discord.Interaction,
        user: discord.Member,
        steam_id: int,
    ):
        """Admin command to remove a Steam ID from any player's account."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        player = await asyncio.to_thread(self.player_service.get_player, user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        current_ids = await asyncio.to_thread(self.player_service.get_steam_ids, user.id)
        if steam_id not in current_ids:
            await interaction.response.send_message(
                f"❌ Steam ID `{steam_id}` is not linked to {user.mention}.",
                ephemeral=True,
            )
            return

        removed = await asyncio.to_thread(self.player_service.remove_steam_id, user.id, steam_id)
        if removed:
            remaining = await asyncio.to_thread(self.player_service.get_steam_ids, user.id)
            if remaining:
                await interaction.response.send_message(
                    f"✅ Removed Steam ID `{steam_id}` from {user.mention}'s account.\n"
                    f"Primary is now `{remaining[0]}`.",
                    ephemeral=True,
                )
            else:
                await interaction.response.send_message(
                    f"✅ Removed Steam ID `{steam_id}` from {user.mention}'s account.\n"
                    "They no longer have any linked Steam accounts.",
                    ephemeral=True,
                )
            logger.info(
                f"Admin {interaction.user.id} ({interaction.user}) removed steam_id {steam_id} "
                f"from {user.id} ({user})"
            )
        else:
            await interaction.response.send_message(
                f"❌ Failed to remove Steam ID `{steam_id}`.",
                ephemeral=True,
            )

    @app_commands.command(
        name="adminsetprimarysteam",
        description="Set a player's primary Steam ID (Admin only)",
    )
    @app_commands.describe(
        user="The user to set primary Steam ID for",
        steam_id="Steam32 ID to set as primary (must already be linked)",
    )
    async def adminsetprimarysteam(
        self,
        interaction: discord.Interaction,
        user: discord.Member,
        steam_id: int,
    ):
        """Admin command to change which Steam ID is primary for a player."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message(
                "❌ Admin only! You need Administrator or Manage Server permissions.",
                ephemeral=True,
            )
            return

        guild_id = interaction.guild.id if interaction.guild else None
        player = await asyncio.to_thread(self.player_service.get_player, user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                f"⚠️ {user.mention} is not registered.",
                ephemeral=True,
            )
            return

        current_ids = await asyncio.to_thread(self.player_service.get_steam_ids, user.id)
        if steam_id not in current_ids:
            await interaction.response.send_message(
                f"❌ Steam ID `{steam_id}` is not linked to {user.mention}.\n"
                f"Linked accounts: {', '.join(f'`{sid}`' for sid in current_ids) if current_ids else 'none'}",
                ephemeral=True,
            )
            return

        if await asyncio.to_thread(self.player_service.set_primary_steam_id, user.id, steam_id):
            await interaction.response.send_message(
                f"✅ Set `{steam_id}` as {user.mention}'s primary Steam account.",
                ephemeral=True,
            )
            logger.info(
                f"Admin {interaction.user.id} ({interaction.user}) set primary steam_id to {steam_id} "
                f"for {user.id} ({user})"
            )
        else:
            await interaction.response.send_message(
                f"❌ Failed to set primary Steam ID.",
                ephemeral=True,
            )

    @app_commands.command(
        name="seedherogrid",
        description="Seed fake players with enriched match data for /herogrid testing (Admin only)",
    )
    async def seedherogrid(self, interaction: discord.Interaction):
        """Create 20 fake players with ~30 enriched matches for hero grid testing."""
        if not has_admin_permission(interaction):
            await interaction.response.send_message("❌ Admin only command.", ephemeral=True)
            return

        await interaction.response.defer(ephemeral=True)

        if not self.match_service:
            await interaction.followup.send("❌ match_service not available.", ephemeral=True)
            return

        # Hero pool: 20 popular heroes with IDs from heroes.json
        HERO_POOL = [
            1, 2, 5, 7, 8, 11, 14, 18, 19, 22,       # AM, Axe, CM, ES, Jug, SF, Pudge, Sven, Tiny, Zeus
            25, 29, 35, 39, 41, 44, 48, 49, 74, 86,    # Lina, Tide, Sniper, QoP, Void, PA, Luna, DK, Invoker, Rubick
        ]

        NUM_PLAYERS = 20
        NUM_MATCHES = 30
        BASE_ID = -1001  # Separate range from lobby fake users

        # Assign each player a "preferred" subset of 4-6 heroes (weighted picks)
        player_hero_pools = {}
        for i in range(NUM_PLAYERS):
            pool_size = random.randint(4, 6)
            player_hero_pools[i] = random.sample(HERO_POOL, k=pool_size)

        seed_guild_id = interaction.guild.id if interaction.guild else None

        def _seed_data():
            # 1. Create fake players
            player_ids = []
            for i in range(NUM_PLAYERS):
                pid = BASE_ID - i  # -1001, -1002, ...
                player_ids.append(pid)
                existing = self.player_service.get_player(pid, seed_guild_id)
                if not existing:
                    self.player_service.add_fake_player(
                        discord_id=pid,
                        discord_username=f"GridTest{i + 1}",
                        guild_id=seed_guild_id,
                        glicko_rating=random.randint(1000, 2000),
                        glicko_rd=random.uniform(50, 200),
                        glicko_volatility=0.06,
                        preferred_roles=random.sample(
                            ["1", "2", "3", "4", "5"], k=random.randint(1, 3)
                        ),
                    )

            # 2. Record matches with enrichment
            matches_created = 0
            for _ in range(NUM_MATCHES):
                shuffled = random.sample(player_ids, k=10)
                team1 = shuffled[:5]
                team2 = shuffled[5:]
                winning_team = random.choice([1, 2])

                mid = self.match_service.record_match_raw(
                    team1_ids=team1,
                    team2_ids=team2,
                    winning_team=winning_team,
                    guild_id=seed_guild_id,
                    lobby_type="shuffle",
                )

                # Enrich each participant
                for team_ids in [team1, team2]:
                    for pid in team_ids:
                        idx = player_ids.index(pid)
                        pool = player_hero_pools[idx]
                        # 70% chance pick from preferred pool, 30% any hero
                        if random.random() < 0.7:
                            hero_id = random.choice(pool)
                        else:
                            hero_id = random.choice(HERO_POOL)

                        self.match_service.update_participant_stats(
                            match_id=mid,
                            discord_id=pid,
                            hero_id=hero_id,
                            kills=random.randint(0, 25),
                            deaths=random.randint(0, 15),
                            assists=random.randint(0, 30),
                            gpm=random.randint(200, 800),
                            xpm=random.randint(200, 700),
                            hero_damage=random.randint(5000, 50000),
                            tower_damage=random.randint(500, 15000),
                            last_hits=random.randint(20, 400),
                            denies=random.randint(0, 40),
                            net_worth=random.randint(5000, 40000),
                        )

                matches_created += 1

            return matches_created

        matches_created = await asyncio.to_thread(_seed_data)

        await interaction.followup.send(
            f"✅ Seeded {NUM_PLAYERS} players and {matches_created} enriched matches.\n"
            f"Run `/herogrid source:All Players min_games:1` to see the grid.",
            ephemeral=True,
        )


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    player_service = getattr(bot, "player_service", None)
    loan_service = getattr(bot, "loan_service", None)
    bankruptcy_service = getattr(bot, "bankruptcy_service", None)
    recalibration_service = getattr(bot, "recalibration_service", None)
    match_service = getattr(bot, "match_service", None)

    # Check if cog is already loaded
    if "AdminCommands" in [cog.__class__.__name__ for cog in bot.cogs.values()]:
        logger.warning("AdminCommands cog is already loaded, skipping duplicate registration")
        return

    await bot.add_cog(
        AdminCommands(
            bot, lobby_service, player_service, loan_service, bankruptcy_service, recalibration_service, match_service
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
            "registeruser",
            "sync",
            "givecoin",
            "resetloancooldown",
            "resetbankruptcycooldown",
            "setinitialrating",
            "extendbetting",
            "recalibrate",
            "resetrecalibrationcooldown",
            "correctmatch",
        ]
    ]
    logger.info(
        f"AdminCommands cog loaded. Registered commands: {admin_commands}. "
        f"Total addfake commands found: {len([c for c in bot.tree.walk_commands() if c.name == 'addfake'])}"
    )
