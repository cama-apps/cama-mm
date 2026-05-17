"""Compatibility aliases and dict-shaped views over the /dig data modules.

The service layer uses simpler dict-based lookups. These aliases bridge the
structured dataclass definitions in the sibling data modules to the dict-based
access patterns used in ``dig_service.py``.

Extracted from the original ``dig_constants`` module; see
``services.dig_constants`` for the public facade.
"""

from __future__ import annotations

from services.dig_data.artifacts import ALL_ARTIFACTS
from services.dig_data.balance import MAX_INVENTORY_SLOTS, PROGRESSIVE_TIPS
from services.dig_data.bosses import BOSSES, PINNACLE_DEPTH
from services.dig_data.events import (
    EVENT_ASCII_ART,
    RANDOM_EVENTS,
    _choice_to_dict,
)
from services.dig_data.items import CONSUMABLES
from services.dig_data.layers import (
    _LAYERS_DEF,
    FREE_DIG_COOLDOWN_SECONDS,
    LAYER_BOUNDARIES,
    PAID_DIG_COSTS_PER_DAY,
)
from services.dig_data.naming import TUNNEL_NAME_TITLE_X, TUNNEL_NAME_TITLE_Y

# ---------------------------------------------------------------------------
# Compatibility Aliases
# ---------------------------------------------------------------------------
# The service layer uses simpler dict-based lookups. These aliases bridge
# the gap between the structured dataclass definitions above and the
# dict-based access patterns used in dig_service.py.

# LAYERS as dicts for service-layer dict-style access
LAYERS: list[dict] = [
    {
        "name": ld.name, "min_depth": ld.depth_min, "max_depth": ld.depth_max,
        "cave_in_pct": ld.cave_in_pct, "jc_min": ld.jc_min, "jc_max": ld.jc_max,
        "advance_min": ld.advance_min, "advance_max": ld.advance_max, "emoji": ld.emoji,
    }
    for ld in _LAYERS_DEF
]

FREE_DIG_COOLDOWN: int = FREE_DIG_COOLDOWN_SECONDS
PAID_DIG_COSTS: list[int] = PAID_DIG_COSTS_PER_DAY
MAX_INVENTORY_SIZE: int = MAX_INVENTORY_SLOTS
INJURY_SLOW_COOLDOWN: int = 6 * 3600  # 6 hours in seconds (injury slower cooldown)

BOSS_BOUNDARIES: list[int] = LAYER_BOUNDARIES  # [25, 50, 75, 100, 150, 200, 275]
BOSS_DEPTHS: list[int] = LAYER_BOUNDARIES

# All encounter boundaries including the pinnacle. Used by service layer to
# detect when to trigger any boss (regular or pinnacle).
ALL_BOSS_BOUNDARIES: list[int] = LAYER_BOUNDARIES + [PINNACLE_DEPTH]

BOSS_NAMES: dict[int, str] = {d: b.name for d, b in BOSSES.items()}
BOSS_DIALOGUE: dict[int, list[str]] = {d: b.dialogue for d, b in BOSSES.items()}
BOSS_ASCII: dict[int, str] = {d: b.ascii_art for d, b in BOSSES.items()}


# Consumable items as dicts for service-layer lookups
CONSUMABLE_ITEMS: dict[str, dict] = {
    c.id: {"name": c.name, "cost": c.cost, "description": c.description, "params": c.params}
    for c in CONSUMABLES.values()
}
ITEM_PRICES: dict[str, int] = {c.id: c.cost for c in CONSUMABLES.values()}

# Artifact pool as dicts
ARTIFACT_POOL: list[dict] = [
    {
        "id": a.id, "name": a.name, "layer": a.layer, "rarity": a.rarity,
        "lore_text": a.lore_text, "is_relic": a.is_relic, "effect": a.effect,
    }
    for a in ALL_ARTIFACTS
]

EVENT_POOL: list[dict] = [
    {
        "id": e.id, "name": e.name, "description": e.description,
        "min_depth": e.min_depth, "max_depth": e.max_depth,
        "safe_option": _choice_to_dict(e.safe_option),
        "risky_option": _choice_to_dict(e.risky_option),
        "complexity": e.complexity,
        "layer": e.layer,
        "rarity": e.rarity,
        "requires_dark": e.requires_dark,
        "social": e.social,
        "ascii_art": e.ascii_art or EVENT_ASCII_ART.get(e.id),
        "buff_on_success": {
            "id": e.buff_on_success.id,
            "name": e.buff_on_success.name,
            "duration_digs": e.buff_on_success.duration_digs,
            "effect": dict(e.buff_on_success.effect),
        } if e.buff_on_success else None,
        "desperate_option": _choice_to_dict(e.desperate_option) if e.desperate_option else None,
        "boon_options": [
            {"id": b.id, "name": b.name, "duration_digs": b.duration_digs, "effect": dict(b.effect)}
            for b in e.boon_options
        ] if e.boon_options else None,
        "min_prestige": e.min_prestige,
        "next_event_id": e.next_event_id,
        "chain_only": e.chain_only,
        "splash": {
            "strategy": e.splash.strategy,
            "victim_count": e.splash.victim_count,
            "penalty_jc": e.splash.penalty_jc,
            "trigger": e.splash.trigger,
            "mode": e.splash.mode,
        } if e.splash else None,
        "guild_modifier_on_success": dict(e.guild_modifier_on_success) if e.guild_modifier_on_success else None,
        "quest_id": e.quest_id,
        "quest_step": e.quest_step,
    }
    for e in RANDOM_EVENTS
]

# Tips as dicts
DIG_TIPS: list[dict] = [
    {"min_depth": t[0], "max_depth": t[1], "text": t[2]}
    for t in PROGRESSIVE_TIPS
]

# Tunnel name titles (title format: "X of Y")
TUNNEL_NAME_TITLES: list[str] = [
    f"{x} of {y}" for x in TUNNEL_NAME_TITLE_X for y in TUNNEL_NAME_TITLE_Y
]
