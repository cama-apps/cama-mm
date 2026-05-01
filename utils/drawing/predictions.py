"""Per-market price chart for prediction-market embeds.

Plots fair-price snapshots over time. One snapshot per ladder refresh / admin
override / market creation; straight-line interpolation between points. With
multiple snapshots, the Y axis auto-zooms to the data range (±10pp, snapped
to nearest 10) so narrow-band markets render legibly. Empty / single-point
series keep the original 0–100 axis since there's no range to zoom around.
"""

from __future__ import annotations

import datetime as _dt
from io import BytesIO

from PIL import Image, ImageDraw

from utils.drawing._common import (
    DISCORD_ACCENT,
    DISCORD_BG,
    DISCORD_GREY,
    DISCORD_WHITE,
    _get_font,
    _get_text_size,
)


def draw_market_fair_history(
    market_id: int,
    snapshots: list[tuple[int, int]],
    created_at: int,
    *,
    title: str | None = None,
) -> BytesIO:
    """Render the fair-price history line chart as a PNG.

    Args:
        market_id: Market identifier (only used for the chart title fallback).
        snapshots: ``[(unix_ts, fair_pct), ...]`` ordered oldest first. May be
            empty (renders an empty chart with the y-axis grid).
        created_at: Market creation timestamp; left edge of the chart.
        title: Optional override for the chart title.

    Returns:
        ``BytesIO`` containing the PNG image.
    """
    width = 700
    height = 280
    padding_left = 50
    padding_right = 30
    padding_top = 40
    padding_bottom = 40
    chart_x = padding_left
    chart_y = padding_top
    chart_width = width - padding_left - padding_right
    chart_height = height - padding_top - padding_bottom

    img = Image.new("RGBA", (width, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    title_font = _get_font(18)
    label_font = _get_font(12)

    title_text = title or f"Market #{market_id} — fair history"
    draw.text((padding_left, 12), title_text, fill=DISCORD_WHITE, font=title_font)

    now_ts = int(_dt.datetime.now(_dt.UTC).timestamp())
    if snapshots:
        last_ts = max(now_ts, snapshots[-1][0])
    else:
        last_ts = max(now_ts, created_at + 1)
    span = max(last_ts - created_at, 1)

    # Y range: auto-zoom to data with ±10pp padding snapped to nearest 10
    # when we have at least 2 points; fall back to 0–100 otherwise.
    if len(snapshots) >= 2:
        pcts = [pct for _, pct in snapshots]
        lo = max(0, ((min(pcts) - 10) // 10) * 10)
        hi = min(100, -((-(max(pcts) + 10)) // 10) * 10)
        if hi <= lo:
            lo, hi = 0, 100
    else:
        lo, hi = 0, 100
    y_span = hi - lo

    def x_for(ts: int) -> int:
        return chart_x + int((ts - created_at) / span * chart_width)

    def y_for(pct: float) -> int:
        clamped = max(float(lo), min(float(hi), pct))
        return chart_y + int((hi - clamped) / y_span * chart_height)

    # 5 evenly spaced gridlines across [lo, hi]; rounded to int for display.
    tick_pcts = [lo + round(y_span * i / 4) for i in range(5)]
    for pct in tick_pcts:
        y = y_for(pct)
        draw.line(
            [(chart_x, y), (chart_x + chart_width, y)],
            fill=DISCORD_GREY,
            width=1,
        )
        label = f"{pct}%"
        text_w, _ = _get_text_size(label_font, label)
        draw.text(
            (chart_x - text_w - 6, y - 7),
            label,
            fill=DISCORD_GREY,
            font=label_font,
        )

    # X axis ticks: start, end, plus 1-2 evenly spaced inside if span warrants.
    use_hhmm = span < 24 * 3600
    fmt = "%H:%M" if use_hhmm else "%m-%d"
    tick_count = 4 if span >= 4 * 3600 else 2
    for i in range(tick_count + 1):
        ts = created_at + (span * i // tick_count)
        x = x_for(ts)
        label = _dt.datetime.fromtimestamp(ts, tz=_dt.UTC).strftime(fmt)
        text_w, _ = _get_text_size(label_font, label)
        draw.text(
            (x - text_w // 2, chart_y + chart_height + 6),
            label,
            fill=DISCORD_GREY,
            font=label_font,
        )

    if not snapshots:
        # Nothing to plot — just leave the gridlines.
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    points = [(x_for(ts), y_for(pct)) for ts, pct in snapshots]
    if len(points) == 1:
        # Single snapshot — draw a horizontal hairline so the user sees where it sits.
        only_x, only_y = points[0]
        draw.line(
            [(chart_x, only_y), (chart_x + chart_width, only_y)],
            fill=DISCORD_ACCENT,
            width=2,
        )
        draw.ellipse(
            [(only_x - 3, only_y - 3), (only_x + 3, only_y + 3)],
            fill=DISCORD_ACCENT,
            outline=DISCORD_WHITE,
        )
    else:
        for i in range(len(points) - 1):
            draw.line([points[i], points[i + 1]], fill=DISCORD_ACCENT, width=2)
        for px, py in points:
            draw.ellipse(
                [(px - 2, py - 2), (px + 2, py + 2)],
                fill=DISCORD_ACCENT,
                outline=DISCORD_WHITE,
            )

    # Annotate the latest fair near the right edge.
    last_ts, last_pct = snapshots[-1]
    last_x, last_y = points[-1]
    label = f"{last_pct}%"
    text_w, _ = _get_text_size(_get_font(14), label)
    draw.text(
        (min(last_x + 6, chart_x + chart_width - text_w), last_y - 8),
        label,
        fill=DISCORD_WHITE,
        font=_get_font(14),
    )

    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp
