"""Tunnel name generation for the dig minigame.

Picks a random tunnel name from one of three pools (adjective+noun, title,
silly). No state, no DB access — pure RNG over name tables in
``dig_constants``.
"""

from __future__ import annotations

import random

from services.dig_constants import (
    TUNNEL_NAME_ADJECTIVES,
    TUNNEL_NAME_NOUNS,
    TUNNEL_NAME_SILLY,
    TUNNEL_NAME_TITLES,
)


class DigTunnelNamingService:
    """Random tunnel-name picker."""

    def generate_tunnel_name(self) -> str:
        """Random name from 3 pool types (40% adj+noun, 35% title, 25% silly)."""
        roll = random.random()
        if roll < 0.40:
            adj = random.choice(TUNNEL_NAME_ADJECTIVES)
            noun = random.choice(TUNNEL_NAME_NOUNS)
            return f"The {adj} {noun}"
        elif roll < 0.75:
            return random.choice(TUNNEL_NAME_TITLES)
        else:
            return random.choice(TUNNEL_NAME_SILLY)
