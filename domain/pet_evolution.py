"""Pure rules for Camagotchi upbringing and adult evolution."""

from __future__ import annotations

import hashlib
from collections.abc import Mapping
from dataclasses import dataclass
from enum import StrEnum


class PetInstinct(StrEnum):
    DELVING = "delving"
    COMPETITION = "competition"
    FORTUNE = "fortune"
    FELLOWSHIP = "fellowship"
    WISDOM = "wisdom"


class PetActivity(StrEnum):
    DIG_COMPLETED = "dig_completed"
    MATCH_RECORDED = "match_recorded"
    PET_BRAWL_COMPLETED = "pet_brawl_completed"
    DUEL_COMPLETED = "duel_completed"
    WHEEL_SPUN = "wheel_spun"
    BET_PLACED = "bet_placed"
    PREDICTION_TRADED = "prediction_traded"
    PET_CARED = "pet_cared"
    TIP_SENT = "tip_sent"
    TRIVIA_COMPLETED = "trivia_completed"


class PetCalling(StrEnum):
    DELVER = "delver"
    CHAMPION = "champion"
    TRICKSTER = "trickster"
    COMPANION = "companion"
    SAGE = "sage"
    PITFIGHTER = "pitfighter"
    PROSPECTOR = "prospector"
    TRAILKEEPER = "trailkeeper"
    ARCHAEOLOGIST = "archaeologist"
    DAREDEVIL = "daredevil"
    GUARDIAN = "guardian"
    TACTICIAN = "tactician"
    CHARMER = "charmer"
    ORACLE = "oracle"
    MENTOR = "mentor"
    WAYFARER = "wayfarer"


@dataclass(frozen=True, slots=True)
class ActivityRule:
    instinct: PetInstinct
    points: int = 2
    once_per_day: bool = False


@dataclass(frozen=True, slots=True)
class EvolutionDecision:
    calling: PetCalling
    primary: PetInstinct | None
    secondary: PetInstinct | None


@dataclass(frozen=True, slots=True)
class CallingProfile:
    label: str
    emoji: str
    blurb: str
    color: int


CALLING_PROFILES = {
    PetCalling.DELVER: CallingProfile(
        "Delver",
        "⛏️",
        "Hears hidden passages in every patch of stone.",
        0xB87333,
    ),
    PetCalling.CHAMPION: CallingProfile(
        "Champion",
        "🏆",
        "Greets every challenge with bright eyes and planted hooves.",
        0xD94A4A,
    ),
    PetCalling.TRICKSTER: CallingProfile(
        "Trickster",
        "🎲",
        "Treats chance as a dance partner and somehow knows the steps.",
        0xE3B341,
    ),
    PetCalling.COMPANION: CallingProfile(
        "Companion",
        "💞",
        "Keeps close to the herd and makes every shared road warmer.",
        0xE77FA1,
    ),
    PetCalling.SAGE: CallingProfile(
        "Sage",
        "📚",
        "Collects questions carefully and answers them at inconvenient moments.",
        0x8067C6,
    ),
    PetCalling.PITFIGHTER: CallingProfile(
        "Pitfighter",
        "⚔️",
        "Found courage underground and brought it back sharpened.",
        0xC45A36,
    ),
    PetCalling.PROSPECTOR: CallingProfile(
        "Prospector",
        "✨",
        "Follows glitter, echoes, and extremely persuasive hunches.",
        0xD69A32,
    ),
    PetCalling.TRAILKEEPER: CallingProfile(
        "Trailkeeper",
        "🏕️",
        "Leaves every dark road safer for the herd behind.",
        0x8D8650,
    ),
    PetCalling.ARCHAEOLOGIST: CallingProfile(
        "Archaeologist",
        "🏺",
        "Digs patiently for stories the stone nearly forgot.",
        0x9B6D54,
    ),
    PetCalling.DAREDEVIL: CallingProfile(
        "Daredevil",
        "🔥",
        "Believes the safest odds are the ones already sprinting away.",
        0xE05A3F,
    ),
    PetCalling.GUARDIAN: CallingProfile(
        "Guardian",
        "🛡️",
        "Turns fierce the instant a friend needs shelter.",
        0xC35B70,
    ),
    PetCalling.TACTICIAN: CallingProfile(
        "Tactician",
        "♟️",
        "Has considered the next move, and several snacks beyond it.",
        0x8A5EA8,
    ),
    PetCalling.CHARMER: CallingProfile(
        "Charmer",
        "🌟",
        "Makes lucky breaks feel suspiciously like introductions.",
        0xE29A70,
    ),
    PetCalling.ORACLE: CallingProfile(
        "Oracle",
        "🔮",
        "Reads tomorrow in patterns everyone else calls coincidence.",
        0x9A70C8,
    ),
    PetCalling.MENTOR: CallingProfile(
        "Mentor",
        "🪶",
        "Always has a lesson, a listening ear, and emergency hay.",
        0x9A76B6,
    ),
    PetCalling.WAYFARER: CallingProfile(
        "Wayfarer",
        "🧭",
        "Carries a little of every path and belongs wherever it wanders.",
        0x5DADE2,
    ),
}

INSTINCT_LABELS = {
    PetInstinct.DELVING: "Delving",
    PetInstinct.COMPETITION: "Competition",
    PetInstinct.FORTUNE: "Fortune",
    PetInstinct.FELLOWSHIP: "Fellowship",
    PetInstinct.WISDOM: "Wisdom",
}

_HINT_TEXT = {
    "balanced": "Every path seems to hold a little wonder.",
    "delving": "Loose stones keep attracting curious little sniffs.",
    "delving_strong": "The deep has started appearing in very serious dreams.",
    "competition": "Any friendly challenge earns an immediate, eager stance.",
    "competition_strong": "Every room looks like an arena waiting to happen.",
    "fortune": "Coincidence keeps getting an expectant little side-eye.",
    "fortune_strong": "Lucky omens are being collected with alarming confidence.",
    "fellowship": "The herd is never allowed to wander too far away.",
    "fellowship_strong": "Looking after friends has become a full-time vocation.",
    "wisdom": "Every new fact receives a thoughtful, woolly pause.",
    "wisdom_strong": "Questions are piling up faster than the snack supply.",
}


def calling_profile(calling: PetCalling | str) -> CallingProfile:
    return CALLING_PROFILES[PetCalling(calling)]


def instinct_label(instinct: PetInstinct | str) -> str:
    return INSTINCT_LABELS[PetInstinct(instinct)]


def hint_text(key: str | None) -> str | None:
    if key is None:
        return None
    return _HINT_TEXT.get(key)


_ACTIVITY_RULES = {
    PetActivity.DIG_COMPLETED: ActivityRule(PetInstinct.DELVING),
    PetActivity.MATCH_RECORDED: ActivityRule(PetInstinct.COMPETITION),
    PetActivity.PET_BRAWL_COMPLETED: ActivityRule(PetInstinct.COMPETITION),
    PetActivity.DUEL_COMPLETED: ActivityRule(PetInstinct.COMPETITION),
    PetActivity.WHEEL_SPUN: ActivityRule(PetInstinct.FORTUNE),
    PetActivity.BET_PLACED: ActivityRule(PetInstinct.FORTUNE),
    PetActivity.PREDICTION_TRADED: ActivityRule(PetInstinct.FORTUNE),
    PetActivity.PET_CARED: ActivityRule(
        PetInstinct.FELLOWSHIP,
        points=1,
        once_per_day=True,
    ),
    PetActivity.TIP_SENT: ActivityRule(PetInstinct.FELLOWSHIP),
    PetActivity.TRIVIA_COMPLETED: ActivityRule(PetInstinct.WISDOM),
}

_PURE_CALLINGS = {
    PetInstinct.DELVING: PetCalling.DELVER,
    PetInstinct.COMPETITION: PetCalling.CHAMPION,
    PetInstinct.FORTUNE: PetCalling.TRICKSTER,
    PetInstinct.FELLOWSHIP: PetCalling.COMPANION,
    PetInstinct.WISDOM: PetCalling.SAGE,
}

_HYBRID_CALLINGS = {
    frozenset((PetInstinct.DELVING, PetInstinct.COMPETITION)): PetCalling.PITFIGHTER,
    frozenset((PetInstinct.DELVING, PetInstinct.FORTUNE)): PetCalling.PROSPECTOR,
    frozenset((PetInstinct.DELVING, PetInstinct.FELLOWSHIP)): PetCalling.TRAILKEEPER,
    frozenset((PetInstinct.DELVING, PetInstinct.WISDOM)): PetCalling.ARCHAEOLOGIST,
    frozenset((PetInstinct.COMPETITION, PetInstinct.FORTUNE)): PetCalling.DAREDEVIL,
    frozenset((PetInstinct.COMPETITION, PetInstinct.FELLOWSHIP)): PetCalling.GUARDIAN,
    frozenset((PetInstinct.COMPETITION, PetInstinct.WISDOM)): PetCalling.TACTICIAN,
    frozenset((PetInstinct.FORTUNE, PetInstinct.FELLOWSHIP)): PetCalling.CHARMER,
    frozenset((PetInstinct.FORTUNE, PetInstinct.WISDOM)): PetCalling.ORACLE,
    frozenset((PetInstinct.FELLOWSHIP, PetInstinct.WISDOM)): PetCalling.MENTOR,
}

_MIN_MEANINGFUL_SCORE = 3


def activity_rule(activity: PetActivity) -> ActivityRule:
    return _ACTIVITY_RULES[PetActivity(activity)]


def _score(scores: Mapping[PetInstinct, int], instinct: PetInstinct) -> int:
    return max(0, int(scores.get(instinct, 0)))


def _tie_rank(pet_id: int, instinct: PetInstinct) -> bytes:
    return hashlib.sha256(f"{int(pet_id)}:{instinct.value}".encode()).digest()


def _ranked_instincts(
    scores: Mapping[PetInstinct, int],
    pet_id: int,
) -> list[tuple[PetInstinct, int]]:
    return sorted(
        ((instinct, _score(scores, instinct)) for instinct in PetInstinct),
        key=lambda item: (-item[1], _tie_rank(pet_id, item[0])),
    )


def _is_balanced(top: int, third: int) -> bool:
    return third >= _MIN_MEANINGFUL_SCORE and third * 4 >= top * 3


def resolve_evolution(
    scores: Mapping[PetInstinct, int],
    *,
    pet_id: int,
) -> EvolutionDecision:
    ranked = _ranked_instincts(scores, pet_id)
    (primary, top), (secondary, second), (_, third) = ranked[:3]

    if top < _MIN_MEANINGFUL_SCORE or _is_balanced(top, third):
        return EvolutionDecision(PetCalling.WAYFARER, None, None)

    if second >= _MIN_MEANINGFUL_SCORE and second * 2 >= top:
        return EvolutionDecision(
            _HYBRID_CALLINGS[frozenset((primary, secondary))],
            primary,
            secondary,
        )

    return EvolutionDecision(_PURE_CALLINGS[primary], primary, None)


def hint_key_for_scores(scores: Mapping[PetInstinct, int]) -> str | None:
    ranked = sorted(
        ((instinct, _score(scores, instinct)) for instinct in PetInstinct),
        key=lambda item: (-item[1], item[0].value),
    )
    (primary, top), (_, second), (_, third) = ranked[:3]

    if top < _MIN_MEANINGFUL_SCORE:
        return None
    if _is_balanced(top, third):
        return "balanced"
    if top >= 6 and top - second >= 3:
        return f"{primary.value}_strong"
    return primary.value
