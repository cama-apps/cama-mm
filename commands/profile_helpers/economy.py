"""Economy tab builder: balance/loans/bankruptcy fields plus the balance-history chart."""

from __future__ import annotations

import asyncio
import functools
import logging
from typing import TYPE_CHECKING

import discord

from config import BANKRUPTCY_PENALTY_RATE
from utils.drawing import draw_balance_chart
from utils.drawing.balance_history import SOURCE_LABELS
from utils.formatting import JOPACOIN_EMOTE, TOMBSTONE_EMOJI

if TYPE_CHECKING:
    from commands.profile import ProfileCommands

logger = logging.getLogger("cama_bot.commands.profile.economy")

COLOR_GREEN = 0x57F287
COLOR_RED = 0xED4245
COLOR_ORANGE = 0xF39C12


async def build_economy_embed(
    cog: ProfileCommands,
    target_user: discord.Member | discord.User,
    target_discord_id: int,
    guild_id: int | None = None,
) -> tuple[discord.Embed, discord.File | None]:
    """Build the Economy tab embed with balance, loans, bankruptcy, and balance chart.

    Returns ``(embed, chart_file)``. ``chart_file`` is ``None`` when the player has
    no chartable balance events — the tab then renders identically to the pre-chart
    behaviour.
    """
    player_repo = cog._get_player_repo()
    loan_service = cog._get_loan_service()
    bankruptcy_service = cog._get_bankruptcy_service()

    if not player_repo:
        return (
            discord.Embed(title="Error", description="Player repository unavailable", color=COLOR_RED),
            None,
        )

    player = player_repo.get_by_id(target_discord_id, guild_id)
    if not player:
        return (
            discord.Embed(
                title="Not Registered",
                description=f"{target_user.display_name} is not registered.",
                color=COLOR_RED,
            ),
            None,
        )

    balance = player.jopacoin_balance or 0

    if balance > 0:
        color = COLOR_GREEN
    elif balance < 0:
        color = COLOR_RED
    else:
        color = COLOR_ORANGE

    embed = discord.Embed(
        title=f"Profile: {target_user.display_name} > Economy",
        color=color,
    )

    balance_emoji = "💰" if balance > 0 else "⚠️" if balance < 0 else "📭"
    embed.add_field(
        name=f"{balance_emoji} Balance",
        value=f"**{balance}** {JOPACOIN_EMOTE}",
        inline=False,
    )

    if loan_service:
        loan_state = loan_service.get_state(target_discord_id, guild_id)

        loan_lines = [
            f"**Loans Taken:** {loan_state.total_loans_taken}",
            f"**Fees Paid:** {loan_state.total_fees_paid} {JOPACOIN_EMOTE}",
        ]

        if loan_state.negative_loans_taken > 0:
            loan_lines.append(f"🔥 **Borrowed While Broke:** {loan_state.negative_loans_taken}x")

        if loan_state.has_outstanding_loan:
            loan_lines.append("\n⚠️ **Outstanding Loan:**")
            loan_lines.append(f"  Principal: {loan_state.outstanding_principal} {JOPACOIN_EMOTE}")
            loan_lines.append(f"  Fee: {loan_state.outstanding_fee} {JOPACOIN_EMOTE}")
            loan_lines.append(f"  **Total Owed:** {loan_state.outstanding_total} {JOPACOIN_EMOTE}")
            loan_lines.append("  *(Repaid on next match)*")

        if loan_state.is_on_cooldown and loan_state.cooldown_ends_at:
            loan_lines.append(f"\n⏳ **Cooldown:** <t:{loan_state.cooldown_ends_at}:R>")
        elif not loan_state.has_outstanding_loan:
            loan_lines.append("\n✅ **Loan Available**")

        embed.add_field(name="🏦 Loans", value="\n".join(loan_lines), inline=True)

    if bankruptcy_service:
        bankruptcy_repo = bankruptcy_service.bankruptcy_repo
        state_data = bankruptcy_repo.get_state(target_discord_id, guild_id)
        bankruptcy_state = bankruptcy_service.get_state(target_discord_id, guild_id)

        bankruptcy_lines = []
        bankruptcy_count = state_data["bankruptcy_count"] if state_data else 0
        bankruptcy_lines.append(f"**Declarations:** {bankruptcy_count}")

        if bankruptcy_state.penalty_games_remaining > 0:
            penalty_rate_pct = int(BANKRUPTCY_PENALTY_RATE * 100)
            bankruptcy_lines.append(f"\n{TOMBSTONE_EMOJI} **Active Penalty:**")
            bankruptcy_lines.append(f"  {penalty_rate_pct}% win bonus")
            bankruptcy_lines.append(f"  {bankruptcy_state.penalty_games_remaining} game(s) remaining")

        if bankruptcy_state.is_on_cooldown and bankruptcy_state.cooldown_ends_at:
            bankruptcy_lines.append(f"\n⏳ **Cooldown:** <t:{bankruptcy_state.cooldown_ends_at}:R>")
        elif balance < 0:
            bankruptcy_lines.append("\n⚠️ **Bankruptcy Available**")

        embed.add_field(
            name=f"{TOMBSTONE_EMOJI} Bankruptcy",
            value="\n".join(bankruptcy_lines),
            inline=True,
        )

    lowest_balance = player_repo.get_lowest_balance(target_discord_id, guild_id)
    if lowest_balance is not None and lowest_balance < 0:
        embed.add_field(
            name="📉 Lowest Balance",
            value=f"**{lowest_balance}** {JOPACOIN_EMOTE}",
            inline=True,
        )

    tip_service = cog._get_tip_service()
    if tip_service:
        tip_stats = tip_service.get_user_tip_stats(target_discord_id, guild_id)

        if tip_stats["tips_sent_count"] > 0 or tip_stats["tips_received_count"] > 0:
            tip_lines = []
            if tip_stats["tips_sent_count"] > 0:
                tip_lines.append(
                    f"**Sent:** {tip_stats['total_sent']} {JOPACOIN_EMOTE} ({tip_stats['tips_sent_count']} tips)"
                )
            if tip_stats["tips_received_count"] > 0:
                tip_lines.append(
                    f"**Received:** {tip_stats['total_received']} {JOPACOIN_EMOTE} ({tip_stats['tips_received_count']} tips)"
                )
            if tip_stats["fees_paid"] > 0:
                tip_lines.append(f"**Fees Paid:** {tip_stats['fees_paid']} {JOPACOIN_EMOTE}")

            embed.add_field(name="💝 Tipping", value="\n".join(tip_lines), inline=True)

    chart_file = await _attach_balance_history_chart(
        embed, cog, target_user, target_discord_id, guild_id
    )

    embed.set_footer(text="Tip: Use /balance for quick check, /loan to borrow")

    return embed, chart_file


async def _attach_balance_history_chart(
    embed: discord.Embed,
    cog: ProfileCommands,
    target_user: discord.Member | discord.User,
    target_discord_id: int,
    guild_id: int | None,
) -> discord.File | None:
    """Generate + attach the per-source balance chart. Returns the ``File`` or ``None``."""
    balance_history_service = cog._get_balance_history_service()
    if balance_history_service is None:
        return None

    try:
        series, per_source_totals = balance_history_service.get_balance_event_series(
            target_discord_id, guild_id
        )
    except Exception as e:  # defensive: underlying repos could raise
        logger.debug(f"Could not build balance history series: {e}")
        return None

    if not series or len(series) < 2:
        return None

    embed.add_field(
        name="📊 Balance History",
        value=_format_breakdown(per_source_totals),
        inline=False,
    )

    try:
        chart_bytes = await asyncio.to_thread(
            functools.partial(
                draw_balance_chart,
                username=target_user.display_name,
                series=series,
                per_source_totals=per_source_totals,
            )
        )
    except Exception as e:
        logger.debug(f"Could not generate balance history chart: {e}")
        return None

    chart_file = discord.File(chart_bytes, filename="balance_history.png")
    embed.set_image(url="attachment://balance_history.png")
    return chart_file


def _format_breakdown(per_source_totals: dict[str, int]) -> str:
    """Render per-source net totals as a multi-line embed value."""
    if not per_source_totals:
        return "_No net activity_"
    ordered = sorted(per_source_totals.items(), key=lambda kv: -abs(kv[1]))
    return "\n".join(
        f"**{SOURCE_LABELS.get(src, src.title())}:** {val:+d} {JOPACOIN_EMOTE}"
        for src, val in ordered
    )
