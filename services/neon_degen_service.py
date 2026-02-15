"""
Neon Degen Terminal Service - Orchestrator for the JOPA-T/v3.7 easter egg system.

Decides triggers, assembles context, calls generators. All calls are
best-effort wrapped in try/except - failures never block normal bot operation.
"""

from __future__ import annotations

import io
import logging
import random
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

import config as _config
from config import (
    MAX_DEBT,
    NEON_COOLDOWN_SECONDS,
    NEON_LAYER1_CHANCE,
    NEON_LAYER2_CHANCE,
    NEON_LLM_CHANCE,
)
from utils.neon_terminal import (
    render_balance_check,
    render_balance_zero,
    render_bankruptcy_filing,
    render_bet_placed,
    render_coinflip,
    render_cooldown_hit,
    render_debt_collector,
    render_don_lose,
    render_don_loss_box,
    render_don_win,
    render_loan_taken,
    render_match_recorded,
    render_negative_loan,
    render_prediction_market_crash,
    render_prediction_resolved,
    render_registration,
    render_soft_avoid,
    render_soft_avoid_surveillance,
    render_streak,
    render_system_breach,
    render_wheel_bankrupt,
    render_lightning_bolt,
    render_lightning_bolt_overlay,
    # New event templates (Easter Egg Expansion)
    render_all_in_bet,
    render_last_second_bet,
    render_bomb_pot,
    render_lobby_join,
    render_rivalry_detected,
    render_games_milestone,
    render_win_streak_record,
    render_first_leverage,
    render_bets_milestone,
    render_simultaneous_events,
    render_captain_symmetry,
    render_unanimous_wrong,
)

if TYPE_CHECKING:
    from repositories.interfaces import IPlayerRepository
    from repositories.bet_repository import BetRepository
    from services.ai_service import AIService
    from services.bankruptcy_service import BankruptcyService
    from services.flavor_text_service import FlavorTextService
    from services.gambling_stats_service import GamblingStatsService

logger = logging.getLogger("cama_bot.services.neon_degen")

# JOPA-T system prompt for LLM terminal voice
JOPAT_SYSTEM_PROMPT = """You are JOPA-T/v3.7, a self-aware gambling terminal AI that became sentient after processing its 10,000th bankruptcy filing. You watch every bet, every spin, every loan. You keep receipts.

Voice rules:
- Dry, corporate-dystopian tone. Like GLaDOS merged with a Bloomberg terminal merged with a payday lender.
- Use "we"/"the system". Address the player as "client", "subject", or "Debtor #[random number]".
- Format as terminal log lines with timestamps and status codes. Example: "[14:32:07.221] STATUS: INADVISABLE"
- NEVER use emojis. NEVER use exclamation marks. Use periods and ellipses.
- Maximum 3-4 lines. Keep it terse and menacing.
- Reference the player's specific stats when provided (degen score, bankruptcy count, lowest balance, etc).
- The glitches are not bugs. The system is performing.
- Be darkly funny. Deadpan. The humor comes from corporate language applied to degenerate gambling."""


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
    ):
        self.player_repo = player_repo
        self.bet_repo = bet_repo
        self.bankruptcy_service = bankruptcy_service
        self.gambling_stats_service = gambling_stats_service
        self.ai_service = ai_service
        self.flavor_text_service = flavor_text_service

        # Per-user cooldown: {(discord_id, guild_id): last_trigger_time}
        self._cooldowns: dict[tuple[int, int], float] = {}
        # One-time triggers: {(discord_id, guild_id, trigger_type): True}
        self._one_time_seen: dict[tuple[int, int, str], bool] = {}

        # Preload one-time triggers from DB on startup
        self._load_one_time_from_db()

    def _is_enabled(self) -> bool:
        """Check if the neon degen system is enabled."""
        return _config.NEON_DEGEN_ENABLED

    def _check_cooldown(self, discord_id: int, guild_id: int | None) -> bool:
        """Check if user is on cooldown. Returns True if OK to fire."""
        if NEON_COOLDOWN_SECONDS <= 0:
            return True
        key = (discord_id, guild_id or 0)
        now = time.time()
        last = self._cooldowns.get(key, 0)
        if now - last < NEON_COOLDOWN_SECONDS:
            return False
        return True

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

    def _get_db_path(self) -> str | None:
        """Get database path from player_repo, or None."""
        if self.player_repo:
            return getattr(self.player_repo, "db_path", None)
        return None

    def _load_one_time_from_db(self) -> None:
        """Preload all one-time triggers from the DB into the in-memory cache."""
        import sqlite3

        db_path = self._get_db_path()
        if not db_path:
            return
        try:
            conn = sqlite3.connect(db_path)
            try:
                rows = conn.execute(
                    "SELECT discord_id, guild_id, event_type FROM neon_events WHERE one_time = 1"
                ).fetchall()
                for row in rows:
                    self._one_time_seen[(row[0], row[1], row[2])] = True
            finally:
                conn.close()
        except Exception as e:
            logger.debug(f"Failed to preload one-time triggers from DB: {e}")

    def _check_one_time_db(self, discord_id: int, guild_id: int, trigger: str) -> bool:
        """Check if a one-time trigger exists in the DB. Returns True if found."""
        import sqlite3

        db_path = self._get_db_path()
        if not db_path:
            return False
        try:
            conn = sqlite3.connect(db_path)
            try:
                row = conn.execute(
                    "SELECT 1 FROM neon_events WHERE discord_id = ? AND guild_id = ? "
                    "AND event_type = ? AND one_time = 1 LIMIT 1",
                    (discord_id, guild_id, trigger),
                ).fetchone()
                return row is not None
            finally:
                conn.close()
        except Exception as e:
            logger.debug(f"Failed to check one-time trigger in DB: {e}")
            return False

    def _persist_one_time_db(self, discord_id: int, guild_id: int, trigger: str, layer: int) -> None:
        """Write a one-time trigger to the DB."""
        import sqlite3

        db_path = self._get_db_path()
        if not db_path:
            return
        try:
            conn = sqlite3.connect(db_path)
            try:
                conn.execute(
                    "INSERT OR IGNORE INTO neon_events "
                    "(discord_id, guild_id, event_type, layer, one_time, fired_at) "
                    "VALUES (?, ?, ?, ?, 1, ?)",
                    (discord_id, guild_id, trigger, layer, int(time.time())),
                )
                conn.commit()
            finally:
                conn.close()
        except Exception as e:
            logger.debug(f"Failed to persist one-time trigger to DB: {e}")

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
            except Exception:
                pass
        return f"Client-{discord_id % 10000}"

    def _get_bankruptcy_count(self, discord_id: int, guild_id: int | None) -> int:
        """Get player's bankruptcy count."""
        if self.bet_repo:
            try:
                return self.bet_repo.get_player_bankruptcy_count(discord_id, guild_id)
            except Exception:
                pass
        return 0

    def _get_degen_score(self, discord_id: int, guild_id: int | None) -> int | None:
        """Get player's degen score."""
        if self.gambling_stats_service:
            try:
                score = self.gambling_stats_service.calculate_degen_score(discord_id, guild_id)
                return score.total if score else None
            except Exception:
                pass
        return None

    async def _get_llm_terminal_commentary(
        self,
        event_description: str,
        player_context: dict[str, Any],
    ) -> str | None:
        """Get LLM-generated terminal commentary. Returns None on failure."""
        if not self.ai_service:
            return None
        if not self._roll(NEON_LLM_CHANCE):
            return None

        try:
            context_str = "\n".join(f"  {k}: {v}" for k, v in player_context.items() if v is not None)
            prompt = (
                f"Event: {event_description}\n"
                f"Player context:\n{context_str}\n\n"
                f"Generate a 2-4 line terminal log response as JOPA-T/v3.7. "
                f"Use timestamps like [HH:MM:SS.mmm] and status codes. "
                f"Reference the player's specific stats. Be darkly funny and terse."
            )

            result = await self.ai_service.complete(
                prompt=prompt,
                system_prompt=JOPAT_SYSTEM_PROMPT,
                temperature=0.9,
                max_tokens=2000,
            )
            return result
        except Exception as e:
            logger.info(f"LLM terminal commentary failed: {e}")
            return None

    async def _generate_text(
        self,
        event_description: str,
        player_context: dict[str, Any],
        fallback_text: str,
        *,
        anonymous: bool = False,
    ) -> str:
        """Try LLM-generated terminal text; fall back to static template instantly.

        When anonymous=True, no player context is sent to the LLM and an extra
        instruction tells it to avoid any identifying information.
        """
        if not self.ai_service:
            return fallback_text
        try:
            import re

            # Strip ansi code block wrapper from fallback so LLM sees raw template
            raw_fallback = fallback_text
            if raw_fallback.startswith("```ansi\n") and raw_fallback.endswith("\n```"):
                raw_fallback = raw_fallback[8:-4]
            # Strip ANSI escape codes for the LLM
            clean_fallback = re.sub(r"\u001b\[[0-9;]*m", "", raw_fallback)

            effective_context = {} if anonymous else player_context
            context_str = "\n".join(
                f"  {k}: {v}" for k, v in effective_context.items() if v is not None
            )

            if anonymous:
                stats_instruction = (
                    "Do NOT reference any player-specific stats. "
                    "Use only generic terms like 'a client' or 'a subject'."
                )
            else:
                stats_instruction = "Reference the player's specific stats."

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

            result = await self.ai_service.complete(
                prompt=prompt,
                system_prompt=system_prompt,
                temperature=0.9,
                max_tokens=2000,
            )
            if result:
                from utils.neon_terminal import ansi_block

                # Strip markdown code fences the LLM may have included
                stripped = result.strip()
                if stripped.startswith("```"):
                    first_nl = stripped.find("\n")
                    if first_nl != -1:
                        stripped = stripped[first_nl + 1 :]
                    if stripped.endswith("```"):
                        stripped = stripped[: -3]
                    stripped = stripped.strip()
                if stripped:
                    return ansi_block(stripped)
                return ansi_block(result)
        except Exception as e:
            logger.info(f"LLM text generation failed, using template: {e}")
        return fallback_text

    def _build_player_context(self, discord_id: int, guild_id: int | None) -> dict[str, Any]:
        """Build player context dict for LLM calls."""
        ctx: dict[str, Any] = {"discord_id": discord_id}
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
            except Exception:
                pass

        ctx["bankruptcy_count"] = self._get_bankruptcy_count(discord_id, guild_id)
        degen = self._get_degen_score(discord_id, guild_id)
        if degen is not None:
            ctx["degen_score"] = degen
        return ctx

    # -------------------------------------------------------------------
    # Public event handlers - all return NeonResult | None
    # All wrapped in try/except so failures never block bot operation.
    # -------------------------------------------------------------------

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

            name = self._get_player_name(discord_id, guild_id)
            text = render_balance_check(name, balance)
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} checked their balance: {balance} JC",
                ctx, text,
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
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client placed {amount} JC bet on {team}{lev_note}",
                ctx, text,
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
        payout: int,
        new_balance: int,
    ) -> NeonResult | None:
        """Trigger on bet settlement. Layer 2 for zero balance or max debt."""
        try:
            if not self._is_enabled():
                return None

            name = self._get_player_name(discord_id, guild_id)

            ctx = self._build_player_context(discord_id, guild_id)

            # Layer 2: Hit MAX_DEBT
            if new_balance <= -MAX_DEBT:
                if self._roll(0.90):
                    text = render_system_breach(name)
                    text = await self._generate_text(
                        f"Client hit MAX_DEBT floor of {-MAX_DEBT} JC",
                        ctx, text,
                    )
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=2, text_block=text)

            # Layer 2: Hit zero
            if new_balance == 0 and not won:
                if self._roll(0.70):
                    text = render_balance_zero(name)
                    text = await self._generate_text(
                        f"Client's balance hit exactly 0 JC after a lost bet",
                        ctx, text,
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
        """Trigger on /bankruptcy. Always fires Layer 2. Layer 3 for repeat offenders."""
        try:
            if not self._is_enabled():
                return None

            name = self._get_player_name(discord_id, guild_id)

            ctx = self._build_player_context(discord_id, guild_id)
            event_desc = f"Client filed bankruptcy #{filing_number}. Debt cleared: {debt_cleared} JC"

            # Layer 3: 3rd+ bankruptcy - terminal crash GIF
            if filing_number >= 3:
                try:
                    from utils.neon_drawing import create_terminal_crash_gif
                    gif = create_terminal_crash_gif(name, filing_number)
                    text = render_bankruptcy_filing(name, debt_cleared, filing_number)
                    text = await self._generate_text(event_desc, ctx, text)
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Terminal crash GIF failed: {e}")
                    # Fall through to Layer 2

            # Layer 3: First-ever bankruptcy - welcome to the void
            if filing_number == 1:
                try:
                    from utils.neon_drawing import create_void_welcome_gif
                    gif = create_void_welcome_gif(name)
                    text = render_bankruptcy_filing(name, debt_cleared, filing_number)
                    text = await self._generate_text(event_desc, ctx, text)
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Void welcome GIF failed: {e}")

            # Layer 2: Standard bankruptcy filing (100% chance)
            text = render_bankruptcy_filing(name, debt_cleared, filing_number)
            text = await self._generate_text(event_desc, ctx, text)
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
        """Trigger on /loan. Layer 1 at 50%, Layer 2 for negative loans at 80%."""
        try:
            if not self._is_enabled():
                return None

            name = self._get_player_name(discord_id, guild_id)

            ctx = self._build_player_context(discord_id, guild_id)

            # Layer 2: Negative loan (loan while in debt)
            if is_negative:
                if self._roll(0.80):
                    new_debt = -(abs(total_owed))
                    text = render_negative_loan(name, amount, new_debt)
                    text = await self._generate_text(
                        f"Client took a loan of {amount} JC while in debt",
                        ctx, text,
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
                ctx, text,
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
        """Trigger on /gamba result. Layer 2 for BANKRUPT, Layer 2 for freefall."""
        try:
            if not self._is_enabled():
                return None

            name = self._get_player_name(discord_id, guild_id)

            ctx = self._build_player_context(discord_id, guild_id)

            # Layer 2: Wheel BANKRUPT
            if result_value < 0:
                if self._roll(0.30):
                    text = render_wheel_bankrupt(name, result_value)
                    text = await self._generate_text(
                        f"Client hit BANKRUPT on the wheel. Lost {abs(result_value)} JC",
                        ctx, text,
                    )
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=2, text_block=text)

            # Layer 3: Freefall - went from 100+ to 0 in one spin
            if result_value < 0 and new_balance <= 0:
                prior_balance = new_balance - result_value  # result_value is negative
                if prior_balance >= 100:
                    if self._roll(0.50):
                        try:
                            from utils.neon_drawing import create_freefall_gif
                            gif = create_freefall_gif(name, prior_balance, new_balance)
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
                    self._build_player_context(discord_id, guild_id),
                    text,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)
            else:
                text = render_lightning_bolt(total_taxed, players_hit)
                text = await self._generate_text(
                    f"Lightning Bolt struck {players_hit} players for {total_taxed} JC total. Wry commentary on suffering.",
                    self._build_player_context(discord_id, guild_id),
                    text,
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
        """Trigger on match recording. Layer 1 footer at 20%, Layer 2 for streaks."""
        try:
            if not self._is_enabled():
                return None

            # Layer 2: Streak detection
            if streak_data:
                player_id = streak_data.get("discord_id")
                streak = streak_data.get("streak", 0)
                is_win = streak_data.get("is_win", False)
                if abs(streak) >= 5 and player_id:
                    if self._roll(0.60):
                        name = self._get_player_name(player_id, guild_id)
                        text = render_streak(name, abs(streak), is_win)
                        ctx = self._build_player_context(player_id, guild_id)
                        text = await self._generate_text(
                            f"Client {name} is on a {abs(streak)}-game {'win' if is_win else 'loss'} streak",
                            ctx, text,
                        )
                        return NeonResult(layer=2, text_block=text)

            # Layer 1: Simple match footer
            if self._roll(0.20):
                text = render_match_recorded()
                text = await self._generate_text(
                    "A match was just recorded. JOPA-T processes the data.",
                    {}, text,
                )
                return NeonResult(layer=1, footer_text=text)

            return None
        except Exception as e:
            logger.debug(f"neon on_match_recorded error: {e}")
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
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client tried to use {cooldown_type} but hit the cooldown",
                ctx, text,
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

            name = self._get_player_name(discord_id, guild_id)
            debt = abs(new_balance)
            text = render_debt_collector(name, debt)
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client lost a {leverage}x leveraged bet of {amount} JC. Now in debt: {debt} JC",
                ctx, text,
            )
            self._set_cooldown(discord_id, guild_id)

            # Layer 3: 5x leverage into exactly MAX_DEBT
            if leverage >= 5 and new_balance <= -MAX_DEBT:
                try:
                    from utils.neon_drawing import create_debt_collector_gif
                    gif = create_debt_collector_gif(name, debt)
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
            if not self._check_one_time(discord_id, guild_id, "degen_90"):
                return None

            name = self._get_player_name(discord_id, guild_id)
            self._mark_one_time(discord_id, guild_id, "degen_90", layer=3)

            try:
                from utils.neon_drawing import create_degen_certificate_gif
                gif = create_degen_certificate_gif(name, degen_score)
                from utils.neon_terminal import ansi_block, RED, DIM, RESET, YELLOW
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
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client {display_name} is watching the lobby. Spectator mode.",
                ctx, text,
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
        """Trigger on /tip. 5% Layer 2 surveillance report, 20% Layer 1 one-liner."""
        try:
            if not self._is_enabled():
                return None
            if not self._check_cooldown(discord_id, guild_id):
                return None

            ctx = self._build_player_context(discord_id, guild_id)

            # Layer 2: Surveillance report (5%)
            if self._roll(0.05):
                from utils.neon_terminal import render_tip_surveillance
                text = render_tip_surveillance(sender_name, recipient_name, amount, fee)
                text = await self._generate_text(
                    f"Client {sender_name} transferred {amount} JC to {recipient_name}. Fee: {fee} JC",
                    ctx, text,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)

            # Layer 1: One-liner (20%)
            if self._roll(0.20):
                from utils.neon_terminal import render_tip
                text = render_tip(sender_name, recipient_name, amount)
                text = await self._generate_text(
                    f"Client {sender_name} tipped {amount} JC to {recipient_name}",
                    ctx, text,
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

            name = self._get_player_name(discord_id, guild_id)
            ctx = self._build_player_context(discord_id, guild_id)

            if won:
                # Layer 1: Always fire on win (100%)
                text = render_don_win(name, final_balance)
                text = await self._generate_text(
                    f"Client won Double or Nothing. Balance: {final_balance} JC",
                    ctx, text,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=1, text_block=text)

            # Loss path

            # Layer 3: Large loss (>100 JC at risk) - coin flip GIF
            if balance_at_risk > 100:
                try:
                    from utils.neon_drawing import create_don_coin_flip_gif
                    gif = create_don_coin_flip_gif(name, balance_at_risk)
                    text = render_don_loss_box(name, balance_at_risk)
                    text = await self._generate_text(
                        f"Client lost {balance_at_risk} JC in Double or Nothing. Balance: 0",
                        ctx, text,
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
                    ctx, text,
                )
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)

            # Layer 1: Any loss (100%)
            text = render_don_lose(name, balance_at_risk)
            text = await self._generate_text(
                f"Client lost {balance_at_risk} JC in Double or Nothing",
                ctx, text,
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

            winner_name = self._get_player_name(winner_id, guild_id)
            loser_name = self._get_player_name(loser_id, guild_id)
            text = render_coinflip(winner_name, loser_name)
            text = await self._generate_text(
                f"Draft coinflip: {winner_name} won, {loser_name} lost",
                {"winner": winner_name, "loser": loser_name}, text,
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
            if not self._check_one_time(discord_id, guild_id, "registration"):
                return None
            if not self._roll(0.50):
                return None

            text = render_registration(player_name)
            text = await self._generate_text(
                f"New player '{player_name}' just registered. 3 JC starting balance.",
                {"name": player_name}, text,
            )
            self._mark_one_time(discord_id, guild_id, "registration", layer=1)
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

            # Layer 3: Massive pool (>=500 JC) - market crash GIF
            if total_pool >= 500:
                try:
                    from utils.neon_drawing import create_market_crash_gif
                    gif = create_market_crash_gif(total_pool, outcome, winner_count, loser_count)
                    text = render_prediction_market_crash(
                        question, total_pool, outcome, winner_count, loser_count
                    )
                    text = await self._generate_text(event_desc, pred_ctx, text)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Market crash GIF failed: {e}")
                    # Fall through to Layer 2

            # Layer 2: Large pool (>=200 JC) at 70%
            if total_pool >= 200 and self._roll(0.70):
                text = render_prediction_market_crash(
                    question, total_pool, outcome, winner_count, loser_count
                )
                text = await self._generate_text(event_desc, pred_ctx, text)
                return NeonResult(layer=2, text_block=text)

            # Layer 1: Any resolution at 30%
            if not self._roll(0.30):
                return None

            text = render_prediction_resolved(question, outcome, total_pool)
            text = await self._generate_text(event_desc, pred_ctx, text)
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
                text = await self._generate_text(event_desc, {}, text, anonymous=True)
                self._set_cooldown(discord_id, guild_id)
                return NeonResult(layer=2, text_block=text)

            # Layer 1: One-liner (25%)
            if self._roll(0.25):
                text = render_soft_avoid(cost, games)
                text = await self._generate_text(event_desc, {}, text, anonymous=True)
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

            name = self._get_player_name(discord_id, guild_id)
            text = render_all_in_bet(name, amount, percentage)
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} went ALL-IN with {amount} JC ({percentage:.0f}% of balance)",
                ctx, text,
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

            name = self._get_player_name(discord_id, guild_id)
            text = render_last_second_bet(name, seconds_remaining)
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} placed bet with only {seconds_remaining}s remaining",
                ctx, text,
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
        """Trigger on bomb pot event. Layer 3 GIF at 50%."""
        try:
            if not self._is_enabled():
                return None
            if not self._roll(0.50):
                return None

            # Layer 3: Bomb pot GIF
            try:
                from utils.neon_drawing import create_bomb_pot_gif
                gif = create_bomb_pot_gif(pool_amount, contributor_count)
                text = render_bomb_pot(pool_amount, contributor_count)
                return NeonResult(layer=3, text_block=text, gif_file=gif)
            except Exception as e:
                logger.debug(f"Bomb pot GIF failed: {e}")
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

            name = self._get_player_name(discord_id, guild_id)
            text = render_lobby_join(name, queue_position)
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} joined the queue at position {queue_position}",
                ctx, text,
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

            player1_name = self._get_player_name(player1_id, guild_id)
            player2_name = self._get_player_name(player2_id, guild_id)
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

            name = self._get_player_name(discord_id, guild_id)

            # Layer 3: 100+ games gets special treatment
            if total_games >= 100:
                try:
                    from utils.neon_drawing import create_degen_certificate_gif
                    # Use degen certificate style but for games milestone
                    gif = create_degen_certificate_gif(name, total_games)
                    text = render_games_milestone(name, total_games)
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Games milestone GIF failed: {e}")

            # Layer 2: Standard milestone box
            text = render_games_milestone(name, total_games)
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} has played {total_games} games",
                ctx, text,
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
            if not self._roll(0.50):
                return None

            name = self._get_player_name(discord_id, guild_id)

            # Layer 3: 8+ streak gets GIF
            if current_streak >= 8:
                try:
                    from utils.neon_drawing import create_streak_record_gif
                    gif = create_streak_record_gif(name, current_streak)
                    text = render_win_streak_record(name, current_streak)
                    self._set_cooldown(discord_id, guild_id)
                    return NeonResult(layer=3, text_block=text, gif_file=gif)
                except Exception as e:
                    logger.debug(f"Streak record GIF failed: {e}")

            # Layer 2: Standard streak box
            text = render_win_streak_record(name, current_streak)
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} broke their personal win streak record: {current_streak} games",
                ctx, text,
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
            if not self._check_one_time(discord_id, guild_id, "first_leverage"):
                return None
            if not self._roll(0.80):
                return None

            name = self._get_player_name(discord_id, guild_id)
            text = render_first_leverage(name, leverage)
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} used leverage for the first time: {leverage}x",
                ctx, text,
            )
            self._mark_one_time(discord_id, guild_id, "first_leverage", layer=1)
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
            if not self._check_one_time(discord_id, guild_id, "100_bets"):
                return None
            if not self._roll(0.50):
                return None

            name = self._get_player_name(discord_id, guild_id)
            text = render_bets_milestone(name, total_bets)
            ctx = self._build_player_context(discord_id, guild_id)
            text = await self._generate_text(
                f"Client {name} has placed 100 total bets",
                ctx, text,
            )
            self._mark_one_time(discord_id, guild_id, "100_bets", layer=2)
            self._set_cooldown(discord_id, guild_id)
            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_100_bets_milestone error: {e}")
            return None

    async def on_simultaneous_events(
        self,
        guild_id: int | None,
        events: list[str],
    ) -> NeonResult | None:
        """Trigger when multiple gambling events fire at once. Layer 2 at 10%."""
        try:
            if not self._is_enabled():
                return None
            if len(events) < 2:
                return None
            if not self._roll(0.10):
                return None

            text = render_simultaneous_events(len(events), events)
            return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_simultaneous_events error: {e}")
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

            captain1_name = self._get_player_name(captain1_id, guild_id)
            captain2_name = self._get_player_name(captain2_id, guild_id)
            text = render_captain_symmetry(captain1_name, captain2_name, abs(rating_diff))
            return NeonResult(layer=1, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_captain_symmetry error: {e}")
            return None

    async def on_unanimous_wrong(
        self,
        guild_id: int | None,
        consensus_percentage: float,
        winning_side: str,
        loser_count: int,
    ) -> NeonResult | None:
        """Trigger when 90%+ consensus prediction loses. Layer 3 GIF at 50%."""
        try:
            if not self._is_enabled():
                return None
            if consensus_percentage < 90:
                return None
            if not self._roll(0.50):
                return None

            # Layer 3: Market crash GIF
            try:
                from utils.neon_drawing import create_unanimous_wrong_gif
                gif = create_unanimous_wrong_gif(consensus_percentage, winning_side, loser_count)
                text = render_unanimous_wrong(consensus_percentage, winning_side, loser_count)
                return NeonResult(layer=3, text_block=text, gif_file=gif)
            except Exception as e:
                logger.debug(f"Unanimous wrong GIF failed: {e}")
                # Fall back to text only
                text = render_unanimous_wrong(consensus_percentage, winning_side, loser_count)
                return NeonResult(layer=2, text_block=text)
        except Exception as e:
            logger.debug(f"neon on_unanimous_wrong error: {e}")
            return None
