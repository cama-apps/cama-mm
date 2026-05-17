"""
Constants for the tunnel digging minigame.

This module is a thin re-export facade. The constants and data definitions
themselves live in the cohesive submodules of the ``services.dig_data``
package, grouped by domain:

- ``services.dig_data.layers``    - layer definitions, pacing, layer weather
- ``services.dig_data.items``     - pickaxe tiers, boss-combat gear, consumables
- ``services.dig_data.artifacts`` - artifact and relic definitions
- ``services.dig_data.bosses``    - boss definitions, combat math, phases,
                                    pinnacle, boss dialogue
- ``services.dig_data.event_types``       - event dataclasses and helpers
- ``services.dig_data.event_art``         - event ASCII art
- ``services.dig_data.event_definitions`` - random events and chain knobs
- ``services.dig_data.quests``    - quest arc definitions and validation
- ``services.dig_data.prestige``  - luminosity, prestige, ascension,
                                    mutations, corruption
- ``services.dig_data.naming``    - tunnel-name word pools and layer ASCII art
- ``services.dig_data.balance``   - decay, sabotage, cave-in, injuries, tips
- ``services.dig_data.aliases``   - dict-shaped views used by the service layer

Importing ``from services.dig_constants import <name>`` keeps working
unchanged for every name the original module exposed.
"""

from __future__ import annotations

# Re-export every public name from each domain module. The import order below
# is alphabetical; the actual dependency order is enforced inside the
# submodules themselves (each imports the sibling names it needs), so the
# facade can list them in any order.
from services.dig_data.aliases import *  # noqa: F401,F403
from services.dig_data.artifacts import *  # noqa: F401,F403
from services.dig_data.balance import *  # noqa: F401,F403
from services.dig_data.bosses import *  # noqa: F401,F403
from services.dig_data.event_art import EVENT_ASCII_ART  # noqa: F401
from services.dig_data.event_definitions import (  # noqa: F401
    EVENT_CHAIN_CHANCE,
    EVENT_CHAIN_JC_MULTIPLIER,
    RANDOM_EVENTS,
)
from services.dig_data.event_types import (  # noqa: F401
    EventChoice,
    EventOutcome,
    EventStep,
    RandomEvent,
    SplashConfig,
    TempBuff,
    _choice_to_dict,
    pick_description,
)
from services.dig_data.items import *  # noqa: F401,F403
from services.dig_data.items import _PICKAXE_TIERS_DEF  # noqa: F401
from services.dig_data.layers import *  # noqa: F401,F403
from services.dig_data.layers import _LAYERS_DEF  # noqa: F401
from services.dig_data.naming import *  # noqa: F401,F403
from services.dig_data.prestige import *  # noqa: F401,F403
from services.dig_data.quests import *  # noqa: F401,F403

# Underscore-prefixed names (``_choice_to_dict``, ``_PICKAXE_TIERS_DEF``,
# ``_LAYERS_DEF``) are not picked up by ``import *``; they are re-exported
# explicitly above because existing callers import them by name.
