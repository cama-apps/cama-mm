"""
Lobby commands: /lobby, /kick, /resetlobby.

Uses Discord threads for lobby management similar to /prediction.
"""

import logging
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from services.lobby_service import LobbyService
from services.permissions import has_admin_permission
from utils.interaction_safety import safe_defer

if TYPE_CHECKING:
    from services.player_service import PlayerService

logger = logging.getLogger("cama_bot.commands.lobby")


class LockedLobbyView(discord.ui.View):
    """View with disabled buttons shown after shuffle."""

    def __init__(self):
        super().__init__(timeout=None)
        # Add disabled buttons
        join_btn = discord.ui.Button(
            label="Join",
            emoji="✅",
            style=discord.ButtonStyle.secondary,
            disabled=True,
            custom_id="lobby:join:disabled",
        )
        leave_btn = discord.ui.Button(
            label="Leave",
            emoji="🚪",
            style=discord.ButtonStyle.secondary,
            disabled=True,
            custom_id="lobby:leave:disabled",
        )
        self.add_item(join_btn)
        self.add_item(leave_btn)


class PersistentLobbyView(discord.ui.View):
    """
    Persistent view that handles lobby join/leave button interactions.

    Registered once on bot startup and handles buttons by custom_id.
    """

    def __init__(self, cog: "LobbyCommands"):
        super().__init__(timeout=None)
        self.cog = cog

    @discord.ui.button(
        label="Join",
        emoji="✅",
        style=discord.ButtonStyle.success,
        custom_id="lobby:join",
    )
    async def join_button(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_join(interaction)

    @discord.ui.button(
        label="Leave",
        emoji="🚪",
        style=discord.ButtonStyle.danger,
        custom_id="lobby:leave",
    )
    async def leave_button(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_leave(interaction)

    async def _handle_join(self, interaction: discord.Interaction):
        """Handle join button press inside thread."""
        # Defer first to avoid timeout - ignore errors if already timed out
        try:
            await interaction.response.defer(ephemeral=True)
        except discord.NotFound:
            pass  # Interaction timed out, continue anyway

        # Check for pending match - no joining after shuffle
        match_service = getattr(self.cog.bot, "match_service", None)
        guild_id = interaction.guild.id if interaction.guild else None
        if match_service and match_service.get_last_shuffle(guild_id):
            try:
                await interaction.followup.send(
                    "❌ Teams have been shuffled! Cannot join now.",
                    ephemeral=True,
                )
            except Exception:
                pass
            return

        player = self.cog.player_service.get_player(interaction.user.id)
        if not player:
            try:
                await interaction.followup.send(
                    "❌ You're not registered! Use `/register` first.",
                    ephemeral=True,
                )
            except Exception:
                pass
            return

        if not player.preferred_roles:
            try:
                await interaction.followup.send(
                    "❌ Set your preferred roles first! Use `/setroles`.",
                    ephemeral=True,
                )
            except Exception:
                pass
            return

        success, reason = self.cog.lobby_service.join_lobby(interaction.user.id)
        if not success:
            try:
                await interaction.followup.send(f"❌ {reason}", ephemeral=True)
            except Exception:
                pass
            return

        lobby = self.cog.lobby_service.get_lobby()
        if lobby:
            # Update thread embed and channel message
            await self.cog._sync_lobby_displays(lobby)

            # Post activity message in thread (mentions user to subscribe them)
            thread_id = self.cog.lobby_service.get_lobby_thread_id()
            if thread_id:
                await self.cog._post_join_activity(thread_id, interaction.user)

    async def _handle_leave(self, interaction: discord.Interaction):
        """Handle leave button press inside thread."""
        # Defer first to avoid timeout - ignore errors if already timed out
        try:
            await interaction.response.defer(ephemeral=True)
        except discord.NotFound:
            pass  # Interaction timed out, continue anyway

        # Check for pending match - no leaving after shuffle
        match_service = getattr(self.cog.bot, "match_service", None)
        guild_id = interaction.guild.id if interaction.guild else None
        if match_service and match_service.get_last_shuffle(guild_id):
            try:
                await interaction.followup.send(
                    "❌ Teams have been shuffled! Cannot leave now.",
                    ephemeral=True,
                )
            except Exception:
                pass
            return

        if self.cog.lobby_service.leave_lobby(interaction.user.id):
            lobby = self.cog.lobby_service.get_lobby()
            if lobby:
                await self.cog._sync_lobby_displays(lobby)

                thread_id = self.cog.lobby_service.get_lobby_thread_id()
                if thread_id:
                    await self.cog._post_leave_activity(thread_id, interaction.user)
        else:
            try:
                await interaction.followup.send(
                    "❌ You're not in the lobby.", ephemeral=True
                )
            except Exception:
                pass


class LobbyCommands(commands.Cog):
    """Slash commands for lobby management."""

    def __init__(self, bot: commands.Bot, lobby_service: LobbyService, player_service):
        self.bot = bot
        self.lobby_service = lobby_service
        self.player_service = player_service

    async def _safe_pin(self, message: discord.Message) -> None:
        """Pin the lobby message, logging but not raising on failure (e.g., missing perms)."""
        try:
            await message.pin(reason="Cama lobby active")
        except discord.Forbidden:
            logger.warning("Cannot pin lobby message: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to pin lobby message: {exc}")

    async def _safe_unpin(
        self, channel: discord.abc.Messageable | None, message_id: int | None
    ) -> None:
        """Unpin the lobby message safely, tolerating missing perms or missing message."""
        if not channel or not message_id:
            return
        try:
            message = await channel.fetch_message(message_id)
        except Exception as exc:
            logger.warning(f"Failed to fetch lobby message for unpin: {exc}")
            return

        try:
            await message.unpin(reason="Cama lobby closed")
        except discord.Forbidden:
            logger.warning("Cannot unpin lobby message: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to unpin lobby message: {exc}")

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

    def _create_lobby_view(self) -> PersistentLobbyView:
        """Create a persistent lobby view for buttons."""
        return PersistentLobbyView(self)

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

        lobby = self.lobby_service.get_or_create_lobby(creator_id=interaction.user.id)
        embed = self.lobby_service.build_lobby_embed(lobby)

        # If message/thread already exists, refresh it; otherwise create new
        message_id = self.lobby_service.get_lobby_message_id()
        thread_id = self.lobby_service.get_lobby_thread_id()

        if message_id and thread_id:
            try:
                # Just update the thread embed (channel message doesn't have embed)
                message = await interaction.channel.fetch_message(message_id)
                await self._update_thread_embed(lobby)

                await interaction.followup.send(
                    f"[View Lobby]({message.jump_url})", ephemeral=True
                )
                return
            except Exception:
                # Fall through to create a new one
                pass

        # Send channel message with embed (same as thread, but no buttons)
        channel_msg = await interaction.channel.send(embed=embed)

        # Create thread from message (static name to avoid rate limits)
        try:
            thread_name = "🎮 Matchmaking Lobby"
            thread = await channel_msg.create_thread(name=thread_name)

            # Send only buttons in thread (channel message already has embed as thread starter)
            view = self._create_lobby_view()
            button_msg = await thread.send("**Join or leave using the buttons below:**", view=view)

            # Store all IDs (embed is on channel_msg, which is also the thread starter)
            self.lobby_service.set_lobby_message_id(
                message_id=channel_msg.id,
                channel_id=interaction.channel.id,
                thread_id=thread.id,
                embed_message_id=channel_msg.id,  # The channel msg IS the embed in thread
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

        if player.id not in lobby.players:
            await interaction.followup.send(
                f"⚠️ {player.mention} is not in the lobby.", ephemeral=True
            )
            return

        removed = self.lobby_service.leave_lobby(player.id)
        if removed:
            await interaction.followup.send(
                f"✅ Kicked {player.mention} from the lobby.", ephemeral=True
            )

            # Update both channel message and thread embed
            await self._sync_lobby_displays(lobby)

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
                        message_text += f" [View pending match]({jump_url}) then use `/record` first."
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

        # Update channel message to show closed and archive thread
        await self._update_channel_message_closed("Lobby Reset")
        await self._archive_lobby_thread("Lobby Reset")

        await self._safe_unpin(interaction.channel, self.lobby_service.get_lobby_message_id())
        self.lobby_service.reset_lobby()
        logger.info(f"Lobby reset by user {interaction.user.id}")
        if can_respond:
            await interaction.followup.send(
                "✅ Lobby reset. You can create a new lobby with `/lobby`.", ephemeral=True
            )


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    player_service = getattr(bot, "player_service", None)
    cog = LobbyCommands(bot, lobby_service, player_service)
    await bot.add_cog(cog)

    # Register persistent view for lobby buttons
    bot.add_view(PersistentLobbyView(cog))
