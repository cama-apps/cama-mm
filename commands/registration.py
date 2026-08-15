"""
Registration commands for the bot: /player register, /player roles, etc.
"""

import asyncio
import functools
import logging

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_guild
from config import (
    LOBBY_CHANNEL_ID,
    LOWSKILL_LOBBY_CHANNEL_ID,
    MMR_MODAL_RETRY_LIMIT,
    MMR_MODAL_TIMEOUT_MINUTES,
)
from opendota_integration import run_opendota_io
from utils.curfew import parse_clock
from utils.formatting import escape_discord_text, format_role_display
from utils.interaction_safety import safe_defer, safe_followup
from utils.neon_helpers import get_neon_service
from utils.playtime import format_hour_ranges, parse_hour_set
from utils.suspension_format import (
    format_suspension_scope,
    format_suspension_terms,
)
from utils.timezone import DEFAULT_TIMEZONE, format_common_timezones

logger = logging.getLogger("cama_bot.commands.registration")


def _player_suspension_summary(state) -> str:
    return (
        f"**Lobby suspension** — {format_suspension_scope(state)}\n"
        f"Term: {format_suspension_terms(state)}\nReason: {state.reason}"
    )


class RegistrationCommands(commands.Cog):
    """Commands for player registration and profile management."""

    player = app_commands.Group(name="player", description="Player registration and profile management")
    player_lobby = app_commands.Group(
        name="lobby",
        description="Lobby notification preferences",
        parent=player,
    )
    player_curfew = app_commands.Group(
        name="curfew",
        description="Auto-lock and auto-unqueue from lobbies during your bedtime hours",
        parent=player,
    )
    player_timezone = app_commands.Group(
        name="timezone",
        description="Your timezone, used by curfew and other time-based features",
        parent=player,
    )
    player_playtime = app_commands.Group(
        name="playtime",
        description="Your preferred dota hours (informational) and the group's most popular times",
        parent=player,
    )

    def __init__(self, bot: commands.Bot, player_service):
        self.bot = bot
        self.player_service = player_service

    @staticmethod
    async def _update_lobby_alert_dm(interaction, dm_message, content: str) -> None:
        """Best-effort edit of the preflight DM so it reflects the final outcome."""
        try:
            await dm_message.edit(content=content)
        except Exception as exc:
            logger.debug(
                "Could not update lobby-alert DM for %s: %s",
                interaction.user.id,
                exc,
            )

    @app_commands.command(name="refer", description="Refer a player before their first game")
    @app_commands.checks.cooldown(1, 5.0)
    @require_guild
    async def refer(self, interaction: discord.Interaction, player: discord.Member):
        """Enroll a player before their first game and share the onboarding steps."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        if player.bot:
            await safe_followup(
                interaction,
                content="❌ You can only refer human players.",
                ephemeral=True,
            )
            return
        if player.id == interaction.user.id:
            await safe_followup(
                interaction,
                content="❌ You can't refer yourself.",
                ephemeral=True,
            )
            return

        try:
            await asyncio.to_thread(
                self.player_service.create_referral,
                interaction.user.id,
                player.id,
                interaction.guild.id,
            )
        except ValueError as exc:
            await safe_followup(interaction, content=f"❌ {exc}", ephemeral=True)
            return

        await safe_followup(
            interaction,
            content="✅ Referral created.",
            ephemeral=True,
        )
        lobby_mentions = []
        for channel_id in dict.fromkeys((LOBBY_CHANNEL_ID, LOWSKILL_LOBBY_CHANNEL_ID)):
            if channel_id is None:
                continue
            channel = interaction.guild.get_channel(channel_id)
            if channel is not None:
                lobby_mentions.append(channel.mention)
        lobby_destination = (
            " or ".join(lobby_mentions) if lobby_mentions else "a lobby channel"
        )

        await safe_followup(
            interaction,
            content=(
                f"**Referral: {player.mention}**\n"
                "1. If needed, run `/player register` with your Steam32 ID "
                "(the number in your Dotabuff URL).\n"
                "2. Run `/player roles` with your Dota positions (1-5).\n"
                f"3. React in {lobby_destination} to join an active lobby, "
                "or run `/lobby` to start one."
            ),
            allowed_mentions=discord.AllowedMentions(
                users=[player],
                roles=False,
                everyone=False,
                replied_user=False,
            ),
        )

    @player_lobby.command(
        name="autonotify",
        description="Manage public lobby alerts or watch a player's next signup",
    )
    @app_commands.describe(
        enabled="Enable or disable persistent lobby notifications",
        playername="DM you once when this player next joins a lobby",
    )
    @require_guild
    async def lobby_autonotify(
        self,
        interaction: discord.Interaction,
        enabled: bool | None = None,
        playername: discord.Member | None = None,
    ):
        """Manage public rally alerts or arm a one-shot player signup DM."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        reminder_service = getattr(self.bot, "reminder_service", None)
        if reminder_service is None:
            await safe_followup(
                interaction,
                content="❌ Lobby notification preferences are unavailable right now.",
                ephemeral=True,
            )
            return

        if playername is not None:
            if enabled is not None:
                await safe_followup(
                    interaction,
                    content=(
                        "❌ Choose either `playername` for a one-time DM or `enabled` "
                        "for public 8/9-player alerts, not both."
                    ),
                    ephemeral=True,
                )
                return
            if playername.id == interaction.user.id:
                await safe_followup(
                    interaction,
                    content="❌ You can't subscribe to your own lobby signup.",
                    ephemeral=True,
                )
                return
            if playername.bot:
                await safe_followup(
                    interaction,
                    content="❌ Bot accounts can't sign up for player lobbies.",
                    ephemeral=True,
                )
                return

            target_name = escape_discord_text(playername.display_name)

            # Detect an existing subscription before the DM preflight so a
            # re-run gets the duplicate notice without a pointless "check
            # passed" DM. Best-effort only: the persist step below still
            # reports duplicates (created=False), so a failed read check
            # falls through to the normal preflight path rather than
            # blocking the command.
            try:
                already_subscribed = await asyncio.to_thread(
                    reminder_service.has_lobby_player_subscription,
                    interaction.user.id,
                    playername.id,
                    interaction.guild.id,
                )
            except Exception as exc:
                logger.warning(
                    "Could not pre-check lobby alert duplicate for subscriber %s and target %s: %s",
                    interaction.user.id,
                    playername.id,
                    exc,
                )
                already_subscribed = False
            if already_subscribed:
                await safe_followup(
                    interaction,
                    content=(
                        f"🔔 You already have a one-time lobby alert set for **{target_name}**."
                    ),
                    ephemeral=True,
                )
                return

            try:
                # Discord has no guild permission bit that proves a user accepts
                # bot DMs. A real send is the only reliable preflight, and the
                # subscription is persisted only after it succeeds. The message
                # is edited below once the real outcome (created, duplicate, or
                # save failure) is known so the DM never overstates progress.
                dm_message = await interaction.user.send(
                    "✅ DM check passed for your one-time lobby alert request "
                    f"about **{target_name}**."
                )
            except discord.Forbidden:
                await safe_followup(
                    interaction,
                    content=(
                        "❌ I can't DM you. Enable direct messages from server members, "
                        "then try again."
                    ),
                    ephemeral=True,
                )
                return
            except discord.HTTPException as exc:
                logger.warning(
                    "Discord rejected the lobby target DM check for %s: %s",
                    interaction.user.id,
                    exc,
                )
                await safe_followup(
                    interaction,
                    content="❌ I couldn't verify that I can DM you. Try again in a moment.",
                    ephemeral=True,
                )
                return
            except Exception as exc:
                logger.warning(
                    "Could not validate DMs for lobby target subscriber %s: %s",
                    interaction.user.id,
                    exc,
                )
                await safe_followup(
                    interaction,
                    content="❌ I couldn't verify that I can DM you. Try again in a moment.",
                    ephemeral=True,
                )
                return

            try:
                created = await asyncio.to_thread(
                    reminder_service.add_lobby_player_subscription,
                    interaction.user.id,
                    playername.id,
                    interaction.guild.id,
                )
            except Exception as exc:
                logger.error(
                    "Error adding one-shot lobby alert for subscriber %s and target %s: %s",
                    interaction.user.id,
                    playername.id,
                    exc,
                    exc_info=True,
                )
                message = "❌ Your DM check passed, but I couldn't save the alert. Try again later."
                await self._update_lobby_alert_dm(interaction, dm_message, message)
                await safe_followup(interaction, content=message, ephemeral=True)
                return

            if created:
                message = (
                    f"🔔 One-time lobby alert set for **{target_name}**. "
                    "I'll DM you the next time they join a lobby in this server."
                )
            else:
                message = (
                    f"🔔 You already have a one-time lobby alert set for **{target_name}**."
                )
            await self._update_lobby_alert_dm(interaction, dm_message, message)
            await safe_followup(interaction, content=message, ephemeral=True)
            return

        enabled = True if enabled is None else enabled
        try:
            await asyncio.to_thread(
                reminder_service.set_preference,
                interaction.user.id,
                interaction.guild.id,
                "lobby",
                enabled,
            )
        except Exception as exc:
            logger.error(
                "Error setting lobby auto-notify for %s: %s",
                interaction.user.id,
                exc,
                exc_info=True,
            )
            await safe_followup(
                interaction,
                content="❌ Couldn't update your lobby notification preference. Try again later.",
                ephemeral=True,
            )
            return

        if enabled:
            message = (
                "✅ Lobby auto-notify is now **ON**. You'll be pinged with 📋 "
                "when a lobby is filling up.\n"
                "This stays enabled across lobbies; reacting 📋 still subscribes "
                "only to the current lobby."
            )
        else:
            message = "🔕 Lobby auto-notify is now **OFF**."
        await safe_followup(interaction, content=message, ephemeral=True)

    @player_lobby.command(
        name="status",
        description="Privately view your active lobby suspension",
    )
    @require_guild
    async def lobby_status(self, interaction: discord.Interaction):
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        moderation_service = getattr(self.bot, "moderation_service", None)
        suspension = await asyncio.to_thread(
            moderation_service.get_active_suspension,
            interaction.user.id,
            guild_id,
        ) if moderation_service is not None else None

        message = (
            _player_suspension_summary(suspension)
            if suspension is not None
            else "✅ All clear — you have no active lobby suspension."
        )
        await safe_followup(interaction, content=message, ephemeral=True)

    @player.command(name="register", description="Register yourself as a player")
    @app_commands.describe(steam_id="Steam32 ID (found in your Dotabuff URL)")
    @require_guild
    async def register(self, interaction: discord.Interaction, steam_id: int):
        """Register a new player."""
        logger.info(
            f"Register command: User {interaction.user.id} ({interaction.user}) registering with Steam ID {steam_id}"
        )

        # Defer response since OpenDota API call might take time
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id

        async def _finalize_register(mmr_override: int | None = None):
            result = await run_opendota_io(
                functools.partial(
                    self.player_service.register_player,
                    discord_id=interaction.user.id,
                    discord_username=str(interaction.user),
                    guild_id=guild_id,
                    steam_id=steam_id,
                    mmr_override=mmr_override,
                )
            )
            await interaction.followup.send(
                f"✅ Registered {interaction.user.mention}!\n"
                f"Cama Rating: {result['cama_rating']} ({result['uncertainty']:.0f}% uncertainty)\n"
                f"Use `/player roles` to set your preferred roles.\n"
                f"Use `/player region` to set your server (US East / US West)."
            )

            # Neon Degen Terminal hook (registration)
            try:
                neon = get_neon_service(self.bot)
                if neon:
                    neon_result = await neon.on_registration(
                        interaction.user.id, guild_id, str(interaction.user)
                    )
                    if neon_result and neon_result.text_block:
                        msg = await interaction.channel.send(neon_result.text_block)
                        async def _del_neon(m, d):
                            try:
                                await asyncio.sleep(d)
                                await m.delete()
                            except Exception as e:
                                logger.debug("Failed to delete neon message: %s", e)
                        asyncio.create_task(_del_neon(msg, 60))
            except Exception as e:
                logger.debug("Failed to send registration neon result: %s", e)

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
                if mmr_val <= 0 or mmr_val > 12000:
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

    @player.command(name="link", description="Link an additional Steam account")
    @app_commands.describe(
        steam_id="Steam32 ID (found in your Dotabuff URL)",
        set_primary="Set as your primary Steam account (default: False)",
    )
    @require_guild
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

        if not self.player_service:
            await interaction.followup.send("❌ Player service not available.", ephemeral=True)
            return

        guild_id = interaction.guild.id

        # Check if player is registered
        player = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await interaction.followup.send(
                "❌ You are not registered. Use `/player register` first.",
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
        current_steam_ids = await asyncio.to_thread(self.player_service.get_steam_ids, interaction.user.id)

        # Check if already linked to this player
        if steam_id in current_steam_ids:
            if set_primary:
                await asyncio.to_thread(self.player_service.set_primary_steam_id, interaction.user.id, steam_id)
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
            await asyncio.to_thread(
                functools.partial(
                    self.player_service.add_steam_id,
                    interaction.user.id,
                    steam_id,
                    is_primary=set_primary or is_first,
                )
            )
        except ValueError as e:
            await interaction.followup.send(
                f"❌ {str(e)}",
                ephemeral=True,
            )
            return

        # Build response message
        new_steam_ids = await asyncio.to_thread(self.player_service.get_steam_ids, interaction.user.id)
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
                "Use `/player steamids` to view all linked accounts.",
                ephemeral=True,
            )

    @player.command(name="unlink", description="Remove a linked Steam account")
    @app_commands.describe(steam_id="Steam32 ID to remove")
    @require_guild
    async def unlinksteam(self, interaction: discord.Interaction, steam_id: int):
        """Remove a linked Steam ID from your account."""
        logger.info(
            f"UnlinkSteam command: User {interaction.user.id} ({interaction.user}) "
            f"unlinking Steam ID {steam_id}"
        )

        if not await safe_defer(interaction, ephemeral=True):
            return

        if not self.player_service:
            await interaction.followup.send("❌ Player service not available.", ephemeral=True)
            return

        guild_id = interaction.guild.id

        # Check if player is registered
        player = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await interaction.followup.send(
                "❌ You are not registered. Use `/player register` first.",
                ephemeral=True,
            )
            return

        # Get current steam_ids
        current_steam_ids = await asyncio.to_thread(self.player_service.get_steam_ids, interaction.user.id)

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
        removed = await asyncio.to_thread(self.player_service.remove_steam_id, interaction.user.id, steam_id)

        if removed:
            remaining = await asyncio.to_thread(self.player_service.get_steam_ids, interaction.user.id)
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

    @player.command(name="steamids", description="View your linked Steam accounts")
    @require_guild
    async def mysteamids(self, interaction: discord.Interaction):
        """View all Steam IDs linked to your account."""
        logger.info(f"MySteamIds command: User {interaction.user.id} ({interaction.user})")

        if not await safe_defer(interaction, ephemeral=True):
            return

        if not self.player_service:
            await interaction.followup.send("❌ Player service not available.", ephemeral=True)
            return

        guild_id = interaction.guild.id

        # Check if player is registered
        player = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await interaction.followup.send(
                "❌ You are not registered. Use `/player register` first.",
                ephemeral=True,
            )
            return

        # Get current steam_ids (primary first)
        steam_ids = await asyncio.to_thread(self.player_service.get_steam_ids, interaction.user.id)

        if not steam_ids:
            await interaction.followup.send(
                "ℹ️ You don't have any Steam accounts linked.\n"
                "Use `/player link` to link your Steam account.",
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
            "\n*Use `/player link` to add more accounts or "
            "`/player unlink` to remove one.*"
        )

        await interaction.followup.send("\n".join(lines), ephemeral=True)

    @player.command(name="roles", description="Set your preferred roles")
    @app_commands.describe(roles="Roles (1-5, e.g., '123' or '1,2,3' for carry, mid, offlane)")
    @require_guild
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

            guild_id = interaction.guild.id
            await asyncio.to_thread(self.player_service.set_roles, interaction.user.id, guild_id, role_list)

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

    @player.command(name="region", description="Set your preferred Dota server (US East / US West)")
    @app_commands.describe(region="Your preferred server — leave blank to see your current setting")
    @app_commands.choices(
        region=[
            app_commands.Choice(name="US East", value="USE"),
            app_commands.Choice(name="US West", value="USW"),
        ]
    )
    @require_guild
    async def set_region(
        self, interaction: discord.Interaction, region: app_commands.Choice[str] | None = None
    ):
        """Set or view the player's preferred server region."""
        logger.info(
            f"SetRegion command: User {interaction.user.id} ({interaction.user}) "
            f"region={region.value if region else None}"
        )
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        try:
            if region is None:
                info = await asyncio.to_thread(
                    self.player_service.get_region_info, interaction.user.id, guild_id
                )
                if info["source"] == "set":
                    msg = f"Your server is set to **{info['name']}**. Pick again to change it."
                elif info["source"] == "inferred":
                    msg = (
                        f"Your server is **{info['name']}** (inferred from your Dota history). "
                        "Use `/player region` and pick one to lock it in."
                    )
                else:
                    msg = (
                        "You haven't set a server yet. "
                        "Use `/player region` and pick US East or US West."
                    )
                await interaction.followup.send(msg, ephemeral=True)
                return

            await asyncio.to_thread(
                self.player_service.set_region, interaction.user.id, guild_id, region.value
            )
            await interaction.followup.send(
                f"✅ Set your server to **{region.name}**.", ephemeral=True
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
        except Exception as e:
            logger.error(f"Error setting region for {interaction.user.id}: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content="❌ Unexpected error setting your server. Try again later.",
                ephemeral=True,
            )

    @player_curfew.command(
        name="set", description="Set your queue curfew: auto-lock/unqueue from bedtime until wake time"
    )
    @app_commands.describe(
        bedtime="Bedtime, 24-hour HH:MM (e.g. 22:00 for 10pm)",
        wake="Wake time, 24-hour HH:MM (e.g. 06:00 for 6am) — queueing unlocks then",
        timezone="IANA timezone name — leave blank to use your /player timezone setting",
    )
    @require_guild
    async def curfew_set(
        self,
        interaction: discord.Interaction,
        bedtime: str,
        wake: str,
        timezone: str | None = None,
    ):
        """Set or update the player's curfew window."""
        logger.info(
            f"CurfewSet command: User {interaction.user.id} ({interaction.user}) "
            f"bedtime={bedtime} wake={wake} timezone={timezone}"
        )
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        try:
            curfew_hour, curfew_minute = parse_clock(bedtime)
            wake_hour, wake_minute = parse_clock(wake)
            await asyncio.to_thread(
                self.player_service.set_curfew,
                interaction.user.id,
                guild_id,
                curfew_hour=curfew_hour,
                curfew_minute=curfew_minute,
                wake_hour=wake_hour,
                wake_minute=wake_minute,
                timezone=timezone,
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
            return
        except Exception as e:
            logger.error(f"Error setting curfew for {interaction.user.id}: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content="❌ Unexpected error setting your curfew. Try again later.",
                ephemeral=True,
            )
            return

        info = await asyncio.to_thread(
            self.player_service.get_curfew_info, interaction.user.id, guild_id
        )
        await interaction.followup.send(
            f"✅ Curfew set: **{info['window']}**. "
            "You'll be blocked from joining a lobby and auto-removed from any lobby you're "
            "already in once your bedtime hits.",
            ephemeral=True,
        )

    @player_curfew.command(name="off", description="Disable your queue curfew")
    @require_guild
    async def curfew_off(self, interaction: discord.Interaction):
        """Disable the player's curfew without discarding their configured hours."""
        logger.info(f"CurfewOff command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        try:
            await asyncio.to_thread(self.player_service.disable_curfew, interaction.user.id, guild_id)
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
            return
        await interaction.followup.send("✅ Curfew disabled. You can queue any time.", ephemeral=True)

    @player_curfew.command(name="status", description="View your queue curfew setting")
    @require_guild
    async def curfew_status(self, interaction: discord.Interaction):
        """Show the player's current curfew configuration."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        try:
            info = await asyncio.to_thread(
                self.player_service.get_curfew_info, interaction.user.id, guild_id
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
            return

        if info["window"] is None:
            msg = "You haven't set a curfew yet. Use `/player curfew set`."
        elif info["enabled"]:
            msg = f"🌙 Curfew is **on**: {info['window']}."
        else:
            msg = f"Curfew is **off** (last set to {info['window']}). Use `/player curfew set` to re-enable."
        await interaction.followup.send(msg, ephemeral=True)

    @player_timezone.command(
        name="set", description="Set your timezone, used by curfew and other time-based features"
    )
    @app_commands.describe(timezone=f"IANA timezone name, e.g. America/New_York (default {DEFAULT_TIMEZONE})")
    @require_guild
    async def timezone_set(self, interaction: discord.Interaction, timezone: str):
        """Set the player's general timezone preference."""
        logger.info(
            f"TimezoneSet command: User {interaction.user.id} ({interaction.user}) timezone={timezone}"
        )
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        try:
            await asyncio.to_thread(
                self.player_service.set_timezone, interaction.user.id, guild_id, timezone
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
            return
        except Exception as e:
            logger.error(f"Error setting timezone for {interaction.user.id}: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content="❌ Unexpected error setting your timezone. Try again later.",
                ephemeral=True,
            )
            return

        await interaction.followup.send(
            f"✅ Timezone set to **{timezone}**. Curfew will use this unless you gave it its own "
            "timezone with `/player curfew set`.",
            ephemeral=True,
        )

    @player_timezone.command(name="status", description="View your timezone setting")
    @require_guild
    async def timezone_status(self, interaction: discord.Interaction):
        """Show the player's current timezone setting."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        try:
            info = await asyncio.to_thread(
                self.player_service.get_timezone_info, interaction.user.id, guild_id
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
            return

        if info["timezone"] is None:
            msg = (
                f"You haven't set a timezone yet (defaults to {DEFAULT_TIMEZONE} where needed). "
                "Use `/player timezone set`."
            )
        else:
            msg = f"Your timezone is **{info['timezone']}**."
        await interaction.followup.send(msg, ephemeral=True)

    @player_timezone.command(
        name="list", description="See common timezone names to use with /player timezone set"
    )
    @require_guild
    async def timezone_list(self, interaction: discord.Interaction):
        """Show a curated menu of common IANA timezone names."""
        if not await safe_defer(interaction, ephemeral=True):
            return
        await interaction.followup.send(format_common_timezones(), ephemeral=True)

    @player_playtime.command(
        name="set",
        description="Set the hours you like to play dota — informational only, doesn't restrict queueing",
    )
    @app_commands.describe(
        hours="Hours you're usually free, 24-hour, your own timezone — e.g. '18-23' or '14,20,21'"
    )
    @require_guild
    async def playtime_set(self, interaction: discord.Interaction, hours: str):
        """Set the player's informational dota play-time hours."""
        logger.info(
            f"PlaytimeSet command: User {interaction.user.id} ({interaction.user}) hours={hours}"
        )
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        try:
            hour_set = parse_hour_set(hours)
            await asyncio.to_thread(
                self.player_service.set_dota_play_hours,
                interaction.user.id,
                guild_id,
                sorted(hour_set),
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
            return
        except Exception as e:
            logger.error(f"Error setting play-time hours for {interaction.user.id}: {e}", exc_info=True)
            await safe_followup(
                interaction,
                content="❌ Unexpected error setting your play-time hours. Try again later.",
                ephemeral=True,
            )
            return

        await interaction.followup.send(
            f"✅ Play-time hours set: **{format_hour_ranges(hour_set)}**. "
            "This is informational only — it doesn't affect queueing.",
            ephemeral=True,
        )

    @player_playtime.command(name="clear", description="Clear your dota play-time hours")
    @require_guild
    async def playtime_clear(self, interaction: discord.Interaction):
        """Clear the player's informational dota play-time hours."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        try:
            await asyncio.to_thread(
                self.player_service.clear_dota_play_hours, interaction.user.id, guild_id
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
            return
        await interaction.followup.send("✅ Play-time hours cleared.", ephemeral=True)

    @player_playtime.command(name="status", description="View your dota play-time hours")
    @require_guild
    async def playtime_status(self, interaction: discord.Interaction):
        """Show the player's own informational play-time hours."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        try:
            info = await asyncio.to_thread(
                self.player_service.get_dota_play_hours_info, interaction.user.id, guild_id
            )
        except ValueError as e:
            await safe_followup(interaction, content=f"❌ {str(e)}", ephemeral=True)
            return

        if not info["hours"]:
            msg = "You haven't set any play-time hours yet. Use `/player playtime set`."
        else:
            msg = f"Your play-time hours: **{format_hour_ranges(info['hours'])}**."
        await interaction.followup.send(msg, ephemeral=True)

    @player_playtime.command(
        name="popular", description="See the group's most popular hours to play dota"
    )
    @require_guild
    async def playtime_popular(self, interaction: discord.Interaction):
        """Show an hour-by-hour histogram of everyone's informational play-time hours."""
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild_id = interaction.guild.id
        counts = await asyncio.to_thread(self.player_service.get_popular_play_hours, guild_id)
        if not any(counts):
            await interaction.followup.send(
                "No one has set their play-time hours yet. Try `/player playtime set`."
            )
            return

        lines = [
            f"{hour:02d}:00 | {'█' * count}{' ' if count else ''}{count}"
            for hour, count in enumerate(counts)
        ]
        await interaction.followup.send(
            f"**🎮 Most Popular Dota Hours** (times in {DEFAULT_TIMEZONE})\n"
            f"```\n{chr(10).join(lines)}\n```"
        )

    @player.command(name="exclusion", description="Check your exclusion factor")
    @require_guild
    async def exclusion(self, interaction: discord.Interaction):
        """Show the player's current exclusion count."""
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id

        player = await asyncio.to_thread(
            self.player_service.get_player, interaction.user.id, guild_id
        )
        if not player:
            await interaction.followup.send(
                "You are not registered. Use `/player register` first.",
                ephemeral=True,
            )
            return

        count = await asyncio.to_thread(
            self.player_service.get_exclusion_count, interaction.user.id, guild_id
        )

        await interaction.followup.send(
            f"Your exclusion factor is **{count}**.\n"
            "Higher = more priority to play next game when there are extra players.",
            ephemeral=True,
        )


async def setup(bot: commands.Bot):
    """Setup function called when loading the cog."""
    player_service = getattr(bot, "player_service", None)

    await bot.add_cog(RegistrationCommands(bot, player_service))
