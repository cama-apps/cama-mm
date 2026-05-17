"""Random event definitions, event ASCII art, and event helpers for /dig.

This module is a thin re-export shim. The contents were split into:

* ``services.dig_data.event_types``       - dataclasses + helpers
* ``services.dig_data.event_art``         - ``EVENT_ASCII_ART``
* ``services.dig_data.event_definitions`` - ``RANDOM_EVENTS`` + chain knobs

Importing from ``services.dig_data.events`` continues to work unchanged.
"""

from __future__ import annotations

from services.dig_data.event_art import EVENT_ASCII_ART
from services.dig_data.event_definitions import (
    EVENT_CHAIN_CHANCE,
    EVENT_CHAIN_JC_MULTIPLIER,
    RANDOM_EVENTS,
)
from services.dig_data.event_types import (
    EventChoice,
    EventOutcome,
    EventStep,
    RandomEvent,
    SplashConfig,
    TempBuff,
    _choice_to_dict,
    pick_description,
)

__all__ = [
    "EventOutcome",
    "SplashConfig",
    "EventChoice",
    "TempBuff",
    "EventStep",
    "RandomEvent",
    "pick_description",
    "RANDOM_EVENTS",
    "EVENT_CHAIN_CHANCE",
    "EVENT_CHAIN_JC_MULTIPLIER",
    "EVENT_ASCII_ART",
    "_choice_to_dict",
]
