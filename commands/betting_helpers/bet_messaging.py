"""Helpers that refresh shuffle wager fields and post betting reminders.

These touch ``cog.bot``, ``cog.match_service``, and ``cog.betting_service`` —
they exist as free coroutines so the cog file stays focused on command
handlers and the runtime contract for ``commands/match.py`` is preserved.
"""

from __future__ import annotations

import asyncio
import functools
import logging
import time
from typing import TYPE_CHECKING

import discord

from config import BET_UNDERDOG_PING_RATIO
from utils.formatting import format_betting_display

if TYPE_CHECKING:
    from commands.betting import BettingCommands

logger = logging.getLogger("cama_bot.commands.betting")


async def update_shuffle_message_wagers(
    cog: BettingCommands,
    guild_id: int | None,
    pending_match_id: int | None = None,
    locked: bool = False,
) -> None:
    """Refresh the shuffle message's wager field with current totals.

    Updates the main channel message, the command-channel copy, and the thread
    copy. Pass ``locked=True`` to render the closed state (no live countdown)
    once betting has locked.
    """
    pending_state = await asyncio.to_thread(
        cog.match_service.get_last_shuffle, guild_id, pending_match_id
    )
    if not pending_state:
        return

    # Get betting display info
    totals = await asyncio.to_thread(
        functools.partial(cog.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
    )
    lock_until = pending_state.bet_lock_until
    betting_mode = pending_state.betting_mode
    field_name, field_value = format_betting_display(
        totals["radiant"],
        totals["dire"],
        betting_mode,
        lock_until,
        locked=locked,
        seed_radiant=pending_state.bet_seed_radiant,
        seed_dire=pending_state.bet_seed_dire,
        seed_bonus=pending_state.bet_seed_bonus,
    )

    # Update main channel message (lobby channel)
    message_info = await asyncio.to_thread(
        cog.match_service.state_service.get_shuffle_message_info, guild_id, pending_match_id
    )
    message_id = message_info.get("message_id") if message_info else None
    channel_id = message_info.get("channel_id") if message_info else None
    if message_id and channel_id:
        await update_embed_betting_field(cog, channel_id, message_id, field_name, field_value)

    # Update command channel message if it exists (different from lobby channel)
    cmd_message_id = message_info.get("cmd_message_id") if message_info else None
    cmd_channel_id = message_info.get("cmd_channel_id") if message_info else None
    if cmd_message_id and cmd_channel_id:
        await update_embed_betting_field(cog, cmd_channel_id, cmd_message_id, field_name, field_value)

    # Update thread message if it exists
    thread_message_id = pending_state.thread_shuffle_message_id
    thread_id = pending_state.thread_shuffle_thread_id
    if thread_message_id and thread_id:
        await update_embed_betting_field(cog, thread_id, thread_message_id, field_name, field_value)


async def update_embed_betting_field(
    cog: BettingCommands,
    channel_id: int,
    message_id: int,
    field_name: str,
    field_value: str,
) -> None:
    """Helper to update the betting field in an embed message."""
    try:
        channel = cog.bot.get_channel(channel_id)
        if channel is None:
            channel = await cog.bot.fetch_channel(channel_id)
        if channel is None:
            return

        message = await channel.fetch_message(message_id)
        if not message or not message.embeds:
            return

        embed = message.embeds[0]
        embed_dict = embed.to_dict()
        fields = embed_dict.get("fields", [])

        # Known wager field names to look for
        wager_field_names = {"💰 Pool Betting", "💰 House Betting (1:1)", "💰 Betting"}

        # Find and update wager field, remove duplicates
        updated = False
        new_fields = []
        for field in fields:
            fname = field.get("name", "")
            if fname in wager_field_names:
                if not updated:
                    # Update the first matching wager field
                    field["name"] = field_name
                    field["value"] = field_value
                    new_fields.append(field)
                    updated = True
                # Skip duplicates (don't add them to new_fields)
            else:
                new_fields.append(field)

        if not updated:
            new_fields.append({"name": field_name, "value": field_value, "inline": False})
        embed_dict["fields"] = new_fields

        new_embed = discord.Embed.from_dict(embed_dict)
        await message.edit(embed=new_embed, allowed_mentions=discord.AllowedMentions.none())
    except Exception as exc:
        logger.warning(f"Failed to update shuffle wagers: {exc}", exc_info=True)


async def send_betting_reminder(
    cog: BettingCommands,
    guild_id: int | None,
    *,
    reminder_type: str,
    lock_until: int | None,
    pending_match_id: int | None = None,
    is_final_warning: bool = False,
) -> None:
    """Send a reminder message replying to the shuffle embed with current bet totals.

    reminder_type: "warning" (terse one-liner), "last_call" (terse + an AI
        betting-announcer hype line), or "closed" (final notice; also flips the
        embed to a locked state).
    pending_match_id: Specific match ID for concurrent match support.
    is_final_warning: True only for the smallest warning offset (the last warning
        before the 1-minute last call; 5 min by default). That tier gets the AI
        persona flavor line and, when the pool is at least BET_UNDERDOG_PING_RATIO
        lopsided, @-pings the under-bet team. Other warnings stay terse.
    """
    pending_state = await asyncio.to_thread(
        cog.match_service.get_last_shuffle, guild_id, pending_match_id=pending_match_id
    )
    if not pending_state:
        return

    message_info = await asyncio.to_thread(
        cog.match_service.get_shuffle_message_info, guild_id, pending_match_id=pending_match_id
    )
    channel_id = message_info.get("channel_id") if message_info else None
    thread_message_id = message_info.get("thread_message_id") if message_info else None
    thread_id = message_info.get("thread_id") if message_info else None

    totals = await asyncio.to_thread(
        functools.partial(cog.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
    )
    betting_mode = pending_state.betting_mode

    # Format bets with odds for pool mode
    _, totals_text = format_betting_display(
        totals["radiant"],
        totals["dire"],
        betting_mode,
        lock_until=None,
        seed_radiant=pending_state.bet_seed_radiant,
        seed_dire=pending_state.bet_seed_dire,
        seed_bonus=pending_state.bet_seed_bonus,
    )
    mode_label = "Pool" if betting_mode == "pool" else "House (1:1)"

    allowed_mentions = discord.AllowedMentions.none()

    if reminder_type == "warning":
        if not lock_until:
            return
        content = f"⏰ **Betting closes <t:{int(lock_until)}:R>** — {totals_text}"
        if is_final_warning:
            underdog_side, ping_ids = _underdog_status(pending_state, totals)
            flavor = await _betting_warning_flavor(
                cog, guild_id, pending_state, totals_text, lock_until, underdog_side
            )
            if flavor:
                content += f"\n\n💬 {flavor}"
            if ping_ids:
                content += "\n" + " ".join(f"<@{pid}>" for pid in ping_ids)
                # Scope strictly to the underdog players. The content carries an
                # LLM-generated flavor line, and a bare ``users=True`` leaves the
                # everyone/roles fields at a truthy default — so the model could
                # broadcast an @everyone/@here. A users-list disables that.
                allowed_mentions = discord.AllowedMentions(
                    everyone=False,
                    roles=False,
                    replied_user=False,
                    users=[discord.Object(id=pid) for pid in ping_ids],
                )
    elif reminder_type == "last_call":
        if not lock_until:
            return
        flavor = await _betting_last_call_flavor(
            cog, guild_id, pending_state, totals_text, lock_until
        )
        content = f"🎲 **Last call — betting closes <t:{int(lock_until)}:R>** — {totals_text}"
        if flavor:
            content += f"\n\n💬 {flavor}"
    elif reminder_type == "closed":
        content = f"🔒 **Betting closed!** Final {mode_label} pool — {totals_text}"
    else:
        return

    # Post to origin channel (stored in shuffle message info, since reset_lobby clears it)
    try:
        # Get origin_channel_id from shuffle message info (lobby_service's is cleared by reset_lobby)
        origin_channel_id = message_info.get("origin_channel_id") if message_info else None
        target_channel_id = origin_channel_id if origin_channel_id else channel_id

        if target_channel_id:
            target_channel = cog.bot.get_channel(target_channel_id)
            if target_channel is None:
                target_channel = await cog.bot.fetch_channel(target_channel_id)
            if target_channel:
                await target_channel.send(content, allowed_mentions=allowed_mentions)
    except Exception as exc:
        logger.warning(f"Failed to send betting reminder to channel: {exc}", exc_info=True)

    # Post to thread
    if thread_message_id and thread_id:
        try:
            thread = cog.bot.get_channel(thread_id)
            if thread is None:
                thread = await cog.bot.fetch_channel(thread_id)
            if thread:
                thread_message = thread.get_partial_message(thread_message_id)
                await thread_message.reply(content, allowed_mentions=allowed_mentions)
        except Exception as exc:
            logger.warning(f"Failed to send betting reminder to thread: {exc}", exc_info=True)

    # On close, flip the embed itself to a locked state so it stops showing a
    # stale "Closes <t:..:R>" countdown (which Discord renders as a past date).
    # A re-opened/extended window is already filtered upstream in
    # _run_bet_reminder_after_delay (current_lock != lock_until), so reaching
    # here means this window genuinely closed at its scheduled time.
    if reminder_type == "closed":
        try:
            await update_shuffle_message_wagers(cog, guild_id, pending_match_id, locked=True)
        except Exception as exc:
            logger.warning(f"Failed to flip betting embed to closed state: {exc}", exc_info=True)


async def _betting_last_call_flavor(
    cog: BettingCommands,
    guild_id: int | None,
    pending_state,
    standings: str,
    lock_until: int | None,
) -> str | None:
    """Build the 1-minute last-call flavor line, or None.

    Picks the biggest voluntary (non-blind) bettor as the callout target; falls
    back to an empty-pool taunt when nobody has bet. Reuses the betting persona
    roster via FlavorTextService; returns None if no flavor service is wired.
    """
    flavor_service = getattr(cog, "flavor_text_service", None)
    if flavor_service is None:
        return None
    top = await asyncio.to_thread(
        functools.partial(
            cog.betting_service.get_top_voluntary_bettor, guild_id, pending_state=pending_state
        )
    )
    seconds_left = max(0, int(lock_until) - int(time.time())) if lock_until else 60
    event_details = {
        "standings": standings,
        "seconds_left": seconds_left,
        "leader_team": top.get("team_bet_on") if top else None,
        "leader_amount": top.get("amount") if top else None,
    }
    leader_discord_id = top.get("discord_id") if top else None
    try:
        return await flavor_service.generate_betting_last_call(
            guild_id, event_details, leader_discord_id=leader_discord_id
        )
    except Exception as exc:
        logger.warning(f"Failed to generate last-call flavor: {exc}", exc_info=True)
        return None


def _underdog_status(pending_state, totals) -> tuple[str | None, list[int]]:
    """Detect a lopsided pool and return the under-bet side and its players.

    "Underdog" here is the lower-money side of the live pool (the long-shot
    payout). Returns ``(underdog_side, real_ids)`` — ``underdog_side`` set to
    "radiant"/"dire" — when one side has money and the other has none, or the
    favorite's money is at least ``BET_UNDERDOG_PING_RATIO`` times the underdog's.
    Otherwise returns ``(None, [])`` (balanced, or an empty pool with nothing to
    compare). ``real_ids`` filters that side's roster to real players (id > 0).
    """
    radiant = totals.get("radiant", 0) or 0
    dire = totals.get("dire", 0) or 0
    hi = max(radiant, dire)
    lo = min(radiant, dire)
    if hi <= 0:
        return None, []  # empty pool — no ratio, no ping
    if lo > 0 and hi / lo < BET_UNDERDOG_PING_RATIO:
        return None, []  # not lopsided enough to ping
    underdog_side = "radiant" if radiant <= dire else "dire"
    team_ids = (
        getattr(pending_state, "radiant_team_ids", None)
        if underdog_side == "radiant"
        else getattr(pending_state, "dire_team_ids", None)
    )
    real_ids = [pid for pid in (team_ids or []) if isinstance(pid, int) and pid > 0]
    return underdog_side, real_ids


async def _betting_warning_flavor(
    cog: BettingCommands,
    guild_id: int | None,
    pending_state,
    standings: str,
    lock_until: int | None,
    underdog_side: str | None,
) -> str | None:
    """Build the final-warning flavor line (default 5 min out), or None.

    Same persona path as the last-call line, but the urgency reads in minutes
    and — when the pool is lopsided — the persona roasts/hypes ``underdog_side``.
    Returns None if no flavor service is wired.
    """
    flavor_service = getattr(cog, "flavor_text_service", None)
    if flavor_service is None:
        return None
    top = await asyncio.to_thread(
        functools.partial(
            cog.betting_service.get_top_voluntary_bettor, guild_id, pending_state=pending_state
        )
    )
    seconds_left = max(0, int(lock_until) - int(time.time())) if lock_until else 300
    event_details = {
        "standings": standings,
        "seconds_left": seconds_left,
        "leader_team": top.get("team_bet_on") if top else None,
        "leader_amount": top.get("amount") if top else None,
    }
    leader_discord_id = top.get("discord_id") if top else None
    try:
        return await flavor_service.generate_betting_warning(
            guild_id,
            event_details,
            leader_discord_id=leader_discord_id,
            underdog_side=underdog_side,
        )
    except Exception as exc:
        logger.warning(f"Failed to generate betting warning flavor: {exc}", exc_info=True)
        return None
