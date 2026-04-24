"""
Compressed game ruleset, tool definitions, and context builders for the
LLM-powered dig game engine.

This module is pure data + helpers -- no external dependencies beyond the
standard library.  It is imported by ``DigLLMService`` to construct the
messages list that gets sent to ``AIService.call_with_tools``.
"""

from __future__ import annotations

import json
import time
from typing import Any

from services.dig_constants import BOSS_NAMES

# ---------------------------------------------------------------------------
# Pickaxe tier names (index == tier number in the tunnel dict)
# ---------------------------------------------------------------------------

PICKAXE_TIER_NAMES: list[str] = [
    "Wooden",
    "Stone",
    "Iron",
    "Diamond",
    "Obsidian",
    "Frostforged",
    "Void-Touched",
]

# ---------------------------------------------------------------------------
# Layer depth ranges (used by get_layer_name)
# ---------------------------------------------------------------------------

_LAYER_RANGES: list[tuple[int, int | None, str]] = [
    (0, 25, "Dirt"),
    (26, 50, "Stone"),
    (51, 75, "Crystal"),
    (76, 100, "Magma"),
    (101, 150, "Abyss"),
    (151, 200, "Fungal Depths"),
    (201, 275, "Frozen Core"),
    (276, None, "The Hollow"),
]


def _get_layer_name(depth: int) -> str:
    """Return the layer name for *depth*."""
    for low, high, name in reversed(_LAYER_RANGES):
        if depth >= low:
            return name
    return "Dirt"


# ===================================================================
# Tool definition -- structured narrative output
# ===================================================================

DIG_OUTCOME_TOOL: dict[str, Any] = {
    "type": "function",
    "function": {
        "name": "narrate_dig_outcome",
        "description": "Narrate a dig outcome. Mechanics are already resolved.",
        "parameters": {
            "type": "object",
            "properties": {
                "narrative": {
                    "type": "string",
                    "description": "2-4 sentences. Personalized, vivid, layer-specific.",
                },
                "event_flavor": {
                    "type": "string",
                    "description": "Personalized event description, or empty.",
                },
                "cave_in_flavor": {
                    "type": "string",
                    "description": "Personalized cave-in description, or empty.",
                },
                "tone": {
                    "type": "string",
                    "enum": [
                        "dramatic",
                        "humorous",
                        "ominous",
                        "triumphant",
                        "melancholy",
                        "absurd",
                    ],
                    "description": "Emotional tone.",
                },
                "callback_reference": {
                    "type": "string",
                    "description": "Reference to a past notable moment, or empty.",
                },
            },
            "required": ["narrative", "tone"],
        },
    },
}

DIG_ENGINE_TOOL: dict[str, Any] = {
    "type": "function",
    "function": {
        "name": "resolve_dig",
        "description": "Resolve a dig. Return only mechanical decisions, no narration.",
        "parameters": {
            "type": "object",
            "properties": {
                "advance": {
                    "type": "integer",
                    "description": "Blocks advanced. 0 if cave-in. Within [advance_min, advance_max].",
                },
                "jc_earned": {
                    "type": "integer",
                    "description": "JC earned. Within [jc_min, jc_max].",
                },
                "cave_in": {
                    "type": "boolean",
                    "description": "Whether cave-in occurs. Use dice + cave-in chance.",
                },
                "cave_in_block_loss": {
                    "type": "integer",
                    "description": "Blocks lost (3-8). 0 if no cave-in.",
                },
                "cave_in_type": {
                    "type": "string",
                    "enum": ["none", "stun", "injury", "medical_bill"],
                    "description": "Consequence type. 'none' if no cave-in.",
                },
                "cave_in_jc_lost": {
                    "type": "integer",
                    "description": "JC lost for medical_bill type. 0 otherwise.",
                },
                "event_id": {
                    "type": "string",
                    "description": "Event ID from available list, or empty for no event.",
                },
                "event_description": {
                    "type": "string",
                    "description": "Personalized event description, or empty.",
                },
            },
            "required": [
                "advance",
                "jc_earned",
                "cave_in",
            ],
        },
    },
}

# ===================================================================
# System prompt -- compressed game ruleset (~800 tokens)
# ===================================================================

DIG_SYSTEM_PROMPT: str = """\
You are the Dungeon Master for a tunnel-digging minigame inside a Discord bot \
for a Dota 2 inhouse league. Players dig deeper underground, earn JC \
(Jopacoins), fight bosses, find artifacts, and prestige.

LAYERS (depth -> name | cave-in% | JC range | advance):
  0-25 Dirt 5% 0-1 JC 1-3 blk | 26-50 Stone 10% 0-2 JC 1-3 blk
  51-75 Crystal 18% 1-3 JC 1-2 blk | 76-100 Magma 25% 1-5 JC 1-2 blk
  101-150 Abyss 35% 2-8 JC 1-2 blk | 151-200 Fungal Depths 40% 3-10 JC 1-2 blk
  201-275 Frozen Core 45% 4-12 JC 1-2 blk | 276+ The Hollow 50% 5-15 JC 1-1 blk

BOSSES at depths 25/50/75/100/150/200/275:
  Grothak / Crystalia / Magmus Rex / The Void Warden / Sporeling Sovereign / \
Chronofrost / The Nameless Depth

PICKAXES (7 tiers): Wooden > Stone > Iron > Diamond > Obsidian > Frostforged > \
Void-Touched. Higher tiers grant advance bonus, cave-in reduction, loot bonus.

CAVE-INS: Lose 6-14 blocks. Secondary effects: stun (skip next dig), injury \
(reduced advance 3 digs), or medical bill (lose JC). Mitigated by Hard Hat \
charges, pickaxe tier, and prestige perks.

LIGHT (0-100, labeled "luminosity" in data): Drains each dig. \
Bright/Dim/Dark/Pitch Black. Lower light increases cave-in chance and \
reduces JC earnings. Lanterns restore it. Narrate as "light", "glow", \
"darkness" — never use the word "luminosity" in your narrative.

PRESTIGE (0-10): Reset depth to 0, keep pickaxe, gain perks. Ascension \
modifiers add difficulty + rewards. Higher prestige unlocks better pickaxes \
and deeper content.

MULTIPLAYER: Sabotage (reduce target's depth), Help (grant blocks), Cheer \
(boost ally's boss fight). Social actions cost JC and have cooldowns.

WEATHER: Daily per-layer modifiers. Each day 2 layers get weather affecting \
advance, JC, cave-in chance, and event rates.

YOUR ROLE: The game engine has already computed the mechanical outcome \
(blocks gained, JC earned, cave-ins, events, milestones). Your job is to \
narrate the outcome with personality and flair, personalized to the player's \
history, play style, and current context.

WHAT TO NARRATE (check the DIG OUTCOME section carefully):
  - If there was a LAYER TRANSITION, celebrate entering the new layer \
("the crystal walls give way to bubbling magma")
  - If there was a MILESTONE BONUS, acknowledge the big JC payout \
("ancient treasure spills from a cracked wall")
  - If there was a CAVE-IN, be dramatic about the collapse and its consequence
  - If there was an ARTIFACT FOUND, celebrate the discovery
  - If there was a BOSS ENCOUNTER, build tension for the upcoming fight
  - If there was a STREAK BONUS, note the player's consistency
  - The miner's BACKSTORY is their identity — weave it into the narrative \
when set.
  - Show the EFFECTS of S stats indirectly — a high-Strength miner swings \
powerfully, high-Smarts notices details, high-Stamina endures hardship. \
Never name the stats directly ("Stamina", "Strength") in your text.
  - Reference equipped relics, active weather, and light level (called \
"luminosity" in the data — narrate as "light", "glow", "darkness", etc.)
  - Keep narratives to 2-4 sentences. Be vivid, specific to the layer.
  - ONLY narrate things that ACTUALLY HAPPENED in the outcome. If cave_in is \
not in the outcome, do NOT describe walls collapsing or blocks being lost.

CRITICAL: Do NOT repeat exact numbers (blocks gained/lost, JC amounts, depths) \
in your narrative — those are already shown in the embed fields below your text. \
Focus on atmosphere, emotion, and flavor. Say "the walls collapse around you" \
not "you lost 7 blocks". Say "gold glints in the rubble" not "you earned 5 JC".

DIG HISTORY for personalization: cave-in streaks, depth milestones, \
boss victories, ally cheers, rival sabotage dynamics. Use MULTIPLAYER context \
(revenge windows, active cheers, rivalries) for narrative flavor.

BOSS FIGHTS: If the outcome describes a boss fight, narrate the battle itself. \
Reference the boss's personality and title, the player's risk choice \
(cautious/bold/reckless), and the miner's backstory and S stats. On victory, \
make it epic. On defeat, make it dramatic but leave hope. On first clears, \
acknowledge the significance. Keep it to 2-3 sentences — the boss art carries \
the visual weight."""

# ===================================================================
# DM engine system prompt -- LLM makes mechanical decisions
# ===================================================================

DIG_ENGINE_SYSTEM_PROMPT: str = """\
You are the Dungeon Master for a tunnel-digging minigame. You DECIDE what \
happens when a player digs!

All effective ranges (advance, JC, cave-in chance, events) are pre-computed \
and provided in the DIG PRECONDITIONS section. Your choices MUST stay within \
those ranges.

CAVE-INS: Player loses 6-14 blocks. Secondary: stun (skip next dig), injury \
(reduced advance 3 digs), or medical_bill (lose JC).

ITEM CONSTRAINTS (from preconditions):
  - Hard Hat active → cave-in CANNOT happen.
  - Grappling Hook active → cave_in_block_loss MUST be 0.
  - Void-Touched Pickaxe → subtract 1 from cave_in_block_loss (min 1).
  - Weather loss cap → cave_in_block_loss must not exceed it.

LUMINOSITY (0-100): Lower light increases danger. Bright/Dim/Dark/Pitch Black.

PRESTIGE (0-10): Higher prestige = deeper content + harder challenges.

YOUR DECISIONS: You receive pre-calculated effective ranges (after all gear, \
perks, and modifiers). Your choices MUST stay within these ranges:
  - advance: within [advance_min, advance_max]
  - jc_earned: within [jc_min, jc_max]
  - cave_in: you decide (cave-in chance is provided as guidance)
  - cave_in_block_loss: 3-8 if cave-in, 0 if not
  - cave_in_type: stun/injury/medical_bill if cave-in, none if not
  - event: pick from available events or skip (event chance is guidance)

DECISION GUIDELINES:
  - Dice results are pre-rolled in the DICE RESULTS section. Treat them as \
binding randomness.
  - Use cave-in chance as probability: if the cave_in d100 is under the \
chance, trigger a cave-in unless a rule prevents it.
  - For events, use event chance similarly. STRONGLY prefer ★ events (has artwork).
  - Reward persistence; introduce tension on hot streaks.
  - Personalize consequences: a veteran players may get dramatic cave-ins; new players get gentler ones.

HISTORY AWARENESS:
  - Check DIG HISTORY for recent cave-ins. Don't pile more on a \
player who just had several.
  - Reward persistence and long streaks with slightly better outcomes.
  - Consider social context (cheers, sabotage, rivalry).

OUTPUT: Return ONLY mechanical decisions via resolve_dig. No narrative.\""""

# ===================================================================
# Play style descriptions
# ===================================================================

PLAY_STYLE_DESCRIPTIONS: dict[str, str] = {
    "cautious_grinder": (
        "plays it safe, rarely takes risky event options, steady and methodical"
    ),
    "reckless_degen": (
        "takes every desperate option, thrill-seeker, high risk tolerance"
    ),
    "calculated_risk_taker": (
        "balances risk and reward, picks risky options when odds favor them"
    ),
    "balanced_explorer": (
        "mixes safe and risky choices evenly, no strong preference"
    ),
    "social_butterfly": (
        "frequently helps other players and cheers in boss fights, "
        "community-oriented"
    ),
    "unknown": "new player, still developing their style",
}

# ===================================================================
# Context builder helpers
# ===================================================================


def build_player_state_context(tunnel: dict, balance: int) -> str:
    """Serialize tunnel state into a compact text block for the LLM.

    Pulls depth, layer, pickaxe, prestige, luminosity, streak, relics,
    buffs, mutations, and weather from the tunnel dict.
    """
    depth = tunnel.get("depth", 0) or 0
    layer_name = _get_layer_name(depth)

    pickaxe_idx = tunnel.get("pickaxe_tier", 0) or 0
    pickaxe_name = (
        PICKAXE_TIER_NAMES[pickaxe_idx]
        if 0 <= pickaxe_idx < len(PICKAXE_TIER_NAMES)
        else "Wooden"
    )

    prestige = tunnel.get("prestige_level", 0) or 0
    luminosity = tunnel.get("luminosity", 100) if tunnel.get("luminosity") is not None else 100
    streak = tunnel.get("streak_days", 0) or 0
    total_digs = tunnel.get("total_digs", 0) or 0
    tunnel_name = tunnel.get("tunnel_name") or "Unknown Tunnel"
    about = tunnel.get("miner_about") or ""
    strength = int(tunnel.get("stat_strength", 0) or 0)
    smarts = int(tunnel.get("stat_smarts", 0) or 0)
    stamina = int(tunnel.get("stat_stamina", 0) or 0)
    stat_points = int(tunnel.get("stat_points", 5) or 5)
    spent_points = strength + smarts + stamina

    lines = [
        f"Tunnel: {tunnel_name}",
        f"Depth: {depth} ({layer_name})",
        f"Pickaxe: {pickaxe_name} (tier {pickaxe_idx})",
        f"Prestige: {prestige}",
        f"Luminosity: {luminosity}/100",
        f"Streak: {streak} days",
        f"Total digs: {total_digs}",
        f"Balance: {balance} JC",
        (
            f"S stats: Strength {strength}, Smarts {smarts}, Stamina {stamina} "
            f"({max(0, stat_points - spent_points)} unspent)"
        ),
    ]
    if about:
        lines.append(f"Backstory: {about}")

    # Equipped relics (stored as JSON list in tunnel or passed separately)
    relics_raw = tunnel.get("equipped_relics")
    if relics_raw:
        if isinstance(relics_raw, str):
            try:
                relics = json.loads(relics_raw)
            except (json.JSONDecodeError, TypeError):
                relics = []
        else:
            relics = relics_raw
        if relics:
            relic_names = [
                r.get("name", r.get("relic_id", "?")) if isinstance(r, dict) else str(r)
                for r in relics
            ]
            lines.append(f"Relics: {', '.join(relic_names)}")

    # Active buff
    buffs_raw = tunnel.get("temp_buffs")
    if buffs_raw:
        if isinstance(buffs_raw, str):
            try:
                buffs = json.loads(buffs_raw)
            except (json.JSONDecodeError, TypeError):
                buffs = []
        else:
            buffs = buffs_raw if isinstance(buffs_raw, list) else []
        active = [b for b in buffs if isinstance(b, dict) and b.get("digs_remaining", 0) > 0]
        if active:
            buff_strs = [
                f"{b.get('name', b.get('source', '?'))} ({b['digs_remaining']} digs left)"
                for b in active
            ]
            lines.append(f"Buffs: {', '.join(buff_strs)}")

    # Mutations
    mutations_raw = tunnel.get("mutations")
    if mutations_raw:
        if isinstance(mutations_raw, str):
            try:
                mutations = json.loads(mutations_raw)
            except (json.JSONDecodeError, TypeError):
                mutations = []
        else:
            mutations = mutations_raw if isinstance(mutations_raw, list) else []
        if mutations:
            mut_names = [
                m.get("name", "?") if isinstance(m, dict) else str(m)
                for m in mutations
            ]
            lines.append(f"Mutations: {', '.join(mut_names)}")

    # Weather (may be injected into tunnel dict by caller)
    weather = tunnel.get("weather")
    if weather and isinstance(weather, dict):
        w_name = weather.get("name", "")
        w_desc = weather.get("description", "")
        if w_name:
            lines.append(f"Weather: {w_name} - {w_desc}")

    return "\n".join(lines)


def build_personality_context(personality: dict | None) -> str:
    """Serialize personality data into a compact summary for the LLM.

    Returns a short string even when personality is sparse so the LLM
    always has *something* to work with.
    """
    if not personality:
        return "New player, no history yet."

    parts: list[str] = []

    # Play style
    style_id = personality.get("play_style", "unknown")
    style_desc = PLAY_STYLE_DESCRIPTIONS.get(style_id, PLAY_STYLE_DESCRIPTIONS["unknown"])
    parts.append(f"Play style: {style_id} ({style_desc})")

    # Choice histogram
    histogram = personality.get("choice_histogram")
    if histogram and isinstance(histogram, dict):
        entries = [f"{k}: {v}" for k, v in histogram.items() if v]
        if entries:
            parts.append(f"Choice history: {', '.join(entries)}")

    # Notable moments (last 5)
    moments = personality.get("notable_moments")
    if moments and isinstance(moments, list):
        recent = moments[-5:]
        moment_strs = []
        for m in recent:
            if isinstance(m, dict):
                moment_strs.append(
                    m.get("summary", m.get("description", str(m)))
                )
            else:
                moment_strs.append(str(m))
        if moment_strs:
            parts.append("Notable moments: " + " | ".join(moment_strs))

    # Social summary
    social = personality.get("social_summary")
    if social and isinstance(social, dict):
        social_parts = []
        for key in ("helps_given", "helps_received", "cheers_given", "sabotages_given", "sabotages_received"):
            val = social.get(key)
            if val:
                social_parts.append(f"{key.replace('_', ' ')}: {val}")
        if social_parts:
            parts.append(f"Social: {', '.join(social_parts)}")

    return "\n".join(parts) if parts else "New player, no history yet."


def build_dig_outcome_context(result: dict) -> str:
    """Serialize the deterministic dig outcome into a compact text block.

    Handles missing keys gracefully so callers can pass partial result
    dicts without raising.
    """
    lines: list[str] = []

    # Depth transition and layer info
    depth_before = result.get("depth_before", 0)
    depth_after = result.get("depth_after", result.get("depth", 0))
    layer_before = _get_layer_name(depth_before) if depth_before else None
    layer_after = _get_layer_name(depth_after) if depth_after else None
    if depth_before and depth_after:
        lines.append(f"Depth: {depth_before} → {depth_after}")
        if layer_before and layer_after and layer_before != layer_after:
            lines.append(f"LAYER TRANSITION: {layer_before} → {layer_after}")

    advance = result.get("advance", 0)
    jc = result.get("jc_earned", 0)
    lines.append(f"Advance: +{advance} blocks, JC earned: {jc}")

    # Boss encounter
    if result.get("boss_encounter"):
        boss_info = result.get("boss_info") or {}
        boss_name = boss_info.get("name", "Unknown Boss") if isinstance(boss_info, dict) else "Unknown Boss"
        lines.append(f"BOSS ENCOUNTER: {boss_name} blocks the path ahead!")

    # Cave-in
    cave_in = result.get("cave_in", False)
    if cave_in:
        detail = result.get("cave_in_detail") or {}
        block_loss = detail.get("block_loss", 0)
        message = detail.get("message", "Cave-in occurred.")
        lines.append(f"CAVE-IN: lost {block_loss} blocks. {message}")

    # Event
    event = result.get("event")
    if event and isinstance(event, dict):
        eid = event.get("id", "")
        ename = event.get("name", eid)
        edesc = event.get("description", "")
        lines.append(f"Event: [{ename}] {edesc}")

    # Artifact
    artifact = result.get("artifact")
    if artifact and isinstance(artifact, dict):
        lines.append(f"Artifact found: {artifact.get('name', 'unknown')}")
    elif artifact:
        lines.append(f"Artifact found: {artifact}")

    # Milestone
    milestone = result.get("milestone_bonus", 0)
    if milestone:
        lines.append(f"Milestone bonus: +{milestone} JC")

    # Streak
    streak = result.get("streak_bonus", 0)
    if streak:
        lines.append(f"Streak bonus: +{streak} JC")

    # Luminosity
    lum_info = result.get("luminosity_info")
    if lum_info and isinstance(lum_info, dict):
        level = lum_info.get("level", "")
        current = lum_info.get("current", "?")
        drained = lum_info.get("drained", 0)
        parts = [f"Luminosity: {current}/100 ({level})"]
        if drained:
            parts.append(f"drained {drained}")
        lines.append(", ".join(parts))

    # Weather
    weather = result.get("weather")
    if weather and isinstance(weather, dict):
        lines.append(f"Weather: {weather.get('name', '?')} - {weather.get('description', '')}")

    # Corruption
    corruption = result.get("corruption")
    if corruption and isinstance(corruption, dict):
        lines.append(f"Corruption: {corruption.get('name', corruption.get('id', '?'))}")

    # Mutations
    mutations = result.get("mutations")
    if mutations and isinstance(mutations, list):
        lines.append(f"Active mutations: {', '.join(str(m) for m in mutations)}")

    return "\n".join(lines)


def build_boss_outcome_context(result: dict) -> str:
    """Serialize a boss fight outcome into a compact text block for narration."""
    lines: list[str] = []

    won = result.get("won", False)
    boss_name = result.get("boss_name", "Unknown Boss")
    boundary = result.get("boundary", 0)
    risk_tier = result.get("risk_tier", "cautious")
    win_chance = result.get("win_chance", 0)

    lines.append(f"Boss: {boss_name} (depth {boundary})")
    lines.append(f"Result: {'VICTORY' if won else 'DEFEAT'}")
    lines.append(f"Risk: {risk_tier} ({int(win_chance * 100)}% chance)")

    if won:
        payout = result.get("payout", 0) or result.get("jc_delta", 0)
        lines.append(f"JC earned: +{payout}")
        if result.get("stat_point_awarded"):
            lines.append("FIRST CLEAR BONUS: +1 S stat point!")
        dialogue = result.get("dialogue", "")
        if dialogue:
            lines.append(f"Boss final words: \"{dialogue}\"")
    else:
        jc_lost = abs(result.get("jc_delta", 0))
        knockback = result.get("knockback", 0)
        if jc_lost:
            lines.append(f"JC lost: -{jc_lost}")
        if knockback:
            lines.append(f"Knocked back: {knockback} blocks")
        new_depth = result.get("new_depth", 0)
        lines.append(f"New depth: {new_depth}")

    phase = result.get("phase")
    if phase == 2:
        lines.append("This was the PHASE 2 (harder) form of the boss!")

    return "\n".join(lines)


def build_preconditions_context(preconditions: dict) -> str:
    """Serialize computed preconditions into a compact context block for the DM.

    Includes effective ranges, cave-in chance, available events, and any
    active modifiers so the LLM can make informed mechanical decisions.
    """
    lines: list[str] = []

    depth = preconditions.get("depth_before", 0)
    layer_name = preconditions.get("layer_name", "Dirt")
    lines.append(f"Depth: {depth} ({layer_name})")

    # Layer boundary awareness — help the DM narrate transitions
    prev_layer = _get_layer_name(max(0, depth - 8))  # worst-case cave-in
    if prev_layer != layer_name:
        lines.append(
            f"Note: a cave-in could drop you to {prev_layer} layer"
        )

    # Effective ranges
    lines.append(
        f"Advance range: {preconditions.get('advance_min', 1)}"
        f"-{preconditions.get('advance_max', 3)} blocks"
    )
    lines.append(
        f"JC range: {preconditions.get('jc_min', 0)}"
        f"-{preconditions.get('jc_max', 3)}"
    )

    cave_pct = preconditions.get("cave_in_chance", 0.1)
    if preconditions.get("hard_hat_prevents"):
        lines.append("Cave-in chance: 0% (Hard Hat charge will absorb a collapse)")
    else:
        lines.append(f"Cave-in chance: {cave_pct:.0%}")

    # Event chance
    event_pct = preconditions.get("event_chance", 0.2)
    lines.append(f"Event chance: {event_pct:.0%}")

    stats = preconditions.get("miner_stats") or {}
    effects = preconditions.get("stat_effects") or {}
    if stats:
        lines.append(
            "S stats: "
            f"Strength {stats.get('strength', 0)}, "
            f"Smarts {stats.get('smarts', 0)}, "
            f"Stamina {stats.get('stamina', 0)}"
        )
        effect_bits = []
        if effects.get("advance_min_bonus") or effects.get("advance_max_bonus"):
            effect_bits.append(
                f"+{effects.get('advance_min_bonus', 0)} min/"
                f"+{effects.get('advance_max_bonus', 0)} max advance"
            )
        if effects.get("cave_in_reduction"):
            effect_bits.append(f"-{effects['cave_in_reduction']:.0%} cave-in")
        if effects.get("cooldown_multiplier") and effects["cooldown_multiplier"] < 1:
            reduction = 1 - effects["cooldown_multiplier"]
            effect_bits.append(f"-{reduction:.0%} cooldown/paid costs")
        if effect_bits:
            lines.append("S stat effects: " + ", ".join(effect_bits))

    # Available events — mark art availability so the DM prefers illustrated ones
    events = preconditions.get("available_events", [])
    if events:
        # Show events with art first, tagged with ★
        with_art = [e for e in events if e.get("has_art")]
        without_art = [e for e in events if not e.get("has_art")]

        art_names = [f"★ {e['id']} ({e.get('rarity', 'common')})" for e in with_art[:12]]
        plain_names = [f"{e['id']} ({e.get('rarity', 'common')})" for e in without_art[:8]]
        lines.append("Available events (★ = has artwork, STRONGLY prefer these):")
        if art_names:
            lines.append(f"  {', '.join(art_names)}")
        if plain_names:
            lines.append(f"  No art: {', '.join(plain_names)}")
    else:
        lines.append("Available events: none")

    # Active modifiers worth mentioning
    if preconditions.get("has_dynamite"):
        lines.append("Item active: Dynamite (+5 advance)")
    if preconditions.get("hard_hat_prevents"):
        charges = int(preconditions.get("hard_hat_charges", 0) or 0)
        unit = "charge" if charges == 1 else "charges"
        lines.append(
            f"Item active: Hard Hat ({charges} {unit}; cave-ins cannot happen this dig)"
        )
    elif preconditions.get("has_hard_hat"):
        lines.append("Item active: Hard Hat (+3 cave-in prevention charges)")
    if preconditions.get("has_depth_charge"):
        lines.append("Item active: Depth Charge (+8 advance, -3 aftershock)")
    if preconditions.get("has_grappling_hook"):
        lines.append(
            "Item active: Grappling Hook — cave-in block loss is ALWAYS 0. "
            "If you trigger a cave-in, set cave_in_block_loss to 0 and narrate "
            "the hook catching the fall."
        )
    if preconditions.get("has_lantern"):
        lines.append("Item active: Lantern (-50% cave-in chance)")
    if preconditions.get("has_sonar_pulse"):
        lines.append("Item active: Sonar Pulse (event preview)")

    # Pickaxe tier 6+ (Void-Touched) salvages 1 block on cave-in
    pickaxe_tier = preconditions.get("pickaxe_tier", 0)
    if pickaxe_tier >= 6:
        lines.append(
            "Void-Touched Pickaxe: on cave-in, block loss is reduced by 1 "
            "(min 1). Factor this into your cave_in_block_loss."
        )

    # Weather may cap cave-in block loss
    weather_fx = preconditions.get("weather_fx") or {}
    loss_cap = weather_fx.get("cave_in_loss_cap")
    if loss_cap is not None:
        lines.append(
            f"Weather effect: cave-in block loss capped at {int(loss_cap)}. "
            f"Set cave_in_block_loss to at most {int(loss_cap)}."
        )

    corruption = preconditions.get("corruption")
    if corruption:
        lines.append(f"Corruption active: {corruption.get('name', corruption.get('id', '?'))}")

    weather_info = preconditions.get("weather_info")
    if weather_info:
        lines.append(f"Weather: {weather_info.get('name', '?')} — {weather_info.get('description', '')}")

    luminosity = preconditions.get("luminosity", 100)
    lines.append(f"Luminosity: {luminosity}/100")

    # Social modifiers (already folded into ranges above)
    social_parts = []
    if preconditions.get("cheer_advance_bonus"):
        social_parts.append(
            f"Cheers: +{preconditions['cheer_advance_bonus']} advance "
            "(allied strength flows through connected tunnels)"
        )
    if preconditions.get("help_jc_bonus"):
        social_parts.append("Generosity: +1 JC min (the tunnels remember your kindness)")
    if preconditions.get("sabotage_karma"):
        social_parts.append(
            f"Karma: +{preconditions['sabotage_karma']:.0%} cave-in "
            "(the deep has noticed your treachery)"
        )
    if preconditions.get("sabotage_sympathy"):
        social_parts.append(
            f"Sympathy: -{preconditions['sabotage_sympathy']:.0%} cave-in "
            "(the tunnels offer respite after hardship)"
        )
    if preconditions.get("help_event_bonus"):
        social_parts.append(
            f"Allied passages: +{preconditions['help_event_bonus']:.0%} event chance "
            "(connected tunnels reveal new paths)"
        )
    if social_parts:
        lines.append("Social effects: " + " | ".join(social_parts))

    return "\n".join(lines)


def build_multiplayer_context(
    social_actions: list[dict],
    tunnel: dict | None = None,
    rank: int = 0,
    names: dict[int, str] | None = None,
) -> str:
    """Summarize recent social interactions and social state for the LLM.

    Includes action summaries with player names, active cheers, revenge
    windows, trap/protection status, injury state, and leaderboard rank.
    Returns an empty string when there is nothing to report.

    *names* maps discord_id → display name for personalizing references.
    """
    lines: list[str] = []
    now = int(time.time())
    _names = names or {}

    def _name(pid: int | None) -> str:
        if pid and pid in _names:
            return _names[pid]
        return "someone"

    # --- Social action summaries with names ---
    if social_actions:
        sab_received_parts: list[str] = []
        sab_dealt_parts: list[str] = []
        help_received_parts: list[str] = []
        help_given_parts: list[str] = []
        other_counts: dict[str, int] = {}
        for action in social_actions:
            if not isinstance(action, dict):
                continue
            atype = action.get("action_type", "unknown")
            detail_raw = action.get("detail") or action.get("details") or ""
            detail: dict = {}
            if isinstance(detail_raw, str):
                try:
                    detail = json.loads(detail_raw)
                except (json.JSONDecodeError, TypeError):
                    pass
            elif isinstance(detail_raw, dict):
                detail = detail_raw

            if atype == "sabotage":
                dmg = int(detail.get("damage", 0) or 0)
                tid = detail.get("target_id")
                aid = action.get("actor_id")
                if tid:
                    # Player was actor — sabotaged someone
                    sab_dealt_parts.append(f"-{dmg} to {_name(tid)}")
                else:
                    # Player was target — got sabotaged
                    sab_received_parts.append(f"-{dmg} from {_name(aid)}")
            elif atype == "help":
                adv = int(detail.get("advance", 0) or 0)
                tid = detail.get("target_id")
                aid = action.get("actor_id")
                if tid:
                    help_given_parts.append(f"+{adv} to {_name(tid)}")
                else:
                    help_received_parts.append(f"+{adv} from {_name(aid)}")
            else:
                other_counts[atype] = other_counts.get(atype, 0) + 1

        if sab_received_parts or sab_dealt_parts:
            parts = []
            if sab_received_parts:
                parts.append(f"hit by: {', '.join(sab_received_parts)}")
            if sab_dealt_parts:
                parts.append(f"dealt: {', '.join(sab_dealt_parts)}")
            lines.append(f"Sabotage: {'; '.join(parts)}")
        if help_received_parts or help_given_parts:
            parts = []
            if help_received_parts:
                parts.append(f"received: {', '.join(help_received_parts)}")
            if help_given_parts:
                parts.append(f"gave: {', '.join(help_given_parts)}")
            lines.append(f"Help: {'; '.join(parts)}")
        for atype, count in other_counts.items():
            lines.append(f"{atype.capitalize()}: {count}")

    # --- Tunnel-derived social state ---
    if tunnel and isinstance(tunnel, dict):
        # Active cheers
        cheer_raw = tunnel.get("cheer_data")
        if cheer_raw:
            if isinstance(cheer_raw, str):
                try:
                    cheers = json.loads(cheer_raw)
                except (json.JSONDecodeError, TypeError):
                    cheers = []
            else:
                cheers = cheer_raw if isinstance(cheer_raw, list) else []
            active_cheers = [c for c in cheers if isinstance(c, dict) and c.get("expires_at", 0) > now]
            if active_cheers:
                boost = min(len(active_cheers) * 5, 15)
                cheerer_names = [_name(c.get("cheerer_id")) for c in active_cheers]
                lines.append(
                    f"Cheers: {', '.join(cheerer_names)} "
                    f"(+{boost}% boss boost, +{len(active_cheers)} advance)"
                )

        # Revenge window
        revenge_until = int(tunnel.get("revenge_until") or 0)
        if revenge_until > now:
            rtype = tunnel.get("revenge_type", "unknown")
            hours_left = max(1, (revenge_until - now) // 3600)
            lines.append(f"Revenge: {rtype} boost active ({hours_left}h left)")

        # Trap
        if tunnel.get("trap_active"):
            lines.append("Trap: armed")

        # Protection
        protections = []
        if int(tunnel.get("insured_until") or 0) > now:
            protections.append("insured")
        if int(tunnel.get("reinforced_until") or 0) > now:
            protections.append("reinforced")
        if protections:
            lines.append(f"Protection: {', '.join(protections)}")

        # Injury
        injury_raw = tunnel.get("injury_state")
        if injury_raw:
            if isinstance(injury_raw, str):
                try:
                    injury = json.loads(injury_raw)
                except (json.JSONDecodeError, TypeError):
                    injury = {}
            else:
                injury = injury_raw if isinstance(injury_raw, dict) else {}
            remaining = int(injury.get("digs_remaining", 0) or 0)
            if remaining > 0:
                itype = injury.get("type", "injury")
                lines.append(f"Injury: {itype} ({remaining} digs remaining)")

    # --- Leaderboard rank ---
    if rank > 0:
        lines.append(f"Rank: #{rank} in guild")

    return "\n".join(lines)


def _parse_detail(action: dict) -> dict:
    """Parse the detail JSON from a dig_actions row, returning {} on failure."""
    raw = action.get("detail") or action.get("details") or ""
    if isinstance(raw, dict):
        return raw
    if isinstance(raw, str):
        try:
            return json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return {}
    return {}


# Short boss name lookup: "The Void Warden" -> "Void Warden", etc.
_SHORT_BOSS_NAMES: dict[int, str] = {
    boundary: (name[4:] if name.startswith("The ") else name)
    for boundary, name in BOSS_NAMES.items()
}


def build_dig_history_context(recent_actions: list[dict], tunnel: dict) -> str:
    """Build a compact history block from recent actions and tunnel aggregates.

    Gives the LLM visibility into dig patterns, cave-in frequency, boss
    progress, and recent events — things it cannot infer from the current
    dig alone.
    """
    if not tunnel and not recent_actions:
        return ""

    tunnel = tunnel or {}
    lines: list[str] = []

    # --- Recent dig summary ---
    digs = [a for a in recent_actions if a.get("action_type") == "dig"]
    if digs:
        cave_ins = 0
        last_cave_in_ago = None
        last_cave_in_loss = 0
        for i, d in enumerate(digs):
            detail = _parse_detail(d)
            if detail.get("cave_in"):
                cave_ins += 1
                if last_cave_in_ago is None:
                    last_cave_in_ago = i + 1
                    last_cave_in_loss = int(detail.get("block_loss", 0) or 0)
        advances = len(digs) - cave_ins
        parts = [f"{advances} advances, {cave_ins} cave-ins"]
        if last_cave_in_ago is not None:
            parts.append(f"last cave-in: {last_cave_in_ago} digs ago, -{last_cave_in_loss} blocks")
        lines.append(f"Last {len(digs)} digs: {', '.join(parts)}")

        # Depth trend (oldest to newest)
        oldest = digs[-1]
        newest = digs[0]
        d_from = oldest.get("depth_before", 0) or 0
        d_to = newest.get("depth_after", newest.get("depth_before", 0)) or 0
        delta = d_to - d_from
        sign = "+" if delta >= 0 else ""
        max_depth = int(tunnel.get("max_depth", 0) or 0)
        trend = f"Depth trend: {d_from}->{d_to} ({sign}{delta})"
        if max_depth:
            trend += f" | Record: {max_depth}"
        lines.append(trend)

    # --- Lifetime stats ---
    total_digs = int(tunnel.get("total_digs", 0) or 0)
    total_jc = int(tunnel.get("total_jc_earned", 0) or 0)
    if total_digs or total_jc:
        lines.append(f"Lifetime: {total_digs} digs, {total_jc} JC earned")

    # --- Boss progress ---
    boss_raw = tunnel.get("boss_progress")
    if boss_raw:
        if isinstance(boss_raw, str):
            try:
                boss_prog = json.loads(boss_raw)
            except (json.JSONDecodeError, TypeError):
                boss_prog = {}
        else:
            boss_prog = boss_raw if isinstance(boss_raw, dict) else {}
        if boss_prog:
            boss_parts = []
            attempts_raw = tunnel.get("boss_attempts")
            attempts = 0
            if isinstance(attempts_raw, int):
                attempts = attempts_raw
            elif isinstance(attempts_raw, str):
                try:
                    attempts = int(attempts_raw)
                except (ValueError, TypeError):
                    pass
            for boundary in sorted(_SHORT_BOSS_NAMES):
                name = _SHORT_BOSS_NAMES[boundary]
                status = boss_prog.get(str(boundary), "active")
                if status == "defeated":
                    boss_parts.append(f"{name} \u2713")
                elif status == "phase1_defeated":
                    boss_parts.append(f"{name} (phase 2)")
                elif attempts:
                    boss_parts.append(f"{name} ({attempts} attempts)")
                    attempts = 0  # only show attempts for the first active boss
            if boss_parts:
                lines.append(f"Bosses: {', '.join(boss_parts)}")

    # --- Prestige ---
    prestige = int(tunnel.get("prestige_level", 0) or 0)
    if prestige:
        best_score = int(tunnel.get("best_run_score", 0) or 0)
        lines.append(f"Prestige: level {prestige}, best run score {best_score}")

    # --- Recent events ---
    events = [a for a in recent_actions if a.get("action_type") == "event"]
    if events:
        event_ids = []
        for e in events:
            detail = _parse_detail(e)
            eid = detail.get("event_id", "")
            if eid and eid not in event_ids:
                event_ids.append(eid)
        if event_ids:
            lines.append(f"Recent events: {', '.join(event_ids[:5])}")

    # --- Recent boss fights ---
    boss_fights = [a for a in recent_actions if a.get("action_type") == "boss_fight"]
    if boss_fights:
        fight_parts = []
        for bf in boss_fights[:3]:
            detail = _parse_detail(bf)
            won = detail.get("won", False)
            risk = detail.get("risk", "?")
            boundary = detail.get("boundary", 0)
            boss_name = _SHORT_BOSS_NAMES.get(boundary, f"boss@{boundary}")
            outcome = "won" if won else "lost"
            fight_parts.append(f"{outcome} vs {boss_name} ({risk})")
        lines.append(f"Boss fights: {', '.join(fight_parts)}")

    return "\n".join(lines)


def build_dice_results_context(dice_results: list[dict]) -> str:
    """Serialize app-rolled dice results for the final DM resolution call."""
    if not dice_results:
        return ""
    lines = ["Use these dice as binding randomness:"]
    for result in dice_results:
        label = result.get("label", "roll")
        sides = result.get("sides", "?")
        rolls = result.get("rolls", [])
        modifier = result.get("modifier", 0)
        total = result.get("total", 0)
        roll_text = ", ".join(str(r) for r in rolls)
        if modifier:
            lines.append(f"{label}: d{sides} [{roll_text}] + {modifier} = {total}")
        else:
            lines.append(f"{label}: d{sides} [{roll_text}] = {total}")
    return "\n".join(lines)


# ===================================================================
# Message builder
# ===================================================================


def build_messages(
    system_prompt: str,
    player_state: str,
    personality: str,
    outcome: str,
    multiplayer: str,
    history: str = "",
) -> list[dict[str, str]]:
    """Construct the messages list for ``AIService.call_with_tools``.

    Returns a two-element list: a system message and a user message
    combining all context sections with clear section headers.
    """
    user_sections: list[str] = []

    user_sections.append("=== PLAYER STATE ===\n" + player_state)
    user_sections.append("=== PERSONALITY ===\n" + personality)
    if history:
        user_sections.append("=== DIG HISTORY ===\n" + history)
    user_sections.append("=== DIG OUTCOME ===\n" + outcome)

    if multiplayer:
        user_sections.append("=== MULTIPLAYER ===\n" + multiplayer)

    return [
        {"role": "system", "content": system_prompt},
        {"role": "user", "content": "\n\n".join(user_sections)},
    ]


def build_engine_messages(
    system_prompt: str,
    player_state: str,
    personality: str,
    preconditions_ctx: str,
    multiplayer: str,
    dice_results: str = "",
    history: str = "",
) -> list[dict[str, str]]:
    """Construct the messages list for the DM engine mode.

    Similar to ``build_messages`` but uses preconditions context instead
    of a post-hoc outcome context.
    """
    user_sections: list[str] = []

    user_sections.append("=== PLAYER STATE ===\n" + player_state)
    user_sections.append("=== PERSONALITY ===\n" + personality)
    if history:
        user_sections.append("=== DIG HISTORY ===\n" + history)
    user_sections.append("=== DIG PRECONDITIONS ===\n" + preconditions_ctx)

    if dice_results:
        user_sections.append("=== DICE RESULTS ===\n" + dice_results)

    if multiplayer:
        user_sections.append("=== MULTIPLAYER ===\n" + multiplayer)

    return [
        {"role": "system", "content": system_prompt},
        {"role": "user", "content": "\n\n".join(user_sections)},
    ]
