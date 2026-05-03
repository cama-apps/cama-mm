"""Curated NPC roster for the dig DM's narrative layer.

Each entry is a titled (no proper-name) figure the LLM may invoke during a
dig narration. The DM is also free to invent novel NPCs and persist them via
the memory blob; this roster is the canon set, biased toward recognition
rather than world-filling.

Entries span the three rolled tone profiles (cosmic_dread, industrial_grim,
cryptic_folkloric) so the chosen voice has someone on-tone to summon when
the moment fits.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class DigNPC:
    """A canonical NPC the DM may reference in dig narration."""

    npc_id: str
    title: str
    voice: str
    triggers: str
    sample_lines: list[str]


NPCS: dict[str, DigNPC] = {
    "the_surveyor": DigNPC(
        npc_id="the_surveyor",
        title="the Surveyor",
        voice="industrial_grim",
        triggers=(
            "Layer transitions, hesitation at boundaries, depth "
            "milestones. Speaks in short fragments about old shafts."
        ),
        sample_lines=[
            "Shaft 1923 went bad here too. Nothing rebuilt it.",
            "Stone reads the same in any layer. Grain runs east. Yours runs through it.",
            "I marked this passage twenty years ago. Nobody asked me back.",
        ],
    ),
    "the_old_hand": DigNPC(
        npc_id="the_old_hand",
        title="the Old Hand",
        voice="industrial_grim",
        triggers=(
            "After cave-ins, after long streaks, after debt or loss. "
            "Sympathetic but dry. Never sentimental."
        ),
        sample_lines=[
            "Walked away from worse. Walk on.",
            "First one's the bad one. Each after, you carry a little less.",
            "You're alright, kid. Keep your hands where you can see them.",
        ],
    ),
    "the_one_who_counts": DigNPC(
        npc_id="the_one_who_counts",
        title="the One Who Counts",
        voice="cosmic_dread",
        triggers=(
            "Boss boundaries, prestige resets, deep layers. Rarely speaks. "
            "Is felt rather than seen. Marks tally on the wall."
        ),
        sample_lines=[
            "There is a wall somewhere. New marks have appeared on it.",
            "You feel counted. You do not know by what.",
            "Something in the dark has finished tallying. It begins again.",
        ],
    ),
    "the_listener": DigNPC(
        npc_id="the_listener",
        title="the Listener",
        voice="cryptic_folkloric",
        triggers=(
            "Low luminosity, risky-streak digs, after a vow or grudge. "
            "Speaks like local superstition. Repeats herself."
        ),
        sample_lines=[
            "The dark has been listening. The dark has been listening.",
            "They say if you hear your own name down here, do not turn.",
            "The names go in. The names do not always come back.",
        ],
    ),
    "the_foreman": DigNPC(
        npc_id="the_foreman",
        title="the Foreman",
        voice="industrial_grim",
        triggers=(
            "Big JC hauls, after debt, near shop activity, after losses "
            "to the bombs / bets. Pragmatic, transactional, mocks waste."
        ),
        sample_lines=[
            "Half of what you dug today, gone before sundown.",
            "You earn it down here. You spend it up there. The arithmetic does the rest.",
            "Nobody's asking how you sleep. They're asking what you owe.",
        ],
    ),
}


def roster_lines() -> list[str]:
    """Compact bullet lines for prompt injection (id, title, triggers)."""
    return [
        f"- {npc.npc_id} ({npc.title}, {npc.voice}): {npc.triggers}"
        for npc in NPCS.values()
    ]
