"""Dig boss-combat gear: slots, tier defs, owned pieces, and loadouts.

Four persistent slots (Weapon / Armor / Boots / Amulet) modify boss-fight
stats in :func:`services.dig_service.DigService.fight_boss`. The Relic
slot is the existing prestige-scaled artifact slot — relics live in the
``dig_artifacts`` table and are exposed here as plain dicts so a
:class:`GearLoadout` can present the full equipped set in one object.

Stat axes per slot:
    Weapon  +player_dmg, +player_hit
    Armor   +player_hp (absorbs more boss hits)
    Boots   -boss_hit (dodge)
    Amulet  +crit_chance, +crit_bonus (occasional bonus damage)
    Relic   existing dig effects only (this branch)

The user spec said "Armor reduces boss_dmg taken"; we implement that
intent as +player_hp because the base boss_dmg is 1 in every risk tier,
so any flat reduction either zeros it (game-breaking) or rounds away.
Adding HP gives a smooth integer scale and reads the same to the
player ("my armor lets me take more hits").

Tier names reuse the seven existing pickaxe tiers (Wooden through
Void-Touched) so naming is consistent across all gear pieces.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum


class GearSlot(str, Enum):
    WEAPON = "weapon"
    ARMOR = "armor"
    BOOTS = "boots"
    AMULET = "amulet"
    RELIC = "relic"


@dataclass(frozen=True)
class GearTierDef:
    """Static definition of a single gear piece (slot + tier combo)."""

    name: str
    tier: int
    slot: GearSlot
    # Boss-combat stats. Zero means "no effect on this axis".
    player_dmg: int = 0
    player_hit: float = 0.0
    player_hp_bonus: int = 0
    boss_hit_reduction: float = 0.0
    crit_chance: float = 0.0
    crit_bonus: int = 0
    # Dig-flow stats — only weapons populate these. Mirrors the legacy
    # PICKAXE_TIERS entries so weapon = pickaxe at the gameplay level.
    advance_bonus: int = 0
    cave_in_reduction: float = 0.0
    loot_bonus: int = 0
    # Acquisition gates
    shop_price: int = 0
    depth_required: int = 0
    prestige_required: int = 0


@dataclass(frozen=True)
class UniqueGearDef:
    """Static definition for an event-only horizontal gear side-grade."""

    item_id: str
    name: str
    slot: GearSlot
    reference_tier: int
    repair_value: int
    max_durability: int
    player_dmg: int = 0
    player_hit: float = 0.0
    player_hp_bonus: int = 0
    boss_hit_reduction: float = 0.0
    crit_chance: float = 0.0
    crit_bonus: int = 0
    advance_bonus: int = 0
    cave_in_reduction: float = 0.0
    loot_bonus: int = 0
    effect_id: str | None = None
    effect_summary: str | None = None


@dataclass
class GearPiece:
    """One owned instance of a gear piece. Mirrors a ``dig_gear`` row."""

    id: int
    slot: GearSlot
    tier: int
    durability: int
    equipped: bool
    acquired_at: int
    source: str
    tier_def: GearTierDef | UniqueGearDef
    item_id: str | None = None
    max_durability: int = 20


@dataclass
class GearLoadout:
    """The four equipped slots for one player at one moment in time.

    Returned by :func:`DigService._get_loadout` and consumed by
    :func:`DigService._apply_gear_to_combat` and the ``/dig gear`` panel.
    """

    weapon: GearPiece | None = None
    armor: GearPiece | None = None
    boots: GearPiece | None = None
    amulet: GearPiece | None = None
    relics: list[dict] = field(default_factory=list)

    def combat_modifiers(self) -> dict[str, float | int]:
        """Return the boss-combat deltas this loadout contributes.

        Caller decides how to fold these into base stats — see
        ``DigService._apply_gear_to_combat``. Empty slots contribute 0.
        """
        pieces = tuple(
            piece
            for piece in (self.weapon, self.armor, self.boots, self.amulet)
            if piece is not None
        )
        return {
            "player_dmg": sum(p.tier_def.player_dmg for p in pieces),
            "player_hit": sum(p.tier_def.player_hit for p in pieces),
            "player_hp_bonus": sum(p.tier_def.player_hp_bonus for p in pieces),
            "boss_hit_reduction": sum(p.tier_def.boss_hit_reduction for p in pieces),
            "crit_chance": sum(p.tier_def.crit_chance for p in pieces),
            "crit_bonus": sum(p.tier_def.crit_bonus for p in pieces),
        }
