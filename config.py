"""
Centralized configuration for the Cama Balanced Shuffle bot.
"""

from __future__ import annotations

import os
from typing import List, Dict, Any

from dotenv import load_dotenv

load_dotenv()


def _parse_int(env_var: str, default: int) -> int:
    raw = os.getenv(env_var)
    if raw is None:
        return default
    try:
        return int(raw)
    except ValueError:
        return default


def _parse_float(env_var: str, default: float) -> float:
    raw = os.getenv(env_var)
    if raw is None:
        return default
    try:
        return float(raw)
    except ValueError:
        return default


def _parse_bool(env_var: str, default: bool) -> bool:
    raw = os.getenv(env_var)
    if raw is None:
        return default
    return raw.lower() in {"1", "true", "yes", "on"}


DB_PATH = os.getenv("DB_PATH", "cama_shuffle.db")
DISCORD_BOT_TOKEN = os.getenv("DISCORD_BOT_TOKEN")
ADMIN_USER_IDS: List[int] = []

_admin_env = os.getenv("ADMIN_USER_IDS", "")
if _admin_env:
    try:
        ADMIN_USER_IDS = [int(uid.strip()) for uid in _admin_env.split(",") if uid.strip()]
    except ValueError:
        ADMIN_USER_IDS = []

LOBBY_READY_THRESHOLD = _parse_int("LOBBY_READY_THRESHOLD", 10)
LOBBY_MAX_PLAYERS = _parse_int("LOBBY_MAX_PLAYERS", 12)
# Legacy: Not used in current balancing algorithm (replaced by Glicko-2 ratings)
WIN_LOSS_MULTIPLIER = _parse_int("WIN_LOSS_MULTIPLIER", 200)
# Legacy: Not used in current balancing algorithm
MMR_WEIGHT = _parse_float("MMR_WEIGHT", 1.0)
USE_GLICKO = _parse_bool("USE_GLICKO", True)

SHUFFLER_SETTINGS: Dict[str, Any] = {
    "role_penalty_weight": _parse_float("ROLE_PENALTY_WEIGHT", 0.1),
    "off_role_multiplier": _parse_float("OFF_ROLE_MULTIPLIER", 0.95),
    "off_role_flat_penalty": _parse_float("OFF_ROLE_FLAT_PENALTY", 100.0),
    "role_matchup_delta_weight": _parse_float("ROLE_MATCHUP_DELTA_WEIGHT", 0.5),
    "exclusion_penalty_weight": _parse_float("EXCLUSION_PENALTY_WEIGHT", 5.0),
}

JOPACOIN_PER_GAME = _parse_int("JOPACOIN_PER_GAME", 1)
JOPACOIN_MIN_BET = _parse_int("JOPACOIN_MIN_BET", 1)
JOPACOIN_WIN_REWARD = _parse_int("JOPACOIN_WIN_REWARD", 2)
BET_LOCK_SECONDS = _parse_int("BET_LOCK_SECONDS", 600)
HOUSE_PAYOUT_MULTIPLIER = _parse_float("HOUSE_PAYOUT_MULTIPLIER", 1.0)


