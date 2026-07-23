"""Helpers for projecting OpenDota draft data into Scout's compact ban rows."""

from __future__ import annotations

import json
from typing import Any


def extract_match_bans(
    enrichment_data: str | dict[str, Any] | None,
) -> list[tuple[int, int, int]]:
    """Return ``(ban_index, team, hero_id)`` rows from an enrichment payload.

    OpenDota represents Radiant as team ``0`` and Dire as team ``1``. Invalid
    or partial payloads intentionally produce only the valid rows they contain;
    enrichment storage must not fail because optional draft data is malformed.
    """
    if not enrichment_data:
        return []

    if isinstance(enrichment_data, str):
        try:
            payload = json.loads(enrichment_data)
        except (json.JSONDecodeError, TypeError):
            return []
    elif isinstance(enrichment_data, dict):
        payload = enrichment_data
    else:
        return []

    picks_bans = payload.get("picks_bans")
    if not isinstance(picks_bans, list):
        return []

    bans: list[tuple[int, int, int]] = []
    for ban_index, entry in enumerate(picks_bans):
        if not isinstance(entry, dict):
            continue

        is_pick = entry.get("is_pick")
        if is_pick is not False and is_pick != 0:
            continue

        team = entry.get("team")
        hero_id = entry.get("hero_id")
        if isinstance(team, bool) or isinstance(hero_id, bool):
            continue
        try:
            team = int(team)
            hero_id = int(hero_id)
        except (TypeError, ValueError):
            continue
        if team not in (0, 1) or hero_id <= 0:
            continue

        bans.append((ban_index, team, hero_id))

    return bans
