"""
Cached data loading from dotabase for trivia questions.
"""

from __future__ import annotations

import re
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
        ))
    return result


@lru_cache(maxsize=1)
def load_items() -> list[ItemData]:
    session = dotabase_session()
    items = session.query(Item).all()
    result = []
    for i in items:
        if not i.localized_name or "_" in i.localized_name:
            continue
        result.append(ItemData(
            id=i.id,
            localized_name=i.localized_name,
            cost=i.cost if i.cost and i.cost > 0 else None,
            lore=i.lore if i.lore else None,
            neutral_tier=i.neutral_tier,
            icon_url=item_icon_url(i.icon),
            is_neutral_enhancement=bool(getattr(i, 'is_neutral_enhancement', False)),
            ability_special=i.ability_special if i.ability_special else None,
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
