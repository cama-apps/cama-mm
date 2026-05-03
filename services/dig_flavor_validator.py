"""Validation + clamping for the dig flavor LLM tool-call output.

The flavor layer can never alter mechanical outcomes other than a small
bounded JC delta, and must produce on-style narration. This module enforces:

- Narrative length cap.
- Numbers-in-prose check (regex-rejects digit-bearing narration).
- ``picked_event_id`` must be in the eligible-events allow-list.
- ``flavor_bonus_pct`` clamped symmetrically to the rolled cap (5 or 10).
- ``npc_appearance.line`` length cap.
- ``memory_update`` byte cap (matches DigRepository.DM_MEMORY_MAX_BYTES).
- ``tone`` must match the rolled tone profile.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

VALID_TONES = frozenset({"cosmic_dread", "industrial_grim", "cryptic_folkloric"})

NARRATIVE_MAX_CHARS = 300
NPC_LINE_MAX_CHARS = 200
MEMORY_MAX_BYTES = 2048
CALLBACK_MAX_CHARS = 200

# Reject digit-bearing narration outright. Numbers belong in embed fields.
_NUMERIC_RE = re.compile(r"\d")


@dataclass(frozen=True)
class FlavorResult:
    """Validated, clamped output from the dig flavor LLM call."""

    narrative: str
    tone: str
    callback_reference: str
    picked_event_id: str | None
    npc_appearance: dict[str, str] | None
    flavor_bonus_pct: float
    memory_update: str | None


def validate_flavor_args(
    tool_args: dict[str, Any],
    *,
    eligible_event_ids: set[str],
    rolled_tone: str,
    bonus_cap_pct: float,
) -> FlavorResult | None:
    """Validate and clamp a `narrate_dig` tool-call payload.

    Returns ``None`` when the narrative is missing or fails the numbers-in-prose
    check; the caller falls back to canned text in that case.

    ``rolled_tone`` is the tone the system prompt told the DM to use; if the
    DM returned a different one, we override (the random roll is authoritative,
    not the LLM's preference). ``bonus_cap_pct`` is the pre-rolled cap (5 or
    10); ``flavor_bonus_pct`` is clamped to ±cap.
    """
    narrative = str(tool_args.get("narrative") or "").strip()
    if not narrative:
        return None
    if _NUMERIC_RE.search(narrative):
        # Hard rule: no numbers in narration.
        return None
    if len(narrative) > NARRATIVE_MAX_CHARS:
        narrative = narrative[: NARRATIVE_MAX_CHARS - 3].rstrip() + "..."

    tone = str(tool_args.get("tone") or rolled_tone)
    if tone not in VALID_TONES:
        tone = rolled_tone

    callback = str(tool_args.get("callback_reference") or "").strip()
    if len(callback) > CALLBACK_MAX_CHARS:
        callback = callback[: CALLBACK_MAX_CHARS - 3].rstrip() + "..."

    raw_event_id = str(tool_args.get("picked_event_id") or "").strip()
    picked_event_id: str | None = None
    if raw_event_id and raw_event_id in eligible_event_ids:
        picked_event_id = raw_event_id

    npc_appearance: dict[str, str] | None = None
    npc_raw = tool_args.get("npc_appearance") or {}
    if isinstance(npc_raw, dict):
        npc_id = str(npc_raw.get("id_or_name") or "").strip()
        npc_line = str(npc_raw.get("line") or "").strip()
        if npc_id and npc_line:
            if len(npc_line) > NPC_LINE_MAX_CHARS:
                npc_line = npc_line[: NPC_LINE_MAX_CHARS - 3].rstrip() + "..."
            npc_appearance = {"id_or_name": npc_id[:80], "line": npc_line}

    bonus_raw = tool_args.get("flavor_bonus_pct")
    try:
        bonus_pct = float(bonus_raw) if bonus_raw is not None else 0.0
    except (TypeError, ValueError):
        bonus_pct = 0.0
    cap = abs(float(bonus_cap_pct))
    if bonus_pct > cap:
        bonus_pct = cap
    elif bonus_pct < -cap:
        bonus_pct = -cap

    memory_raw = tool_args.get("memory_update")
    memory_update: str | None = None
    if memory_raw is not None:
        memory_text = str(memory_raw)
        encoded = memory_text.encode("utf-8")
        if len(encoded) > MEMORY_MAX_BYTES:
            memory_text = encoded[:MEMORY_MAX_BYTES].decode("utf-8", errors="ignore")
        memory_update = memory_text

    return FlavorResult(
        narrative=narrative,
        tone=tone,
        callback_reference=callback,
        picked_event_id=picked_event_id,
        npc_appearance=npc_appearance,
        flavor_bonus_pct=bonus_pct,
        memory_update=memory_update,
    )


def validate_splash_narrative(narrative: str) -> str:
    """Validate splash narration: trim, length-cap, reject empty.

    Splash narratives may contain numbers (less strict than dig narration);
    the splash prompt already forbids JC amounts, but we don't regex-reject.
    """
    text = (narrative or "").strip()
    if not text:
        return ""
    if len(text) > 200:
        text = text[:197].rstrip() + "..."
    return text
