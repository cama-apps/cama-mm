"""
LLM game engine service for the dig minigame.

Enhances deterministic dig results with personalized AI narration
by calling the LLM via AIService and validating structured output.
"""

from __future__ import annotations

import asyncio
import logging
import random

from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.ai_service import AIService
from services.dig_llm_prompts import (
    DIG_ENGINE_SYSTEM_PROMPT,
    DIG_ENGINE_TOOL,
    DIG_OUTCOME_TOOL,
    DIG_SYSTEM_PROMPT,
    build_boss_outcome_context,
    build_dice_results_context,
    build_dig_history_context,
    build_dig_outcome_context,
    build_engine_messages,
    build_messages,
    build_multiplayer_context,
    build_personality_context,
    build_player_state_context,
    build_preconditions_context,
)

logger = logging.getLogger("cama_bot.services.dig_llm")

VALID_TONES = frozenset({
    "dramatic", "humorous", "ominous", "triumphant", "melancholy", "absurd",
})

VALID_CHOICES = frozenset({
    "safe", "risky", "desperate", "fight", "retreat", "scout", "help",
})


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


class DigLLMValidator:
    """Validates and sanitizes LLM tool-call outputs for dig narration."""

    def validate_narrative(self, tool_args: dict) -> dict:
        """Validate and clamp LLM narrative output fields.

        Ensures all fields are within length limits, tones are valid,
        and missing optional fields get safe defaults.
        """
        narrative = str(tool_args.get("narrative", ""))
        if len(narrative) > 500:
            narrative = narrative[:497] + "..."

        tone = str(tool_args.get("tone", "dramatic"))
        if tone not in VALID_TONES:
            tone = "dramatic"

        event_flavor = str(tool_args.get("event_flavor", ""))
        if len(event_flavor) > 300:
            event_flavor = event_flavor[:297] + "..."

        cave_in_flavor = str(tool_args.get("cave_in_flavor", ""))
        if len(cave_in_flavor) > 300:
            cave_in_flavor = cave_in_flavor[:297] + "..."

        callback_reference = str(tool_args.get("callback_reference", ""))
        if len(callback_reference) > 200:
            callback_reference = callback_reference[:197] + "..."

        return {
            "narrative": narrative,
            "tone": tone,
            "event_flavor": event_flavor,
            "cave_in_flavor": cave_in_flavor,
            "callback_reference": callback_reference,
        }

    def validate_engine_outcome(
        self, tool_args: dict, preconditions: dict,
    ) -> dict:
        """Validate and clamp a DM engine outcome against game rules.

        Ensures advance/JC stay within computed ranges, cave-in fields are
        consistent, event IDs exist in the available pool, and narrative
        fields are within length limits.
        """
        # Advance
        advance = int(tool_args.get("advance", 1))
        advance = max(
            preconditions["advance_min"],
            min(advance, preconditions["advance_max"]),
        )

        # JC
        jc_earned = int(tool_args.get("jc_earned", 0))
        jc_earned = max(
            preconditions["jc_min"],
            min(jc_earned, preconditions["jc_max"]),
        )

        # Cave-in
        cave_in = bool(tool_args.get("cave_in", False))
        # Hard hat prevents cave-in
        if preconditions.get("hard_hat_prevents"):
            cave_in = False

        cave_in_block_loss = 0
        cave_in_type = "none"
        cave_in_jc_lost = 0
        if cave_in:
            cave_in_block_loss = int(tool_args.get("cave_in_block_loss", 5))
            cave_in_block_loss = max(3, min(8, cave_in_block_loss))

            raw_type = str(tool_args.get("cave_in_type", "stun"))
            cave_in_type = (
                raw_type if raw_type in ("stun", "injury", "medical_bill") else "stun"
            )

            if cave_in_type == "medical_bill":
                cave_in_jc_lost = int(tool_args.get("cave_in_jc_lost", 3))
                cave_in_jc_lost = max(0, min(cave_in_jc_lost, 10))
            advance = 0
            jc_earned = 0

        # Event
        event_id = str(tool_args.get("event_id", ""))
        available_ids = {e["id"] for e in preconditions.get("available_events", [])}
        if event_id and event_id not in available_ids:
            event_id = ""

        event_description = str(tool_args.get("event_description", ""))
        if len(event_description) > 500:
            event_description = event_description[:497] + "..."

        return {
            "advance": advance,
            "jc_earned": jc_earned,
            "cave_in": cave_in,
            "cave_in_block_loss": cave_in_block_loss,
            "cave_in_type": cave_in_type,
            "cave_in_jc_lost": cave_in_jc_lost,
            "event_id": event_id,
            "event_description": event_description,
        }


def _default_dice_rolls(preconditions: dict) -> list[dict]:
    """Fallback dice request when the model does not provide a usable one."""
    advance_span = (
        int(preconditions.get("advance_max", 1))
        - int(preconditions.get("advance_min", 1))
        + 1
    )
    jc_span = (
        int(preconditions.get("jc_max", 0))
        - int(preconditions.get("jc_min", 0))
        + 1
    )
    return [
        {"label": "cave_in", "sides": 100, "count": 1, "modifier": 0},
        {"label": "event", "sides": 100, "count": 1, "modifier": 0},
        {"label": "advance", "sides": max(2, min(advance_span, 100)), "count": 1, "modifier": 0},
        {"label": "jc", "sides": max(2, min(jc_span, 100)), "count": 1, "modifier": 0},
        {"label": "cave_in_loss", "sides": 6, "count": 1, "modifier": 2},
        {"label": "consequence", "sides": 3, "count": 1, "modifier": 0},
    ]


def _roll_dice(rolls: list[dict]) -> list[dict]:
    """Execute validated dice requests using the app's RNG."""
    results = []
    for roll in rolls:
        sides = roll["sides"]
        count = roll["count"]
        modifier = roll["modifier"]
        values = [random.randint(1, sides) for _ in range(count)]
        results.append({
            "label": roll["label"],
            "sides": sides,
            "count": count,
            "modifier": modifier,
            "rolls": values,
            "total": sum(values) + modifier,
        })
    return results


class DigLLMService:
    """Enhances deterministic dig results with personalized LLM narration.

    This service sits between the deterministic DigService and the embed
    builder. It takes a completed dig result dict, gathers player context
    (tunnel state, personality, social history), and asks the LLM to
    generate a short narrative that reflects the outcome.

    On any failure (timeout, API error, validation), the original result
    dict is returned unchanged -- LLM enhancement is always optional.
    """

    def __init__(
        self,
        ai_service: AIService,
        dig_repo: DigRepository,
        player_repo: PlayerRepository,
        dig_service=None,
    ):
        self.ai_service = ai_service
        self.dig_repo = dig_repo
        self.player_repo = player_repo
        self.dig_service = dig_service
        self.validator = DigLLMValidator()

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

    async def enhance(
        self,
        result: dict,
        discord_id: int,
        guild_id: int,
    ) -> dict:
        """Enhance a deterministic dig result with LLM-generated narration.

        Gathers player context, calls the LLM with a structured tool,
        validates the output, and merges narrative fields into the result.

        On ANY exception the original result is returned unchanged.
        """
        try:
            # Gather context from repos (synchronous, so use to_thread)
            tunnel, balance, personality, social_actions, recent_actions, rank = (
                await asyncio.gather(
                    asyncio.to_thread(self.dig_repo.get_tunnel, discord_id, guild_id),
                    asyncio.to_thread(self.player_repo.get_balance, discord_id, guild_id),
                    asyncio.to_thread(self.dig_repo.get_personality, discord_id, guild_id),
                    asyncio.to_thread(
                        self.dig_repo.get_recent_social_actions,
                        discord_id,
                        guild_id,
                    ),
                    asyncio.to_thread(
                        self.dig_repo.get_recent_actions, discord_id, guild_id, 15,
                    ),
                    asyncio.to_thread(self.dig_repo.get_player_rank, discord_id, guild_id),
                )
            )

            # Resolve player names for social context
            names = await asyncio.to_thread(
                self._resolve_names, social_actions, tunnel, guild_id,
            )

            # Build prompt context
            player_state = build_player_state_context(tunnel or {}, balance)
            personality_ctx = build_personality_context(personality)
            outcome_ctx = build_dig_outcome_context(result)
            history_ctx = build_dig_history_context(recent_actions or [], tunnel or {})
            multiplayer_ctx = build_multiplayer_context(
                social_actions or [], tunnel, rank, names=names,
            )

            messages = build_messages(
                DIG_SYSTEM_PROMPT,
                player_state,
                personality_ctx,
                outcome_ctx,
                multiplayer_ctx,
                history=history_ctx,
            )

            # Call LLM with a hard timeout
            llm_result = await asyncio.wait_for(
                self.ai_service.call_with_tools(
                    messages,
                    [DIG_OUTCOME_TOOL],
                    tool_choice={
                        "type": "function",
                        "function": {"name": "narrate_dig_outcome"},
                    },
                    max_tokens=800,
                ),
                timeout=2.5,
            )

            if llm_result.tool_name != "narrate_dig_outcome":
                logger.warning(
                    "LLM returned unexpected tool %s, skipping enhancement",
                    llm_result.tool_name,
                )
                return result

            validated = self.validator.validate_narrative(llm_result.tool_args)

            result["llm_narrative"] = validated["narrative"]
            result["llm_tone"] = validated["tone"]
            if validated["event_flavor"]:
                result["llm_event_flavor"] = validated["event_flavor"]
            if validated["cave_in_flavor"]:
                result["llm_cave_in_flavor"] = validated["cave_in_flavor"]
            # Only set callback if it's an actual sentence, not a raw ID
            cb = validated["callback_reference"]
            if cb and " " in cb:
                result["llm_callback"] = cb

            return result

        except Exception:
            logger.warning("LLM enhancement failed, returning raw result", exc_info=True)
            return result

    async def _narrate_outcome(
        self,
        result: dict,
        tunnel: dict | None,
        balance: int,
        personality: dict | None,
        social_actions: list[dict] | None,
        recent_actions: list[dict] | None = None,
        rank: int = 0,
        names: dict[int, str] | None = None,
    ) -> dict:
        """Narrate a completed dig result using the actual outcome.

        Called AFTER apply_dig_outcome so the narrative sees milestones,
        layer transitions, boss encounters, artifacts — everything that
        actually happened.  Best-effort: on any failure the result is
        returned unchanged (no narrative, game still works).
        """
        try:
            # Patch tunnel depth to post-dig value so player state shows
            # the new layer, not the old one.
            narr_tunnel = dict(tunnel) if tunnel else {}
            narr_tunnel["depth"] = result.get("depth_after", narr_tunnel.get("depth", 0))

            player_state = build_player_state_context(narr_tunnel, balance)
            personality_ctx = build_personality_context(personality)
            outcome_ctx = build_dig_outcome_context(result)
            history_ctx = build_dig_history_context(recent_actions or [], narr_tunnel)
            multiplayer_ctx = build_multiplayer_context(
                social_actions or [], narr_tunnel, rank, names=names,
            )

            messages = build_messages(
                DIG_SYSTEM_PROMPT,
                player_state,
                personality_ctx,
                outcome_ctx,
                multiplayer_ctx,
                history=history_ctx,
            )

            llm_result = await asyncio.wait_for(
                self.ai_service.call_with_tools(
                    messages,
                    [DIG_OUTCOME_TOOL],
                    tool_choice={
                        "type": "function",
                        "function": {"name": "narrate_dig_outcome"},
                    },
                    max_tokens=800,
                ),
                timeout=5.0,
            )

            if llm_result.tool_name != "narrate_dig_outcome":
                return result

            validated = self.validator.validate_narrative(llm_result.tool_args)

            result["llm_narrative"] = validated["narrative"]
            result["llm_tone"] = validated["tone"]
            if validated["event_flavor"]:
                result["llm_event_flavor"] = validated["event_flavor"]
            if validated["cave_in_flavor"]:
                result["llm_cave_in_flavor"] = validated["cave_in_flavor"]
            if validated["callback_reference"]:
                result["llm_callback"] = validated["callback_reference"]

            return result

        except Exception:
            logger.debug("Post-outcome narration failed, returning raw result", exc_info=True)
            return result

    async def narrate_boss_fight(
        self,
        result: dict,
        discord_id: int,
        guild_id: int,
    ) -> str | None:
        """Generate a narrative for a completed boss fight. Returns the
        narrative string, or None on failure."""
        try:
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
                    asyncio.to_thread(
                        self.dig_repo.get_player_rank, discord_id, guild_id,
                    ),
                )
            )

            names = await asyncio.to_thread(
                self._resolve_names, social_actions, tunnel, guild_id,
            )

            player_state = build_player_state_context(tunnel or {}, balance)
            personality_ctx = build_personality_context(personality)
            boss_ctx = build_boss_outcome_context(result)
            history_ctx = build_dig_history_context(recent_actions or [], tunnel or {})
            multiplayer_ctx = build_multiplayer_context(
                social_actions or [], tunnel, rank, names=names,
            )

            messages = build_messages(
                DIG_SYSTEM_PROMPT,
                player_state,
                personality_ctx,
                boss_ctx,
                multiplayer_ctx,
                history=history_ctx,
            )

            llm_result = await asyncio.wait_for(
                self.ai_service.call_with_tools(
                    messages,
                    [DIG_OUTCOME_TOOL],
                    tool_choice={
                        "type": "function",
                        "function": {"name": "narrate_dig_outcome"},
                    },
                    max_tokens=800,
                ),
                timeout=5.0,
            )

            if llm_result.tool_name != "narrate_dig_outcome":
                return None

            validated = self.validator.validate_narrative(llm_result.tool_args)
            return validated["narrative"] or None

        except Exception:
            logger.debug("Boss fight narration failed", exc_info=True)
            return None

    async def run_dig(
        self,
        discord_id: int,
        guild_id: int,
        preconditions: dict,
    ) -> dict:
        """DM-powered dig: LLM determines the outcome.

        1. Build context from preconditions + personality
        2. Roll dice locally and include results in context
        3. Call the LLM with DIG_ENGINE_TOOL for the outcome
        4. Validate/clamp the response
        5. Call dig_service.apply_dig_outcome() to write to DB
        6. Narrate the result
        7. Return result dict for embed builder

        Falls back to deterministic on any failure.
        """
        try:
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
                    asyncio.to_thread(
                        self.dig_repo.get_player_rank, discord_id, guild_id,
                    ),
                )
            )

            names = await asyncio.to_thread(
                self._resolve_names, social_actions, tunnel, guild_id,
            )

            player_state = build_player_state_context(tunnel or {}, balance)
            personality_ctx = build_personality_context(personality)
            preconditions_ctx = build_preconditions_context(preconditions)
            history_ctx = build_dig_history_context(recent_actions or [], tunnel or {})
            multiplayer_ctx = build_multiplayer_context(
                social_actions or [], tunnel, rank, names=names,
            )

            # Roll dice locally — no LLM call needed for randomness
            dice_rolls = _default_dice_rolls(preconditions)
            dice_ctx = build_dice_results_context(_roll_dice(dice_rolls))

            messages = build_engine_messages(
                DIG_ENGINE_SYSTEM_PROMPT,
                player_state,
                personality_ctx,
                preconditions_ctx,
                multiplayer_ctx,
                dice_results=dice_ctx,
                history=history_ctx,
            )

            llm_result = await asyncio.wait_for(
                self.ai_service.call_with_tools(
                    messages,
                    [DIG_ENGINE_TOOL],
                    tool_choice={
                        "type": "function",
                        "function": {"name": "resolve_dig"},
                    },
                    max_tokens=1200,
                ),
                timeout=10.0,
            )

            if llm_result.tool_name != "resolve_dig":
                logger.warning(
                    "LLM returned unexpected tool %s, falling back to deterministic",
                    llm_result.tool_name,
                )
                return await asyncio.to_thread(
                    self.dig_service._execute_deterministic_outcome, preconditions,
                )

            outcome = self.validator.validate_engine_outcome(
                llm_result.tool_args, preconditions,
            )

            result = await asyncio.to_thread(
                self.dig_service.apply_dig_outcome, preconditions, outcome,
            )

            # Narrate the ACTUAL outcome — milestones, layer transitions,
            # boss encounters, artifacts are all known now.
            result = await self._narrate_outcome(
                result, tunnel, balance, personality, social_actions,
                recent_actions=recent_actions, rank=rank, names=names,
            )
            return result

        except Exception:
            logger.warning("DM dig failed, falling back to deterministic", exc_info=True)
            return await asyncio.to_thread(
                self.dig_service._execute_deterministic_outcome, preconditions,
            )

    def update_personality(
        self,
        discord_id: int,
        guild_id: int,
        action_type: str,
        choice: str | None = None,
        details: dict | None = None,
    ) -> None:
        """Update a player's personality profile based on their actions.

        Synchronous -- intended to be called via asyncio.to_thread from
        the command layer.

        Tracks choice histogram, notable moments, and reclassifies
        play style when the histogram changes.
        """
        personality = self.dig_repo.get_personality(discord_id, guild_id)
        if personality is None:
            personality = {
                "play_style": "unknown",
                "choice_histogram": {},
                "notable_moments": [],
            }

        histogram = personality.get("choice_histogram", {})
        notable = personality.get("notable_moments", [])

        # Update choice histogram
        if choice and choice in VALID_CHOICES:
            histogram[choice] = histogram.get(choice, 0) + 1

        # Check for notable moments
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
