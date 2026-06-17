"""Action handlers for the tip/paydebt/bankruptcy/loan/reserve command bodies.

Extracted from the cog so the slash-command handlers in ``commands/betting.py``
can stay thin orchestrators around the imported helpers.
"""

from __future__ import annotations

import asyncio
import functools
import logging
import math
import random
from typing import TYPE_CHECKING

import discord

from commands.betting_helpers.messages import (
    BANKRUPTCY_COOLDOWN_MESSAGES,
    BANKRUPTCY_DENIED_MESSAGES,
    BANKRUPTCY_SUCCESS_MESSAGES,
    LOAN_DENIED_COOLDOWN_MESSAGES,
    LOAN_SUCCESS_MESSAGES,
    NEGATIVE_LOAN_MESSAGES,
)
from config import (
    DISBURSE_MIN_FUND,
    LOAN_FEE_RATE,
    TIP_FEE_RATE,
)
from services.flavor_text_service import FlavorEvent
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup
from utils.neon_helpers import send_neon_result
from utils.rate_limiter import GLOBAL_RATE_LIMITER

if TYPE_CHECKING:
    from commands.betting import BettingCommands

logger = logging.getLogger("cama_bot.commands.betting")


async def tip_action(
    cog: BettingCommands, interaction: discord.Interaction, player: discord.Member, amount: int,
) -> None:
    guild = interaction.guild if interaction.guild else None
    rl_gid = guild.id if guild else 0
    rl = GLOBAL_RATE_LIMITER.check(
        scope="tip",
        guild_id=rl_gid,
        user_id=interaction.user.id,
        limit=5,
        per_seconds=10,
    )
    if not rl.allowed:
        await interaction.response.send_message(
            f"Please wait {rl.retry_after_seconds}s before using `/tip` again.",
            ephemeral=True,
        )
        return

    # Always public since giving to another player
    if not await safe_defer(interaction, ephemeral=False):
        return

    # Validate amount
    if amount <= 0:
        await interaction.followup.send(
            "Amount must be positive.",
            ephemeral=True,
        )
        return

    # Check if tipping themselves
    if player.id == interaction.user.id:
        await interaction.followup.send(
            "You cannot tip yourself.",
            ephemeral=True,
        )
        return

    # Extract guild_id early for consistent audit trail
    guild_id = interaction.guild.id if interaction.guild else None

    # Check if both players are registered
    sender = await asyncio.to_thread(cog.player_service.get_player, interaction.user.id, guild_id)
    recipient = await asyncio.to_thread(cog.player_service.get_player, player.id, guild_id)

    if not sender:
        await interaction.followup.send(
            "You need to `/player register` before you can tip.",
            ephemeral=True,
        )
        return

    if not recipient:
        await interaction.followup.send(
            f"{player.mention} is not registered.",
            ephemeral=True,
        )
        return

    # Get mana effects for sender
    mana_effects_service = getattr(cog.bot, "mana_effects_service", None)
    effects = None
    if mana_effects_service:
        try:
            from domain.models.mana_effects import ManaEffects as _METip
            _fx_tip = await asyncio.to_thread(mana_effects_service.get_effects, interaction.user.id, guild_id)
            if isinstance(_fx_tip, _METip):
                effects = _fx_tip
        except Exception:
            effects = None

    # Calculate fee (1% minimum 1 coin, rounded up)
    # Plains: free tips (0% fee)
    if effects and effects.color == "White" and effects.plains_tip_fee_rate is not None:
        fee = 0
    else:
        fee = max(1, math.ceil(amount * TIP_FEE_RATE))

    # Plains tithe: extra debit on sender, also to the reserve. Folded into
    # tip_atomic so the sender debit and reserve credit commit together.
    tithe = 0
    if effects and effects.plains_tithe_rate > 0:
        tithe = max(1, int(amount * effects.plains_tithe_rate))

    total_cost = amount + fee + tithe

    # Check sender balance first (most fundamental constraint)
    sender_balance = await asyncio.to_thread(cog.player_service.get_balance, interaction.user.id, guild_id)
    if sender_balance < total_cost:
        cost_breakdown = f"{amount} tip + {fee} fee"
        if tithe:
            cost_breakdown += f" + {tithe} tithe"
        await interaction.followup.send(
            f"Insufficient balance. You need {total_cost} {JOPACOIN_EMOTE} "
            f"({cost_breakdown}). You have {sender_balance} {JOPACOIN_EMOTE}.",
            ephemeral=True,
        )
        return

    # Check if sender has outstanding loan (blocked from tipping)
    if cog.loan_service:
        loan_state = await asyncio.to_thread(cog.loan_service.get_state, interaction.user.id, guild_id)
        if loan_state.has_outstanding_loan:
            await interaction.followup.send(
                f"You cannot tip while you have an outstanding loan. "
                f"Play a match to repay your loan ({loan_state.outstanding_total} {JOPACOIN_EMOTE}).",
                ephemeral=True,
            )
            return

    # Atomic transfer: sender debit + recipient credit + reserve credit
    # (fee + tithe) all commit together.
    try:
        await asyncio.to_thread(
            functools.partial(
                cog.player_service.tip_atomic,
                from_discord_id=interaction.user.id,
                to_discord_id=player.id,
                guild_id=guild_id,
                amount=amount,
                fee=fee,
                tithe=tithe,
            )
        )
    except ValueError as exc:
        await interaction.followup.send(f"{exc}", ephemeral=True)
        return
    except Exception as exc:
        logger.error(f"Failed to process tip transfer: {exc}", exc_info=True)
        await interaction.followup.send(
            "Failed to process tip. Please try again.",
            ephemeral=True,
        )
        return

    # Mana post-effects on tip. These run as separate, non-atomic balance
    # adjustments after the transfer commits; guard each so a failure logs
    # loudly instead of silently leaving partially-applied money drift.
    mana_notes = []
    if effects and mana_effects_service:
        # Green steady bonus: recipient gets +1 JC
        if effects.green_steady_bonus > 0:
            try:
                await asyncio.to_thread(cog.player_service.adjust_balance, player.id, guild_id, effects.green_steady_bonus)
                mana_notes.append(f"🌲 +{effects.green_steady_bonus} bonus to recipient")
            except Exception:
                logger.error("Tip green_steady_bonus adjustment failed", exc_info=True)

        # Swamp self-tax
        if effects.swamp_self_tax > 0:
            try:
                await asyncio.to_thread(cog.player_service.adjust_balance, interaction.user.id, guild_id, -effects.swamp_self_tax)
                mana_notes.append(f"🌿 Swamp tax: -{effects.swamp_self_tax}")
            except Exception:
                logger.error("Tip swamp_self_tax adjustment failed", exc_info=True)

        # Swamp siphon
        if effects.swamp_siphon:
            try:
                siphon = await asyncio.to_thread(mana_effects_service.execute_siphon, interaction.user.id, guild_id)
                if siphon:
                    mana_notes.append(f"🌿 Siphon: +{siphon['amount']}")
            except Exception:
                logger.error("Tip swamp_siphon adjustment failed", exc_info=True)

        if tithe:
            mana_notes.append(f"🌾 Tithe: -{tithe}")

    mana_suffix = ""
    if mana_notes:
        mana_suffix = "\n" + " | ".join(mana_notes)

    # Transfer succeeded - send success message (with mana badge for sender)
    from utils.mana_display import resolve_mana_badge
    _tip_badge = await resolve_mana_badge(cog.bot, interaction.user.id, guild_id)
    _tip_prefix = f"{_tip_badge} " if _tip_badge else ""
    await interaction.followup.send(
        f"{_tip_prefix}{interaction.user.mention} tipped {amount} {JOPACOIN_EMOTE} to {player.mention}! "
        f"({fee} {JOPACOIN_EMOTE} fee to Jopacoin Reserve){mana_suffix}",
        ephemeral=False,
    )

    # Witch's Curse: tipping out is degen-coded → loss-rated for the sender.
    curse_service = getattr(cog.bot, "curse_service", None)
    if curse_service is not None and interaction.channel is not None:
        from services.curse_service import spawn_curse_flame
        spawn_curse_flame(
            curse_service,
            interaction.channel,
            target_id=interaction.user.id,
            guild_id=guild_id,
            system="tip",
            outcome="loss",
            event_context={"amount": amount, "recipient_id": player.id},
            target_display_name=getattr(interaction.user, "display_name", None),
        )

    # Neon Degen Terminal hook
    neon = cog._get_neon_service()
    if neon:
        neon_result = await neon.on_tip(
            interaction.user.id, guild_id,
            sender_name=interaction.user.name,
            recipient_name=player.name,
            amount=amount,
            fee=fee,
        )
        await send_neon_result(interaction, neon_result)

    # Log the transaction (non-critical - failure here doesn't affect the tip)
    if cog.tip_service:
        try:
            await asyncio.to_thread(
                functools.partial(
                    cog.tip_service.log_tip,
                    sender_id=interaction.user.id,
                    recipient_id=player.id,
                    amount=amount,
                    fee=fee,
                    guild_id=guild_id,
                )
            )
        except Exception as log_exc:
            # Log failure but don't notify user - tip already succeeded
            logger.warning(f"Failed to log tip transaction: {log_exc}")



async def paydebt_action(
    cog: BettingCommands, interaction: discord.Interaction, player: discord.Member, amount: int,
) -> None:
    guild = interaction.guild if interaction.guild else None
    rl_gid = guild.id if guild else 0
    rl = GLOBAL_RATE_LIMITER.check(
        scope="paydebt",
        guild_id=rl_gid,
        user_id=interaction.user.id,
        limit=5,
        per_seconds=10,
    )
    if not rl.allowed:
        await interaction.response.send_message(
            f"Please wait {rl.retry_after_seconds}s before using `/paydebt` again.",
            ephemeral=True,
        )
        return

    # Validate amount
    if amount <= 0:
        await interaction.response.send_message(
            "Amount must be positive.",
            ephemeral=True,
        )
        return

    # Check if paying their own debt
    if player.id == interaction.user.id:
        await interaction.response.send_message(
            "You cannot pay your own debt.",
            ephemeral=True,
        )
        return

    # Always public since helping another player
    if not await safe_defer(interaction, ephemeral=False):
        return

    guild_id = guild.id if guild else None
    try:
        result = await asyncio.to_thread(
            functools.partial(
                cog.player_service.pay_debt_atomic,
                from_discord_id=interaction.user.id,
                to_discord_id=player.id,
                guild_id=guild_id,
                amount=amount,
            )
        )

        await interaction.followup.send(
            f"{interaction.user.mention} paid {result['amount_paid']} {JOPACOIN_EMOTE} "
            f"toward {player.mention}'s debt!",
            ephemeral=False,
        )
    except ValueError as exc:
        await interaction.followup.send(f"{exc}", ephemeral=True)



async def bankruptcy_action(
    cog: BettingCommands, interaction: discord.Interaction,
) -> None:
    guild = interaction.guild if interaction.guild else None
    rl_gid = guild.id if guild else 0
    rl = GLOBAL_RATE_LIMITER.check(
        scope="bankruptcy",
        guild_id=rl_gid,
        user_id=interaction.user.id,
        limit=2,
        per_seconds=30,
    )
    if not rl.allowed:
        await interaction.response.send_message(
            f"The bankruptcy court requires you to wait {rl.retry_after_seconds}s "
            "before filing again.",
            ephemeral=True,
        )
        return

    if not await safe_defer(interaction, ephemeral=False):
        return

    if not cog.bankruptcy_service:
        await interaction.followup.send("Bankruptcy service is not available.", ephemeral=True)
        return

    user_id = interaction.user.id
    guild_id = guild.id if guild else None

    # Check if player is registered
    player = await asyncio.to_thread(cog.player_service.get_player, user_id, guild_id)
    if not player:
        await interaction.followup.send(
            "You need to `/player register` before you can declare bankruptcy. "
            "Though maybe that's a good sign you shouldn't gamble.",
            ephemeral=True,
        )
        return

    # Check if bankruptcy is allowed
    check = await asyncio.to_thread(cog.bankruptcy_service.validate_bankruptcy, user_id, guild_id)

    if not check.success:
        from services import error_codes
        if check.error_code == error_codes.NOT_IN_DEBT:
            message = random.choice(BANKRUPTCY_DENIED_MESSAGES)
            balance = await asyncio.to_thread(cog.player_service.get_balance, user_id, guild_id)
            await interaction.followup.send(
                f"{interaction.user.mention} tried to declare bankruptcy...\n\n"
                f"{message}\n\nTheir balance: {balance} {JOPACOIN_EMOTE}",
                ephemeral=False,
            )
            return
        elif check.error_code == error_codes.BANKRUPTCY_COOLDOWN:
            message = random.choice(BANKRUPTCY_COOLDOWN_MESSAGES)
            state = await asyncio.to_thread(cog.bankruptcy_service.get_state, user_id, guild_id)
            cooldown_ends = state.cooldown_ends_at
            cooldown_str = f"<t:{cooldown_ends}:R>" if cooldown_ends else "soon"
            await interaction.followup.send(
                f"{interaction.user.mention} tried to declare bankruptcy again...\n\n"
                f"{message}\n\nThey can file again {cooldown_str}.",
                ephemeral=False,
            )
            # Neon Degen Terminal hook (cooldown hit)
            neon = cog._get_neon_service()
            if neon:
                try:
                    neon_result = await neon.on_cooldown_hit(user_id, guild_id, "bankruptcy")
                    await send_neon_result(interaction, neon_result)
                except Exception as e:
                    logger.debug("Failed to send bankruptcy cooldown neon result: %s", e)
            return

    # Declare bankruptcy
    result = await asyncio.to_thread(cog.bankruptcy_service.execute_bankruptcy, user_id, guild_id)

    if not result.success:
        await interaction.followup.send(
            "Something went wrong with your bankruptcy filing. The universe is cruel.",
            ephemeral=True,
        )
        return

    decl = result.value

    # Swamp mana: reduced bankruptcy penalty (3 games instead of 5)
    _mana_fx_bk = getattr(cog.bot, "mana_effects_service", None)
    if _mana_fx_bk:
        try:
            from domain.models.mana_effects import ManaEffects as _MEBk
            _bk_effects = await asyncio.to_thread(_mana_fx_bk.get_effects, user_id, guild_id)
            if not isinstance(_bk_effects, _MEBk):
                _bk_effects = None
        except Exception:
            _bk_effects = None
        if _bk_effects and _bk_effects.color == "Black" and _bk_effects.swamp_bankruptcy_games < decl.penalty_games:
            # Reduce penalty games to swamp level
            reduction = decl.penalty_games - _bk_effects.swamp_bankruptcy_games
            await asyncio.to_thread(
                cog.bankruptcy_service.add_penalty_games, user_id, guild_id, -reduction
            )
            decl = type(decl)(
                debt_cleared=decl.debt_cleared,
                penalty_games=_bk_effects.swamp_bankruptcy_games,
                penalty_rate=decl.penalty_rate,
                new_balance=decl.new_balance,
            )

    # Format success message
    message = random.choice(BANKRUPTCY_SUCCESS_MESSAGES).format(
        debt=decl.debt_cleared,
        games=decl.penalty_games,
        rate=int(decl.penalty_rate * 100),
    )

    # Try to get AI-generated flavor text
    ai_flavor = None
    if cog.flavor_text_service:
        try:
            ai_flavor = await cog.flavor_text_service.generate_event_flavor(
                guild_id=guild_id,
                event=FlavorEvent.BANKRUPTCY_DECLARED,
                discord_id=user_id,
                event_details={
                    "debt_cleared": decl.debt_cleared,
                    "penalty_games": decl.penalty_games,
                    "penalty_rate": decl.penalty_rate,
                },
            )
        except Exception as e:
            logger.warning(f"Failed to generate AI flavor for bankruptcy: {e}")

    penalty_rate_pct = int(decl.penalty_rate * 100)
    flavor_line = f"\n\n*{ai_flavor}*" if ai_flavor else ""
    await interaction.followup.send(
        f"**{interaction.user.mention} HAS DECLARED BANKRUPTCY**\n\n"
        f"{message}{flavor_line}\n\n"
        f"**Details:**\n"
        f"Debt cleared: {decl.debt_cleared} {JOPACOIN_EMOTE}\n"
        f"Penalty: {penalty_rate_pct}% win bonus until you **WIN** {decl.penalty_games} games\n"
        f"New balance: 0 {JOPACOIN_EMOTE}",
        ephemeral=False,
    )

    # Neon Degen Terminal hook - at most ONE neon event per /bankruptcy action
    neon = cog._get_neon_service()
    if neon:
        filing_number = await cog._get_bankruptcy_filing_number(user_id, guild_id)
        degen_score = neon._get_degen_score(user_id, guild_id)

        candidates = [
            lambda: neon.on_bankruptcy(
                user_id, guild_id,
                debt_cleared=decl.debt_cleared,
                filing_number=filing_number,
            ),
        ]
        if degen_score is not None and degen_score >= 90:
            candidates.append(
                lambda: neon.on_degen_milestone(user_id, guild_id, degen_score)
            )

        await cog._send_first_neon_result(interaction, *candidates)



async def loan_action(
    cog: BettingCommands, interaction: discord.Interaction, amount: int,
) -> None:
    """Take out a loan. You receive the full amount but owe amount + fee."""
    if not cog.loan_service:
        await interaction.response.send_message(
            "Loan service is not available.", ephemeral=True
        )
        return

    user_id = interaction.user.id
    guild_id = interaction.guild.id if interaction.guild else None

    # Check if registered
    if not await asyncio.to_thread(cog.player_service.get_player, user_id, guild_id):
        await interaction.response.send_message(
            "You need to `/player register` before taking loans.", ephemeral=True
        )
        return

    # Defer early - AI flavor text calls below can take several seconds
    await interaction.response.defer()

    # Mana modifies fee rate / max amount per-call without mutating the service.
    mana_effects_service = getattr(cog.bot, "mana_effects_service", None)
    loan_fee_override: float | None = None
    loan_max_override: int | None = None
    if mana_effects_service is not None:
        try:
            _mana_loan = await asyncio.to_thread(
                mana_effects_service.apply_loan_modifiers,
                user_id,
                guild_id,
                base_fee_rate=cog.loan_service.fee_rate,
                base_limit=cog.loan_service.max_amount,
            )
            if _mana_loan["color"] is not None:
                loan_fee_override = _mana_loan["fee_rate"]
                loan_max_override = _mana_loan["limit"]
        except Exception:
            logger.debug("Mana loan modifier lookup failed", exc_info=True)

    # Check eligibility
    from services import error_codes as _ec
    check = await asyncio.to_thread(
        cog.loan_service.validate_loan,
        user_id, amount, guild_id,
        fee_rate_override=loan_fee_override,
        max_amount_override=loan_max_override,
    )

    if not check.success:
        if check.error_code == _ec.LOAN_ALREADY_EXISTS:
            state = await asyncio.to_thread(cog.loan_service.get_state, user_id, guild_id)
            await interaction.followup.send(
                f"You already have an outstanding loan of **{state.outstanding_total}** {JOPACOIN_EMOTE} "
                f"(principal: {state.outstanding_principal}, fee: {state.outstanding_fee}).\n\n"
                "Repay it by playing in a match first!",
            )
            return
        elif check.error_code == _ec.COOLDOWN_ACTIVE:
            state = await asyncio.to_thread(cog.loan_service.get_state, user_id, guild_id)
            remaining = state.cooldown_ends_at - int(__import__("time").time())
            hours = remaining // 3600
            minutes = (remaining % 3600) // 60
            # Try AI flavor, fallback to static message
            msg = None
            if cog.flavor_text_service:
                try:
                    msg = await cog.flavor_text_service.generate_event_flavor(
                        guild_id=guild_id,
                        event=FlavorEvent.LOAN_COOLDOWN,
                        discord_id=user_id,
                        event_details={
                            "cooldown_remaining_hours": hours,
                            "cooldown_remaining_minutes": minutes,
                            "requested_amount": amount,
                        },
                    )
                except Exception as e:
                    logger.warning(f"Failed to generate AI flavor for loan cooldown: {e}")
            if not msg:
                msg = random.choice(LOAN_DENIED_COOLDOWN_MESSAGES)
            await interaction.followup.send(
                f"{msg}\n\n⏳ Cooldown ends in **{hours}h {minutes}m**.",
            )
            # Neon Degen Terminal hook (cooldown hit)
            neon = cog._get_neon_service()
            if neon:
                try:
                    neon_result = await neon.on_cooldown_hit(user_id, guild_id, "loan")
                    await send_neon_result(interaction, neon_result)
                except Exception as e:
                    logger.debug("Failed to send loan cooldown neon result: %s", e)
            return
        elif check.error_code == _ec.LOAN_AMOUNT_EXCEEDED:
            await interaction.followup.send(check.error)
            return
        else:
            await interaction.followup.send(check.error)
            return

    # Take the loan
    loan_result = await asyncio.to_thread(
        cog.loan_service.execute_loan,
        user_id, amount, guild_id,
        fee_rate_override=loan_fee_override,
        max_amount_override=loan_max_override,
    )

    if not loan_result.success:
        await interaction.followup.send(
            "Failed to process loan. Please try again.", ephemeral=True
        )
        return

    result = loan_result.value

    fee_pct = int(LOAN_FEE_RATE * 100)

    # Try to get AI-generated flavor text
    ai_flavor = None
    if cog.flavor_text_service:
        event_type = (
            FlavorEvent.NEGATIVE_LOAN
            if result.was_negative_loan
            else FlavorEvent.LOAN_TAKEN
        )
        try:
            ai_flavor = await cog.flavor_text_service.generate_event_flavor(
                guild_id=guild_id,
                event=event_type,
                discord_id=user_id,
                event_details={
                    "amount": result.amount,
                    "fee": result.fee,
                    "total_owed": result.total_owed,
                    "new_balance": result.new_balance,
                    "total_loans_taken": result.total_loans_taken,
                    "was_negative_loan": result.was_negative_loan,
                },
            )
        except Exception as e:
            logger.warning(f"Failed to generate AI flavor for loan: {e}")

    # Check if this was a negative loan (peak degen behavior)
    if result.was_negative_loan:
        # Use AI flavor as main message if available, otherwise fallback to static
        if ai_flavor:
            msg = ai_flavor
        else:
            msg = random.choice(NEGATIVE_LOAN_MESSAGES).format(
                amount=result.amount,
                emote=JOPACOIN_EMOTE,
            )
        embed = discord.Embed(
            title="🎪 LEGENDARY DEGEN MOVE 🎪",
            description=msg,
            color=0x9B59B6,  # Purple for peak degen
        )
        from utils.mana_display import resolve_mana_badge
        _loan_badge = await resolve_mana_badge(cog.bot, user_id, guild_id)
        if _loan_badge:
            embed.title = f"{_loan_badge} {embed.title}"
        embed.add_field(
            name="The Damage",
            value=(
                f"Borrowed: **{result.amount}** {JOPACOIN_EMOTE}\n"
                f"Fee ({fee_pct}%): **{result.fee}** {JOPACOIN_EMOTE}\n"
                f"Total Owed: **{result.total_owed}** {JOPACOIN_EMOTE}\n"
                f"New Balance: **{result.new_balance}** {JOPACOIN_EMOTE}"
            ),
            inline=False,
        )
        embed.add_field(
            name="⚠️ Repayment",
            value="You will repay the full amount **after your next match**.",
            inline=False,
        )
        embed.set_footer(
            text=f"Loan #{result.total_loans_taken} | Go bet it all, you beautiful degen"
        )
    else:
        # Use AI flavor as main message if available, otherwise fallback to static
        if ai_flavor:
            msg = ai_flavor
        else:
            msg = random.choice(LOAN_SUCCESS_MESSAGES).format(
                amount=result.amount,
                owed=result.total_owed,
                fee=result.fee,
                emote=JOPACOIN_EMOTE,
            )
        embed = discord.Embed(
            title="🏦 Loan Approved",
            description=msg,
            color=0x2ECC71,  # Green
        )
        from utils.mana_display import resolve_mana_badge
        _loan_badge = await resolve_mana_badge(cog.bot, user_id, guild_id)
        if _loan_badge:
            embed.title = f"{_loan_badge} {embed.title}"
        embed.add_field(
            name="Details",
            value=(
                f"Borrowed: **{result.amount}** {JOPACOIN_EMOTE}\n"
                f"Fee ({fee_pct}%): **{result.fee}** {JOPACOIN_EMOTE}\n"
                f"Total Owed: **{result.total_owed}** {JOPACOIN_EMOTE}\n"
                f"New Balance: **{result.new_balance}** {JOPACOIN_EMOTE}"
            ),
            inline=False,
        )
        embed.add_field(
            name="📅 Repayment",
            value="You will repay the full amount **after your next match**.",
            inline=False,
        )
        embed.set_footer(
            text=f"Loan #{result.total_loans_taken} | Fee deposited to Jopacoin Reserve"
        )

    # Mana post-effects on loan
    mana_notes_loan = []
    _mana_fx_loan = getattr(cog.bot, "mana_effects_service", None)
    _loan_effects = None
    if _mana_fx_loan:
        try:
            from domain.models.mana_effects import ManaEffects as _MELoan
            _loan_effects_raw = await asyncio.to_thread(_mana_fx_loan.get_effects, user_id, guild_id)
            if isinstance(_loan_effects_raw, _MELoan):
                _loan_effects = _loan_effects_raw
        except Exception:
            pass
        if _loan_effects:
            # Swamp self-tax. Runs as a separate, non-atomic adjustment after
            # the loan commits; guard it so a failure logs loudly instead of
            # silently leaving partially-applied money drift.
            if _loan_effects.swamp_self_tax > 0:
                try:
                    await asyncio.to_thread(cog.player_service.adjust_balance, user_id, guild_id, -_loan_effects.swamp_self_tax)
                    mana_notes_loan.append(f"🌿 Swamp tax: -{_loan_effects.swamp_self_tax}")
                except Exception:
                    logger.error("Loan swamp_self_tax adjustment failed", exc_info=True)
            # Swamp siphon
            if _loan_effects.swamp_siphon:
                try:
                    siphon = await asyncio.to_thread(_mana_fx_loan.execute_siphon, user_id, guild_id)
                    if siphon:
                        mana_notes_loan.append(f"🌿 Siphon: +{siphon['amount']}")
                except Exception:
                    logger.error("Loan swamp_siphon adjustment failed", exc_info=True)
    _loan_mana_suffix = ""
    if mana_notes_loan:
        _loan_mana_suffix = "\n" + " | ".join(mana_notes_loan)

    if _loan_mana_suffix:
        embed.add_field(name="Mana Effects", value=_loan_mana_suffix.strip(), inline=False)

    await interaction.followup.send(embed=embed)

    # Witch's Curse: taking a loan is degen-coded → loss-rated.
    curse_service = getattr(cog.bot, "curse_service", None)
    if curse_service is not None and interaction.channel is not None:
        from services.curse_service import spawn_curse_flame
        spawn_curse_flame(
            curse_service,
            interaction.channel,
            target_id=user_id,
            guild_id=guild_id,
            system="loan",
            outcome="loss",
            event_context={
                "amount": result.amount,
                "fee": result.fee,
                "total_owed": result.total_owed,
                "was_negative_loan": result.was_negative_loan,
            },
            target_display_name=getattr(interaction.user, "display_name", None),
        )

    # Neon Degen Terminal hook
    neon = cog._get_neon_service()
    if neon:
        neon_result = await neon.on_loan(
            user_id, guild_id,
            amount=result.amount,
            total_owed=result.total_owed,
            is_negative=result.was_negative_loan,
        )
        await send_neon_result(interaction, neon_result)



async def nonprofit_action(
    cog: BettingCommands, interaction: discord.Interaction,
) -> None:
    """View the server operations budget held in reserve."""
    if not cog.loan_service:
        await interaction.response.send_message(
            "Loan service is not available.", ephemeral=True
        )
        return

    if not await safe_defer(interaction, ephemeral=False):
        return

    guild_id = interaction.guild.id if interaction.guild else None
    total = await asyncio.to_thread(cog.loan_service.get_nonprofit_fund, guild_id)

    # Check for active proposal with reserved funds
    reserved = 0
    if cog.disburse_service:
        proposal = await asyncio.to_thread(cog.disburse_service.get_proposal, guild_id)
        if proposal:
            reserved = proposal.fund_amount

    embed = discord.Embed(
        title="🏛️ Jopacoin Reserve",
        description=(
            "The reserve is the server operations budget. Fees, fines, and "
            "economy sinks collect here before Tax Men allocate the budget.\n\n"
            "*A public balance sheet for server business.*"
        ),
        color=0xE91E63,  # Pink
    )

    if reserved > 0:
        embed.add_field(
            name="Available Budget",
            value=f"**{total}** {JOPACOIN_EMOTE}",
            inline=True,
        )
        embed.add_field(
            name="Reserved Budget",
            value=f"**{reserved}** {JOPACOIN_EMOTE}",
            inline=True,
        )
        embed.add_field(
            name="Total",
            value=f"**{total + reserved}** {JOPACOIN_EMOTE}",
            inline=True,
        )
    else:
        embed.add_field(
            name="Available Budget",
            value=f"**{total}** {JOPACOIN_EMOTE}",
            inline=False,
        )

    # Show status based on fund level (including reserved)
    effective_total = total + reserved
    if effective_total >= DISBURSE_MIN_FUND:
        if reserved > 0:
            status_value = f"Proposal active ({reserved} reserved)"
        else:
            status_value = f"Ready for allocation! (min: {DISBURSE_MIN_FUND})"
    else:
        status_value = f"Collecting... ({effective_total}/{DISBURSE_MIN_FUND} needed)"

    embed.add_field(
        name="Status",
        value=status_value,
        inline=True,
    )

    # Show last disbursement info if available
    if cog.disburse_service:
        last_disburse = await asyncio.to_thread(cog.disburse_service.get_last_disbursement, guild_id)
        if last_disburse:
            time_str = f"<t:{last_disburse['disbursed_at']}:R>"

            # Format recipients
            recipients = last_disburse["recipients"]
            if recipients:
                # Show up to 3 recipients
                recipient_strs = []
                for discord_id, amount in recipients[:3]:
                    recipient_strs.append(f"<@{discord_id}>: +{amount}")
                if len(recipients) > 3:
                    recipient_strs.append(f"+{len(recipients) - 3} more")
                recipients_text = "\n".join(recipient_strs)
            else:
                recipients_text = "No recipients"

            method_labels = {
                "even": "Even Split",
                "proportional": "Proportional",
                "neediest": "Neediest First",
            }
            method_label = method_labels.get(
                last_disburse["method"], last_disburse["method"]
            )

            embed.add_field(
                name="Last Disbursement",
                value=(
                    f"**{last_disburse['total_amount']}** {JOPACOIN_EMOTE} "
                    f"via {method_label}\n{time_str}\n{recipients_text}"
                ),
                inline=False,
            )

    embed.set_footer(text="Use /disburse propose to start a reserve allocation vote!")

    await safe_followup(interaction, embed=embed)

