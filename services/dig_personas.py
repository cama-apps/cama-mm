"""Persona roster for /dig's cryptic GIF captions.

The dig minigame keeps a quiet, mythic tone. When a rare animated moment fires,
one of these voices narrates it in a line or two — cryptic, weighty, and never
explaining mechanics. Mirrors flavor_personas.pick_persona: a themed roster
picked at random (optionally biased toward the moment), invisible to users.
"""

from __future__ import annotations

import random
from dataclasses import dataclass

DIG_NARRATOR_SYSTEM_PROMPT = (
    "You are an ancient, indifferent presence of the deep earth, narrating the "
    "fate of a lone digger. You speak in omen and image, never in explanation.\n\n"
    "Voice rules:\n"
    "- One or two short lines. Cryptic, mythic, weighty — like an epitaph or a riddle.\n"
    "- NEVER name game mechanics, numbers, currencies, items, depths, or systems. "
    "Only image, omen, and consequence.\n"
    "- No emojis. No exclamation marks. No modern slang. No instructions to the reader.\n"
    "- You are not helpful. You are old, vast, and certain.\n"
    "- Do not use the digger's name. Do not break the spell."
)


@dataclass(frozen=True)
class DigVoice:
    """A narrator voice for /dig GIF captions."""

    key: str
    name: str
    description: str
    affinity: tuple[str, ...] = ()  # event keys this voice favors


DIG_VOICES: dict[str, DigVoice] = {
    "the_deep": DigVoice(
        "the_deep", "THE DEEP", "vast and indifferent, speaking in geologic time"
    ),
    "the_stone": DigVoice(
        "the_stone", "THE STONE", "terse, riddling, mineral and cold"
    ),
    "old_pick": DigVoice(
        "old_pick", "OLD PICK", "a long-dead miner's dry, weary murmur"
    ),
    "the_vein": DigVoice(
        "the_vein",
        "THE VEIN",
        "covetous and glittering, hungry for what is unearthed",
        ("rare_relic", "legendary_relic", "boss_victory"),
    ),
    "the_damp": DigVoice(
        "the_damp",
        "THE DAMP",
        "creeping, cold, patient — the dark that waits",
        ("cave_in",),
    ),
    "the_lantern": DigVoice(
        "the_lantern",
        "THE LANTERN",
        "the last light; wry, almost kind, flickering",
        ("boss_victory", "rare_relic"),
    ),
    "a_drowned_map": DigVoice(
        "a_drowned_map",
        "A DROWNED MAP",
        "measures all distances, mourns nothing, charts the descent",
        ("pinnacle", "prestige", "cave_in"),
    ),
}


DIG_FALLBACK_LINES: dict[str, list[str]] = {
    "boss_victory": [
        "the guardian kept its post for an age. it keeps nothing now.",
        "something that never knelt has knelt.",
        "the dark is quieter by one old hunger.",
    ],
    "rare_relic": [
        "the deep gives up one of its own. it will want it back.",
        "older than the tunnel, older than the hand that holds it.",
        "it was waiting. it is always waiting.",
    ],
    "legendary_relic": [
        "it waited longer than your name will last.",
        "the deep opens its hand. it does this once an age.",
        "something old surfaces, and remembers being held.",
    ],
    "cave_in": [
        "the dark keeps what the dark is owed.",
        "stone forgets you were ever here.",
        "the way down closes like a mouth.",
    ],
    "pinnacle": [
        "there is no further down. only this.",
        "the descent ends where the world does.",
        "you have reached the floor of everything.",
    ],
    "prestige": [
        "what goes down far enough comes back changed.",
        "the dark exhales, and lets one rise.",
        "you climb out wearing the deep like a second skin.",
    ],
    "_default": [
        "the deep notices. it rarely does.",
        "something shifts in the dark, and is still again.",
    ],
}


def pick_dig_voice(
    event_key: str | None = None, rng: random.Random | None = None
) -> DigVoice:
    """Pick a dig narrator voice, biased toward those with affinity for the event.

    Pass a seeded `random.Random` for deterministic tests.
    """
    chooser = rng if rng is not None else random
    voices = list(DIG_VOICES.values())
    if event_key:
        matching = [v for v in voices if event_key in v.affinity]
        if matching and chooser.random() < 0.7:
            return chooser.choice(matching)
    return chooser.choice(voices)


def fallback_line(event_key: str, rng: random.Random | None = None) -> str:
    """A static cryptic line for the event (used when the LLM is off/declined)."""
    chooser = rng if rng is not None else random
    pool = DIG_FALLBACK_LINES.get(event_key) or DIG_FALLBACK_LINES["_default"]
    return chooser.choice(pool)
