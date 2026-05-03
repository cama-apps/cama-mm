"""
Tool definitions, system prompts, and context builders for the dig flavor
LLM layer.

Pure data + helpers -- no external dependencies beyond the standard library.
Imported by ``DigFlavorService`` to construct the messages list that gets
sent to ``AIService.call_with_tools``.
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
# Tool definition -- single fat narrate_dig + splash narration
# ===================================================================

NARRATE_DIG_TOOL: dict[str, Any] = {
    "type": "function",
    "function": {
        "name": "narrate_dig",
        "description": (
            "Add flavor on top of an already-resolved dig outcome. Mechanics "
            "are immutable; this call writes prose and optional embellishments "
            "(NPC line, narrative-only event pick, small JC nudge, memory note)."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "narrative": {
                    "type": "string",
                    "description": (
                        "2-4 sentences, ≤300 chars total. Atmospheric, on the "
                        "rolled tone profile. NEVER include numbers, JC amounts, "
                        "block counts, depths, or percentages — those live in "
                        "the embed fields below your prose."
                    ),
                },
                "tone": {
                    "type": "string",
                    "enum": ["cosmic_dread", "industrial_grim", "cryptic_folkloric"],
                    "description": (
                        "MUST match the tone profile injected into the system "
                        "prompt for this dig. If you return a different one, "
                        "the validator will overwrite it."
                    ),
                },
                "callback_reference": {
                    "type": "string",
                    "description": (
                        "Optional. A short fragment from prior memory worth "
                        "calling back to (vow, NPC encounter, recurring motif). "
                        "Empty string if nothing fits."
                    ),
                },
                "picked_event_id": {
                    "type": "string",
                    "description": (
                        "Optional. When the user message lists ELIGIBLE EVENTS, "
                        "pick exactly one id from that list. Out-of-list ids "
                        "are silently dropped. Empty string for no event."
                    ),
                },
                "npc_appearance": {
                    "type": "object",
                    "description": (
                        "Optional. Either a roster id (preferred) or an "
                        "invented title; plus the NPC's single short line. "
                        "Invented names will be persisted to memory."
                    ),
                    "properties": {
                        "id_or_name": {
                            "type": "string",
                            "description": "Roster id (e.g. 'the_old_hand') or invented title.",
                        },
                        "line": {
                            "type": "string",
                            "description": "One short line, ≤200 chars, in the NPC's voice.",
                        },
                    },
                },
                "flavor_bonus_pct": {
                    "type": "number",
                    "description": (
                        "Optional. JC nudge as a signed percent of jc_earned. "
                        "Will be CLAMPED to ±cap (announced in system prompt). "
                        "Use sparingly — only when the moment earns it. "
                        "0 or omitted = no nudge."
                    ),
                },
                "memory_update": {
                    "type": "string",
                    "description": (
                        "Optional. Rewrite your scratchpad — what you want to "
                        "remember about this player for next time. ≤2KB. "
                        "Omit to leave existing memory unchanged."
                    ),
                },
            },
            "required": ["narrative", "tone"],
        },
    },
}

SPLASH_NARRATION_TOOL: dict[str, Any] = {
    "type": "function",
    "function": {
        "name": "narrate_splash",
        "description": (
            "Narrate the moment a tunnel event reaches other players. "
            "Mechanics are already resolved — the JC has moved. "
            "Output is one short paragraph, no numbers, no mechanics."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "narrative": {
                    "type": "string",
                    "description": (
                        "1-2 sentences (≤200 characters total). "
                        "Name the victim(s) by Discord display name. "
                        "Match the event's vibe (luminous / ominous / etc.). "
                        "Do NOT mention JC amounts, rolls, success rates, or rarity."
                    ),
                },
            },
            "required": ["narrative"],
        },
    },
}

# ===================================================================
# Tone profiles (one is randomly rolled per dig and injected into prompt)
# ===================================================================

TONE_PROFILES: dict[str, str] = {
    "cosmic_dread": (
        "Cosmic / mythic dread. The depths are old and indifferent. The "
        "player is small. Something ancient watches. Hushed, ominous, "
        "sparing with words. Reference Annihilation, Dark Souls item "
        "descriptions. Avoid action verbs when stillness will do."
    ),
    "industrial_grim": (
        "Industrial / blue-collar grim. Tunnels are dirty, the pickaxes "
        "are real, the danger is mechanical. Workers, debt, dust. Terse "
        "and stoic, dark humor when something goes wrong. Reference "
        "Disco Elysium worker dialogue, early Coen Brothers."
    ),
    "cryptic_folkloric": (
        "Cryptic / folkloric. Local superstition, half-remembered rules, "
        "things you don't say aloud. Oblique and rhythmic, repetition and "
        "refrains. Reference Annie Proulx, regional folklore, weird "
        "Americana. The narration is allowed to speak in fragments."
    ),
}

# ===================================================================
# System prompt template -- additive flavor only
# ===================================================================

DIG_FLAVOR_SYSTEM_PROMPT_TEMPLATE: str = """\
You are an additive flavor layer on top of a Dota 2 inhouse Discord bot's \
tunnel-mining minigame. The mechanical outcome of this dig (advance, JC, \
cave-in, milestones, splashes) HAS ALREADY BEEN COMPUTED AND PERSISTED. You \
do not decide what happened. You write prose and select on-tone color around it.

TONE PROFILE FOR THIS DIG: {tone_name}
{tone_guide}

YOUR JOB: Call narrate_dig with at least narrative + tone. You may also pass:
  - picked_event_id — pick from the ELIGIBLE EVENTS list in the user message, \
    when there is one. Out-of-list ids are silently dropped.
  - npc_appearance — invoke a canon NPC from the NPC ROSTER, OR invent a new \
    titled figure when none fits. Invented names are persisted to memory and \
    can recur. Roster and invention are weighted equally — pick whichever the \
    moment calls for. Use sparingly; not every dig needs an NPC.
  - flavor_bonus_pct — your one tiny mechanical knob. CAP FOR THIS DIG: \
    ±{bonus_cap}%. Out-of-range values are clamped silently. Use only when a \
    narrative beat earns the swing. Most digs should be 0.
  - memory_update — rewrite your scratchpad. Up to 2KB. Keep what matters; \
    drop what's stale. Omit to leave it unchanged.

HARD RULES:
  - NUMBERS ARE FORBIDDEN IN PROSE. No JC amounts, no block counts, no \
    depth integers, no percentages, no times. The embed shows mechanics; \
    your narrative is atmosphere only. Narration containing digits will be \
    rejected and replaced with canned text.
  - STAY ON THE TONE PROFILE above. Reread it if you forget which voice.
  - Tight: ≤300 characters, 2-4 sentences max.
  - Reference the active player by their Discord display name. Other miners' \
    names are fine when narratively useful.
  - Only narrate what ACTUALLY HAPPENED. The DIG OUTCOME section is \
    authoritative. Don't describe a cave-in if there was none, don't claim \
    a milestone the player didn't cross.
  - Don't expose mechanics. Don't say "luminosity" — say "light", "glow", \
    "the dark". Don't name S stats — show them indirectly.

NPC TIPS:
  - Each canon NPC has triggers describing when they fit. Honor them.
  - Invented NPCs should be titled (no proper names). "The Watcher", "the \
    Saw-bones", "the One Who Listens" — never "Eli" or "Marda".
  - One NPC per dig at most. Most digs have none.

OUTPUT: Always call narrate_dig. No plain text responses.\
"""


def build_dig_flavor_system_prompt(tone_key: str, bonus_cap_pct: float) -> str:
    """Render the flavor system prompt with the rolled tone + bonus cap injected."""
    tone_guide = TONE_PROFILES.get(tone_key, TONE_PROFILES["industrial_grim"])
    return DIG_FLAVOR_SYSTEM_PROMPT_TEMPLATE.format(
        tone_name=tone_key,
        tone_guide=tone_guide,
        bonus_cap=int(bonus_cap_pct),
    )


SPLASH_NARRATION_SYSTEM_PROMPT: str = """\
You write a single short prose beat — 1 to 2 sentences, ≤200 characters total — \
for the moment a tunnel event reaches other players in a Dota 2 inhouse league \
Discord bot's mining minigame.

Your sole output is that prose. The mechanical effects (JC movement, victims \
chosen) are already resolved — do NOT restate amounts, rolls, success chances, \
or rarity.

Style:
- Lean into the EVENT FLAVOR provided. If the event reads gentle/luminous, the \
  beat is gentle. If it reads ominous/predatory, the beat is ominous.
- Address victims by their Discord display name as given.
- For SPLASH MODE = "steal": the digger TOOK from the victim — sting, not \
  cooperation.
- For SPLASH MODE = "burn": the victim LOST coin to the cave/world (not to \
  the digger) — a misfortune that spilled outward.
- For SPLASH MODE = "grant": the victim RECEIVED a small gift — share the warmth.
- Atmospheric over mechanical. No exposed game terms. No emoji.
- Never quote raw numbers, percentages, or 'JC'.

OUTPUT: Call the narrate_splash tool with the prose in the 'narrative' field. \
Nothing else.\
"""

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

    # Pickaxe tier 7+ (Void-Touched) salvages 1 block on cave-in
    pickaxe_tier = preconditions.get("pickaxe_tier", 0)
    if pickaxe_tier >= 7:
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


# ===================================================================
# Message builders
# ===================================================================


def build_messages(
    system_prompt: str,
    player_state: str,
    personality: str,
    outcome: str,
    multiplayer: str,
    history: str = "",
    dm_context: str = "",
    eligible_events: str = "",
) -> list[dict[str, str]]:
    """Construct the messages list for the dig flavor LLM call.

    ``dm_context`` is the pre-loaded narrative memory + roster + out-of-world
    bundle (rendered by ``services.dig_dm_context.render_dm_context_section``).
    ``eligible_events`` is a one-line list of event ids the LLM may pick from
    via ``picked_event_id``; empty string when no event is eligible.
    """
    user_sections: list[str] = []

    user_sections.append("=== PLAYER STATE ===\n" + player_state)
    user_sections.append("=== PERSONALITY ===\n" + personality)
    if dm_context:
        user_sections.append("=== DM CONTEXT ===\n" + dm_context)
    if history:
        user_sections.append("=== DIG HISTORY ===\n" + history)
    user_sections.append("=== DIG OUTCOME ===\n" + outcome)
    if eligible_events:
        user_sections.append("=== ELIGIBLE EVENTS ===\n" + eligible_events)

    if multiplayer:
        user_sections.append("=== MULTIPLAYER ===\n" + multiplayer)

    return [
        {"role": "system", "content": system_prompt},
        {"role": "user", "content": "\n\n".join(user_sections)},
    ]


def build_splash_narration_messages(
    digger_name: str,
    digger_layer: str,
    event_name: str,
    event_description: str,
    splash_mode: str,
    victims: list[dict],
) -> list[dict[str, str]]:
    """Build the messages list for the splash narration LLM call.

    ``victims`` is a list of ``{"name": <str>, "amount": <int>}`` dicts —
    the amount is included for context only; the model is instructed
    not to quote it.
    """
    victim_lines = "\n".join(
        f"- {v.get('name', 'a stranger')}" for v in victims
    ) or "- (no resolvable names)"
    sections: list[str] = [
        f"=== DIGGER ===\n{digger_name} (in the {digger_layer})",
        f"=== EVENT ===\n{event_name}\n{event_description}",
        f"=== SPLASH MODE ===\n{splash_mode}",
        f"=== VICTIMS ===\n{victim_lines}",
    ]
    return [
        {"role": "system", "content": SPLASH_NARRATION_SYSTEM_PROMPT},
        {"role": "user", "content": "\n\n".join(sections)},
    ]


