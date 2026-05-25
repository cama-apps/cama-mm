"""
Lobby commands: /lobby, /kick, /resetlobby.

Uses Discord threads for lobby management similar to /prediction.
"""

import asyncio
import functools
import logging
import time

import discord
from discord import app_commands
from discord.ext import commands

from commands.checks import require_guild
from config import LOBBY_CHANNEL_ID
from services.lobby_service import LobbyService
from services.permissions import has_admin_permission
from utils.formatting import (
    FROGLING_EMOJI_ID,
    FROGLING_EMOTE,
    JOPACOIN_EMOJI_ID,
    format_duration_short,
)
from utils.interaction_safety import safe_defer, safe_followup, update_lobby_message_closed
from utils.neon_helpers import get_neon_service
from utils.pin_helpers import safe_unpin_all_bot_messages
from utils.rate_limiter import GLOBAL_RATE_LIMITER

logger = logging.getLogger("cama_bot.commands.lobby")

# Players who joined within this window are considered active regardless of status
RECENT_JOIN_THRESHOLD = 5 * 60  # 5 minutes

# A ready check older than this is stale: the next /readycheck deletes the
# buried original and posts a fresh one (resetting ✅ confirmations and pruning
# AFK no-shows) instead of editing it in place.
READYCHECK_STALE_THRESHOLD = 30 * 60  # 30 minutes


class LobbyCommands(commands.Cog):
    """Slash commands for lobby management."""

    def __init__(self, bot: commands.Bot, lobby_service: LobbyService, player_service):
        self.bot = bot
        self.lobby_service = lobby_service
        self.player_service = player_service

    def rebuild_readycheck_embed(self, guild_id: int | None = None) -> discord.Embed | None:
        """Rebuild the readycheck embed from stored data. Used by bot.py reaction handler."""
        player_data = self.lobby_service.get_readycheck_player_data(guild_id=guild_id)
        if not player_data:
            return None
        reacted = self.lobby_service.get_readycheck_reacted(guild_id=guild_id)
        embed, _ = build_readycheck_embed(
            player_data, reacted, ready_threshold=self.lobby_service.ready_threshold
        )
        return embed

    async def _get_lobby_target_channel(
        self, interaction: discord.Interaction
    ) -> discord.abc.Messageable | None:
        """
        Get the target channel for lobby embeds.

        Returns the dedicated lobby channel when :data:`LOBBY_CHANNEL_ID` is
        configured, accessible, and in the same guild as the interaction;
        otherwise falls back to ``interaction.channel``. Returns ``None`` only
        if no usable channel could be resolved.
        """
        # If no dedicated channel configured, use interaction channel
        if not LOBBY_CHANNEL_ID:
            return interaction.channel

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
                    return interaction.channel

                perms = channel.permissions_for(channel.guild.me)
                if not perms.send_messages or not perms.create_public_threads:
                    logger.warning(
                        f"Bot lacks permissions in dedicated lobby channel {LOBBY_CHANNEL_ID}"
                    )
                    return interaction.channel

            return channel
        except (discord.NotFound, discord.Forbidden) as exc:
            logger.warning(f"Cannot access dedicated lobby channel {LOBBY_CHANNEL_ID}: {exc}")
            return interaction.channel
        except Exception as exc:
            logger.warning(f"Error fetching dedicated lobby channel: {exc}")
            return interaction.channel

    async def _safe_pin(self, message: discord.Message) -> None:
        """Pin the lobby message, logging but not raising on failure (e.g., missing perms)."""
        try:
            await message.pin(reason="Cama lobby active")
        except discord.Forbidden:
            logger.warning("Cannot pin lobby message: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to pin lobby message: {exc}")


    async def _remove_user_lobby_reactions(
        self,
        user: discord.User | discord.Member,
        guild_id: int | None = None,
    ) -> None:
        """Remove a user's lobby reactions (sword and frogling) from the channel lobby message."""
        message_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_message_id, guild_id=guild_id
        )
        channel_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_channel_id, guild_id=guild_id
        )
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
            except Exception as e:
                logger.debug("Failed to remove sword reaction: %s", e)
            # Remove frogling reaction
            try:
                frogling_emoji = discord.PartialEmoji(name="frogling", id=FROGLING_EMOJI_ID)
                await message.remove_reaction(frogling_emoji, user)
            except Exception as e:
                logger.debug("Failed to remove frogling reaction: %s", e)
        except discord.Forbidden:
            logger.warning("Cannot remove reaction: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to remove user lobby reactions: {exc}")

    async def _update_lobby_message(self, interaction: discord.Interaction, lobby) -> None:
        guild_id = interaction.guild.id if interaction.guild else None
        message_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_message_id, guild_id=guild_id
        )
        if not message_id:
            return
        try:
            channel = interaction.channel
            message = await channel.fetch_message(message_id)
            embed = await asyncio.to_thread(self.lobby_service.build_lobby_embed, lobby, guild_id)
            if embed:
                await message.edit(embed=embed, allowed_mentions=discord.AllowedMentions.none())
        except Exception as exc:
            logger.warning(f"Failed to update lobby message: {exc}")

    async def _sync_lobby_displays(self, lobby, guild_id: int | None = None) -> None:
        """Update channel message embed (which is also the thread starter)."""
        embed = await asyncio.to_thread(self.lobby_service.build_lobby_embed, lobby, guild_id)

        # Update channel message - this also updates the thread starter view
        message_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_message_id, guild_id=guild_id
        )
        channel_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_channel_id, guild_id=guild_id
        )
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
        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id, guild_id=guild_id
        )
        embed_message_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_embed_message_id, guild_id=guild_id
        )

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

    async def _update_channel_message_closed(
        self, reason: str = "Lobby Closed", guild_id: int | None = None
    ) -> None:
        """Update the channel message embed to show lobby is closed."""
        await update_lobby_message_closed(self.bot, self.lobby_service, reason, guild_id=guild_id)

    async def _archive_lobby_thread(
        self, reason: str = "Lobby Reset", guild_id: int | None = None
    ) -> None:
        """Lock and archive the lobby thread with a status message."""
        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id, guild_id=guild_id
        )
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
            except Exception as e:
                logger.debug("Failed to send archive message to thread: %s", e)

            try:
                await thread.edit(name=f"🚫 {reason}", locked=True, archived=True)
            except discord.Forbidden:
                try:
                    await thread.edit(archived=True)
                except Exception as e:
                    logger.debug("Failed to archive thread: %s", e)
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
            return False, "⚠️ Set your preferred roles with `/player roles` to auto-join."

        # Attempt to join (pending match check now inside LobbyService)
        success, reason, pending_info = await asyncio.to_thread(
            self.lobby_service.join_lobby, user_id, guild_id
        )
        if not success:
            # Auto-join failures are silent (including pending match)
            logger.info(f"Auto-join failed for {user_id}: {reason}")
            return False, None

        # Refresh lobby state
        lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)

        # Update displays
        await self._sync_lobby_displays(lobby, guild_id)

        # Post join activity in thread
        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id, guild_id=guild_id
        )
        if thread_id:
            await self._post_join_activity(thread_id, interaction.user)

        # Rally/ready notifications
        from bot import notify_lobby_rally, notify_lobby_ready

        channel_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_channel_id, guild_id=guild_id
        )
        if channel_id and thread_id:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                thread = self.bot.get_channel(thread_id)
                if not thread:
                    thread = await self.bot.fetch_channel(thread_id)

                is_ready = await asyncio.to_thread(self.lobby_service.is_ready, lobby)
                if not is_ready:
                    await notify_lobby_rally(channel, thread, lobby, guild_id or 0)
                else:
                    await notify_lobby_ready(channel, lobby, guild_id=guild_id or 0)
            except Exception as exc:
                logger.warning(f"Failed to send rally/ready notification on auto-join: {exc}")

        return True, None

    @app_commands.command(name="lobby", description="Create or view the matchmaking lobby")
    @require_guild
    async def lobby(self, interaction: discord.Interaction):
        logger.info(f"Lobby command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild_id = interaction.guild.id
        player = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await safe_followup(
                interaction, content="❌ You're not registered! Use `/player register` first.", ephemeral=True
            )
            return

        # Acquire per-guild lock to prevent race condition when multiple users
        # call /lobby simultaneously in the same guild.
        async with self.lobby_service.get_creation_lock(guild_id=guild_id):
            lobby = await asyncio.to_thread(
                functools.partial(
                    self.lobby_service.get_or_create_lobby,
                    creator_id=interaction.user.id,
                    guild_id=guild_id,
                )
            )
            embed = await asyncio.to_thread(self.lobby_service.build_lobby_embed, lobby, guild_id)

            # If message/thread already exists, refresh it; otherwise create new
            message_id = await asyncio.to_thread(
                self.lobby_service.get_lobby_message_id, guild_id=guild_id
            )
            thread_id = await asyncio.to_thread(
                self.lobby_service.get_lobby_thread_id, guild_id=guild_id
            )

            if message_id and thread_id:
                try:
                    # Fetch message from the dedicated/lobby channel (not necessarily interaction channel)
                    lobby_channel_id = await asyncio.to_thread(
                        self.lobby_service.get_lobby_channel_id, guild_id=guild_id
                    )
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
                    refreshed_lobby = await asyncio.to_thread(
                        self.lobby_service.get_lobby, guild_id=guild_id
                    )
                    await self._update_thread_embed(
                        refreshed_lobby,
                        guild_id=guild_id,
                    )

                    # Build response based on join result
                    if joined:
                        response = f"✅ Joined! [View Lobby]({message.jump_url})"
                    elif warning:
                        response = f"{warning} [View Lobby]({message.jump_url})"
                    else:
                        response = f"[View Lobby]({message.jump_url})"

                    await safe_followup(interaction, content=response, ephemeral=True)
                    return
                except Exception as e:
                    # Log so a real failure (e.g. permissions, network) doesn't
                    # silently produce a duplicate lobby with orphan channel
                    # artifacts. We still fall through to create a new one.
                    logger.warning(
                        "Existing lobby refresh failed; creating a new lobby: %s", e
                    )

            # Get target channel (dedicated or fallback to interaction channel)
            target_channel = await self._get_lobby_target_channel(interaction)
            if not target_channel:
                await safe_followup(
                    interaction, content="❌ Could not find a valid channel to post the lobby.", ephemeral=True
                )
                return

            # Store the origin channel (where /lobby was run) for rally notifications
            origin_channel_id = interaction.channel.id

            # Send channel message with embed
            channel_msg = await target_channel.send(embed=embed)

            # Pin the lobby message for visibility
            await self._safe_pin(channel_msg)

            # Add reaction emojis for joining (sword for regular, frogling for conditional,
            # jopacoin for gamba notifications, bell for /readycheck shortcut)
            try:
                await channel_msg.add_reaction("⚔️")
                # Add frogling emoji using PartialEmoji with ID
                frogling_emoji = discord.PartialEmoji(name="frogling", id=FROGLING_EMOJI_ID)
                await channel_msg.add_reaction(frogling_emoji)
                # Add jopacoin emoji for subscribing to gamba notifications
                jopacoin_emoji = discord.PartialEmoji(name="jopacoin", id=JOPACOIN_EMOJI_ID)
                await channel_msg.add_reaction(jopacoin_emoji)
                # Bell triggers a ready check (equivalent to /readycheck)
                await channel_msg.add_reaction("🔔")
            except Exception as e:
                logger.debug("Failed to add lobby reactions: %s", e)

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
                        guild_id=guild_id,
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

                await safe_followup(interaction, content=response, ephemeral=True)
                return

            except discord.Forbidden:
                # Thread permissions required
                logger.warning("Cannot create lobby thread: missing Create Public Threads permission.")
                await channel_msg.delete()
                await safe_followup(
                    interaction, content="❌ Bot needs 'Create Public Threads' permission to create lobbies.",
                    ephemeral=True,
                )
            except Exception as exc:
                logger.exception(f"Error creating lobby thread: {exc}")
                await channel_msg.delete()
                await safe_followup(
                    interaction, content="❌ Failed to create lobby thread. Please try again or contact an admin.",
                    ephemeral=True,
                )

    @app_commands.command(
        name="kick",
        description="Kick a player from the lobby (Admin or lobby creator only)",
    )
    @app_commands.describe(player="The player to kick from the lobby")
    @require_guild
    async def kick(self, interaction: discord.Interaction, player: discord.Member):
        logger.info(f"Kick command: User {interaction.user.id} kicking {player.id}")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)
        if not lobby:
            await safe_followup(interaction, content="⚠️ No active lobby.", ephemeral=True)
            return

        is_admin = has_admin_permission(interaction)
        is_creator = lobby.created_by == interaction.user.id
        if not (is_admin or is_creator):
            await safe_followup(
                interaction, content="❌ Permission denied. Admin or lobby creator only.",
                ephemeral=True,
            )
            return

        if player.id == interaction.user.id:
            await safe_followup(
                interaction, content="❌ You can't kick yourself. Use the Leave button in the lobby thread.",
                ephemeral=True,
            )
            return

        # Check if player is in regular or conditional set
        in_regular = player.id in lobby.players
        in_conditional = player.id in lobby.conditional_players

        if not in_regular and not in_conditional:
            await safe_followup(
                interaction, content=f"⚠️ {player.mention} is not in the lobby.", ephemeral=True
            )
            return

        # Remove from whichever set they're in
        if in_regular:
            removed = await asyncio.to_thread(
                self.lobby_service.leave_lobby, player.id, guild_id
            )
        else:
            removed = await asyncio.to_thread(
                self.lobby_service.leave_lobby_conditional, player.id, guild_id
            )
        if removed:
            await safe_followup(
                interaction, content=f"✅ Kicked {player.mention} from the lobby.", ephemeral=True
            )

            # Re-fetch lobby after removal so the embed reflects the current state.
            lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)

            # Update both channel message and thread embed
            await self._sync_lobby_displays(lobby, guild_id)

            # Remove kicked player's lobby reactions (sword and frogling)
            await self._remove_user_lobby_reactions(player, guild_id=guild_id)

            # Post kick activity in thread
            thread_id = await asyncio.to_thread(
                self.lobby_service.get_lobby_thread_id, guild_id=guild_id
            )
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
            except Exception as e:
                logger.debug("Failed to DM kicked player: %s", e)
        else:
            await safe_followup(interaction, content=f"❌ Failed to kick {player.mention}.", ephemeral=True)

    @app_commands.command(name="join", description="Join the matchmaking lobby")
    @require_guild
    async def join(self, interaction: discord.Interaction):
        """Join the matchmaking lobby from any channel."""
        logger.info(f"Join command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id

        # Check registration
        player = await asyncio.to_thread(self.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await safe_followup(
                interaction, content="❌ You're not registered! Use `/player register` first.", ephemeral=True
            )
            return

        # Check roles set
        if not player.preferred_roles:
            await safe_followup(
                interaction, content="❌ Set your preferred roles first! Use `/player roles`.", ephemeral=True
            )
            return

        # Check lobby exists
        lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)
        if not lobby:
            await safe_followup(
                interaction, content="⚠️ No active lobby. Use `/lobby` to create one.", ephemeral=True
            )
            return

        # Attempt to join (pending match check now inside LobbyService)
        success, reason, pending_info = await asyncio.to_thread(
            self.lobby_service.join_lobby, interaction.user.id, guild_id
        )
        if not success:
            if reason == "in_pending_match" and pending_info:
                pending_match_id = pending_info.pending_match_id
                jump_url = pending_info.shuffle_message_jump_url
                message_text = f"❌ You're already in a pending match (Match #{pending_match_id})!"
                if jump_url:
                    message_text += f" [View your match]({jump_url}) and use `/record` to complete it first."
                else:
                    message_text += " Use `/record` to complete it first."
                await safe_followup(interaction, content=message_text, ephemeral=True)
            elif reason == "lobby_full":
                await safe_followup(interaction, content="❌ Lobby is full.", ephemeral=True)
            else:
                await safe_followup(interaction, content="❌ Already in lobby or lobby is closed.", ephemeral=True)
            return

        # Refresh lobby state after join
        lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)

        # Update displays and post activity
        await self._sync_lobby_displays(lobby, guild_id)
        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id, guild_id=guild_id
        )
        if thread_id:
            await self._post_join_activity(thread_id, interaction.user)

        # Rally/ready notifications
        from bot import notify_lobby_rally, notify_lobby_ready

        channel_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_channel_id, guild_id=guild_id
        )
        if channel_id and thread_id:
            try:
                channel = self.bot.get_channel(channel_id)
                if not channel:
                    channel = await self.bot.fetch_channel(channel_id)
                thread = self.bot.get_channel(thread_id)
                if not thread:
                    thread = await self.bot.fetch_channel(thread_id)

                is_ready = await asyncio.to_thread(self.lobby_service.is_ready, lobby)
                if not is_ready:
                    await notify_lobby_rally(channel, thread, lobby, guild_id)
                else:
                    await notify_lobby_ready(channel, lobby, guild_id=guild_id)
            except Exception as exc:
                logger.warning(f"Failed to send rally/ready notification: {exc}")

        await safe_followup(interaction, content="✅ Joined the lobby!", ephemeral=True)

        # Neon Degen Terminal hook for lobby join
        try:
            neon = get_neon_service(self.bot)
            if neon and lobby:
                queue_position = len(lobby.players) + len(lobby.conditional_players)
                neon_result = await neon.on_lobby_join(
                    interaction.user.id, guild_id, queue_position
                )
                if neon_result and (neon_result.text_block or neon_result.footer_text):
                    channel_id = await asyncio.to_thread(
                        self.lobby_service.get_lobby_channel_id, guild_id=guild_id
                    )
                    if channel_id:
                        channel = self.bot.get_channel(channel_id)
                        if channel:
                            text = neon_result.text_block or neon_result.footer_text
                            await channel.send(text)
        except Exception as e:
            logger.debug(f"Neon lobby join hook error: {e}")

    @app_commands.command(name="leave", description="Leave the matchmaking lobby")
    @require_guild
    async def leave(self, interaction: discord.Interaction):
        """Leave the matchmaking lobby from any channel."""
        logger.info(f"Leave command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=True):
            return

        guild_id = interaction.guild.id
        lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)
        if not lobby:
            await safe_followup(interaction, content="⚠️ No active lobby.", ephemeral=True)
            return

        in_regular = interaction.user.id in lobby.players
        in_conditional = interaction.user.id in lobby.conditional_players

        if not in_regular and not in_conditional:
            await safe_followup(interaction, content="⚠️ You're not in the lobby.", ephemeral=True)
            return

        # Remove from appropriate queue
        if in_regular:
            await asyncio.to_thread(
                self.lobby_service.leave_lobby, interaction.user.id, guild_id
            )
        else:
            await asyncio.to_thread(
                self.lobby_service.leave_lobby_conditional, interaction.user.id, guild_id
            )

        # Re-fetch lobby after removal so the embed reflects the current state.
        lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)

        # Update displays
        await self._sync_lobby_displays(lobby, guild_id)

        # Remove user's reactions
        await self._remove_user_lobby_reactions(interaction.user, guild_id=guild_id)

        # Post leave activity in thread
        thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id, guild_id=guild_id
        )
        if thread_id:
            await self._post_leave_activity(thread_id, interaction.user)

        await safe_followup(interaction, content="✅ Left the lobby.", ephemeral=True)

    @app_commands.command(
        name="resetlobby",
        description="Reset the current lobby (Admin or lobby creator only)",
    )
    @require_guild
    async def resetlobby(self, interaction: discord.Interaction):
        """Allow admins or lobby creators to reset/abort an unfilled lobby."""
        logger.info(f"Reset lobby command: User {interaction.user.id} ({interaction.user})")
        can_respond = await safe_defer(interaction, ephemeral=True)

        guild_id = interaction.guild.id
        match_service = getattr(self.bot, "match_service", None)
        if match_service:
            pending_match = await asyncio.to_thread(match_service.get_last_shuffle, guild_id)
            if pending_match:
                if can_respond:
                    jump_url = pending_match.shuffle_message_jump_url
                    message_text = "❌ There's a pending match that needs to be recorded!"
                    if jump_url:
                        message_text += (
                            f" [View pending match]({jump_url}) then use `/record` first."
                        )
                    else:
                        message_text += " Use `/record` first."
                    await safe_followup(interaction, content=message_text, ephemeral=True)
                return

        lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)
        if not lobby:
            if can_respond:
                await safe_followup(interaction, content="⚠️ No active lobby.", ephemeral=True)
            return

        is_admin = has_admin_permission(interaction)
        is_creator = lobby.created_by == interaction.user.id
        if not (is_admin or is_creator):
            if can_respond:
                await safe_followup(
                    interaction, content="❌ Permission denied. Admin or lobby creator only.",
                    ephemeral=True,
                )
            return

        # Block if there's an active draft
        draft_state_manager = getattr(self.bot, "draft_state_manager", None)
        has_active_draft = (
            await asyncio.to_thread(draft_state_manager.has_active_draft, guild_id)
            if draft_state_manager
            else False
        )
        if has_active_draft:
            if can_respond:
                await safe_followup(
                    interaction, content="❌ There's an active draft in progress. "
                    "Use `/draft restart` first to clear the draft.",
                    ephemeral=True,
                )
            return

        # Update channel message to show closed and archive thread
        await self._update_channel_message_closed("Lobby Reset", guild_id=guild_id)
        await self._archive_lobby_thread("Lobby Reset", guild_id=guild_id)

        # Unpin from the lobby channel (may be dedicated channel, not interaction channel)
        lobby_channel_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_channel_id, guild_id=guild_id
        )
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
        await asyncio.to_thread(self.lobby_service.reset_lobby, guild_id)

        # Clear lobby rally cooldowns
        from bot import clear_lobby_rally_cooldowns
        clear_lobby_rally_cooldowns(guild_id)

        logger.info(f"Lobby reset by user {interaction.user.id}")
        if can_respond:
            await safe_followup(
                interaction, content="✅ Lobby reset. You can create a new lobby with `/lobby`.",
                ephemeral=True,
            )


    @app_commands.command(
        name="readycheck",
        description="Check lobby players' online status and ping those who are away",
    )
    @require_guild
    async def readycheck(self, interaction: discord.Interaction):
        logger.info(f"Readycheck command: User {interaction.user.id} ({interaction.user})")
        if not await safe_defer(interaction, ephemeral=False):
            return

        guild = interaction.guild
        guild_id = guild.id
        status, info = await self._execute_readycheck(guild, guild_id, interaction.user.id)

        if status == "no_lobby":
            await safe_followup(interaction, content="⚠️ No active lobby.", ephemeral=True)
        elif status == "no_guild":
            await safe_followup(interaction, content="❌ This command must be used in a server.", ephemeral=True)
        elif status == "cooldown":
            await safe_followup(
                interaction,
                content=f"⏳ Ready check on cooldown. Try again in {info['retry_after_seconds']}s.",
                ephemeral=True,
            )
        elif status == "no_thread":
            await safe_followup(
                interaction, content="❌ No lobby thread found. Create a lobby with `/lobby` first.",
                ephemeral=True,
            )
        elif status == "ok":
            verb = "refreshed" if info.get("is_refresh") else "posted"
            await safe_followup(
                interaction,
                content=f"✅ Ready check {verb}! [View]({info['message_jump_url']})",
                ephemeral=True,
            )
        else:  # "error"
            await safe_followup(
                interaction,
                content="❌ Ready check failed — make sure a lobby exists, then try again.",
                ephemeral=True,
            )

    async def _execute_readycheck(
        self,
        guild: discord.Guild | None,
        guild_id: int | None,
        invoker_id: int,
    ) -> tuple[str, dict]:
        """Run the readycheck flow. Returns (status, info).

        Shared by /readycheck and the 🔔 lobby-embed reaction shortcut so the
        cooldown is genuinely a single per-guild bucket. Does not touch any
        Discord interaction object — callers translate the status into the
        appropriate user-facing feedback (ephemeral followup or reaction
        removal). ``invoker_id`` is the user who triggered the check; they are
        never pruned and are auto-counted as ready.

        status: one of "ok" | "no_lobby" | "no_thread" | "cooldown"
                | "no_guild" | "error"
        info contents:
            ok        -> {"message_jump_url": str, "is_refresh": bool,
                          "pruned_count": int}
            cooldown  -> {"retry_after_seconds": int}
            (others)  -> {}
        """
        lobby = await asyncio.to_thread(self.lobby_service.get_lobby, guild_id=guild_id)
        if not lobby:
            return "no_lobby", {}

        if not guild:
            return "no_guild", {}

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
            return "cooldown", {"retry_after_seconds": rl.retry_after_seconds}

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

            # Voice status - if in voice, they're definitely not AFK
            in_voice = member.voice is not None
            if in_voice:
                is_deafened = bool(
                    getattr(member.voice, "self_deaf", False)
                    or getattr(member.voice, "deaf", False)
                )
                signals.append("🔇" if is_deafened else "🔊")

            # Dota status
            if _is_playing_dota(member):
                signals.append("🎮")

            # Presence status - determines AFK classification (unless in voice)
            status = member.status
            if status in (discord.Status.online, discord.Status.dnd):
                signals.append("🟢")
            elif status == discord.Status.idle:
                signals.append("🟡")
                if not is_recent and not in_voice:
                    is_afk = True
            else:  # offline/invisible
                signals.append("🔴")
                if not is_recent and not in_voice:
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
        existing_msg_id = await asyncio.to_thread(
            self.lobby_service.get_readycheck_message_id, guild_id=guild_id
        )
        existing_channel_id = await asyncio.to_thread(
            self.lobby_service.get_readycheck_channel_id, guild_id=guild_id
        )
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

        # If the existing check is stale (30+ min old), delete the buried message
        # and start fresh: prune the players flagged AFK who never confirmed on
        # the previous check (keep the trigger-er and anyone who reacted), then
        # fall through to posting a new message with confirmations reset.
        pruned_ids: list[int] = []
        if is_refresh and msg is not None:
            created_at = await asyncio.to_thread(
                self.lobby_service.get_readycheck_created_at, guild_id=guild_id
            )
            if created_at is not None and (now - created_at) > READYCHECK_STALE_THRESHOLD:
                old_reacted = await asyncio.to_thread(
                    self.lobby_service.get_readycheck_reacted, guild_id=guild_id
                )
                pruned_ids = [
                    pid
                    for pid in list(current_lobby_set)
                    if player_data.get(pid, {}).get("group") == "afk"
                    and pid not in old_reacted
                    and pid != invoker_id
                ]
                if pruned_ids:
                    logger.info(
                        "Stale readycheck prune: removing %d AFK player(s) %s from guild %s",
                        len(pruned_ids),
                        pruned_ids,
                        guild_id,
                    )
                for pid in pruned_ids:
                    if pid in lobby.conditional_players:
                        await asyncio.to_thread(
                            self.lobby_service.leave_lobby_conditional, pid, guild_id
                        )
                    else:
                        await asyncio.to_thread(
                            self.lobby_service.leave_lobby, pid, guild_id
                        )
                    player_data.pop(pid, None)
                    current_lobby_set.discard(pid)
                    await self._remove_user_lobby_reactions(
                        discord.Object(id=pid), guild_id=guild_id
                    )
                if pruned_ids:
                    lobby = await asyncio.to_thread(
                        self.lobby_service.get_lobby, guild_id=guild_id
                    )
                    await self._sync_lobby_displays(lobby, guild_id)
                try:
                    await msg.delete()
                except (discord.NotFound, discord.HTTPException) as e:
                    logger.debug("Failed to delete stale readycheck message: %s", e)
                msg = None
                is_refresh = False

        # On refresh: update data + prune reacted. On new: store fresh.
        if is_refresh:
            await asyncio.to_thread(
                self.lobby_service.update_readycheck_data,
                current_lobby_set,
                player_data,
                guild_id=guild_id,
            )
        reacted = (
            await asyncio.to_thread(
                self.lobby_service.get_readycheck_reacted, guild_id=guild_id
            )
            if is_refresh
            else {}
        )
        # Running the check counts the trigger-er as ready (if they're in the
        # lobby). Reflected in the embed now; persisted to the stored reacted
        # set after the message is saved below.
        if invoker_id in current_lobby_set:
            reacted = dict(reacted)
            reacted[invoker_id] = f"<@{invoker_id}>"

        # Build embed from stored data (excludes reacted from Active/AFK)
        embed, mention_ids = build_readycheck_embed(
            player_data, reacted, ready_threshold=self.lobby_service.ready_threshold
        )

        # Resolve target channel - lobby thread only
        target_channel = None
        lobby_thread_id = await asyncio.to_thread(
            self.lobby_service.get_lobby_thread_id, guild_id=guild_id
        )
        if lobby_thread_id:
            try:
                target_channel = self.bot.get_channel(lobby_thread_id)
                if not target_channel:
                    target_channel = await self.bot.fetch_channel(lobby_thread_id)
            except Exception as e:
                logger.debug("Failed to fetch lobby thread channel: %s", e)

        if not target_channel:
            return "no_thread", {}

        # Ping all lobby members (exclude those who already reacted)
        allowed_mentions = discord.AllowedMentions(
            users=[discord.Object(id=uid) for uid in mention_ids]
        )
        ping_content = None
        if mention_ids:
            tags = " ".join(f"<@{uid}>" for uid in mention_ids)
            ping_content = f"⚔️ **Ready check!** {tags}"

        if is_refresh and msg:
            await msg.edit(embed=embed)
            await asyncio.to_thread(
                self.lobby_service.update_readycheck_data,
                current_lobby_set,
                player_data,
                guild_id=guild_id,
            )
            if invoker_id in current_lobby_set:
                await asyncio.to_thread(
                    self.lobby_service.add_readycheck_reaction,
                    invoker_id,
                    f"<@{invoker_id}>",
                    guild_id=guild_id,
                )
            if ping_content:
                await msg.channel.send(ping_content, allowed_mentions=allowed_mentions)
            return "ok", {
                "message_jump_url": msg.jump_url,
                "is_refresh": True,
                "pruned_count": 0,
            }

        # Post to lobby thread (target_channel is guaranteed to exist here)
        msg = await target_channel.send(embed=embed)
        try:
            await msg.add_reaction("✅")
        except Exception as e:
            logger.debug("Failed to add checkmark reaction: %s", e)
        if ping_content:
            await target_channel.send(ping_content, allowed_mentions=allowed_mentions)

        await asyncio.to_thread(
            self.lobby_service.set_readycheck_state,
            msg.id,
            msg.channel.id,
            current_lobby_set,
            player_data,
            guild_id=guild_id,
        )
        if invoker_id in current_lobby_set:
            await asyncio.to_thread(
                self.lobby_service.add_readycheck_reaction,
                invoker_id,
                f"<@{invoker_id}>",
                guild_id=guild_id,
            )
        if pruned_ids:
            note = (
                "🧹 Removed (away during ready check): "
                + " ".join(f"<@{pid}>" for pid in pruned_ids)
                + " — re-join with /join if you're back."
            )
            await target_channel.send(
                note,
                allowed_mentions=discord.AllowedMentions(
                    users=[discord.Object(id=pid) for pid in pruned_ids]
                ),
            )
        return "ok", {
            "message_jump_url": msg.jump_url,
            "is_refresh": False,
            "pruned_count": len(pruned_ids),
        }


def build_readycheck_embed(
    player_data: dict[int, dict],
    reacted: dict[int, str],
    ready_threshold: int = 10,
) -> tuple[discord.Embed, list[int]]:
    """Build the readycheck embed from stored classification data.

    Returns (embed, mention_ids) where mention_ids are all lobby members to ping.
    """
    now = time.time()
    count = len(player_data)
    description = f"**{count}** players in lobby"
    if count < ready_threshold:
        description += f" · need {ready_threshold - count} more for a full game"
    embed = discord.Embed(
        title="Ready Check",
        description=description,
        color=discord.Color.blue(),
    )

    active_lines: list[str] = []
    afk_lines: list[str] = []
    mention_ids: list[int] = []

    for pid, d in player_data.items():
        if pid in reacted:
            continue
        if d["is_member"]:
            mention_ids.append(pid)
        frogling = f" {FROGLING_EMOTE}" if d["is_conditional"] else ""
        join_ts = d.get("join_ts")
        time_str = f" ({format_duration_short(now - join_ts)})" if join_ts else ""
        if d["group"] == "active":
            active_lines.append(f"{d['name']} {d['signals']}{frogling}{time_str}")
        else:
            if d["is_member"]:
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
