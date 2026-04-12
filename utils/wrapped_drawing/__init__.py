"""Wrapped-style slide drawing utilities.

This subpackage preserves the original ``utils.wrapped_drawing`` module API as
a flat re-export so existing imports continue to work. Internally the module is
split by slide type: ``_common`` for shared primitives/palette, ``summary`` for
the server/personal/records cards, ``awards`` for award cards and grid, and
``story`` for the story beats (teammates/rivals, hero spotlight, lane
breakdown, package deal, chart wrapping).
"""

from utils.wrapped_drawing._common import (
    ACCENT_BLUE,
    ACCENT_GOLD,
    ACCENT_GREEN,
    ACCENT_PURPLE,
    ACCENT_RED,
    BG_GRADIENT_END,
    BG_GRADIENT_START,
    CATEGORY_COLORS,
    NA_COLOR,
    SLIDE_COLORS,
    TEXT_DARK,
    TEXT_GREY,
    TEXT_WHITE,
    WORST_LABEL_COLOR,
    _center_text,
    _draw_gradient_background,
    _draw_rgba_gradient_background,
    _draw_rounded_rect,
    _draw_wrapped_header,
    _get_font,
    _word_wrap,
)
from utils.wrapped_drawing.awards import draw_awards_grid, draw_wrapped_award
from utils.wrapped_drawing.story import (
    draw_hero_spotlight_slide,
    draw_lane_breakdown_slide,
    draw_package_deal_slide,
    draw_pairwise_slide,
    draw_story_slide,
    wrap_chart_in_slide,
)
from utils.wrapped_drawing.summary import (
    draw_records_slide,
    draw_summary_stats_slide,
    draw_wrapped_personal,
    draw_wrapped_summary,
)

__all__ = [
    # Palette + helpers
    "ACCENT_BLUE",
    "ACCENT_GOLD",
    "ACCENT_GREEN",
    "ACCENT_PURPLE",
    "ACCENT_RED",
    "BG_GRADIENT_END",
    "BG_GRADIENT_START",
    "CATEGORY_COLORS",
    "NA_COLOR",
    "SLIDE_COLORS",
    "TEXT_DARK",
    "TEXT_GREY",
    "TEXT_WHITE",
    "WORST_LABEL_COLOR",
    "_center_text",
    "_draw_gradient_background",
    "_draw_rgba_gradient_background",
    "_draw_rounded_rect",
    "_draw_wrapped_header",
    "_get_font",
    "_word_wrap",
    # Server / personal cards
    "draw_wrapped_summary",
    "draw_wrapped_personal",
    "draw_summary_stats_slide",
    "draw_records_slide",
    # Awards
    "draw_wrapped_award",
    "draw_awards_grid",
    # Story beats
    "draw_story_slide",
    "draw_pairwise_slide",
    "draw_hero_spotlight_slide",
    "draw_lane_breakdown_slide",
    "draw_package_deal_slide",
    "wrap_chart_in_slide",
]
