"""Image generation utilities for Dota 2 stats visualization.

This subpackage preserves the original flat ``utils.drawing`` module API via
re-exports so existing imports continue to work. Internally the module is
split by chart type: ``_common`` for shared colors/fonts/heatmap helpers,
``tables`` for match tables, ``roles`` for role/lane/attribute charts,
``gamba`` for the betting P&L chart, ``ratings`` for rating/Glicko charts,
``heroes`` for hero performance/grid/image helpers, and ``analysis`` for
match-prediction, advantage, and scout report charts.
"""

from utils.drawing._common import (
    DISCORD_ACCENT,
    DISCORD_BG,
    DISCORD_DARKER,
    DISCORD_GREEN,
    DISCORD_GREY,
    DISCORD_RED,
    DISCORD_WHITE,
    DISCORD_YELLOW,
    POSITION_COLORS,
    ROLE_COLORS,
    ROLE_ORDER,
    _get_font,
    _get_text_size,
    _heatmap_contest_rate,
    _heatmap_winrate,
    _lerp_color,
)
from utils.drawing.analysis import (
    draw_advantage_graph,
    draw_prediction_over_time,
    draw_scout_report,
)
from utils.drawing.balance_history import draw_balance_chart
from utils.drawing.gamba import draw_gamba_chart
from utils.drawing.heroes import (
    _fetch_hero_image,
    _get_hero_images_batch,
    draw_hero_grid,
    draw_hero_performance_chart,
)
from utils.drawing.ratings import (
    draw_calibration_curve,
    draw_rating_comparison_chart,
    draw_rating_distribution,
    draw_rating_history_chart,
)
from utils.drawing.roles import (
    draw_attribute_distribution,
    draw_lane_distribution,
    draw_role_graph,
)
from utils.drawing.tables import draw_matches_table

__all__ = [
    # Common palette + helpers
    "DISCORD_ACCENT",
    "DISCORD_BG",
    "DISCORD_DARKER",
    "DISCORD_GREEN",
    "DISCORD_GREY",
    "DISCORD_RED",
    "DISCORD_WHITE",
    "DISCORD_YELLOW",
    "POSITION_COLORS",
    "ROLE_COLORS",
    "ROLE_ORDER",
    "_get_font",
    "_get_text_size",
    "_heatmap_contest_rate",
    "_heatmap_winrate",
    "_lerp_color",
    # Tables
    "draw_matches_table",
    # Roles / lanes / attributes
    "draw_role_graph",
    "draw_lane_distribution",
    "draw_attribute_distribution",
    # Gamba P&L
    "draw_gamba_chart",
    # Balance history
    "draw_balance_chart",
    # Ratings
    "draw_rating_history_chart",
    "draw_rating_distribution",
    "draw_calibration_curve",
    "draw_rating_comparison_chart",
    # Heroes
    "draw_hero_performance_chart",
    "draw_hero_grid",
    "_fetch_hero_image",
    "_get_hero_images_batch",
    # Analysis / scout
    "draw_prediction_over_time",
    "draw_advantage_graph",
    "draw_scout_report",
]
