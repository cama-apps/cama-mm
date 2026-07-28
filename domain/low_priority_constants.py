"""Low-priority matchmaking rules.

Low priority is assigned by an admin (``/lowprio add``) and requires
``LOW_PRIORITY_REQUIRED_WINS`` wins to clear: each recorded win decrements
``wins_remaining`` in-transaction with the match record (see
``match_repository``). While active, the shuffler applies the goodness
penalty and effectiveness reduction below.
"""

LOW_PRIORITY_EFFECTIVENESS = 0.5
LOW_PRIORITY_GOODNESS_PENALTY = 500.0
LOW_PRIORITY_REQUIRED_WINS = 3
