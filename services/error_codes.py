"""
Standard error codes for service layer.

These error codes allow command handlers to programmatically handle
specific error conditions without parsing error message text.

Usage:
    from services.error_codes import NOT_FOUND, INSUFFICIENT_FUNDS
    from services.result import Result

    if player is None:
        return Result.fail("Player not found", code=NOT_FOUND)

    if balance < amount:
        return Result.fail("Insufficient funds", code=INSUFFICIENT_FUNDS)
"""

# General errors
NOT_FOUND = "not_found"
VALIDATION_ERROR = "validation_error"
STATE_ERROR = "state_error"
PERMISSION_DENIED = "permission_denied"
RATE_LIMITED = "rate_limited"

# Player/registration errors
PLAYER_NOT_FOUND = "player_not_found"
PLAYER_ALREADY_EXISTS = "player_already_exists"
INVALID_STEAM_ID = "invalid_steam_id"
INVALID_ROLES = "invalid_roles"

# Lobby errors
LOBBY_NOT_FOUND = "lobby_not_found"
LOBBY_FULL = "lobby_full"
LOBBY_CLOSED = "lobby_closed"
NOT_IN_LOBBY = "not_in_lobby"
ALREADY_IN_LOBBY = "already_in_lobby"
INSUFFICIENT_PLAYERS = "insufficient_players"

# Match errors
MATCH_NOT_FOUND = "match_not_found"
MATCH_ALREADY_RECORDED = "match_already_recorded"
VOTING_IN_PROGRESS = "voting_in_progress"
INVALID_RESULT = "invalid_result"

# Economy/betting errors
INSUFFICIENT_FUNDS = "insufficient_funds"
MAX_DEBT_EXCEEDED = "max_debt_exceeded"
BETTING_CLOSED = "betting_closed"
NO_PENDING_MATCH = "no_pending_match"
ALREADY_BET = "already_bet"
IN_DEBT = "in_debt"

# Loan errors
LOAN_ALREADY_EXISTS = "loan_already_exists"
NO_OUTSTANDING_LOAN = "no_outstanding_loan"
LOAN_AMOUNT_EXCEEDED = "loan_amount_exceeded"
COOLDOWN_ACTIVE = "cooldown_active"

# Bankruptcy errors
BANKRUPTCY_COOLDOWN = "bankruptcy_cooldown"
NOT_IN_DEBT = "not_in_debt"

# Prediction errors
PREDICTION_NOT_FOUND = "prediction_not_found"
PREDICTION_CLOSED = "prediction_closed"
PREDICTION_RESOLVED = "prediction_resolved"
INVALID_POSITION = "invalid_position"
ALREADY_VOTED = "already_voted"

# Enrichment errors
MATCH_NOT_ENRICHED = "match_not_enriched"
EXTERNAL_API_ERROR = "external_api_error"

# Recalibration errors
RECALIBRATION_COOLDOWN = "recalibration_cooldown"
INSUFFICIENT_GAMES = "insufficient_games"
