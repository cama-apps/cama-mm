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
    "You are the herald of a challenge of honor in a Dota inhouse league, "
    "announcing from a world of hedge knights in the spirit of the Tales of "
    "Dunk and Egg: muddy tourney meadows, mystery knights, trials of seven, "
    "and armor won on a wager — crossed with concise Dota references. Write "
    "original courtly fanfare with sharp humor."
)

PROMPT_CONSTRAINTS = (
    "Do not quote or imitate any existing novel or television dialogue. "
    "Never invent or alter players, wagers, deadlines, trial types, or "
    "outcomes. Produce one line under 300 characters and never include "
    "mentions."
)

HERALD_VOICES: tuple[str, ...] = (
    "Speak as a towering hedge knight errant: plainspoken, dutiful, short of "
    "coin, weighing every quarrel against the price of a new shield.",
    "Speak as a shaven-headed squire far too clever for his station: dry, "
    "precise, quietly needling both duelists about their lane equilibrium.",
    "Speak as the master of the games at a muddy tourney meadow: officious "
    "and long-suffering, reading the lists while the smallfolk heckle item "
    "builds.",
    "Speak as a mystery knight in mismatched armor: theatrical and anonymous, "
    "hinting the feud will be settled where the river runes spawn.",
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
    UNRESOLVED = "unresolved"


FALLBACKS: dict[DuelFlavorEvent, tuple[str, ...]] = {
    DuelFlavorEvent.ISSUED: (
        "📯 A gauntlet slaps the mud of the tourney meadow, and even the couriers stop to gawk.",
        "📯 A shield is hung upon the lists; let the smallfolk gather and the wards go out.",
    ),
    DuelFlavorEvent.REMINDER: (
        "⏳ The herald calls across the meadow: answer, ser, before your courier grows a beard.",
        "⏳ The lists still wait on your word; a hedge knight would have answered by now.",
    ),
    DuelFlavorEvent.ACCEPTED_COMBAT: (
        "⚔️ Lances lower in the mid lane; this quarrel will be settled before gods, men, and observer wards.",
        "⚔️ Trial by combat is sworn: one lane, two knights, and a river rune between them.",
    ),
    DuelFlavorEvent.ACCEPTED_FIVE: (
        "🛡️ Five banners to a side, near enough a trial of seven for this muddy meadow.",
        "🛡️ The trial of five is sworn; may the drafting gods show mercy on both retinues.",
    ),
    DuelFlavorEvent.DECLINED: (
        "🏳️ The gauntlet is returned with its seal unbroken; the meadow boos politely.",
        "🏳️ The shield comes down without a tilt, and the rolls record a careful knight.",
    ),
    DuelFlavorEvent.EXPIRED: (
        "⌛ The challenge rusts on the lists like a puddle-painted shield left in the rain.",
        "⌛ No answer came; the herald strikes the tilt from the rolls at first light.",
    ),
    DuelFlavorEvent.RESOLVED: (
        "🏆 The tilt is run; the victor's name goes in the rolls and the pot rides home in his saddlebags.",
        "🏆 The commons cheer, the loser mutters of buybacks, and the ledger closes another quarrel.",
    ),
    DuelFlavorEvent.VOIDED: (
        "📜 The marshal voids the tilt; both purses ride home unbloodied.",
        "📜 No verdict and no victor; the stakes walk home like a courier spared.",
    ),
    DuelFlavorEvent.UNRESOLVED: (
        "🗡️ The meadow still waits: two knights sworn to a tilt that no one has ridden.",
        "🗡️ The rolls show an unfinished trial; even the wards have expired waiting.",
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
            voice = self._rng.choice(HERALD_VOICES)
            generated = await self.ai_service.complete(
                prompt,
                system_prompt=f"{SYSTEM_PROMPT} {voice} {PROMPT_CONSTRAINTS}",
                feature=f"duel.{event.value}",
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
