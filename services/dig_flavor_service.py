"""
Dig flavor service — additive LLM layer over deterministic dig mechanics.

The deterministic resolver in ``DigService`` is the single source of truth
for advance, JC, cave-ins, milestones, splash targets, and boss outcomes.
This service runs AFTER ``apply_dig_outcome`` has persisted the mechanical
result and writes prose, picks an NPC line, optionally nudges JC by a tiny
clamped amount, and rewrites the DM's narrative scratchpad.

On any LLM failure (timeout, error, validator rejection) the original result
is returned unchanged with no flavor fields, no JC delta, no memory write —
identical to the legacy canned-text path.
"""

from __future__ import annotations

import asyncio
import logging
import random

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.ai_service import AIService
from services.dig_dm_context import (
    DigDMContextBuilder,
    render_dm_context_section,
)
from services.dig_flavor_validator import (
    FlavorResult,
    validate_flavor_args,
    validate_splash_narrative,
)
from services.dig_llm_prompts import (
    NARRATE_DIG_TOOL,
    SPLASH_NARRATION_TOOL,
    TONE_PROFILES,
    _get_layer_name,
    build_boss_outcome_context,
    build_dig_flavor_system_prompt,
    build_dig_history_context,
    build_dig_outcome_context,
    build_messages,
    build_multiplayer_context,
    build_personality_context,
    build_player_state_context,
    build_splash_narration_messages,
)

logger = logging.getLogger("cama_bot.services.dig_flavor")


VALID_CHOICES = frozenset({
    "safe", "risky", "desperate", "fight", "retreat", "scout", "help",
})

# Tone roll: uniform random across the three voices.
TONE_KEYS: tuple[str, ...] = tuple(TONE_PROFILES.keys())

# JC bonus cap roll: 10% chance of ±10%, otherwise ±5%.
BONUS_CAP_BIG_PCT: float = 10.0
BONUS_CAP_SMALL_PCT: float = 5.0
BONUS_CAP_BIG_CHANCE: float = 0.10


def classify_play_style(histogram: dict) -> str:
    """Classify a player's play style from their choice histogram.

    Returns a descriptive label based on the dominant choice pattern.
    """
    total = sum(histogram.values())
    if total < 5:
        return "unknown"

    safe_pct = histogram.get("safe", 0) / total
    risky_pct = histogram.get("risky", 0) / total
    desperate_pct = histogram.get("desperate", 0) / total
    help_pct = histogram.get("help", 0) / total

    if help_pct > 0.3:
        return "social_butterfly"
    if safe_pct > 0.7:
        return "cautious_grinder"
    if desperate_pct > 0.15:
        return "reckless_degen"
    if risky_pct > 0.4:
        return "calculated_risk_taker"
    return "balanced_explorer"


def _roll_tone(rng: random.Random | None = None) -> str:
    chooser = rng if rng is not None else random
    return chooser.choice(TONE_KEYS)


def _roll_bonus_cap(rng: random.Random | None = None) -> float:
    chooser = rng if rng is not None else random
    return BONUS_CAP_BIG_PCT if chooser.random() < BONUS_CAP_BIG_CHANCE else BONUS_CAP_SMALL_PCT


class DigFlavorService:
    """Additive LLM flavor layer for the dig minigame."""

    def __init__(
        self,
        ai_service: AIService,
        dig_repo: DigRepository,
        player_repo: PlayerRepository,
        context_builder: DigDMContextBuilder,
    ) -> None:
        self.ai_service = ai_service
        self.dig_repo = dig_repo
        self.player_repo = player_repo
        self.context_builder = context_builder

    # ------------------------------------------------------------------
    # Internal: gather + build messages
    # ------------------------------------------------------------------

    def _resolve_names(
        self,
        social_actions: list[dict] | None,
        tunnel: dict | None,
        guild_id: int,
    ) -> dict[int, str]:
        """Resolve discord IDs from social actions and cheers to usernames."""
        ids: set[int] = set()
        for a in social_actions or []:
            for key in ("actor_id", "target_id"):
                pid = a.get(key)
                if pid:
                    ids.add(int(pid))
        if tunnel:
            import json as _json
            cheer_raw = tunnel.get("cheer_data")
            if cheer_raw and isinstance(cheer_raw, str):
                try:
                    for c in _json.loads(cheer_raw):
                        cid = c.get("cheerer_id")
                        if cid:
                            ids.add(int(cid))
                except (ValueError, TypeError):
                    pass
            elif isinstance(cheer_raw, list):
                for c in cheer_raw:
                    if isinstance(c, dict):
                        cid = c.get("cheerer_id")
                        if cid:
                            ids.add(int(cid))
        if not ids:
            return {}
        players = self.player_repo.get_by_ids(list(ids), guild_id)
        return {p.discord_id: p.name for p in players if p}

    async def _gather(
        self, discord_id: int, guild_id: int,
    ) -> tuple[
        dict | None, int, dict | None, list[dict] | None,
        list[dict] | None, int, dict[int, str], object,
    ]:
        """Single-shot fetch of everything the flavor call needs."""
        tunnel, balance, personality, social_actions, recent_actions, rank = (
            await asyncio.gather(
                asyncio.to_thread(self.dig_repo.get_tunnel, discord_id, guild_id),
                asyncio.to_thread(self.player_repo.get_balance, discord_id, guild_id),
                asyncio.to_thread(self.dig_repo.get_personality, discord_id, guild_id),
                asyncio.to_thread(
                    self.dig_repo.get_recent_social_actions, discord_id, guild_id,
                ),
                asyncio.to_thread(
                    self.dig_repo.get_recent_actions, discord_id, guild_id, 15,
                ),
                asyncio.to_thread(self.dig_repo.get_player_rank, discord_id, guild_id),
            )
        )
        names = await asyncio.to_thread(
            self._resolve_names, social_actions, tunnel, guild_id,
        )
        dm_context = await self.context_builder.build(discord_id, guild_id)
        return (
            tunnel, balance, personality, social_actions,
            recent_actions, rank, names, dm_context,
        )

    def _eligible_event_ids(self, result: dict) -> set[str]:
        """Extract eligible-event ids from the dig outcome.

        The deterministic resolver may already have picked one event (event
        in result) and/or surfaced ``available_events`` (a small candidate
        pool). The DM's pick_event must be one of those.
        """
        ids: set[str] = set()
        ev = result.get("event")
        if isinstance(ev, dict) and ev.get("id"):
            ids.add(str(ev["id"]))
        for entry in result.get("available_events", []) or []:
            if isinstance(entry, dict) and entry.get("id"):
                ids.add(str(entry["id"]))
        return ids

    # ------------------------------------------------------------------
    # Public: the unified flavor entry point
    # ------------------------------------------------------------------

    async def flavor(
        self,
        result: dict,
        discord_id: int,
        guild_id: int,
        *,
        is_boss: bool = False,
        rng: random.Random | None = None,
    ) -> dict:
        """Layer flavor on top of an already-applied dig outcome.

        Mutates and returns ``result``. Best-effort: any failure path leaves
        ``result`` mechanically untouched (no JC delta, no memory write, no
        added flavor keys).

        ``is_boss=True`` switches the outcome serialization to the boss
        outcome shape (shared narrate_dig tool).
        """
        try:
            (
                tunnel, balance, personality, social_actions,
                recent_actions, rank, names, dm_context,
            ) = await self._gather(discord_id, guild_id)

            tone_key = _roll_tone(rng)
            bonus_cap = _roll_bonus_cap(rng)
            jc_earned = int(result.get("jc_earned", 0) or 0)

            # Patch tunnel depth to post-dig value so player state shows the
            # new layer, not the old one.
            narr_tunnel = dict(tunnel) if tunnel else {}
            narr_tunnel["depth"] = result.get(
                "depth_after", narr_tunnel.get("depth", 0),
            )

            player_state = build_player_state_context(narr_tunnel, balance)
            personality_ctx = build_personality_context(personality)
            outcome_ctx = (
                build_boss_outcome_context(result)
                if is_boss
                else build_dig_outcome_context(result)
            )
            history_ctx = build_dig_history_context(
                recent_actions or [], narr_tunnel,
            )
            multiplayer_ctx = build_multiplayer_context(
                social_actions or [], narr_tunnel, rank, names=names,
            )

            eligible_ids = self._eligible_event_ids(result)
            eligible_str = ", ".join(sorted(eligible_ids)) if eligible_ids else ""

            messages = build_messages(
                build_dig_flavor_system_prompt(tone_key, bonus_cap),
                player_state,
                personality_ctx,
                outcome_ctx,
                multiplayer_ctx,
                history=history_ctx,
                dm_context=render_dm_context_section(dm_context),
                eligible_events=eligible_str,
            )

            llm_result = await asyncio.wait_for(
                self.ai_service.call_with_tools(
                    messages,
                    [NARRATE_DIG_TOOL],
                    tool_choice={
                        "type": "function",
                        "function": {"name": "narrate_dig"},
                    },
                    max_tokens=600,
                ),
                timeout=5.0,
            )

            if llm_result.tool_name != "narrate_dig":
                return result

            validated = validate_flavor_args(
                llm_result.tool_args,
                eligible_event_ids=eligible_ids,
                rolled_tone=tone_key,
                bonus_cap_pct=bonus_cap,
            )
            if validated is None:
                return result

            self._apply_flavor_to_result(
                result, validated, jc_earned, discord_id, guild_id,
            )
            return result

        except Exception:
            logger.debug("Dig flavor failed, returning raw result", exc_info=True)
            return result

    def _apply_flavor_to_result(
        self,
        result: dict,
        validated: FlavorResult,
        jc_earned: int,
        discord_id: int,
        guild_id: int,
    ) -> None:
        """Merge validated flavor output into the result dict + persist side effects."""
        result["llm_narrative"] = validated.narrative
        result["llm_tone"] = validated.tone
        if validated.callback_reference:
            result["llm_callback"] = validated.callback_reference
        if validated.npc_appearance:
            result["llm_npc"] = validated.npc_appearance
        if validated.picked_event_id:
            result["llm_picked_event_id"] = validated.picked_event_id

        # JC nudge: convert pct to int, skip when there's no base JC to nudge.
        if jc_earned and abs(validated.flavor_bonus_pct) > 0:
            delta = int(round(jc_earned * validated.flavor_bonus_pct / 100.0))
            if delta != 0:
                try:
                    self.player_repo.add_balance(discord_id, guild_id, delta)
                    result["llm_jc_delta"] = delta
                except Exception:
                    logger.debug("Flavor JC delta apply failed", exc_info=True)

        # Memory rewrite: last-write-wins.
        if validated.memory_update is not None:
            try:
                self.dig_repo.set_dm_memory(
                    discord_id, guild_id, validated.memory_update,
                )
            except Exception:
                logger.debug("DM memory write failed", exc_info=True)

    # ------------------------------------------------------------------
    # Public: boss fight final outcome (one LLM call per fight)
    # ------------------------------------------------------------------

    async def narrate_boss_fight(
        self,
        result: dict,
        discord_id: int,
        guild_id: int,
    ) -> str | None:
        """Generate flavor for a completed boss fight.

        Mutates ``result`` to add llm_* keys (same shape as ``flavor``) and
        returns the narrative string for callers that just want the line.
        """
        await self.flavor(result, discord_id, guild_id, is_boss=True)
        return result.get("llm_narrative") or None

    # ------------------------------------------------------------------
    # Public: splash narration (splasher's POV, splasher's memory only)
    # ------------------------------------------------------------------

    async def narrate_splash(
        self,
        *,
        digger_id: int,
        guild_id: int,
        event_name: str,
        event_description: str,
        splash_mode: str,
        victims: list[dict],
        digger_layer: str | None = None,
    ) -> str:
        """Produce a short prose blurb naming the splash victims.

        ``victims`` is a list of ``{"discord_id": int, "amount": int}`` dicts.
        Returns the empty string on any failure — callers fall back to the
        deterministic per-victim lines.
        """
        if not victims:
            return ""
        try:
            ids = [int(v["discord_id"]) for v in victims if "discord_id" in v]
            ids_with_digger = list({digger_id, *ids})
            players = await asyncio.to_thread(
                self.player_repo.get_by_ids, ids_with_digger, guild_id,
            )
            name_by_id = {p.discord_id: p.name for p in players if p}
            digger_name = name_by_id.get(digger_id, "the digger")

            if not digger_layer:
                tunnel = await asyncio.to_thread(
                    self.dig_repo.get_tunnel, digger_id, guild_id,
                )
                depth = (tunnel or {}).get("depth", 0)
                digger_layer = _get_layer_name(int(depth))

            victim_payload = [
                {
                    "name": name_by_id.get(int(v["discord_id"]), "a stranger"),
                    "amount": int(v.get("amount", 0)),
                }
                for v in victims if "discord_id" in v
            ]

            messages = build_splash_narration_messages(
                digger_name=digger_name,
                digger_layer=digger_layer,
                event_name=event_name,
                event_description=event_description,
                splash_mode=splash_mode,
                victims=victim_payload,
            )

            llm_result = await asyncio.wait_for(
                self.ai_service.call_with_tools(
                    messages,
                    [SPLASH_NARRATION_TOOL],
                    tool_choice={
                        "type": "function",
                        "function": {"name": "narrate_splash"},
                    },
                    max_tokens=200,
                ),
                timeout=3.0,
            )
            if llm_result.tool_name != "narrate_splash":
                return ""
            return validate_splash_narrative(
                str(llm_result.tool_args.get("narrative") or "")
            )
        except Exception:
            logger.debug("Splash narration failed", exc_info=True)
            return ""

    # ------------------------------------------------------------------
    # Personality writer (kept from the old service; same semantics)
    # ------------------------------------------------------------------

    def update_personality(
        self,
        discord_id: int,
        guild_id: int,
        action_type: str,
        choice: str | None = None,
        details: dict | None = None,
    ) -> None:
        """Update a player's personality profile based on their actions."""
        personality = self.dig_repo.get_personality(discord_id, guild_id)
        if personality is None:
            personality = {
                "play_style": "unknown",
                "choice_histogram": {},
                "notable_moments": [],
            }

        histogram = personality.get("choice_histogram", {})
        notable = personality.get("notable_moments", [])

        if choice and choice in VALID_CHOICES:
            histogram[choice] = histogram.get(choice, 0) + 1

        if details:
            moment = _extract_notable_moment(action_type, details)
            if moment:
                notable.append(moment)
                notable = notable[-10:]  # keep last 10

        personality["choice_histogram"] = histogram
        personality["notable_moments"] = notable
        personality["play_style"] = classify_play_style(histogram)

        self.dig_repo.upsert_personality(discord_id, guild_id, personality)


def _extract_notable_moment(action_type: str, details: dict) -> str | None:
    """Extract a notable moment string from action details, if any."""
    notable_keys = {
        "first_boss_kill": "Slew their first boss",
        "prestige": "Ascended to a new prestige level",
        "artifact_found": "Discovered a rare artifact",
        "cave_in_streak": "Suffered a devastating cave-in streak",
    }
    for key, description in notable_keys.items():
        if details.get(key):
            return description
    return None
