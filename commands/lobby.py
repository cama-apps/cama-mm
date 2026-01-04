"""
Lobby commands: /lobby, /kick.
"""

import logging

import discord
from discord import app_commands
from discord.ext import commands

from services.lobby_service import LobbyService
from services.permissions import has_admin_permission
from utils.interaction_safety import safe_defer

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

        # If message already exists, refresh it; otherwise create
        message_id = self.lobby_service.get_lobby_message_id()
        if message_id:
            try:
                message = await interaction.channel.fetch_message(message_id)
                await message.edit(embed=embed, allowed_mentions=discord.AllowedMentions.none())
                await interaction.followup.send(f"[View Lobby]({message.jump_url})", ephemeral=True)
                return
            except Exception:
                # Fall through to create a new one
                pass

        message = await interaction.followup.send(
            embed=embed,
            allowed_mentions=discord.AllowedMentions.none(),
        )
        self.lobby_service.set_lobby_message_id(message.id, interaction.channel.id)
        await self._safe_pin(message)
        try:
            await message.add_reaction("⚔️")
        except Exception:
            pass

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
                "❌ You can't kick yourself. Remove your reaction (⚔️) to leave instead.",
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
            message_id = self.lobby_service.get_lobby_message_id()
            if message_id:
                try:
                    message = await interaction.channel.fetch_message(message_id)
                    await message.remove_reaction("⚔️", player)
                except Exception as exc:
                    logger.warning(f"Failed to remove reaction for kicked player: {exc}")
            await self._update_lobby_message(interaction, lobby)
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
        if not await safe_defer(interaction, ephemeral=True):
            return

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

        await self._safe_unpin(interaction.channel, self.lobby_service.get_lobby_message_id())
        self.lobby_service.reset_lobby()
        await interaction.followup.send(
            "✅ Lobby reset. You can create a new lobby with `/lobby`.", ephemeral=True
        )


async def setup(bot: commands.Bot):
    lobby_service = getattr(bot, "lobby_service", None)
    player_service = getattr(bot, "player_service", None)
    await bot.add_cog(LobbyCommands(bot, lobby_service, player_service))
