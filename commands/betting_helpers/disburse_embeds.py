"""Embed builders for the nonprofit fund disbursement (`/disburse`) flow."""

from __future__ import annotations

import asyncio

import discord

from utils.formatting import JOPACOIN_EMOTE


def build_disburse_embed(proposal) -> discord.Embed:
    """Create embed for disbursement proposal."""
    votes = proposal.votes
    total_votes = proposal.total_votes
    quorum = proposal.quorum_required
    progress = proposal.quorum_progress

    embed = discord.Embed(
        title="💝 Nonprofit Fund Disbursement Vote",
        description=(
            f"Vote on how to distribute **{proposal.fund_amount}** {JOPACOIN_EMOTE}.\n\n"
            "Click a button below to vote!"
        ),
        color=0xE91E63,  # Pink
    )

    # Voting options with counts
    embed.add_field(
        name="📊 Even Split",
        value=f"Split equally to debtors\n**{votes['even']}** votes",
        inline=True,
    )
    embed.add_field(
        name="📈 Proportional",
        value=f"More debt = more funds\n**{votes['proportional']}** votes",
        inline=True,
    )
    embed.add_field(
        name="🎯 Neediest First",
        value=f"All to most indebted\n**{votes['neediest']}** votes",
        inline=True,
    )
    embed.add_field(
        name="💸 Stimulus",
        value=f"Even split to active non-top-3\n**{votes['stimulus']}** votes",
        inline=True,
    )
    embed.add_field(
        name="🎲 Lottery",
        value=f"Random active player wins all\n**{votes.get('lottery', 0)}** votes",
        inline=True,
    )
    embed.add_field(
        name="👴 Social Security",
        value=f"By games played (excl. top 3)\n**{votes.get('social_security', 0)}** votes",
        inline=True,
    )
    embed.add_field(
        name="💎 Richest",
        value=f"All to the richest player\n**{votes.get('richest', 0)}** votes",
        inline=True,
    )
    embed.add_field(
        name="❌ Cancel",
        value=f"Keep funds in nonprofit\n**{votes.get('cancel', 0)}** votes",
        inline=True,
    )

    # Progress bar
    bar_length = 20
    filled = int(progress * bar_length)
    bar = "█" * filled + "░" * (bar_length - filled)
    embed.add_field(
        name="Quorum Progress",
        value=f"`{bar}` {total_votes}/{quorum} ({int(progress * 100)}%)",
        inline=False,
    )

    if proposal.quorum_reached:
        embed.add_field(
            name="✅ Quorum Reached!",
            value="The next vote will trigger automatic disbursement.",
            inline=False,
        )

    embed.set_footer(text="Ties are broken in favor of Even Split")

    return embed


async def build_disburse_votes_embed(proposal, disburse_service) -> discord.Embed:
    """Create admin-only embed showing detailed voter information."""
    votes = proposal.votes
    total_votes = proposal.total_votes
    quorum = proposal.quorum_required
    progress = proposal.quorum_progress

    embed = discord.Embed(
        title="🔍 Disbursement Vote Details (Admin Only)",
        description=f"Fund Amount: **{proposal.fund_amount}** {JOPACOIN_EMOTE}",
        color=0x9C27B0,  # Purple (admin color)
    )

    # Proposal info
    embed.add_field(
        name="📋 Proposal Status",
        value=(
            f"**Quorum:** {total_votes}/{quorum} ({int(progress * 100)}%)\n"
            f"**Status:** {'✅ Ready' if proposal.quorum_reached else '⏳ Voting'}"
        ),
        inline=False,
    )

    # Vote breakdown
    vote_lines = []
    for method in disburse_service.METHODS:
        count = votes.get(method, 0)
        pct = (count / total_votes * 100) if total_votes > 0 else 0
        label = disburse_service.METHOD_LABELS[method]
        vote_lines.append(f"**{label}:** {count} ({pct:.0f}%)")

    embed.add_field(
        name="📊 Vote Breakdown",
        value="\n".join(vote_lines),
        inline=False,
    )

    # Individual votes
    guild_id = proposal.guild_id if proposal.guild_id != 0 else None
    individual_votes = await asyncio.to_thread(
        disburse_service.get_individual_votes, guild_id
    )

    if individual_votes:
        voter_lines = []
        for vote in individual_votes:
            discord_id = vote["discord_id"]
            method = vote["vote_method"]
            method_label = disburse_service.METHOD_LABELS.get(method, method)
            voter_lines.append(f"• <@{discord_id}> → {method_label}")

        voters_text = "\n".join(voter_lines)
    else:
        voters_text = "*No votes yet*"

    # Truncate if too long (Discord field limit is 1024 chars)
    if len(voters_text) > 1024:
        voters_text = voters_text[:1021] + "..."

    embed.add_field(
        name="👥 Individual Votes",
        value=voters_text,
        inline=False,
    )

    embed.set_footer(text="This information is only visible to you")

    return embed
