"""Gamba P&L chart with adaptive event markers (``draw_gamba_chart``)."""

from __future__ import annotations

import math
from io import BytesIO

from PIL import Image, ImageDraw

from utils.drawing._common import (
    DISCORD_ACCENT,
    DISCORD_BG,
    DISCORD_DARKER,
    DISCORD_GREEN,
    DISCORD_GREY,
    DISCORD_GRID,
    DISCORD_RED,
    DISCORD_WHITE,
    _get_font,
    _get_text_size,
    draw_x_axis_labels,
    draw_y_axis_labels,
    draw_zero_line,
    make_projection,
)


def _marker_radius(info: dict) -> int:
    """Return a fixed, legible radius for each explained marker type."""
    source = info.get("source", "bet")
    if source == "double_or_nothing":
        return 6
    if source == "wheel" or info.get("leverage", 1) > 1:
        return 5
    return 3


def _marker_priority(info: dict) -> int:
    """Prioritize distinctive event types when nearby markers compete for space."""
    if info.get("source") == "double_or_nothing":
        return 4
    if info.get("leverage", 1) > 1:
        return 3
    if info.get("source") == "wheel":
        return 2
    return 1


def _select_marker_indices(
    points: list[tuple[int, int]],
    infos: list[dict],
    radii: list[int],
    *,
    required_indices: set[int] | None = None,
    min_gap: int = 3,
) -> list[int]:
    """Cull overlapping markers while retaining the full-resolution P&L line.

    Peak, trough, and endpoint markers can be passed as required. Other events are
    selected by semantic priority, then impact, so special event shapes survive a
    dense run without turning hundreds of points into an unreadable blob.
    """
    if not (len(points) == len(infos) == len(radii)):
        raise ValueError("marker points, infos, and radii must have equal lengths")

    required = sorted(i for i in (required_indices or set()) if 0 <= i < len(points))
    selected = list(required)
    selected_set = set(required)

    candidates = sorted(
        (i for i in range(len(points)) if i not in selected_set),
        key=lambda i: (
            -_marker_priority(infos[i]),
            -abs(infos[i].get("profit", 0)),
            i,
        ),
    )
    for index in candidates:
        x_pos, y_pos = points[index]
        radius = radii[index]
        if all(
            (x_pos - points[other][0]) ** 2 + (y_pos - points[other][1]) ** 2
            >= (radius + radii[other] + min_gap) ** 2
            for other in selected
        ):
            selected.append(index)

    return sorted(selected)


def _star_points(center_x: int, center_y: int, radius: int) -> list[tuple[int, int]]:
    points: list[tuple[int, int]] = []
    for i in range(16):
        angle = math.radians(i * 22.5 - 90)
        point_radius = radius if i % 2 == 0 else radius * 0.42
        points.append(
            (
                center_x + int(point_radius * math.cos(angle)),
                center_y + int(point_radius * math.sin(angle)),
            )
        )
    return points


def _draw_event_marker(
    draw: ImageDraw.ImageDraw,
    point: tuple[int, int],
    info: dict,
    color: str,
    radius: int,
) -> None:
    """Draw one fixed-size event marker using the legend's shape language."""
    x_pos, y_pos = point
    source = info.get("source", "bet")

    if source == "wheel":
        draw.ellipse(
            [(x_pos - radius, y_pos - radius), (x_pos + radius, y_pos + radius)],
            fill=color,
            outline=DISCORD_DARKER,
        )
        spoke_radius = radius - 2
        draw.line(
            [(x_pos - spoke_radius, y_pos), (x_pos + spoke_radius, y_pos)],
            fill=DISCORD_WHITE,
            width=1,
        )
        draw.line(
            [(x_pos, y_pos - spoke_radius), (x_pos, y_pos + spoke_radius)],
            fill=DISCORD_WHITE,
            width=1,
        )
    elif source == "double_or_nothing":
        draw.polygon(
            _star_points(x_pos, y_pos, radius),
            fill=color,
            outline=DISCORD_DARKER,
        )
    elif info.get("leverage", 1) > 1:
        draw.polygon(
            [
                (x_pos, y_pos - radius),
                (x_pos + radius, y_pos),
                (x_pos, y_pos + radius),
                (x_pos - radius, y_pos),
            ],
            fill=color,
            outline=DISCORD_DARKER,
        )
    else:
        draw.ellipse(
            [(x_pos - radius, y_pos - radius), (x_pos + radius, y_pos + radius)],
            fill=color,
        )


def _outcome_color(info: dict) -> str:
    outcome = info.get("outcome")
    if outcome == "won":
        return DISCORD_GREEN
    if outcome == "neutral":
        return DISCORD_GREY
    return DISCORD_RED


def _draw_legend(
    draw: ImageDraw.ImageDraw,
    *,
    x_pos: int,
    y_pos: int,
    infos: list[dict],
) -> None:
    """Draw compact outcome and event-type groups above the plot."""
    group_font = _get_font(9)
    label_font = _get_font(12)
    marker_radius = 4
    cursor_x = x_pos

    def group_label(label: str) -> None:
        nonlocal cursor_x
        draw.text((cursor_x, y_pos + 1), label, fill=DISCORD_GREY, font=group_font)
        cursor_x += _get_text_size(group_font, label)[0] + 10

    def text_label(label: str) -> None:
        nonlocal cursor_x
        draw.text((cursor_x, y_pos), label, fill=DISCORD_GREY, font=label_font)
        cursor_x += _get_text_size(label_font, label)[0] + 14

    outcomes = {info.get("outcome") for info in infos}
    group_label("OUTCOME")
    for outcome, label, color in (
        ("won", "Win", DISCORD_GREEN),
        ("lost", "Loss", DISCORD_RED),
        ("neutral", "Neutral", DISCORD_GREY),
    ):
        if outcome not in outcomes:
            continue
        center = (cursor_x + marker_radius, y_pos + 6)
        draw.ellipse(
            [
                (center[0] - marker_radius, center[1] - marker_radius),
                (center[0] + marker_radius, center[1] + marker_radius),
            ],
            fill=color,
        )
        cursor_x += marker_radius * 2 + 4
        text_label(label)

    type_entries: list[tuple[str, dict]] = []
    if any(info.get("leverage", 1) > 1 for info in infos):
        type_entries.append(("Leverage", {"source": "bet", "leverage": 2}))
    if any(info.get("source") == "double_or_nothing" for info in infos):
        type_entries.append(("DoN", {"source": "double_or_nothing"}))
    if any(info.get("source") == "wheel" for info in infos):
        type_entries.append(("Wheel", {"source": "wheel"}))

    if type_entries:
        cursor_x += 4
        group_label("TYPE")
        for label, info in type_entries:
            type_radius = 5
            center = (cursor_x + type_radius, y_pos + 6)
            _draw_event_marker(draw, center, info, DISCORD_ACCENT, type_radius)
            cursor_x += type_radius * 2 + 4
            text_label(label)


def _boxes_overlap(
    first: tuple[int, int, int, int], second: tuple[int, int, int, int]
) -> bool:
    return not (
        first[2] + 3 <= second[0]
        or second[2] + 3 <= first[0]
        or first[3] + 3 <= second[1]
        or second[3] + 3 <= first[1]
    )


def _draw_callout(
    draw: ImageDraw.ImageDraw,
    *,
    point: tuple[int, int],
    text: str,
    color: str,
    font,
    bounds: tuple[int, int, int, int],
    occupied: list[tuple[int, int, int, int]],
    prefer_left: bool,
    prefer_above: bool,
) -> tuple[int, int, int, int]:
    """Place a readable label pill near a point and clamp it to the plot."""
    text_width, text_height = _get_text_size(font, text)
    box_width = text_width + 12
    box_height = text_height + 8
    point_x, point_y = point
    left, top, right, bottom = bounds

    horizontal_options = (prefer_left, not prefer_left)
    vertical_options = (prefer_above, not prefer_above)
    placements: list[tuple[int, int, int, int]] = []
    for place_above in vertical_options:
        for place_left in horizontal_options:
            raw_x = point_x - box_width - 8 if place_left else point_x + 8
            raw_y = point_y - box_height - 8 if place_above else point_y + 8
            box_x = min(max(raw_x, left + 3), right - box_width - 3)
            box_y = min(max(raw_y, top + 3), bottom - box_height - 3)
            placements.append((box_x, box_y, box_x + box_width, box_y + box_height))

    box = next(
        (
            candidate
            for candidate in placements
            if not any(_boxes_overlap(candidate, used) for used in occupied)
        ),
        placements[0],
    )
    anchor_x = min(max(point_x, box[0]), box[2])
    anchor_y = min(max(point_y, box[1]), box[3])
    draw.line([point, (anchor_x, anchor_y)], fill=color, width=1)
    draw.rounded_rectangle(box, radius=5, fill=DISCORD_DARKER, outline=DISCORD_GRID)
    draw.text((box[0] + 6, box[1] + 3), text, fill=color, font=font)
    occupied.append(box)
    return box


def _draw_centered_text(
    draw: ImageDraw.ImageDraw,
    text: str,
    font,
    center_x: int,
    y_pos: int,
    color: str,
) -> None:
    text_width = _get_text_size(font, text)[0]
    draw.text((center_x - text_width // 2, y_pos), text, fill=color, font=font)


def _draw_stat_strip(
    draw: ImageDraw.ImageDraw,
    *,
    width: int,
    padding: int,
    y_pos: int,
    event_count: int,
    stats: dict,
) -> None:
    """Separate all-event journey scope from bet-only performance metrics."""
    strip = (padding, y_pos, width - padding, y_pos + 43)
    draw.rounded_rectangle(strip, radius=7, fill=DISCORD_DARKER, outline=DISCORD_GRID)

    win_rate = stats.get("win_rate", 0)
    net_pnl = stats.get("net_pnl", 0)
    roi = stats.get("roi", 0)
    metrics = [
        ("EVENTS", f"{event_count:,}", DISCORD_WHITE),
        ("BETS", f"{stats.get('total_bets', 0):,}", DISCORD_WHITE),
        ("BET WIN RATE", f"{win_rate:.0%}", DISCORD_GREEN if win_rate >= 0.5 else DISCORD_RED),
        ("BET P&L", f"{net_pnl:+,d}", DISCORD_GREEN if net_pnl >= 0 else DISCORD_RED),
        ("BET ROI", f"{roi:+.1%}", DISCORD_GREEN if roi >= 0 else DISCORD_RED),
    ]
    label_font = _get_font(9)
    value_font = _get_font(13)
    strip_width = strip[2] - strip[0]
    column_width = strip_width / len(metrics)

    for index, (label, value, color) in enumerate(metrics):
        center_x = int(strip[0] + column_width * (index + 0.5))
        if index:
            divider_x = int(strip[0] + column_width * index)
            draw.line(
                [(divider_x, strip[1] + 8), (divider_x, strip[3] - 8)],
                fill=DISCORD_GRID,
                width=1,
            )
        _draw_centered_text(draw, label, label_font, center_x, strip[1] + 5, DISCORD_GREY)
        _draw_centered_text(draw, value, value_font, center_x, strip[1] + 20, color)


def draw_gamba_chart(
    username: str,
    degen_score: int,
    degen_title: str,
    degen_emoji: str,
    pnl_series: list[tuple[int, int, dict]],
    stats: dict,
) -> BytesIO:
    """Generate a cumulative P&L chart with adaptive, collision-free markers."""
    width = 700
    height = 400
    padding = 60
    chart_x = padding
    chart_y = 88
    chart_width = width - chart_x - 26
    chart_height = 222

    img = Image.new("RGBA", (width, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    title_font = _get_font(22)
    subtitle_font = _get_font(15)
    value_font = _get_font(12)

    draw.text((padding, 12), f"{username}'s Gamba Journey", fill=DISCORD_WHITE, font=title_font)

    # Native color emoji do not render reliably through Pillow's text stack. The tier
    # name carries the same information without producing a missing-glyph square.
    _ = degen_emoji
    subtitle = f"Degen Score {degen_score}"
    if degen_title:
        subtitle += f"  ·  {degen_title}"
    draw.text((padding, 40), subtitle, fill=DISCORD_GREY, font=subtitle_font)

    if not pnl_series:
        empty_font = _get_font(18)
        message = "No betting history"
        message_width = _get_text_size(empty_font, message)[0]
        draw.text(
            ((width - message_width) // 2, height // 2),
            message,
            fill=DISCORD_GREY,
            font=empty_font,
        )
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    bet_nums = [point[0] for point in pnl_series]
    pnl_values = [point[1] for point in pnl_series]
    bet_infos = [point[2] for point in pnl_series]

    _draw_legend(draw, x_pos=padding, y_pos=64, infos=bet_infos)

    proj = make_projection(
        y_values=[float(pnl) for pnl in pnl_values],
        total_x=len(bet_nums),
        chart_x=chart_x,
        chart_y=chart_y,
        chart_width=chart_width,
        chart_height=chart_height,
    )
    line_points = [proj.to_pixel(point[0], point[1]) for point in pnl_series]

    zero_y = draw_zero_line(draw, proj)
    draw_y_axis_labels(draw, proj, min(pnl_values), max(pnl_values))
    draw_x_axis_labels(draw, proj, bet_nums)

    if len(pnl_series) > 1:
        overlay = Image.new("RGBA", (width, height), (0, 0, 0, 0))
        overlay_draw = ImageDraw.Draw(overlay)

        positive_points = [(chart_x, zero_y)]
        negative_points = [(chart_x, zero_y)]
        for (event_number, pnl, _), (x_pos, y_pos) in zip(pnl_series, line_points, strict=True):
            _ = event_number
            positive_points.append((x_pos, y_pos if pnl >= 0 else zero_y))
            negative_points.append((x_pos, y_pos if pnl <= 0 else zero_y))
        positive_points.append((chart_x + chart_width, zero_y))
        negative_points.append((chart_x + chart_width, zero_y))
        overlay_draw.polygon(positive_points, fill=(87, 242, 135, 38))
        overlay_draw.polygon(negative_points, fill=(237, 66, 69, 38))
        img = Image.alpha_composite(img, overlay)
        draw = ImageDraw.Draw(img)

        draw.line(line_points, fill=DISCORD_DARKER, width=4)
        for index in range(len(line_points) - 1):
            delta = pnl_values[index + 1] - pnl_values[index]
            color = DISCORD_GREEN if delta > 0 else DISCORD_RED if delta < 0 else DISCORD_GREY
            draw.line([line_points[index], line_points[index + 1]], fill=color, width=2)

    peak_index = pnl_values.index(max(pnl_values))
    trough_index = pnl_values.index(min(pnl_values))
    radii = [_marker_radius(info) for info in bet_infos]
    required_indices = {0, len(pnl_series) - 1, peak_index, trough_index}
    visible_marker_indices = _select_marker_indices(
        line_points,
        bet_infos,
        radii,
        required_indices=required_indices,
    )
    for index in visible_marker_indices:
        _draw_event_marker(
            draw,
            line_points[index],
            bet_infos[index],
            _outcome_color(bet_infos[index]),
            radii[index],
        )

    scale_font = _get_font(9)
    draw.text(
        (chart_x + 7, chart_y + 5),
        "P&L · SIGNED LOG SCALE",
        fill=DISCORD_GREY,
        font=scale_font,
    )

    occupied: list[tuple[int, int, int, int]] = []
    bounds = (chart_x, chart_y, chart_x + chart_width, chart_y + chart_height)
    if pnl_values[peak_index] > 0 and peak_index != len(pnl_series) - 1:
        _draw_callout(
            draw,
            point=line_points[peak_index],
            text=f"Peak {pnl_values[peak_index]:+,d}",
            color=DISCORD_GREEN,
            font=value_font,
            bounds=bounds,
            occupied=occupied,
            prefer_left=line_points[peak_index][0] > chart_x + chart_width * 0.65,
            prefer_above=True,
        )
    if pnl_values[trough_index] < 0 and trough_index != len(pnl_series) - 1:
        _draw_callout(
            draw,
            point=line_points[trough_index],
            text=f"Low {pnl_values[trough_index]:+,d}",
            color=DISCORD_RED,
            font=value_font,
            bounds=bounds,
            occupied=occupied,
            prefer_left=line_points[trough_index][0] > chart_x + chart_width * 0.65,
            prefer_above=False,
        )

    current_value = pnl_values[-1]
    current_color = DISCORD_GREEN if current_value >= 0 else DISCORD_RED
    current_label = "Now"
    if len(pnl_series) - 1 == peak_index:
        current_label = "Now · peak"
    elif len(pnl_series) - 1 == trough_index:
        current_label = "Now · low"
    _draw_callout(
        draw,
        point=line_points[-1],
        text=f"{current_label} {current_value:+,d}",
        color=current_color,
        font=value_font,
        bounds=bounds,
        occupied=occupied,
        prefer_left=True,
        prefer_above=True,
    )

    _draw_stat_strip(
        draw,
        width=width,
        padding=padding,
        y_pos=346,
        event_count=len(pnl_series),
        stats=stats,
    )

    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp
