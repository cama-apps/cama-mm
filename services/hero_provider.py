"""Provides Dota hero names for Mafia subgame flavor."""

import random

from utils.hero_lookup import _HEROES, _load_heroes


class HeroProvider:
    """Samples unique Dota hero names for player flavor in a Mafia game."""

    def __init__(self, rng: random.Random | None = None):
        self._rng = rng or random

    def all_hero_names(self) -> list[str]:
        _load_heroes()
        return list(_HEROES.values())

    def sample_unique(self, n: int) -> list[str]:
        """Return `n` hero names. Unique if pool is large enough; else samples with replacement."""
        names = self.all_hero_names()
        if not names:
            return [f"Hero {i + 1}" for i in range(n)]
        if n <= len(names):
            return self._rng.sample(names, n)
        return self._rng.choices(names, k=n)
