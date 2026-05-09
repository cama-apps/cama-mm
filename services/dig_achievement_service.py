"""Achievement unlock checks for the dig minigame.

Walks the static ``ACHIEVEMENTS`` table after each significant action and
records anything newly satisfied, paying out the JC reward via
``player_repo``. Stateless aside from the two repository handles.
"""

from __future__ import annotations

import json
from typing import TYPE_CHECKING

from services.dig_constants import ACHIEVEMENTS, BOSS_BOUNDARIES

if TYPE_CHECKING:
    from repositories.dig_repository import DigRepository
    from repositories.player_repository import PlayerRepository


def _normalize_boss_progress(tunnel: dict) -> dict:
    """Read ``boss_progress`` JSON into a flat ``{depth_str: status}`` dict.

    Mirrors ``DigService._get_boss_progress`` for the achievement check —
    duplicated here so this service has zero dependency on the orchestrator's
    private helpers (and zero risk of an import cycle).
    """
    canonical = {str(b): "active" for b in BOSS_BOUNDARIES}
    raw = tunnel.get("boss_progress")
    if not raw:
        return canonical
    try:
        stored = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return canonical
    normalized: dict = {}
    for key, val in stored.items():
        if isinstance(val, dict):
            normalized[key] = val.get("status", "active")
        else:
            normalized[key] = val
    canonical.update(normalized)
    return canonical


class DigAchievementService:
    """Achievement unlock evaluator."""

    def __init__(
        self,
        dig_repo: DigRepository,
        player_repo: PlayerRepository,
    ) -> None:
        self.dig_repo = dig_repo
        self.player_repo = player_repo

    def check_achievements(
        self, discord_id: int, guild_id, tunnel: dict, context: dict
    ) -> list[dict]:
        """
        Check all achievement conditions. Return newly unlocked achievements.

        context: dict with what just happened (action, advance, boss_win, etc.)
        """
        existing = self.dig_repo.get_achievements(discord_id, guild_id)
        existing_ids = {a.get("achievement_id") for a in existing}

        newly_unlocked = []

        for ach in ACHIEVEMENTS:
            if ach["id"] in existing_ids:
                continue

            unlocked = False
            condition = ach.get("condition", {})
            ctype = condition.get("type")

            if ctype == "depth":
                if tunnel.get("depth", 0) >= condition.get("value", 0):
                    unlocked = True
            elif ctype == "total_digs":
                if tunnel.get("total_digs", 0) >= condition.get("value", 0):
                    unlocked = True
            elif ctype == "streak":
                if tunnel.get("streak_days", 0) >= condition.get("value", 0):
                    unlocked = True
            elif ctype == "boss_win":
                if context.get("action") == "boss_win":
                    unlocked = True
            elif ctype == "all_bosses":
                bp = context.get("boss_progress") or _normalize_boss_progress(tunnel)
                if all(
                    (v.get("status") if isinstance(v, dict) else v) == "defeated"
                    for v in bp.values()
                ):
                    unlocked = True
            elif ctype == "prestige":
                if tunnel.get("prestige_level", 0) >= condition.get("value", 0):
                    unlocked = True
            elif ctype == "cave_in" and context.get("action") == "cave_in":
                unlocked = True

            if unlocked:
                self.dig_repo.add_achievement(
                    discord_id, guild_id,
                    achievement_id=ach["id"],
                    name=ach["name"],
                )
                newly_unlocked.append({
                    "id": ach["id"],
                    "name": ach["name"],
                    "description": ach.get("description", ""),
                    "reward": ach.get("reward", 0),
                })

                # Award JC reward
                if ach.get("reward", 0) > 0:
                    self.player_repo.add_balance(discord_id, guild_id, ach["reward"])

        return newly_unlocked
