"""Compact rendering helpers for the profile gambling tab."""

from __future__ import annotations

import discord

from utils.embed_safety import EMBED_LIMITS, truncate_field

AUTO_BET_FIELD_NAME = "Auto-Bet P&L"


def _int_attr(value: object, name: str) -> int:
    try:
        return int(getattr(value, name, 0) or 0)
    except (TypeError, ValueError):
        return 0


def _group_line(label: str, group: object) -> str:
    bet_count = max(0, _int_attr(group, "bet_count"))
    wins = max(0, _int_attr(group, "wins"))
    losses = max(0, bet_count - wins)
    total_wagered = max(0, _int_attr(group, "total_wagered"))
    net_pnl = _int_attr(group, "net_pnl")
    bet_word = "bet" if bet_count == 1 else "bets"
    return (
        f"**{label}:** {net_pnl:+,} JC · {bet_count:,} {bet_word} · "
        f"{wins:,}W–{losses:,}L · {total_wagered:,} wagered"
    )


def _direction_label(directions: object) -> tuple[str, str]:
    normalized: list[str] = []
    for direction in directions or ():
        value = str(getattr(direction, "value", direction)).lower()
        if value in {"long", "short"} and value not in normalized:
            normalized.append(value)

    if normalized == ["long"]:
        return "↗", "LONG"
    if normalized == ["short"]:
        return "↘", "SHORT"
    if normalized:
        return "↕", "/".join(value.upper() for value in normalized)
    return "•", "AUTO"


def _target_line(target: object) -> str:
    icon, directions = _direction_label(getattr(target, "directions", ()))
    target_id = _int_attr(target, "target_id")
    bet_count = max(0, _int_attr(target, "bet_count"))
    wins = max(0, _int_attr(target, "wins"))
    losses = max(0, bet_count - wins)
    total_wagered = max(0, _int_attr(target, "total_wagered"))
    net_pnl = _int_attr(target, "net_pnl")
    return (
        f"{icon} <@{target_id}> {directions} · {net_pnl:+,} JC · "
        f"{wins:,}W–{losses:,}L · {total_wagered:,} wagered"
    )


def _omitted_line(count: int) -> str:
    noun = "target" if count == 1 else "targets"
    return f"*…{count:,} more {noun} omitted*"


def _render_value(
    summary_lines: list[str],
    target_lines: list[str],
    *,
    budget: int,
) -> str:
    """Fit summaries and as many target rows as possible into ``budget``."""
    summary = "\n".join(summary_lines)
    if not target_lines:
        return truncate_field(summary, budget)

    heading = "**By player:**"

    def compose(target_count: int) -> str:
        lines = [*summary_lines, heading, *target_lines[:target_count]]
        omitted = len(target_lines) - target_count
        if omitted:
            lines.append(_omitted_line(omitted))
        return "\n".join(lines)

    # Even with no target rows, retain the exact omitted count. If the embed is
    # already unusually close to Discord's total limit, trim the summaries to
    # make room for that marker rather than producing an invalid embed.
    zero_target_value = compose(0)
    if len(zero_target_value) > budget:
        suffix = f"\n{heading}\n{_omitted_line(len(target_lines))}"
        prefix_budget = budget - len(suffix)
        if prefix_budget <= 0:
            return truncate_field(_omitted_line(len(target_lines)), budget)
        if prefix_budget < 4:
            return suffix.lstrip()
        return f"{truncate_field(summary, prefix_budget)}{suffix}"

    best = zero_target_value
    for target_count in range(1, len(target_lines) + 1):
        candidate = compose(target_count)
        if len(candidate) > budget:
            break
        best = candidate
    return best


def add_auto_bet_performance_field(
    embed: discord.Embed,
    performance: object,
) -> None:
    """Append a Discord-safe lifetime automatic-betting P&L summary."""
    if performance is None or len(embed.fields) >= EMBED_LIMITS["max_fields"]:
        return

    total = getattr(performance, "total", None)
    generic = getattr(performance, "generic", None)
    arbitrage = getattr(performance, "arbitrage", None)
    targets = list(getattr(performance, "targets", ()) or ())
    if not any(
        (
            _int_attr(total, "bet_count"),
            _int_attr(generic, "bet_count"),
            _int_attr(arbitrage, "bet_count"),
            *(_int_attr(target, "bet_count") for target in targets),
        )
    ):
        return

    remaining_total = EMBED_LIMITS["total"] - len(embed) - len(AUTO_BET_FIELD_NAME)
    value_budget = min(EMBED_LIMITS["field_value"], remaining_total)
    if value_budget < 4:
        return

    summary_lines = [
        _group_line("Auto total", total),
        _group_line("Generic auto", generic),
        _group_line("Arbitrage hedges", arbitrage),
    ]
    value = _render_value(
        summary_lines,
        [_target_line(target) for target in targets],
        budget=value_budget,
    )
    if value:
        embed.add_field(name=AUTO_BET_FIELD_NAME, value=value, inline=False)
