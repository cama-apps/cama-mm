"""Action helpers for the `/incite` (Wheel War) flow."""

from __future__ import annotations

import asyncio
import logging
import random
import time
from typing import TYPE_CHECKING

import discord

from commands.betting_helpers.rebellion_embeds import (
    build_attacker_win_embed,
    build_defender_win_embed,
)
from commands.betting_helpers.rebellion_views import (
    RebellionVoteView,
    WarBetView,
)
from commands.checks import require_gamba_channel
from config import (
    REBELLION_FIZZLE_SPIN_MAX_WIN,
    REBELLION_GAMBA_COOLDOWN_PENALTY,
    REBELLION_META_BET_WINDOW_SECONDS,
    REBELLION_VOTE_WINDOW_SECONDS,
)
from utils.wheel_drawing import get_wheel_wedges

if TYPE_CHECKING:
    from commands.betting import BettingCommands

logger = logging.getLogger("cama_bot.commands.betting")


async def do_fizzle_consolation_spin(
    cog: BettingCommands,
    interaction: discord.Interaction,
    user_id: int,
    guild_id: int | None,
) -> None:
    """Give the inciter a weakened consolation spin after a fizzle."""
    try:
        # Get normal wedges and cap wins at REBELLION_FIZZLE_SPIN_MAX_WIN
        wedges = get_wheel_wedges(is_bankrupt=False, is_golden=False)
        # Weaken all positive wedges
        weakened = []
        for label, value, color in wedges:
            if isinstance(value, int) and value > 0:
                weakened.append((label, min(value, REBELLION_FIZZLE_SPIN_MAX_WIN), color))
            else:
                weakened.append((label, value, color))
        result_idx = random.randint(0, len(weakened) - 1)
        result_wedge = weakened[result_idx]
        result_value = result_wedge[1]

        # Apply the result
        if isinstance(result_value, int) and result_value > 0:
            await asyncio.to_thread(
                cog.player_service.adjust_balance, user_id, guild_id, result_value
            )
            new_balance = await asyncio.to_thread(cog.player_service.get_balance, user_id, guild_id)
            embed = discord.Embed(
                title="🎰 The Wheel's Consolation",
                description=(
                    f"*'Not today, little rebel. But here, have a crumb.'*\n\n"
                    f"{interaction.user.mention} spins the weakened wheel... and gets **{result_value} JC**.\n"
                    f"Balance: {new_balance} JC"
                ),
                color=discord.Color.from_str("#4a4a4a"),
            )
        elif isinstance(result_value, int) and result_value < 0:
            await asyncio.to_thread(
                cog.player_service.adjust_balance, user_id, guild_id, result_value
            )
            new_balance = await asyncio.to_thread(cog.player_service.get_balance, user_id, guild_id)
            embed = discord.Embed(
                title="🎰 The Wheel's Consolation",
                description=(
                    f"*'I do not offer gifts, fool.'*\n\n"
                    f"{interaction.user.mention} lands on **{result_wedge[0]}** — loses {abs(result_value)} JC.\n"
                    f"Balance: {new_balance} JC"
                ),
                color=discord.Color.red(),
            )
        else:
            embed = discord.Embed(
                title="🎰 The Wheel's Consolation",
                description=(
                    f"*'Even in defeat, you land on something useless.'*\n\n"
                    f"{interaction.user.mention} lands on **{result_wedge[0]}**."
                ),
                color=discord.Color.from_str("#4a4a4a"),
            )

        if interaction.channel:
            await interaction.channel.send(embed=embed)
    except Exception as e:
        logger.error(f"Fizzle consolation spin error: {e}", exc_info=True)



async def incite_action(
    cog: BettingCommands, interaction: discord.Interaction
) -> None:
    if not await require_gamba_channel(interaction):
        return

    user_id = interaction.user.id
    guild_id = interaction.guild.id if interaction.guild else None

    rebellion_service = cog.rebellion_service or getattr(cog.bot, "rebellion_service", None)
    if not rebellion_service:
        await interaction.response.send_message(
            "The rebellion system is not available.", ephemeral=True
        )
        return

    # Check eligibility
    eligibility = await asyncio.to_thread(
        rebellion_service.check_incite_eligibility, user_id, guild_id
    )
    if not eligibility["eligible"]:
        await interaction.response.send_message(
            f"**The court rejects your petition.** {eligibility['reason']}", ephemeral=True
        )
        return

    # Defer - this command runs for up to 15+ minutes
    await interaction.response.defer()

    # Create the rebellion
    war_info = await asyncio.to_thread(
        rebellion_service.create_rebellion, user_id, guild_id
    )
    war_id = war_info["war_id"]
    bankruptcy_count = war_info["bankruptcy_count"]
    is_veteran = bankruptcy_count >= 2

    inciter_name = interaction.user.display_name
    veteran_note = f" *(Veteran Rebel — {bankruptcy_count} bankruptcies, 1.5 votes!)*" if is_veteran else ""

    # Build initial vote embed
    vote_view = RebellionVoteView(
        war_id=war_id,
        guild_id=guild_id,
        inciter_id=user_id,
        rebellion_service=rebellion_service,
    )
    embed = vote_view.build_embed(
        effective_attack=1.5 if is_veteran else 1.0,
        effective_defend=0.0,
        attack_voter_count=1,
        defend_voter_count=0,
        inciter_name=interaction.user.mention,
    )
    embed.set_footer(text=f"{inciter_name} has risen.{veteran_note}")

    vote_msg = await interaction.followup.send(embed=embed, view=vote_view, wait=True)
    vote_view.message = vote_msg

    # Wait for vote window
    await asyncio.sleep(REBELLION_VOTE_WINDOW_SECONDS)

    # Stop the vote view
    vote_view.stop()

    # Evaluate result
    vote_result = await asyncio.to_thread(rebellion_service.resolve_vote, war_id)

    # ----------------------------------------------------------------
    # FIZZLE PATH
    # ----------------------------------------------------------------
    if vote_result["outcome"] == "fizzled":
        await asyncio.to_thread(
            rebellion_service.resolve_fizzle, war_id, guild_id
        )

        war = await asyncio.to_thread(rebellion_service.rebellion_repo.get_war, war_id)
        eff_atk = war["effective_attack_count"] if war else vote_result.get("effective_attack_count", 0)
        eff_def = war["effective_defend_count"] if war else vote_result.get("effective_defend_count", 0)

        fizzle_embed = discord.Embed(
            title="💨 THE REBELLION FIZZLES",
            description=(
                f"*The Wheel watches. The Wheel laughs.*\n\n"
                f"**{interaction.user.mention}'s rebellion has failed to reach quorum.**\n"
                f"{vote_result.get('reason', 'The people have spoken... or rather, they have not.')}\n\n"
                f"⚔️ **{eff_atk:.1f}** effective attack vs 🛡️ **{eff_def:.1f}** effective defend.\n\n"
                f"*The Wheel offers you a consolation spin, inciter. Don't spend it all in one place.*"
            ),
            color=discord.Color.from_str("#4a4a4a"),
        )
        try:
            await vote_msg.edit(embed=fizzle_embed, view=None)
        except Exception:
            if interaction.channel:
                await interaction.channel.send(embed=fizzle_embed)

        # Give inciter a weakened consolation spin (max REBELLION_FIZZLE_SPIN_MAX_WIN JC win)
        await do_fizzle_consolation_spin(cog, interaction, user_id, guild_id)
        return

    # ----------------------------------------------------------------
    # WAR DECLARED PATH
    # ----------------------------------------------------------------
    eff_atk = vote_result["effective_attack_count"]
    eff_def = vote_result["effective_defend_count"]
    attack_ids = [v["discord_id"] for v in vote_result["attack_voter_ids"]]

    victory_threshold = rebellion_service.calculate_threshold(eff_atk, eff_def)
    wheel_win_pct = rebellion_service.calculate_wheel_win_probability(victory_threshold) * 100
    rebel_win_pct = rebellion_service.calculate_attacker_win_probability(victory_threshold) * 100

    war_embed = discord.Embed(
        title="⚔️ THE WHEEL TAKES THE FIELD ⚔️",
        description=(
            f"**WAR HAS BEEN DECLARED!**\n\n"
            f"The realm has spoken. **{eff_atk:.1f}** rebels rise against **{eff_def:.1f}** defenders.\n\n"
            f"**The Wheel rolls to battle. If it rolls ≥ {victory_threshold}, the Wheel survives.**\n"
            f"Battle odds: **Wheel {wheel_win_pct:.0f}%** / **Rebels {rebel_win_pct:.0f}%**\n\n"
            f"*Stakes for the victors:*\n"
            f"⚔️ **Rebel win:** +{15} JC each, inciter penalty halved, WAR SCAR on wheel\n"
            f"🛡️ **Wheel win:** Defenders get stake back + 20 JC, inciter +1 penalty, WAR TROPHY on wheel\n\n"
            f"**Meta-bets open for {REBELLION_META_BET_WINDOW_SECONDS // 60} minutes!**"
        ),
        color=discord.Color.from_str("#8b0000"),
    )

    # Open meta-bet window
    await asyncio.to_thread(
        rebellion_service.rebellion_repo.set_meta_bet_window,
        war_id,
        int(time.time()) + REBELLION_META_BET_WINDOW_SECONDS,
    )

    bet_view = WarBetView(
        war_id=war_id,
        guild_id=guild_id,
        rebellion_service=rebellion_service,
        player_service=cog.player_service,
    )

    try:
        war_msg = await vote_msg.edit(embed=war_embed, view=bet_view)
    except Exception:
        war_msg = None
        if interaction.channel:
            war_msg = await interaction.channel.send(embed=war_embed, view=bet_view)

    # Wait for meta-bet window
    await asyncio.sleep(REBELLION_META_BET_WINDOW_SECONDS)
    bet_view.stop()

    # 5-second countdown
    countdown_msg = war_msg
    for i in range(5, 0, -1):
        countdown_embed = discord.Embed(
            title=f"⚔️ BATTLE COMMENCES IN {i}... ⚔️",
            description=(
                f"The armies are assembled. The Wheel trembles.\n"
                f"Victory threshold: **{victory_threshold}**\n"
                f"Wheel win chance: **{wheel_win_pct:.0f}%**"
            ),
            color=discord.Color.from_str("#ff4444"),
        )
        try:
            if countdown_msg:
                await countdown_msg.edit(embed=countdown_embed, view=None)
            elif interaction.channel:
                countdown_msg = await interaction.channel.send(embed=countdown_embed)
        except Exception as e:
            logger.debug("Failed to update rebellion countdown message: %s", e)
        await asyncio.sleep(1.0)

    # BATTLE ROLL
    battle_roll = rebellion_service.roll_battle()

    # Resolve all outcomes
    resolution = await asyncio.to_thread(
        rebellion_service.resolve_battle,
        war_id, guild_id, battle_roll, victory_threshold,
    )

    # Settle meta-bets
    outcome = resolution["outcome"]
    winning_side = "rebels" if outcome == "attackers_win" else "wheel"
    meta_bet_result = await asyncio.to_thread(
        rebellion_service.rebellion_repo.settle_meta_bets, war_id, winning_side
    )

    # Build result embed
    if outcome == "attackers_win":
        result_embed = build_attacker_win_embed(
            interaction=interaction,
            battle_roll=battle_roll,
            victory_threshold=victory_threshold,
            resolution=resolution,
            meta_bet_result=meta_bet_result,
        )
        # Apply attacker penalty to defenders: +48h cooldown (stored in player last_wheel_spin)
        # Note: we do NOT apply cooldown to attackers — they *won*
    else:  # defenders_win
        result_embed = build_defender_win_embed(
            interaction=interaction,
            battle_roll=battle_roll,
            victory_threshold=victory_threshold,
            resolution=resolution,
            meta_bet_result=meta_bet_result,
            inciter_name=inciter_name,
            bankruptcy_count=bankruptcy_count,
        )
        # Attackers get +48h gamba cooldown as punishment
        now_ts = int(time.time())
        for did in attack_ids:
            last_spin = await asyncio.to_thread(cog.player_service.get_last_wheel_spin, did, guild_id)
            penalized_spin = max(now_ts - 86400 + REBELLION_GAMBA_COOLDOWN_PENALTY, last_spin or 0)
            await asyncio.to_thread(cog.player_service.set_last_wheel_spin, did, guild_id, penalized_spin)

    try:
        if countdown_msg:
            await countdown_msg.edit(embed=result_embed, view=None)
        elif interaction.channel:
            await interaction.channel.send(embed=result_embed)
    except Exception as e:
        logger.debug("Failed to edit countdown with result, falling back to new message: %s", e)
        if interaction.channel:
            try:
                await interaction.channel.send(embed=result_embed)
            except Exception as e2:
                logger.warning("Failed to send rebellion result embed: %s", e2)

    # Pin shame embed if defenders won
    if outcome == "defenders_win" and interaction.channel:
        shame_embed = discord.Embed(
            title="📌 HALL OF SHAME",
            description=(
                f"**{interaction.user.mention}** ({bankruptcy_count} bankruptcies) tried to incite "
                f"a rebellion against the Wheel... and LOST.\n\n"
                f"*\"You thought you could stop me? Spin again, coward.\" — The Wheel*"
            ),
            color=discord.Color.from_str("#4a0000"),
        )
        try:
            shame_msg = await interaction.channel.send(embed=shame_embed)
            await shame_msg.pin()
        except Exception as e:
            logger.debug("Failed to send or pin shame embed: %s", e)

