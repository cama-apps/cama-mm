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


def _parse_optional_int(env_var: str) -> int | None:
    raw = os.getenv(env_var)
    if raw is None:
        return None
    try:
        return int(raw)
    except ValueError:
        return None


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


def _parse_int_list_raw(raw: str | None, default: list[int]) -> list[int]:
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

TAX_MAN_USER_IDS = _parse_int_list_raw(
    os.getenv("TAX_MAN_USER_IDS", os.getenv("TAX_MEN_USER_IDS")),
    [],
)
TAX_FINE_COOLDOWN_SECONDS = _parse_int("TAX_FINE_COOLDOWN_SECONDS", 30 * 24 * 60 * 60)

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

# Dedicated dig channel - if set, public /dig embeds are posted here instead
# of the command channel. Ephemeral followups (gear panel, info, shop, etc.)
# stay in the invocation channel regardless.
DIG_CHANNEL_ID: int | None = None
_dig_channel_raw = os.getenv("DIG_CHANNEL_ID")
if _dig_channel_raw:
    try:
        DIG_CHANNEL_ID = int(_dig_channel_raw.strip())
    except ValueError:
        DIG_CHANNEL_ID = None

# Dedicated mafia channel. Public mafia embeds post here and /mafia commands
# are gated to it (threads under it inherit). Hardcoded rather than env-driven.
MAFIA_CHANNEL_ID: int = 1514997325385306132
USE_GLICKO = _parse_bool("USE_GLICKO", True)
OPENSKILL_SHUFFLE_CHANCE = _parse_float("OPENSKILL_SHUFFLE_CHANCE", 0.01)  # 1% chance per shuffle


SHUFFLER_SETTINGS: dict[str, Any] = {
    "off_role_multiplier": _parse_float("OFF_ROLE_MULTIPLIER", 0.95),
    "off_role_flat_penalty": _parse_float("OFF_ROLE_FLAT_PENALTY", 420.0),
    "role_matchup_delta_weight": _parse_float("ROLE_MATCHUP_DELTA_WEIGHT", 0.19),
    "exclusion_penalty_weight": _parse_float("EXCLUSION_PENALTY_WEIGHT", 70.0),
    "region_split_penalty": _parse_float("REGION_SPLIT_PENALTY", 500.0),
    # Recent match penalty: players who participated in the most recent match
    # get this penalty added to goodness score (making them more likely to sit out)
    # Hardcoded default - not configurable via env var (silent operation)
    "recent_match_penalty_weight": 210.0,
}

NEW_PLAYER_EXCLUSION_BOOST = _parse_int("NEW_PLAYER_EXCLUSION_BOOST", 4)
RD_PRIORITY_WEIGHT = _parse_float("RD_PRIORITY_WEIGHT", 0.2)

JOPACOIN_PER_GAME = _parse_int("JOPACOIN_PER_GAME", 3)
STREAMING_BONUS = _parse_int("STREAMING_BONUS", 1)  # JC awarded for Go Live + Dota 2
FIRST_GAME_BONUS = _parse_int("FIRST_GAME_BONUS", 2)  # JC awarded to all players in first game after 5pm PST
FIRST_GAME_RESET_HOUR = _parse_int("FIRST_GAME_RESET_HOUR", 17)  # Hour (0-23) in America/Los_Angeles
JOPACOIN_MIN_BET = _parse_int("JOPACOIN_MIN_BET", 1)
JOPACOIN_WIN_REWARD = _parse_int("JOPACOIN_WIN_REWARD", 4)
JOPACOIN_EXCLUSION_REWARD = _parse_int("JOPACOIN_EXCLUSION_REWARD", 3)
BET_LOCK_SECONDS = _parse_int("BET_LOCK_SECONDS", 900)  # 15 minutes
# Betting reminders during the open window: plain terse reminders fire at each
# offset (seconds before lock); the AI "last call" fires at BET_LAST_CALL_OFFSET.
BET_REMINDER_OFFSETS = _parse_int_list("BET_REMINDER_OFFSETS", [600, 300])  # 10m, 5m left
BET_LAST_CALL_OFFSET = _parse_int("BET_LAST_CALL_OFFSET", 60)  # 1m left
# When the live pool is at least this lopsided (favorite money / underdog money),
# the final warning @-pings the under-bet team to drum up action.
BET_UNDERDOG_PING_RATIO = _parse_float("BET_UNDERDOG_PING_RATIO", 4.0)
HOUSE_PAYOUT_MULTIPLIER = _parse_float("HOUSE_PAYOUT_MULTIPLIER", 1.0)
DOTA_BET_SEED_AMOUNT = _parse_int("DOTA_BET_SEED_AMOUNT", 50)

# Shared minigame/PvP economy policy. Keep the hostile-loss eligibility floor
# independent from auto-liquidity so tuning betting does not silently retune
# who can be targeted by hostile effects.
MINIGAME_JC_DELTA_SCALE = _parse_float("MINIGAME_JC_DELTA_SCALE", 0.8)
HOSTILE_LOSS_MIN_BALANCE = _parse_int("HOSTILE_LOSS_MIN_BALANCE", 50)

# Auto-liquidity (blind bets) configuration
AUTO_BLIND_ENABLED = _parse_bool("AUTO_BLIND_ENABLED", True)  # Enable auto-blind bets in pool mode
AUTO_BLIND_THRESHOLD = _parse_int("AUTO_BLIND_THRESHOLD", 50)  # Min balance to trigger blind (inclusive)
AUTO_BLIND_PERCENTAGE = _parse_float("AUTO_BLIND_PERCENTAGE", 0.10)  # 10% of balance
AUTO_SPECTATOR_BET_ENABLED = _parse_bool("AUTO_SPECTATOR_BET_ENABLED", True)
AUTO_SPECTATOR_BET_COUNT = _parse_int("AUTO_SPECTATOR_BET_COUNT", 5)
AUTO_SPECTATOR_BET_PERCENTAGE = _parse_float("AUTO_SPECTATOR_BET_PERCENTAGE", 0.01)  # 1% of balance

# Bomb Pot configuration (randomly triggered ~20% of matches)
BOMB_POT_CHANCE = _parse_float("BOMB_POT_CHANCE", 0.20)  # 20% chance per match
BOMB_POT_BLIND_PERCENTAGE = _parse_float("BOMB_POT_BLIND_PERCENTAGE", 0.15)  # 15% plus ante
BOMB_POT_ANTE = _parse_int("BOMB_POT_ANTE", 10)  # Flat 10 JC ante (mandatory, can go negative)
BOMB_POT_PARTICIPATION_BONUS = _parse_int("BOMB_POT_PARTICIPATION_BONUS", 1)  # Extra +1 JC for all players

# Leverage betting configuration
LEVERAGE_TIERS = _parse_int_list("LEVERAGE_TIERS", [2, 3, 5])

# Debt configuration
MAX_DEBT = _parse_int("MAX_DEBT", 500)  # Floor: balance can't go below -MAX_DEBT
GARNISHMENT_PERCENTAGE = _parse_float("GARNISHMENT_PERCENTAGE", 1.0)  # 100% of winnings go to debt

# Bankruptcy configuration
BANKRUPTCY_COOLDOWN_SECONDS = _parse_int("BANKRUPTCY_COOLDOWN_SECONDS", 604800)  # 1 week
BANKRUPTCY_PENALTY_GAMES = _parse_int("BANKRUPTCY_PENALTY_GAMES", 3)  # wins needed to clear the penalty
BANKRUPTCY_PENALTY_RATE = _parse_float("BANKRUPTCY_PENALTY_RATE", 0.75)  # fraction of winnings KEPT (0.75 => lose 25%)
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
SHOP_PROTECT_HERO_COST = _parse_int("SHOP_PROTECT_HERO_COST", 500)
SHOP_JOPA_COIN_COST = _parse_int("SHOP_JOPA_COIN_COST", 10000)
SHOP_NEW_MYSTERY_GIFT_COST = _parse_int("SHOP_NEW_MYSTERY_GIFT_COST", 20000)
SHOP_WITCHS_CURSE_COST = _parse_int("SHOP_WITCHS_CURSE_COST", 100)
WITCHS_CURSE_DURATION_DAYS = _parse_int("WITCHS_CURSE_DURATION_DAYS", 7)
WITCHS_CURSE_LOSS_TRIGGER_PCT = _parse_int("WITCHS_CURSE_LOSS_TRIGGER_PCT", 25)
WITCHS_CURSE_COOLDOWN_SECONDS = _parse_int("WITCHS_CURSE_COOLDOWN_SECONDS", 3600)  # 60-min per-target cap
SHOP_DOUBLE_OR_NOTHING_COST = _parse_int("SHOP_DOUBLE_OR_NOTHING_COST", 50)
DOUBLE_OR_NOTHING_COOLDOWN_SECONDS = _parse_int("DOUBLE_OR_NOTHING_COOLDOWN_SECONDS", 2592000)  # 30 days
PINGEDASH_COST = _parse_int("PINGEDASH_COST", 10)
PINGEDASH_COOLDOWN_SECONDS = _parse_int("PINGEDASH_COOLDOWN_SECONDS", 24 * 60 * 60)
PINGEDASH_TARGET_USER_ID = _parse_optional_int("PINGEDASH_TARGET_USER_ID")

# Soft Avoid configuration
SHOP_SOFT_AVOID_COST = _parse_int("SHOP_SOFT_AVOID_COST", 750)  # Fallback cost to soft avoid a player
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
WHEEL_TARGET_EV = _parse_float("WHEEL_TARGET_EV", -27.5)  # Target expected value per spin
# Bankrupt wheel target EV: positive so the wheel pays out on average — easier
# escape. Trimmed from +25 to +12 once the Mario Kart deflation trio landed; the
# natural floor of the 22 non-BANKRUPT wedges is around +13, so the old +25 was
# unreachable and BANKRUPT always clamped to -1.
WHEEL_BANKRUPT_TARGET_EV = _parse_float("WHEEL_BANKRUPT_TARGET_EV", 12.0)

# Estimated EV for special wedges — total economic impact, not just spinner's personal outcome.
# Used to adjust BANKRUPT value so overall wheel drain stays at WHEEL_TARGET_EV.
# RED_SHELL: zero-sum transfer between players, no JC created or destroyed
WHEEL_RED_SHELL_EST_EV = _parse_float("WHEEL_RED_SHELL_EST_EV", 0.0)
# BLUE_SHELL: mostly zero-sum transfer, self-hit (~1/N chance) sends to nonprofit
WHEEL_BLUE_SHELL_EST_EV = _parse_float("WHEEL_BLUE_SHELL_EST_EV", -4.0)
# LIGHTNING_BOLT: taxes ALL positive-balance players 2-5.7%, all to nonprofit sink
# Wider range raises the average tax from 3.5% to 3.85%; estimated impact scales by 10%.
WHEEL_LIGHTNING_BOLT_EST_EV = _parse_float("WHEEL_LIGHTNING_BOLT_EST_EV", -60.5)
# COMMUNE: all positive-balance players donate 1 JC to spinner; positive for spinner
# estimate ~8 active players with positive balance → spinner receives ~8 JC
WHEEL_COMMUNE_EST_EV = _parse_float("WHEEL_COMMUNE_EST_EV", 8.0)
# COMEBACK: grants one-use pardon token; next BANKRUPT becomes LOSE
# estimated ~15 JC value (soft positive: negates a future BANKRUPT hit)
WHEEL_COMEBACK_EST_EV = _parse_float("WHEEL_COMEBACK_EST_EV", 15.0)
# Bankrupt-only wedges: previously calibrated as 0, now have honest estimates
# so the BANKRUPT wedge value compensates correctly. Note: a few normal-wheel
# wedges (LIGHTNING_BOLT, EMERGENCY, BLUE_SHELL, RED_SHELL) are overridden in
# utils/wheel_drawing.py::_BANKRUPT_SPECIAL_OVERRIDES because a bankrupt
# spinner has no positive balance to tax — the constants below stay accurate
# for the normal wheel context.
WHEEL_EXTEND_1_EST_EV = _parse_float("WHEEL_EXTEND_1_EST_EV", -10.0)
WHEEL_EXTEND_2_EST_EV = _parse_float("WHEEL_EXTEND_2_EST_EV", -20.0)
WHEEL_JAILBREAK_EST_EV = _parse_float("WHEEL_JAILBREAK_EST_EV", 10.0)
WHEEL_EMERGENCY_EST_EV = _parse_float("WHEEL_EMERGENCY_EST_EV", -25.0)
WHEEL_CHAIN_REACTION_EST_EV = _parse_float("WHEEL_CHAIN_REACTION_EST_EV", -25.0)
WHEEL_TOWN_TRIAL_EST_EV = _parse_float("WHEEL_TOWN_TRIAL_EST_EV", 0.0)
WHEEL_DISCOVER_EST_EV = _parse_float("WHEEL_DISCOVER_EST_EV", 5.0)

# Lightning Bolt (wheel wedge: server-wide tax to nonprofit)
LIGHTNING_BOLT_PCT_MIN = _parse_float("LIGHTNING_BOLT_PCT_MIN", 0.02)
LIGHTNING_BOLT_PCT_MAX = _parse_float("LIGHTNING_BOLT_PCT_MAX", 0.057)
LIGHTNING_BOLT_MIN_TAX = _parse_int("LIGHTNING_BOLT_MIN_TAX", 1)

# Golden Wheel (exclusive to top N jopacoin balance holders)
WHEEL_GOLDEN_TOP_N = _parse_int("WHEEL_GOLDEN_TOP_N", 3)
WHEEL_GOLDEN_TARGET_EV = _parse_float("WHEEL_GOLDEN_TARGET_EV", -75.0)
# Estimated EVs for special golden wedges — used to calibrate OVEREXTENDED value
# so the overall wheel EV stays at WHEEL_GOLDEN_TARGET_EV.
WHEEL_GOLDEN_HEIST_EST_EV = _parse_float("WHEEL_GOLDEN_HEIST_EST_EV", 33.0)          # per wedge (×2)
WHEEL_GOLDEN_MARKET_CRASH_EST_EV = _parse_float("WHEEL_GOLDEN_MARKET_CRASH_EST_EV", 35.0)
WHEEL_GOLDEN_COMPOUND_EST_EV = _parse_float("WHEEL_GOLDEN_COMPOUND_EST_EV", 100.0)  # flat +100 reward
WHEEL_GOLDEN_TRICKLE_DOWN_EST_EV = _parse_float("WHEEL_GOLDEN_TRICKLE_DOWN_EST_EV", 65.0)
WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MIN = _parse_float("WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MIN", 0.02)
WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MAX = _parse_float("WHEEL_GOLDEN_TRICKLE_DOWN_PCT_MAX", 0.05)
WHEEL_GOLDEN_DIVIDEND_EST_EV = _parse_float("WHEEL_GOLDEN_DIVIDEND_EST_EV", 10.0)
WHEEL_GOLDEN_HOSTILE_TAKEOVER_EST_EV = _parse_float("WHEEL_GOLDEN_HOSTILE_TAKEOVER_EST_EV", 35.0)
# RECESSION: server-wide deflation. Every positive-balance player loses a % of
# their balance (richer = bigger loss in absolute terms); funds vanish into the
# nonprofit fund. Spinner is included since they're top-N. EV here is from the
# spinner's perspective only (their own loss); calibrated live in code.
WHEEL_GOLDEN_RECESSION_EST_EV = _parse_float("WHEEL_GOLDEN_RECESSION_EST_EV", -200.0)
WHEEL_GOLDEN_RECESSION_TOP_PCT = _parse_float("WHEEL_GOLDEN_RECESSION_TOP_PCT", 0.06)
WHEEL_GOLDEN_RECESSION_MID_PCT = _parse_float("WHEEL_GOLDEN_RECESSION_MID_PCT", 0.035)
WHEEL_GOLDEN_RECESSION_REST_PCT = _parse_float("WHEEL_GOLDEN_RECESSION_REST_PCT", 0.02)
WHEEL_GOLDEN_RECESSION_MID_RANK_END = _parse_int("WHEEL_GOLDEN_RECESSION_MID_RANK_END", 10)

# Mario Kart wedges — chaos items that target other players, not the spinner.
# EV values represent TOTAL economic impact (JC created or destroyed per spin),
# matching the convention used by RED_SHELL/LIGHTNING_BOLT/RECESSION above.

# BANANA_PEEL: player ranked directly below spinner takes a flat 15-29 JC burn.
# Spinner unchanged. One player burned per spin → total economic impact ≈ -22.
WHEEL_BANANA_PEEL_EST_EV = _parse_float("WHEEL_BANANA_PEEL_EST_EV", -22.0)
WHEEL_BANANA_PEEL_LOSS_MIN = _parse_int("WHEEL_BANANA_PEEL_LOSS_MIN", 15)
WHEEL_BANANA_PEEL_LOSS_MAX = _parse_int("WHEEL_BANANA_PEEL_LOSS_MAX", 29)

# GREEN_SHELL: spinner atomically steals 15-25 JC from a random other positive-balance
# player. Zero-sum transfer → total economic impact = 0.
WHEEL_GREEN_SHELL_EST_EV = _parse_float("WHEEL_GREEN_SHELL_EST_EV", 0.0)
WHEEL_GREEN_SHELL_STEAL_MIN = _parse_int("WHEEL_GREEN_SHELL_STEAL_MIN", 15)
WHEEL_GREEN_SHELL_STEAL_MAX = _parse_int("WHEEL_GREEN_SHELL_STEAL_MAX", 25)

# BOMB_OMB: 3 random other positive-balance players each take a 10-23 JC burn.
# Spinner unchanged. Heavy global deflation per spin → total economic impact ≈ -49.5.
WHEEL_BOMB_OMB_EST_EV = _parse_float("WHEEL_BOMB_OMB_EST_EV", -49.5)
WHEEL_BOMB_OMB_VICTIM_LOSS_MIN = _parse_int("WHEEL_BOMB_OMB_VICTIM_LOSS_MIN", 10)
WHEEL_BOMB_OMB_VICTIM_LOSS_MAX = _parse_int("WHEEL_BOMB_OMB_VICTIM_LOSS_MAX", 23)
WHEEL_BOMB_OMB_VICTIM_COUNT = _parse_int("WHEEL_BOMB_OMB_VICTIM_COUNT", 3)

# Tip transaction fee (clamped to 0.0 - 0.5 to prevent economy-breaking values)
_raw_tip_fee_rate = _parse_float("TIP_FEE_RATE", 0.01)
TIP_FEE_RATE = max(0.0, min(0.5, _raw_tip_fee_rate))  # 1% default, max 50%

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

# Prediction market (order-book mechanic) configuration
PREDICTION_CONTRACT_VALUE = _parse_int("PREDICTION_CONTRACT_VALUE", 10)          # jopa paid per winning contract
PREDICTION_TICK_SIZE = _parse_int("PREDICTION_TICK_SIZE", 1)                     # jopa per price tick (= 1% probability)
PREDICTION_LEVELS_PER_SIDE = _parse_int("PREDICTION_LEVELS_PER_SIDE", 3)         # ladder depth each side (initial seed)
PREDICTION_SIZE_PER_LEVEL = _parse_int("PREDICTION_SIZE_PER_LEVEL", 50)          # contracts per level (initial seed)
PREDICTION_SPREAD_TICKS = _parse_int("PREDICTION_SPREAD_TICKS", 2)               # top-of-book offset from mid (initial seed)
PREDICTION_REFRESH_LEVELS_PER_SIDE = _parse_int("PREDICTION_REFRESH_LEVELS_PER_SIDE", 3)  # daily-refresh ladder depth
PREDICTION_REFRESH_SIZE_PER_LEVEL = _parse_int("PREDICTION_REFRESH_SIZE_PER_LEVEL", 10)   # daily-refresh contracts/level
PREDICTION_REFRESH_SPREAD_TICKS = _parse_int("PREDICTION_REFRESH_SPREAD_TICKS", 4)        # daily-refresh top-of-book offset (wider than seed)
PREDICTION_REFRESH_SECONDS = _parse_int("PREDICTION_REFRESH_SECONDS", 86400)     # per-market refresh interval (~daily)
PREDICTION_REFRESH_WAKE_SECONDS = _parse_int("PREDICTION_REFRESH_WAKE_SECONDS", 3600)  # how often the worker wakes to scan
PREDICTION_DRIFT_MIN = _parse_int("PREDICTION_DRIFT_MIN", -3)                    # inclusive uniform integer drift
PREDICTION_DRIFT_MAX = _parse_int("PREDICTION_DRIFT_MAX", 3)
PREDICTION_FADE_TICKS = _parse_int("PREDICTION_FADE_TICKS", 5)                   # how far fair fades when one side fully consumed
PREDICTION_PRICE_LOW = _parse_int("PREDICTION_PRICE_LOW", 5)                     # clamp on fair so the seed ladder fits in {1..99}; wider refresh levels get filtered there
PREDICTION_PRICE_HIGH = _parse_int("PREDICTION_PRICE_HIGH", 95)
PREDICTION_RECENT_TRADES_SHOWN = _parse_int("PREDICTION_RECENT_TRADES_SHOWN", 5)
PREDICTION_DIGEST_HOUR_UTC = _parse_int("PREDICTION_DIGEST_HOUR_UTC", 12)        # anchor UTC hour; guild digest fires here and 12h opposite (twice daily)
PREDICTION_INITIAL_FAIR_DEFAULT = _parse_int("PREDICTION_INITIAL_FAIR_DEFAULT", 50)
PREDICTION_MAX_CONTRACTS_PER_TRADE = _parse_int("PREDICTION_MAX_CONTRACTS_PER_TRADE", 1000)  # hard cap on a single buy

# Trivia configuration
TRIVIA_COOLDOWN_SECONDS = _parse_int("TRIVIA_COOLDOWN_SECONDS", 21600)  # 6 hours
TRIVIA_ANSWER_TIMEOUT_SECONDS = _parse_int("TRIVIA_ANSWER_TIMEOUT_SECONDS", 15)
TRIVIA_REWARD_PER_QUESTION = _parse_int("TRIVIA_REWARD_PER_QUESTION", 1)
TRIVIA_BANKRUPT_MULTIPLIER = _parse_float("TRIVIA_BANKRUPT_MULTIPLIER", 2.0)  # Streak-bonus multiplier when balance ≤ 0

# Player-stat trivia configuration. This is intentionally a separate cooldown
# from /trivia: each run is a fixed daily set whose questions are frozen when
# the session starts, so later stat updates cannot change an answer mid-game.
PLAYER_TRIVIA_COOLDOWN_SECONDS = _parse_int(
    "PLAYER_TRIVIA_COOLDOWN_SECONDS", 86400
)
PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS = _parse_int(
    "PLAYER_TRIVIA_ANSWER_TIMEOUT_SECONDS", 20
)
PLAYER_TRIVIA_QUESTION_COUNT = _parse_int("PLAYER_TRIVIA_QUESTION_COUNT", 10)
PLAYER_TRIVIA_REWARD_PER_CORRECT = _parse_int(
    "PLAYER_TRIVIA_REWARD_PER_CORRECT", 1
)
PLAYER_TRIVIA_RECENT_DAYS = _parse_int("PLAYER_TRIVIA_RECENT_DAYS", 30)
PLAYER_TRIVIA_INCLUDE_SPICY = _parse_bool("PLAYER_TRIVIA_INCLUDE_SPICY", False)

# White mana stipend (paid from nonprofit fund on /mana claim while bankrupt)
WHITE_BANKRUPT_STIPEND = _parse_int("WHITE_BANKRUPT_STIPEND", 5)


# Neon Degen Terminal Easter Egg configuration
NEON_DEGEN_ENABLED = _parse_bool("NEON_DEGEN_ENABLED", True)
NEON_LAYER1_CHANCE = _parse_float("NEON_LAYER1_CHANCE", 0.35)  # Subtle text triggers
NEON_LAYER2_CHANCE = _parse_float("NEON_LAYER2_CHANCE", 0.70)  # Medium ASCII art triggers
NEON_LLM_CHANCE = _parse_float("NEON_LLM_CHANCE", 0.60)  # Chance of LLM commentary on Layer 2+
NEON_COOLDOWN_SECONDS = _parse_int("NEON_COOLDOWN_SECONDS", 60)  # Per-user cooldown
NEON_MVP_CHANCE = _parse_float("NEON_MVP_CHANCE", 0.10)  # 10% per winning player after enrichment
NEON_DIG_CHANCE = _parse_float("NEON_DIG_CHANCE", 0.12)  # Base roll for routine dig GIF events
NEON_BIGWIN_FLOOR = _parse_float("NEON_BIGWIN_FLOOR", 0.05)  # Min big-win GIF roll at the payout floor
NEON_BIGWIN_FULL_PAYOUT = _parse_int("NEON_BIGWIN_FULL_PAYOUT", 5000)  # Payout where big-win odds saturate
NEON_BIGWIN_MIN_PAYOUT = _parse_int("NEON_BIGWIN_MIN_PAYOUT", 500)  # Min payout to qualify for a big-win GIF
