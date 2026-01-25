"""
Lobby commands: /lobby, /kick, /resetlobby.

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
from utils.formatting import FROGLING_EMOJI_ID, JOPACOIN_EMOJI_ID
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
            embed = self.lobby_service.build_lobby_embed(lobby)
            if embed:
                await message.edit(embed=embed, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to update lobby message: {exc}")

    async def _sync_lobby_displays(self, lobby) -> None:
        """Update channel message embed (which is also the thread starter)."""
        embed = self.lobby_service.build_lobby_embed(lobby)

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

    async def _update_thread_embed(self, lobby, embed=None) -> None:
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
                embed = self.lobby_service.build_lobby_embed(lobby)
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

    @app_commands.command(name="lobby", description="Create or view the matchmaking lobby")
    async def lobby(self, interaction: discord.Interaction):
        logger.info(f"Lobby command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=False):
            return

        player = self.player_service.get_player(interaction.user.id)
        if not player:
            await interaction.followup.send(
                "❌ You're not registered! Use `/register` first.", ephemeral=True
            )
            return

        # Block if a match is pending recording
        guild_id = interaction.guild.id if interaction.guild else None
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
            embed = self.lobby_service.build_lobby_embed(lobby)

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
                    await self._update_thread_embed(lobby)

                    await interaction.followup.send(f"[View Lobby]({message.jump_url})", ephemeral=True)
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

                # Complete the deferred response
                await interaction.followup.send(
                    f"✅ Lobby created! [View Lobby]({channel_msg.jump_url})", ephemeral=True
                )
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
                    f"❌ Failed to create lobby thread: {exc}",
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
            await self._sync_lobby_displays(lobby)

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

        # Check registration
        player = self.player_service.get_player(interaction.user.id)
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
        guild_id = interaction.guild.id if interaction.guild else None
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
        await self._sync_lobby_displays(lobby)
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
        await self._sync_lobby_displays(lobby)

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


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    player_service = getattr(bot, "player_service", None)
    cog = LobbyCommands(bot, lobby_service, player_service)
    await bot.add_cog(cog)
