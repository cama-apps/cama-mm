"""Discord UI views for the nonprofit fund disbursement (`/disburse`) flow."""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING

import discord

from utils.formatting import JOPACOIN_EMOTE

if TYPE_CHECKING:
    from commands.betting import BettingCommands
    from services.disburse_service import DisburseService

logger = logging.getLogger("cama_bot.commands.betting")


class DisburseVoteView(discord.ui.View):
    """Persistent view for disbursement voting."""

    def __init__(self, disburse_service: DisburseService, cog: BettingCommands):
        super().__init__(timeout=None)  # Persistent - no timeout
        self.disburse_service = disburse_service
        self.cog = cog

    async def _handle_vote(
        self, interaction: discord.Interaction, method: str, label: str
    ):
        """Handle a vote button press."""
        guild_id = interaction.guild.id if interaction.guild else None

        # Check if user is registered
        player = await asyncio.to_thread(self.cog.player_service.get_player, interaction.user.id, guild_id)
        if not player:
            await interaction.response.send_message(
                "You must be registered to vote. Use `/player register` first.",
                ephemeral=True,
            )
            return

        # Check for active proposal
        proposal = await asyncio.to_thread(self.disburse_service.get_proposal, guild_id)
        if not proposal:
            await interaction.response.send_message(
                "This vote has ended or been reset.", ephemeral=True
            )
            return

        try:
            result = await asyncio.to_thread(
                self.disburse_service.add_vote,
                guild_id, interaction.user.id, method,
            )
        except ValueError as e:
            await interaction.response.send_message(str(e), ephemeral=True)
            return

        # Check if quorum reached and execute
        if result["quorum_reached"]:
            # Execute disbursement
            try:
                disbursement = await asyncio.to_thread(self.disburse_service.execute_disbursement, guild_id)

                # Handle cancel specially
                if disbursement.get("cancelled"):
                    embed = discord.Embed(
                        title="❌ Proposal Cancelled",
                        description=disbursement.get("message", "Proposal cancelled by vote."),
                        color=0xFF6B6B,  # Red
                    )
                    await interaction.response.send_message(embed=embed)
                # Build result message
                elif disbursement["total_disbursed"] == 0:
                    result_msg = disbursement.get(
                        "message", "No funds were distributed."
                    )
                    embed = discord.Embed(
                        title="💝 Disbursement Complete!",
                        description=result_msg,
                        color=0x00FF00,  # Green
                    )
                    await interaction.response.send_message(embed=embed)
                else:
                    recipients = disbursement["distributions"]
                    recipient_lines = []
                    for discord_id, amount in recipients[:10]:
                        recipient_lines.append(f"<@{discord_id}>: +{amount}")
                    if len(recipients) > 10:
                        recipient_lines.append(f"...and {len(recipients) - 10} more")

                    result_msg = (
                        f"**{disbursement['total_disbursed']}** {JOPACOIN_EMOTE} "
                        f"distributed via **{disbursement['method_label']}** to "
                        f"{disbursement['recipient_count']} player(s):\n"
                        + "\n".join(recipient_lines)
                    )

                    # Send result as new message
                    embed = discord.Embed(
                        title="💝 Disbursement Complete!",
                        description=result_msg,
                        color=0x00FF00,  # Green
                    )
                    await interaction.response.send_message(embed=embed)

                # Disable buttons on the original message
                try:
                    if proposal.message_id and proposal.channel_id:
                        channel = self.cog.bot.get_channel(proposal.channel_id)
                        if channel:
                            msg = await channel.fetch_message(proposal.message_id)
                            # Create disabled view
                            disabled_view = discord.ui.View(timeout=None)
                            for item in self.children:
                                if isinstance(item, discord.ui.Button):
                                    new_btn = discord.ui.Button(
                                        label=item.label,
                                        emoji=item.emoji,
                                        style=discord.ButtonStyle.secondary,
                                        disabled=True,
                                        custom_id=item.custom_id,
                                    )
                                    disabled_view.add_item(new_btn)
                            await msg.edit(view=disabled_view)
                except Exception as e:
                    logger.warning(f"Failed to disable vote buttons: {e}")

            except ValueError as e:
                await interaction.response.send_message(
                    f"Disbursement failed: {e}", ephemeral=True
                )
        else:
            # Just acknowledge the vote
            await interaction.response.send_message(
                f"Your vote for **{label}** has been recorded! "
                f"({result['total_votes']}/{result['quorum_required']} for quorum)",
                ephemeral=True,
            )

            # Update the embed
            await self.cog.update_disburse_message(guild_id)

    @discord.ui.button(
        label="Even Split",
        emoji="📊",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:even",
    )
    async def vote_even(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "even", "Even Split")

    @discord.ui.button(
        label="Proportional",
        emoji="📈",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:proportional",
    )
    async def vote_proportional(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "proportional", "Proportional")

    @discord.ui.button(
        label="Neediest First",
        emoji="🎯",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:neediest",
    )
    async def vote_neediest(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "neediest", "Neediest First")

    @discord.ui.button(
        label="Stimulus",
        emoji="💸",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:stimulus",
    )
    async def vote_stimulus(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "stimulus", "Stimulus")

    @discord.ui.button(
        label="Lottery",
        emoji="🎲",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:lottery",
    )
    async def vote_lottery(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "lottery", "Lottery")

    @discord.ui.button(
        label="Social Security",
        emoji="👴",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:social_security",
    )
    async def vote_social_security(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "social_security", "Social Security")

    @discord.ui.button(
        label="Richest",
        emoji="💎",
        style=discord.ButtonStyle.primary,
        custom_id="disburse:richest",
    )
    async def vote_richest(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "richest", "Richest")

    @discord.ui.button(
        label="Cancel",
        emoji="❌",
        style=discord.ButtonStyle.danger,
        custom_id="disburse:cancel",
    )
    async def vote_cancel(
        self, interaction: discord.Interaction, button: discord.ui.Button
    ):
        await self._handle_vote(interaction, "cancel", "Cancel")
