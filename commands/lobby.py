"""
Lobby commands: /lobby, /kick, /resetlobby, /rc.

Uses Discord threads for lobby management similar to /prediction.
"""

import asyncio
import logging
from typing import TYPE_CHECKING

import discord
from discord import app_commands
from discord.ext import commands

from config import AFK_CHECK_ACTIVITY_WINDOW_SECONDS, AFK_CHECK_DEFAULT_WAIT_TIME
from services.lobby_service import LobbyService
from services.permissions import has_admin_permission
from utils.formatting import get_player_display_name
from utils.interaction_safety import safe_defer

if TYPE_CHECKING:
    from services.player_service import PlayerService

logger = logging.getLogger("cama_bot.commands.lobby")


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

    async def _remove_user_sword_reaction(self, user: discord.User | discord.Member) -> None:
        """Remove a user's sword reaction from the channel lobby message."""
        message_id = self.lobby_service.get_lobby_message_id()
        channel_id = self.lobby_service.get_lobby_channel_id()
        if not message_id or not channel_id:
            return

        try:
            channel = self.bot.get_channel(channel_id)
            if not channel:
                channel = await self.bot.fetch_channel(channel_id)
            message = await channel.fetch_message(message_id)
            await message.remove_reaction("⚔️", user)
        except discord.Forbidden:
            logger.warning("Cannot remove reaction: missing Manage Messages permission.")
        except Exception as exc:
            logger.warning(f"Failed to remove user sword reaction: {exc}")

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

        # Pin the lobby message for visibility
        await self._safe_pin(channel_msg)

        # Add sword emoji for reaction-based joining
        try:
            await channel_msg.add_reaction("⚔️")
        except Exception:
            pass

        # Create thread from message (static name to avoid rate limits)
        try:
            thread_name = "🎮 Matchmaking Lobby"
            thread = await channel_msg.create_thread(name=thread_name)

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

            # Remove kicked player's sword reaction
            await self._remove_user_sword_reaction(player)

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

        # Cancel any active ready check
        ready_check_service = getattr(self.bot, "ready_check_service", None)
        if ready_check_service:
            ready_check_service.cancel_check(guild_id)
            ready_check_service.clear_message_id(guild_id)
            logger.info(f"Cancelled ready check for guild {guild_id} during lobby reset")

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

    @app_commands.command(
        name="rc",
        description="Check which lobby players might be AFK based on activity signals",
    )
    @app_commands.describe(
        wait_time="Seconds to wait before final report (default: 30s, max: 60s)"
    )
    async def ready_check(
        self, interaction: discord.Interaction, wait_time: int = AFK_CHECK_DEFAULT_WAIT_TIME
    ):
        """
        Run an activity-based ready check to detect AFK players.

        Checks for multiple activity signals:
        - Discord online/DND status
        - In voice channel (not deafened)
        - Recent messages in lobby thread
        - Recent ⚔️ reactions on lobby message
        - Typing indicator

        Players with no activity signals are pinged, then re-checked after wait_time.
        """
        logger.info(f"/rc command invoked by user {interaction.user.id}, wait_time={wait_time}")

        if not await safe_defer(interaction, ephemeral=True):
            return

        # Validate wait time
        if wait_time < 10 or wait_time > 60:
            await interaction.followup.send(
                "❌ Wait time must be between 10 and 60 seconds.", ephemeral=True
            )
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

        # Get AFK detection service
        afk_service = getattr(self.bot, "afk_detection_service", None)
        if not afk_service:
            await interaction.followup.send(
                "❌ AFK detection service not available.", ephemeral=True
            )
            return

        # Get lobby message and thread
        lobby_message_id = self.lobby_service.get_lobby_message_id()
        thread_id = self.lobby_service.get_lobby_thread_id()
        lobby_thread = None

        if thread_id and guild:
            try:
                lobby_thread = await self.bot.fetch_channel(thread_id)
            except Exception as exc:
                logger.warning(f"Could not fetch lobby thread {thread_id}: {exc}")

        # Initial activity check
        await interaction.followup.send("🔍 Checking player activity...", ephemeral=True)

        activity_results = {}
        for pid in player_ids:
            status = await afk_service.check_player_activity(
                player_id=pid,
                guild=guild,
                lobby_message_id=lobby_message_id,
                lobby_thread=lobby_thread,
                activity_window_seconds=AFK_CHECK_ACTIVITY_WINDOW_SECONDS,
            )
            activity_results[pid] = status

        # Identify AFK players
        afk_players = [pid for pid, status in activity_results.items() if not status.is_active]
        active_players = [pid for pid, status in activity_results.items() if status.is_active]

        # If everyone is active, report immediately
        if not afk_players:
            embed = self._build_activity_report_embed(
                activity_results, players, player_ids, guild, all_active=True
            )
            if lobby_thread:
                await lobby_thread.send(embed=embed)
            else:
                await interaction.channel.send(embed=embed)
            return

        # Ping AFK players and wait
        if lobby_thread:
            afk_mentions = " ".join(f"<@{pid}>" for pid in afk_players)
            await lobby_thread.send(
                f"⚠️ **AFK Check:** {afk_mentions} - Please confirm you're here! "
                f"Checking again in {wait_time} seconds..."
            )

        await interaction.followup.send(
            f"⏳ Pinged {len(afk_players)} potentially AFK players. "
            f"Waiting {wait_time} seconds...",
            ephemeral=True,
        )

        # Wait before re-checking
        await asyncio.sleep(wait_time)

        # Re-check activity
        final_results = {}
        for pid in player_ids:
            status = await afk_service.check_player_activity(
                player_id=pid,
                guild=guild,
                lobby_message_id=lobby_message_id,
                lobby_thread=lobby_thread,
                activity_window_seconds=AFK_CHECK_ACTIVITY_WINDOW_SECONDS,
            )
            final_results[pid] = status

        # Build and send final report
        embed = self._build_activity_report_embed(
            final_results, players, player_ids, guild, all_active=False
        )

        if lobby_thread:
            await lobby_thread.send(embed=embed)
        else:
            await interaction.channel.send(embed=embed)

        logger.info(f"Ready check completed for guild {guild_id}")

    def _build_activity_report_embed(
        self,
        activity_results: dict,
        players: list,
        player_ids: list[int],
        guild: discord.Guild | None,
        all_active: bool = False,
    ) -> discord.Embed:
        """Build the activity report embed."""
        active_players = [
            (pid, status)
            for pid, status in activity_results.items()
            if status.is_active
        ]
        afk_players = [
            (pid, status)
            for pid, status in activity_results.items()
            if not status.is_active
        ]

        total = len(activity_results)
        active_count = len(active_players)
        afk_count = len(afk_players)

        if all_active:
            embed = discord.Embed(
                title="✅ All Players Active!",
                description=f"All {total} players in lobby are showing activity.",
                color=discord.Color.green(),
            )
        else:
            embed = discord.Embed(
                title="📋 Ready Check Results",
                description=f"Activity check complete: {active_count}/{total} active",
                color=discord.Color.blue() if afk_count == 0 else discord.Color.orange(),
            )

        # Active players section
        if active_players:
            active_lines = []
            for pid, status in active_players[:25]:  # Discord limit
                player = next((p for p in players if p.discord_id == pid), None)
                if player:
                    display_name = get_player_display_name(player, pid, guild)
                    afk_service = getattr(self.bot, "afk_detection_service", None)
                    signals_str = afk_service.format_activity_status(status) if afk_service else ""
                    active_lines.append(f"• {display_name} {signals_str}")

            embed.add_field(
                name=f"✅ Active Players ({len(active_players)})",
                value="\n".join(active_lines) if active_lines else "None",
                inline=False,
            )

        # AFK players section
        if afk_players:
            afk_lines = []
            for pid, status in afk_players[:25]:  # Discord limit
                player = next((p for p in players if p.discord_id == pid), None)
                if player:
                    display_name = get_player_display_name(player, pid, guild)
                    afk_service = getattr(self.bot, "afk_detection_service", None)
                    signals_str = afk_service.format_activity_status(status) if afk_service else ""
                    afk_lines.append(f"• {display_name} {signals_str}")

            embed.add_field(
                name=f"⚠️ Potentially AFK ({len(afk_players)})",
                value="\n".join(afk_lines) if afk_lines else "None",
                inline=False,
            )

        embed.set_footer(
            text="Activity signals: 🟢 online | 🎙️ voice | 💬 message | ⚔️ reaction | ⌨️ typing"
        )

        return embed


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    player_service = getattr(bot, "player_service", None)
    cog = LobbyCommands(bot, lobby_service, player_service)
    await bot.add_cog(cog)
