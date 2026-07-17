"""Fail-soft herald narration for duel challenge lifecycle events."""

import asyncio
import random
import unicodedata
from enum import Enum
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from repositories.interfaces import IGuildConfigRepository
    from services.ai_service import AIService


SYSTEM_PROMPT = (
    "You are a grim medieval tournament herald announcing a challenge of honor "
    "in a Dota inhouse league. Write original courtly fanfare with sharp humor, "
    "jousting imagery, and concise Dota references. Do not quote or imitate any "
    "existing novel or television dialogue. Never invent or alter players, wagers, "
    "deadlines, trial types, or outcomes. Produce one line under 300 characters and "
    "never include mentions."
)


class DuelFlavorEvent(str, Enum):
    """Duel lifecycle moments that can receive herald narration."""

    ISSUED = "issued"
    REMINDER = "reminder"
    ACCEPTED_COMBAT = "accepted_combat"
    ACCEPTED_FIVE = "accepted_five"
    DECLINED = "declined"
    EXPIRED = "expired"
    RESOLVED = "resolved"
    VOIDED = "voided"


FALLBACKS: dict[DuelFlavorEvent, tuple[str, ...]] = {
    DuelFlavorEvent.ISSUED: (
        "📯 The lists are opened; a gauntlet lands harder than a missed last hit.",
    ),
    DuelFlavorEvent.REMINDER: (
        "⏳ The herald tolls again: answer the challenge before the courier grows old.",
    ),
    DuelFlavorEvent.ACCEPTED_COMBAT: (
        "⚔️ Helms lower and lances rise; this quarrel will be settled in combat.",
    ),
    DuelFlavorEvent.ACCEPTED_FIVE: (
        "🛡️ Five banners gather on each side; the lanes shall judge this grand dispute.",
    ),
    DuelFlavorEvent.DECLINED: (
        "🏳️ The gauntlet is returned; the court records a strategic retreat.",
    ),
    DuelFlavorEvent.EXPIRED: (
        "⌛ The challenge fades unanswered, like a smoke breaking before the gank.",
    ),
    DuelFlavorEvent.RESOLVED: (
        "🏆 The dust settles and the herald seals the victor's name in the ledger.",
    ),
    DuelFlavorEvent.VOIDED: (
        "📜 The marshal strikes the challenge from the rolls; no lance shall fall today.",
    ),
}


class DuelFlavorService:
    """Generate optional AI herald lines with static fallbacks for every event."""

    def __init__(
        self,
        ai_service: "AIService | None",
        guild_config_repo: "IGuildConfigRepository | None",
        rng: random.Random | None = None,
    ) -> None:
        self.ai_service = ai_service
        self.guild_config_repo = guild_config_repo
        self._rng = rng or random

    async def generate(
        self,
        event: DuelFlavorEvent,
        guild_id: int,
        details: dict[str, Any],
    ) -> str:
        """Return a short, mention-safe herald line without affecting duel state."""
        def fallback() -> str:
            return self._rng.choice(FALLBACKS[event])

        if self.ai_service is None:
            return fallback()

        try:
            if self.guild_config_repo is not None:
                ai_enabled = await asyncio.to_thread(
                    self.guild_config_repo.get_ai_enabled, guild_id
                )
                if not ai_enabled:
                    return fallback()

            cleaned_details = {
                key: _sanitize_detail(value) for key, value in details.items()
            }
            prompt = f"Event: {event.value}. Details: {cleaned_details}."
            generated = await self.ai_service.complete(
                prompt,
                system_prompt=SYSTEM_PROMPT,
            )
        except Exception:
            return fallback()

        cleaned_output = _sanitize_output(generated)
        return cleaned_output or fallback()


def _sanitize_detail(value: Any) -> str:
    cleaned = "".join(
        character
        for character in str(value)
        if character not in "<>@`" and unicodedata.category(character) != "Cc"
    )
    return cleaned[:80]


def _sanitize_output(value: str | None) -> str:
    if not value:
        return ""
    return " ".join(value.split()).replace("@", "＠")[:300]
