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
LOBBY_READY_COOLDOWN_SECONDS = _parse_int("LOBBY_READY_COOLDOWN_SECONDS", 60)

# Dedicated lobby channel - if set, lobby embeds are posted here instead of command channel
LOBBY_CHANNEL_ID: int | None = None
_lobby_channel_raw = os.getenv("LOBBY_CHANNEL_ID")
if _lobby_channel_raw:
    try:
        LOBBY_CHANNEL_ID = int(_lobby_channel_raw.strip())
    except ValueError:
        LOBBY_CHANNEL_ID = None
USE_GLICKO = _parse_bool("USE_GLICKO", True)
OPENSKILL_SHUFFLE_CHANCE = _parse_float("OPENSKILL_SHUFFLE_CHANCE", 0.01)  # 1% chance per shuffle

SHUFFLER_SETTINGS: dict[str, Any] = {
    "off_role_multiplier": _parse_float("OFF_ROLE_MULTIPLIER", 0.95),
    "off_role_flat_penalty": _parse_float("OFF_ROLE_FLAT_PENALTY", 380.0),
    "role_matchup_delta_weight": _parse_float("ROLE_MATCHUP_DELTA_WEIGHT", 0.19),
    "exclusion_penalty_weight": _parse_float("EXCLUSION_PENALTY_WEIGHT", 60.0),
    # Recent match penalty: players who participated in the most recent match
    # get this penalty added to goodness score (making them more likely to sit out)
    # Hardcoded default - not configurable via env var (silent operation)
    "recent_match_penalty_weight": 165.0,
}

NEW_PLAYER_EXCLUSION_BOOST = _parse_int("NEW_PLAYER_EXCLUSION_BOOST", 4)
RD_PRIORITY_WEIGHT = _parse_float("RD_PRIORITY_WEIGHT", 0.2)

JOPACOIN_PER_GAME = _parse_int("JOPACOIN_PER_GAME", 1)
STREAMING_BONUS = _parse_int("STREAMING_BONUS", 1)  # JC awarded for Go Live + Dota 2
FIRST_GAME_BONUS = _parse_int("FIRST_GAME_BONUS", 1)  # JC awarded to all players in first game after 5pm PST
FIRST_GAME_RESET_HOUR = _parse_int("FIRST_GAME_RESET_HOUR", 17)  # Hour (0-23) in America/Los_Angeles
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
LOTTERY_ACTIVITY_DAYS = _parse_int("LOTTERY_ACTIVITY_DAYS", 14)  # Days of activity required for lottery eligibility

# Shop pricing
SHOP_ANNOUNCE_COST = _parse_int("SHOP_ANNOUNCE_COST", 10)
SHOP_ANNOUNCE_TARGET_COST = _parse_int("SHOP_ANNOUNCE_TARGET_COST", 100)
SHOP_PROTECT_HERO_COST = _parse_int("SHOP_PROTECT_HERO_COST", 250)
SHOP_MYSTERY_GIFT_COST = _parse_int("SHOP_MYSTERY_GIFT_COST", 10000)
SHOP_DOUBLE_OR_NOTHING_COST = _parse_int("SHOP_DOUBLE_OR_NOTHING_COST", 50)
DOUBLE_OR_NOTHING_COOLDOWN_SECONDS = _parse_int("DOUBLE_OR_NOTHING_COOLDOWN_SECONDS", 2592000)  # 30 days

# Soft Avoid configuration
SHOP_SOFT_AVOID_COST = _parse_int("SHOP_SOFT_AVOID_COST", 700)  # Cost to soft avoid a player
SOFT_AVOID_GAMES_DURATION = _parse_int("SOFT_AVOID_GAMES_DURATION", 10)  # Number of games avoid lasts
SOFT_AVOID_PENALTY = _parse_float("SOFT_AVOID_PENALTY", 180.0)  # Penalty added to shuffler when pair on same team

# Package Deal configuration
SHOP_PACKAGE_DEAL_BASE_COST = _parse_int("SHOP_PACKAGE_DEAL_BASE_COST", 500)  # Base cost for package deal
SHOP_PACKAGE_DEAL_RATING_DIVISOR = _parse_float("SHOP_PACKAGE_DEAL_RATING_DIVISOR", 10.0)  # Divide sum of ratings by this
PACKAGE_DEAL_GAMES_DURATION = _parse_int("PACKAGE_DEAL_GAMES_DURATION", 10)  # Number of games deal lasts
PACKAGE_DEAL_PENALTY = _parse_float("PACKAGE_DEAL_PENALTY", 100.0)  # Penalty when pair on DIFFERENT teams
PACKAGE_DEAL_SPLIT_PENALTY = _parse_float("PACKAGE_DEAL_SPLIT_PENALTY", 100.0)  # Penalty when one selected, one excluded
RATING_SPREAD_DIVISOR = _parse_float("RATING_SPREAD_DIVISOR", 10.0)  # Divisor for (max_rating - min_rating) pool spread penalty

# Recalibrate shop item
SHOP_RECALIBRATE_COST = _parse_int("SHOP_RECALIBRATE_COST", 300)

# Wheel of Fortune configuration
WHEEL_COOLDOWN_SECONDS = _parse_int("WHEEL_COOLDOWN_SECONDS", 86400)  # 24 hours
WHEEL_LOSE_PENALTY_COOLDOWN = _parse_int("WHEEL_LOSE_PENALTY_COOLDOWN", 432000)  # 5 days for LOSE
WHEEL_BANKRUPT_PENALTY = _parse_int("WHEEL_BANKRUPT_PENALTY", 100)
WHEEL_MAX_REWARD = _parse_int("WHEEL_MAX_REWARD", 100)
WHEEL_ANIMATION_FRAMES = _parse_int("WHEEL_ANIMATION_FRAMES", 5)  # Number of spin frames
WHEEL_FRAME_DELAY_MS = _parse_int("WHEEL_FRAME_DELAY_MS", 1000)  # Delay between frames (ms)
WHEEL_TARGET_EV = _parse_float("WHEEL_TARGET_EV", -25.0)  # Target expected value per spin

# Estimated EV for special wedges — total economic impact, not just spinner's personal outcome.
# Used to adjust BANKRUPT value so overall wheel drain stays at WHEEL_TARGET_EV.
# RED_SHELL: zero-sum transfer between players, no JC created or destroyed
WHEEL_RED_SHELL_EST_EV = _parse_float("WHEEL_RED_SHELL_EST_EV", 0.0)
# BLUE_SHELL: mostly zero-sum transfer, self-hit (~1/N chance) sends to nonprofit
WHEEL_BLUE_SHELL_EST_EV = _parse_float("WHEEL_BLUE_SHELL_EST_EV", -4.0)
# LIGHTNING_BOLT: taxes ALL positive-balance players 2-5%, all to nonprofit sink
# estimate ~15 active players * ~100 avg balance * ~3.5% avg tax = ~55 JC destroyed
WHEEL_LIGHTNING_BOLT_EST_EV = _parse_float("WHEEL_LIGHTNING_BOLT_EST_EV", -55.0)
# COMMUNE: all positive-balance players donate 1 JC to spinner; positive for spinner
# estimate ~8 active players with positive balance → spinner receives ~8 JC
WHEEL_COMMUNE_EST_EV = _parse_float("WHEEL_COMMUNE_EST_EV", 8.0)
# COMEBACK: grants one-use pardon token; next BANKRUPT becomes LOSE
# estimated ~15 JC value (soft positive: negates a future BANKRUPT hit)
WHEEL_COMEBACK_EST_EV = _parse_float("WHEEL_COMEBACK_EST_EV", 15.0)

# Lightning Bolt (wheel wedge: server-wide tax to nonprofit)
LIGHTNING_BOLT_PCT_MIN = _parse_float("LIGHTNING_BOLT_PCT_MIN", 0.02)
LIGHTNING_BOLT_PCT_MAX = _parse_float("LIGHTNING_BOLT_PCT_MAX", 0.05)
LIGHTNING_BOLT_MIN_TAX = _parse_int("LIGHTNING_BOLT_MIN_TAX", 1)

# Golden Wheel (exclusive to top N jopacoin balance holders)
WHEEL_GOLDEN_TOP_N = _parse_int("WHEEL_GOLDEN_TOP_N", 3)
WHEEL_GOLDEN_TARGET_EV = _parse_float("WHEEL_GOLDEN_TARGET_EV", -50.0)
# Estimated EVs for special golden wedges — used to calibrate OVEREXTENDED value
# so the overall wheel EV stays at WHEEL_GOLDEN_TARGET_EV.
WHEEL_GOLDEN_HEIST_EST_EV = _parse_float("WHEEL_GOLDEN_HEIST_EST_EV", 33.0)          # per wedge (×2)
WHEEL_GOLDEN_MARKET_CRASH_EST_EV = _parse_float("WHEEL_GOLDEN_MARKET_CRASH_EST_EV", 35.0)
WHEEL_GOLDEN_COMPOUND_EST_EV = _parse_float("WHEEL_GOLDEN_COMPOUND_EST_EV", 30.0)
WHEEL_GOLDEN_TRICKLE_DOWN_EST_EV = _parse_float("WHEEL_GOLDEN_TRICKLE_DOWN_EST_EV", 65.0)
WHEEL_GOLDEN_DIVIDEND_EST_EV = _parse_float("WHEEL_GOLDEN_DIVIDEND_EST_EV", 10.0)
WHEEL_GOLDEN_HOSTILE_TAKEOVER_EST_EV = _parse_float("WHEEL_GOLDEN_HOSTILE_TAKEOVER_EST_EV", 35.0)

# Tip transaction fee (clamped to 0.0 - 0.5 to prevent economy-breaking values)
_raw_tip_fee_rate = _parse_float("TIP_FEE_RATE", 0.01)
TIP_FEE_RATE = max(0.0, min(0.5, _raw_tip_fee_rate))  # 1% default, max 50%

# Steam/Valve API
STEAM_API_KEY = os.getenv("STEAM_API_KEY")

# AI/LLM Configuration (Groq or Cerebras via LiteLLM)
GROQ_API_KEY = os.getenv("GROQ_API_KEY")
CEREBRAS_API_KEY = os.getenv("CEREBRAS_API_KEY")
# Prefer Groq, fall back to Cerebras
LLM_API_KEY = GROQ_API_KEY or CEREBRAS_API_KEY
AI_MODEL = os.getenv("AI_MODEL", "groq/qwen/qwen3-32b" if GROQ_API_KEY else "cerebras/qwen-3-235b-a22b-instruct-2507")
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

# Wheel War (Rebellion) configuration
REBELLION_VOTE_WINDOW_SECONDS = _parse_int("REBELLION_VOTE_WINDOW_SECONDS", 900)          # 15 min vote window
REBELLION_ATTACK_QUORUM = _parse_int("REBELLION_ATTACK_QUORUM", 5)                        # Min effective ATTACK votes for war
REBELLION_BASE_THRESHOLD = _parse_int("REBELLION_BASE_THRESHOLD", 25)                     # Base JC threshold (wheel must roll ≥ this)
REBELLION_THRESHOLD_STEP = _parse_int("REBELLION_THRESHOLD_STEP", 5)                      # JC per net voter difference
REBELLION_MIN_THRESHOLD = _parse_int("REBELLION_MIN_THRESHOLD", 5)
REBELLION_MAX_THRESHOLD = _parse_int("REBELLION_MAX_THRESHOLD", 60)
REBELLION_DEFENDER_STAKE = _parse_int("REBELLION_DEFENDER_STAKE", 10)                     # JC to vote DEFEND
REBELLION_VETERAN_REBEL_VOTE_WEIGHT = _parse_float("REBELLION_VETERAN_REBEL_VOTE_WEIGHT", 1.5)  # Vote multiplier for 2+ bankruptcies
REBELLION_VETERAN_REBEL_MIN_BANKRUPTCIES = _parse_int("REBELLION_VETERAN_REBEL_MIN_BANKRUPTCIES", 2)
REBELLION_ATTACKER_FLAT_REWARD = _parse_int("REBELLION_ATTACKER_FLAT_REWARD", 15)         # JC per attacker on win
REBELLION_INCITER_FLAT_REWARD = _parse_int("REBELLION_INCITER_FLAT_REWARD", 30)           # JC for inciter on win
REBELLION_DEFENDER_WIN_REWARD = _parse_int("REBELLION_DEFENDER_WIN_REWARD", 20)           # JC per defender on win
REBELLION_FIRST_DEFENDER_BONUS = _parse_int("REBELLION_FIRST_DEFENDER_BONUS", 10)         # Extra JC for champion defender
REBELLION_WHEEL_EFFECT_SPINS = _parse_int("REBELLION_WHEEL_EFFECT_SPINS", 10)             # Spins the wheel effect lasts
REBELLION_BANKRUPT_WEAKEN_RATE = _parse_float("REBELLION_BANKRUPT_WEAKEN_RATE", 0.25)     # BANKRUPT * 0.75 when attackers win
REBELLION_BANKRUPT_STRENGTHEN_RATE = _parse_float("REBELLION_BANKRUPT_STRENGTHEN_RATE", 0.50)  # BANKRUPT * 1.50 when defenders win
REBELLION_WAR_TROPHY_VALUE = _parse_int("REBELLION_WAR_TROPHY_VALUE", 80)                 # JC value of WAR TROPHY wedge
REBELLION_RETRIBUTION_STEAL = _parse_int("REBELLION_RETRIBUTION_STEAL", 20)               # JC stolen by RETRIBUTION from attackers
REBELLION_CELEBRATION_SPIN_WINDOW = _parse_int("REBELLION_CELEBRATION_SPIN_WINDOW", 86400)  # 24h to use free spin
REBELLION_INCITER_COOLDOWN_SECONDS = _parse_int("REBELLION_INCITER_COOLDOWN_SECONDS", 604800)  # 7 days between incitements
REBELLION_META_BET_WINDOW_SECONDS = _parse_int("REBELLION_META_BET_WINDOW_SECONDS", 120)  # 2-min betting window after war declared
REBELLION_META_BET_MAX = _parse_int("REBELLION_META_BET_MAX", 50)                         # Max JC per meta-bet
REBELLION_FIZZLE_SPIN_MAX_WIN = _parse_int("REBELLION_FIZZLE_SPIN_MAX_WIN", 5)            # Fizzle consolation spin max win cap
REBELLION_GAMBA_COOLDOWN_PENALTY = _parse_int("REBELLION_GAMBA_COOLDOWN_PENALTY", 172800)  # 48h extra cooldown for attackers on defender win

# Prediction market (order-book mechanic) configuration
PREDICTION_CONTRACT_VALUE = _parse_int("PREDICTION_CONTRACT_VALUE", 100)         # jopa paid per winning contract
PREDICTION_TICK_SIZE = _parse_int("PREDICTION_TICK_SIZE", 1)                     # jopa per price tick (= 1% probability)
PREDICTION_LEVELS_PER_SIDE = _parse_int("PREDICTION_LEVELS_PER_SIDE", 3)         # ladder depth each side
PREDICTION_SIZE_PER_LEVEL = _parse_int("PREDICTION_SIZE_PER_LEVEL", 5)           # contracts per level (uniform)
PREDICTION_SPREAD_TICKS = _parse_int("PREDICTION_SPREAD_TICKS", 1)               # top-of-book offset from mid
PREDICTION_REFRESH_SECONDS = _parse_int("PREDICTION_REFRESH_SECONDS", 86400)     # per-market refresh interval (~daily)
PREDICTION_REFRESH_WAKE_SECONDS = _parse_int("PREDICTION_REFRESH_WAKE_SECONDS", 3600)  # how often the worker wakes to scan
PREDICTION_DRIFT_MIN = _parse_int("PREDICTION_DRIFT_MIN", -2)                    # inclusive uniform integer drift
PREDICTION_DRIFT_MAX = _parse_int("PREDICTION_DRIFT_MAX", 2)
PREDICTION_FADE_TICKS = _parse_int("PREDICTION_FADE_TICKS", 5)                   # how far fair fades when one side fully consumed
PREDICTION_PRICE_LOW = _parse_int("PREDICTION_PRICE_LOW", 4)                     # hard clamp on fair so ladder fits in {1..99}
PREDICTION_PRICE_HIGH = _parse_int("PREDICTION_PRICE_HIGH", 96)
PREDICTION_RECENT_TRADES_SHOWN = _parse_int("PREDICTION_RECENT_TRADES_SHOWN", 5)
PREDICTION_DIGEST_HOUR_UTC = _parse_int("PREDICTION_DIGEST_HOUR_UTC", 12)        # UTC hour for daily guild digest
PREDICTION_INITIAL_FAIR_DEFAULT = _parse_int("PREDICTION_INITIAL_FAIR_DEFAULT", 50)

# Trivia configuration
TRIVIA_COOLDOWN_SECONDS = _parse_int("TRIVIA_COOLDOWN_SECONDS", 21600)  # 6 hours
TRIVIA_ANSWER_TIMEOUT_SECONDS = _parse_int("TRIVIA_ANSWER_TIMEOUT_SECONDS", 15)
TRIVIA_REWARD_PER_QUESTION = _parse_int("TRIVIA_REWARD_PER_QUESTION", 1)


# Neon Degen Terminal Easter Egg configuration
NEON_DEGEN_ENABLED = _parse_bool("NEON_DEGEN_ENABLED", True)
NEON_LAYER1_CHANCE = _parse_float("NEON_LAYER1_CHANCE", 0.35)  # Subtle text triggers
NEON_LAYER2_CHANCE = _parse_float("NEON_LAYER2_CHANCE", 0.70)  # Medium ASCII art triggers
NEON_LLM_CHANCE = _parse_float("NEON_LLM_CHANCE", 0.60)  # Chance of LLM commentary on Layer 2+
NEON_COOLDOWN_SECONDS = _parse_int("NEON_COOLDOWN_SECONDS", 60)  # Per-user cooldown
NEON_MVP_CHANCE = _parse_float("NEON_MVP_CHANCE", 0.10)  # 10% per winning player after enrichment
