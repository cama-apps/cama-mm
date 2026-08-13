"""Low-priority matchmaking rules.

Low priority is assigned by an admin and requires a configurable number of
wins to clear. ``LOW_PRIORITY_REQUIRED_WINS`` is the default; assignments are
bounded by ``LOW_PRIORITY_MIN_WINS`` and ``LOW_PRIORITY_MAX_WINS``. Each
eligible recorded win decrements ``wins_remaining`` in-transaction with the
match record. While active, the shuffler applies the goodness selection
penalty, same-team grouping bonus, and directed purchase modifiers below;
positive rating gains receive the temporary multiplier.
"""

LOW_PRIORITY_EFFECTIVENESS = 0.5
LOW_PRIORITY_AVOID_TARGET_EFFECTIVENESS = 2.0
LOW_PRIORITY_RATING_GAIN_MULTIPLIER = 1.1
LOW_PRIORITY_GOODNESS_PENALTY = 600.0
LOW_PRIORITY_TEAM_GROUPING_BONUS = 100.0
LOW_PRIORITY_REQUIRED_WINS = 3
LOW_PRIORITY_MIN_WINS = 1
LOW_PRIORITY_MAX_WINS = 20
