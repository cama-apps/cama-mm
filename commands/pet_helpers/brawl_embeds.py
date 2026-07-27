"""Embed builders for pet brawls.

Synchronous (art renders through PIL) — call through asyncio.to_thread.
Builders that attach art return ``(embed, discord.File | None)`` with the
attachment:// URL already set, matching commands.pet_helpers.embeds.
"""

from __future__ import annotations

import discord

from commands.pet_helpers.embeds import COLOR_DEAD, TIER_BADGE
from domain.models.pet import Pet, PetStage
from domain.models.pet_brawl import PetBrawl
from domain.pet_brawl import MAX_ROUNDS, Duelist, PetBrawlState
from domain.pet_constants import PET_BRAWL_TURN_SECONDS, get_species
from utils.embeds import COLOR_BLUE, COLOR_GREEN, COLOR_ORANGE
from utils.pet_assets import get_pet_card, get_versus_card

COLOR_BRAWL = COLOR_ORANGE


def hp_bar(hp: int, max_hp: int) -> str:
    filled = max(0, min(10, round(hp * 10 / max_hp))) if max_hp else 0
    return "█" * filled + "░" * (10 - filled)


def _duelist_line(duelist: Duelist) -> str:
    species = get_species(duelist.species_id)
    badge = TIER_BADGE.get(species.tier, "")
    return (
        f"{badge}**{duelist.name}** ({species.display_name}) — "
        f"`{hp_bar(duelist.hp, duelist.max_hp)}` "
        f"{max(0, duelist.hp)}/{duelist.max_hp} HP"
    )


def build_challenge_embed(
    brawl: PetBrawl,
    challenger_pet: Pet,
    recipient_pet: Pet,
    now: int,
) -> tuple[discord.Embed, discord.File | None]:
    challenger_species = get_species(challenger_pet.species)
    recipient_species = get_species(recipient_pet.species)
    embed = discord.Embed(
        title="⚔️ A pet brawl challenge!",
        description=(
            f"<@{brawl.challenger_id}>'s "
            f"{TIER_BADGE.get(challenger_species.tier, '')}"
            f"**{challenger_pet.name}** challenges <@{brawl.recipient_id}>'s "
            f"{TIER_BADGE.get(recipient_species.tier, '')}"
            f"**{recipient_pet.name}** to single combat!\n\n"
            f"Expires <t:{brawl.expires_at}:R>."
        ),
        color=COLOR_BRAWL,
    )
    embed.set_footer(text="No coin at stake — hunger and honor only.")
    file = get_versus_card(
        challenger_pet.species,
        challenger_pet.stage(now).value,
        challenger_pet.pet_id,
        challenger_pet.accessory,
        recipient_pet.species,
        recipient_pet.stage(now).value,
        recipient_pet.pet_id,
        recipient_pet.accessory,
    )
    if file:
        embed.set_image(url=f"attachment://{file.filename}")
    return embed, file


def build_battle_embed(
    state: PetBrawlState,
    log_lines: tuple[str, ...],
    picks: dict[str, object],
) -> discord.Embed:
    embed = discord.Embed(
        title=f"⚔️ Pet Brawl — Round {state.round_no + 1}/{MAX_ROUNDS}",
        description=f"{_duelist_line(state.a)}\n{_duelist_line(state.b)}",
        color=COLOR_BRAWL,
    )
    if log_lines:
        embed.add_field(
            name=f"Round {state.round_no}", value="\n".join(log_lines), inline=False
        )
    lock_a = "✅" if "a" in picks else "🤔"
    lock_b = "✅" if "b" in picks else "🤔"
    embed.set_footer(
        text=(
            f"{state.a.name} {lock_a} · {state.b.name} {lock_b} — "
            f"{PET_BRAWL_TURN_SECONDS}s per move, picks are secret"
        )
    )
    return embed


def build_result_embed(
    settlement: dict,
    rounds: int,
    final_log: tuple[str, ...],
) -> tuple[discord.Embed, discord.File | None]:
    winner: Duelist = settlement["winner"]
    loser: Duelist = settlement["loser"]
    records: dict[int, tuple[int, int]] = settlement["records"]
    winner_delta = settlement["winner_delta"]
    loser_delta = settlement["loser_delta"]

    winner_gain_line = (
        f"**{winner.name}** feasts on victory: hunger +{winner_delta}"
        if winner_delta > 0
        else f"**{winner.name}** is too well-fed to want a victory snack"
    )
    loser_loss_line = (
        f"**{loser.name}** sulks off: hunger {loser_delta}"
        if loser_delta < 0
        else f"**{loser.name}** was too hungry to lose anything more"
    )
    win_rec = records.get(winner.pet_id, (0, 0))
    lose_rec = records.get(loser.pet_id, (0, 0))
    embed = discord.Embed(
        title=f"🏆 {winner.name} wins the brawl!",
        description=(
            ("\n".join(final_log) + "\n\n" if final_log else "")
            + f"Victory in {rounds} round{'s' if rounds != 1 else ''}.\n"
            f"{winner_gain_line}\n{loser_loss_line}\n\n"
            f"⚔️ **{winner.name}** {win_rec[0]}W-{win_rec[1]}L · "
            f"**{loser.name}** {lose_rec[0]}W-{lose_rec[1]}L"
        ),
        color=COLOR_GREEN,
    )
    file = get_pet_card(
        winner.species_id,
        PetStage.ADULT.value if winner.is_adult else PetStage.BABY.value,
        "happy",
        winner.pet_id,
    )
    if file:
        embed.set_image(url=f"attachment://{file.filename}")
    return embed, file


def build_declined_embed(recipient_id: int) -> discord.Embed:
    return discord.Embed(
        title="🐫 Challenge declined",
        description=f"<@{recipient_id}> politely declines. The camas graze on.",
        color=COLOR_BLUE,
    )


def build_expired_embed() -> discord.Embed:
    return discord.Embed(
        title="🌾 Challenge expired",
        description="Nobody answered the call. The arena stands empty.",
        color=COLOR_DEAD,
    )


def build_void_embed(reason: str) -> discord.Embed:
    return discord.Embed(
        title="🌪️ Brawl called off", description=reason, color=COLOR_DEAD
    )
