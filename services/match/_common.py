"""Shared logger for the match service package.

The module-level ``logger`` lived at module scope in the former monolithic
``services.match_service`` module. It is hoisted here so the focused mixin
modules can import it without forming an import cycle through
``services.match_service`` itself. ``services.match_service`` re-exports it so
existing ``from services.match_service import ...`` callers keep working
unchanged.
"""

import logging

logger = logging.getLogger("cama_bot.services.match")


def coalesce_os_baseline(
    old_mu: float | None,
    old_sigma: float | None,
    default_mu: float,
    default_sigma: float,
) -> tuple[float, float]:
    """Coalesce a None OpenSkill ``(mu, sigma)`` baseline to the engine defaults.

    The OS engine substitutes its defaults for a None baseline when it computes,
    so the rating_history "before" snapshot must record those same defaults —
    otherwise the recorded "before" (None) disagrees with the math that produced
    the "after". Used by both the Phase 1 (recording) and Phase 2 (post-enrich)
    rating-history writes so the coalesce rule lives in exactly one place.
    """
    return (
        old_mu if old_mu is not None else default_mu,
        old_sigma if old_sigma is not None else default_sigma,
    )
