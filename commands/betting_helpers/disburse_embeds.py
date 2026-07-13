"""Embed builders for the Jopacoin Reserve disbursement (`/disburse`) flow."""

from __future__ import annotations

import math

import discord

from utils.formatting import JOPACOIN_EMOTE

DISBURSE_VOTES_PAGE_SIZE = 15


def build_disburse_embed(proposal) -> discord.Embed:
    """Create embed for disbursement proposal."""
    votes = proposal.votes
    total_votes = proposal.total_votes
    quorum = proposal.quorum_required
    progress = proposal.quorum_progress

    embed = discord.Embed(
        title="🏛️ Jopacoin Reserve Allocation Vote",
        description=(
            f"Vote on how to allocate **{proposal.fund_amount}** {JOPACOIN_EMOTE} "
            "from the server operations budget.\n\n"
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
        name="🔥 Burn",
        value=f"Remove reserve funds\n**{votes.get('burn', 0)}** votes",
        inline=True,
    )
    embed.add_field(
        name="🎰 Next Match Pot",
        value=(
            "Split all funds evenly into the next betting pot\n"
            f"**{votes.get('next_match_pot', 0)}** votes"
        ),
        inline=True,
    )
    embed.add_field(
        name="❌ Cancel",
        value=f"Keep budget in reserve\n**{votes.get('cancel', 0)}** votes",
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


def build_disburse_votes_embed(
    proposal,
    disburse_service,
    individual_votes: list[dict],
    *,
    page: int = 0,
    page_size: int = DISBURSE_VOTES_PAGE_SIZE,
) -> discord.Embed:
    """Create Tax Man embed showing detailed voter information."""
    votes = proposal.votes
    total_votes = proposal.total_votes
    quorum = proposal.quorum_required
    progress = proposal.quorum_progress
    total_pages = max(1, math.ceil(len(individual_votes) / page_size))
    page = max(0, min(page, total_pages - 1))
    start = page * page_size
    end = start + page_size
    page_votes = individual_votes[start:end]

    embed = discord.Embed(
        title="🔍 Disbursement Vote Details (Tax Man)",
        description=f"Reserve Budget: **{proposal.fund_amount}** {JOPACOIN_EMOTE}",
        color=0x9C27B0,
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

    if page_votes:
        voter_lines = []
        for vote in page_votes:
            discord_id = vote["discord_id"]
            method = vote["vote_method"]
            method_label = disburse_service.METHOD_LABELS.get(method, method)
            voter_lines.append(f"• <@{discord_id}> → {method_label}")

        voters_text = "\n".join(voter_lines)
    else:
        voters_text = "*No votes yet*"

    if individual_votes:
        field_name = (
            f"👥 Individual Votes ({start + 1}-{min(end, len(individual_votes))} "
            f"of {len(individual_votes)})"
        )
    else:
        field_name = "👥 Individual Votes"
    embed.add_field(
        name=field_name,
        value=voters_text,
        inline=False,
    )

    embed.set_footer(
        text=f"Tax Man audit only | Page {page + 1}/{total_pages}"
    )

    return embed
