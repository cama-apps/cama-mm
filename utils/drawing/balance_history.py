"""Balance-journey chart with per-source colored markers (``draw_balance_chart``).

Parallels :func:`utils.drawing.gamba.draw_gamba_chart` but plots cumulative balance
delta across all jopacoin-moving sources (bets, predictions, wheel, DoN, tips,
disbursements, match bonuses). Markers are colored by source rather than by outcome,
and the legend shows one swatch per source present in the series.
"""

from __future__ import annotations

from io import BytesIO

from PIL import Image, ImageDraw

from utils.drawing._common import (
    DISCORD_BG,
    DISCORD_GREEN,
    DISCORD_GREY,
    DISCORD_RED,
    DISCORD_WHITE,
    _get_font,
    _get_text_size,
    draw_x_axis_labels,
    draw_y_axis_labels,
    draw_zero_line,
    make_projection,
)

# Source → marker color. Keys match the ``source`` strings emitted by
# :mod:`services.balance_history_service`.
SOURCE_COLORS: dict[str, str] = {
    "bets": "#3B82F6",              # Blue
    "predictions": "#A855F7",       # Purple
    "wheel": "#FACC15",             # Yellow
    "double_or_nothing": "#F97316", # Orange
    "tips": "#EC4899",              # Pink
    "disburse": "#22C55E",          # Green
    "bonus": "#9CA3AF",             # Grey
}

# Short labels used in the legend.
SOURCE_LABELS: dict[str, str] = {
    "bets": "Bets",
    "predictions": "Predictions",
    "wheel": "Wheel",
    "double_or_nothing": "DoN",
    "tips": "Tips",
    "disburse": "Disburse",
    "bonus": "Bonuses",
}


def draw_balance_chart(
    username: str,
    series: list[tuple[int, int, dict]],
    per_source_totals: dict[str, int],
) -> BytesIO:
    """Render the balance-journey chart as a PNG.

    Args:
        username: Player display name.
        series: ``(event_number, cumulative_delta, info)`` tuples. ``info`` must carry
            ``source`` keyed into :data:`SOURCE_COLORS`.
        per_source_totals: ``{source: net_delta}`` (non-zero only). Shown as a footer
            summary.

    Returns:
        ``BytesIO`` containing the PNG image.
    """
    width = 700
    height = 400
    padding = 60
    header_height = 50
    footer_height = 40
    chart_width = width - padding * 2
    chart_height = height - header_height - footer_height - padding

    img = Image.new("RGBA", (width, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    title_font = _get_font(22)
    subtitle_font = _get_font(16)
    label_font = _get_font(14)
    value_font = _get_font(13)

    draw.text((padding, 12), f"{username}'s Balance Journey", fill=DISCORD_WHITE, font=title_font)

    net_total = sum(per_source_totals.values()) if per_source_totals else 0
    subtitle_color = DISCORD_GREEN if net_total >= 0 else DISCORD_RED
    subtitle_text = f"Net: {net_total:+d} jopacoin across {len(series)} events"
    draw.text((padding, 38), subtitle_text, fill=subtitle_color, font=subtitle_font)

    if not series:
        draw.text(
            (width // 2 - 80, height // 2),
            "No balance history yet",
            fill=DISCORD_GREY,
            font=title_font,
        )
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    event_nums = [p[0] for p in series]
    deltas = [p[1] for p in series]
    infos = [p[2] for p in series]

    chart_x = padding
    chart_y = header_height + 20

    proj = make_projection(
        y_values=[float(d) for d in deltas],
        total_x=len(event_nums),
        chart_x=chart_x,
        chart_y=chart_y,
        chart_width=chart_width,
        chart_height=chart_height,
    )

    zero_y = draw_zero_line(draw, proj)
    draw_y_axis_labels(draw, proj, min(deltas), max(deltas))
    draw_x_axis_labels(draw, proj, event_nums)

    # Fill area above/below zero line.
    if len(series) > 1:
        overlay = Image.new("RGBA", (width, height), (0, 0, 0, 0))
        overlay_draw = ImageDraw.Draw(overlay)

        pos_points: list[tuple[int, int]] = [(chart_x, zero_y)]
        for event_num, value, _ in series:
            px, py = proj.to_pixel(event_num, value)
            pos_points.append((px, py if value >= 0 else zero_y))
        pos_points.append((chart_x + chart_width, zero_y))
        if len(pos_points) > 2:
            overlay_draw.polygon(pos_points, fill=(87, 242, 135, 60))

        neg_points: list[tuple[int, int]] = [(chart_x, zero_y)]
        for event_num, value, _ in series:
            px, py = proj.to_pixel(event_num, value)
            neg_points.append((px, py if value <= 0 else zero_y))
        neg_points.append((chart_x + chart_width, zero_y))
        if len(neg_points) > 2:
            overlay_draw.polygon(neg_points, fill=(237, 66, 69, 60))

        img = Image.alpha_composite(img, overlay)
        draw = ImageDraw.Draw(img)

    # Line with up-green / down-red segments.
    if len(series) > 1:
        line_points = [proj.to_pixel(p[0], p[1]) for p in series]
        for i in range(len(line_points) - 1):
            color = DISCORD_GREEN if deltas[i + 1] >= deltas[i] else DISCORD_RED
            draw.line([line_points[i], line_points[i + 1]], fill=color, width=2)

    # Per-source markers — simple circle, colored by source.
    marker_radius = 4
    default_color = SOURCE_COLORS["bets"]
    for event_num, value, info in series:
        px, py = proj.to_pixel(event_num, value)
        color = SOURCE_COLORS.get(info.get("source", ""), default_color)
        draw.ellipse(
            [(px - marker_radius, py - marker_radius), (px + marker_radius, py + marker_radius)],
            fill=color,
            outline=DISCORD_WHITE,
        )

    # Peak / trough / now annotations — same behaviour as gamba chart.
    peak_idx = deltas.index(max(deltas))
    trough_idx = deltas.index(min(deltas))

    peak_x, peak_y = proj.to_pixel(event_nums[peak_idx], deltas[peak_idx])
    trough_x, trough_y = proj.to_pixel(event_nums[trough_idx], deltas[trough_idx])

    if deltas[peak_idx] > 0:
        draw.text(
            (peak_x + 5, peak_y - 15),
            f"Peak: +{deltas[peak_idx]}",
            fill=DISCORD_GREEN,
            font=value_font,
        )

    if deltas[trough_idx] < 0:
        draw.text(
            (trough_x + 5, trough_y + 5),
            f"Trough: {deltas[trough_idx]}",
            fill=DISCORD_RED,
            font=value_font,
        )

    curr_x, curr_y = proj.to_pixel(event_nums[-1], deltas[-1])
    curr_color = DISCORD_GREEN if deltas[-1] >= 0 else DISCORD_RED
    draw.text(
        (curr_x - 40, curr_y - 20),
        f"Now: {deltas[-1]:+d}",
        fill=curr_color,
        font=label_font,
    )

    # Legend — one swatch per source actually present in the series.
    sources_present: list[str] = []
    seen: set[str] = set()
    for info in infos:
        src = info.get("source", "")
        if src and src not in seen and src in SOURCE_COLORS:
            seen.add(src)
            sources_present.append(src)

    legend_y = chart_y + chart_height + 22
    legend_font = _get_font(13)
    swatch_size = 10
    cursor_x = padding
    max_x = width - padding
    for src in sources_present:
        label = SOURCE_LABELS.get(src, src)
        label_w = _get_text_size(legend_font, label)[0]
        entry_width = swatch_size + 4 + label_w + 14
        # Drop remaining sources if they'd overflow the legend row. In practice the
        # seven supported sources fit comfortably — this is a safety cap, not a layout.
        if cursor_x + entry_width > max_x and cursor_x != padding:
            break
        draw.ellipse(
            [
                (cursor_x, legend_y + 1),
                (cursor_x + swatch_size, legend_y + 1 + swatch_size),
            ],
            fill=SOURCE_COLORS[src],
            outline=DISCORD_WHITE,
        )
        draw.text(
            (cursor_x + swatch_size + 4, legend_y - 1),
            label,
            fill=DISCORD_GREY,
            font=legend_font,
        )
        cursor_x += entry_width

    # Footer — compact per-source net totals.
    footer_y = height - footer_height + 5
    stat_font = _get_font(15)
    if per_source_totals:
        totals_parts = [
            f"{SOURCE_LABELS.get(src, src)} {val:+d}"
            for src, val in sorted(per_source_totals.items(), key=lambda kv: -abs(kv[1]))
        ]
        footer_text = "  |  ".join(totals_parts)
    else:
        footer_text = "No activity"

    text_w = _get_text_size(stat_font, footer_text)[0]
    if text_w > width - padding * 2:
        # Fall back to a terse summary when too many sources would overflow.
        footer_text = f"{len(series)} events | net {net_total:+d}"
        text_w = _get_text_size(stat_font, footer_text)[0]
    draw.text(((width - text_w) // 2, footer_y), footer_text, fill=DISCORD_WHITE, font=stat_font)

    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp
