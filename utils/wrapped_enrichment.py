"""Compact projections of OpenDota data used by Cama Wrapped."""

from __future__ import annotations

from collections.abc import Collection
from typing import Any


def extract_wrapped_enrichment_facts(
    match_data: Any,
    steam_ids: Collection[int],
) -> dict[str, Any] | None:
    """Return only the OpenDota fields consumed by Wrapped.

    ``None`` represents a missing or malformed match payload. A valid match
    payload still produces a fact record when the player cannot be matched;
    this preserves match-level comeback/throw values and the historical
    ``rapier_count == 0`` behavior.
    """
    if not isinstance(match_data, dict):
        return None

    players = match_data.get("players")
    if not isinstance(players, list):
        players = []
    player_data = next(
        (
            player
            for player in players
            if isinstance(player, dict) and player.get("account_id") in steam_ids
        ),
        None,
    )

    purchase_log = player_data.get("purchase_log") if player_data else None
    rapier_count = (
        sum(
            1
            for item in purchase_log
            if isinstance(item, dict) and item.get("key") == "rapier"
        )
        if isinstance(purchase_log, list)
        else 0
    )
    return {
        "actions_per_min": player_data.get("actions_per_min") if player_data else None,
        "courier_kills": player_data.get("courier_kills") if player_data else None,
        "pings": player_data.get("pings") if player_data else None,
        "rapier_count": rapier_count,
        "lane_role": player_data.get("lane_role") if player_data else None,
        "comeback": match_data.get("comeback"),
        "throw": match_data.get("throw"),
    }
