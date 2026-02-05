"""
Lobby commands: /lobby, /kick, /resetlobby, /rc, /stopreadycheck.

Uses Discord threads for lobby management similar to /prediction.
"""

import logging
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from config import LOBBY_CHANNEL_ID
from services.lobby_service import LobbyService
from services.permissions import has_admin_permission
from utils.formatting import FROGLING_EMOJI_ID, JOPACOIN_EMOJI_ID, get_player_display_name
from utils.interaction_safety import safe_defer
from utils.pin_helpers import safe_unpin_all_bot_messages

if TYPE_CHECKING:
    from services.player_service import PlayerService

logger = logging.getLogger("cama_bot.commands.lobby")


class LobbyCommands(commands.Cog):
    """Slash commands for lobby management."""

    def __init__(self, bot: commands.Bot, lobby_service: LobbyService, player_service):
        self.bot = bot
        self.lobby_service = lobby_service
        self.player_service = player_service

    async def _get_lobby_target_channel(
        self, interaction: discord.Interaction
    ) -> tuple[discord.abc.Messageable | None, bool]:
        """
        Get the target channel for lobby embeds.

        Returns:
            (channel, is_dedicated) tuple:
            - channel: The channel to post to (dedicated or interaction channel)
            - is_dedicated: True if posting to dedicated channel, False if fallback
        """
        # If no dedicated channel configured, use interaction channel
        if not LOBBY_CHANNEL_ID:
            return interaction.channel, False

        try:
            channel = self.bot.get_channel(LOBBY_CHANNEL_ID)
            if not channel:
                channel = await self.bot.fetch_channel(LOBBY_CHANNEL_ID)

            # Verify we can send messages to this channel
            if isinstance(channel, discord.TextChannel):
                # Ensure dedicated channel is in the same guild
                if interaction.guild and channel.guild.id != interaction.guild.id:
                    logger.warning(
                        f"Dedicated lobby channel {LOBBY_CHANNEL_ID} is in different guild"
                    )
                    return interaction.channel, False

                perms = channel.permissions_for(channel.guild.me)
                if not perms.send_messages or not perms.create_public_threads:
                    logger.warning(
                        f"Bot lacks permissions in dedicated lobby channel {LOBBY_CHANNEL_ID}"
                    )
                    return interaction.channel, False

            return channel, True
        except (discord.NotFound, discord.Forbidden) as exc:
            logger.warning(f"Cannot access dedicated lobby channel {LOBBY_CHANNEL_ID}: {exc}")
            return interaction.channel, False
        except Exception as exc:
            logger.warning(f"Error fetching dedicated lobby channel: {exc}")
            return interaction.channel, False

    async def _safe_pin(self, message: discord.Message) -> None:
        """Pin the lobby message, logging but not raising on failure (e.g., missing perms)."""
        try:
            await message.pin(reason="Cama lobby active")
        except discord.Forbidden:
            logger.warning("Cannot pin lobby message: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to pin lobby message: {exc}")


    async def _remove_user_lobby_reactions(self, user: discord.User | discord.Member) -> None:
        """Remove a user's lobby reactions (sword and frogling) from the channel lobby message."""
        message_id = self.lobby_service.get_lobby_message_id()
        channel_id = self.lobby_service.get_lobby_channel_id()
        if not message_id or not channel_id:
            return

        try:
            channel = self.bot.get_channel(channel_id)
            if not channel:
                channel = await self.bot.fetch_channel(channel_id)
            message = await channel.fetch_message(message_id)
            # Remove sword reaction
            try:
                await message.remove_reaction("⚔️", user)
            except Exception:
                pass
            # Remove frogling reaction
            try:
                frogling_emoji = discord.PartialEmoji(name="frogling", id=FROGLING_EMOJI_ID)
                await message.remove_reaction(frogling_emoji, user)
            except Exception:
                pass
        except discord.Forbidden:
            logger.warning("Cannot remove reaction: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to remove user lobby reactions: {exc}")

    async def _update_lobby_message(self, interaction: discord.Interaction, lobby) -> None:
        message_id = self.lobby_service.get_lobby_message_id()
        if not message_id:
            return
        try:
            channel = interaction.channel
            message = await channel.fetch_message(message_id)
            guild_id = interaction.guild.id if interaction.guild else None
            embed = self.lobby_service.build_lobby_embed(lobby, guild_id)
            if embed:
                await message.edit(embed=embed, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to update lobby message: {exc}")

    async def _sync_lobby_displays(self, lobby, guild_id: int | None = None) -> None:
        """Update channel message embed (which is also the thread starter)."""
        embed = self.lobby_service.build_lobby_embed(lobby, guild_id)

        # Update channel message - this also updates the thread starter view
        message_id = self.lobby_service.get_lobby_message_id()
        channel_id = self.lobby_service.get_lobby_channel_id()
        if message_id and channel_id:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                message = await channel.fetch_message(message_id)
                await message.edit(content=None, embed=embed)
                logger.info(f"Updated lobby embed: {lobby.get_player_count()} players")
            except Exception as exc:
                logger.warning(f"Failed to update channel message: {exc}")

    async def _update_thread_embed(self, lobby, embed=None, guild_id: int | None = None) -> None:
        """Update the pinned embed in the lobby thread."""
        thread_id = self.lobby_service.get_lobby_thread_id()
        embed_message_id = self.lobby_service.get_lobby_embed_message_id()

        if not thread_id or not embed_message_id:
            return

        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            message = await thread.fetch_message(embed_message_id)
            if not embed:
                embed = self.lobby_service.build_lobby_embed(lobby, guild_id)
            if embed:
                await message.edit(embed=embed)
        except Exception as exc:
            logger.warning(f"Failed to update thread embed: {exc}")

    async def _post_join_activity(self, thread_id: int, user: discord.User) -> None:
        """Post a join message in thread and mention user to subscribe them."""
        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            # Mention user to subscribe them to the thread
            await thread.send(f"✅ {user.mention} joined the lobby!")
        except Exception as exc:
            logger.warning(f"Failed to post join activity: {exc}")

    async def _post_leave_activity(self, thread_id: int, user: discord.User) -> None:
        """Post a leave message in thread."""
        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            await thread.send(f"🚪 **{user.display_name}** left the lobby.")
        except Exception as exc:
            logger.warning(f"Failed to post leave activity: {exc}")

    async def _update_channel_message_closed(self, reason: str = "Lobby Closed") -> None:
        """Update the channel message embed to show lobby is closed."""
        message_id = self.lobby_service.get_lobby_message_id()
        channel_id = self.lobby_service.get_lobby_channel_id()
        if not message_id or not channel_id:
            return

        try:
            channel = self.bot.get_channel(channel_id)
            if not channel:
                channel = await self.bot.fetch_channel(channel_id)
            message = await channel.fetch_message(message_id)

            # Create a closed embed
            embed = discord.Embed(
                title=f"🚫 {reason}",
                description="This lobby has been closed.",
                color=discord.Color.dark_grey(),
            )
            await message.edit(embed=embed, view=None)
        except Exception as exc:
            logger.warning(f"Failed to update channel message as closed: {exc}")

    async def _archive_lobby_thread(self, reason: str = "Lobby Reset") -> None:
        """Lock and archive the lobby thread with a status message."""
        thread_id = self.lobby_service.get_lobby_thread_id()
        if not thread_id:
            return

        try:
            thread = self.bot.get_channel(thread_id)
            if not thread:
                thread = await self.bot.fetch_channel(thread_id)

            # Skip if already archived
            if getattr(thread, "archived", False):
                return

            try:
                await thread.send(f"🚫 **{reason}**")
            except Exception:
                pass  # Thread might be archived already

            try:
                await thread.edit(name=f"🚫 {reason}", locked=True, archived=True)
            except discord.Forbidden:
                try:
                    await thread.edit(archived=True)
                except Exception:
                    pass
        except Exception as exc:
            logger.warning(f"Failed to archive lobby thread: {exc}")

    async def _auto_join_lobby(
        self, interaction: discord.Interaction, lobby
    ) -> tuple[bool, str | None]:
        """
        Auto-join user to lobby if not already in it.

        Returns:
            (joined, message) tuple:
            - joined: True if user was joined, False if already in or couldn't join
            - message: Warning message if roles not set, None otherwise
        """
        user_id = interaction.user.id
        guild_id = interaction.guild.id if interaction.guild else None

        # Already in lobby (regular or conditional)
        if user_id in lobby.players or user_id in lobby.conditional_players:
            return False, None

        # Check if player has roles set
        player = self.player_service.get_player(user_id, guild_id)
        if not player or not player.preferred_roles:
            return False, "⚠️ Set your preferred roles with `/setroles` to auto-join."

        # Check for pending match
        match_service = getattr(self.bot, "match_service", None)
        if match_service:
            pending_match = match_service.get_last_shuffle(guild_id)
            if pending_match:
                return False, None  # Don't show warning, the main command handles this

        # Attempt to join
        success, reason = self.lobby_service.join_lobby(user_id)
        if not success:
            logger.info(f"Auto-join failed for {user_id}: {reason}")
            return False, None

        # Refresh lobby state
        lobby = self.lobby_service.get_lobby()

        # Update displays
        await self._sync_lobby_displays(lobby, guild_id)

        # Post join activity in thread
        thread_id = self.lobby_service.get_lobby_thread_id()
        if thread_id:
            await self._post_join_activity(thread_id, interaction.user)

        # Rally/ready notifications
        from bot import notify_lobby_rally, notify_lobby_ready

        channel_id = self.lobby_service.get_lobby_channel_id()
        if channel_id and thread_id:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                thread = self.bot.get_channel(thread_id)
                if not thread:
                    thread = await self.bot.fetch_channel(thread_id)

                if not self.lobby_service.is_ready(lobby):
                    await notify_lobby_rally(channel, thread, lobby, guild_id or 0)
                else:
                    await notify_lobby_ready(channel, lobby)
            except Exception as exc:
                logger.warning(f"Failed to send rally/ready notification on auto-join: {exc}")

        return True, None

    @app_commands.command(name="lobby", description="Create or view the matchmaking lobby")
    async def lobby(self, interaction: discord.Interaction):
        logger.info(f"Lobby command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        player = self.player_service.get_player(interaction.user.id, guild_id)
        if not player:
            await interaction.followup.send(
                "❌ You're not registered! Use `/register` first.", ephemeral=True
            )
            return

        # Block if a match is pending recording
        match_service = getattr(self.bot, "match_service", None)
        if match_service:
            pending_match = match_service.get_last_shuffle(guild_id)
            if pending_match:
                jump_url = pending_match.get("shuffle_message_jump_url")
                message_text = "❌ There's a pending match that needs to be recorded!"
                if jump_url:
                    message_text += f" [View pending match]({jump_url}) then use `/record` first."
                else:
                    message_text += " Use `/record` first."
                await interaction.followup.send(message_text, ephemeral=True)
                return

        # Acquire lock to prevent race condition when multiple users call /lobby simultaneously
        async with self.lobby_service.creation_lock:
            lobby = self.lobby_service.get_or_create_lobby(creator_id=interaction.user.id)
            embed = self.lobby_service.build_lobby_embed(lobby, guild_id)

            # If message/thread already exists, refresh it; otherwise create new
            message_id = self.lobby_service.get_lobby_message_id()
            thread_id = self.lobby_service.get_lobby_thread_id()

            if message_id and thread_id:
                try:
                    # Fetch message from the dedicated/lobby channel (not necessarily interaction channel)
                    lobby_channel_id = self.lobby_service.get_lobby_channel_id()
                    if lobby_channel_id:
                        channel = self.bot.get_channel(lobby_channel_id)
                        if not channel:
                            channel = await self.bot.fetch_channel(lobby_channel_id)
                        message = await channel.fetch_message(message_id)
                    else:
                        message = await interaction.channel.fetch_message(message_id)

                    # Auto-join the user if not already in lobby
                    joined, warning = await self._auto_join_lobby(interaction, lobby)

                    # Refresh embed after potential join
                    await self._update_thread_embed(self.lobby_service.get_lobby(), guild_id=guild_id)

                    # Build response based on join result
                    if joined:
                        response = f"✅ Joined! [View Lobby]({message.jump_url})"
                    elif warning:
                        response = f"{warning} [View Lobby]({message.jump_url})"
                    else:
                        response = f"[View Lobby]({message.jump_url})"

                    await interaction.followup.send(response, ephemeral=True)
                    return
                except Exception:
                    # Fall through to create a new one
                    pass

            # Get target channel (dedicated or fallback to interaction channel)
            target_channel, is_dedicated = await self._get_lobby_target_channel(interaction)
            if not target_channel:
                await interaction.followup.send(
                    "❌ Could not find a valid channel to post the lobby.", ephemeral=True
                )
                return

            # Store the origin channel (where /lobby was run) for rally notifications
            origin_channel_id = interaction.channel.id

            # Send channel message with embed
            channel_msg = await target_channel.send(embed=embed)

            # Pin the lobby message for visibility
            await self._safe_pin(channel_msg)

            # Add reaction emojis for joining (sword for regular, frogling for conditional, jopacoin for gamba notifications)
            try:
                await channel_msg.add_reaction("⚔️")
                # Add frogling emoji using PartialEmoji with ID
                frogling_emoji = discord.PartialEmoji(name="frogling", id=FROGLING_EMOJI_ID)
                await channel_msg.add_reaction(frogling_emoji)
                # Add jopacoin emoji for subscribing to gamba notifications
                jopacoin_emoji = discord.PartialEmoji(name="jopacoin", id=JOPACOIN_EMOJI_ID)
                await channel_msg.add_reaction(jopacoin_emoji)
            except Exception:
                pass

            # Create thread from message (static name to avoid rate limits)
            try:
                thread_name = "🎮 Matchmaking Lobby"
                thread = await channel_msg.create_thread(name=thread_name)

                # Store all IDs (embed is on channel_msg, which is also the thread starter)
                # Also store origin_channel_id for rally notifications
                self.lobby_service.set_lobby_message_id(
                    message_id=channel_msg.id,
                    channel_id=target_channel.id,  # Where the embed lives (dedicated or interaction)
                    thread_id=thread.id,
                    embed_message_id=channel_msg.id,  # The channel msg IS the embed in thread
                    origin_channel_id=origin_channel_id,  # Where /lobby was run (for rally)
                )

                # Auto-join the user who created the lobby
                joined, warning = await self._auto_join_lobby(interaction, lobby)

                # Build response based on join result
                if joined:
                    response = f"✅ Lobby created and joined! [View Lobby]({channel_msg.jump_url})"
                elif warning:
                    response = f"✅ Lobby created! {warning} [View Lobby]({channel_msg.jump_url})"
                else:
                    response = f"✅ Lobby created! [View Lobby]({channel_msg.jump_url})"

                await interaction.followup.send(response, ephemeral=True)
                return

            except discord.Forbidden:
                # Thread permissions required
                logger.warning("Cannot create lobby thread: missing Create Public Threads permission.")
                await channel_msg.delete()
                await interaction.followup.send(
                    "❌ Bot needs 'Create Public Threads' permission to create lobbies.",
                    ephemeral=True,
                )
            except Exception as exc:
                logger.exception(f"Error creating lobby thread: {exc}")
                await channel_msg.delete()
                await interaction.followup.send(
                    "❌ Failed to create lobby thread. Please try again or contact an admin.",
                    ephemeral=True,
                )

    @app_commands.command(
        name="kick",
        description="Kick a player from the lobby (Admin or lobby creator only)",
    )
    @app_commands.describe(player="The player to kick from the lobby")
    async def kick(self, interaction: discord.Interaction, player: discord.Member):
        logger.info(f"Kick command: User {interaction.user.id} kicking {player.id}")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        lobby = self.lobby_service.get_lobby()
        if not lobby:
            await interaction.followup.send("⚠️ No active lobby.", ephemeral=True)
            return

        is_admin = has_admin_permission(interaction)
        is_creator = lobby.created_by == interaction.user.id
        if not (is_admin or is_creator):
            await interaction.followup.send(
                "❌ Permission denied. Admin or lobby creator only.",
                ephemeral=True,
            )
            return

        if player.id == interaction.user.id:
            await interaction.followup.send(
                "❌ You can't kick yourself. Use the Leave button in the lobby thread.",
                ephemeral=True,
            )
            return

        # Check if player is in regular or conditional set
        in_regular = player.id in lobby.players
        in_conditional = player.id in lobby.conditional_players

        if not in_regular and not in_conditional:
            await interaction.followup.send(
                f"⚠️ {player.mention} is not in the lobby.", ephemeral=True
            )
            return

        # Remove from whichever set they're in
        if in_regular:
            removed = self.lobby_service.leave_lobby(player.id)
        else:
            removed = self.lobby_service.leave_lobby_conditional(player.id)
        if removed:
            await interaction.followup.send(
                f"✅ Kicked {player.mention} from the lobby.", ephemeral=True
            )

            # Update both channel message and thread embed
            await self._sync_lobby_displays(lobby, guild_id)

            # Remove kicked player's lobby reactions (sword and frogling)
            await self._remove_user_lobby_reactions(player)

            # Post kick activity in thread
            thread_id = self.lobby_service.get_lobby_thread_id()
            if thread_id:
                try:
                    thread = self.bot.get_channel(thread_id)
                    if not thread:
                        thread = await self.bot.fetch_channel(thread_id)
                    await thread.send(
                        f"👢 **{player.display_name}** was kicked by {interaction.user.display_name}."
                    )
                except Exception as exc:
                    logger.warning(f"Failed to post kick activity: {exc}")

            # DM the kicked player
            try:
                await player.send(
                    f"You were kicked from the matchmaking lobby by {interaction.user.mention}."
                )
            except Exception:
                pass
        else:
            await interaction.followup.send(f"❌ Failed to kick {player.mention}.", ephemeral=True)

    @app_commands.command(name="join", description="Join the matchmaking lobby")
    async def join(self, interaction: discord.Interaction):
        """Join the matchmaking lobby from any channel."""
        logger.info(f"Join command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Check registration
        player = self.player_service.get_player(interaction.user.id, guild_id)
        if not player:
            await interaction.followup.send(
                "❌ You're not registered! Use `/register` first.", ephemeral=True
            )
            return

        # Check roles set
        if not player.preferred_roles:
            await interaction.followup.send(
                "❌ Set your preferred roles first! Use `/setroles`.", ephemeral=True
            )
            return

        # Check lobby exists
        lobby = self.lobby_service.get_lobby()
        if not lobby:
            await interaction.followup.send(
                "⚠️ No active lobby. Use `/lobby` to create one.", ephemeral=True
            )
            return

        # Block if a match is pending recording
        match_service = getattr(self.bot, "match_service", None)
        if match_service:
            pending_match = match_service.get_last_shuffle(guild_id)
            if pending_match:
                jump_url = pending_match.get("shuffle_message_jump_url")
                message_text = "❌ There's a pending match that needs to be recorded!"
                if jump_url:
                    message_text += f" [View pending match]({jump_url}) then use `/record` first."
                else:
                    message_text += " Use `/record` first."
                await interaction.followup.send(message_text, ephemeral=True)
                return

        # Attempt to join
        success, reason = self.lobby_service.join_lobby(interaction.user.id)
        if not success:
            await interaction.followup.send(f"❌ {reason}", ephemeral=True)
            return

        # Refresh lobby state after join
        lobby = self.lobby_service.get_lobby()

        # Update displays and post activity
        await self._sync_lobby_displays(lobby, guild_id)
        thread_id = self.lobby_service.get_lobby_thread_id()
        if thread_id:
            await self._post_join_activity(thread_id, interaction.user)

        # Rally/ready notifications
        from bot import notify_lobby_rally, notify_lobby_ready

        channel_id = self.lobby_service.get_lobby_channel_id()
        if channel_id and thread_id:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                thread = self.bot.get_channel(thread_id)
                if not thread:
                    thread = await self.bot.fetch_channel(thread_id)

                if not self.lobby_service.is_ready(lobby):
                    await notify_lobby_rally(channel, thread, lobby, guild_id or 0)
                else:
                    await notify_lobby_ready(channel, lobby)
            except Exception as exc:
                logger.warning(f"Failed to send rally/ready notification: {exc}")

        await interaction.followup.send("✅ Joined the lobby!", ephemeral=True)

    @app_commands.command(name="leave", description="Leave the matchmaking lobby")
    async def leave(self, interaction: discord.Interaction):
        """Leave the matchmaking lobby from any channel."""
        logger.info(f"Leave command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None
        lobby = self.lobby_service.get_lobby()
        if not lobby:
            await interaction.followup.send("⚠️ No active lobby.", ephemeral=True)
            return

        in_regular = interaction.user.id in lobby.players
        in_conditional = interaction.user.id in lobby.conditional_players

        if not in_regular and not in_conditional:
            await interaction.followup.send("⚠️ You're not in the lobby.", ephemeral=True)
            return

        # Remove from appropriate queue
        if in_regular:
            self.lobby_service.leave_lobby(interaction.user.id)
        else:
            self.lobby_service.leave_lobby_conditional(interaction.user.id)

        # Update displays
        await self._sync_lobby_displays(lobby, guild_id)

        # Remove user's reactions
        await self._remove_user_lobby_reactions(interaction.user)

        # Post leave activity in thread
        thread_id = self.lobby_service.get_lobby_thread_id()
        if thread_id:
            await self._post_leave_activity(thread_id, interaction.user)

        await interaction.followup.send("✅ Left the lobby.", ephemeral=True)

    @app_commands.command(
        name="resetlobby",
        description="Reset the current lobby (Admin or lobby creator only)",
    )
    async def resetlobby(self, interaction: discord.Interaction):
        """Allow admins or lobby creators to reset/abort an unfilled lobby."""
        logger.info(f"Reset lobby command: User {interaction.user.id} ({interaction.user})")
        can_respond = await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id if interaction.guild else None
        match_service = getattr(self.bot, "match_service", None)
        if match_service:
            pending_match = match_service.get_last_shuffle(guild_id)
            if pending_match:
                if can_respond:
                    jump_url = pending_match.get("shuffle_message_jump_url")
                    message_text = "❌ There's a pending match that needs to be recorded!"
                    if jump_url:
                        message_text += (
                            f" [View pending match]({jump_url}) then use `/record` first."
                        )
                    else:
                        message_text += " Use `/record` first."
                    await interaction.followup.send(message_text, ephemeral=True)
                return

        lobby = self.lobby_service.get_lobby()
        if not lobby:
            if can_respond:
                await interaction.followup.send("⚠️ No active lobby.", ephemeral=True)
            return

        is_admin = has_admin_permission(interaction)
        is_creator = lobby.created_by == interaction.user.id
        if not (is_admin or is_creator):
            if can_respond:
                await interaction.followup.send(
                    "❌ Permission denied. Admin or lobby creator only.",
                    ephemeral=True,
                )
            return

        # Block if there's an active draft
        draft_state_manager = getattr(self.bot, "draft_state_manager", None)
        if draft_state_manager and draft_state_manager.has_active_draft(guild_id):
            if can_respond:
                await interaction.followup.send(
                    "❌ There's an active draft in progress. "
                    "Use `/restartdraft` first to clear the draft.",
                    ephemeral=True,
                )
            return

        # Update channel message to show closed and archive thread
        await self._update_channel_message_closed("Lobby Reset")
        await self._archive_lobby_thread("Lobby Reset")

        # Unpin from the lobby channel (may be dedicated channel, not interaction channel)
        lobby_channel_id = self.lobby_service.get_lobby_channel_id()
        lobby_channel = None
        if lobby_channel_id:
            try:
                lobby_channel = self.bot.get_channel(lobby_channel_id)
                if not lobby_channel:
                    lobby_channel = await self.bot.fetch_channel(lobby_channel_id)
            except Exception:
                lobby_channel = interaction.channel
        else:
            lobby_channel = interaction.channel
        await safe_unpin_all_bot_messages(lobby_channel, self.bot.user)
        self.lobby_service.reset_lobby()

        # Clear lobby rally cooldowns
        from bot import clear_lobby_rally_cooldowns
        clear_lobby_rally_cooldowns(guild_id or 0)

        logger.info(f"Lobby reset by user {interaction.user.id}")
        if can_respond:
            await interaction.followup.send(
                "✅ Lobby reset. You can create a new lobby with `/lobby`.",
                ephemeral=True,
            )

    @app_commands.command(
        name="rc",
        description="Ping players to check readiness with Discord status and voice check",
    )
    async def ready_check(self, interaction: discord.Interaction):
        """
        Ping all lobby players once in DMs and lobby thread with Ready button.

        Checks Discord status (online/DND) and voice channel presence at invocation.
        Players click Ready button in either DM or thread to confirm.
        """
        logger.info(f"/rc command invoked by user {interaction.user.id}")

        if not await safe_defer(interaction, ephemeral=True):
            return

        # Check if lobby exists
        lobby = self.lobby_service.get_lobby()
        if not lobby:
            await interaction.followup.send(
                "❌ No active lobby. Use `/lobby` to create one!", ephemeral=True
            )
            return

        if lobby.get_player_count() == 0:
            await interaction.followup.send(
                "❌ Lobby is empty. No players to check!", ephemeral=True
            )
            return

        # Get lobby info
        player_ids, players = self.lobby_service.get_lobby_players(lobby)
        guild = interaction.guild
        guild_id = guild.id if guild else None

        # Check for admin presence and select designated player
        admin_in_lobby = any(has_admin_permission(pid, guild_id) for pid in player_ids)
        designated_player_id = None
        designated_display = "Admins have control"

        if not admin_in_lobby and players:
            # Find player with most total games
            designated_player = max(players, key=lambda p: p.get_total_games())
            designated_player_id = designated_player.discord_id

            # Set in lobby state
            self.lobby_service.lobby_manager.set_designated_player(designated_player_id)

            # Announce in message
            designated_member = guild.get_member(designated_player_id) if guild else None
            designated_display = (
                designated_member.mention
                if designated_member
                else f"<@{designated_player_id}>"
            )

        # Start ready check state tracking
        ready_check_service = getattr(self.bot, "ready_check_service", None)
        if ready_check_service:
            ready_check_service.start_check(
                guild_id or 0, player_ids, designated_player_id, admin_in_lobby
            )

        # Get lobby thread
        thread_id = self.lobby_service.get_lobby_thread_id()
        lobby_thread = None

        if thread_id and guild:
            try:
                lobby_thread = await self.bot.fetch_channel(thread_id)
            except Exception as exc:
                logger.warning(f"Could not fetch lobby thread {thread_id}: {exc}")

        if not lobby_thread:
            await interaction.followup.send(
                "❌ Could not find lobby thread. Make sure the lobby is created properly.",
                ephemeral=True
            )
            return

        # Check Discord status and voice for each player
        online_players = []
        voice_players = []

        for pid in player_ids:
            member = guild.get_member(pid) if guild else None
            if member:
                # Check Discord status
                if member.status in [discord.Status.online, discord.Status.dnd]:
                    online_players.append(pid)

                # Check voice channel
                if member.voice and not (member.voice.self_deaf or member.voice.deaf):
                    voice_players.append(pid)

        # Send DM to each player with Ready button
        from utils.ready_check_view import ReadyCheckView
        view = ReadyCheckView(self)

        dm_sent_count = 0
        for pid in player_ids:
            try:
                user = await self.bot.fetch_user(pid)
                await user.send(
                    f"🎮 **Ready Check** for lobby in {guild.name if guild else 'the server'}!\n"
                    f"Click the button below to confirm you're ready.",
                    view=view
                )
                dm_sent_count += 1
            except Exception as exc:
                logger.debug(f"Could not send DM to {pid}: {exc}")

        # Send message in lobby thread with Ready button
        thread_msg = await lobby_thread.send(
            f"🎮 **Ready Check!**\n"
            f"**Lobby Control:** {designated_display}\n\n"
            f"All players: Click the **Ready** button below to confirm!\n"
            f"({dm_sent_count}/{len(player_ids)} DMs sent)",
            view=view
        )

        # Build initial status embed
        ready_players = ready_check_service.get_state(guild_id or 0).ready_players if ready_check_service else set()

        embed = self._build_ready_check_status_embed(
            player_ids=player_ids,
            players=players,
            ready_players=ready_players,
            online_players=online_players,
            voice_players=voice_players,
            guild=guild,
            designated_display=designated_display,
        )

        status_msg = await lobby_thread.send(embed=embed)

        # Store status message for updates when players click Ready
        if ready_check_service:
            ready_check_service.set_status_message(
                guild_id or 0,
                status_msg.id,
                lobby_thread.id,
                online_players,
                voice_players,
            )

        await interaction.followup.send(
            f"✅ Ready check sent! Pinged {dm_sent_count} players in DMs + lobby thread.",
            ephemeral=True
        )

        logger.info(f"Ready check sent for guild {guild_id}")

    @app_commands.command(
        name="readycheck",
        description="Ping players to check readiness with Discord status and voice check",
    )
    async def readycheck_alias(self, interaction: discord.Interaction):
        """Alias for /rc command."""
        await self.ready_check(interaction)

    @app_commands.command(
        name="stopreadycheck",
        description="Stop ready check and remove players who didn't ready up",
    )
    async def stop_ready_check(self, interaction: discord.Interaction):
        """
        Stop the active ready check and kick players who didn't ready up.

        Only keeps players who clicked the Ready button in the lobby.
        """
        logger.info(f"/stopreadycheck command invoked by user {interaction.user.id}")

        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else 0

        # Get ready check service
        ready_check_service = getattr(self.bot, "ready_check_service", None)
        if not ready_check_service:
            await interaction.followup.send(
                "❌ Ready check service not available.", ephemeral=True
            )
            return

        # Get active ready check state
        state = ready_check_service.get_state(guild_id)
        if not state:
            await interaction.followup.send(
                "❌ No active ready check to stop.", ephemeral=True
            )
            return

        # Get lobby
        lobby = self.lobby_service.get_lobby()
        if not lobby:
            await interaction.followup.send(
                "❌ No active lobby found.", ephemeral=True
            )
            return

        # Get lists of ready and not ready players
        ready_players = list(state.ready_players)
        not_ready_players = [pid for pid in state.total_players if pid not in state.ready_players]

        # Remove not ready players from lobby
        removed_count = 0
        for pid in not_ready_players:
            if lobby.remove_player(pid):
                removed_count += 1
            elif lobby.remove_conditional_player(pid):
                removed_count += 1

        # Persist lobby changes
        if removed_count > 0:
            self.lobby_service.lobby_manager._persist_lobby()

        # Cancel ready check
        ready_check_service.cancel_check(guild_id)

        # Clear designated player
        self.lobby_service.lobby_manager.set_designated_player(None)

        # Build result message
        result_msg = (
            f"✅ **Ready check stopped!**\n\n"
            f"**Kept in lobby:** {len(ready_players)} player(s) who readied up\n"
            f"**Removed:** {removed_count} player(s) who didn't ready up"
        )

        if ready_players:
            guild = interaction.guild
            player_mentions = []
            for pid in ready_players[:25]:  # Limit to 25 for Discord
                player = self.player_service.get_player(pid, guild_id)
                if player:
                    display = get_player_display_name(player, pid, guild)
                    player_mentions.append(display)
                else:
                    player_mentions.append(f"<@{pid}>")

            result_msg += f"\n\n**Ready players:**\n" + "\n".join(f"• {name}" for name in player_mentions)

        await interaction.followup.send(result_msg, ephemeral=False)

        # Update lobby embed if it exists
        thread_id = self.lobby_service.get_lobby_thread_id()
        if thread_id:
            try:
                lobby_thread = await self.bot.fetch_channel(thread_id)
                embed_message_id = self.lobby_service.get_lobby_embed_message_id()
                if embed_message_id:
                    embed_message = await lobby_thread.fetch_message(embed_message_id)
                    # Rebuild lobby embed
                    await self._update_lobby_embed_message(embed_message)
            except Exception as exc:
                logger.warning(f"Failed to update lobby embed: {exc}")

        logger.info(
            f"Ready check stopped for guild {guild_id}: "
            f"kept {len(ready_players)}, removed {removed_count}"
        )

    def _build_ready_check_status_embed(
        self,
        player_ids: list[int],
        players: list,
        ready_players: set[int],
        online_players: list[int],
        voice_players: list[int],
        guild: discord.Guild | None,
        designated_display: str,
    ) -> discord.Embed:
        """Build status embed showing ready confirmations and Discord/voice status."""
        ready_count = len(ready_players)
        total = len(player_ids)

        # Categorize players
        ready_list = []
        not_ready_online = []
        not_ready_voice = []
        not_ready_offline = []

        for pid in player_ids:
            player = next((p for p in players if p.discord_id == pid), None)
            display = get_player_display_name(player, pid, guild) if player else f"<@{pid}>"

            if pid in ready_players:
                ready_list.append(display)
            elif pid in voice_players:
                not_ready_voice.append(f"{display} (in voice)")
            elif pid in online_players:
                not_ready_online.append(f"{display} (online)")
            else:
                not_ready_offline.append(f"<@{pid}> (offline)")

        # Build embed
        color = discord.Color.green() if ready_count == total else discord.Color.orange()
        embed = discord.Embed(
            title="🎮 Ready Check Status",
            description=f"**Lobby Control:** {designated_display}\n**Ready:** {ready_count}/{total} players",
            color=color,
        )

        # Ready section
        if ready_list:
            embed.add_field(
                name=f"✅ Ready ({len(ready_list)})",
                value="\n".join(f"• {name}" for name in ready_list[:25]) or "None",
                inline=False,
            )

        # Not ready sections
        not_ready_all = not_ready_voice + not_ready_online + not_ready_offline
        if not_ready_all:
            embed.add_field(
                name=f"⏳ Waiting ({len(not_ready_all)})",
                value="\n".join(f"• {name}" for name in not_ready_all[:25]),
                inline=False,
            )

        embed.set_footer(text="Click the Ready button in your DM or above to confirm!")

        return embed

    async def update_ready_check_embed(self, guild_id: int):
        """
        Update ready check status embed after button click.

        Called by ReadyCheckView when a player clicks the Ready button.
        """
        try:
            # Get ready check service
            ready_check_service = getattr(self.bot, "ready_check_service", None)
            if not ready_check_service:
                return

            state = ready_check_service.get_state(guild_id)
            if not state:
                return

            # Get status message
            if not state.status_message_id or not state.status_channel_id:
                logger.debug(f"No status message stored for guild {guild_id}")
                return

            # Fetch channel and message
            try:
                channel = await self.bot.fetch_channel(state.status_channel_id)
                status_msg = await channel.fetch_message(state.status_message_id)
            except Exception as exc:
                logger.warning(f"Could not fetch status message: {exc}")
                return

            # Get lobby and player data
            lobby = self.lobby_service.get_lobby()
            if not lobby:
                return

            player_ids, players = self.lobby_service.get_lobby_players(lobby)
            guild = self.bot.get_guild(guild_id) if guild_id else None

            # Get designated player display
            designated_display = "Admins have control"
            if not state.admin_in_lobby and state.designated_player_id:
                member = guild.get_member(state.designated_player_id) if guild else None
                designated_display = (
                    member.mention if member else f"<@{state.designated_player_id}>"
                )

            # Rebuild embed with updated ready players
            embed = self._build_ready_check_status_embed(
                player_ids=list(state.total_players),
                players=players,
                ready_players=state.ready_players,
                online_players=state.online_players,
                voice_players=state.voice_players,
                guild=guild,
                designated_display=designated_display,
            )

            await status_msg.edit(embed=embed)
            logger.info(f"Updated ready check status embed for guild {guild_id}")

        except Exception as exc:
            logger.warning(f"Error updating ready check embed: {exc}")


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    player_service = getattr(bot, "player_service", None)
    cog = LobbyCommands(bot, lobby_service, player_service)
    await bot.add_cog(cog)
