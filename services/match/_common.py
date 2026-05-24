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
