"""Read-only display aggregators for the dig minigame.

Leaderboard / hall of fame / museum / collection / guild-stats queries —
all of them are pure ``dig_repo`` reads with light shaping for the embed
layer. No tunnel-state mutation, no balance changes, no cross-service
dependencies.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from services.dig_constants import ARTIFACT_POOL

if TYPE_CHECKING:
    from repositories.dig_repository import DigRepository


def _ok(**kwargs) -> dict:
    """Return a standard success result. Mirrors DigService._ok."""
    result = {"success": True, "error": None}
    result.update(kwargs)
    if "depth_after" in result and "depth" not in result:
        result["depth"] = result["depth_after"]
    return result


class DigLeaderboardService:
    """Leaderboard, hall of fame, museum, and collection read aggregators."""

    def __init__(self, dig_repo: DigRepository) -> None:
        self.dig_repo = dig_repo

    def get_leaderboard(self, guild_id) -> dict:
        """Get top 10 tunnels and ASCII community mine view."""
        tunnels = self.dig_repo.get_top_tunnels(guild_id, limit=10)
        tunnels = [dict(t) for t in tunnels]

        # Generate ASCII art
        max_depth = max((t.get("depth", 0) for t in tunnels), default=1) or 1
        lines = []
        for i, t in enumerate(tunnels, 1):
            depth = t.get("depth", 0)
            bar_len = max(1, int(40 * depth / max_depth))
            bar = "█" * bar_len
            name = t.get("tunnel_name", "???")[:15]
            lines.append(f"{i:>2}. {name:<15} {bar} {depth}m")

        ascii_art = "\n".join(lines)

        return {
            "tunnels": tunnels,
            "ascii_art": ascii_art,
        }

    def get_hall_of_fame(self, guild_id) -> dict:
        """Get guild leaderboard of best prestige run scores."""
        rows = self.dig_repo.get_hall_of_fame(guild_id)
        entries = []
        for row in rows:
            r = dict(row) if not isinstance(row, dict) else row
            entries.append({
                "discord_id": r.get("discord_id"),
                "tunnel_name": r.get("tunnel_name", "Unknown"),
                "prestige_level": r.get("prestige_level", 0),
                "best_run_score": r.get("best_run_score", 0),
            })
        return _ok(entries=entries)

    def get_collection(self, discord_id: int, guild_id) -> dict:
        """Return a player's discovered artifacts grouped by rarity.

        ``dig_artifacts`` rows store no rarity, so it is resolved from
        ``ARTIFACT_POOL`` by ``artifact_id``.
        """
        artifacts = self.dig_repo.get_artifacts(discord_id, guild_id)
        rarity_by_id = {a["id"]: a["rarity"] for a in ARTIFACT_POOL}
        collection = {}
        for a in artifacts:
            a = dict(a)
            rarity = rarity_by_id.get(a.get("artifact_id"), "common")
            if rarity not in collection:
                collection[rarity] = []
            collection[rarity].append(a)
        return {"artifacts": collection, "total": len(artifacts)}

    def get_museum(self, guild_id) -> dict:
        """Return guild artifact registry with first finders and counts."""
        entries = self.dig_repo.get_registry(guild_id)
        entries = [dict(e) for e in entries]
        layer_by_id = {a["id"]: a["layer"] for a in ARTIFACT_POOL}

        # Group by the artifact's layer (resolved from the pool by id).
        by_layer = {}
        for e in entries:
            layer = layer_by_id.get(e.get("artifact_id")) or "unknown"
            if layer not in by_layer:
                by_layer[layer] = []
            by_layer[layer].append(e)

        return {
            "entries": entries,
            "by_layer": by_layer,
            "total_discovered": len(entries),
            "total_possible": len(ARTIFACT_POOL),
        }

    def get_guild_stats(self, guild_id) -> dict:
        """Aggregate stats for the guild."""
        tunnels = self.dig_repo.get_all_tunnels(guild_id)
        tunnels = [dict(t) for t in tunnels]

        if not tunnels:
            return _ok(
                total_digs=0,
                total_depth=0,
                total_jc_earned=0,
                most_active=None,
                deepest=None,
                tunnel_count=0,
            )

        total_digs = sum(t.get("total_digs", 0) or 0 for t in tunnels)
        total_depth = sum(t.get("depth", 0) or 0 for t in tunnels)
        total_jc = sum(t.get("total_jc_earned", 0) or 0 for t in tunnels)

        most_active = max(tunnels, key=lambda t: t.get("total_digs", 0) or 0)
        deepest = max(tunnels, key=lambda t: t.get("depth", 0) or 0)

        return _ok(
            total_digs=total_digs,
            total_depth=total_depth,
            total_jc_earned=total_jc,
            most_active={
                "discord_id": most_active.get("discord_id"),
                "name": most_active.get("tunnel_name"),
                "total_digs": most_active.get("total_digs", 0),
            },
            deepest={
                "discord_id": deepest.get("discord_id"),
                "name": deepest.get("tunnel_name"),
                "depth": deepest.get("depth", 0),
            },
            tunnel_count=len(tunnels),
        )
