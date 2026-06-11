"""Flavor narration for the Daily Mafia subgame.

Currently uses static templates. The `ai_service` hook is reserved for a future
upgrade to AI-generated narration; for now the service always falls back to
templates so behavior is deterministic and offline-safe.
"""

import random

from domain.models.mafia import MafiaGame, MafiaPlayer, MafiaRole, MafiaTwist, MafiaWinner

_SETUP_TEMPLATES = [
    "The lobby falls silent as suspicion settles over the night.",
    "Somewhere in this roster, knives are being sharpened.",
    "It's another day in the inhouse — and someone here is not who they claim to be.",
    "The sun sets on the gamba channel. Trust no one until dawn.",
    "Rumors of a plot have reached the village. Eyes narrow.",
    "A new game begins. May the most paranoid town survive.",
    "Tonight, the courier delivers more than wards.",
    "The captain's draft has nothing on this betrayal.",
    "The match is on. The mafia are already among you.",
    "Mid lane is quiet tonight. Too quiet.",
]

_TWIST_FLAVOR = {
    MafiaTwist.BLOOD_MOON: "🌑 A blood moon rises. The mafia hungers for two souls tonight.",
    MafiaTwist.TOWN_HALL: "🏛️ Town hall is in session. No lynch will be permitted today.",
    MafiaTwist.MEMORY_FOG: "🌫️ A thick fog rolls in. The detective's notes will fail them tonight.",
    MafiaTwist.PLAGUE: "☠️ A plague stalks the village. An additional life will be claimed at dawn.",
}

_DEATH_TEMPLATES = [
    "{hero} ({name}) was found cold at first light. They were a {role}.",
    "{name} ({hero}) didn't make it through the night. {role} confirmed.",
    "The mafia struck. {name}, who carried {hero}, lies dead — a {role}.",
    "{hero} could not save them. {name} ({role}) is no more.",
    "Dawn reveals {name}'s body. Their role: {role}. Their hero: {hero}.",
    "A scream in the dark. A {role} named {name} ({hero}) has fallen.",
    "{name} ({hero}) was hooked into the void. The {role} is gone.",
    "Initiate, ult, dead. {name} ({hero}) — a {role} — has been removed from the game.",
    "{name} got Pudge'd in their sleep. A {role} ({hero}) lost.",
    "{hero} could not buy back from this one. {name} ({role}) is dead.",
]

_PLAGUE_DEATH_TEMPLATES = [
    "The plague claimed {name} ({hero}) before sunrise. A {role} extinguished.",
    "{name} ({hero}) died of fever, not steel. They were a {role}.",
    "Even the doctor could not save {name} ({hero}) from the plague. A {role} lost.",
]

_LYNCH_TEMPLATES = [
    "By a show of hands, {name} ({hero}) was hanged from the rax. Their role: {role}.",
    "The town has spoken. {name} ({hero}), a {role}, is executed.",
    "{name} ({hero}) was sent to the well. Confirmed: {role}.",
    "Cleaved at high noon. {name} ({hero}) — {role} — has been lynched.",
    "{name} dies trying to claim innocence. {role} ({hero}) was the truth.",
]

_NO_LYNCH_TEMPLATES = [
    "The town could not agree. No one is hanged today.",
    "The vote is tied. The rax stays empty.",
    "Cold feet at the well. No execution today.",
    "Town hall recesses without a verdict.",
]

_TOWN_WIN_TEMPLATES = [
    "🏆 The town stands victorious. The mafia have been ground out of mid.",
    "🏆 Roshan is denied. The town wins this one.",
    "🏆 GG: the town has read every gank. The mafia fall.",
    "🏆 Buyback exhausted. The town wins.",
    "🏆 The town's wards held. Every mafia is dead.",
]

_MAFIA_WIN_TEMPLATES = [
    "🔪 The mafia have wiped the town. Victory in the dark.",
    "🔪 Throne destroyed. The mafia win.",
    "🔪 The town never stood a chance. Mafia takes the game.",
    "🔪 GG WP — the mafia outnumber the living.",
    "🔪 The mafia walk away from a smoking village. They win.",
]

_JESTER_WIN_TEMPLATES = [
    "🃏 The Jester laughs from the grave. They wanted to be lynched — and you obliged.",
    "🃏 You played yourselves. The Jester wins.",
    "🃏 A clown's victory. The Jester gets the last laugh.",
]

_NONE_WIN_TEMPLATES = [
    "The dust settles with no clear victor. The game ends in silence.",
    "Neither side could close. The day ends inconclusive.",
]


class MafiaFlavorService:
    """Produces narration text for Mafia events.

    Always returns a usable string. If `ai_service` is set, future upgrades may
    attempt AI generation first; today this service always uses static templates.
    """

    def __init__(self, ai_service=None, flavor_text_service=None, rng: random.Random | None = None):
        self.ai_service = ai_service
        self.flavor_text_service = flavor_text_service
        self._rng = rng or random

    async def setup_narration(self, game: MafiaGame) -> str:
        base = self._rng.choice(_SETUP_TEMPLATES)
        if game.twist_event:
            twist_line = _TWIST_FLAVOR.get(game.twist_event, "")
            if twist_line:
                return f"{base}\n{twist_line}"
        return base

    async def death_narration(
        self, victim: MafiaPlayer, *, by_plague: bool = False
    ) -> str:
        templates = _PLAGUE_DEATH_TEMPLATES if by_plague else _DEATH_TEMPLATES
        return self._rng.choice(templates).format(
            hero=victim.hero_name or "an unknown hero",
            name=f"<@{victim.discord_id}>",
            role=_role_label(victim.role),
        )

    async def lynch_narration(self, victim: MafiaPlayer) -> str:
        return self._rng.choice(_LYNCH_TEMPLATES).format(
            hero=victim.hero_name or "an unknown hero",
            name=f"<@{victim.discord_id}>",
            role=_role_label(victim.role),
        )

    async def no_lynch_narration(self) -> str:
        return self._rng.choice(_NO_LYNCH_TEMPLATES)

    async def resolution_narration(self, winner: MafiaWinner) -> str:
        if winner == MafiaWinner.TOWN:
            return self._rng.choice(_TOWN_WIN_TEMPLATES)
        if winner == MafiaWinner.MAFIA:
            return self._rng.choice(_MAFIA_WIN_TEMPLATES)
        if winner == MafiaWinner.JESTER:
            return self._rng.choice(_JESTER_WIN_TEMPLATES)
        return self._rng.choice(_NONE_WIN_TEMPLATES)


def _role_label(role: MafiaRole) -> str:
    return {
        MafiaRole.MAFIA: "Mafia",
        MafiaRole.DOCTOR: "Doctor",
        MafiaRole.DETECTIVE: "Detective",
        MafiaRole.VIGILANTE: "Vigilante",
        MafiaRole.TOWNIE: "Townie",
        MafiaRole.JESTER: "Jester",
    }.get(role, role.value)
