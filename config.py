"""
Centralized configuration for the Cama Balanced Shuffle bot.
"""

from __future__ import annotations

import os
from typing import Any

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


def _parse_int_list(env_var: str, default: list[int]) -> list[int]:
    raw = os.getenv(env_var)
    if raw is None:
        return default
    try:
        return [int(x.strip()) for x in raw.split(",") if x.strip()]
    except ValueError:
        return default


DB_PATH = os.getenv("DB_PATH", "cama_shuffle.db")
DISCORD_BOT_TOKEN = os.getenv("DISCORD_BOT_TOKEN")
ADMIN_USER_IDS: list[int] = []

_admin_env = os.getenv("ADMIN_USER_IDS", "")
if _admin_env:
    try:
        ADMIN_USER_IDS = [int(uid.strip()) for uid in _admin_env.split(",") if uid.strip()]
    except ValueError:
        ADMIN_USER_IDS = []

LOBBY_READY_THRESHOLD = _parse_int("LOBBY_READY_THRESHOLD", 10)
LOBBY_MAX_PLAYERS = _parse_int("LOBBY_MAX_PLAYERS", 20)
LOBBY_RALLY_COOLDOWN_SECONDS = _parse_int("LOBBY_RALLY_COOLDOWN_SECONDS", 120)  # 2 minutes

# Dedicated lobby channel - if set, lobby embeds are posted here instead of command channel
LOBBY_CHANNEL_ID: int | None = None
_lobby_channel_raw = os.getenv("LOBBY_CHANNEL_ID")
if _lobby_channel_raw:
    try:
        LOBBY_CHANNEL_ID = int(_lobby_channel_raw.strip())
    except ValueError:
        LOBBY_CHANNEL_ID = None
# Legacy: Not used in current balancing algorithm (replaced by Glicko-2 ratings)
WIN_LOSS_MULTIPLIER = _parse_int("WIN_LOSS_MULTIPLIER", 200)
# Legacy: Not used in current balancing algorithm
MMR_WEIGHT = _parse_float("MMR_WEIGHT", 1.0)
USE_GLICKO = _parse_bool("USE_GLICKO", True)
OPENSKILL_SHUFFLE_CHANCE = _parse_float("OPENSKILL_SHUFFLE_CHANCE", 0.10)  # 10% chance per shuffle

SHUFFLER_SETTINGS: dict[str, Any] = {
    "off_role_multiplier": _parse_float("OFF_ROLE_MULTIPLIER", 0.95),
    "off_role_flat_penalty": _parse_float("OFF_ROLE_FLAT_PENALTY", 350.0),
    "role_matchup_delta_weight": _parse_float("ROLE_MATCHUP_DELTA_WEIGHT", 0.19),
    "exclusion_penalty_weight": _parse_float("EXCLUSION_PENALTY_WEIGHT", 45.0),
    # Recent match penalty: players who participated in the most recent match
    # get this penalty added to goodness score (making them more likely to sit out)
    # Hardcoded default - not configurable via env var (silent operation)
    "recent_match_penalty_weight": 25.0,
}

NEW_PLAYER_EXCLUSION_BOOST = _parse_int("NEW_PLAYER_EXCLUSION_BOOST", 4)
RD_PRIORITY_WEIGHT = _parse_float("RD_PRIORITY_WEIGHT", 0.1)

JOPACOIN_PER_GAME = _parse_int("JOPACOIN_PER_GAME", 1)
JOPACOIN_MIN_BET = _parse_int("JOPACOIN_MIN_BET", 1)
JOPACOIN_WIN_REWARD = _parse_int("JOPACOIN_WIN_REWARD", 2)
JOPACOIN_EXCLUSION_REWARD = _parse_int("JOPACOIN_EXCLUSION_REWARD", 3)
BET_LOCK_SECONDS = _parse_int("BET_LOCK_SECONDS", 900)  # 15 minutes
HOUSE_PAYOUT_MULTIPLIER = _parse_float("HOUSE_PAYOUT_MULTIPLIER", 1.0)

# Auto-liquidity (blind bets) configuration
AUTO_BLIND_ENABLED = _parse_bool("AUTO_BLIND_ENABLED", True)  # Enable auto-blind bets in pool mode
AUTO_BLIND_THRESHOLD = _parse_int("AUTO_BLIND_THRESHOLD", 50)  # Min balance to trigger blind (inclusive)
AUTO_BLIND_PERCENTAGE = _parse_float("AUTO_BLIND_PERCENTAGE", 0.05)  # 5% of balance

# Bomb Pot configuration (randomly triggered ~20% of matches)
BOMB_POT_CHANCE = _parse_float("BOMB_POT_CHANCE", 0.20)  # 20% chance per match
BOMB_POT_BLIND_PERCENTAGE = _parse_float("BOMB_POT_BLIND_PERCENTAGE", 0.10)  # 10% vs normal 5%
BOMB_POT_ANTE = _parse_int("BOMB_POT_ANTE", 10)  # Flat 10 JC ante (mandatory, can go negative)
BOMB_POT_PARTICIPATION_BONUS = _parse_int("BOMB_POT_PARTICIPATION_BONUS", 1)  # Extra +1 JC for all players

# Leverage betting configuration
LEVERAGE_TIERS = _parse_int_list("LEVERAGE_TIERS", [2, 3, 5])

# Debt configuration
MAX_DEBT = _parse_int("MAX_DEBT", 500)  # Floor: balance can't go below -MAX_DEBT
GARNISHMENT_PERCENTAGE = _parse_float("GARNISHMENT_PERCENTAGE", 1.0)  # 100% of winnings go to debt

# Bankruptcy configuration
BANKRUPTCY_COOLDOWN_SECONDS = _parse_int("BANKRUPTCY_COOLDOWN_SECONDS", 604800)  # 1 week
BANKRUPTCY_PENALTY_GAMES = _parse_int("BANKRUPTCY_PENALTY_GAMES", 5)  # 5 games with reduced rewards
BANKRUPTCY_PENALTY_RATE = _parse_float("BANKRUPTCY_PENALTY_RATE", 0.5)  # 50% of win rewards
BANKRUPTCY_FRESH_START_BALANCE = _parse_int("BANKRUPTCY_FRESH_START_BALANCE", 3)  # Balance after bankruptcy

# Loan configuration
LOAN_COOLDOWN_SECONDS = _parse_int("LOAN_COOLDOWN_SECONDS", 259200)  # 3 days
LOAN_MAX_AMOUNT = _parse_int("LOAN_MAX_AMOUNT", 100)  # Max loan amount
LOAN_FEE_RATE = _parse_float("LOAN_FEE_RATE", 0.20)  # 20% flat fee

# Disbursement configuration
DISBURSE_MIN_FUND = _parse_int("DISBURSE_MIN_FUND", 250)  # Min fund to propose disbursement
DISBURSE_QUORUM_PERCENTAGE = _parse_float("DISBURSE_QUORUM_PERCENTAGE", 0.40)  # 40% of players

# Shop pricing
SHOP_ANNOUNCE_COST = _parse_int("SHOP_ANNOUNCE_COST", 10)
SHOP_ANNOUNCE_TARGET_COST = _parse_int("SHOP_ANNOUNCE_TARGET_COST", 100)
SHOP_PROTECT_HERO_COST = _parse_int("SHOP_PROTECT_HERO_COST", 200)
SHOP_MYSTERY_GIFT_COST = _parse_int("SHOP_MYSTERY_GIFT_COST", 10000)
SHOP_DOUBLE_OR_NOTHING_COST = _parse_int("SHOP_DOUBLE_OR_NOTHING_COST", 50)
DOUBLE_OR_NOTHING_COOLDOWN_SECONDS = _parse_int("DOUBLE_OR_NOTHING_COOLDOWN_SECONDS", 2592000)  # 30 days

# Soft Avoid configuration
SHOP_SOFT_AVOID_COST = _parse_int("SHOP_SOFT_AVOID_COST", 500)  # Cost to soft avoid a player
SOFT_AVOID_GAMES_DURATION = _parse_int("SOFT_AVOID_GAMES_DURATION", 10)  # Number of games avoid lasts
SOFT_AVOID_PENALTY = _parse_float("SOFT_AVOID_PENALTY", 250.0)  # Penalty added to shuffler when pair on same team

# Package Deal configuration
SHOP_PACKAGE_DEAL_BASE_COST = _parse_int("SHOP_PACKAGE_DEAL_BASE_COST", 500)  # Base cost for package deal
SHOP_PACKAGE_DEAL_RATING_DIVISOR = _parse_float("SHOP_PACKAGE_DEAL_RATING_DIVISOR", 10.0)  # Divide sum of ratings by this
PACKAGE_DEAL_GAMES_DURATION = _parse_int("PACKAGE_DEAL_GAMES_DURATION", 10)  # Number of games deal lasts
PACKAGE_DEAL_PENALTY = _parse_float("PACKAGE_DEAL_PENALTY", 200.0)  # Penalty when pair on DIFFERENT teams (lower than soft avoid)

# Wheel of Fortune configuration
WHEEL_COOLDOWN_SECONDS = _parse_int("WHEEL_COOLDOWN_SECONDS", 86400)  # 24 hours
WHEEL_LOSE_PENALTY_COOLDOWN = _parse_int("WHEEL_LOSE_PENALTY_COOLDOWN", 432000)  # 5 days for LOSE
WHEEL_BANKRUPT_PENALTY = _parse_int("WHEEL_BANKRUPT_PENALTY", 100)
WHEEL_MAX_REWARD = _parse_int("WHEEL_MAX_REWARD", 100)
WHEEL_ANIMATION_FRAMES = _parse_int("WHEEL_ANIMATION_FRAMES", 5)  # Number of spin frames
WHEEL_FRAME_DELAY_MS = _parse_int("WHEEL_FRAME_DELAY_MS", 1000)  # Delay between frames (ms)
WHEEL_TARGET_EV = _parse_float("WHEEL_TARGET_EV", -10.0)  # Target expected value per spin

# Estimated EV for special wedges — total economic impact, not just spinner's personal outcome.
# Used to adjust BANKRUPT value so overall wheel drain stays at WHEEL_TARGET_EV.
# RED_SHELL: zero-sum transfer between players, no JC created or destroyed
WHEEL_RED_SHELL_EST_EV = _parse_float("WHEEL_RED_SHELL_EST_EV", 0.0)
# BLUE_SHELL: mostly zero-sum transfer, self-hit (~1/N chance) sends to nonprofit
WHEEL_BLUE_SHELL_EST_EV = _parse_float("WHEEL_BLUE_SHELL_EST_EV", -2.0)
# LIGHTNING_BOLT: taxes ALL positive-balance players 1-3%, all to nonprofit sink
# estimate ~15 active players * ~100 avg balance * ~2% avg tax = ~30 JC destroyed
WHEEL_LIGHTNING_BOLT_EST_EV = _parse_float("WHEEL_LIGHTNING_BOLT_EST_EV", -30.0)

# Lightning Bolt (wheel wedge: server-wide tax to nonprofit)
LIGHTNING_BOLT_PCT_MIN = _parse_float("LIGHTNING_BOLT_PCT_MIN", 0.01)
LIGHTNING_BOLT_PCT_MAX = _parse_float("LIGHTNING_BOLT_PCT_MAX", 0.03)
LIGHTNING_BOLT_MIN_TAX = _parse_int("LIGHTNING_BOLT_MIN_TAX", 1)

# Tip transaction fee (clamped to 0.0 - 0.5 to prevent economy-breaking values)
_raw_tip_fee_rate = _parse_float("TIP_FEE_RATE", 0.01)
TIP_FEE_RATE = max(0.0, min(0.5, _raw_tip_fee_rate))  # 1% default, max 50%

# Steam/Valve API
STEAM_API_KEY = os.getenv("STEAM_API_KEY")

# AI/LLM Configuration (Cerebras via LiteLLM)
CEREBRAS_API_KEY = os.getenv("CEREBRAS_API_KEY")
AI_MODEL = os.getenv("AI_MODEL", "cerebras/zai-glm-4.7")
AI_TIMEOUT_SECONDS = _parse_float("AI_TIMEOUT_SECONDS", 15.0)
AI_MAX_TOKENS = _parse_int("AI_MAX_TOKENS", 500)
AI_RATE_LIMIT_REQUESTS = _parse_int("AI_RATE_LIMIT_REQUESTS", 10)  # Requests per window
AI_RATE_LIMIT_WINDOW = _parse_int("AI_RATE_LIMIT_WINDOW", 60)  # Window in seconds
AI_FEATURES_ENABLED = _parse_bool("AI_FEATURES_ENABLED", False)  # Global default for AI flavor text

# Glicko-2 rating system configuration
CALIBRATION_RD_THRESHOLD = _parse_float("CALIBRATION_RD_THRESHOLD", 100.0)  # Players with RD <= this are considered calibrated
MAX_RATING_SWING_PER_GAME = _parse_float("MAX_RATING_SWING_PER_GAME", 400.0)  # Cap on individual rating change per match
ADMIN_RATING_ADJUSTMENT_MAX_GAMES = _parse_int("ADMIN_RATING_ADJUSTMENT_MAX_GAMES", 50)  # Max games for allowing admin rating adjustments
RD_DECAY_CONSTANT = _parse_float("RD_DECAY_CONSTANT", 50.0)  # Constant for Glicko-2 RD decay formula (c value)
RD_DECAY_GRACE_PERIOD_WEEKS = _parse_int("RD_DECAY_GRACE_PERIOD_WEEKS", 2)  # No decay for first N weeks after last match
MMR_MODAL_TIMEOUT_MINUTES = _parse_int("MMR_MODAL_TIMEOUT_MINUTES", 5)  # Timeout for MMR input modal
MMR_MODAL_RETRY_LIMIT = _parse_int("MMR_MODAL_RETRY_LIMIT", 3)  # Maximum retries for invalid MMR input

# Streak-based rating adjustment configuration
STREAK_THRESHOLD = _parse_int("STREAK_THRESHOLD", 3)  # Min streak length for multiplier (boost applies ON this game)
STREAK_MULTIPLIER_PER_GAME = _parse_float("STREAK_MULTIPLIER_PER_GAME", 0.20)  # 20% boost per game at/above threshold

# Recalibration configuration
RECALIBRATION_COOLDOWN_SECONDS = _parse_int("RECALIBRATION_COOLDOWN_SECONDS", 7776000)  # 90 days
RECALIBRATION_INITIAL_RD = _parse_float("RECALIBRATION_INITIAL_RD", 350.0)  # RD to reset to
RECALIBRATION_INITIAL_VOLATILITY = _parse_float("RECALIBRATION_INITIAL_VOLATILITY", 0.06)  # Volatility to reset to

# Player Stake Pool configuration (draft mode auto-liquidity)
PLAYER_STAKE_POOL_SIZE = _parse_int("PLAYER_STAKE_POOL_SIZE", 50)  # Total auto-liquidity pool (5 per drafted player)
PLAYER_STAKE_PER_PLAYER = _parse_int("PLAYER_STAKE_PER_PLAYER", 5)  # Auto-liquidity per drafted player
PLAYER_STAKE_ENABLED = _parse_bool("PLAYER_STAKE_ENABLED", True)  # Enable stake pool in draft mode
STAKE_WIN_PROB_MIN = _parse_float("STAKE_WIN_PROB_MIN", 0.10)  # Clamp to prevent extreme odds
STAKE_WIN_PROB_MAX = _parse_float("STAKE_WIN_PROB_MAX", 0.90)

# Spectator Pool configuration
SPECTATOR_POOL_PLAYER_CUT = _parse_float("SPECTATOR_POOL_PLAYER_CUT", 0.10)  # 10% to winning players

# Match Enrichment configuration
ENRICHMENT_DISCOVERY_TIME_WINDOW = _parse_int("ENRICHMENT_DISCOVERY_TIME_WINDOW", 7200)  # 2 hours (seconds)
ENRICHMENT_MIN_PLAYER_MATCH = _parse_int("ENRICHMENT_MIN_PLAYER_MATCH", 10)  # All 10 players required for strict validation
ENRICHMENT_RETRY_DELAYS = _parse_int_list("ENRICHMENT_RETRY_DELAYS", [1, 5, 20, 60, 180])  # Exponential backoff delays (seconds)

# Wrapped (monthly summary) configuration
WRAPPED_ENABLED = _parse_bool("WRAPPED_ENABLED", True)
WRAPPED_MIN_GAMES = _parse_int("WRAPPED_MIN_GAMES", 3)  # Min games to appear in wrapped
WRAPPED_MIN_BETS = _parse_int("WRAPPED_MIN_BETS", 3)  # Min bets for betting awards
WRAPPED_CHECK_INTERVAL_HOURS = _parse_int("WRAPPED_CHECK_INTERVAL_HOURS", 12)  # Hours between checks (12-24)

# Neon Degen Terminal Easter Egg configuration
NEON_DEGEN_ENABLED = _parse_bool("NEON_DEGEN_ENABLED", True)
NEON_LAYER1_CHANCE = _parse_float("NEON_LAYER1_CHANCE", 0.35)  # Subtle text triggers
NEON_LAYER2_CHANCE = _parse_float("NEON_LAYER2_CHANCE", 0.70)  # Medium ASCII art triggers
NEON_LLM_CHANCE = _parse_float("NEON_LLM_CHANCE", 0.60)  # Chance of LLM commentary on Layer 2+
NEON_COOLDOWN_SECONDS = _parse_int("NEON_COOLDOWN_SECONDS", 60)  # Per-user cooldown
