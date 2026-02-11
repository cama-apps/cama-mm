"""
Lobby commands: /lobby, /kick, /resetlobby.

Uses Discord threads for lobby management similar to /prediction.
"""

import asyncio
import functools
import logging
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from config import LOBBY_CHANNEL_ID
from services.lobby_service import LobbyService
from services.permissions import has_admin_permission
from utils.formatting import FROGLING_EMOJI_ID, FROGLING_EMOTE, JOPACOIN_EMOJI_ID, format_duration_short
from utils.interaction_safety import safe_defer
from utils.pin_helpers import safe_unpin_all_bot_messages
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from services.player_service import PlayerService

logger = logging.getLogger("cama_bot.commands.lobby")

# Players who joined within this window are considered active regardless of status
RECENT_JOIN_THRESHOLD = 5 * 60  # 5 minutes


class LobbyCommands(commands.Cog):
    """Slash commands for lobby management."""

    def __init__(self, bot: commands.Bot, lobby_service: LobbyService, player_service):
        self.bot = bot
        self.lobby_service = lobby_service
        self.player_service = player_service

    def rebuild_readycheck_embed(self) -> discord.Embed | None:
        """Rebuild the readycheck embed from stored data. Used by bot.py reaction handler."""
        player_data = self.lobby_service.get_readycheck_player_data()
        if not player_data:
            return None
        reacted = self.lobby_service.get_readycheck_reacted()
        embed, _ = build_readycheck_embed(player_data, reacted)
        return embed

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
            embed = await asyncio.to_thread(self.lobby_service.build_lobby_embed, lobby, guild_id)
            if embed:
                await message.edit(embed=embed, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to update lobby message: {exc}")

    async def _sync_lobby_displays(self, lobby, guild_id: int | None = None) -> None:
        """Update channel message embed (which is also the thread starter)."""
        embed = await asyncio.to_thread(self.lobby_service.build_lobby_embed, lobby, guild_id)

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
                embed = await asyncio.to_thread(self.lobby_service.build_lobby_embed, lobby, guild_id)
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
        player = await asyncio.to_thread(self.player_service.get_player, user_id, guild_id)
        if not player or not player.preferred_roles:
            return False, "⚠️ Set your preferred roles with `/setroles` to auto-join."

        # Check if this player is already in a pending match
        match_service = getattr(self.bot, "match_service", None)
        if match_service:
            player_match = await asyncio.to_thread(
                match_service.state_service.get_pending_match_for_player, guild_id, user_id
            )
            if player_match:
                return False, None  # Player is in a pending match, can't auto-join

        # Attempt to join
        success, reason = await asyncio.to_thread(self.lobby_service.join_lobby, user_id)
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
        player = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await interaction.followup.send(
                "❌ You're not registered! Use `/register` first.", ephemeral=True
            )
            return

        # Acquire lock to prevent race condition when multiple users call /lobby simultaneously
        async with self.lobby_service.creation_lock:
            lobby = await asyncio.to_thread(
                functools.partial(self.lobby_service.get_or_create_lobby, creator_id=interaction.user.id)
            )
            embed = await asyncio.to_thread(self.lobby_service.build_lobby_embed, lobby, guild_id)

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
                await asyncio.to_thread(
                    functools.partial(
                        self.lobby_service.set_lobby_message_id,
                        message_id=channel_msg.id,
                        channel_id=target_channel.id,  # Where the embed lives (dedicated or interaction)
                        thread_id=thread.id,
                        embed_message_id=channel_msg.id,  # The channel msg IS the embed in thread
                        origin_channel_id=origin_channel_id,  # Where /lobby was run (for rally)
                    )
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
            removed = await asyncio.to_thread(self.lobby_service.leave_lobby, player.id)
        else:
            removed = await asyncio.to_thread(self.lobby_service.leave_lobby_conditional, player.id)
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
        player = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
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

        # Block if THIS player is already in a pending match
        match_service = getattr(self.bot, "match_service", None)
        if match_service:
            player_match = await asyncio.to_thread(
                match_service.state_service.get_pending_match_for_player, guild_id, interaction.user.id
            )
            if player_match:
                pending_match_id = player_match.get("pending_match_id")
                jump_url = player_match.get("shuffle_message_jump_url")
                message_text = f"❌ You're already in a pending match (Match #{pending_match_id})!"
                if jump_url:
                    message_text += f" [View your match]({jump_url}) and use `/record` to complete it first."
                else:
                    message_text += " Use `/record` to complete it first."
                await interaction.followup.send(message_text, ephemeral=True)
                return

        # Attempt to join
        success, reason = await asyncio.to_thread(self.lobby_service.join_lobby, interaction.user.id)
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

        # Neon Degen Terminal hook for lobby join
        try:
            neon = getattr(self.bot, "neon_degen_service", None)
            if neon and lobby:
                queue_position = len(lobby.players) + len(lobby.conditional_players)
                neon_result = await neon.on_lobby_join(
                    interaction.user.id, guild_id, queue_position
                )
                if neon_result and (neon_result.text_block or neon_result.footer_text):
                    channel_id = self.lobby_service.get_lobby_channel_id()
                    if channel_id:
                        channel = self.bot.get_channel(channel_id)
                        if channel:
                            text = neon_result.text_block or neon_result.footer_text
                            await channel.send(text)
        except Exception as e:
            logger.debug(f"Neon lobby join hook error: {e}")

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
            await asyncio.to_thread(self.lobby_service.leave_lobby, interaction.user.id)
        else:
            await asyncio.to_thread(self.lobby_service.leave_lobby_conditional, interaction.user.id)

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
        name="joinnext",
        description="Queue for the next match while a game is in progress",
    )
    async def joinnext(self, interaction: discord.Interaction):
        """Join the next match queue while a current game is pending."""
        logger.info(f"Joinnext command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Require a pending match or active draft
        match_service = getattr(self.bot, "match_service", None)
        draft_state_manager = getattr(self.bot, "draft_state_manager", None)
        pending_match = None
        if match_service:
            pending_match = await asyncio.to_thread(match_service.get_last_shuffle, guild_id)
        active_draft = draft_state_manager and draft_state_manager.has_active_draft(guild_id)

        if not pending_match and not active_draft:
            await interaction.followup.send(
                "⚠️ No active game in progress. Use `/join` instead.", ephemeral=True
            )
            return

        # Check registration
        player = await asyncio.to_thread(
            self.player_service.get_player, interaction.user.id, guild_id
        )
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

        # Attempt to join the next-match queue (separate from main lobby)
        success, reason = await asyncio.to_thread(
            self.lobby_service.join_next_lobby, interaction.user.id
        )
        if not success:
            await interaction.followup.send(f"❌ {reason}", ephemeral=True)
            return

        # Get updated next_lobby for count
        next_lobby = self.lobby_service.get_next_lobby()
        count = next_lobby.get_total_count() if next_lobby else 1
        await interaction.followup.send(
            f"✅ Joined the next match queue! ({count} queued)", ephemeral=True
        )

    @app_commands.command(
        name="leavenext",
        description="Leave the next match queue",
    )
    async def leavenext(self, interaction: discord.Interaction):
        """Leave the next match queue while a current game is pending."""
        logger.info(f"Leavenext command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id if interaction.guild else None

        # Require a pending match or active draft
        match_service = getattr(self.bot, "match_service", None)
        draft_state_manager = getattr(self.bot, "draft_state_manager", None)
        pending_match = None
        if match_service:
            pending_match = await asyncio.to_thread(match_service.get_last_shuffle, guild_id)
        active_draft = draft_state_manager and draft_state_manager.has_active_draft(guild_id)

        if not pending_match and not active_draft:
            await interaction.followup.send(
                "⚠️ No active game in progress. Use `/leave` instead.", ephemeral=True
            )
            return

        # Check next_lobby exists and user is in it
        next_lobby = self.lobby_service.get_next_lobby()
        if not next_lobby:
            await interaction.followup.send(
                "⚠️ You're not in the next match queue.", ephemeral=True
            )
            return

        in_queue = (
            interaction.user.id in next_lobby.players
            or interaction.user.id in next_lobby.conditional_players
        )
        if not in_queue:
            await interaction.followup.send(
                "⚠️ You're not in the next match queue.", ephemeral=True
            )
            return

        await asyncio.to_thread(self.lobby_service.leave_next_lobby, interaction.user.id)
        await interaction.followup.send("✅ Left the next match queue.", ephemeral=True)

    @app_commands.command(
        name="viewnext",
        description="View players queued for the next match",
    )
    async def viewnext(self, interaction: discord.Interaction):
        """View the next-match queue."""
        logger.info(f"Viewnext command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        next_lobby = self.lobby_service.get_next_lobby()
        if not next_lobby or next_lobby.get_total_count() == 0:
            await interaction.followup.send(
                "⚠️ No players queued for the next match.", ephemeral=True
            )
            return

        lines = []
        for i, pid in enumerate(next_lobby.players, 1):
            lines.append(f"{i}. <@{pid}>")
        if next_lobby.conditional_players:
            for pid in next_lobby.conditional_players:
                lines.append(f"- <@{pid}> *(conditional)*")

        count = next_lobby.get_total_count()
        message_text = f"**Next Match Queue** ({count} player{'s' if count != 1 else ''}):\n" + "\n".join(lines)

        await interaction.followup.send(
            message_text, ephemeral=True,
            allowed_mentions=discord.AllowedMentions.none(),
        )

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
            pending_match = await asyncio.to_thread(match_service.get_last_shuffle, guild_id)
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
        await asyncio.to_thread(self.lobby_service.reset_lobby)

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
        name="readycheck",
        description="Check lobby players' online status and ping those who are away",
    )
    async def readycheck(self, interaction: discord.Interaction):
        logger.info(f"Readycheck command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild_id = interaction.guild.id if interaction.guild else None

        lobby = self.lobby_service.get_lobby()
        if not lobby:
            await interaction.followup.send("⚠️ No active lobby.", ephemeral=True)
            return

        if lobby.get_total_count() < 10:
            await interaction.followup.send(
                f"⚠️ Need at least 10 players for a ready check ({lobby.get_total_count()}/10).",
                ephemeral=True,
            )
            return

        guild = interaction.guild
        if not guild:
            await interaction.followup.send("❌ This command must be used in a server.", ephemeral=True)
            return

        # Global shared rate limit (1 per 120s per guild) — checked after
        # preconditions so failed attempts don't consume the cooldown
        rl = GLOBAL_RATE_LIMITER.check(
            scope="readycheck",
            guild_id=guild_id or 0,
            user_id=0,
            limit=1,
            per_seconds=120,
        )
        if not rl.allowed:
            await interaction.followup.send(
                f"⏳ Ready check on cooldown. Try again in {rl.retry_after_seconds}s.",
                ephemeral=True,
            )
            return

        all_player_ids = list(lobby.players | lobby.conditional_players)
        current_lobby_set = set(all_player_ids)

        # Classify every player — store structured data for later rebuilds
        player_data: dict[int, dict] = {}
        now = time.time()

        for pid in all_player_ids:
            member = guild.get_member(pid)
            if not member:
                try:
                    member = await guild.fetch_member(pid)
                except Exception:
                    # Can't fetch member - treat as AFK
                    player = await asyncio.to_thread(self.player_service.get_player, pid, guild_id)
                    fallback_name = player.name if player else f"User {pid}"
                    player_data[pid] = {
                        "group": "afk",
                        "signals": "🔴",
                        "name": fallback_name,
                        "is_conditional": pid in lobby.conditional_players,
                        "join_ts": lobby.player_join_times.get(pid),
                        "is_member": False,
                    }
                    continue

            # Get join time for classification
            join_ts = lobby.player_join_times.get(pid)
            time_in_lobby = (now - join_ts) if join_ts else float('inf')
            is_recent = time_in_lobby < RECENT_JOIN_THRESHOLD

            signals = []
            is_afk = False

            # Voice status (informational only - deafened doesn't mean AFK)
            if member.voice is not None:
                is_deafened = bool(
                    getattr(member.voice, "self_deaf", False)
                    or getattr(member.voice, "deaf", False)
                )
                signals.append("🔇" if is_deafened else "🔊")

            # Dota status
            if _is_playing_dota(member):
                signals.append("🎮")

            # Presence status - this determines AFK classification
            status = member.status
            if status in (discord.Status.online, discord.Status.dnd):
                signals.append("🟢")
            elif status == discord.Status.idle:
                signals.append("🟡")
                if not is_recent:
                    is_afk = True
            else:  # offline/invisible
                signals.append("🔴")
                if not is_recent:
                    is_afk = True

            player_data[pid] = {
                "group": "afk" if is_afk else "active",
                "signals": "".join(signals),
                "name": member.display_name,
                "is_conditional": pid in lobby.conditional_players,
                "join_ts": join_ts,
                "is_member": True,
            }

        # Check if refreshing an existing readycheck
        existing_msg_id = self.lobby_service.get_readycheck_message_id()
        existing_channel_id = self.lobby_service.get_readycheck_channel_id()
        is_refresh = False
        msg = None

        if existing_msg_id and existing_channel_id:
            try:
                ch = self.bot.get_channel(existing_channel_id)
                if not ch:
                    ch = await self.bot.fetch_channel(existing_channel_id)
                msg = await ch.fetch_message(existing_msg_id)
                is_refresh = True
            except (discord.NotFound, discord.HTTPException):
                msg = None

        # On refresh: update data + prune reacted. On new: store fresh.
        if is_refresh:
            self.lobby_service.update_readycheck_data(current_lobby_set, player_data)
        reacted = self.lobby_service.get_readycheck_reacted() if is_refresh else {}

        # Build embed from stored data (excludes reacted from Active/AFK)
        embed, mention_ids = build_readycheck_embed(player_data, reacted)

        # Resolve target channel - lobby thread only
        target_channel = None
        lobby_thread_id = self.lobby_service.get_lobby_thread_id()
        if lobby_thread_id:
            try:
                target_channel = self.bot.get_channel(lobby_thread_id)
                if not target_channel:
                    target_channel = await self.bot.fetch_channel(lobby_thread_id)
            except Exception:
                pass

        if not target_channel:
            await interaction.followup.send(
                "❌ No lobby thread found. Create a lobby with `/lobby` first.",
                ephemeral=True,
            )
            return

        # Ping AFK players (exclude those who already reacted)
        allowed_mentions = discord.AllowedMentions(
            users=[discord.Object(id=uid) for uid in mention_ids]
        )
        ping_content = None
        if mention_ids:
            tags = " ".join(f"<@{uid}>" for uid in mention_ids)
            ping_content = f"**Possibly AFK:** {tags}"

        if is_refresh and msg:
            await msg.edit(embed=embed)
            self.lobby_service.update_readycheck_data(current_lobby_set, player_data)
            if ping_content:
                await msg.channel.send(ping_content, allowed_mentions=allowed_mentions)
            await interaction.followup.send(
                f"✅ Ready check refreshed! [View]({msg.jump_url})", ephemeral=True
            )
        else:
            # Post to lobby thread (target_channel is guaranteed to exist here)
            msg = await target_channel.send(embed=embed)
            try:
                await msg.add_reaction("✅")
            except Exception:
                pass
            if ping_content:
                await target_channel.send(ping_content, allowed_mentions=allowed_mentions)
            await interaction.followup.send(
                f"✅ Ready check posted! [View]({msg.jump_url})", ephemeral=True
            )

            self.lobby_service.set_readycheck_state(
                msg.id, msg.channel.id, current_lobby_set, player_data
            )


def build_readycheck_embed(
    player_data: dict[int, dict],
    reacted: dict[int, str],
) -> tuple[discord.Embed, list[int]]:
    """Build the readycheck embed from stored classification data.

    Returns (embed, mention_ids) where mention_ids are AFK members to ping.
    """
    now = time.time()
    embed = discord.Embed(
        title="Ready Check",
        description=f"**{len(player_data)}** players in lobby",
        color=discord.Color.blue(),
    )

    active_lines: list[str] = []
    afk_lines: list[str] = []
    mention_ids: list[int] = []

    for pid, d in player_data.items():
        if pid in reacted:
            continue
        frogling = f" {FROGLING_EMOTE}" if d["is_conditional"] else ""
        join_ts = d.get("join_ts")
        time_str = f" ({format_duration_short(now - join_ts)})" if join_ts else ""
        if d["group"] == "active":
            active_lines.append(f"{d['name']} {d['signals']}{frogling}{time_str}")
        else:
            if d["is_member"]:
                mention_ids.append(pid)
                afk_lines.append(f"<@{pid}> {d['signals']}{frogling}{time_str}")
            else:
                afk_lines.append(f"{d['name']} {d['signals']}{frogling}{time_str}")

    if active_lines:
        embed.add_field(
            name=f"✅ Likely Active ({len(active_lines)})",
            value="\n".join(active_lines),
            inline=False,
        )
    if afk_lines:
        embed.add_field(
            name=f"⚠️ Possibly AFK ({len(afk_lines)})",
            value="\n".join(afk_lines),
            inline=False,
        )
    if reacted:
        embed.add_field(
            name=f"✅ Reacted to Ready Check ({len(reacted)})",
            value="\n".join(reacted.values()),
            inline=False,
        )

    embed.set_footer(text="React with ✅ to confirm you are ready")
    return embed, mention_ids


def _is_playing_dota(member: discord.Member) -> bool:
    """Check if a member is currently playing Dota 2."""
    for activity in member.activities:
        if isinstance(activity, discord.Game) and activity.name and "dota" in activity.name.lower():
            return True
        if isinstance(activity, discord.Activity) and activity.name and "dota" in activity.name.lower():
            return True
    return False


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    player_service = getattr(bot, "player_service", None)
    cog = LobbyCommands(bot, lobby_service, player_service)
    await bot.add_cog(cog)
