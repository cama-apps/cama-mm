"""
Cached data loading from dotabase for trivia questions.
"""

from __future__ import annotations

import json
import re
from collections import Counter
from dataclasses import dataclass
from functools import lru_cache

from dotabase import Ability, Hero, Item, Response, dotabase_session
from sqlalchemy.orm import joinedload

_STEAM_CDN = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react"


# ---------------------------------------------------------------------------
# Dataclasses
# ---------------------------------------------------------------------------

@dataclass(frozen=True, slots=True)
class HeroData:
    id: int
    name: str  # internal name (npc_dota_hero_...)
    localized_name: str
    real_name: str | None
    hype: str | None
    bio: str | None
    attr_primary: str | None
    is_melee: bool
    base_movement: int | None
    base_armor: int | None
    attack_rate: float | None
    attr_str_gain: float | None
    attr_agi_gain: float | None
    attr_int_gain: float | None
    image_url: str | None
    armor_at_level1: float | None  # base_armor + agility_base / 6 (stat panel value at level 1)
    vision_night: int | None
    turn_rate: float | None
    attack_damage_min: int | None
    attack_damage_max: int | None


@dataclass(frozen=True, slots=True)
class AbilityData:
    id: int
    name: str
    localized_name: str
    hero_id: int | None
    hero_name: str | None
    damage_type: str | None
    damage: str | None
    cooldown: str | None
    lore: str | None
    scepter_upgrades: bool
    scepter_description: str | None
    shard_upgrades: bool
    shard_description: str | None
    innate: bool
    icon_url: str | None
    mana_cost: str | None


@dataclass(frozen=True, slots=True)
class ItemData:
    id: int
    localized_name: str
    cost: int | None
    lore: str | None
    neutral_tier: int | None
    icon_url: str | None
    is_neutral_enhancement: bool
    ability_special: str | None  # JSON string of bonus descriptions
    active_cooldown: str | None  # cooldown for active use (e.g. BKB, Blink)


@dataclass(frozen=True, slots=True)
class VoicelineData:
    hero_id: int
    text: str


@dataclass(frozen=True, slots=True)
class FacetData:
    id: int
    localized_name: str
    hero_id: int
    hero_name: str | None
    description: str | None


# ---------------------------------------------------------------------------
# CDN URL helpers
# ---------------------------------------------------------------------------

def hero_image_url(hero_name: str) -> str | None:
    """Build Steam CDN hero portrait URL from internal name."""
    slug = hero_name.replace("npc_dota_hero_", "")
    if not slug:
        return None
    return f"{_STEAM_CDN}/heroes/{slug}.png"


def ability_icon_url(icon_path: str | None) -> str | None:
    """Build Steam CDN ability icon URL from dotabase icon path."""
    if not icon_path:
        return None
    # e.g. /panorama/images/spellicons/antimage_mana_break_png.png
    slug = icon_path.replace("/panorama/images/spellicons/", "").replace("_png.png", "")
    if slug.endswith(".png"):
        slug = slug[:-4]
    if not slug:
        return None
    return f"{_STEAM_CDN}/abilities/{slug}.png"


def item_icon_url(icon_path: str | None) -> str | None:
    """Build Steam CDN item icon URL from dotabase icon path."""
    if not icon_path:
        return None
    # e.g. /panorama/images/items/blink_png.png
    slug = icon_path.replace("/panorama/images/items/", "").replace("_png.png", "")
    if slug.endswith(".png"):
        slug = slug[:-4]
    if not slug:
        return None
    return f"{_STEAM_CDN}/items/{slug}.png"


# ---------------------------------------------------------------------------
# Data loaders (cached)
# ---------------------------------------------------------------------------

@lru_cache(maxsize=1)
def load_heroes() -> list[HeroData]:
    session = dotabase_session()
    heroes = session.query(Hero).all()
    result = []
    for h in heroes:
        agi_base = h.attr_agility_base or 0
        base_a = h.base_armor if h.base_armor is not None else 0
        result.append(HeroData(
            id=h.id,
            name=h.name or "",
            localized_name=h.localized_name or "",
            real_name=h.real_name if h.real_name else None,
            hype=h.hype if h.hype else None,
            bio=h.bio if h.bio else None,
            attr_primary=h.attr_primary,
            is_melee=bool(h.is_melee),
            base_movement=h.base_movement,
            base_armor=h.base_armor,
            attack_rate=h.attack_rate if h.attack_rate else None,
            attr_str_gain=h.attr_strength_gain if h.attr_strength_gain else None,
            attr_agi_gain=h.attr_agility_gain if h.attr_agility_gain else None,
            attr_int_gain=h.attr_intelligence_gain if h.attr_intelligence_gain else None,
            image_url=hero_image_url(h.name or ""),
            armor_at_level1=round(base_a + agi_base / 6, 4),
            vision_night=h.vision_night if h.vision_night else None,
            turn_rate=h.turn_rate if h.turn_rate else None,
            attack_damage_min=h.attack_damage_min if h.attack_damage_min else None,
            attack_damage_max=h.attack_damage_max if h.attack_damage_max else None,
        ))
    return result


@lru_cache(maxsize=1)
def load_abilities() -> list[AbilityData]:
    session = dotabase_session()
    abilities = session.query(Ability).options(joinedload(Ability.hero)).all()
    result = []
    for a in abilities:
        if a.is_talent:
            continue
        name = a.localized_name
        if not name:
            continue
        if "_" in name:
            continue  # skip internal/hidden abilities (e.g., rubick_hidden3)
        hero_name = a.hero.localized_name if a.hero else None
        result.append(AbilityData(
            id=a.id,
            name=a.name or "",
            localized_name=name,
            hero_id=a.hero_id,
            hero_name=hero_name,
            damage_type=a.damage_type if a.damage_type else None,
            damage=a.damage if a.damage and a.damage != "0" else None,
            cooldown=a.cooldown if a.cooldown and a.cooldown != "0" else None,
            lore=a.lore if a.lore else None,
            scepter_upgrades=bool(a.scepter_upgrades),
            scepter_description=a.scepter_description if a.scepter_description else None,
            shard_upgrades=bool(a.shard_upgrades),
            shard_description=a.shard_description if a.shard_description else None,
            innate=bool(a.innate),
            icon_url=ability_icon_url(a.icon),
            mana_cost=a.mana_cost if a.mana_cost and a.mana_cost != "0" else None,
        ))
    return result


@lru_cache(maxsize=1)
def load_items() -> list[ItemData]:
    session = dotabase_session()
    items = session.query(Item).all()

    # First pass: filter unavailable items and extract base_level for suffix logic
    raw: list[tuple[Item, int | None]] = []
    for i in items:
        if not i.localized_name or "_" in i.localized_name:
            continue
        item_json = json.loads(i.json_data) if isinstance(i.json_data, str) else (i.json_data or {})
        # Skip items disabled in Valve's game files (removed/unavailable items like Trident,
        # Iron Talon, etc.). Neutral drops are exempt — they're obtained, not purchased.
        if i.neutral_tier is None and item_json.get("ItemPurchasable") in (0, "0"):
            continue
        base_level = item_json.get("ItemBaseLevel")
        raw.append((i, int(base_level) if base_level is not None else None))

    # Items whose localized_name appears at multiple base levels need a level suffix
    # (currently only Dagon 1-5 all share the name "Dagon")
    name_counts = Counter(i.localized_name for i, _ in raw)

    # Second pass: build ItemData with level suffix applied where needed
    result = []
    for i, base_level in raw:
        name = i.localized_name
        if name_counts[name] > 1 and base_level is not None:
            level_str = f" {base_level}"
            if not name.endswith(level_str):
                name = name + level_str
        result.append(ItemData(
            id=i.id,
            localized_name=name,
            cost=i.cost if i.cost and i.cost > 0 else None,
            lore=i.lore if i.lore else None,
            neutral_tier=i.neutral_tier,
            icon_url=item_icon_url(i.icon),
            is_neutral_enhancement=bool(getattr(i, "is_neutral_enhancement", False)),
            ability_special=i.ability_special if i.ability_special else None,
            active_cooldown=i.cooldown if i.cooldown and i.cooldown != "0" else None,
        ))
    return result


@lru_cache(maxsize=1)
def load_voicelines() -> list[VoicelineData]:
    """Load voicelines suitable for trivia (clean, reasonable length)."""
    session = dotabase_session()
    responses = (
        session.query(Response)
        .filter(Response.hero_id.isnot(None), Response.text_simple.isnot(None))
        .all()
    )
    result = []
    for r in responses:
        text = (r.text_simple or "").strip()
        # Filter: reasonable length, no hero name leak via criteria check
        if len(text) < 15 or len(text) > 120:
            continue
        # Skip generic/boring lines
        if text.lower() in {"", "hahaha", "ha ha ha"}:
            continue
        result.append(VoicelineData(hero_id=r.hero_id, text=text))
    return result


@lru_cache(maxsize=1)
def load_facets() -> list[FacetData]:
    session = dotabase_session()
    heroes = session.query(Hero).options(joinedload(Hero.facets)).all()
    result = []
    for h in heroes:
        if not h.facets:
            continue
        for f in h.facets:
            if not f.localized_name or not f.description:
                continue
            result.append(FacetData(
                id=f.id,
                localized_name=f.localized_name,
                hero_id=h.id,
                hero_name=h.localized_name,
                description=f.description,
            ))
    return result


# ---------------------------------------------------------------------------
# Lookup helpers
# ---------------------------------------------------------------------------

_hero_by_id: dict[int, HeroData] = {}


def get_hero_by_id(hero_id: int) -> HeroData | None:
    if not _hero_by_id:
        for h in load_heroes():
            _hero_by_id[h.id] = h
    return _hero_by_id.get(hero_id)


def redact_hero_name(text: str, hero_name: str) -> str:
    """Remove hero name references from text for lore/bio questions."""
    if not text or not hero_name:
        return text or ""
    # Redact full name and individual words (for multi-word names)
    result = re.sub(re.escape(hero_name), "???", text, flags=re.IGNORECASE)
    for word in hero_name.split():
        if len(word) > 2:  # Don't redact tiny words like "of"
            result = re.sub(r"\b" + re.escape(word) + r"\b", "???", result, flags=re.IGNORECASE)
    return result
