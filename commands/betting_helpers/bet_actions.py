"""Action handlers for the bet/mybets/bets/balance command bodies.

Extracted from the cog so the slash-command handlers in ``commands/betting.py``
can stay thin orchestrators around the imported helpers.
"""

from __future__ import annotations

import asyncio
import functools
import logging
import time
from typing import TYPE_CHECKING

import discord
from discord import app_commands

from config import (
    BANKRUPTCY_PENALTY_RATE,
    GARNISHMENT_PERCENTAGE,
    JOPACOIN_MIN_BET,
)
from services.permissions import has_admin_permission
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer
from utils.neon_helpers import send_neon_result
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from commands.betting import BettingCommands

logger = logging.getLogger("cama_bot.commands.betting")


async def bet_action(
    cog: BettingCommands, interaction: discord.Interaction, team: app_commands.Choice[str], amount: int, leverage: app_commands.Choice[int] | None, match: int | None,
) -> None:
    guild = interaction.guild if interaction.guild else None
    rl_gid = guild.id if guild else 0
    rl = GLOBAL_RATE_LIMITER.check(
        scope="bet",
        guild_id=rl_gid,
        user_id=interaction.user.id,
        limit=5,
        per_seconds=20,
    )
    if not rl.allowed:
        await interaction.response.send_message(
            f"⏳ Please wait {rl.retry_after_seconds}s before using `/bet` again.",
            ephemeral=True,
        )
        return

    if not await safe_defer(interaction, ephemeral=True):
        return
    guild_id = interaction.guild.id if interaction.guild else None
    user_id = interaction.user.id

    if amount < JOPACOIN_MIN_BET:
        await interaction.followup.send(
            f"Minimum bet is {JOPACOIN_MIN_BET} {JOPACOIN_EMOTE}.", ephemeral=True
        )
        return

    # Handle match selection for concurrent match support
    pending_state = None
    pending_match_id = match  # Optional match ID from parameter

    if pending_match_id is not None:
        # Explicit match ID provided - use that specific match
        pending_state = await asyncio.to_thread(
            cog.match_service.state_service.get_last_shuffle, guild_id, pending_match_id
        )
        if not pending_state:
            await interaction.followup.send(
                f"❌ Match #{pending_match_id} not found or already completed.", ephemeral=True
            )
            return
    else:
        # No match specified - try auto-detection
        all_pending = await asyncio.to_thread(
            cog.match_service.state_service.get_all_pending_matches, guild_id
        )
        if not all_pending:
            await interaction.followup.send("❌ No active match to bet on.", ephemeral=True)
            return

        if len(all_pending) == 1:
            # Single match - use it (backward compatible)
            pending_state = all_pending[0]
        else:
            # Multiple matches - try to find one the user is in
            player_match = await asyncio.to_thread(
                cog.match_service.state_service.get_pending_match_for_player, guild_id, user_id
            )
            if player_match:
                pending_state = player_match
            else:
                # User is a spectator with multiple matches - require explicit selection
                match_list = ", ".join(f"Match #{m.pending_match_id}" for m in all_pending if m.pending_match_id)
                await interaction.followup.send(
                    f"❌ Multiple matches in progress ({match_list}). "
                    "Please specify which match to bet on using the `match` parameter.",
                    ephemeral=True,
                )
                return

    if not pending_state:
        await interaction.followup.send("❌ No active match to bet on.", ephemeral=True)
        return

    pending_match_id = pending_state.pending_match_id

    # Unified betting through BettingService (works for both shuffle and draft modes)
    lev = leverage.value if leverage else 1

    # Red mana: unlock 10x leverage
    if lev == 10:
        _mana_fx = getattr(cog.bot, "mana_effects_service", None)
        _has_10x = False
        if _mana_fx:
            try:
                from domain.models.mana_effects import ManaEffects as _MEBet
                _bet_effects = await asyncio.to_thread(_mana_fx.get_effects, user_id, guild_id)
                if isinstance(_bet_effects, _MEBet):
                    _has_10x = _bet_effects.red_10x_leverage
            except Exception:
                pass
        if not _has_10x:
            await interaction.followup.send(
                "❌ 10x leverage is exclusive to **Red mana (Mountain)** players!", ephemeral=True
            )
            return

    effective_bet = amount * lev

    try:
        await asyncio.to_thread(
            functools.partial(
                cog.betting_service.place_bet,
                guild_id, user_id, team.value, amount, pending_state, leverage=lev,
            )
        )
    except ValueError as exc:
        await interaction.followup.send(f"❌ {exc}", ephemeral=True)
        return

    # Green steady bonus on bet placement (silent — no embed callout).
    _mana_fx_bet = getattr(cog.bot, "mana_effects_service", None)
    if _mana_fx_bet is not None:
        try:
            _bet_fx = await asyncio.to_thread(_mana_fx_bet.get_effects, user_id, guild_id)
            if _bet_fx.match_bet_steady_bonus > 0:
                await asyncio.to_thread(
                    cog.player_service.adjust_balance,
                    user_id, guild_id, _bet_fx.match_bet_steady_bonus,
                )
        except Exception:
            logger.debug("Mana steady bonus on bet placement failed", exc_info=True)

    await cog._update_shuffle_message_wagers(guild_id, pending_match_id)

    # Build response message
    betting_mode = pending_state.betting_mode if pending_state else "pool"
    pool_warning = ""
    if betting_mode == "pool":
        pool_warning = "\n⚠️ Pool mode: odds may shift as more bets come in. Use `/mybets` to check current EV."

    # Include match ID note if there's a pending_match_id
    match_note = f" (Match #{pending_match_id})" if pending_match_id else ""

    from utils.mana_display import resolve_mana_badge
    _bet_badge = await resolve_mana_badge(cog.bot, user_id, guild_id)
    _bet_prefix = f"{_bet_badge} " if _bet_badge else ""

    if lev > 1:
        await interaction.followup.send(
            f"{_bet_prefix}Bet placed{match_note}: {amount} {JOPACOIN_EMOTE} on {team.name} at {lev}x leverage "
            f"(effective: {effective_bet} {JOPACOIN_EMOTE}).{pool_warning}",
            ephemeral=True,
        )
    else:
        await interaction.followup.send(
            f"{_bet_prefix}Bet placed{match_note}: {amount} {JOPACOIN_EMOTE} on {team.name}.{pool_warning}",
            ephemeral=True,
        )

    # Neon Degen Terminal hooks - at most ONE neon event per /bet action
    neon = cog._get_neon_service()
    if neon:
        try:
            # Pre-fetch data needed by multiple event checks
            player_repo = cog.betting_service.player_repo
            player = await asyncio.to_thread(
                player_repo.get_by_id, user_id, guild_id
            )
            balance_before = (player.jopacoin_balance + amount) if player else 0

            # Increment total bets (side-effect needed regardless of which event fires)
            total_bets = await asyncio.to_thread(
                player_repo.increment_total_bets_placed, user_id, guild_id
            ) if player else 0

            # Compute seconds remaining for last-second check
            seconds_remaining = 0
            if pending_state:
                lock_time = pending_state.lock_time or 0
                import time as _time
                seconds_remaining = max(0, int(lock_time - _time.time()))

            # Build candidate event lambdas, rarest/one-time first
            candidates = []

            # First leverage bet (one-time)
            if player and lev > 1 and not player.first_leverage_used:
                async def _first_leverage():
                    result = await neon.on_first_leverage_bet(user_id, guild_id, lev)
                    if result is not None:
                        await asyncio.to_thread(
                            player_repo.mark_first_leverage_used, user_id, guild_id
                        )
                    return result
                candidates.append(_first_leverage)

            # 100 bets milestone (one-time)
            if total_bets == 100:
                candidates.append(
                    lambda: neon.on_100_bets_milestone(user_id, guild_id, total_bets)
                )

            # All-in bet
            if player and balance_before > 0:
                candidates.append(
                    lambda: neon.on_all_in_bet(user_id, guild_id, amount, balance_before)
                )

            # Last-second bet
            if 0 < seconds_remaining <= 60:
                candidates.append(
                    lambda: neon.on_last_second_bet(user_id, guild_id, seconds_remaining)
                )

            # Standard bet placed (most common, lowest priority)
            candidates.append(
                lambda: neon.on_bet_placed(user_id, guild_id, amount, lev, team.value)
            )

            await cog._send_first_neon_result(interaction, *candidates)
        except Exception as e:
            logger.debug(f"Easter egg event hooks error: {e}")



async def mybets_action(
    cog: BettingCommands, interaction: discord.Interaction,
) -> None:
    guild = interaction.guild if interaction.guild else None
    rl_gid = guild.id if guild else 0
    rl = GLOBAL_RATE_LIMITER.check(
        scope="mybets",
        guild_id=rl_gid,
        user_id=interaction.user.id,
        limit=5,
        per_seconds=10,
    )
    if not rl.allowed:
        await interaction.response.send_message(
            f"⏳ Please wait {rl.retry_after_seconds}s before using `/mybets` again.",
            ephemeral=True,
        )
        return

    if not await safe_defer(interaction, ephemeral=True):
        return

    guild_id = interaction.guild.id if interaction.guild else None

    # Get all pending bets for the user (across all matches)
    all_bets = await asyncio.to_thread(
        cog.betting_service.bet_repo.get_all_player_pending_bets,
        guild_id, interaction.user.id
    )
    if not all_bets:
        await interaction.followup.send("You have no active bets.", ephemeral=True)
        return

    # Get all pending matches for context
    all_pending = await asyncio.to_thread(
        cog.match_service.state_service.get_all_pending_matches, guild_id
    )
    pending_by_id = {m.pending_match_id: m for m in all_pending if m.pending_match_id}

    # Group bets by pending_match_id
    bets_by_match: dict[int | None, list[dict]] = {}
    for bet in all_bets:
        pmid = bet.get("pending_match_id")
        if pmid not in bets_by_match:
            bets_by_match[pmid] = []
        bets_by_match[pmid].append(bet)

    # Build output for each match
    output_sections = []
    for pmid, bets in bets_by_match.items():
        pending_state = pending_by_id.get(pmid) if pmid else None

        # Calculate totals for this match
        total_amount = sum(b["amount"] for b in bets)
        total_effective = sum(b["amount"] * (b.get("leverage", 1) or 1) for b in bets)
        team_name = bets[0]["team_bet_on"].title()

        # Build bet lines
        bet_lines = []
        for i, bet in enumerate(bets, 1):
            leverage = bet.get("leverage", 1) or 1
            effective = bet["amount"] * leverage
            time_str = f"<t:{int(bet['bet_time'])}:t>"
            is_blind = bet.get("is_blind", 0)
            auto_tag = " (auto)" if is_blind else ""
            if leverage > 1:
                bet_lines.append(
                    f"{i}. {bet['amount']} {JOPACOIN_EMOTE} at {leverage}x "
                    f"(effective: {effective} {JOPACOIN_EMOTE}){auto_tag} — {time_str}"
                )
            else:
                bet_lines.append(f"{i}. {bet['amount']} {JOPACOIN_EMOTE}{auto_tag} — {time_str}")

        # Header with match ID if multiple matches
        match_label = f" (Match #{pmid})" if pmid and len(bets_by_match) > 1 else ""
        if len(bets) == 1:
            header = f"**Active bet on {team_name}{match_label}:**"
        else:
            header = f"**Active bets on {team_name}{match_label}** ({len(bets)} bets):"

        # Show total if multiple bets
        if len(bets) > 1:
            if total_amount != total_effective:
                bet_lines.append(
                    f"\n**Total:** {total_amount} {JOPACOIN_EMOTE} "
                    f"(effective: {total_effective} {JOPACOIN_EMOTE})"
                )
            else:
                bet_lines.append(f"\n**Total:** {total_amount} {JOPACOIN_EMOTE}")

        section_msg = header + "\n" + "\n".join(bet_lines)

        # Add EV info for pool mode
        betting_mode = pending_state.betting_mode if pending_state else "pool"
        if betting_mode == "pool" and pending_state:
            totals = await asyncio.to_thread(
                functools.partial(cog.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
            )
            total_pool = totals["radiant"] + totals["dire"]
            my_team_total = totals[bets[0]["team_bet_on"]]

            if my_team_total > 0 and total_pool > 0:
                my_share = total_effective / my_team_total
                potential_payout = int(total_pool * my_share)
                other_team = "dire" if bets[0]["team_bet_on"] == "radiant" else "radiant"
                odds_ratio = totals[other_team] / my_team_total if my_team_total > 0 else 0

                section_msg += (
                    f"\n\n📊 **Current Pool Odds** (may change):"
                    f"\nTotal pool: {total_pool} {JOPACOIN_EMOTE}"
                    f"\nYour team ({team_name}): {my_team_total} {JOPACOIN_EMOTE}"
                    f"\nIf you win: ~{potential_payout} {JOPACOIN_EMOTE} ({odds_ratio:.2f}:1 odds)"
                )
        elif betting_mode == "house":
            # House mode: 1:1 payout
            potential_payout = total_effective * 2
            section_msg += f"\n\nIf you win: {potential_payout} {JOPACOIN_EMOTE} (1:1 odds)"

        output_sections.append(section_msg)

    # Join all sections with a separator if multiple matches
    if len(output_sections) > 1:
        base_msg = "\n\n---\n\n".join(output_sections)
    else:
        base_msg = output_sections[0]

    await interaction.followup.send(base_msg, ephemeral=True)



async def bets_action(
    cog: BettingCommands, interaction: discord.Interaction, match: int | None,
) -> None:
    """View all bets in the current pool."""
    if not has_admin_permission(interaction):
        guild = interaction.guild if interaction.guild else None
        rl_gid = guild.id if guild else 0
        rl = GLOBAL_RATE_LIMITER.check(
            scope="bets",
            guild_id=rl_gid,
            user_id=interaction.user.id,
            limit=1,
            per_seconds=60,
        )
        if not rl.allowed:
            await interaction.response.send_message(
                f"⏳ Please wait {rl.retry_after_seconds}s before using `/bets` again.",
                ephemeral=True,
            )
            return

    if not await safe_defer(interaction, ephemeral=True):
        return

    guild_id = interaction.guild.id if interaction.guild else None
    user_id = interaction.user.id

    # Handle match selection for concurrent match support
    pending_state = None
    pending_match_id = match  # Optional match ID from parameter

    if pending_match_id is not None:
        # Explicit match ID provided - use that specific match
        pending_state = await asyncio.to_thread(
            cog.match_service.state_service.get_last_shuffle, guild_id, pending_match_id
        )
        if not pending_state:
            await interaction.followup.send(
                f"❌ Match #{pending_match_id} not found or already completed.", ephemeral=True
            )
            return
    else:
        # No match specified - try auto-detection
        all_pending = await asyncio.to_thread(
            cog.match_service.state_service.get_all_pending_matches, guild_id
        )
        if not all_pending:
            await interaction.followup.send("No active match to show bets for.", ephemeral=True)
            return

        if len(all_pending) == 1:
            # Single match - use it (backward compatible)
            pending_state = all_pending[0]
        else:
            # Multiple matches - try to find one the user is in
            player_match = await asyncio.to_thread(
                cog.match_service.state_service.get_pending_match_for_player, guild_id, user_id
            )
            if player_match:
                pending_state = player_match
            else:
                # User is a spectator with multiple matches - require explicit selection
                match_list = ", ".join(f"Match #{m.pending_match_id}" for m in all_pending if m.pending_match_id)
                await interaction.followup.send(
                    f"❌ Multiple matches in progress ({match_list}). "
                    "Please specify which match to view using the `match` parameter.",
                    ephemeral=True,
                )
                return

    if not pending_state:
        await interaction.followup.send("No active match to show bets for.", ephemeral=True)
        return

    pending_match_id = pending_state.pending_match_id

    all_bets = await asyncio.to_thread(
        functools.partial(cog.betting_service.get_all_pending_bets, guild_id, pending_state=pending_state)
    )
    if not all_bets:
        await interaction.followup.send("No bets placed yet.", ephemeral=True)
        return

    # Get current odds
    totals = await asyncio.to_thread(
        functools.partial(cog.betting_service.get_pot_odds, guild_id, pending_state=pending_state)
    )
    total_pool = totals["radiant"] + totals["dire"]
    radiant_mult = total_pool / totals["radiant"] if totals["radiant"] > 0 else None
    dire_mult = total_pool / totals["dire"] if totals["dire"] > 0 else None

    # Build embed
    match_label = f"Match #{pending_match_id} — " if pending_match_id else ""
    embed = discord.Embed(
        title=f"📊 {match_label}Pool Bets ({len(all_bets)} bets)",
        color=discord.Color.gold(),
    )

    # Current odds header
    lock_until = pending_state.bet_lock_until
    radiant_odds_str = f"{radiant_mult:.2f}x" if radiant_mult else "—"
    dire_odds_str = f"{dire_mult:.2f}x" if dire_mult else "—"
    odds_text = (
        f"🟢 Radiant: {totals['radiant']} {JOPACOIN_EMOTE} ({radiant_odds_str}) | "
        f"🔴 Dire: {totals['dire']} {JOPACOIN_EMOTE} ({dire_odds_str})"
    )
    if lock_until:
        odds_text += f"\nBetting closes <t:{lock_until}:R>"
    embed.add_field(name="Current Odds", value=odds_text, inline=False)

    # Group bets by team
    radiant_bets = [b for b in all_bets if b["team_bet_on"] == "radiant"]
    dire_bets = [b for b in all_bets if b["team_bet_on"] == "dire"]

    # Check if betting is still open and if user is admin
    is_admin = has_admin_permission(interaction)
    betting_open = lock_until and int(time.time()) < lock_until
    show_names = is_admin or not betting_open

    # Format bet line helper
    def format_bet_line(bet: dict, index: int) -> str:
        leverage = bet.get("leverage", 1) or 1
        is_blind = bet.get("is_blind", 0)
        odds_at_placement = bet.get("odds_at_placement")

        # Base amount - hide names for non-admins while betting is open
        if show_names:
            line = f"<@{bet['discord_id']}> • {bet['amount']}"
        else:
            line = f"Bettor #{index} • {bet['amount']}"

        # Auto tag
        if is_blind:
            line += " (auto)"

        # Leverage notation
        if leverage > 1:
            effective = bet["amount"] * leverage
            line += f" at {leverage}x → {effective} eff"

        # Odds at placement
        if odds_at_placement:
            line += f" • {odds_at_placement:.2f}x"

        return line

    # Radiant bets section
    if radiant_bets:
        radiant_lines = [format_bet_line(b, i + 1) for i, b in enumerate(radiant_bets)]
        # Truncate if too long
        radiant_text = "\n".join(radiant_lines[:15])
        if len(radiant_bets) > 15:
            radiant_text += f"\n... +{len(radiant_bets) - 15} more"
        embed.add_field(
            name=f"🟢 Radiant Bets ({len(radiant_bets)})",
            value=radiant_text or "None",
            inline=False,
        )

    # Dire bets section
    if dire_bets:
        dire_lines = [format_bet_line(b, i + 1) for i, b in enumerate(dire_bets)]
        dire_text = "\n".join(dire_lines[:15])
        if len(dire_bets) > 15:
            dire_text += f"\n... +{len(dire_bets) - 15} more"
        embed.add_field(
            name=f"🔴 Dire Bets ({len(dire_bets)})",
            value=dire_text or "None",
            inline=False,
        )

    # Pool summary
    radiant_pct = (totals["radiant"] / total_pool * 100) if total_pool > 0 else 0
    dire_pct = (totals["dire"] / total_pool * 100) if total_pool > 0 else 0
    summary_text = (
        f"**Total:** {total_pool} {JOPACOIN_EMOTE} effective\n"
        f"Radiant: {totals['radiant']} ({radiant_pct:.0f}%) | Dire: {totals['dire']} ({dire_pct:.0f}%)"
    )
    embed.add_field(name="Pool Summary", value=summary_text, inline=False)

    await interaction.followup.send(embed=embed, ephemeral=True)



async def balance_action(
    cog: BettingCommands, interaction: discord.Interaction,
) -> None:
    guild = interaction.guild if interaction.guild else None
    rl_gid = guild.id if guild else 0
    rl = GLOBAL_RATE_LIMITER.check(
        scope="balance",
        guild_id=rl_gid,
        user_id=interaction.user.id,
        limit=5,
        per_seconds=10,
    )
    if not rl.allowed:
        await interaction.response.send_message(
            f"Please wait {rl.retry_after_seconds}s before using `/balance` again.",
            ephemeral=True,
        )
        return

    if not await safe_defer(interaction, ephemeral=True):
        return

    user_id = interaction.user.id
    guild_id = guild.id if guild else None
    balance = await asyncio.to_thread(cog.player_service.get_balance, user_id, guild_id)

    # Mana emoji badge (empty string if unassigned)
    from utils.mana_display import resolve_mana_badge
    mana_badge = await resolve_mana_badge(cog.bot, user_id, guild_id)
    mana_prefix = f"{mana_badge} " if mana_badge else ""

    # Check for bankruptcy penalty
    penalty_info = ""
    if cog.bankruptcy_service:
        state = await asyncio.to_thread(cog.bankruptcy_service.get_state, user_id, guild_id)
        if state.penalty_games_remaining > 0:
            penalty_rate_pct = int(BANKRUPTCY_PENALTY_RATE * 100)
            penalty_info = (
                f"\n**Bankruptcy penalty:** {penalty_rate_pct}% win bonus "
                f"for {state.penalty_games_remaining} more win(s)"
            )

    # Check for loan info
    loan_info = ""
    if cog.loan_service:
        loan_state = await asyncio.to_thread(cog.loan_service.get_state, user_id, guild_id)
        # Show outstanding loan prominently
        if loan_state.has_outstanding_loan:
            loan_info = (
                f"\n⚠️ **Outstanding loan:** {loan_state.outstanding_total} {JOPACOIN_EMOTE} "
                f"(repaid after next match)"
            )
        if loan_state.total_loans_taken > 0:
            loan_info += f"\n**Loans taken:** {loan_state.total_loans_taken} (fees paid: {loan_state.total_fees_paid})"
        if loan_state.is_on_cooldown and loan_state.cooldown_ends_at:
            import time
            remaining = loan_state.cooldown_ends_at - int(time.time())
            hours = remaining // 3600
            minutes = (remaining % 3600) // 60
            loan_info += f"\n**Loan cooldown:** {hours}h {minutes}m remaining"

    if balance >= 0:
        await interaction.followup.send(
            f"{mana_prefix}{interaction.user.mention} has {balance} {JOPACOIN_EMOTE}.{penalty_info}{loan_info}",
            ephemeral=True,
        )
    else:
        # Show debt information
        garnishment_pct = int(GARNISHMENT_PERCENTAGE * 100)

        await interaction.followup.send(
            f"{mana_prefix}{interaction.user.mention} has **{balance}** {JOPACOIN_EMOTE} (in debt)\n"
            f"Garnishment: {garnishment_pct}% of winnings go to debt repayment{penalty_info}{loan_info}\n\n"
            f"Use `/bankruptcy` to clear your debt (with penalties).\n"
            f"Use `/loan` to borrow more jopacoin (with a fee).",
            ephemeral=True,
        )

    # Neon Degen Terminal hook
    neon = cog._get_neon_service()
    if neon:
        neon_result = await neon.on_balance_check(user_id, guild_id, balance)
        await send_neon_result(interaction, neon_result)



