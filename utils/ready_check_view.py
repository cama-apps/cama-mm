"""
ReadyCheckView: Discord UI view with persistent buttons for ready checks.

Provides:
- "Ready" button for all players to confirm readiness
- "Remove AFK" button for admins and designated player to remove inactive players
"""

import logging

import discord

from services.permissions import has_admin_permission

logger = logging.getLogger("cama_bot.utils.ready_check_view")


class ReadyCheckView(discord.ui.View):
    """Persistent view for ready check with Ready and Remove AFK buttons."""

    def __init__(self, cog):
        """
        Initialize persistent ready check view.

        Args:
            cog: The LobbyCommands cog instance for callback access
        """
        super().__init__(timeout=None)  # Persistent view, no timeout
        self.cog = cog

    @discord.ui.button(
        label="I'm Ready!",
        emoji="✅",
        style=discord.ButtonStyle.success,
        custom_id="readycheck:ready",
    )
    async def ready_button(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        """
        Handle Ready button click.

        Marks player as ready and updates the embed to reflect the change.
        """
        await interaction.response.defer(ephemeral=True)

        try:
            guild_id = interaction.guild.id if interaction.guild else 0
            player_id = interaction.user.id

            # Get ready check service
            ready_check_service = getattr(self.cog.bot, "ready_check_service", None)
            if not ready_check_service:
                await interaction.followup.send(
                    "❌ Ready check service not available.", ephemeral=True
                )
                return

            # Get current state
            state = ready_check_service.get_state(guild_id)
            if not state:
                await interaction.followup.send(
                    "❌ No active ready check in this server.", ephemeral=True
                )
                return

            # Check if player is in lobby
            if player_id not in state.total_players:
                await interaction.followup.send(
                    "❌ You are not in the current lobby.", ephemeral=True
                )
                return

            # Mark player as ready
            success = ready_check_service.mark_ready(guild_id, player_id)
            if success:
                await interaction.followup.send("✅ Marked as ready!", ephemeral=True)
                logger.info(f"Player {player_id} marked ready in guild {guild_id}")

                # Update the embed if possible (cog should have method for this)
                if hasattr(self.cog, "update_ready_check_embed"):
                    try:
                        await self.cog.update_ready_check_embed(guild_id)
                    except Exception as exc:
                        logger.warning(f"Failed to update embed: {exc}")
            else:
                await interaction.followup.send(
                    "❌ Failed to mark as ready.", ephemeral=True
                )

        except Exception as exc:
            logger.error(f"Error in ready_button: {exc}", exc_info=True)
            await interaction.followup.send(
                "❌ An error occurred. Please try again.", ephemeral=True
            )

    @discord.ui.button(
        label="Remove AFK",
        emoji="🚫",
        style=discord.ButtonStyle.danger,
        custom_id="readycheck:remove_afk",
    )
    async def remove_afk_button(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        """
        Handle Remove AFK button click.

        Only admins or the designated player can use this button.
        Shows a dropdown of AFK players and allows removal from lobby.
        """
        await interaction.response.defer(ephemeral=True)

        try:
            guild_id = interaction.guild.id if interaction.guild else 0
            user_id = interaction.user.id

            # Get ready check service
            ready_check_service = getattr(self.cog.bot, "ready_check_service", None)
            if not ready_check_service:
                await interaction.followup.send(
                    "❌ Ready check service not available.", ephemeral=True
                )
                return

            # Get current state
            state = ready_check_service.get_state(guild_id)
            if not state:
                await interaction.followup.send(
                    "❌ No active ready check in this server.", ephemeral=True
                )
                return

            # Check permissions: Admin OR Designated Player
            is_admin = has_admin_permission(user_id, guild_id)
            is_designated = state.designated_player_id == user_id

            if not (is_admin or is_designated):
                await interaction.followup.send(
                    "❌ Only admins or the designated player can remove AFK players.",
                    ephemeral=True,
                )
                return

            # Get AFK players (no ready confirmation AND no activity)
            # We need to check activity via the cog's AFK detection service
            afk_detection_service = getattr(self.cog.bot, "afk_detection_service", None)
            lobby_service = getattr(self.cog.bot, "lobby_service", None)

            if not afk_detection_service or not lobby_service:
                await interaction.followup.send(
                    "❌ Required services not available.", ephemeral=True
                )
                return

            # Get lobby to check message/thread IDs
            lobby_manager = lobby_service.lobby_manager
            lobby = lobby_manager.get_lobby()
            if not lobby:
                await interaction.followup.send(
                    "❌ No active lobby found.", ephemeral=True
                )
                return

            # Find AFK players: no ready AND no activity
            afk_players = []
            for player_id in state.total_players:
                # Check if player clicked ready
                if player_id in state.ready_players:
                    continue

                # Check if player has activity signals
                # Note: We'd need to store the latest activity check results
                # For now, just check ready status
                # TODO: Implement activity check caching or pass activity results
                afk_players.append(player_id)

            if not afk_players:
                await interaction.followup.send(
                    "✅ No AFK players detected. Everyone is either ready or showing activity!",
                    ephemeral=True,
                )
                return

            # Show selection dropdown for AFK players
            # For simplicity, we'll show a message listing AFK players
            # A full implementation would use a Select menu
            afk_mentions = [f"<@{pid}>" for pid in afk_players[:10]]
            await interaction.followup.send(
                f"⚠️ **AFK Players ({len(afk_players)}):**\n"
                + "\n".join(afk_mentions)
                + "\n\n*Full AFK removal with selection dropdown coming soon.*\n"
                + "*For now, use `/kick` command to remove specific players.*",
                ephemeral=True,
            )

            logger.info(
                f"User {user_id} viewed AFK list in guild {guild_id}: {len(afk_players)} AFK"
            )

        except Exception as exc:
            logger.error(f"Error in remove_afk_button: {exc}", exc_info=True)
            await interaction.followup.send(
                "❌ An error occurred. Please try again.", ephemeral=True
            )
