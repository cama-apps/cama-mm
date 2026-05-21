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
    tier_def: GearTierDef


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

    def combat_modifiers(self) -> dict[str, float]:
        """Return the boss-combat deltas this loadout contributes.

        Caller decides how to fold these into base stats — see
        ``DigService._apply_gear_to_combat``. Empty slots contribute 0.
        """
        return {
            "player_dmg": self.weapon.tier_def.player_dmg if self.weapon else 0,
            "player_hit": self.weapon.tier_def.player_hit if self.weapon else 0.0,
            "player_hp_bonus": self.armor.tier_def.player_hp_bonus if self.armor else 0,
            "boss_hit_reduction": self.boots.tier_def.boss_hit_reduction if self.boots else 0.0,
            "crit_chance": self.amulet.tier_def.crit_chance if self.amulet else 0.0,
            "crit_bonus": self.amulet.tier_def.crit_bonus if self.amulet else 0,
        }
