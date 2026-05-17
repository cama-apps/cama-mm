"""Dataclasses and helpers for /dig random events.

Extracted from ``services.dig_data.events``; see that module for the public
re-export shim.
"""

from __future__ import annotations

import random
from dataclasses import dataclass, field
from typing import Any

# ---------------------------------------------------------------------------
# Random Events
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class EventOutcome:
    """Possible outcome of a choice in a random event."""
    description: str
    advance: int                    # blocks gained (+) or lost (-)
    jc: int                         # JC gained (+) or lost (-)
    cave_in: bool                   # does this trigger a cave-in?


@dataclass(frozen=True)
class SplashConfig:
    """Splash effect that reaches other players when a digger's event resolves.

    ``strategy`` selects the victim pool:
        * ``"random_active"``  - recently-active players in the guild
        * ``"richest_n"``      - top-N positive-balance players
        * ``"active_diggers"`` - players who have dug in the last 7 days

    ``trigger`` picks when the splash fires on the event outcome:
    ``"success"``, ``"failure"``, or ``"always"``.

    ``mode`` controls direction:
        * ``"burn"``  - victims' JC is debited (coins destroyed, deflation lever)
        * ``"grant"`` - targets are credited JC (cooperative splash, e.g.
                        Io tether pact sharing spoils with a partner)
        * ``"steal"`` - victims' JC is transferred to the digger via
                        ``steal_atomic`` (no fee, can push victim below 0
                        down to MAX_DEBT — matches Red/Blue Shell semantics)

    For ``"burn"`` debits are clamped so a non-negative player is not pushed
    below 0. ``"steal"`` is unclamped on the victim side (intentional).
    """

    strategy: str
    victim_count: int
    penalty_jc: int
    trigger: str = "failure"
    mode: str = "burn"


@dataclass(frozen=True)
class EventChoice:
    """A choice the player can make during an event."""
    label: str
    success: EventOutcome
    failure: EventOutcome | None    # None if the choice always succeeds
    success_chance: float           # 0-1, 1.0 = guaranteed


@dataclass(frozen=True)
class TempBuff:
    """Temporary modifier applied by an event outcome."""
    id: str
    name: str
    duration_digs: int
    effect: dict = field(default_factory=dict)  # {"cave_in_reduction": 0.10} or {"advance_bonus": 2}


@dataclass(frozen=True)
class EventStep:
    """One step in a multi-step complex encounter."""
    description: tuple[str, ...]
    choices: list[EventChoice] = field(default_factory=list)


@dataclass(frozen=True)
class RandomEvent:
    """Immutable definition for a random tunnel event."""
    id: str
    name: str                       # internal label (logs, admin, debug) — not shown to players
    description: tuple[str, ...]    # 1+ flavor variants; one is picked at display time
    min_depth: int | None           # None = any depth
    max_depth: int | None           # None = any depth
    safe_option: EventChoice
    risky_option: EventChoice
    # Expansion fields (defaults for backward compatibility with existing events)
    complexity: str = "choice"      # "simple" | "choice" | "complex"
    layer: str | None = None        # restrict to specific layer name, None = any
    rarity: str = "common"          # "common" | "uncommon" | "rare" | "legendary"
    steps: tuple[EventStep, ...] | None = None  # for complex multi-step events
    buff_on_success: TempBuff | None = None      # temp buff granted on risky success
    requires_dark: bool = False     # only triggers at Pitch Black luminosity
    social: bool = False            # references other players
    ascii_art: str | None = None    # roguelike-style ASCII scene (5-7 lines)
    # Prestige expansion fields
    desperate_option: EventChoice | None = None   # third choice: very low odds, massive reward/fail
    boon_options: tuple[TempBuff, ...] | None = None  # for complexity="boon" events
    min_prestige: int = 0           # minimum prestige level required
    next_event_id: str | None = None  # deterministic chain-next; only consumed when prestige >= min_prestige
    # Splash: optional penalty applied to other players in the guild when
    # this event resolves (see SplashConfig.trigger for which outcome fires it).
    splash: SplashConfig | None = None
    # Guild modifier set on risky/desperate success — drives marquee events
    # that toll a guild-wide window (e.g. helltide_active). The dict carries
    # ``id``, ``duration_seconds``, and an optional ``payload``. Requires
    # ``DigService.dig_guild_modifier_repo`` to be wired or it's a no-op.
    guild_modifier_on_success: dict | None = None
    # If True, this event is excluded from the random-pool selector and only
    # reachable via deterministic chain (``next_event_id`` from a predecessor).
    # Use for narrative arc successors that should not appear out of order.
    chain_only: bool = False
    # Quest tagging: when set, this event is one stage of a multi-dig narrative
    # arc. It is filtered out of the random pool unless the player is on the
    # matching active stage (or eligible to start the quest at stage 1).
    # Advances on successful *desperate* choice only.
    quest_id: str | None = None
    quest_step: int | None = None


def pick_description(event: Any) -> str:
    """Pick a random flavor-text variant from an event.

    Accepts RandomEvent, EventStep, wrapper with ``_d`` dict, or plain dict.
    If the description is a tuple/list, one entry is chosen at random.
    If it's a bare string (legacy/dynamic payloads), it's returned as-is.
    """
    desc: Any
    if isinstance(event, dict):
        desc = event.get("description", "")
    elif hasattr(event, "_d") and isinstance(event._d, dict):
        desc = event._d.get("description", "")
    else:
        desc = getattr(event, "description", "")
    if isinstance(desc, (tuple, list)):
        if not desc:
            return ""
        return random.choice(desc)
    return desc or ""


# Events as dicts
def _choice_to_dict(c: EventChoice) -> dict:
    """Convert an EventChoice to a dict for service-layer access."""
    return {
        "label": c.label,
        "success": {"description": c.success.description, "advance": c.success.advance, "jc": c.success.jc, "cave_in": c.success.cave_in},
        "failure": {"description": c.failure.description, "advance": c.failure.advance, "jc": c.failure.jc, "cave_in": c.failure.cave_in} if c.failure else None,
        "success_chance": c.success_chance,
    }
