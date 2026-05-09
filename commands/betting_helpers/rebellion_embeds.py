"""Embed builders for the Wheel War (`/incite`) result screens."""

from __future__ import annotations

import discord


def build_attacker_win_embed(
    interaction: discord.Interaction,
    battle_roll: int,
    victory_threshold: int,
    resolution: dict,
    meta_bet_result: dict,
) -> discord.Embed:
    war_scar = resolution.get("war_scar_label", "unknown")
    embed = discord.Embed(
        title="🎉 THE REBELS TRIUMPH! THE WHEEL IS HUMILIATED! 🎉",
        description=(
            f"**The Wheel rolled {battle_roll}. Victory threshold was {victory_threshold}.**\n"
            f"*The Wheel crumbles before the righteous fury of the people!*\n\n"
            f"**Rewards:**\n"
            f"⚔️ **Inciter:** +{resolution.get('inciter_reward', 30)} JC — "
            f"Penalty games cut from {resolution.get('inciter_penalty_before', 0)} to {resolution.get('inciter_penalty_after', 0)}\n"
            f"⚔️ **All Attackers:** +{resolution.get('attacker_flat_reward', 15)} JC + equal share of defender stakes\n\n"
            f"**Wheel Effects (next 10 guild spins):**\n"
            f"💀 **WAR SCAR:** The {war_scar} JC wedge becomes 0 JC\n"
            f"🩹 **BANKRUPT weakened** (-25%)\n"
            f"🎁 **Free spin** for all guild members within 24 hours!\n\n"
            f"*Meta-bet pool: {meta_bet_result.get('total_pool', 0)} JC settled.*"
        ),
        color=discord.Color.green(),
    )
    return embed


def build_defender_win_embed(
    interaction: discord.Interaction,
    battle_roll: int,
    victory_threshold: int,
    resolution: dict,
    meta_bet_result: dict,
    inciter_name: str,
    bankruptcy_count: int,
) -> discord.Embed:
    embed = discord.Embed(
        title="🎰 THE WHEEL STANDS VICTORIOUS! THE REBELLION IS CRUSHED! 🎰",
        description=(
            f"**The Wheel rolled {battle_roll}. Victory threshold was {victory_threshold}.**\n"
            f"*The Wheel's iron grip is unbroken. The rebels scatter in disgrace.*\n\n"
            f"**Outcomes:**\n"
            f"🛡️ **Defenders:** Stake returned + 20 JC each\n"
            f"🏆 **Champion Defender:** Additional +10 JC\n"
            f"😤 **Inciter ({inciter_name}):** +1 penalty game (now {resolution.get('inciter_penalty_added', 1)} added)\n"
            f"⏰ **All Attackers:** +48h gamba cooldown as punishment\n\n"
            f"**Wheel Effects (next 10 guild spins):**\n"
            f"🏆 **WAR TROPHY** wedge (+80 JC) added\n"
            f"⚔️ **RETRIBUTION** wedge added (steals from attackers)\n"
            f"💪 **BANKRUPT emboldened** (+50%)\n\n"
            f"*Meta-bet pool: {meta_bet_result.get('total_pool', 0)} JC settled.*"
        ),
        color=discord.Color.from_str("#8b0000"),
    )
    return embed
