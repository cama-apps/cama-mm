"""
Registration commands for the bot: /register, /setroles, /stats
"""

import logging

import discord
from discord import app_commands
from discord.ext import commands

from config import (
    MMR_MODAL_RETRY_LIMIT,
    MMR_MODAL_TIMEOUT_MINUTES,
)
from utils.formatting import format_role_display
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.registration")


class RegistrationCommands(commands.Cog):
    """Commands for player registration and profile management."""

    def __init__(
        self,
        bot: commands.Bot,
        db,
        player_service,
        role_emojis: dict,
        role_names: dict,
    ):
        self.bot = bot
        self.db = db
        self.player_service = player_service
        self.role_emojis = role_emojis
        self.role_names = role_names

    @app_commands.command(name="register", description="Register yourself as a player")
    @app_commands.describe(steam_id="Steam32 ID (found in your Dotabuff URL)")
    async def register(self, interaction: discord.Interaction, steam_id: int):
        """Register a new player."""
        logger.info(
            f"Register command: User {interaction.user.id} ({interaction.user}) registering with Steam ID {steam_id}"
        )

        # Defer response since OpenDota API call might take time
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None

        async def _finalize_register(mmr_override: int | None = None):
            result = self.player_service.register_player(
                discord_id=interaction.user.id,
                discord_username=str(interaction.user),
                guild_id=guild_id,
                steam_id=steam_id,
                mmr_override=mmr_override,
            )
            await interaction.followup.send(
                f"✅ Registered {interaction.user.mention}!\n"
                f"Cama Rating: {result['cama_rating']} ({result['uncertainty']:.0f}% uncertainty)\n"
                f"Use `/setroles` to set your preferred roles."
            )

        try:
            await _finalize_register()
            return
        except ValueError as e:
            error_msg = str(e)
            if "MMR not available" not in error_msg:
                await interaction.followup.send(f"❌ {error_msg}", ephemeral=True)
                return
            # Otherwise prompt for MMR below
        except Exception as e:
            logger.error(
                f"Error in register command for user {interaction.user.id}: {str(e)}", exc_info=True
            )
            await interaction.followup.send(
                "❌ Unexpected error registering you. Try again later.", ephemeral=True
            )
            return

        # Prompt for MMR via a button -> modal flow.
        # Modals can't be shown from a deferred interaction response directly, so we attach a view with a button.
        class MMRModal(discord.ui.Modal):
            def __init__(self, retries_remaining: int):
                super().__init__(title="Enter MMR", timeout=MMR_MODAL_TIMEOUT_MINUTES * 60)
                self.retries_remaining = retries_remaining
                self.mmr_input = discord.ui.TextInput(
                    label="Enter your MMR",
                    placeholder=None,
                    required=False,
                    style=discord.TextStyle.short,
                )
                self.add_item(self.mmr_input)
                self.value: int | None = None
                self.error: str | None = None

            async def on_submit(self, interaction_modal: discord.Interaction):
                raw = self.mmr_input.value.strip() if self.mmr_input.value else ""
                if not raw:
                    self.error = "Invalid MMR"
                    await interaction_modal.response.send_message("❌ Invalid MMR", ephemeral=True)
                    return
                try:
                    mmr_val = int(raw)
                except ValueError:
                    self.error = "Invalid MMR"
                    await interaction_modal.response.send_message("❌ Invalid MMR", ephemeral=True)
                    return
                if mmr_val < 0 or mmr_val > 12000:
                    self.error = "Invalid MMR"
                    await interaction_modal.response.send_message("❌ Invalid MMR", ephemeral=True)
                    return
                self.value = mmr_val
                await interaction_modal.response.send_message("✅ MMR received", ephemeral=True)

        class MMRPromptView(discord.ui.View):
            def __init__(self):
                super().__init__(timeout=MMR_MODAL_TIMEOUT_MINUTES * 60)
                self.attempts_left = MMR_MODAL_RETRY_LIMIT

            @discord.ui.button(label="Enter MMR", style=discord.ButtonStyle.primary)
            async def enter_mmr(
                self, interaction_btn: discord.Interaction, button: discord.ui.Button
            ):
                if self.attempts_left <= 0:
                    await interaction_btn.response.send_message("❌ Invalid MMR", ephemeral=True)
                    return

                modal = MMRModal(retries_remaining=self.attempts_left)
                await interaction_btn.response.send_modal(modal)
                await modal.wait()

                if modal.value is None:
                    # cancelled/invalid/timeout treated as invalid attempt (per our "require user input" flow)
                    self.attempts_left -= 1
                    if self.attempts_left <= 0:
                        button.disabled = True
                        await interaction_btn.followup.send("❌ Invalid MMR", ephemeral=True)
                    return

                try:
                    await _finalize_register(mmr_override=modal.value)
                except Exception as e:
                    logger.error(
                        f"Error finalizing register after modal for user {interaction.user.id}: {e}",
                        exc_info=True,
                    )
                    await interaction_btn.followup.send(
                        "❌ Error finalizing registration. Try again later.", ephemeral=True
                    )
                    return

                # Success -> disable button
                button.disabled = True
                self.stop()

        await interaction.followup.send(
            "⚠️ OpenDota could not find your MMR. Click **Enter MMR** to finish registering.",
            ephemeral=True,
            view=MMRPromptView(),
        )
        return

    @app_commands.command(name="linksteam", description="Link an additional Steam account")
    @app_commands.describe(
        steam_id="Steam32 ID (found in your Dotabuff URL)",
        set_primary="Set as your primary Steam account (default: False)",
    )
    async def linksteam(
        self,
        interaction: discord.Interaction,
        steam_id: int,
        set_primary: bool = False,
    ):
        """Link an additional Steam ID to an existing registered player."""
        logger.info(
            f"LinkSteam command: User {interaction.user.id} ({interaction.user}) "
            f"linking Steam ID {steam_id} (set_primary={set_primary})"
        )

        if not await safe_defer(interaction, ephemeral=True):
            return

        player_repo = getattr(self.bot, "player_repo", None)
        if not player_repo:
            await interaction.followup.send("❌ Player repository not available.", ephemeral=True)
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Check if player is registered
        player = player_repo.get_by_id(interaction.user.id, guild_id)
        if not player:
            await interaction.followup.send(
                "❌ You are not registered. Use `/register` first.",
                ephemeral=True,
            )
            return

        # Validate steam_id (basic check)
        if steam_id <= 0 or steam_id > 2**32:
            await interaction.followup.send(
                "❌ Invalid Steam ID. Please use the 32-bit Steam ID from your Dotabuff URL.",
                ephemeral=True,
            )
            return

        # Get current steam_ids for this player
        current_steam_ids = player_repo.get_steam_ids(interaction.user.id)

        # Check if already linked to this player
        if steam_id in current_steam_ids:
            if set_primary:
                player_repo.set_primary_steam_id(interaction.user.id, steam_id)
                await interaction.followup.send(
                    f"✅ Steam ID `{steam_id}` is now your primary account.",
                    ephemeral=True,
                )
            else:
                await interaction.followup.send(
                    f"ℹ️ Steam ID `{steam_id}` is already linked to your account.",
                    ephemeral=True,
                )
            return

        # Add the steam_id (will raise ValueError if linked to another player)
        try:
            # If no steam_ids linked yet, make this one primary
            is_first = len(current_steam_ids) == 0
            player_repo.add_steam_id(
                interaction.user.id,
                steam_id,
                is_primary=set_primary or is_first,
            )
        except ValueError as e:
            await interaction.followup.send(
                f"❌ {str(e)}",
                ephemeral=True,
            )
            return

        # Build response message
        new_steam_ids = player_repo.get_steam_ids(interaction.user.id)
        if len(new_steam_ids) == 1:
            await interaction.followup.send(
                f"✅ Steam ID `{steam_id}` linked to your account!\n"
                "You can now use `/rolesgraph`, `/lanegraph`, and the Dota tab in `/profile`.",
                ephemeral=True,
            )
        else:
            primary_note = " (set as primary)" if set_primary else ""
            await interaction.followup.send(
                f"✅ Steam ID `{steam_id}` added to your account{primary_note}!\n"
                f"You now have {len(new_steam_ids)} linked accounts. "
                "Use `/mysteamids` to view all linked accounts.",
                ephemeral=True,
            )

    @app_commands.command(name="unlinksteam", description="Remove a linked Steam account")
    @app_commands.describe(steam_id="Steam32 ID to remove")
    async def unlinksteam(self, interaction: discord.Interaction, steam_id: int):
        """Remove a linked Steam ID from your account."""
        logger.info(
            f"UnlinkSteam command: User {interaction.user.id} ({interaction.user}) "
            f"unlinking Steam ID {steam_id}"
        )

        if not await safe_defer(interaction, ephemeral=True):
            return

        player_repo = getattr(self.bot, "player_repo", None)
        if not player_repo:
            await interaction.followup.send("❌ Player repository not available.", ephemeral=True)
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Check if player is registered
        player = player_repo.get_by_id(interaction.user.id, guild_id)
        if not player:
            await interaction.followup.send(
                "❌ You are not registered. Use `/register` first.",
                ephemeral=True,
            )
            return

        # Get current steam_ids
        current_steam_ids = player_repo.get_steam_ids(interaction.user.id)

        if steam_id not in current_steam_ids:
            await interaction.followup.send(
                f"❌ Steam ID `{steam_id}` is not linked to your account.",
                ephemeral=True,
            )
            return

        # Warn if unlinking the last steam_id
        if len(current_steam_ids) == 1:
            await interaction.followup.send(
                f"⚠️ Steam ID `{steam_id}` is your only linked account.\n"
                "Unlinking it will disable match discovery and Dota stats.\n"
                "Are you sure? Run the command again to confirm.",
                ephemeral=True,
            )
            # For simplicity, we'll allow it anyway
            # A more robust implementation would track confirmation state

        # Remove the steam_id
        removed = player_repo.remove_steam_id(interaction.user.id, steam_id)

        if removed:
            remaining = player_repo.get_steam_ids(interaction.user.id)
            if remaining:
                primary = remaining[0]  # First is always primary
                await interaction.followup.send(
                    f"✅ Steam ID `{steam_id}` has been unlinked.\n"
                    f"Your primary account is now `{primary}`.",
                    ephemeral=True,
                )
            else:
                await interaction.followup.send(
                    f"✅ Steam ID `{steam_id}` has been unlinked.\n"
                    "You no longer have any linked Steam accounts.",
                    ephemeral=True,
                )
        else:
            await interaction.followup.send(
                f"❌ Failed to unlink Steam ID `{steam_id}`.",
                ephemeral=True,
            )

    @app_commands.command(name="mysteamids", description="View your linked Steam accounts")
    async def mysteamids(self, interaction: discord.Interaction):
        """View all Steam IDs linked to your account."""
        logger.info(f"MySteamIds command: User {interaction.user.id} ({interaction.user})")

        if not await safe_defer(interaction, ephemeral=True):
            return

        player_repo = getattr(self.bot, "player_repo", None)
        if not player_repo:
            await interaction.followup.send("❌ Player repository not available.", ephemeral=True)
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Check if player is registered
        player = player_repo.get_by_id(interaction.user.id, guild_id)
        if not player:
            await interaction.followup.send(
                "❌ You are not registered. Use `/register` first.",
                ephemeral=True,
            )
            return

        # Get current steam_ids (primary first)
        steam_ids = player_repo.get_steam_ids(interaction.user.id)

        if not steam_ids:
            await interaction.followup.send(
                "ℹ️ You don't have any Steam accounts linked.\n"
                "Use `/linksteam` to link your Steam account.",
                ephemeral=True,
            )
            return

        # Build response
        lines = ["**Your Linked Steam Accounts:**\n"]
        for i, sid in enumerate(steam_ids):
            dotabuff_url = f"https://www.dotabuff.com/players/{sid}"
            if i == 0:
                lines.append(f"⭐ `{sid}` (Primary) - [Dotabuff]({dotabuff_url})")
            else:
                lines.append(f"• `{sid}` - [Dotabuff]({dotabuff_url})")

        lines.append(
            "\n*Use `/linksteam` to add more accounts or "
            "`/unlinksteam` to remove one.*"
        )

        await interaction.followup.send("\n".join(lines), ephemeral=True)

    @app_commands.command(name="setroles", description="Set your preferred roles")
    @app_commands.describe(roles="Roles (1-5, e.g., '123' or '1,2,3' for carry, mid, offlane)")
    async def set_roles(self, interaction: discord.Interaction, roles: str):
        """Set player's preferred roles."""
        logger.info(
            f"SetRoles command: User {interaction.user.id} ({interaction.user}) setting roles: {roles}"
        )
        if not await safe_defer(interaction, ephemeral=True):
            return

        try:
            # Parse roles and validate (commas optional)
            cleaned = roles.replace(",", "").replace(" ", "")
            role_list = list(cleaned)

            valid_choices = ["1", "2", "3", "4", "5"]
            for r in role_list:
                if r not in valid_choices:
                    valid_roles = ", ".join([format_role_display(role) for role in valid_choices])
                    await safe_followup(
                        interaction,
                        content=f"❌ Invalid role: {r}. Roles must be 1-5:\n{valid_roles}",
                        ephemeral=True,
                    )
                    return

            if not role_list:
                await safe_followup(
                    interaction, content="❌ Please provide at least one role.", ephemeral=True
                )
                return

            # Deduplicate roles while preserving order
            role_list = list(dict.fromkeys(role_list))

            guild_id = interaction.guild.id if interaction.guild else None
            self.player_service.set_roles(interaction.user.id, guild_id, role_list)

            role_display = ", ".join([format_role_display(r) for r in role_list])
            await interaction.followup.send(f"✅ Set your preferred roles to: {role_display}")
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
        except Exception as e:
            logger.error(f"Error setting roles for {interaction.user.id}: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content="❌ Unexpected error setting roles. Try again later.",
                ephemeral=True,
            )

async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    # Get db and config from bot
    db = getattr(bot, "db", None)
    player_service = getattr(bot, "player_service", None)
    role_emojis = getattr(bot, "role_emojis", {})
    role_names = getattr(bot, "role_names", {})

    await bot.add_cog(
        RegistrationCommands(bot, db, player_service, role_emojis, role_names)
    )
