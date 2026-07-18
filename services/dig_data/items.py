"""Pickaxe tiers, boss-combat gear, and consumable items for /dig.

Extracted from the original ``dig_constants`` module; see
``services.dig_constants`` for the public facade.
"""

from __future__ import annotations

from dataclasses import dataclass

# ---------------------------------------------------------------------------
# Pickaxe Tiers
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class PickaxeTier:
    """Immutable definition for a pickaxe upgrade tier."""
    name: str
    advance_bonus: int              # extra blocks per dig
    cave_in_reduction: float        # absolute % reduction (0.05 = 5%)
    loot_bonus: int                 # extra JC per dig
    # Requirements
    depth_required: int
    jc_cost: int
    prestige_required: int          # 0 means no prestige gate


_PICKAXE_TIERS_DEF: list[PickaxeTier] = [
    PickaxeTier("Wooden",       0, 0.0,  0, depth_required=0,   jc_cost=0,    prestige_required=0),
    PickaxeTier("Stone",        1, 0.0,  0, depth_required=25,  jc_cost=15,   prestige_required=0),
    PickaxeTier("Iron",         1, 0.05, 0, depth_required=50,  jc_cost=50,   prestige_required=0),
    PickaxeTier("Diamond",      2, 0.05, 2, depth_required=75,  jc_cost=150,  prestige_required=0),
    PickaxeTier("Obsidian",     3, 0.10, 3, depth_required=100, jc_cost=300,  prestige_required=1),
    PickaxeTier("Stormrend",    3, 0.15, 3, depth_required=150, jc_cost=450,  prestige_required=2),
    PickaxeTier("Frostforged",  3, 0.20, 3, depth_required=200, jc_cost=600,  prestige_required=3),
    PickaxeTier("Void-Touched", 4, 0.20, 5, depth_required=275, jc_cost=1200, prestige_required=5),
]

PICKAXE_TIERS: list[dict] = [
    {
        "name": p.name, "advance_bonus": p.advance_bonus,
        "cave_in_reduction": p.cave_in_reduction, "loot_bonus": p.loot_bonus,
        "depth_required": p.depth_required, "jc_cost": p.jc_cost,
        "prestige_required": p.prestige_required,
    }
    for p in _PICKAXE_TIERS_DEF
]


# ---------------------------------------------------------------------------
# Boss-combat Gear
# ---------------------------------------------------------------------------
# Four persistent slots (Weapon, Armor, Boots, Amulet) modify boss-fight
# stats. Weapon reuses the pickaxe names (Wooden → Void-Touched); Armor,
# Boots, and Amulet each vary their suffix per tier (Iron Hauberk, Iron
# Sabatons, Iron Talisman, ...). The shop gates each tier by prior depth
# and current prestige; boss victories can still drop pieces for free.
# Durability ticks once per
# boss fight; at zero the piece stays equipped but its effects remain
# disabled until repaired.

from domain.models.dig_gear import GearSlot, GearTierDef, UniqueGearDef  # noqa: E402

GEAR_MAX_DURABILITY: int = 20
GEAR_REPAIR_COST_PCT: float = 0.10
GEAR_BOSS_DROP_RATE: float = 0.07
# Maps boss-boundary depth → tier index of the dropped piece. Boundaries
# missing from this map (25/50/75) drop nothing; players buy low-tier
# shop gear there instead.
GEAR_DROP_DEPTH_TIER_MAP: dict[int, int] = {100: 4, 150: 5, 200: 6, 275: 7}

# Weapon = pickaxe. Tier-by-tier the dig stats here mirror PICKAXE_TIERS
# above so weapon and pickaxe collapse to the same item; the new boss
# stat columns are layered on top.
WEAPON_TIERS: list[GearTierDef] = [
    GearTierDef("Wooden Pickaxe",       tier=0, slot=GearSlot.WEAPON,
                player_dmg=0, player_hit=0.00,
                advance_bonus=0, cave_in_reduction=0.00, loot_bonus=0,
                shop_price=0,    depth_required=0,   prestige_required=0),
    GearTierDef("Stone Pickaxe",        tier=1, slot=GearSlot.WEAPON,
                player_dmg=0, player_hit=0.01,
                advance_bonus=1, cave_in_reduction=0.00, loot_bonus=0,
                shop_price=15,   depth_required=25,  prestige_required=0),
    GearTierDef("Iron Pickaxe",         tier=2, slot=GearSlot.WEAPON,
                player_dmg=0, player_hit=0.02,
                advance_bonus=1, cave_in_reduction=0.05, loot_bonus=0,
                shop_price=50,   depth_required=50,  prestige_required=0),
    GearTierDef("Diamond Pickaxe",      tier=3, slot=GearSlot.WEAPON,
                player_dmg=1, player_hit=0.03,
                advance_bonus=2, cave_in_reduction=0.05, loot_bonus=2,
                shop_price=150,  depth_required=75,  prestige_required=0),
    GearTierDef("Obsidian Pickaxe",     tier=4, slot=GearSlot.WEAPON,
                player_dmg=1, player_hit=0.04,
                advance_bonus=3, cave_in_reduction=0.10, loot_bonus=3,
                shop_price=300,  depth_required=100, prestige_required=1),
    GearTierDef("Stormrend Pickaxe",    tier=5, slot=GearSlot.WEAPON,
                player_dmg=1, player_hit=0.045,
                advance_bonus=3, cave_in_reduction=0.15, loot_bonus=3,
                shop_price=450,  depth_required=150, prestige_required=2),
    GearTierDef("Frostforged Pickaxe",  tier=6, slot=GearSlot.WEAPON,
                player_dmg=2, player_hit=0.05,
                advance_bonus=3, cave_in_reduction=0.20, loot_bonus=3,
                shop_price=600,  depth_required=200, prestige_required=3),
    GearTierDef("Void-Touched Pickaxe", tier=7, slot=GearSlot.WEAPON,
                player_dmg=2, player_hit=0.07,
                advance_bonus=4, cave_in_reduction=0.20, loot_bonus=5,
                shop_price=1200, depth_required=275, prestige_required=5),
]

# Armor adds player_hp (so the piece "soaks" boss hits). Base player_hp
# is 2–5 depending on risk_tier, so a full Void Bulwark roughly doubles
# survivability — the dominant survivability lever.
ARMOR_TIERS: list[GearTierDef] = [
    GearTierDef("Wooden Chestguard",    tier=0, slot=GearSlot.ARMOR,
                player_hp_bonus=0, shop_price=0),
    GearTierDef("Stone Cuirass",        tier=1, slot=GearSlot.ARMOR,
                player_hp_bonus=0, shop_price=20,   depth_required=25),
    GearTierDef("Iron Hauberk",         tier=2, slot=GearSlot.ARMOR,
                player_hp_bonus=1, shop_price=60,   depth_required=50),
    GearTierDef("Diamond Breastplate",  tier=3, slot=GearSlot.ARMOR,
                player_hp_bonus=2, shop_price=180,  depth_required=75),
    GearTierDef("Obsidian Brigandine",  tier=4, slot=GearSlot.ARMOR,
                player_hp_bonus=3, shop_price=350,  depth_required=100, prestige_required=1),
    GearTierDef("Stormrend Lamellar",   tier=5, slot=GearSlot.ARMOR,
                player_hp_bonus=3, shop_price=525,  depth_required=150, prestige_required=2),
    GearTierDef("Frostforged Carapace", tier=6, slot=GearSlot.ARMOR,
                player_hp_bonus=3, shop_price=700,  depth_required=200, prestige_required=3),
    GearTierDef("Void-Touched Bulwark", tier=7, slot=GearSlot.ARMOR,
                player_hp_bonus=4, shop_price=1400, depth_required=275, prestige_required=5),
]

# Boots reduce boss_hit (the chance an incoming attack lands). Stays
# bounded to a sane range so even Void boots don't make the player
# untouchable on their own.
BOOTS_TIERS: list[GearTierDef] = [
    GearTierDef("Wooden Clogs",           tier=0, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.00, shop_price=0),
    GearTierDef("Stone Galoshes",         tier=1, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.02, shop_price=25,   depth_required=25),
    GearTierDef("Iron Sabatons",          tier=2, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.04, shop_price=70,   depth_required=50),
    GearTierDef("Diamond Greaves",        tier=3, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.06, shop_price=200,  depth_required=75),
    GearTierDef("Obsidian Stompers",      tier=4, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.08, shop_price=400,  depth_required=100, prestige_required=1),
    GearTierDef("Stormrend Striders",     tier=5, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.09, shop_price=600,  depth_required=150, prestige_required=2),
    GearTierDef("Frostforged Treads",     tier=6, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.10, shop_price=800,  depth_required=200, prestige_required=3),
    GearTierDef("Void-Touched Sollerets", tier=7, slot=GearSlot.BOOTS,
                boss_hit_reduction=0.13, shop_price=1500, depth_required=275, prestige_required=5),
]

# Amulet adds crit_chance and crit_bonus. Pure boss-combat stat; no
# milestone grants, no dig-flow effects. Crit values stack additively
# with the risk-tier (Bold/Reckless) values inside _apply_gear_to_combat.
AMULET_TIERS: list[GearTierDef] = [
    GearTierDef("Twine Cord",            tier=0, slot=GearSlot.AMULET,
                crit_chance=0.00, crit_bonus=0, shop_price=0),
    GearTierDef("Stone Pendant",         tier=1, slot=GearSlot.AMULET,
                crit_chance=0.02, crit_bonus=0, shop_price=25,   depth_required=25),
    GearTierDef("Iron Talisman",         tier=2, slot=GearSlot.AMULET,
                crit_chance=0.04, crit_bonus=0, shop_price=70,   depth_required=50),
    GearTierDef("Diamond Charm",         tier=3, slot=GearSlot.AMULET,
                crit_chance=0.06, crit_bonus=0, shop_price=200,  depth_required=75),
    GearTierDef("Obsidian Amulet",       tier=4, slot=GearSlot.AMULET,
                crit_chance=0.07, crit_bonus=1, shop_price=400,  depth_required=100, prestige_required=1),
    GearTierDef("Stormrend Necklace",    tier=5, slot=GearSlot.AMULET,
                crit_chance=0.08, crit_bonus=1, shop_price=600,  depth_required=150, prestige_required=2),
    GearTierDef("Frostforged Pendant",   tier=6, slot=GearSlot.AMULET,
                crit_chance=0.09, crit_bonus=1, shop_price=800,  depth_required=200, prestige_required=3),
    GearTierDef("Void-Touched Talisman", tier=7, slot=GearSlot.AMULET,
                crit_chance=0.10, crit_bonus=1, shop_price=1500, depth_required=275, prestige_required=5),
]

GEAR_TIER_TABLES: dict[GearSlot, list[GearTierDef]] = {
    GearSlot.WEAPON: WEAPON_TIERS,
    GearSlot.ARMOR:  ARMOR_TIERS,
    GearSlot.BOOTS:  BOOTS_TIERS,
    GearSlot.AMULET: AMULET_TIERS,
}


# Event-only side-grades. Their reference tier is used only for inventory
# ordering and compatibility with the existing gear row shape; their authored
# modifiers and durability come from this registry.
UNIQUE_GEAR: dict[str, UniqueGearDef] = {
    "glassbreaker_pick": UniqueGearDef(
        "glassbreaker_pick", "Glassbreaker Pick", GearSlot.WEAPON, 3, 200, 8,
        player_dmg=2, player_hit=-0.08,
        advance_bonus=2, cave_in_reduction=0.05, loot_bonus=2,
        effect_summary="Diamond dig bonuses; +2 boss damage; -8% hit chance.",
    ),
    "needle_pick": UniqueGearDef(
        "needle_pick", "Needle Pick", GearSlot.WEAPON, 3, 200, 16,
        player_hit=0.08, crit_chance=0.03,
        advance_bonus=2, cave_in_reduction=0.05, loot_bonus=2,
        effect_summary="Diamond dig bonuses; +8% hit chance; +3% crit chance.",
    ),
    "briarplate": UniqueGearDef(
        "briarplate", "Briarplate", GearSlot.ARMOR, 3, 200, 14,
        player_hp_bonus=1, effect_id="reflect_first_hit",
        effect_summary="+1 HP; reflect 1 damage on the first boss hit.",
    ),
    "nullweave_mantle": UniqueGearDef(
        "nullweave_mantle", "Nullweave Mantle", GearSlot.ARMOR, 3, 200, 12,
        effect_id="block_first_status",
        effect_summary="Block the first boss status effect.",
    ),
    "springheel_boots": UniqueGearDef(
        "springheel_boots", "Springheel Boots", GearSlot.BOOTS, 3, 200, 14,
        boss_hit_reduction=0.04, effect_id="springheel_counter",
        effect_summary=(
            "-4% boss hit chance; counter the first boss miss for 1 damage."
        ),
    ),
    "anchor_boots": UniqueGearDef(
        "anchor_boots", "Anchor Boots", GearSlot.BOOTS, 3, 200, 16,
        player_hp_bonus=1, effect_id="block_first_skip",
        effect_summary="+1 HP; block the first skipped player round.",
    ),
    "loaded_die": UniqueGearDef(
        "loaded_die", "Loaded Die", GearSlot.AMULET, 3, 200, 12,
        player_hit=-0.05, crit_chance=0.10, crit_bonus=1,
        effect_summary="-5% hit chance; +10% crit chance; +1 crit damage.",
    ),
    "blood_locket": UniqueGearDef(
        "blood_locket", "Blood Locket", GearSlot.AMULET, 3, 200, 14,
        player_hp_bonus=-1, crit_chance=0.05, effect_id="heal_first_crit",
        effect_summary="-1 HP; +5% crit chance; first crit heals 1 HP.",
    ),
}


# ---------------------------------------------------------------------------
# Consumable Items
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class Consumable:
    """Immutable definition for a purchasable consumable item."""
    id: str
    name: str
    cost: int
    description: str
    # Mechanical parameters stored as a dict for flexible use
    params: dict[str, int | float]


CONSUMABLES: dict[str, Consumable] = {
    "dynamite": Consumable(
        id="dynamite",
        name="Dynamite",
        cost=5,
        description="+5 bonus blocks on next dig.",
        params={"bonus_blocks": 5},
    ),
    "hard_hat": Consumable(
        id="hard_hat",
        name="Hard Hat",
        cost=8,
        description="Absorbs the next 3 cave-ins. Each absorb costs a little light.",
        params={"uses": 3, "luminosity_drain_per_absorb": 10},
    ),
    "lantern": Consumable(
        id="lantern",
        name="Lantern",
        cost=4,
        description="-50% cave-in next dig. Reveals what's stirring nearby.",
        params={"cave_in_reduction": 0.50, "scan": 1, "boss_scout_blocks": 10},
    ),
    "reinforcement": Consumable(
        id="reinforcement",
        name="Reinforcement",
        cost=6,
        description="48h: half damage from sabotage, big cave-ins are capped.",
        params={
            "decay_prevent_hours": 48,
            "sabotage_reduction": 0.50,
            "cave_in_loss_cap": 8,
        },
    ),
    "torch": Consumable(
        id="torch",
        name="Torch",
        cost=6,
        description="+50 luminosity. Light the way.",
        params={"luminosity_restore": 50},
    ),
    "grappling_hook": Consumable(
        id="grappling_hook",
        name="Grappling Hook",
        cost=10,
        description="Cushions the next 5 cave-ins: no block loss, no stun.",
        params={"uses": 5},
    ),
    "sonar_pulse": Consumable(
        id="sonar_pulse",
        name="Sonar Pulse",
        cost=8,
        description="Reveals the next event and lets it pass you by once.",
        params={"event_preview": 1, "skip": 1},
    ),
    "depth_charge": Consumable(
        id="depth_charge",
        name="Depth Charge",
        cost=15,
        description="+10 bonus blocks on next dig. A louder, deeper blast than Dynamite.",
        params={"bonus_blocks": 10},
    ),
    "void_bait": Consumable(
        id="void_bait",
        name="Void Bait",
        cost=20,
        description="3 digs: 2x event chance, with a thumb on the scale for rare finds.",
        params={
            "event_multiplier": 2.0,
            "duration_digs": 3,
            "rare_weight_mult": 1.25,
            "legendary_weight_mult": 1.5,
        },
    ),
    "streak_charm": Consumable(
        id="streak_charm",
        name="Streak Charm",
        cost=15,
        description="Passively saves one daily dig streak after exactly one missed day.",
        params={"missed_day_saves": 1},
    ),
    "tempered_whetstone": Consumable(
        id="tempered_whetstone",
        name="Tempered Whetstone",
        cost=60,
        description="One boss attempt: +1 damage on every player hit.",
        params={"boss_prep_player_damage": 1},
    ),
    "warding_salts": Consumable(
        id="warding_salts",
        name="Warding Salts",
        cost=50,
        description="One boss attempt: blocks the first mechanic or status effect.",
        params={"boss_prep_block_mechanic": 1},
    ),
    "rescue_line": Consumable(
        id="rescue_line",
        name="Rescue Line",
        cost=40,
        description="One boss attempt: halves defeat knockback and extra gear wear.",
        params={"boss_prep_rescue": 1},
    ),
}

BOSS_PREP_ITEM_IDS: frozenset[str] = frozenset({
    "tempered_whetstone",
    "warding_salts",
    "rescue_line",
})

HARD_HAT_USES: int = 3
