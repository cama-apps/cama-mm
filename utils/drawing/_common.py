"""Shared colors, role order, font helpers, and heatmap primitives for ``utils.drawing``."""

from __future__ import annotations

import math
from dataclasses import dataclass

from PIL import ImageDraw, ImageFont

from utils.fonts import get_font

# ─── Discord-like dark theme colors ────────────────────────────────────────

DISCORD_BG = "#36393F"
DISCORD_DARKER = "#2F3136"
DISCORD_ACCENT = "#5865F2"
DISCORD_GREEN = "#57F287"
DISCORD_RED = "#ED4245"
DISCORD_YELLOW = "#FEE75C"
DISCORD_WHITE = "#FFFFFF"
DISCORD_GREY = "#B9BBBE"
DISCORD_GRID = "#454950"

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


# ─── Font + size helpers ───────────────────────────────────────────────────

def _get_font(size: int = 16) -> ImageFont.FreeTypeFont:
    """Get a font, falling back to default if custom fonts unavailable."""
    return get_font(size)


def _get_text_size(font: ImageFont.FreeTypeFont, text: str) -> tuple[int, int]:
    """Get text dimensions for the given font."""
    bbox = font.getbbox(text)
    return (bbox[2] - bbox[0], bbox[3] - bbox[1])


# ─── Heatmap color utilities (used by the scout report) ──────────────────

def _lerp_color(c1: tuple, c2: tuple, t: float) -> tuple[int, int, int]:
    """Linearly interpolate between two RGB tuples."""
    t = max(0.0, min(1.0, t))
    return tuple(int(c1[i] + (c2[i] - c1[i]) * t) for i in range(3))


def _heatmap_contest_rate(rate: float) -> tuple[int, int, int]:
    """Heatmap for contest rate: grey (0%) -> amber (50%) -> red (100%)."""
    rate = max(0.0, min(1.0, rate))
    if rate < 0.5:
        return _lerp_color((150, 150, 150), (255, 180, 50), rate / 0.5)
    return _lerp_color((255, 180, 50), (255, 60, 60), (rate - 0.5) / 0.5)


def _heatmap_winrate(rate: float) -> tuple[int, int, int]:
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


_Y_LABEL_MAGNITUDES = [
    1,
    2,
    5,
    10,
    20,
    50,
    100,
    200,
    500,
    1000,
    2000,
    5000,
    10000,
    20000,
    50000,
    100000,
]


def _format_compact_axis_value(value: float) -> str:
    """Format a signed axis value without spending width on insignificant zeroes."""
    magnitude = abs(value)
    if magnitude >= 1_000_000:
        compact = f"{magnitude / 1_000_000:g}m"
    elif magnitude >= 1_000:
        compact = f"{magnitude / 1_000:g}k"
    else:
        compact = f"{magnitude:g}"
    return f"{'+' if value > 0 else '-'}{compact}"


def _select_y_axis_ticks(
    proj: ChartProjection,
    actual_min: float,
    actual_max: float,
    *,
    max_labels: int = 8,
    min_gap: int = 20,
) -> list[tuple[float, int]]:
    """Choose legible signed-log ticks using projected pixel spacing.

    Signed-log axes put the 1/2/5 ticks within each decade surprisingly close
    together. Select from those familiar values with a maximin pass so the labels
    stay distributed across the full plot instead of forming a wall around zero.
    Zero is drawn separately by :func:`draw_zero_line`.
    """
    candidates: list[tuple[float, int]] = []
    for magnitude in _Y_LABEL_MAGNITUDES:
        for value in (-float(magnitude), float(magnitude)):
            if value < actual_min * 1.2 or value > actual_max * 1.2:
                continue
            log_value = signed_log(value)
            if log_value < proj.min_log_y or log_value > proj.max_log_y:
                continue
            y_pos = proj.chart_y + int(
                (proj.max_log_y - log_value) / proj.log_y_range * proj.chart_height
            )
            candidates.append((value, y_pos))

    zero_y = proj.chart_y + int(proj.max_log_y / proj.log_y_range * proj.chart_height)
    selected_y = [zero_y]
    selected: list[tuple[float, int]] = []

    while candidates and len(selected) < max(max_labels - 1, 0):
        value, y_pos = max(
            candidates,
            key=lambda candidate: (
                min(abs(candidate[1] - used_y) for used_y in selected_y),
                abs(candidate[0]),
            ),
        )
        distance = min(abs(y_pos - used_y) for used_y in selected_y)
        candidates.remove((value, y_pos))
        if distance < min_gap:
            continue
        selected.append((value, y_pos))
        selected_y.append(y_pos)

    return sorted(selected, key=lambda tick: tick[1])


def draw_y_axis_labels(
    draw: ImageDraw.ImageDraw,
    proj: ChartProjection,
    actual_min: float,
    actual_max: float,
) -> None:
    """Draw a sparse set of collision-free labels and horizontal guides."""
    value_font = _get_font(12)
    for value, y_pos in _select_y_axis_ticks(proj, actual_min, actual_max):
        draw.line(
            [(proj.chart_x, y_pos), (proj.chart_x + proj.chart_width, y_pos)],
            fill=DISCORD_GRID,
            width=1,
        )
        label = _format_compact_axis_value(value)
        text_w = _get_text_size(value_font, label)[0]
        draw.text(
            (proj.chart_x - text_w - 8, y_pos - 6),
            label,
            fill=DISCORD_GREY,
            font=value_font,
        )


def _select_x_axis_ticks(x_indices: list[int], max_labels: int = 5) -> list[int]:
    """Return at most ``max_labels`` evenly spaced values, including both ends."""
    if not x_indices or max_labels <= 0:
        return []
    count = min(len(x_indices), max_labels)
    if count == 1:
        return [x_indices[0]]
    positions = [round(i * (len(x_indices) - 1) / (count - 1)) for i in range(count)]
    return [x_indices[position] for position in positions]


def draw_x_axis_labels(
    draw: ImageDraw.ImageDraw,
    proj: ChartProjection,
    x_indices: list[int],
) -> None:
    """Draw a quiet baseline with no more than five evenly spaced labels."""
    value_font = _get_font(12)
    baseline_y = proj.chart_y + proj.chart_height
    draw.line(
        [(proj.chart_x, baseline_y), (proj.chart_x + proj.chart_width, baseline_y)],
        fill=DISCORD_GRID,
        width=1,
    )
    for value in _select_x_axis_ticks(x_indices):
        x_pos, _ = proj.to_pixel(value, 0)
        draw.line(
            [(x_pos, baseline_y), (x_pos, baseline_y + 3)],
            fill=DISCORD_GREY,
            width=1,
        )
        label = str(value)
        text_w = _get_text_size(value_font, label)[0]
        draw.text(
            (x_pos - text_w // 2, baseline_y + 6),
            label,
            fill=DISCORD_GREY,
            font=value_font,
        )


def new_figure(*, figsize, facecolor, nrows=1, ncols=1):
    """Return ``(figure, axes)`` for a chart, detached from pyplot.

    ``plt.subplots`` registers the figure in pyplot's process-global Gcf, so it
    lives until an explicit ``plt.close``. Every chart here closed only on its
    success path, so any exception in between — a singular KDE covariance, a
    failed savefig — retained the figure forever in this long-lived process.

    A bare ``Figure`` is never registered: it is reclaimed by refcounting, so
    no close is needed and no failure path can leak. It also keeps chart
    rendering off pyplot's global state, which matters because these run in
    executor threads via ``asyncio.to_thread``.
    """
    import matplotlib

    matplotlib.use("Agg")
    from matplotlib.figure import Figure

    figure = Figure(figsize=figsize, facecolor=facecolor)
    # Attach the Agg canvas explicitly so savefig never has to guess.
    from matplotlib.backends.backend_agg import FigureCanvasAgg

    FigureCanvasAgg(figure)
    return figure, figure.subplots(nrows, ncols)
