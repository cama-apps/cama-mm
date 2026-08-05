"""Low-priority matchmaking rules.

Low priority is assigned by an admin and requires a configurable number of
wins to clear. ``LOW_PRIORITY_REQUIRED_WINS`` is the default; assignments are
bounded by ``LOW_PRIORITY_MIN_WINS`` and ``LOW_PRIORITY_MAX_WINS``. Each
eligible recorded win decrements ``wins_remaining`` in-transaction with the
match record. While active, the shuffler applies the goodness penalty and
effectiveness reduction below.
"""

LOW_PRIORITY_EFFECTIVENESS = 0.5
LOW_PRIORITY_GOODNESS_PENALTY = 500.0
LOW_PRIORITY_REQUIRED_WINS = 3
LOW_PRIORITY_MIN_WINS = 1
LOW_PRIORITY_MAX_WINS = 20
