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


RARITY_DROP_RATES: dict[str, float] = {
    "Common": 0.05,
    "Uncommon": 0.02,
    "Rare": 0.005,
    "Legendary": 0.001,
}

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
]

# Collectible (non-relic) Artifacts (14) ────────────────────────
COLLECTIBLE_ARTIFACTS: list[ArtifactDef] = [
    ArtifactDef(
        id="ancient_shovel",
        name="Ancient Shovel",
        layer="Dirt",
        rarity="Common",
        lore_text="A wooden shovel from the First Diggers. The handle is worn smooth by countless hands.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="petrified_worm",
        name="Petrified Worm",
        layer="Dirt",
        rarity="Common",
        lore_text="A worm the size of your forearm, frozen in stone mid-wriggle. Unsettling.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="rusty_coin",
        name="Rusty Coin",
        layer="Dirt",
        rarity="Common",
        lore_text="An old Jopacoin, so corroded you can barely make out the grinning face on it.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="fossil_imprint",
        name="Fossil Imprint",
        layer="Stone",
        rarity="Common",
        lore_text="The impression of a creature that hasn't existed for millennia. It looks... angry.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="stone_tablet",
        name="Stone Tablet",
        layer="Stone",
        rarity="Uncommon",
        lore_text="Covered in runes that roughly translate to: 'Kilroy was here.'",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="geode_heart",
        name="Geode Heart",
        layer="Stone",
        rarity="Uncommon",
        lore_text="Crack it open and amethyst crystals sparkle inside. Too pretty to sell.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="singing_shard",
        name="Singing Shard",
        layer="Crystal",
        rarity="Common",
        lore_text="This crystal fragment emits a faint melody when held. The tune is oddly catchy.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="prismatic_lens",
        name="Prismatic Lens",
        layer="Crystal",
        rarity="Uncommon",
        lore_text="Light bends impossibly through this lens, revealing colors that shouldn't exist.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="frozen_flame",
        name="Frozen Flame",
        layer="Crystal",
        rarity="Rare",
        lore_text="A flame trapped in crystal, still flickering after centuries. It's warm to the touch.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="lava_pearl",
        name="Lava Pearl",
        layer="Magma",
        rarity="Uncommon",
        lore_text="Formed over millennia in a magma pocket. It glows with inner heat and smugness.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="charred_diary",
        name="Charred Diary",
        layer="Magma",
        rarity="Common",
        lore_text="Most pages are ash, but one reads: 'Day 412. Still hot. Still digging. Send help.'",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="void_fragment",
        name="Void Fragment",
        layer="Abyss",
        rarity="Uncommon",
        lore_text="A shard of absolute nothing. Looking at it too long makes you question your life choices.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="abyssal_eye",
        name="Abyssal Eye",
        layer="Abyss",
        rarity="Rare",
        lore_text="It blinks. You're sure it blinks. The Void Warden says it's 'decorative.'",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="entropy_marble",
        name="Entropy Marble",
        layer="Abyss",
        rarity="Legendary",
        lore_text="Contains a miniature universe in its final moments. Beautiful and deeply unsettling.",
        is_relic=False, effect=None,
    ),
    # Fungal Depths collectibles
    ArtifactDef(
        id="glowing_spore",
        name="Glowing Spore",
        layer="Fungal Depths",
        rarity="Common",
        lore_text="It pulses like a heartbeat. Don't name it. You'll get attached.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="fungal_scripture",
        name="Fungal Scripture",
        layer="Fungal Depths",
        rarity="Uncommon",
        lore_text="Written in spore patterns. Roughly translates to: 'We were here before the stone.'",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="sovereign_cap",
        name="Sovereign's Cap",
        layer="Fungal Depths",
        rarity="Rare",
        lore_text="A mushroom cap the size of a dinner plate, still warm from the Sporeling Sovereign's head.",
        is_relic=False, effect=None,
    ),
    # Frozen Core collectibles
    ArtifactDef(
        id="ice_memory",
        name="Ice Memory",
        layer="Frozen Core",
        rarity="Common",
        lore_text="A crystal of frozen time. Inside, a snowflake falls forever.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="paradox_coin",
        name="Paradox Coin",
        layer="Frozen Core",
        rarity="Uncommon",
        lore_text="Both heads and tails simultaneously. Useless for coin flips. Priceless for philosophers.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="chrono_shard",
        name="Chrono Shard",
        layer="Frozen Core",
        rarity="Rare",
        lore_text="It shows you what this cave looked like a million years ago: exactly the same.",
        is_relic=False, effect=None,
    ),
    # The Hollow collectibles
    ArtifactDef(
        id="hollow_whisper",
        name="Hollow Whisper",
        layer="The Hollow",
        rarity="Uncommon",
        lore_text="A captured whisper from The Hollow. It says your name sometimes.",
        is_relic=False, effect=None,
    ),
    ArtifactDef(
        id="depth_record",
        name="Depth Record",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="A stone tablet recording the deepest dig ever attempted. The last entry is dated tomorrow.",
        is_relic=False, effect=None,
    ),
    # Special — Roshan drop
    ArtifactDef(
        id="aegis_fragment",
        name="Aegis Fragment",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="A shard of immortality, cracked but not broken. It pulses with defiant energy.",
        is_relic=True,
        effect="Revives from next cave-in (consumed on use)",
    ),
    ArtifactDef(
        id="cheese",
        name="Cheese",
        layer="The Hollow",
        rarity="Legendary",
        lore_text="Aged in the deepest pit of the world. The smell alone could wake an ancient.",
        is_relic=False, effect=None,
    ),
    # PoE nod
    ArtifactDef(
        id="frozen_azurite",
        name="Frozen Azurite",
        layer="Frozen Core",
        rarity="Uncommon",
        lore_text="A deep blue crystal that hums with stored energy. Cartographers prize these above gold.",
        is_relic=False, effect=None,
    ),
]

ALL_ARTIFACTS: list[ArtifactDef] = RELICS + COLLECTIBLE_ARTIFACTS

ARTIFACT_BY_ID: dict[str, ArtifactDef] = {a.id: a for a in ALL_ARTIFACTS}

