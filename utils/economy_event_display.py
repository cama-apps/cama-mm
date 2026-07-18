"""Shared public presentation for server-wide Jopacoin economy events."""

from __future__ import annotations

import discord

from domain.models.economy_event import EconomyEventEffects


def _format_effect_percent(change: float) -> str:
    magnitude = round(abs(float(change)), 1)
    if magnitude.is_integer():
        return f"{int(magnitude)}%"
    return f"{magnitude:.1f}%"


def _relative_effect(
    multiplier: float,
    *,
    lower: str = "lower",
    higher: str = "higher",
) -> str | None:
    change = (float(multiplier) - 1.0) * 100.0
    if abs(change) < 0.05:
        return None
    direction = lower if change < 0 else higher
    return f"**{_format_effect_percent(change)} {direction}**"


def _event_effect_lines(effects: EconomyEventEffects) -> list[str]:
    lines: list[str] = []

    reward = _relative_effect(effects.reward_multiplier)
    if reward:
        lines.append(f"🎁 Generated rewards are {reward}.")

    gamba_parts: list[str] = []
    gamba_win = _relative_effect(effects.gamba_win_multiplier)
    if gamba_win:
        gamba_parts.append(f"wins {gamba_win}")
    gamba_loss = _relative_effect(
        effects.gamba_loss_multiplier,
        lower="softer",
        higher="harsher",
    )
    if gamba_loss:
        gamba_parts.append(f"losses {gamba_loss}")
    if gamba_parts:
        lines.append(f"🎰 Gamba: {', '.join(gamba_parts)}.")

    bet = _relative_effect(effects.bet_payout_multiplier)
    if bet:
        lines.append(f"⚔️ Match-bet payouts are {bet}.")

    prediction_parts: list[str] = []
    prediction_payout = _relative_effect(effects.prediction_payout_multiplier)
    if prediction_payout:
        prediction_parts.append(f"resolution payouts {prediction_payout}")
    prediction_depth = _relative_effect(
        effects.prediction_depth_multiplier,
        lower="thinner",
        higher="deeper",
    )
    if prediction_depth:
        prediction_parts.append(f"liquidity {prediction_depth}")
    spread = int(effects.prediction_spread_ticks_delta)
    if spread:
        width = "wider" if spread > 0 else "tighter"
        prediction_parts.append(f"spreads **{abs(spread)} ticks {width}**")
    if prediction_parts:
        lines.append(f"📈 Prediction markets: {', '.join(prediction_parts)}.")

    if not lines:
        lines.append("⚖️ Payouts and market conditions remain at their normal strength.")
    return lines


def _immediate_effect_text(effects: EconomyEventEffects) -> str:
    actions: list[str] = []
    if effects.reserve_burn_jc:
        actions.append(
            f"**{effects.reserve_burn_jc:,} JC** burned from the Jopa Reserve"
        )
    if effects.wallet_burn_jc:
        actions.append(
            f"**{effects.wallet_burn_jc:,} JC** burned from positive wallets"
        )
    if effects.reserve_release_jc:
        actions.append(
            f"**{effects.reserve_release_jc:,} JC** redistributed from the Jopa Reserve"
        )
    if actions:
        return "The spell took immediate hold: " + "; ".join(actions) + "."
    return (
        "No JC moved when this spell activated. Its pressure arrives through "
        "adjusted payouts and liquidity as activity settles."
    )


def build_public_economy_event_embed(
    event: dict | None,
    *,
    icon_url: str | None = None,
) -> discord.Embed:
    """Build the single public event card used by announcements and commands."""
    if not event:
        return discord.Embed(
            title="🌤️ The Economy Is Between Spells",
            description=(
                "No server-wide economic edict is active. The next decree is "
                "scheduled for **10 AM Pacific**."
            ),
            color=0x7F8C8D,
        )

    severity = max(1, min(3, int(event.get("severity", 1))))
    level = ("I", "II", "III")[severity - 1]
    direction = str(event.get("direction") or "neutral")
    icon, color, edict = {
        "deflationary": ("🌑", 0xD94B4B, "A deflationary edict grips the server."),
        "boon": ("✨", 0x43B581, "A boon washes over the server."),
        "neutral": ("🕰️", 0x7F8C8D, "The market bends without changing course."),
    }.get(direction, ("🕰️", 0x7F8C8D, "The market bends without changing course."))
    announcement = str(event.get("announcement") or "").strip()
    flavor = announcement.splitlines()[0] if announcement else edict
    ends_at = event.get("ends_at")
    expiry = (
        f"Active until <t:{int(ends_at)}:R>."
        if ends_at is not None
        else "Active until the next 10 AM Pacific rollover."
    )
    effects = EconomyEventEffects.from_mapping(event.get("effects"))

    embed = discord.Embed(
        title=f"{icon} {event['name']} — Level {level}",
        description=f"*{flavor}*\n\n**{edict}** {expiry}",
        color=color,
    )
    embed.add_field(
        name="The Spell Takes Effect",
        value="\n".join(_event_effect_lines(effects)),
        inline=False,
    )
    embed.add_field(
        name="Immediate Impact",
        value=_immediate_effect_text(effects),
        inline=False,
    )
    embed.set_footer(text="The treasury watches. The edict endures.")
    if icon_url:
        embed.set_thumbnail(url=icon_url)
    return embed
