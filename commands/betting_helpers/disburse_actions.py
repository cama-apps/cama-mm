"""Action handlers for the `/disburse` subcommands.

These are split out of the cog as free coroutines because they only need the
cog as a service container — they have no Discord-specific decorator state.
"""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING

import discord

from commands.betting_helpers.disburse_embeds import build_disburse_embed
from commands.betting_helpers.disburse_views import DisburseVotesView, DisburseVoteView
from services.permissions import has_tax_man_permission
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer

if TYPE_CHECKING:
    from commands.betting import BettingCommands

logger = logging.getLogger("cama_bot.commands.betting")


async def disburse_propose(
    cog: BettingCommands, interaction: discord.Interaction, guild_id: int | None
) -> None:
    """Create a new disbursement proposal."""
    can, reason = await asyncio.to_thread(cog.disburse_service.can_propose, guild_id)
    if not can:
        if reason == "active_proposal_exists":
            await interaction.response.send_message(
                "A disbursement vote is already active. Use `/disburse status` to see it.",
                ephemeral=True,
            )
        elif reason == cog.disburse_service.MONETARY_RECOVERY_CODE:
            await interaction.response.send_message(
                cog.disburse_service.MONETARY_RECOVERY_REASON,
                ephemeral=True,
            )
        elif reason.startswith("insufficient_fund:"):
            parts = reason.split(":")
            current = int(parts[1])
            needed = int(parts[2])
            await interaction.response.send_message(
                f"Insufficient funds. Current: **{current}** {JOPACOIN_EMOTE}, "
                f"minimum required: **{needed}** {JOPACOIN_EMOTE}",
                ephemeral=True,
            )
        elif reason == "no_debtors":
            await interaction.response.send_message(
                "No players with negative balance to receive funds.", ephemeral=True
            )
        else:
            await interaction.response.send_message(
                f"Cannot create proposal: {reason}", ephemeral=True
            )
        return

    try:
        proposal = await asyncio.to_thread(cog.disburse_service.create_proposal, guild_id)
    except ValueError as e:
        await interaction.response.send_message(str(e), ephemeral=True)
        return

    # Create embed and view
    embed = build_disburse_embed(proposal)
    view = DisburseVoteView(cog.disburse_service, cog)

    await interaction.response.send_message(embed=embed, view=view)

    # Store message ID for updates
    msg = await interaction.original_response()
    await asyncio.to_thread(
        cog.disburse_service.set_proposal_message,
        guild_id, msg.id, interaction.channel_id,
    )


async def disburse_status(
    cog: BettingCommands, interaction: discord.Interaction, guild_id: int | None
) -> None:
    """Show current proposal status, replacing the old message to keep it visible."""
    proposal = await asyncio.to_thread(cog.disburse_service.get_proposal, guild_id)
    if not proposal:
        if not getattr(cog.disburse_service, "voting_enabled", True):
            await interaction.response.send_message(
                cog.disburse_service.MONETARY_RECOVERY_REASON
                + " There is no active allocation ballot.",
                ephemeral=True,
            )
            return
        await interaction.response.send_message(
            "No active disbursement proposal. Use `/disburse propose` to create one.",
            ephemeral=True,
        )
        return

    # Delete the old message if it exists (to avoid it getting lost in chat)
    if proposal.message_id and proposal.channel_id:
        try:
            old_channel = cog.bot.get_channel(proposal.channel_id)
            if old_channel:
                old_message = await old_channel.fetch_message(proposal.message_id)
                if old_message:
                    await old_message.delete()
        except discord.errors.NotFound:
            pass  # Message already deleted
        except Exception as e:
            logger.warning(f"Failed to delete old disburse message: {e}")

    # Send new message with embed and voting buttons
    embed = build_disburse_embed(proposal)
    view = (
        DisburseVoteView(cog.disburse_service, cog)
        if getattr(cog.disburse_service, "voting_enabled", True)
        else None
    )
    content = (
        None
        if getattr(cog.disburse_service, "voting_enabled", True)
        else cog.disburse_service.MONETARY_RECOVERY_REASON
    )
    await interaction.response.send_message(content=content, embed=embed, view=view)

    # Update stored message reference to point to the new message
    msg = await interaction.original_response()
    await asyncio.to_thread(
        cog.disburse_service.set_proposal_message,
        guild_id, msg.id, interaction.channel_id,
    )


async def disburse_reset(
    cog: BettingCommands, interaction: discord.Interaction, guild_id: int | None
) -> None:
    """Reset (cancel) the active proposal. Tax Man only."""
    if not has_tax_man_permission(interaction):
        await interaction.response.send_message(
            "Only Tax Men can reset disbursement proposals.", ephemeral=True
        )
        return

    success = await asyncio.to_thread(cog.disburse_service.reset_proposal, guild_id)
    if success:
        await interaction.response.send_message(
            "Disbursement proposal has been reset.", ephemeral=False
        )
    else:
        await interaction.response.send_message(
            "No active proposal to reset.", ephemeral=True
        )


async def disburse_votes(
    cog: BettingCommands, interaction: discord.Interaction, guild_id: int | None
) -> None:
    """Show detailed voting information with voter identities. Tax Man only."""
    if not has_tax_man_permission(interaction):
        await interaction.response.send_message(
            "Only Tax Men can view detailed voting information.", ephemeral=True
        )
        return

    proposal = await asyncio.to_thread(cog.disburse_service.get_proposal, guild_id)
    if not proposal:
        await interaction.response.send_message(
            "No active disbursement proposal. Use `/disburse status` to check.",
            ephemeral=True,
        )
        return

    guild_key = proposal.guild_id if proposal.guild_id != 0 else None
    individual_votes = await asyncio.to_thread(
        cog.disburse_service.get_individual_votes, guild_key
    )
    view = DisburseVotesView(
        proposal=proposal,
        disburse_service=cog.disburse_service,
        individual_votes=individual_votes,
        requester_id=interaction.user.id,
    )
    embed = view.build_embed()
    await interaction.response.send_message(
        embed=embed,
        view=view if view.total_pages > 1 else None,
        ephemeral=True,
    )


async def disburse_execute(
    cog: BettingCommands, interaction: discord.Interaction, guild_id: int | None
) -> None:
    """Force-execute the active proposal using the current leading method. Tax Man only."""
    if not has_tax_man_permission(interaction):
        await interaction.response.send_message(
            "Only Tax Men can force-execute disbursement proposals.", ephemeral=True
        )
        return

    if not getattr(cog.disburse_service, "voting_enabled", True):
        await interaction.response.send_message(
            cog.disburse_service.MONETARY_RECOVERY_REASON,
            ephemeral=True,
        )
        return

    # Show current state before executing
    proposal = await asyncio.to_thread(cog.disburse_service.get_proposal, guild_id)
    if not proposal:
        await interaction.response.send_message(
            "No active disbursement proposal.", ephemeral=True
        )
        return

    if not await safe_defer(interaction):
        return

    try:
        disbursement = await asyncio.to_thread(
            cog.disburse_service.force_execute, guild_id
        )
    except ValueError as e:
        await interaction.followup.send(
            content=f"Cannot execute: {e}", ephemeral=True
        )
        return

    # Handle cancel
    if disbursement.get("cancelled"):
        embed = discord.Embed(
            title="❌ Proposal Cancelled (Tax Man)",
            description=disbursement.get("message", "Proposal cancelled."),
            color=0xFF6B6B,
        )
        await interaction.followup.send(embed=embed)
    elif disbursement["total_disbursed"] == 0 or disbursement.get("message"):
        embed = discord.Embed(
            title="💝 Disbursement Complete (Tax Man)",
            description=disbursement.get("message", "No funds were distributed."),
            color=0x00FF00,
        )
        await interaction.followup.send(embed=embed)
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

        embed = discord.Embed(
            title="💝 Disbursement Complete (Tax Man)",
            description=result_msg,
            color=0x00FF00,
        )
        embed.set_footer(text=f"Resolved by {interaction.user.display_name}")
        await interaction.followup.send(embed=embed)

    # Disable buttons on the original voting message
    try:
        if proposal.message_id and proposal.channel_id:
            channel = cog.bot.get_channel(proposal.channel_id)
            if channel:
                msg = await channel.fetch_message(proposal.message_id)
                disabled_view = discord.ui.View(timeout=None)
                for method in cog.disburse_service.METHODS:
                    label = cog.disburse_service.METHOD_LABELS[method]
                    emoji = {"even": "📊", "proportional": "📈", "neediest": "🎯",
                             "stimulus": "💸", "lottery": "🎲",
                             "social_security": "👴", "richest": "💎", "cancel": "❌"}.get(method)
                    style = discord.ButtonStyle.danger if method == "cancel" else discord.ButtonStyle.secondary
                    btn = discord.ui.Button(
                        label=label, emoji=emoji, style=style,
                        disabled=True, custom_id=f"disburse:{method}",
                    )
                    disabled_view.add_item(btn)
                await msg.edit(view=disabled_view)
    except Exception as e:
        logger.warning(f"Failed to disable vote buttons after force-execute: {e}")


async def update_disburse_message(
    cog: BettingCommands, guild_id: int | None
) -> None:
    """Update the disbursement proposal message with current vote counts."""
    proposal = await asyncio.to_thread(cog.disburse_service.get_proposal, guild_id)
    if not proposal or not proposal.message_id or not proposal.channel_id:
        return

    try:
        channel = cog.bot.get_channel(proposal.channel_id)
        if not channel:
            return

        message = await channel.fetch_message(proposal.message_id)
        if not message:
            return

        embed = build_disburse_embed(proposal)
        await message.edit(embed=embed)
    except discord.errors.NotFound:
        pass
    except Exception as e:
        logger.warning(f"Failed to update disburse message: {e}")
