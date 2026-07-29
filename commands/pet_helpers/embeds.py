"""Embed builders for the /pet cog.

All builders are synchronous and may render procedural art via
utils.pet_assets — call them through asyncio.to_thread. Each returns
``(embed, discord.File | None)``; when a file is returned the embed already
references it via an attachment:// URL.
"""

from __future__ import annotations

import discord

from domain.models.pet import Pet, PetMood, PetStage, PetStatus, RefundNotice
from domain.pet_constants import (
    DIG_WORK_CAP_BLOCKS,
    DIG_WORK_UNITS_PER_BLOCK,
    FEED_CAP_PER_DAY,
    FOOD_ITEMS,
    SALT_LICK,
    SOLO_TRAINING_SESSION_CAP,
    SPECIES,
    get_species,
)
from domain.pet_evolution import (
    PetCalling,
    calling_profile,
    hint_text,
    instinct_label,
)
from utils.embeds import COLOR_BLUE, COLOR_GREEN, COLOR_ORANGE, COLOR_RED
from utils.formatting import JOPACOIN_EMOTE
from utils.game_date import game_date_for_timestamp
from utils.pet_assets import get_egg_card, get_pet_card, get_tombstone_card

COLOR_EGG = 0xF1C40F  # warm gold
COLOR_DEAD = 0x5D6D7E  # slate gray
COLOR_LEGENDARY = 0xE91E63

MOOD_EMOJI = {
    PetMood.HAPPY: "😊",
    PetMood.CONTENT: "🙂",
    PetMood.HUNGRY: "😟",
    PetMood.STARVING: "😫",
}
MOOD_COLOR = {
    PetMood.HAPPY: COLOR_GREEN,
    PetMood.CONTENT: COLOR_BLUE,
    PetMood.HUNGRY: COLOR_ORANGE,
    PetMood.STARVING: COLOR_RED,
}
TIER_BADGE = {
    "common": "",
    "uncommon": "🔹 ",
    "rare": "🔮 ",
    "legendary": "👑 ",
}
TIER_REVEAL = {
    "common": "It's a",
    "uncommon": "Oh! It's a",
    "rare": "✨ Incredible — it's a",
    "legendary": "🎆 UNBELIEVABLE — it's the",
}


def hunger_bar(hunger: int) -> str:
    filled = max(0, min(10, round(hunger / 10)))
    return "█" * filled + "░" * (10 - filled)


def format_age(age_seconds: int) -> str:
    days = age_seconds // 86400
    if days >= 1:
        return f"{days} day{'s' if days != 1 else ''} old"
    hours = age_seconds // 3600
    return f"{hours} hour{'s' if hours != 1 else ''} old"


def species_label(pet: Pet) -> str:
    species = get_species(pet.species)
    return f"{TIER_BADGE.get(species.tier, '')}{species.display_name}"


def calling_label(pet: Pet) -> str | None:
    if pet.evolution_calling is None:
        return None
    profile = calling_profile(pet.evolution_calling)
    return f"{profile.emoji} {profile.label}"


def _calling_paths(pet: Pet) -> str:
    instincts = [
        instinct_label(instinct)
        for instinct in (pet.evolution_primary, pet.evolution_secondary)
        if instinct is not None
    ]
    return " + ".join(instincts) if instincts else "A little of every path"


def supplies_line(supplies: dict[str, int] | None) -> str:
    if not supplies:
        return "None — visit `/pet shop`"
    parts = []
    for item_id, food in FOOD_ITEMS.items():
        qty = supplies.get(item_id, 0)
        if qty:
            parts.append(f"{food.emoji} {food.display_name} ×{qty}")
    return " · ".join(parts) if parts else "None — visit `/pet shop`"


def build_status_embed(
    status: PetStatus,
    decay_per_day: int,
    now: int,
    *,
    owner_name: str,
    next_fee: int,
    flavor_text: str | None = None,
    brawl_record: tuple[int, int] | None = None,
    career: dict | None = None,
    solo_training_sessions: int | None = None,
) -> tuple[discord.Embed, discord.File | None]:
    pet = status.pet
    if pet is None:
        embed, file = _build_petless_embed(
            status, owner_name=owner_name, next_fee=next_fee
        )
    elif status.stage == PetStage.EGG:
        embed, file = _build_egg_embed(pet, status)
    else:
        embed, file = _build_living_embed(
            pet,
            status,
            decay_per_day,
            now,
            brawl_record=brawl_record,
            career=career,
            solo_training_sessions=solo_training_sessions,
        )
    if flavor_text:
        embed.add_field(
            name="💬 Cama chatter",
            value=flavor_text,
            inline=False,
        )
    return embed, file


def _build_petless_embed(
    status: PetStatus, *, owner_name: str, next_fee: int
) -> tuple[discord.Embed, discord.File | None]:
    dead = status.last_dead
    if dead is None:
        embed = discord.Embed(
            title=f"{owner_name} has no cama",
            description=(
                "A hollow feeling. A camaless existence.\n\n"
                f"Adopt a mysterious egg with `/pet adopt` — {next_fee} {JOPACOIN_EMOTE}"
            ),
            color=COLOR_BLUE,
        )
        return embed, None
    species = get_species(dead.species)
    age = format_age(dead.age_seconds(dead.died_at or dead.adopted_at))
    calling = calling_label(dead)
    identity = f"{species_label(dead)}"
    if calling:
        identity += f" · {calling}"
    embed = discord.Embed(
        title=f"In memoriam: {dead.name}",
        description=(
            f"{identity} · died of {dead.death_cause or 'starvation'}, "
            f"{age} — <t:{dead.died_at}:R>\n\n"
            f"_{species.blurb}_\n\n"
            f"F.\n\nAdopt again with `/pet adopt` — {next_fee} {JOPACOIN_EMOTE}"
        ),
        color=COLOR_DEAD,
    )
    file = get_tombstone_card(dead.name, dead.pet_id)
    if file:
        embed.set_image(url=f"attachment://{file.filename}")
    return embed, file


def _build_egg_embed(
    pet: Pet, status: PetStatus
) -> tuple[discord.Embed, discord.File | None]:
    embed = discord.Embed(
        title=f"🥚 {pet.name}",
        description=(
            f"A mysterious egg, warm to the touch.\n"
            f"Hatches <t:{pet.hatched_at}:R>. What's inside? Nobody knows."
        ),
        color=COLOR_EGG,
    )
    embed.add_field(
        name="Supplies", value=supplies_line(status.supplies), inline=False
    )
    file = get_egg_card(pet.pet_id)
    if file:
        embed.set_image(url=f"attachment://{file.filename}")
    return embed, file


def _build_living_embed(
    pet: Pet,
    status: PetStatus,
    decay_per_day: int,
    now: int,
    *,
    brawl_record: tuple[int, int] | None = None,
    career: dict | None = None,
    solo_training_sessions: int | None = None,
) -> tuple[discord.Embed, discord.File | None]:
    species = get_species(pet.species)
    mood = status.mood or PetMood.CONTENT
    pampered = pet.pampered_until is not None and pet.pampered_until > now
    profile = (
        calling_profile(pet.evolution_calling)
        if pet.evolution_calling is not None
        else None
    )
    color = (
        profile.color
        if profile is not None
        else COLOR_LEGENDARY if species.tier == "legendary" else MOOD_COLOR[mood]
    )
    title = f"{pet.name} — {species_label(pet)}"
    if profile is not None:
        title += f" · {profile.emoji} {profile.label}"

    embed = discord.Embed(
        title=title,
        description=f"_{species.blurb}_",
        color=color,
    )
    stage_name = (status.stage or PetStage.BABY).value.title()
    embed.add_field(
        name="Stage",
        value=f"{stage_name} · {format_age(status.age_seconds)}",
        inline=True,
    )
    mood_text = f"{MOOD_EMOJI[mood]} {mood.value.title()}"
    if pampered:
        mood_text += " (🧂 pampered)"
    embed.add_field(name="Mood", value=mood_text, inline=True)
    embed.add_field(
        name="Hunger",
        value=f"`{hunger_bar(status.hunger)}` {status.hunger}/100",
        inline=False,
    )
    stored_blocks = status.dig_work_units / DIG_WORK_UNITS_PER_BLOCK
    stored_text = f"{stored_blocks:.1f}".rstrip("0").rstrip(".")
    embed.add_field(
        name="Mining",
        value=(
            f"{stored_text}/{DIG_WORK_CAP_BLOCKS} blocks banked"
            f" · +{status.dig_work_rate}/day"
        ),
        inline=False,
    )
    upbringing = hint_text(status.evolution_hint)
    if upbringing:
        embed.add_field(name="Upbringing", value=upbringing, inline=False)
    if profile is not None:
        embed.add_field(
            name="Calling",
            value=f"_{profile.blurb}_\nPaths: {_calling_paths(pet)}",
            inline=False,
        )
    if status.hunger <= 30:
        starve_at = pet.starvation_time(decay_per_day)
        embed.add_field(
            name="⚠️ Warning",
            value=f"Will starve <t:{starve_at}:R> without food!",
            inline=False,
        )
    embed.add_field(
        name="Supplies", value=supplies_line(status.supplies), inline=False
    )
    if pet.accessory:
        from domain.pet_constants import get_accessory

        trinket = get_accessory(pet.accessory)
        embed.add_field(
            name="Trinket",
            value=f"{trinket.emoji} {trinket.display_name}",
            inline=True,
        )
    if brawl_record is not None and any(brawl_record):
        wins, losses = brawl_record
        embed.add_field(name="⚔️ Brawls", value=f"{wins}W · {losses}L", inline=True)
    if career is not None:
        level = career["str"] + career["int"] + career["dex"]
        title = " · ".join(
            value for value in (career.get("rank"), career.get("build")) if value
        )
        training = (
            f"Level {level}/4 · {career['xp']}/20 XP\n"
            f"STR {career['str']} · INT {career['int']} · DEX {career['dex']}"
        )
        if solo_training_sessions is not None:
            training += (
                f"\nSolo: {solo_training_sessions}/"
                f"{SOLO_TRAINING_SESSION_CAP} banked · `/pet train`"
            )
        if title:
            training += f"\n{title}"
        embed.add_field(
            name="🏋️ Training",
            value=training,
            inline=False,
        )
    feeds_used = pet.feeds_used_on(game_date_for_timestamp(now))
    feeds_left = max(0, FEED_CAP_PER_DAY - feeds_used)
    embed.set_footer(text=f"{feeds_left}/{FEED_CAP_PER_DAY} feeds left today")
    file = get_pet_card(
        pet.species,
        (status.stage or PetStage.BABY).value,
        pet.art_mood(now, decay_per_day),
        pet.pet_id,
        accessory=pet.accessory,
        calling=pet.evolution_calling,
        primary=pet.evolution_primary,
        secondary=pet.evolution_secondary,
    )
    if file:
        embed.set_image(url=f"attachment://{file.filename}")
    return embed, file


def build_shop_embed(
    supplies: dict[str, int] | None, species_id: str, balance: int | None
) -> discord.Embed:
    from domain.pet_constants import food_cost

    embed = discord.Embed(
        title="🛒 Cama Pantry",
        description="Buy with `/pet buy` or the buttons on `/pet status`.",
        color=COLOR_BLUE,
    )
    supplies = supplies or {}
    for item_id, food in FOOD_ITEMS.items():
        cost = food_cost(species_id, item_id)
        discount = " (discounted!)" if cost < food.cost else ""
        embed.add_field(
            name=f"{food.emoji} {food.display_name} — {cost} {JOPACOIN_EMOTE}{discount}",
            value=f"+{food.restore} hunger · owned ×{supplies.get(item_id, 0)}\n_{food.blurb}_",
            inline=False,
        )
    lick_cost = food_cost(species_id, SALT_LICK.item_id)
    embed.add_field(
        name=f"{SALT_LICK.emoji} {SALT_LICK.display_name} — {lick_cost} {JOPACOIN_EMOTE}",
        value=f"Applies immediately: pampered mood for 12h.\n_{SALT_LICK.blurb}_",
        inline=False,
    )
    if balance is not None:
        embed.set_footer(text=f"Balance: {balance} jopacoin")
    return embed


def build_hatch_embed(
    pet: Pet,
    *,
    flavor_text: str | None = None,
) -> tuple[discord.Embed, discord.File | None]:
    species = get_species(pet.species)
    reveal = TIER_REVEAL.get(species.tier, "It's a")
    embed = discord.Embed(
        title=f"🐣 {pet.name} hatched!",
        description=(
            f"<@{pet.discord_id}>'s egg cracked open... "
            f"**{reveal} {species.display_name}!**\n\n_{species.blurb}_"
        ),
        color=COLOR_LEGENDARY if species.tier == "legendary" else COLOR_GREEN,
    )
    if flavor_text:
        embed.add_field(
            name="💬 Cama chatter",
            value=flavor_text,
            inline=False,
        )
    file = get_pet_card(pet.species, "baby", "happy", pet.pet_id)
    if file:
        embed.set_image(url=f"attachment://{file.filename}")
    return embed, file


def build_evolution_embed(
    pet: Pet,
    *,
    flavor_text: str | None = None,
) -> tuple[discord.Embed, discord.File | None]:
    profile = calling_profile(pet.evolution_calling or PetCalling.WAYFARER)
    paths = _calling_paths(pet)
    embed = discord.Embed(
        title=f"{profile.emoji} {pet.name} evolved — {profile.label}!",
        description=(
            f"<@{pet.discord_id}>'s {species_label(pet)} found a calling.\n\n"
            f"**{paths}**\n_{profile.blurb}_"
        ),
        color=profile.color,
    )
    if flavor_text:
        embed.add_field(
            name="💬 Cama chatter",
            value=flavor_text,
            inline=False,
        )
    file = get_pet_card(
        pet.species,
        "adult",
        "happy",
        pet.pet_id,
        accessory=pet.accessory,
        calling=pet.evolution_calling,
        primary=pet.evolution_primary,
        secondary=pet.evolution_secondary,
    )
    if file:
        embed.set_image(url=f"attachment://{file.filename}")
    return embed, file


def build_death_embed(
    pet: Pet,
    *,
    flavor_text: str | None = None,
) -> tuple[discord.Embed, discord.File | None]:
    age = format_age(pet.age_seconds(pet.died_at or pet.adopted_at))
    cause = (
        "was given to the altar"
        if pet.death_cause == "sacrifice"
        else "starved"
    )
    calling = calling_label(pet)
    embed = discord.Embed(
        title=f"🪦 {pet.name} has died",
        description=(
            f"<@{pet.discord_id}>'s {species_label(pet)}"
            f"{f' · {calling}' if calling else ''} "
            f"{cause}, {age}.\n"
            f"Time of death: <t:{pet.died_at}:f>. F."
        ),
        color=COLOR_DEAD,
    )
    if flavor_text:
        embed.add_field(
            name="💬 Cama chatter",
            value=flavor_text,
            inline=False,
        )
    file = get_tombstone_card(pet.name, pet.pet_id)
    if file:
        embed.set_image(url=f"attachment://{file.filename}")
    return embed, file


def build_refund_embed(notice: RefundNotice) -> discord.Embed:
    lines = []
    for payout in sorted(notice.payouts, key=lambda p: -p.amount)[:15]:
        lines.append(
            f"<@{payout.discord_id}> spent {payout.consumed_jc} on care → "
            f"rolled **{payout.multiplier_pct}%** → +{payout.amount} {JOPACOIN_EMOTE}"
        )
    scaled = (
        "\n\n_The nonprofit couldn't cover the full total — refunds were "
        "scaled down pro-rata._"
        if notice.scaled_down
        else ""
    )
    return discord.Embed(
        title=f"🏦 Weekly pet care refund — {notice.week_key}",
        description=(
            "The jopacoin nonprofit rewards responsible cama ownership.\n\n"
            + "\n".join(lines)
            + scaled
        ),
        color=COLOR_GREEN,
    )


def build_graveyard_embed(
    pets: list[Pet],
    owner_name: str,
    camadex: tuple[list[str], int] | None = None,
    callingdex: tuple[list[PetCalling], int] | None = None,
) -> discord.Embed:
    if not pets:
        embed = discord.Embed(
            title=f"🪦 {owner_name}'s graveyard",
            description="No pets have died. A spotless record. So far.",
            color=COLOR_BLUE,
        )
    else:
        lines = []
        for pet in pets:
            age = format_age(pet.age_seconds(pet.died_at or pet.adopted_at))
            calling = calling_label(pet)
            lines.append(
                f"**{pet.name}** · {species_label(pet)}"
                f"{f' · {calling}' if calling else ''}"
                f" · {age} · <t:{pet.died_at}:R>"
            )
        embed = discord.Embed(
            title=f"🪦 {owner_name}'s graveyard",
            description="\n".join(lines) + "\n\nF.",
            color=COLOR_DEAD,
        )
    if camadex is not None:
        raised, total = camadex
        discovered = set(raised)
        entries = [
            get_species(species_id).display_name
            if species_id in discovered
            else "???"
            for species_id in SPECIES
        ]
        embed.add_field(
            name=f"📖 Camadex {len(discovered & set(SPECIES))}/{total}",
            value=" · ".join(entries),
            inline=False,
        )
    if callingdex is not None:
        raised, total = callingdex
        discovered = {PetCalling(calling) for calling in raised}
        entries = [
            calling_profile(calling).label if calling in discovered else "???"
            for calling in PetCalling
        ]
        embed.add_field(
            name=f"Callings {len(discovered)}/{total}",
            value=" · ".join(entries),
            inline=False,
        )
    return embed


def build_leaderboard_embed(
    pets: list[Pet],
    decay_per_day: int,
    now: int,
    records: dict[int, tuple[int, int]] | None = None,
) -> discord.Embed:
    if not pets:
        return discord.Embed(
            title="🏆 Oldest living camas",
            description="No pets have hatched yet. `/pet adopt` to be first.",
            color=COLOR_BLUE,
        )
    lines = []
    medals = ["🥇", "🥈", "🥉"]
    for i, pet in enumerate(pets):
        medal = medals[i] if i < len(medals) else f"{i + 1}."
        mood = pet.mood(now, decay_per_day)
        record = (records or {}).get(pet.pet_id, (0, 0))
        record_text = f" · ⚔️ {record[0]}W-{record[1]}L" if any(record) else ""
        lines.append(
            f"{medal} **{pet.name}** ({species_label(pet)}) — "
            f"<@{pet.discord_id}> · {format_age(pet.age_seconds(now))} "
            f"{MOOD_EMOJI[mood]}{record_text}"
        )
    return discord.Embed(
        title="🏆 Oldest living camas",
        description="\n".join(lines),
        color=COLOR_GREEN,
    )
