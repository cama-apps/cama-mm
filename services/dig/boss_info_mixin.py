"""BossInfoMixin mixin for :class:`DigService`.

Boss encounter-info builders and dialogue helpers: rendering barks,
picking dialogue lines, and assembling the boss-encounter payload the
UI renders. No combat resolution lives here — see ``combat_mixin``.

Mixin split out of the former monolithic ``dig_service`` module; it
carries no state of its own and is composed into ``DigService``.
"""

import json
import random

from services.dig_constants import (
    BOSS_ASCII,
    BOSS_DIALOGUE,
    BOSS_DIALOGUE_V2,
)


class BossInfoMixin:
    """BossInfoMixin — see module docstring.

    Composed into :class:`~services.dig_service.DigService`; relies on the
    attributes and helpers that the other mixins and the constructor provide.
    """

    def build_next_boss_encounter(self, discord_id: int, guild_id) -> dict | None:
        """Public boss-info payload for the player's current boss boundary, or None.

        Used by the auto-continue UX: after a phase-1 victory clears the
        encounter view, we re-fetch the next-phase encounter info and post a
        fresh BossEncounterView. The boss_progress now reflects the next
        phase, so the dialogue / boss state line up with what the player is
        about to fight.
        """
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return None
        tunnel = dict(tunnel)
        boss_progress = self._get_boss_progress_entries(tunnel)
        depth = tunnel.get("depth", 0)
        at_boss = self._at_boss_boundary(depth, boss_progress)
        if at_boss is None:
            return None
        info = self._build_boss_info(discord_id, guild_id, tunnel, at_boss)
        if isinstance(info, dict):
            return info
        # Some _build_boss_info paths return a dataclass — coerce.
        return info.__dict__ if hasattr(info, "__dict__") else None

    def _render_boss_bark(self, template: str, tunnel: dict) -> str:
        """Render a dialogue line by substituting stat-aware tokens.

        Supported tokens:
          {streak}          → streak_days (defaults to 1)
          {depth}           → current depth
          {prestige}        → current prestige_level
          {killed_boss_name} → name of a previously defeated boss in this delve;
                                falls back to "the early dark" when none.
        Lines without tokens render verbatim.
        """
        try:
            killed_name = "the early dark"
            try:
                bp = json.loads(tunnel.get("boss_progress") or "{}")
                defeated_ids = []
                for _depth, entry in bp.items():
                    status = entry.get("status") if isinstance(entry, dict) else entry
                    boss_id = entry.get("boss_id", "") if isinstance(entry, dict) else ""
                    if status == "defeated" and boss_id:
                        defeated_ids.append(boss_id)
                if defeated_ids:
                    from services.dig_constants import (
                        get_boss_by_id as _get_boss_by_id,
                    )
                    boss_def = _get_boss_by_id(random.choice(defeated_ids))
                    if boss_def is not None:
                        killed_name = boss_def.name
            except (json.JSONDecodeError, TypeError):
                pass

            return template.format(
                streak=tunnel.get("streak_days", 1) or 1,
                depth=tunnel.get("depth", 0) or 0,
                prestige=tunnel.get("prestige_level", 0) or 0,
                killed_boss_name=killed_name,
            )
        except (KeyError, IndexError):
            return template

    def _pick_boss_dialogue_line(
        self, boss_id: str, slot: str, fallback: str,
    ) -> str:
        """Random-pick a hand-authored line from BOSS_DIALOGUE_V2[boss_id][slot].

        Falls back to ``fallback`` when the boss or slot is missing — this
        keeps grandfathered bosses without a v2 entry from breaking the embed.
        """
        boss_lines = BOSS_DIALOGUE_V2.get(boss_id, {}).get(slot, [])
        if not boss_lines:
            return fallback
        return random.choice(boss_lines)

    def _read_boss_progress_entry(self, boss_progress: dict, boundary: int) -> dict:
        """Normalize a boss_progress entry to dict shape with default fields.

        Legacy string values (``"active"`` / ``"defeated"`` / ``"phase1_defeated"``)
        are wrapped as ``{"status": <string>}``. Missing fields default to
        ``status="active"``.
        """
        raw = boss_progress.get(str(boundary))
        if raw is None:
            return {"status": "active"}
        if isinstance(raw, str):
            return {"status": raw}
        if isinstance(raw, dict):
            return dict(raw)  # caller-mutable copy
        return {"status": "active"}

    def _build_boss_info(
        self, discord_id: int, guild_id, tunnel: dict, boundary: int,
    ) -> dict:
        """Build the boss encounter payload for a boundary.

        Locks a specific boss for this tunnel at this tier (idempotent), then
        picks a dialogue line from ``BOSS_DIALOGUE_V2`` keyed on
        ``first_meet`` / ``after_<last_outcome>`` if available, falling back
        to the legacy v1 dialogue list. Tokens like ``{streak}`` are
        substituted via ``_render_boss_bark``.

        For the pinnacle (depth 300), uses the rotating PINNACLE_BOSSES
        pool and the per-phase title/archetype.

        Updates ``first_meet_seen`` so the first-meet line only fires once
        per delve.
        """
        if self._is_pinnacle_depth(boundary):
            return self._build_pinnacle_info(discord_id, guild_id, tunnel)

        boss = self._ensure_boss_locked(discord_id, guild_id, tunnel, boundary)
        attempts = tunnel.get("boss_attempts", 0) or 0

        boss_progress = json.loads(tunnel.get("boss_progress") or "{}")
        entry = self._read_boss_progress_entry(boss_progress, boundary)
        last_outcome = entry.get("last_outcome")
        first_meet_seen = bool(entry.get("first_meet_seen", False))

        # Choose dialogue slot.
        if not first_meet_seen:
            slot = "first_meet"
        elif last_outcome in ("defeated", "retreat", "scout", "close_win"):
            slot = f"after_{last_outcome}"
        else:
            slot = "first_meet"  # default to first-meet flavor when no history

        v1_fallback_list = boss.dialogue or BOSS_DIALOGUE.get(boundary, ["..."])
        v1_fallback = v1_fallback_list[min(attempts, len(v1_fallback_list) - 1)]
        line = self._pick_boss_dialogue_line(boss.boss_id, slot, v1_fallback)
        rendered = self._render_boss_bark(line, tunnel)

        # Mark first-meet seen so subsequent encounters use outcome-aware lines.
        if not first_meet_seen:
            entry["first_meet_seen"] = True
            # Preserve boss_id when present (multi-boss tier lookup).
            entry.setdefault("boss_id", boss.boss_id)
            boss_progress[str(boundary)] = entry
            self.dig_repo.update_tunnel(
                discord_id, guild_id,
                boss_progress=json.dumps(boss_progress),
            )
            tunnel["boss_progress"] = json.dumps(boss_progress)

        # Mana: Blue reveals exact boss HP (info advantage).
        mana_reveal_hp = False
        if self.mana_effects_service is not None:
            try:
                _bf = self.mana_effects_service.get_effects(discord_id, guild_id)
                mana_reveal_hp = bool(_bf.boss_reveal_hp)
            except Exception:
                mana_reveal_hp = False

        return {
            "boundary": boundary,
            "boss_id": boss.boss_id,
            "name": boss.name,
            "dialogue": rendered,
            "ascii_art": boss.ascii_art or BOSS_ASCII.get(boundary, ""),
            "luminosity_display": self._luminosity_combat_display(tunnel),
            "mana_reveal_hp": mana_reveal_hp,
        }
