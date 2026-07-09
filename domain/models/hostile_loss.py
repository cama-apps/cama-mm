"""Typed values for hostile Jopacoin loss protection."""

from __future__ import annotations

from dataclasses import dataclass
from enum import StrEnum


class HostileLossKind(StrEnum):
    """Hostile balance changes eligible for mana protection."""

    PYROCLASM = "pyroclasm"
    SOUL_HARVEST = "soul_harvest"
    WILDFIRE = "wildfire"
    BLOOD_PACT = "blood_pact"
    SWAMP_SIPHON = "swamp_siphon"
    RED_SHELL = "red_shell"
    BLUE_SHELL = "blue_shell"
    LIGHTNING_BOLT = "lightning_bolt"
    BANANA_PEEL = "banana_peel"
    GREEN_SHELL = "green_shell"
    BOMB_OMB = "bomb_omb"
    EMERGENCY = "emergency"
    COMMUNE = "commune"
    DECAY = "decay"
    HEIST = "heist"
    MARKET_CRASH = "market_crash"
    TRICKLE_DOWN = "trickle_down"
    HOSTILE_TAKEOVER = "hostile_takeover"
    RECESSION = "recession"
    DIG_SPLASH_BURN = "dig_splash_burn"
    DIG_SPLASH_STEAL = "dig_splash_steal"
    SABOTAGE = "sabotage"


class HostileLossDestination(StrEnum):
    """Where the part of a hostile loss that lands is sent."""

    BURN = "burn"
    PLAYER = "player"
    RESERVE = "reserve"


@dataclass(frozen=True, slots=True)
class ProtectionDetail:
    """One protection layer applied while resolving a hostile loss."""

    source: str
    absorbed: int
    rate: float
    capacity_before: int | None = None
    capacity_after: int | None = None
    buff_id: int | None = None
    retroactive: bool = False


@dataclass(frozen=True, slots=True)
class HostileLossResult:
    """Immutable outcome of one idempotent hostile-loss settlement."""

    event_id: int
    event_key: str
    kind: HostileLossKind
    destination: HostileLossDestination
    victim_id: int
    guild_id: int
    actor_id: int | None
    recipient_id: int | None
    requested: int
    attempted: int
    absorbed: int
    applied: int
    victim_balance_before: int
    victim_balance_after: int
    destination_balance_before: int | None
    destination_balance_after: int | None
    shieldable: bool
    duplicate: bool
    details: tuple[ProtectionDetail, ...] = ()

    # Compatibility names used by command presentation helpers.
    @property
    def attempted_loss(self) -> int:
        return self.attempted

    @property
    def absorbed_amount(self) -> int:
        return self.absorbed

    @property
    def applied_loss(self) -> int:
        return self.applied


@dataclass(frozen=True, slots=True)
class NonJcProtectionResult:
    """Outcome of checking a non-JC hostile action such as sabotage."""

    blocked: bool
    source: str | None
    buff_id: int | None
    event_id: int
    duplicate: bool = False
