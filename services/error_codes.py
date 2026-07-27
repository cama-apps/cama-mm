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

# Player/registration errors
PLAYER_NOT_FOUND = "player_not_found"

# Lobby errors

# Match errors

# Economy/betting errors
INSUFFICIENT_FUNDS = "insufficient_funds"

# Loan errors
LOAN_ALREADY_EXISTS = "loan_already_exists"
NO_OUTSTANDING_LOAN = "no_outstanding_loan"
LOAN_AMOUNT_EXCEEDED = "loan_amount_exceeded"
COOLDOWN_ACTIVE = "cooldown_active"

# Bankruptcy errors
BANKRUPTCY_COOLDOWN = "bankruptcy_cooldown"
NOT_IN_DEBT = "not_in_debt"

# Prediction errors
PREDICTION_RESOLVED = "prediction_resolved"

# Pet errors
ALREADY_HAS_PET = "already_has_pet"
NO_PET = "no_pet"
PET_DEAD = "pet_dead"
PET_EGG = "pet_egg"
PET_HATCHED = "pet_hatched"
ALREADY_GILDED = "already_gilded"
PET_FULL = "pet_full"
NO_SUPPLIES = "no_supplies"
FEED_CAP = "feed_cap"
ALREADY_PAMPERED = "already_pampered"
STACK_CAP = "stack_cap"

# Enrichment errors

# Recalibration errors
