"""Shared colors, role order, font helpers, and heatmap primitives for ``utils.drawing``."""

from __future__ import annotations

import math
from dataclasses import dataclass

from PIL import ImageDraw, ImageFont

# ─── Discord-like dark theme colors ────────────────────────────────────────

DISCORD_BG = "#36393F"
DISCORD_DARKER = "#2F3136"
DISCORD_ACCENT = "#5865F2"
DISCORD_GREEN = "#57F287"
DISCORD_RED = "#ED4245"
DISCORD_YELLOW = "#FEE75C"
DISCORD_WHITE = "#FFFFFF"
DISCORD_GREY = "#B9BBBE"

# Role colors for radar graph
ROLE_COLORS = {
    "Carry": "#F44336",
    "Nuker": "#9C27B0",
    "Initiator": "#3F51B5",
    "Disabler": "#00BCD4",
    "Durable": "#4CAF50",
    "Escape": "#FFEB3B",
    "Support": "#FF9800",
    "Pusher": "#795548",
    "Jungler": "#607D8B",
}

# Fixed role order for consistent radar graph positioning
# Arranged for visual clarity: core roles at top, support at bottom
ROLE_ORDER = [
    "Carry",      # Top
    "Nuker",      # Top-right
    "Initiator",  # Right
    "Disabler",   # Bottom-right
    "Durable",    # Bottom
    "Escape",     # Bottom-left
    "Support",    # Left
    "Pusher",     # Top-left
    "Jungler",    # Near top-left
]

# Colors per draft position for the scout report heatmap.
POSITION_COLORS = {
    1: "#FF9800",  # Orange - Carry
    2: "#9C27B0",  # Purple - Mid
    3: "#4CAF50",  # Green - Offlane
    4: "#00BCD4",  # Cyan - Soft Support
    5: "#2196F3",  # Blue - Hard Support
}


# ─── Font + size helpers ───────────────────────────────────────────────────

def _get_font(size: int = 16) -> ImageFont.FreeTypeFont:
    """Get a font, falling back to default if custom fonts unavailable."""
    try:
        return ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", size)
    except OSError:
        try:
            return ImageFont.truetype("arial.ttf", size)
        except OSError:
            return ImageFont.load_default()


def _get_text_size(font: ImageFont.FreeTypeFont, text: str) -> tuple[int, int]:
    """Get text dimensions for the given font."""
    bbox = font.getbbox(text)
    return (bbox[2] - bbox[0], bbox[3] - bbox[1])


# ─── Heatmap color utilities (used by the scout report) ──────────────────

def _lerp_color(c1: tuple, c2: tuple, t: float) -> tuple:
    """Linearly interpolate between two RGB tuples."""
    t = max(0.0, min(1.0, t))
    return tuple(int(c1[i] + (c2[i] - c1[i]) * t) for i in range(3))


def _heatmap_contest_rate(rate: float) -> tuple:
    """Heatmap for contest rate: grey (0%) -> amber (50%) -> red (100%)."""
    rate = max(0.0, min(1.0, rate))
    if rate < 0.5:
        return _lerp_color((150, 150, 150), (255, 180, 50), rate / 0.5)
    return _lerp_color((255, 180, 50), (255, 60, 60), (rate - 0.5) / 0.5)


def _heatmap_winrate(rate: float) -> tuple:
    """Heatmap for win rate: red (0%) -> yellow (50%) -> green (100%)."""
    rate = max(0.0, min(1.0, rate))
    if rate < 0.5:
        return _lerp_color((255, 60, 60), (255, 220, 50), rate / 0.5)
    return _lerp_color((255, 220, 50), (80, 220, 80), (rate - 0.5) / 0.5)


# ─── Signed-log chart primitives (used by gamba and balance_history charts) ──

def signed_log(x: float) -> float:
    """Signed log: ``sign(x) * log1p(|x|)``. Compresses magnitude while keeping sign."""
    if x == 0:
        return 0.0
    sign = 1 if x > 0 else -1
    return sign * math.log1p(abs(x))


def signed_log_inverse(y: float) -> float:
    """Inverse of :func:`signed_log`."""
    if y == 0:
        return 0.0
    sign = 1 if y > 0 else -1
    return sign * (math.exp(abs(y)) - 1)


@dataclass
class ChartProjection:
    """Rectangle + signed-log Y bounds for mapping (event_index, value) to pixels."""

    chart_x: int
    chart_y: int
    chart_width: int
    chart_height: int
    total_x: int
    min_log_y: float
    max_log_y: float

    @property
    def log_y_range(self) -> float:
        return max(self.max_log_y - self.min_log_y, 1e-9)

    def to_pixel(self, x_index: int, y_value: float) -> tuple[int, int]:
        x = self.chart_x + int(
            (x_index - 1) / max(self.total_x - 1, 1) * self.chart_width
        )
        log_y = signed_log(y_value)
        y = self.chart_y + int(
            (self.max_log_y - log_y) / self.log_y_range * self.chart_height
        )
        return (x, y)


def make_projection(
    y_values: list[float],
    total_x: int,
    chart_x: int,
    chart_y: int,
    chart_width: int,
    chart_height: int,
    padding_ratio: float = 0.1,
) -> ChartProjection:
    """Compute a :class:`ChartProjection` that fits ``y_values`` with padding."""
    log_values = [signed_log(v) for v in y_values] or [0.0]
    min_log = min(min(log_values), 0.0)
    max_log = max(max(log_values), 0.0)
    rng = max(abs(max_log - min_log), 0.1)
    min_log -= rng * padding_ratio
    max_log += rng * padding_ratio
    return ChartProjection(
        chart_x=chart_x,
        chart_y=chart_y,
        chart_width=chart_width,
        chart_height=chart_height,
        total_x=total_x,
        min_log_y=min_log,
        max_log_y=max_log,
    )


def draw_zero_line(draw: ImageDraw.ImageDraw, proj: ChartProjection) -> int:
    """Draw the y=0 reference line. Return zero's pixel y."""
    zero_y = proj.chart_y + int(
        (proj.max_log_y - 0) / proj.log_y_range * proj.chart_height
    )
    draw.line(
        [(proj.chart_x, zero_y), (proj.chart_x + proj.chart_width, zero_y)],
        fill=DISCORD_GREY,
        width=1,
    )
    draw.text((proj.chart_x - 25, zero_y - 6), "0", fill=DISCORD_GREY, font=_get_font(13))
    return zero_y


_Y_LABEL_MAGNITUDES = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000, 50000, 100000]


def draw_y_axis_labels(
    draw: ImageDraw.ImageDraw,
    proj: ChartProjection,
    actual_min: float,
    actual_max: float,
) -> None:
    """Draw Y-axis labels at nice round values within the visible log range."""
    label_values: list[float] = [0]
    for magnitude in _Y_LABEL_MAGNITUDES:
        if magnitude <= abs(actual_max) * 1.2:
            label_values.append(magnitude)
        if -magnitude >= actual_min * 1.2:
            label_values.append(-magnitude)

    value_font = _get_font(13)
    for val in sorted(set(label_values)):
        log_val = signed_log(val)
        if log_val < proj.min_log_y or log_val > proj.max_log_y:
            continue
        if val == 0:
            continue  # Drawn by draw_zero_line
        y_pos = proj.chart_y + int(
            (proj.max_log_y - log_val) / proj.log_y_range * proj.chart_height
        )
        label = f"{int(val):+d}"
        text_w = _get_text_size(value_font, label)[0]
        draw.text(
            (proj.chart_x - text_w - 8, y_pos - 6),
            label,
            fill=DISCORD_GREY,
            font=value_font,
        )


def draw_x_axis_labels(
    draw: ImageDraw.ImageDraw,
    proj: ChartProjection,
    x_indices: list[int],
) -> None:
    """Draw X-axis labels at ~5 evenly-spaced points."""
    value_font = _get_font(13)
    x_step = max(len(x_indices) // 5, 1)
    for i in range(0, len(x_indices), x_step):
        x_pos, _ = proj.to_pixel(x_indices[i], 0)
        label = str(x_indices[i])
        text_w = _get_text_size(value_font, label)[0]
        draw.text(
            (x_pos - text_w // 2, proj.chart_y + proj.chart_height + 5),
            label,
            fill=DISCORD_GREY,
            font=value_font,
        )
