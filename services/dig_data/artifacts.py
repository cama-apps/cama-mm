"""Artifact and relic definitions for /dig.

Extracted from the original ``dig_constants`` module; see
``services.dig_constants`` for the public facade.
"""

from __future__ import annotations

from dataclasses import dataclass

# ---------------------------------------------------------------------------
# Artifacts
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class ArtifactDef:
    """Immutable definition for a discoverable artifact."""
    id: str
    name: str
    layer: str                      # layer name (e.g. "Dirt", "Stone")
    rarity: str                     # Common | Uncommon | Rare | Legendary
    lore_text: str
    is_relic: bool
    effect: str | None              # description of mechanical effect, or None
    min_prestige: int = 0           # gates the artifact behind a prestige threshold (0 = available from P0)


# Functional Relics (6) ─────────────────────────────────────────
RELICS: list[ArtifactDef] = [
    ArtifactDef(
        id="mole_claws",
        name="Mole Claws",
        layer="Dirt",
        rarity="Rare",
        lore_text="Fashioned from the claws of the Great Undermole, these gloves let you tear through earth like butter.",
        is_relic=True,
        effect="+1 advance permanently",
    ),
    ArtifactDef(
        id="crystal_compass",
        name="Crystal Compass",
        layer="Crystal",
        rarity="Rare",
        lore_text="A shard of living crystal that hums near danger. It always points away from collapse.",
        is_relic=True,
        effect="-3% cave-in permanently",
    ),
    ArtifactDef(
        id="magma_heart",
        name="Magma Heart",
        layer="Magma",
        rarity="Rare",
        lore_text="Still beating after a thousand years in the lava, this heart radiates warmth and fortune.",
        is_relic=True,
        effect="+1 JC loot permanently",
    ),
    ArtifactDef(
        id="obsidian_shield",
        name="Obsidian Shield",
        layer="Magma",
        rarity="Rare",
        lore_text="Forged in volcanic fury, this shield absorbs ill intent from rival diggers.",
        is_relic=True,
        effect="-15% sabotage damage permanently",
    ),
    ArtifactDef(
        id="root_network",
        name="Root Network",
        layer="Stone",
        rarity="Rare",
        lore_text="Ancient roots lace through the stone, binding your tunnel walls against the passage of time.",
        is_relic=True,
        effect="-25% decay rate permanently",
    ),
    ArtifactDef(
        id="echo_stone",
        name="Echo Stone",
        layer="Crystal",
        rarity="Legendary",
        lore_text="This stone whispers the locations of hidden things. Collectors would kill for it—some have.",
        is_relic=True,
        effect="+10% artifact find chance permanently",
    ),
    ArtifactDef(
        id="spore_cloak",
        name="Spore Cloak",
        layer="Fungal Depths",
        rarity="Rare",
        lore_text="Woven from living mycelium, this cloak feeds on darkness and gives back light.",
        is_relic=True,
        effect="-50% luminosity drain permanently",
    ),
    ArtifactDef(
        id="frozen_clock",
        name="Frozen Clock",
        layer="Frozen Core",
        rarity="Rare",
        lore_text="The hands haven't moved in millennia. Time itself seems embarrassed by this.",
        is_relic=True,
        effect="Decay halved permanently",
    ),
    ArtifactDef(
        id="hollow_eye",
        name="Hollow Eye",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="It sees everything. Every path, every choice, every consequence. It blinks when you're not looking.",
        is_relic=True,
        effect="Complex events reveal all paths",
    ),
    ArtifactDef(
        id="mycelium_link",
        name="Mycelium Link",
        layer="Fungal Depths",
        rarity="Rare",
        lore_text="A living thread connecting you to the fungal network. When you help someone, the network amplifies it.",
        is_relic=True,
        effect="+5% help bonus when helping others",
    ),
    # P5-gated relics — only drop from boss kills once you've prestiged five times.
    # Mixed bag: one boss-combat, one dig-economy, one risk-mitigation.
    ArtifactDef(
        id="hollow_fang",
        name="Hollow Fang",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="Bone shorn from something the Hollow forgot to swallow. It hums when bosses are near.",
        is_relic=True,
        effect="+15% damage against bosses",
        min_prestige=5,
    ),
    ArtifactDef(
        id="echo_lantern",
        name="Echo Lantern",
        layer="The Hollow",
        rarity="Rare",
        lore_text="A lamp that catches sound instead of light. Reverberations slip a few extra coins from each stone.",
        is_relic=True,
        effect="+15% JC yield per dig",
        min_prestige=5,
    ),
    ArtifactDef(
        id="patient_stone",
        name="Patient Stone",
        layer="The Hollow",
        rarity="Rare",
        lore_text="Worn smooth by a hand that waited too long. Steadies the ground beneath you.",
        is_relic=True,
        effect="-30% depth lost to cave-ins",
        min_prestige=5,
    ),
    # ── New relics (mana-synergy / risk-reward / social-PvP / weather-passive) ──
    ArtifactDef(
        id="prism_heart",
        name="Prism Heart",
        layer="Crystal",
        rarity="Legendary",
        lore_text="A geode that drinks colour from the air. What it gives back depends on what you brought.",
        is_relic=True,
        effect="Bonus shifts with today's mana color",
    ),
    ArtifactDef(
        id="mana_conduit",
        name="Mana Conduit",
        layer="Crystal",
        rarity="Rare",
        lore_text="A filament of half-fused crystal. When you spend deeply, a thread of it returns to your pocket.",
        is_relic=True,
        effect="Refund 25% of any tap-mana shop cost",
    ),
    ArtifactDef(
        id="bloodstone",
        name="Bloodstone",
        layer="Magma",
        rarity="Legendary",
        lore_text="It does not pulse. It bargains. Half the time it doubles your haul; the other half it's hungry.",
        is_relic=True,
        effect="Coin-flip per dig: +50% or -25% JC",
    ),
    ArtifactDef(
        id="gamblers_charm",
        name="Gambler's Charm",
        layer="Magma",
        rarity="Rare",
        lore_text="Won, lost, won again. A talisman that pays you for surviving the bad rolls.",
        is_relic=True,
        effect="Survive a cave-in: gain 50% of would-be lost depth as JC",
    ),
    ArtifactDef(
        id="vendetta_coin",
        name="Vendetta Coin",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="Two faces. Both yours. Both angry. It remembers every shove down the shaft.",
        is_relic=True,
        effect="Sabotaged → reflect 50% damage and gain JC",
    ),
    ArtifactDef(
        id="mentors_lantern",
        name="Mentor's Lantern",
        layer="Dirt",
        rarity="Rare",
        lore_text="Old miners say a lantern shared lights two paths. It pays the truth of that out, in coin.",
        is_relic=True,
        effect="Helping another digger: both gain +10 JC",
    ),
    ArtifactDef(
        id="stormcaller",
        name="Stormcaller",
        layer="Stone",
        rarity="Rare",
        lore_text="Threaded with copper veins. It hums on storm days and goes silent in fair weather.",
        is_relic=True,
        effect="Storm: +50% yield, no hazard. Sunny: +10%",
    ),
    ArtifactDef(
        id="slow_drip",
        name="Slow Drip",
        layer="Dirt",
        rarity="Rare",
        lore_text="A clay jar that fills itself a coin at a time when no one is looking.",
        is_relic=True,
        effect="Idle income: 0.5 JC/min while away (cap 100/day)",
    ),
    # ── Prestige-4 boss-trophy relics ──────────────────────────────
    # Carve-drop from a specific boss (see TROPHY_RELIC_IDS); each has a
    # conditional mid-fight effect resolved in the boss duel loop.
    ArtifactDef(
        id="weeping_fang",
        name="Weeping Fang",
        layer="Fungal Depths",
        rarity="Rare",
        lore_text="A fang that never stops weeping. Whatever it bit is still out there, somewhere, getting slower.",
        is_relic=True,
        effect="Venom: bosses lose 1 HP per round (4 rounds)",
        min_prestige=4,
    ),
    ArtifactDef(
        id="runebitten_shard",
        name="Runebitten Shard",
        layer="Frozen Core",
        rarity="Legendary",
        lore_text="A splinter of a blade that remembers a name. Held too long, it starts trying out yours.",
        is_relic=True,
        effect="Lifesteal: heal 1 HP on your first hit each boss fight",
        min_prestige=4,
    ),
    ArtifactDef(
        id="aching_spine",
        name="Aching Spine",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="It is still growing. You keep it in a box. The box is never quite big enough.",
        is_relic=True,
        effect="Regrowth: heal 1 HP after any round you take no damage",
        min_prestige=4,
    ),
    ArtifactDef(
        id="listening_shard",
        name="Listening Shard",
        layer="Fungal Depths",
        rarity="Rare",
        lore_text="Hold it to your ear. It tells you what hasn't happened yet, in a voice you almost recognize.",
        is_relic=True,
        effect="Forewarned: the boss cannot hit you on round 1",
        min_prestige=3,
    ),
    ArtifactDef(
        id="hateborn_ember",
        name="Hateborn Ember",
        layer="Frozen Core",
        rarity="Rare",
        lore_text="It runs warm. It runs warmer when you do. It has never once gone cold.",
        is_relic=True,
        effect="Last stand: +1 damage to bosses while at 1 HP",
        min_prestige=3,
    ),
    # ── Prestige-4 general relics (dig-find in deep layers + boss pool) ──
    ArtifactDef(
        id="deepveined_coal",
        name="Deepveined Coal",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="It burns brightest where there's no light to spare. It seems to prefer it that way.",
        is_relic=True,
        effect="+20% JC while in the dark",
        min_prestige=4,
    ),
    ArtifactDef(
        id="diviners_knot",
        name="Diviner's Knot",
        layer="Frozen Core",
        rarity="Rare",
        lore_text="A tangle of cold cord with a single true end. Find it and the dark owes you a favor.",
        is_relic=True,
        effect="+10% success on risky events",
        min_prestige=4,
    ),
    ArtifactDef(
        id="pathfinders_spur",
        name="Pathfinder's Spur",
        layer="Fungal Depths",
        rarity="Rare",
        lore_text="The deeper you go, the harder it pulls. It wants to see the bottom too.",
        is_relic=True,
        effect="+1 advance per dig at depth 150+",
        min_prestige=4,
    ),
    # Special — Roshan drop (functional relic; kept after non-relic collectibles were cut).
    ArtifactDef(
        id="aegis_fragment",
        name="Aegis Fragment",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="A shard of immortality, cracked but not broken. It pulses with defiant energy.",
        is_relic=True,
        effect="Revives from next cave-in (consumed on use)",
    ),
    # ── "Buff fun" batch: proc/jackpot, streak, and boss-combat relics ──
    # Hybrid effect text (flavor + a light hint, no hard numbers); the actual
    # conservative values live in the effect code, not in these strings.
    ArtifactDef(
        id="midas_splinter",
        name="Midas Splinter",
        layer="Dirt",
        rarity="Rare",
        lore_text="A fleck of something that was never quite ore. It warms when the digging is good.",
        is_relic=True,
        effect="Sometimes the rock just *gives* (chance to double a dig's coin)",
    ),
    ArtifactDef(
        id="lucky_seam",
        name="Lucky Seam",
        layer="Stone",
        rarity="Rare",
        lore_text="Old hands swear the mountain keeps one good secret for every thousand it buries.",
        is_relic=True,
        effect="Most veins are ore; one in a great many is a fortune (rare jackpot strike)",
    ),
    ArtifactDef(
        id="prospectors_streak",
        name="Prospector's Streak",
        layer="Stone",
        rarity="Rare",
        lore_text="A tally-stone worn smooth by a thumb that counted every safe step — until it didn't.",
        is_relic=True,
        effect="Confidence compounds down here (more coin the longer you avoid a collapse; lose it all if one hits)",
    ),
    ArtifactDef(
        id="first_light",
        name="First Light",
        layer="Dirt",
        rarity="Rare",
        lore_text="It holds a sliver of dawn that never fully goes out — brightest before anyone else has stirred.",
        is_relic=True,
        effect="The first swing of the day rings truest (your first dig each day pays more)",
        min_prestige=2,
    ),
    ArtifactDef(
        id="berserkers_mark",
        name="Berserker's Mark",
        layer="Magma",
        rarity="Rare",
        lore_text="A brand that drinks pain and asks for more. It was cut into someone once, against their will.",
        is_relic=True,
        effect="It drinks your bruises and hits back (more damage to bosses the more you've been hit)",
        min_prestige=3,
    ),
    ArtifactDef(
        id="gamblers_edge",
        name="Gambler's Edge",
        layer="The Hollow",
        rarity="Rare",
        lore_text="A coin filed to a blade on one edge. It has never once landed the way you feared.",
        is_relic=True,
        effect="The dice like you in the dark (your hits sometimes land double against a boss)",
        min_prestige=4,
    ),
    ArtifactDef(
        id="deaths_door",
        name="Death's Door",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="You have died here before. You got up. The Nameless Depth remembers, and resents it.",
        is_relic=True,
        effect="You got up last time (a chance to survive a killing blow)",
        min_prestige=5,
    ),
]

# Signature trophy relics carve-drop from a specific boss (see BossDef.trophy_relic_id)
# and are excluded from the random prestige-relic pool and the dig-time roll, so the
# only way to get one is to beat the boss it belongs to.
TROPHY_RELIC_IDS: frozenset[str] = frozenset({
    "weeping_fang", "runebitten_shard", "aching_spine",
    "listening_shard", "hateborn_ember", "deaths_door",
})

# Per-kill chance a boss's signature trophy carves, rolled until the player owns it.
TROPHY_CARVE_RATE: float = 0.25


ALL_ARTIFACTS: list[ArtifactDef] = list(RELICS)

ARTIFACT_BY_ID: dict[str, ArtifactDef] = {a.id: a for a in ALL_ARTIFACTS}

