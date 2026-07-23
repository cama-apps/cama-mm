"""Persistent route choices offered after Dig tier-boss victories."""

from __future__ import annotations

import random
from dataclasses import dataclass
from typing import Protocol

ROUTE_EFFECT_KEYS = frozenset({
    "advance_bonus",
    "advance_max_penalty",
    "artifact_multiplier",
    "cave_in_bonus",
    "cave_in_loss_bonus",
    "cave_in_loss_cap",
    "event_chance_multiplier",
    "luminosity_drain_multiplier",
    "luminosity_drain_reduction",
})


@dataclass(frozen=True)
class DigRoute:
    """One route that can modify digs for a single layer segment."""

    id: str
    name: str
    description: str
    layer: str | None
    effects: dict[str, int | float]

    def __post_init__(self) -> None:
        unknown = set(self.effects) - ROUTE_EFFECT_KEYS
        if unknown:
            raise ValueError(f"Route {self.id!r} has unknown effects: {sorted(unknown)}")


def _route(
    route_id: str,
    name: str,
    description: str,
    *,
    layer: str | None = None,
    **effects: int | float,
) -> DigRoute:
    return DigRoute(route_id, name, description, layer, effects)


UNIVERSAL_ROUTES: tuple[DigRoute, ...] = (
    _route(
        "shored_passage",
        "Shored Passage",
        "Careful supports cut collapse risk by 8 points, but trim 1 from your maximum advance.",
        cave_in_bonus=-0.08,
        advance_max_penalty=1,
    ),
    _route(
        "fractured_shortcut",
        "Fractured Shortcut",
        "+1 advance through unstable stone; collapses are 8 points likelier and cost 2 more blocks.",
        advance_bonus=1,
        cave_in_bonus=0.08,
        cave_in_loss_bonus=2,
    ),
    _route(
        "lantern_road",
        "Lantern Road",
        "Light drains 50% slower, but events are 25% scarcer and artifact odds fall by 25%.",
        luminosity_drain_reduction=0.50,
        event_chance_multiplier=-0.25,
        artifact_multiplier=0.75,
    ),
    _route(
        "echoing_gallery",
        "Echoing Gallery",
        "Events are 50% more frequent; collapses rise 4 points and light drains 25% faster.",
        event_chance_multiplier=0.50,
        cave_in_bonus=0.04,
        luminosity_drain_multiplier=0.25,
    ),
)


LAYER_ROUTE_POOLS: dict[str, tuple[DigRoute, ...]] = {
    "Stone": (
        _route(
            "old_supports",
            "Old Supports",
            "Collapse risk falls 5 points and losses cap at 6 blocks, but maximum advance falls by 1.",
            layer="Stone",
            cave_in_bonus=-0.05,
            cave_in_loss_cap=6,
            advance_max_penalty=1,
        ),
        _route(
            "surveyors_cut",
            "Surveyor's Cut",
            "Events rise 25% and collapse risk falls 3 points, but maximum advance falls by 1.",
            layer="Stone",
            event_chance_multiplier=0.25,
            cave_in_bonus=-0.03,
            advance_max_penalty=1,
        ),
        _route(
            "fossil_seam",
            "Fossil Seam",
            "Artifact odds rise 75%; collapses rise 5 points and light drains 25% faster.",
            layer="Stone",
            artifact_multiplier=1.75,
            cave_in_bonus=0.05,
            luminosity_drain_multiplier=0.25,
        ),
    ),
    "Crystal": (
        _route(
            "prismatic_fault",
            "Prismatic Fault",
            "Events rise 50% and artifact odds rise 25%, with 6 points more collapse risk.",
            layer="Crystal",
            event_chance_multiplier=0.50,
            artifact_multiplier=1.25,
            cave_in_bonus=0.06,
        ),
        _route(
            "glass_labyrinth",
            "Glass Labyrinth",
            "Artifact odds double, but maximum advance falls by 1 and light drains 50% faster.",
            layer="Crystal",
            artifact_multiplier=2.0,
            advance_max_penalty=1,
            luminosity_drain_multiplier=0.50,
        ),
        _route(
            "resonant_gallery",
            "Resonant Gallery",
            "Events rise 75%; light drains 50% faster and collapses rise 4 points.",
            layer="Crystal",
            event_chance_multiplier=0.75,
            luminosity_drain_multiplier=0.50,
            cave_in_bonus=0.04,
        ),
    ),
    "Magma": (
        _route(
            "cooling_sluice",
            "Cooling Sluice",
            "Collapse risk falls 10 points and light drain falls 25%, but advance and artifact odds suffer.",
            layer="Magma",
            cave_in_bonus=-0.10,
            luminosity_drain_reduction=0.25,
            advance_max_penalty=1,
            artifact_multiplier=0.75,
        ),
        _route(
            "lava_tube",
            "Lava Tube",
            "+1 advance, with 7 points more collapse risk and 50% faster light drain.",
            layer="Magma",
            advance_bonus=1,
            cave_in_bonus=0.07,
            luminosity_drain_multiplier=0.50,
        ),
        _route(
            "ember_vein",
            "Ember Vein",
            "Artifacts rise 75% and events 25%; light drains 75% faster and collapses rise 4 points.",
            layer="Magma",
            artifact_multiplier=1.75,
            event_chance_multiplier=0.25,
            luminosity_drain_multiplier=0.75,
            cave_in_bonus=0.04,
        ),
    ),
    "Abyss": (
        _route(
            "whispering_cut",
            "Whispering Cut",
            "Events rise 75%; light drains 50% faster and collapses rise 5 points.",
            layer="Abyss",
            event_chance_multiplier=0.75,
            luminosity_drain_multiplier=0.50,
            cave_in_bonus=0.05,
        ),
        _route(
            "blind_descent",
            "Blind Descent",
            "Artifact odds double, but light drains twice as fast and collapses rise 8 points.",
            layer="Abyss",
            artifact_multiplier=2.0,
            luminosity_drain_multiplier=1.0,
            cave_in_bonus=0.08,
        ),
        _route(
            "anchor_line",
            "Anchor Line",
            "Collapse losses cap at 7 blocks and risk falls 4 points, but maximum advance falls by 1.",
            layer="Abyss",
            cave_in_loss_cap=7,
            cave_in_bonus=-0.04,
            advance_max_penalty=1,
        ),
    ),
    "Fungal Depths": (
        _route(
            "mycelial_track",
            "Mycelial Track",
            "+1 advance and 25% more events; light drains 50% faster and collapses rise 4 points.",
            layer="Fungal Depths",
            advance_bonus=1,
            event_chance_multiplier=0.25,
            luminosity_drain_multiplier=0.50,
            cave_in_bonus=0.04,
        ),
        _route(
            "sporelit_garden",
            "Sporelit Garden",
            "Light drains 50% slower and artifacts rise 25%; events fall 25% and collapses rise 5 points.",
            layer="Fungal Depths",
            luminosity_drain_reduction=0.50,
            artifact_multiplier=1.25,
            event_chance_multiplier=-0.25,
            cave_in_bonus=0.05,
        ),
        _route(
            "rotcap_hollow",
            "Rotcap Hollow",
            "Events rise 75% and artifacts 50%; collapses rise 8 points and light drains 25% faster.",
            layer="Fungal Depths",
            event_chance_multiplier=0.75,
            artifact_multiplier=1.50,
            cave_in_bonus=0.08,
            luminosity_drain_multiplier=0.25,
        ),
    ),
    "Frozen Core": (
        _route(
            "frozen_stillness",
            "Frozen Stillness",
            "Collapse risk falls 12 points, but events fall 50%, artifacts 25%, and maximum advance by 1.",
            layer="Frozen Core",
            cave_in_bonus=-0.12,
            event_chance_multiplier=-0.50,
            artifact_multiplier=0.75,
            advance_max_penalty=1,
        ),
        _route(
            "timefracture",
            "Timefracture",
            "+1 advance and 50% more events; collapses rise 8 points and cost 1 more block.",
            layer="Frozen Core",
            advance_bonus=1,
            event_chance_multiplier=0.50,
            cave_in_bonus=0.08,
            cave_in_loss_bonus=1,
        ),
        _route(
            "icebound_cache",
            "Icebound Cache",
            "Artifact odds double, but maximum advance falls by 1, light drains 50% faster, and collapses rise 4 points.",
            layer="Frozen Core",
            artifact_multiplier=2.0,
            advance_max_penalty=1,
            luminosity_drain_multiplier=0.50,
            cave_in_bonus=0.04,
        ),
    ),
    "The Hollow": (
        _route(
            "void_harvest",
            "Void Harvest",
            "Artifact odds double and events rise 50%; collapses rise 12 points and light drains 50% faster.",
            layer="The Hollow",
            artifact_multiplier=2.0,
            event_chance_multiplier=0.50,
            cave_in_bonus=0.12,
            luminosity_drain_multiplier=0.50,
        ),
        _route(
            "silent_road",
            "Silent Road",
            "Collapse risk and light drain fall, but events, artifacts, and maximum advance all fall.",
            layer="The Hollow",
            cave_in_bonus=-0.10,
            luminosity_drain_reduction=0.50,
            event_chance_multiplier=-0.50,
            artifact_multiplier=0.75,
            advance_max_penalty=1,
        ),
        _route(
            "maws_shortcut",
            "Maw's Shortcut",
            "+2 advance; collapses rise 15 points, cost 3 more blocks, and light drains twice as fast.",
            layer="The Hollow",
            advance_bonus=2,
            cave_in_bonus=0.15,
            cave_in_loss_bonus=3,
            luminosity_drain_multiplier=1.0,
        ),
    ),
}


def _build_route_index() -> dict[str, DigRoute]:
    routes = [*UNIVERSAL_ROUTES]
    for pool in LAYER_ROUTE_POOLS.values():
        routes.extend(pool)
    index = {route.id: route for route in routes}
    if len(index) != len(routes):
        raise ValueError("Dig route ids must be unique")
    return index


ROUTE_BY_ID: dict[str, DigRoute] = _build_route_index()


class _RouteRandom(Protocol):
    def choice(self, seq): ...
    def sample(self, population, k: int): ...
    def shuffle(self, x) -> None: ...


def generate_route_offer(
    layer: str,
    *,
    previous_route_id: str | None = None,
    rng: _RouteRandom = random,
) -> tuple[str, str, str]:
    """Return one universal and two layer routes without permitting rerolls."""
    themed_pool = LAYER_ROUTE_POOLS.get(layer)
    if themed_pool is None:
        raise ValueError(f"No route pool for layer {layer!r}")

    universal_ids = [route.id for route in UNIVERSAL_ROUTES]
    without_previous = [route_id for route_id in universal_ids if route_id != previous_route_id]
    if without_previous:
        universal_ids = without_previous

    themed_ids = [route.id for route in themed_pool]
    themed_without_previous = [route_id for route_id in themed_ids if route_id != previous_route_id]
    if len(themed_without_previous) >= 2:
        themed_ids = themed_without_previous

    offered = [rng.choice(universal_ids), *rng.sample(themed_ids, 2)]
    rng.shuffle(offered)
    return tuple(offered)
