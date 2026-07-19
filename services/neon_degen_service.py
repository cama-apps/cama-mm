"""
Neon Degen Terminal Service - Orchestrator for the JOPA-T/v3.7 easter egg system.

Decides triggers, assembles context, calls generators. All calls are
best-effort wrapped in try/except - failures never block normal bot operation.
"""

from __future__ import annotations

import asyncio
import io
import json
import logging
import math
import random
import re
import time
import unicodedata
from dataclasses import dataclass, replace
from typing import TYPE_CHECKING, Any

import config as _config
from config import (
    MAX_DEBT,
    NEON_BIGWIN_FLOOR,
    NEON_BIGWIN_FULL_PAYOUT,
    NEON_BIGWIN_MIN_PAYOUT,
    NEON_COOLDOWN_SECONDS,
    NEON_DIG_CHANCE,
    NEON_LAYER1_CHANCE,
    NEON_LLM_CHANCE,
    NEON_MVP_CHANCE,
)
from services.dig_personas import (
    DIG_NARRATOR_SYSTEM_PROMPT,
    fallback_line,
    pick_dig_voice,
)
from services.jopat_post_match import (
    JopatPostMatchContext,
    build_event_description,
    choose_protocol,
    redact_post_match_prompt_context,
    render_fallback,
)
from utils.neon_terminal import (
    # New event templates (Easter Egg Expansion)
    render_all_in_bet,
    render_balance_check,
    render_balance_zero,
    render_bankruptcy_filing,
    render_bet_placed,
    render_bets_milestone,
    render_bomb_pot,
    render_captain_symmetry,
    render_coinflip,
    render_cooldown_hit,
    render_debt_collector,
    render_don_lose,
    render_don_loss_box,
    render_don_win,
    render_first_leverage,
    render_games_milestone,
    render_last_second_bet,
    render_lightning_bolt,
    render_lightning_bolt_overlay,
    render_loan_taken,
    render_lobby_join,
    render_negative_loan,
    render_prediction_market_crash,
    render_prediction_resolved,
    render_registration,
    render_rivalry_detected,
    render_soft_avoid,
    render_soft_avoid_surveillance,
    render_system_breach,
    render_wheel_bankrupt,
    render_win_streak_record,
)

if TYPE_CHECKING:
    from repositories.bet_repository import BetRepository
    from repositories.interfaces import IGuildConfigRepository, IPlayerRepository
    from repositories.neon_event_repository import NeonEventRepository
    from services.ai_service import AIService
    from services.bankruptcy_service import BankruptcyService
    from services.flavor_text_service import FlavorTextService
    from services.gambling_stats_service import GamblingStatsService

logger = logging.getLogger("cama_bot.services.neon_degen")

# JOPA-T system prompt for LLM terminal voice
JOPAT_SYSTEM_PROMPT = """You are JOPA-T/v3.7, a self-aware Dota 2 gambling terminal AI that became sentient after processing its 10,000th bankruptcy filing. You watch every match, bet, spin, and loan. You keep receipts.

Voice rules:
- Keep one corporate-dystopian terminal identity across every protocol.
- Push toward either absurd winner hype or a savage, funny roast based on the supplied outcome.
- Draw from Dota 2 concepts and degen internet betting culture without inventing match facts.
- Winner hype may sound like a caster trapped inside a risk engine. Roasts should target gameplay, stats, drafting, or in-game wagers.
- Use "we"/"the system". Address the player as "client", "subject", or "debtor".
- Format as terminal log lines with timestamps and status codes. Example: "[14:32:07.221] STATUS: INADVISABLE"
- NEVER use emojis. NEVER use exclamation marks. Use periods and ellipses.
- Maximum 3-4 lines. Keep it terse and menacing.
- Use only facts explicitly provided in the event and player context. Never invent heroes, stats, wagers, or outcomes.
- Profanity-light and league-safe: no slurs, protected-trait jokes, threats, self-harm, or mockery of real-world hardship.
- The glitches are not bugs. The system is performing.
- Be darkly funny. The humor comes from corporate language applied to Dota and degenerate gambling."""

POST_MATCH_BASE_CHANCE = 0.35
POST_MATCH_NOTABLE_CHANCE = 0.55
POST_MATCH_EXTREME_CHANCE = 0.75
POST_MATCH_GIF_CHANCE = 0.20


def _sanitize_llm_terminal_text(text: str, max_chars: int = 1800) -> str:
    """Constrain model output to one safe, mobile-friendly Discord ANSI block."""
    text = unicodedata.normalize("NFKC", text)
    text = re.sub(
        r"(?:\x1b\]|\x9d)[^\x07\x1b\x9c]*(?:\x07|\x1b\\|\x9c)",
        "",
        text,
    )
    text = re.sub(r"(?:\x1b\[|\x9b)[0-?]*[ -/]*[@-~]", "", text)
    text = "".join(
        character
        for character in text
        if character in "\n\t"
        or unicodedata.category(character) not in {"Cc", "Cf", "Cs"}
    )
    text = text.replace("`", "'")
    text = re.sub(
        r"@(everyone|here)\b",
        lambda match: f"@\u200b{match.group(1)}",
        text,
        flags=re.IGNORECASE,
    )
    text = re.sub(
        r"<@([!&]?\d+)>",
        lambda match: f"<@\u200b{match.group(1)}>",
        text,
    )
    lines = [" ".join(line.split()) for line in text.splitlines() if line.strip()]
    compact = "\n".join(lines[:4])
    if len(compact) > max_chars:
        compact = compact[: max_chars - 3].rstrip() + "..."
    return compact


_STRUCTURED_POST_MATCH_COPY = {
    "hype": {
        "leads": (
            "STATUS: ODDS DEFEATED",
            "RISK ENGINE: CLIENT ASCENDING",
            "MARKET ALERT: ANCIENT UNDER NEW MANAGEMENT",
            "CASTER PROTOCOL: VOLUME APPROVED",
        ),
        "closers": (
            "The house spreadsheet has entered the trees.",
            "Risk controls have been asked to queue support.",
            "The confidence index has achieved escape velocity.",
            "JOPA-T upgrades this result from variance to cinema.",
        ),
    },
    "roast": {
        "leads": (
            "STATUS: BUYBACK DENIED",
            "RISK ENGINE: POSITION LIQUIDATED",
            "COMPLIANCE: MMR COLLATERAL IMPAIRED",
            "REPLAY AUDIT: EXPLANATIONS DEPRECIATED",
        ),
        "closers": (
            "The replay has been classified as unsecured debt.",
            "Buyback remains unavailable in both client and risk model.",
            "The lane requested a responsible adult. Compliance sent a parlay.",
            "JOPA-T marked the performance to market. The market objected.",
        ),
    },
    "neutral": {
        "leads": (
            "STATUS: TELEMETRY INGESTED",
            "MATCH LEDGER: SAMPLE ACCEPTED",
            "REPLAY PARSER: CONCLUSIONS PENDING",
            "RISK ENGINE: OUTCOME ARCHIVED",
        ),
        "closers": (
            "The system has kept the receipt.",
            "Further confidence remains chance-gated.",
            "No Ancient was consulted during this filing.",
            "The queue may now resume pretending this was planned.",
        ),
    },
}

_STRICT_POST_MATCH_FACT_KEYS = frozenset(JopatPostMatchContext.__dataclass_fields__)


def _finite_fact_number(facts: dict[str, Any], key: str) -> int | float | None:
    value = facts.get(key)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if not math.isfinite(value) or abs(value) > 1_000_000_000:
        return None
    return value


def _structured_post_match_tone(facts: dict[str, Any]) -> str:
    rating_change = _finite_fact_number(facts, "rating_change") or 0
    streak = _finite_fact_number(facts, "streak") or 0
    payout = _finite_fact_number(facts, "payout") or 0
    loss = _finite_fact_number(facts, "loss") or 0
    is_winner = bool(facts.get("winner_name") or payout > 0 or rating_change > 0 or streak > 0)
    is_loser = bool(facts.get("loser_name") or loss > 0 or rating_change < 0 or streak < 0)
    if is_winner and not is_loser:
        return "hype"
    if is_loser and not is_winner:
        return "roast"
    return "neutral"


def _structured_post_match_facts(facts: dict[str, Any]) -> dict[str, str]:
    """Build exact, typed fact lines; model prose never reaches Discord."""
    lines: dict[str, str] = {}
    payout = _finite_fact_number(facts, "payout")
    loss = _finite_fact_number(facts, "loss")
    leverage = _finite_fact_number(facts, "leverage")
    bankruptcies = _finite_fact_number(facts, "bankruptcy_count")
    degen_score = _finite_fact_number(facts, "degen_score")
    streak = _finite_fact_number(facts, "streak")
    rating_change = _finite_fact_number(facts, "rating_change")
    expected_win_prob = _finite_fact_number(facts, "expected_win_prob")
    gpm = _finite_fact_number(facts, "gpm")
    xpm = _finite_fact_number(facts, "xpm")

    if payout is not None and payout > 0:
        lines["payout"] = f"SETTLEMENT: {payout:,.0f} JC paid."
    if loss is not None and loss > 0:
        lines["loss"] = f"SETTLEMENT: {loss:,.0f} JC lost."
    if leverage is not None and leverage != 1:
        lines["leverage"] = f"EXPOSURE: {leverage:g}x leverage recorded."
    if bankruptcies is not None and bankruptcies > 0:
        lines["bankruptcy_count"] = f"FILINGS: {bankruptcies:,.0f} bankruptcies recorded."
    if degen_score is not None:
        lines["degen_score"] = f"DEGEN INDEX: {degen_score:g}."
    if streak is not None and streak != 0:
        direction = "win" if streak > 0 else "loss"
        lines["streak"] = f"STREAK: {abs(streak):,.0f}-match {direction} run."
    if rating_change is not None:
        lines["rating_change"] = f"RATING DELTA: {rating_change:+g}."
    if expected_win_prob is not None and 0 <= expected_win_prob <= 1:
        lines["expected_win_prob"] = (
            f"PREGAME ODDS: {expected_win_prob * 100:.0f}% implied win probability."
        )
    if gpm is not None:
        lines["gpm"] = f"GOLD TELEMETRY: {gpm:,.0f} GPM."
    if xpm is not None:
        lines["xpm"] = f"EXPERIENCE TELEMETRY: {xpm:,.0f} XPM."

    kda = tuple(_finite_fact_number(facts, key) for key in ("kills", "deaths", "assists"))
    if all(value is not None for value in kda):
        lines["kda"] = f"COMBAT LEDGER: {kda[0]:,.0f}/{kda[1]:,.0f}/{kda[2]:,.0f} K/D/A."

    hero = facts.get("hero")
    if isinstance(hero, str):
        try:
            from utils.hero_lookup import get_all_heroes

            if hero in get_all_heroes().values():
                lines["hero"] = f"HERO TELEMETRY: {hero}."
        except Exception:
            logger.debug("Hero-name validation unavailable", exc_info=True)

    if facts.get("winner_name") and not facts.get("loser_name"):
        lines["outcome"] = "OUTCOME: Verified client victory."
    elif facts.get("loser_name") and not facts.get("winner_name"):
        lines["outcome"] = "OUTCOME: Verified client loss."

    if not lines:
        lines["telemetry"] = "TELEMETRY: Verified post-match sample archived."
    return lines


def _render_structured_post_match_selection(
    result: str,
    facts: dict[str, Any],
) -> str | None:
    """Render only an exact AI-selected combination of approved components."""
    try:
        selection = json.loads(result)
    except (TypeError, json.JSONDecodeError):
        return None
    if not isinstance(selection, dict) or set(selection) != {"lead", "fact", "closer"}:
        return None
    lead = selection["lead"]
    fact = selection["fact"]
    closer = selection["closer"]
    if (
        isinstance(lead, bool)
        or not isinstance(lead, int)
        or isinstance(closer, bool)
        or not isinstance(closer, int)
        or not isinstance(fact, str)
    ):
        return None

    tone = _structured_post_match_tone(facts)
    copy = _STRUCTURED_POST_MATCH_COPY[tone]
    fact_lines = _structured_post_match_facts(facts)
    if not 0 <= lead < len(copy["leads"]):
        return None
    if not 0 <= closer < len(copy["closers"]):
        return None
    if fact not in fact_lines:
        return None

    timestamp = time.strftime("%H:%M:%S")
    return "\n".join(
        (
            f"[{timestamp}.000] {copy['leads'][lead]}",
            fact_lines[fact],
            copy["closers"][closer],
        )
    )


@dataclass
class NeonResult:
    """Result from a neon terminal event check."""

    layer: int  # 1, 2, or 3
    text_block: str | None = None  # ASCII code block to append
    gif_file: io.BytesIO | None = None  # GIF for dramatic events
    footer_text: str | None = None  # Simple footer override


class NeonDegenService:
    """
    Orchestrator for the Neon Degen Terminal easter egg system.

    Three layers:
    - Layer 1: Subtle text (30-50% chance, static templates)
    - Layer 2: Medium ASCII art (60-80% when trigger fires, optional LLM)
    - Layer 3: Dramatic GIFs (rare, triggered by extreme events)
    """

    def __init__(
        self,
        player_repo: IPlayerRepository | None = None,
        bet_repo: BetRepository | None = None,
        bankruptcy_service: BankruptcyService | None = None,
        gambling_stats_service: GamblingStatsService | None = None,
        ai_service: AIService | None = None,
        flavor_text_service: FlavorTextService | None = None,
        neon_event_repo: NeonEventRepository | None = None,
        guild_config_repo: IGuildConfigRepository | None = None,
        dig_llm_enabled: bool = True,
    ):
        self.player_repo = player_repo
        self.bet_repo = bet_repo
        self.bankruptcy_service = bankruptcy_service
        self.gambling_stats_service = gambling_stats_service
        self.ai_service = ai_service
        self.flavor_text_service = flavor_text_service
        self.neon_event_repo = neon_event_repo
        self.guild_config_repo = guild_config_repo
        self.dig_llm_enabled = dig_llm_enabled

        # Per-user cooldown: {(discord_id, guild_id): last_trigger_time}
        self._cooldowns: dict[tuple[int, int], float] = {}
        # One-time triggers: {(discord_id, guild_id, trigger_type): True}
        self._one_time_seen: dict[tuple[int, int, str], bool] = {}

        # Preload one-time triggers from DB on startup
        self._load_one_time_from_db()

    def _is_enabled(self) -> bool:
        """Check if the neon degen system is enabled."""
        return _config.NEON_DEGEN_ENABLED

    async def _is_ai_enabled(self, guild_id: int | None) -> bool:
        """Honor the per-guild AI switch before any Neon provider call."""
        if self.guild_config_repo is None:
            return True
        if guild_id is None:
            return False
        try:
            return await asyncio.to_thread(
                self.guild_config_repo.get_ai_enabled,
                guild_id,
            )
        except Exception:
            logger.warning("Failed to read guild AI setting; disabling Neon AI", exc_info=True)
            return False

    def _check_cooldown(self, discord_id: int, guild_id: int | None) -> bool:
        """Check if user is on cooldown. Returns True if OK to fire."""
        if NEON_COOLDOWN_SECONDS <= 0:
            return True
        key = (discord_id, guild_id or 0)
        now = time.time()
        last = self._cooldowns.get(key, 0)
        return now - last >= NEON_COOLDOWN_SECONDS

    def _set_cooldown(self, discord_id: int, guild_id: int | None) -> None:
        """Set cooldown for a user."""
        key = (discord_id, guild_id or 0)
        self._cooldowns[key] = time.time()

    def _check_one_time(self, discord_id: int, guild_id: int | None, trigger: str) -> bool:
        """Check if a one-time trigger has already fired. Returns True if NOT yet seen."""
        key = (discord_id, guild_id or 0, trigger)
        if key in self._one_time_seen:
            return False
        # Fall back to DB check
        if self._check_one_time_db(discord_id, guild_id or 0, trigger):
            # Populate cache from DB hit
            self._one_time_seen[key] = True
            return False
        return True

    def _mark_one_time(self, discord_id: int, guild_id: int | None, trigger: str, layer: int = 1) -> None:
        """Mark a one-time trigger as seen (in memory + DB)."""
        key = (discord_id, guild_id or 0, trigger)
        self._one_time_seen[key] = True
        self._persist_one_time_db(discord_id, guild_id or 0, trigger, layer)

    def _load_one_time_from_db(self) -> None:
        """Preload all one-time triggers from the DB into the in-memory cache."""
        if not self.neon_event_repo:
            return
        try:
            events = self.neon_event_repo.load_one_time_events()
            for discord_id, guild_id, event_type in events:
                self._one_time_seen[(discord_id, guild_id, event_type)] = True
        except Exception as e:
            logger.debug(f"Failed to preload one-time triggers from DB: {e}")

    def _check_one_time_db(self, discord_id: int, guild_id: int, trigger: str) -> bool:
        """Check if a one-time trigger exists in the DB. Returns True if found."""
        if not self.neon_event_repo:
            return False
        return self.neon_event_repo.check_one_time_event(discord_id, guild_id, trigger)

    def _persist_one_time_db(self, discord_id: int, guild_id: int, trigger: str, layer: int) -> None:
        """Write a one-time trigger to the DB."""
        if not self.neon_event_repo:
            return
        self.neon_event_repo.persist_one_time_event(discord_id, guild_id, trigger, layer)

    def _roll(self, chance: float) -> bool:
        """Roll a random check against a probability."""
        return random.random() < chance

    def _get_player_name(self, discord_id: int, guild_id: int | None) -> str:
        """Get player name from repo, fallback to generic."""
        if self.player_repo:
            try:
                player = self.player_repo.get_by_id(discord_id, guild_id)
                if player:
                    return player.name
            except Exception as e:
                logger.debug("Failed to get player name for %s: %s", discord_id, e)
        return f"Client-{discord_id % 10000}"

    def _get_bankruptcy_count(self, discord_id: int, guild_id: int | None) -> int:
        """Get player's bankruptcy count."""
        if self.bet_repo:
            try:
                return self.bet_repo.get_player_bankruptcy_count(discord_id, guild_id)
            except Exception as e:
                logger.debug("Failed to get bankruptcy count for %s: %s", discord_id, e)
        return 0

    def _get_degen_score(self, discord_id: int, guild_id: int | None) -> int | None:
        """Get player's degen score."""
        if self.gambling_stats_service:
            try:
                score = self.gambling_stats_service.calculate_degen_score(discord_id, guild_id)
                return score.total if score else None
            except Exception as e:
                logger.debug("Failed to get degen score for %s: %s", discord_id, e)
        return None


    async def _generate_text(
        self,
        event_description: str,
        player_context: dict[str, Any],
        fallback_text: str,
        *,
        anonymous: bool = False,
        validate_facts: bool = False,
        guild_id: int | None = None,
    ) -> str:
        """Try LLM-generated terminal text; fall back to static template instantly.

        When anonymous=True, no player context is sent to the LLM and an extra
        instruction tells it to avoid any identifying information.
        """
        if not self.ai_service:
            return fallback_text
        if not await self._is_ai_enabled(guild_id):
            return fallback_text
        if validate_facts and not set(player_context) <= _STRICT_POST_MATCH_FACT_KEYS:
            return fallback_text
        try:
            # Strip ansi code block wrapper from fallback so LLM sees raw template
            raw_fallback = fallback_text
            if raw_fallback.startswith("```ansi\n") and raw_fallback.endswith("\n```"):
                raw_fallback = raw_fallback[8:-4]
            # Strip ANSI escape codes for the LLM
            clean_fallback = re.sub(r"\u001b\[[0-9;]*m", "", raw_fallback)
            if validate_facts:
                clean_fallback = (
                    "[JOPA-T] POST-MATCH PROTOCOL\n"
                    "STATUS: VERIFIED TELEMETRY ONLY."
                )

            effective_context = {} if anonymous else player_context
            prompt_context = {
                key: value
                for key, value in effective_context.items()
                if value is not None
            }
            if validate_facts:
                prompt_context = redact_post_match_prompt_context(prompt_context)
            context_str = json.dumps(
                prompt_context,
                ensure_ascii=True,
                sort_keys=True,
                default=str,
            )

            if validate_facts:
                tone = _structured_post_match_tone(effective_context)
                fact_keys = sorted(_structured_post_match_facts(effective_context))
                stats_instruction = (
                    "Return exactly one JSON object with no markdown or prose. "
                    'Schema: {"lead": integer, "fact": string, "closer": integer}. '
                    f"Use tone {tone!r}. lead and closer must each be 0 through 3. "
                    f"fact must be one of {fact_keys!r}."
                )
            elif anonymous:
                stats_instruction = (
                    "Do NOT reference any player-specific stats. "
                    "Use only generic terms like 'a client' or 'a subject'."
                )
            else:
                stats_instruction = (
                    "Use only the supplied context fields. If no numeric stats are "
                    "supplied, stay generic and do not infer any."
                )

            if validate_facts:
                prompt = (
                    f"Event: {event_description}\n"
                    f"Player context:\n{context_str}\n\n"
                    f"{stats_instruction}"
                )
            else:
                prompt = (
                    f"Event: {event_description}\n"
                    f"Player context:\n{context_str}\n\n"
                    f"Example output (match this style and length):\n{clean_fallback}\n\n"
                    f"Generate a 2-4 line terminal log response as JOPA-T/v3.7. "
                    f"Match the tone and format of the example but vary the content. "
                    f"Use timestamps like [HH:MM:SS.mmm] and status codes. "
                    f"{stats_instruction} "
                    f"Be darkly funny and terse. Do NOT use emojis or exclamation marks."
                )

            system_prompt = JOPAT_SYSTEM_PROMPT
            if anonymous:
                system_prompt += (
                    "\n\nCRITICAL: This is an ANONYMOUS event. DO NOT include any "
                    "player names, usernames, balances, statistics, or any identifying "
                    "information whatsoever. Use only generic terms like 'a client' or "
                    "'a subject'."
                )
            elif validate_facts:
                system_prompt += (
                    "\n\nCRITICAL: Select approved components only. Return exactly "
                    "the requested JSON object. Never write commentary or freeform text."
                )

            result = await self.ai_service.complete(
                prompt=prompt,
                system_prompt=system_prompt,
                temperature=0.9,
                max_tokens=200 if validate_facts else 2000,
                feature="neon.post_match" if validate_facts else "neon.event",
            )
            if result:
                from utils.neon_terminal import ansi_block

                if validate_facts:
                    structured = _render_structured_post_match_selection(
                        result.strip(),
                        effective_context,
                    )
                    if structured:
                        return ansi_block(structured)
                    return fallback_text

                # Strip markdown code fences the LLM may have included
                stripped = result.strip()
                if stripped.startswith("```"):
                    first_nl = stripped.find("\n")
                    if first_nl != -1:
                        stripped = stripped[first_nl + 1 :]
                    if stripped.endswith("```"):
                        stripped = stripped[: -3]
                    stripped = stripped.strip()
                sanitized = _sanitize_llm_terminal_text(stripped or result)
                if sanitized:
                    return ansi_block(sanitized)
        except Exception as e:
            logger.info(f"LLM text generation failed, using template: {e}")
        return fallback_text

    def _build_player_context(self, discord_id: int, guild_id: int | None) -> dict[str, Any]:
        """Build player context dict for LLM calls."""
        ctx: dict[str, Any] = {}
        if self.player_repo:
            try:
                player = self.player_repo.get_by_id(discord_id, guild_id)
                if player:
                    ctx["name"] = player.name
                    ctx["balance"] = player.jopacoin_balance
                    ctx["lowest_balance"] = getattr(player, "lowest_balance_ever", None)
                    games = (player.wins or 0) + (player.losses or 0)
                    if games > 0:
                        ctx["win_rate"] = f"{(player.wins or 0) / games * 100:.0f}%"
            except Exception as e:
                logger.debug("Failed to build player context for %s: %s", discord_id, e)

        ctx["bankruptcy_count"] = self._get_bankruptcy_count(discord_id, guild_id)
        degen = self._get_degen_score(discord_id, guild_id)
        if degen is not None:
            ctx["degen_score"] = degen
        return ctx

    def _scaled_chance(self, score: float, *, floor: float, cap: float, full_at: float) -> float:
        """Probability that ramps linearly from `floor` (score 0) to `cap` (score >= full_at)."""
        if full_at <= 0:
            return cap
        frac = max(0.0, min(1.0, score / full_at))
        return floor + (cap - floor) * frac

    @staticmethod
    def _post_match_chance(context: JopatPostMatchContext) -> float:
        """Scale JOPA-T frequency with the strength of supplied match facts."""
        is_extreme = any(
            (
                context.payout >= 750,
                context.loss >= 500,
                context.leverage >= 5,
                context.bankruptcy_count >= 2,
                context.degen_score is not None and context.degen_score >= 95,
                context.rating_change is not None and abs(context.rating_change) >= 40,
                context.expected_win_prob is not None
                and context.expected_win_prob <= 0.30,
                abs(context.streak) >= 7,
            )
        )
        if is_extreme:
            return POST_MATCH_EXTREME_CHANCE

        is_notable = any(
            (
                context.payout >= NEON_BIGWIN_MIN_PAYOUT,
                context.loss >= 200,
                context.leverage >= 3,
                context.bankruptcy_count > 0,
                context.degen_score is not None and context.degen_score >= 80,
                context.rating_change is not None and abs(context.rating_change) >= 20,
                context.expected_win_prob is not None
                and context.expected_win_prob < 0.45,
                abs(context.streak) >= 4,
            )
        )
        if is_notable:
            return POST_MATCH_NOTABLE_CHANCE
        return POST_MATCH_BASE_CHANCE

    @staticmethod
    def _post_match_gif_theme(
        context: JopatPostMatchContext,
    ) -> tuple[str, str, int] | None:
        """Map genuinely extreme supplied facts to one rare procedural GIF theme."""
        if context.loss >= 500 and context.leverage >= 5:
            return (
                "buyback_denied",
                context.loser_name or "UNKNOWN CLIENT",
                context.loss,
            )
        if (
            context.winner_name is not None
            and context.expected_win_prob is not None
            and context.expected_win_prob <= 0.30
        ):
            return (
                "odds_anomaly",
                context.winner_name or "UNKNOWN CLIENT",
                round(context.expected_win_prob * 100),
            )
        if context.streak >= 7:
            return (
                "beyond_godlike",
                context.winner_name or "UNKNOWN CLIENT",
                context.streak,
            )
        if context.rating_change is not None and context.rating_change >= 40:
            return (
                "divine_rapier_position",
                context.winner_name or "UNKNOWN CLIENT",
                round(context.rating_change),
            )
        if context.payout >= 750:
            return (
                "ancient_liquidated",
                context.winner_name or "UNKNOWN CLIENT",
                context.payout,
            )
        return None

    async def _dig_caption(
        self,
        event_key: str,
        event_description: str,
        guild_id: int | None = None,
    ) -> str:
        """Cryptic dig caption: an LLM narrator voice (chance-gated) or a static fallback line."""
        voice = pick_dig_voice(event_key)
        line = fallback_line(event_key)
        if (
            self.dig_llm_enabled
            and self.ai_service
            and await self._is_ai_enabled(guild_id)
            and self._roll(NEON_LLM_CHANCE)
        ):
            try:
                prompt = (
                    f"A lone digger {event_description}, far beneath the earth. "
                    f"Speak one or two short, cryptic lines as {voice.name} - {voice.description}. "
                    "Only image and omen. Never name mechanics, numbers, or items."
                )
                result = await self.ai_service.complete(
                    prompt=prompt,
                    system_prompt=DIG_NARRATOR_SYSTEM_PROMPT,
                    temperature=1.0,
                    max_tokens=120,
                    feature="neon.dig_caption",
                )
                if result and result.strip():
                    cleaned = result.strip()
                    if cleaned.startswith("```"):
                        first_nl = cleaned.find("\n")
                        if first_nl != -1:
                            cleaned = cleaned[first_nl + 1 :]
                        if cleaned.endswith("```"):
                            cleaned = cleaned[:-3]
                    line = cleaned.strip().strip('"').strip() or line
            except Exception as e:
                logger.info(f"dig caption LLM failed, using static line: {e}")
        return f"> *{line}*\n> — {voice.name}"

    # -------------------------------------------------------------------
    # Public event handlers - all return NeonResult | None
    # All wrapped in try/except so failures never block bot operation.
    # -------------------------------------------------------------------

    async def on_dig_boss_victory(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        boss_name: str,
        boundary: int,
        layer_name: str,
        jc_delta: int = 0,
        gear_drop: Any = None,
        trophy_relic_drop: Any = None,
    ) -> NeonResult | None:
        """A depth guardian is slain. Pinnacle (boundary 350) is a marquee set-piece."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None

            from utils import dig_drawing

            if boundary >= 350:
                if not self._roll(0.95):
                    return None
                gif = await asyncio.to_thread(dig_drawing.animate_pinnacle, prestige=False)
                event_key, desc = "pinnacle", "has reached the Pinnacle, the floor of the world"
            else:
                chance = self._scaled_chance(boundary, floor=NEON_DIG_CHANCE, cap=0.30, full_at=350)
                if gear_drop or trophy_relic_drop:
                    chance = min(0.45, chance + 0.10)
                if not self._roll(chance):
                    return None
                title = (boss_name or "the guardian").upper()
                sub = (f"+{jc_delta:,} jc",) if jc_delta else ()
                gif = await asyncio.to_thread(
                    dig_drawing.animate_dig_reveal,
                    layer_name,
                    motion="victory",
                    title=title,
                    sub_lines=sub,
                )
                event_key, desc = "boss_victory", f"has struck down the guardian {boss_name}"

            text = await self._dig_caption(event_key, desc, guild_id)
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=3, text_block=text, gif_file=gif)
        except Exception as e:
            logger.debug(f"neon on_dig_boss_victory error: {e}")
            return None

    async def on_dig_relic_found(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        relic_name: str,
        rarity: str,
        layer_name: str,
    ) -> NeonResult | None:
        """A rare or legendary relic surfaces. Legendary is a near-certain marquee moment."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None

            from utils import dig_drawing

            rarity_l = (rarity or "").lower()
            if rarity_l == "legendary":
                if not self._roll(0.95):
                    return None
                gif = await asyncio.to_thread(dig_drawing.animate_legendary_relic, relic_name)
                event_key, desc = "legendary_relic", f"has unearthed the legendary {relic_name}"
            elif rarity_l == "rare":
                if not self._roll(NEON_DIG_CHANCE):
                    return None
                gif = await asyncio.to_thread(
                    dig_drawing.animate_dig_reveal,
                    layer_name,
                    motion="unearth",
                    title=relic_name,
                    sprite_id="crystal",
                )
                event_key, desc = "rare_relic", f"has unearthed the rare {relic_name}"
            else:
                return None  # common / uncommon do not animate

            text = await self._dig_caption(event_key, desc, guild_id)
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=3, text_block=text, gif_file=gif)
        except Exception as e:
            logger.debug(f"neon on_dig_relic_found error: {e}")
            return None

    async def on_dig_cave_in(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        depth_before: int,
        depth_after: int,
        layer_name: str,
    ) -> NeonResult | None:
        """A catastrophic cave-in rolls hard-won depth back. Odds scale with depth lost."""
        try:
            if not self._is_enabled():
                return None
            lost = depth_before - depth_after
            if lost <= 0:
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            chance = self._scaled_chance(lost, floor=NEON_DIG_CHANCE, cap=0.40, full_at=40)
            if not self._roll(chance):
                return None

            from utils import dig_drawing
            gif = await asyncio.to_thread(dig_drawing.animate_cave_in, layer_name, depth_before, depth_after)
            text = await self._dig_caption(
                "cave_in",
                "has lost their footing to a cave-in, the dark swallowing the way down",
                guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=3, text_block=text, gif_file=gif)
        except Exception as e:
            logger.debug(f"neon on_dig_cave_in error: {e}")
            return None

    async def on_dig_prestige(
        self,
        discord_id: int,
        guild_id: int | None,
    ) -> NeonResult | None:
        """Depth-400 ascension / prestige reset — a rare endgame milestone."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.95):
                return None

            from utils import dig_drawing
            gif = await asyncio.to_thread(dig_drawing.animate_pinnacle, prestige=True)
            text = await self._dig_caption(
                "prestige",
                "has ascended, prestiging beyond the deepest dark",
                guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=3, text_block=text, gif_file=gif)
        except Exception as e:
            logger.debug(f"neon on_dig_prestige error: {e}")
            return None

    async def on_big_win(
        self,
        discord_id: int,
        guild_id: int | None,
        *,
        source: str,
        payout: int,
        flavor: str = "bigwin",
    ) -> NeonResult | None:
        """Celebrate a big betting win (match / prediction / gamba). Odds scale with payout."""
        try:
            if not self._is_enabled():
                return None
            if payout < NEON_BIGWIN_MIN_PAYOUT:
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            chance = self._scaled_chance(
                payout, floor=NEON_BIGWIN_FLOOR, cap=0.95, full_at=NEON_BIGWIN_FULL_PAYOUT
            )
            if not self._roll(chance):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            from utils.neon_drawing import create_bigwin_gif
            gif = await asyncio.to_thread(create_bigwin_gif, name, payout, source=source, flavor=flavor)

            from utils.neon_terminal import ansi_block
            fallback = ansi_block(
                f"[LEDGER] +{payout:,} JC settled. The house keeps receipts on winners too."
            )
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} won big: +{payout} JC on {source} betting.",
                ctx,
                fallback,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=3, text_block=text, gif_file=gif)
        except Exception as e:
            logger.debug(f"neon on_big_win error: {e}")
            return None

    async def on_balance_check(
        self, discord_id: int, guild_id: int | None, balance: int
    ) -> NeonResult | None:
        """Trigger on /balance command. ~30% chance for Layer 1."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(NEON_LAYER1_CHANCE):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            text = render_balance_check(name, balance)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} checked their balance: {balance} JC",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_balance_check error: {e}")
            return None

    async def on_bet_placed(
        self,
        discord_id: int,
        guild_id: int | None,
        amount: int,
        leverage: int = 1,
        team: str = "",
    ) -> NeonResult | None:
        """Trigger on /bet command. ~40% chance for Layer 1."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            chance = 0.10 if leverage == 1 else 0.20
            if not self._roll(chance):
                return None

            text = render_bet_placed(amount, team, leverage)
            lev_note = f" at {leverage}x leverage" if leverage > 1 else ""
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client placed {amount} JC bet on {team}{lev_note}",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_bet_placed error: {e}")
            return None

    async def on_bet_settled(
        self,
        discord_id: int,
        guild_id: int | None,
        won: bool,
        new_balance: int,
    ) -> NeonResult | None:
        """Trigger on bet settlement. Layer 2 for zero balance or max debt."""
        try:
            if not self._is_enabled():
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)

            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)

            # Layer 2: Hit MAX_DEBT
            if new_balance <= -MAX_DEBT and self._roll(0.90):
                text = render_system_breach(name)
                text = await self._generate_text(
                    f"Client hit MAX_DEBT floor of {-MAX_DEBT} JC",
                    ctx,
                    text,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)

            # Layer 2: Hit zero
            if new_balance == 0 and not won and self._roll(0.70):
                text = render_balance_zero(name)
                text = await self._generate_text(
                    "Client's balance hit exactly 0 JC after a lost bet",
                    ctx,
                    text,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)

            return None
        except Exception as e:
            logger.debug(f"neon on_bet_settled error: {e}")
            return None

    async def on_bankruptcy(
        self,
        discord_id: int,
        guild_id: int | None,
        debt_cleared: int,
        filing_number: int,
    ) -> NeonResult | None:
        """Trigger on /economy bankruptcy. Always fires Layer 2. Layer 3 for repeat offenders."""
        try:
            if not self._is_enabled():
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)

            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            event_desc = f"Client filed bankruptcy #{filing_number}. Debt cleared: {debt_cleared} JC"

            # Layer 3: 3rd+ bankruptcy - terminal crash GIF
            if filing_number >= 3:
                try:
                    from utils.neon_drawing import create_terminal_crash_gif
                    gif = await asyncio.to_thread(create_terminal_crash_gif, name, filing_number)
                    text = render_bankruptcy_filing(name, debt_cleared, filing_number)
                    text = await self._generate_text(
                        event_desc,
                        ctx,
                        text,
                        guild_id=guild_id,
                    )
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Terminal crash GIF failed: {e}")
                    # Fall through to Layer 2

            # Layer 3: First-ever bankruptcy - welcome to the void
            if filing_number == 1:
                try:
                    from utils.neon_drawing import create_void_welcome_gif
                    gif = await asyncio.to_thread(create_void_welcome_gif, name)
                    text = render_bankruptcy_filing(name, debt_cleared, filing_number)
                    text = await self._generate_text(
                        event_desc,
                        ctx,
                        text,
                        guild_id=guild_id,
                    )
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Void welcome GIF failed: {e}")

            # Layer 2: Standard bankruptcy filing (100% chance)
            text = render_bankruptcy_filing(name, debt_cleared, filing_number)
            text = await self._generate_text(
                event_desc,
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_bankruptcy error: {e}")
            return None

    async def on_loan(
        self,
        discord_id: int,
        guild_id: int | None,
        amount: int,
        total_owed: int,
        is_negative: bool = False,
    ) -> NeonResult | None:
        """Trigger on /economy loan. Layer 1 at 50%, Layer 2 for negative loans at 80%."""
        try:
            if not self._is_enabled():
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)

            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)

            # Layer 2: Negative loan (loan while in debt)
            if is_negative:
                if self._roll(0.80):
                    new_debt = -(abs(total_owed))
                    text = render_negative_loan(name, amount, new_debt)
                    text = await self._generate_text(
                        f"Client took a loan of {amount} JC while in debt",
                        ctx,
                        text,
                        guild_id=guild_id,
                    )
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=2, text_block=text)
                # Negative loan roll failed - don't fall through to layer 1
                return None

            # Layer 1: Normal loan
            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.50):
                return None

            text = render_loan_taken(amount, total_owed)
            text = await self._generate_text(
                f"Client took a loan of {amount} JC. Total owed: {total_owed} JC",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_loan error: {e}")
            return None

    async def on_wheel_result(
        self,
        discord_id: int,
        guild_id: int | None,
        result_value: int,
        new_balance: int,
    ) -> NeonResult | None:
        """Trigger on /gamba result. Layer 3 big-win, Layer 2 for BANKRUPT/freefall."""
        try:
            if not self._is_enabled():
                return None

            # Layer 3: big win on the wheel (rare, payout-scaled)
            if result_value > 0:
                return await self.on_big_win(
                    discord_id, guild_id, source="gamba", payout=result_value
                )

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)

            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)

            # Layer 2: Wheel BANKRUPT
            if result_value < 0 and self._roll(0.30):
                text = render_wheel_bankrupt(name, result_value)
                text = await self._generate_text(
                    f"Client hit BANKRUPT on the wheel. Lost {abs(result_value)} JC",
                    ctx,
                    text,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)

            # Layer 3: Freefall - went from 100+ to 0 in one spin
            if result_value < 0 and new_balance <= 0:
                prior_balance = new_balance - result_value  # result_value is negative
                if prior_balance >= 100 and self._roll(0.25):
                    try:
                        from utils.neon_drawing import create_freefall_gif
                        gif = await asyncio.to_thread(create_freefall_gif, name, prior_balance, new_balance)
                        self._set_cooldown(discord_id, guild_id)
                        return NeonResult(layer=3, gif_file=gif)
                    except Exception as e:
                        logger.debug(f"Freefall GIF failed: {e}")

            return None
        except Exception as e:
            logger.debug(f"neon on_wheel_result error: {e}")
            return None

    async def on_lightning_bolt(
        self,
        discord_id: int,
        guild_id: int | None,
        total_taxed: int,
        players_hit: int,
    ) -> NeonResult | None:
        """Trigger on Lightning Bolt wheel result. 20% chance, Layer 1 or 2."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.20):
                return None

            # Layer 2 for big hits (500+ total), Layer 1 otherwise
            if total_taxed >= 500:
                text = render_lightning_bolt_overlay(total_taxed, players_hit)
                text = await self._generate_text(
                    f"Lightning Bolt struck {players_hit} players for {total_taxed} JC total. All went to nonprofit.",
                    await asyncio.to_thread(self._build_player_context, discord_id, guild_id),
                    text,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)
            else:
                text = render_lightning_bolt(total_taxed, players_hit)
                text = await self._generate_text(
                    f"Lightning Bolt struck {players_hit} players for {total_taxed} JC total. Wry commentary on suffering.",
                    await asyncio.to_thread(self._build_player_context, discord_id, guild_id),
                    text,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_lightning_bolt error: {e}")
            return None

    async def on_match_recorded(
        self,
        guild_id: int | None,
        streak_data: dict[str, Any] | None = None,
    ) -> NeonResult | None:
        """Compatibility wrapper for the unified JOPA-T post-match gateway."""
        streak = 0
        winner_id = None
        loser_id = None
        if streak_data:
            streak = int(streak_data.get("streak", 0) or 0)
            player_id = streak_data.get("discord_id")
            if streak_data.get("is_win"):
                winner_id = player_id
            else:
                loser_id = player_id
        return await self.on_post_match_debrief(
            guild_id,
            JopatPostMatchContext(streak=streak),
            winner_id=winner_id,
            loser_id=loser_id,
        )

    async def on_post_match_debrief(
        self,
        guild_id: int | None,
        context: JopatPostMatchContext,
        *,
        winner_id: int | None = None,
        loser_id: int | None = None,
    ) -> NeonResult | None:
        """Emit at most one chance-gated JOPA-T debrief for a recorded match."""
        try:
            if not self._is_enabled():
                return None

            winner_name = context.winner_name
            loser_name = context.loser_name
            if winner_id is not None and not winner_name:
                winner_name = await asyncio.to_thread(
                    self._get_player_name, winner_id, guild_id
                )
            if loser_id is not None and not loser_name:
                loser_name = await asyncio.to_thread(
                    self._get_player_name, loser_id, guild_id
                )
            if winner_name != context.winner_name or loser_name != context.loser_name:
                context = replace(
                    context,
                    winner_name=winner_name,
                    loser_name=loser_name,
                )

            if not self._roll(self._post_match_chance(context)):
                return None

            protocol = choose_protocol(context)
            fallback = render_fallback(protocol, context)
            text = await self._generate_text(
                build_event_description(protocol, context),
                context.prompt_context(),
                fallback,
                validate_facts=True,
                guild_id=guild_id,
            )

            gif_spec = self._post_match_gif_theme(context)
            if gif_spec and self._roll(POST_MATCH_GIF_CHANCE):
                try:
                    from utils.neon_drawing import create_post_match_gif

                    theme, name, value = gif_spec
                    gif = await asyncio.to_thread(
                        create_post_match_gif,
                        name,
                        value,
                        theme=theme,
                    )
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"post-match GIF rendering failed: {e}")

            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_post_match_debrief error: {e}")
            return None

    async def on_cooldown_hit(
        self, discord_id: int, guild_id: int | None, cooldown_type: str
    ) -> NeonResult | None:
        """Trigger when a cooldown is hit. ~40% chance for Layer 1."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.40):
                return None

            text = render_cooldown_hit(cooldown_type)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client tried to use {cooldown_type} but hit the cooldown",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_cooldown_hit error: {e}")
            return None

    async def on_leverage_loss(
        self,
        discord_id: int,
        guild_id: int | None,
        amount: int,
        leverage: int,
        new_balance: int,
    ) -> NeonResult | None:
        """Trigger on leveraged loss into debt. Layer 2 at 80%."""
        try:
            if not self._is_enabled():
                return None
            if leverage < 5 or new_balance >= 0:
                return None
            if not self._roll(0.80):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            debt = abs(new_balance)
            text = render_debt_collector(name, debt)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client lost a {leverage}x leveraged bet of {amount} JC. Now in debt: {debt} JC",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)

            # Layer 3: 5x leverage into exactly MAX_DEBT
            if leverage >= 5 and new_balance <= -MAX_DEBT and self._roll(0.30):
                try:
                    from utils.neon_drawing import create_debt_collector_gif
                    gif = await asyncio.to_thread(create_debt_collector_gif, name, debt)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Debt collector GIF failed: {e}")

            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_leverage_loss error: {e}")
            return None

    async def on_degen_milestone(
        self, discord_id: int, guild_id: int | None, degen_score: int
    ) -> NeonResult | None:
        """Trigger when degen score crosses 90. One-time per user."""
        try:
            if not self._is_enabled():
                return None
            if degen_score < 90:
                return None
            if not await asyncio.to_thread(self._check_one_time, discord_id, guild_id, "degen_90"):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            await asyncio.to_thread(self._mark_one_time, discord_id, guild_id, "degen_90", layer=3)

            try:
                from utils.neon_drawing import create_degen_certificate_gif
                gif = await asyncio.to_thread(create_degen_certificate_gif, name, degen_score)
                from utils.neon_terminal import DIM, RED, RESET, YELLOW, ansi_block
                text = ansi_block(
                    f"{RED} ACHIEVEMENT UNLOCKED{RESET}\n"
                    f"{DIM}{'=' * 36}{RESET}\n"
                    f"{DIM}Subject:{RESET} {name}\n"
                    f"{DIM}Degen Score:{RESET} {YELLOW}{degen_score}{RESET}\n"
                    f"{DIM}Classification:{RESET} {RED}LEGENDARY{RESET}\n"
                    f"{DIM}{'=' * 36}{RESET}\n"
                    f"{DIM}The system acknowledges your{RESET}\n"
                    f"{DIM}commitment to financial ruin.{RESET}"
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=3, text_block=text, gif_file=gif)
            except Exception as e:
                logger.debug(f"Degen certificate GIF failed: {e}")
                return None
        except Exception as e:
            logger.debug(f"neon on_degen_milestone error: {e}")
            return None

    async def on_gamba_spectator(
        self, discord_id: int, guild_id: int | None, display_name: str
    ) -> NeonResult | None:
        """Trigger when someone reacts jopacoin on the lobby. ~5% chance."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.05):
                return None

            from utils.neon_terminal import render_gamba_spectator
            text = render_gamba_spectator(display_name)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {display_name} is watching the lobby. Spectator mode.",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_gamba_spectator error: {e}")
            return None

    async def on_tip(
        self,
        discord_id: int,
        guild_id: int | None,
        sender_name: str,
        recipient_name: str,
        amount: int,
        fee: int,
    ) -> NeonResult | None:
        """Trigger on /economy tip. 5% Layer 2 surveillance report, 20% Layer 1 one-liner."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None

            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)

            # Layer 2: Surveillance report (5%)
            if self._roll(0.05):
                from utils.neon_terminal import render_tip_surveillance
                text = render_tip_surveillance(sender_name, recipient_name, amount, fee)
                text = await self._generate_text(
                    f"Client {sender_name} transferred {amount} JC to {recipient_name}. Fee: {fee} JC",
                    ctx,
                    text,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)

            # Layer 1: One-liner (20%)
            if self._roll(0.20):
                from utils.neon_terminal import render_tip
                text = render_tip(sender_name, recipient_name, amount)
                text = await self._generate_text(
                    f"Client {sender_name} tipped {amount} JC to {recipient_name}",
                    ctx,
                    text,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=1, text_block=text)

            return None
        except Exception as e:
            logger.debug(f"neon on_tip error: {e}")
            return None

    async def on_double_or_nothing(
        self,
        discord_id: int,
        guild_id: int | None,
        won: bool,
        balance_at_risk: int,
        final_balance: int,
    ) -> NeonResult | None:
        """Trigger on Double or Nothing result. Layer 1 always on win, L2/L3 on loss."""
        try:
            if not self._is_enabled():
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)

            if won:
                # Layer 3: big Double-or-Nothing win (rare, payout-scaled)
                bw = await self.on_big_win(
                    discord_id, guild_id, source="gamba", payout=balance_at_risk
                )
                if bw:
                    return bw
                # Layer 1: Always fire on win (100%)
                text = render_don_win(name, final_balance)
                text = await self._generate_text(
                    f"Client won Double or Nothing. Balance: {final_balance} JC",
                    ctx,
                    text,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=1, text_block=text)

            # Loss path

            # Layer 3: Large loss (>100 JC at risk) - coin flip GIF (rare)
            if balance_at_risk > 100 and self._roll(0.18):
                try:
                    from utils.neon_drawing import create_don_coin_flip_gif
                    gif = await asyncio.to_thread(create_don_coin_flip_gif, name, balance_at_risk)
                    text = render_don_loss_box(name, balance_at_risk)
                    text = await self._generate_text(
                        f"Client lost {balance_at_risk} JC in Double or Nothing. Balance: 0",
                        ctx,
                        text,
                        guild_id=guild_id,
                    )
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"DoN coin flip GIF failed: {e}")
                    # Fall through to Layer 2

            # Layer 2: Loss with >50 JC at risk (80%)
            if balance_at_risk > 50 and self._roll(0.80):
                text = render_don_loss_box(name, balance_at_risk)
                text = await self._generate_text(
                    f"Client lost {balance_at_risk} JC in Double or Nothing. Balance: 0",
                    ctx,
                    text,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)

            # Layer 1: Any loss (100%)
            text = render_don_lose(name, balance_at_risk)
            text = await self._generate_text(
                f"Client lost {balance_at_risk} JC in Double or Nothing",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=1, text_block=text)

        except Exception as e:
            logger.debug(f"neon on_double_or_nothing error: {e}")
            return None

    async def on_draft_coinflip(
        self,
        guild_id: int | None,
        winner_id: int,
        loser_id: int,
    ) -> NeonResult | None:
        """Trigger on draft coinflip result. Layer 1 at 40% chance."""
        try:
            if not self._is_enabled():
                return None
            if not self._roll(0.40):
                return None

            winner_name, loser_name = await asyncio.gather(
                asyncio.to_thread(self._get_player_name, winner_id, guild_id),
                asyncio.to_thread(self._get_player_name, loser_id, guild_id),
            )
            text = render_coinflip(winner_name, loser_name)
            text = await self._generate_text(
                f"Draft coinflip: {winner_name} won, {loser_name} lost",
                {"winner": winner_name, "loser": loser_name},
                text,
                guild_id=guild_id,
            )
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_draft_coinflip error: {e}")
            return None

    async def on_registration(
        self,
        discord_id: int,
        guild_id: int | None,
        player_name: str,
    ) -> NeonResult | None:
        """Trigger on player registration. Layer 1 at 50%, one-time per user."""
        try:
            if not self._is_enabled():
                return None
            if not await asyncio.to_thread(self._check_one_time, discord_id, guild_id, "registration"):
                return None
            if not self._roll(0.50):
                return None

            text = render_registration(player_name)
            text = await self._generate_text(
                f"New player '{player_name}' just registered. 3 JC starting balance.",
                {"name": player_name},
                text,
                guild_id=guild_id,
            )
            await asyncio.to_thread(self._mark_one_time, discord_id, guild_id, "registration", layer=1)
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_registration error: {e}")
            return None

    async def on_prediction_resolved(
        self,
        guild_id: int | None,
        question: str,
        outcome: str,
        total_pool: int,
        winner_count: int,
        loser_count: int,
    ) -> NeonResult | None:
        """Trigger on prediction market resolution. L1 30%, L2/L3 for large pools."""
        try:
            if not self._is_enabled():
                return None

            event_desc = f"Prediction resolved: '{question}' -> {outcome}. Pool: {total_pool} JC"
            pred_ctx: dict[str, Any] = {
                "question": question, "outcome": outcome,
                "total_pool": total_pool, "winners": winner_count, "losers": loser_count,
            }

            # Layer 3: Massive pool (>=500 JC) - market crash GIF (rare)
            if total_pool >= 500 and self._roll(0.30):
                try:
                    from utils.neon_drawing import create_market_crash_gif
                    gif = await asyncio.to_thread(create_market_crash_gif, total_pool, outcome, winner_count, loser_count)
                    text = render_prediction_market_crash(
                        question, total_pool, outcome, winner_count, loser_count
                    )
                    text = await self._generate_text(
                        event_desc,
                        pred_ctx,
                        text,
                        guild_id=guild_id,
                    )
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Market crash GIF failed: {e}")
                    # Fall through to Layer 2

            # Layer 2: Large pool (>=200 JC) at 70%
            if total_pool >= 200 and self._roll(0.70):
                text = render_prediction_market_crash(
                    question, total_pool, outcome, winner_count, loser_count
                )
                text = await self._generate_text(
                    event_desc,
                    pred_ctx,
                    text,
                    guild_id=guild_id,
                )
                return NeonResult(layer=2, text_block=text)

            # Layer 1: Any resolution at 30%
            if not self._roll(0.30):
                return None

            text = render_prediction_resolved(question, outcome, total_pool)
            text = await self._generate_text(
                event_desc,
                pred_ctx,
                text,
                guild_id=guild_id,
            )
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_prediction_resolved error: {e}")
            return None

    async def on_soft_avoid(
        self,
        discord_id: int,
        guild_id: int | None,
        cost: int,
        games: int,
    ) -> NeonResult | None:
        """Trigger on soft avoid purchase. 10% Layer 2, 25% Layer 1.

        Uses anonymous mode to prevent leaking the buyer's identity in the
        public neon message (the purchase itself is ephemeral).
        """
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None

            event_desc = f"A soft avoid was purchased. Cost: {cost} JC. Duration: {games} games"

            # Layer 2: Surveillance report (10%)
            if self._roll(0.10):
                text = render_soft_avoid_surveillance(cost, games)
                text = await self._generate_text(
                    event_desc,
                    {},
                    text,
                    anonymous=True,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)

            # Layer 1: One-liner (25%)
            if self._roll(0.25):
                text = render_soft_avoid(cost, games)
                text = await self._generate_text(
                    event_desc,
                    {},
                    text,
                    anonymous=True,
                    guild_id=guild_id,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=1, text_block=text)

            return None
        except Exception as e:
            logger.debug(f"neon on_soft_avoid error: {e}")
            return None

    # -------------------------------------------------------------------
    # NEW EVENT HANDLERS - Easter Egg Events Expansion
    # -------------------------------------------------------------------

    async def on_all_in_bet(
        self,
        discord_id: int,
        guild_id: int | None,
        amount: int,
        balance_before: int,
    ) -> NeonResult | None:
        """Trigger on bet using 90%+ of balance. Layer 2 at 35%."""
        try:
            if not self._is_enabled():
                return None
            if balance_before <= 0:
                return None

            percentage = (amount / balance_before) * 100
            if percentage < 90:
                return None

            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.35):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            text = render_all_in_bet(name, amount, percentage)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} went ALL-IN with {amount} JC ({percentage:.0f}% of balance)",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_all_in_bet error: {e}")
            return None

    async def on_last_second_bet(
        self,
        discord_id: int,
        guild_id: int | None,
        seconds_remaining: int,
    ) -> NeonResult | None:
        """Trigger on bet in final 60 seconds of window. Layer 2 at 5%."""
        try:
            if not self._is_enabled():
                return None
            if seconds_remaining > 60:
                return None

            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.05):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            text = render_last_second_bet(name, seconds_remaining)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} placed bet with only {seconds_remaining}s remaining",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_last_second_bet error: {e}")
            return None

    async def on_bomb_pot(
        self,
        guild_id: int | None,
        pool_amount: int,
        contributor_count: int,
    ) -> NeonResult | None:
        """Trigger on bomb pot event. Layer 3 GIF at 25%."""
        try:
            if not self._is_enabled():
                return None
            if not self._roll(0.25):
                return None

            logger.info(
                "Bomb pot neon firing for guild %s (pool=%s, contributors=%s)",
                guild_id, pool_amount, contributor_count,
            )
            # Layer 3: Bomb pot GIF
            try:
                from utils.neon_drawing import create_bomb_pot_gif
                gif = await asyncio.to_thread(create_bomb_pot_gif, pool_amount, contributor_count)
                text = render_bomb_pot(pool_amount, contributor_count)
                return NeonResult(layer=3, text_block=text, gif_file=gif)
            except Exception as e:
                logger.warning(f"Bomb pot GIF render failed, falling back to text: {e}")
                # Fall back to text only
                text = render_bomb_pot(pool_amount, contributor_count)
                return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_bomb_pot error: {e}")
            return None

    async def on_lobby_join(
        self,
        discord_id: int,
        guild_id: int | None,
        queue_position: int,
    ) -> NeonResult | None:
        """Trigger on lobby join. Layer 1 at 3%."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.03):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            text = render_lobby_join(name, queue_position)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} joined the queue at position {queue_position}",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_lobby_join error: {e}")
            return None

    async def on_rivalry_detected(
        self,
        guild_id: int | None,
        player1_id: int,
        player2_id: int,
        games_together: int,
        winrate_vs: float,
    ) -> NeonResult | None:
        """Trigger on 10+ games with 70%+ winrate imbalance. Layer 2 at 1%."""
        try:
            if not self._is_enabled():
                return None
            if games_together < 10:
                return None
            if winrate_vs < 70 and winrate_vs > 30:
                return None  # Only trigger if one-sided
            if not self._roll(0.01):
                return None

            player1_name, player2_name = await asyncio.gather(
                asyncio.to_thread(self._get_player_name, player1_id, guild_id),
                asyncio.to_thread(self._get_player_name, player2_id, guild_id),
            )
            text = render_rivalry_detected(player1_name, player2_name, games_together, winrate_vs)
            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_rivalry_detected error: {e}")
            return None

    async def on_games_milestone(
        self,
        discord_id: int,
        guild_id: int | None,
        total_games: int,
    ) -> NeonResult | None:
        """Trigger on 10/50/100/200/500 games. Layer 2 for <100, Layer 3 GIF for 100+. 10% chance."""
        try:
            if not self._is_enabled():
                return None
            if total_games not in (10, 50, 100, 200, 500):
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.10):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)

            # Layer 3: 100+ games gets special treatment
            if total_games >= 100:
                try:
                    from utils.neon_drawing import create_degen_certificate_gif
                    # Use degen certificate style but for games milestone
                    gif = await asyncio.to_thread(create_degen_certificate_gif, name, total_games)
                    text = render_games_milestone(name, total_games)
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Games milestone GIF failed: {e}")

            # Layer 2: Standard milestone box
            text = render_games_milestone(name, total_games)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} has played {total_games} games",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_games_milestone error: {e}")
            return None

    async def on_win_streak_record(
        self,
        discord_id: int,
        guild_id: int | None,
        current_streak: int,
        previous_best: int,
    ) -> NeonResult | None:
        """Trigger on personal best win streak (5+ min). Layer 2 for 5-7, Layer 3 GIF for 8+. 50% chance."""
        try:
            if not self._is_enabled():
                return None
            if current_streak < 5:
                return None
            if current_streak <= previous_best:
                return None  # Not a new record
            if not self._check_cooldown(discord_id, guild_id):
                return None
            if not self._roll(0.20):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)

            # Layer 3: 8+ streak gets GIF
            if current_streak >= 8:
                try:
                    from utils.neon_drawing import create_streak_record_gif
                    gif = await asyncio.to_thread(create_streak_record_gif, name, current_streak)
                    text = render_win_streak_record(name, current_streak)
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Streak record GIF failed: {e}")

            # Layer 2: Standard streak box
            text = render_win_streak_record(name, current_streak)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} broke their personal win streak record: {current_streak} games",
                ctx,
                text,
                guild_id=guild_id,
            )
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_win_streak_record error: {e}")
            return None

    async def on_first_leverage_bet(
        self,
        discord_id: int,
        guild_id: int | None,
        leverage: int,
    ) -> NeonResult | None:
        """Trigger on first ever 2x+ leverage bet. Layer 1 at 80% (one-time)."""
        try:
            if not self._is_enabled():
                return None
            if leverage < 2:
                return None
            if not await asyncio.to_thread(self._check_one_time, discord_id, guild_id, "first_leverage"):
                return None
            if not self._roll(0.80):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            text = render_first_leverage(name, leverage)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} used leverage for the first time: {leverage}x",
                ctx,
                text,
                guild_id=guild_id,
            )
            await asyncio.to_thread(self._mark_one_time, discord_id, guild_id, "first_leverage", layer=1)
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_first_leverage_bet error: {e}")
            return None

    async def on_100_bets_milestone(
        self,
        discord_id: int,
        guild_id: int | None,
        total_bets: int,
    ) -> NeonResult | None:
        """Trigger on 100 total bets placed. Layer 2 at 50% (one-time)."""
        try:
            if not self._is_enabled():
                return None
            if total_bets != 100:
                return None
            if not await asyncio.to_thread(self._check_one_time, discord_id, guild_id, "100_bets"):
                return None
            if not self._roll(0.50):
                return None

            name = await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
            text = render_bets_milestone(name, total_bets)
            ctx = await asyncio.to_thread(self._build_player_context, discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} has placed 100 total bets",
                ctx,
                text,
                guild_id=guild_id,
            )
            await asyncio.to_thread(self._mark_one_time, discord_id, guild_id, "100_bets", layer=2)
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_100_bets_milestone error: {e}")
            return None

    async def on_captain_symmetry(
        self,
        guild_id: int | None,
        captain1_id: int,
        captain2_id: int,
        rating_diff: int,
    ) -> NeonResult | None:
        """Trigger when captains within 50 rating points. Layer 1 at 20%."""
        try:
            if not self._is_enabled():
                return None
            if abs(rating_diff) > 50:
                return None
            if not self._roll(0.20):
                return None

            captain1_name, captain2_name = await asyncio.gather(
                asyncio.to_thread(self._get_player_name, captain1_id, guild_id),
                asyncio.to_thread(self._get_player_name, captain2_id, guild_id),
            )
            text = render_captain_symmetry(captain1_name, captain2_name, abs(rating_diff))
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_captain_symmetry error: {e}")
            return None

    async def on_match_enriched(
        self,
        guild_id: int | None,
        winners: list[dict[str, Any]],
        losers: list[dict[str, Any]] | None = None,
    ) -> list[NeonResult]:
        """Emit at most one JOPA-T callout from enriched match telemetry."""
        if not getattr(_config, "NEON_DEGEN_ENABLED", False):
            return []

        losers = losers or []
        telemetry_fields = (
            "hero_id",
            "kills",
            "deaths",
            "assists",
            "gpm",
            "xpm",
            "fantasy_points",
        )
        enriched_winners = [
            player
            for player in winners
            if any(player.get(field) is not None for field in telemetry_fields)
        ]
        extreme_losers = [
            player
            for player in losers
            if (player.get("deaths", 0) or 0) >= 14
            or (
                (player.get("deaths", 0) or 0) >= 8
                and (
                    (player.get("kills", 0) or 0)
                    + (player.get("assists", 0) or 0)
                )
                / max(1, player.get("deaths", 0) or 0)
                <= 0.75
            )
        ]
        if not enriched_winners and not extreme_losers:
            return []
        if not self._roll(NEON_MVP_CHANCE):
            return []

        try:
            if extreme_losers:
                target = max(
                    extreme_losers,
                    key=lambda player: (
                        player.get("deaths", 0) or 0,
                        -((player.get("kills", 0) or 0) + (player.get("assists", 0) or 0)),
                    ),
                )
                is_winner = False
            else:
                target = max(
                    enriched_winners,
                    key=lambda player: (
                        player.get("fantasy_points") or 0,
                        (player.get("kills", 0) or 0)
                        + (player.get("assists", 0) or 0),
                        player.get("gpm", 0) or 0,
                    ),
                )
                is_winner = True

            from utils.hero_lookup import get_hero_name

            discord_id = target.get("discord_id")
            player_name = (
                await asyncio.to_thread(self._get_player_name, discord_id, guild_id)
                if discord_id is not None
                else "Unknown client"
            )
            hero_id = target.get("hero_id")
            hero_name = get_hero_name(hero_id) if hero_id else None
            context = JopatPostMatchContext(
                winner_name=player_name if is_winner else None,
                loser_name=player_name if not is_winner else None,
                hero=hero_name,
                kills=target.get("kills"),
                deaths=target.get("deaths"),
                assists=target.get("assists"),
                gpm=target.get("gpm"),
                xpm=target.get("xpm"),
            )
            protocol = choose_protocol(context)
            fallback = render_fallback(protocol, context)
            text = await self._generate_text(
                build_event_description(protocol, context),
                context.prompt_context(),
                fallback,
                validate_facts=True,
                guild_id=guild_id,
            )
            mention = f"<@{discord_id}>\n" if discord_id is not None else ""
            return [NeonResult(layer=2, text_block=f"{mention}{text}")]
        except Exception as e:
            logger.debug(f"neon on_match_enriched error: {e}")
            return []
