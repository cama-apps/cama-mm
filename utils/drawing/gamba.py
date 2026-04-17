"""Gamba P&L chart with bet markers (``draw_gamba_chart``)."""

from __future__ import annotations

import math
from io import BytesIO

from PIL import Image, ImageDraw

from utils.drawing._common import (
    DISCORD_ACCENT,
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


def draw_gamba_chart(
    username: str,
    degen_score: int,
    degen_title: str,
    degen_emoji: str,
    pnl_series: list[tuple[int, int, dict]],
    stats: dict,
) -> BytesIO:
    """
    Generate a cumulative P&L chart with bet markers.

    Args:
        username: Player's display name
        degen_score: Degen score (0-100)
        degen_title: Degen tier title
        degen_emoji: Degen tier emoji
        pnl_series: List of (bet_number, cumulative_pnl, bet_info) tuples
        stats: Dict with total_bets, win_rate, net_pnl, roi

    Returns:
        BytesIO containing the PNG image
    """
    # Image dimensions
    width = 700
    height = 400
    padding = 60
    header_height = 50
    footer_height = 40
    chart_width = width - padding * 2
    chart_height = height - header_height - footer_height - padding

    # Create image
    img = Image.new("RGBA", (width, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Fonts
    title_font = _get_font(22)
    subtitle_font = _get_font(16)
    label_font = _get_font(14)
    value_font = _get_font(13)

    # Draw header
    title = f"{username}'s Gamba Journey"
    draw.text((padding, 12), title, fill=DISCORD_WHITE, font=title_font)

    subtitle = f"Degen Score: {degen_score} {degen_emoji} \"{degen_title}\""
    draw.text((padding, 38), subtitle, fill=DISCORD_GREY, font=subtitle_font)

    # Handle empty data
    if not pnl_series:
        draw.text(
            (width // 2 - 60, height // 2),
            "No betting history",
            fill=DISCORD_GREY,
            font=title_font,
        )
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Extract data
    bet_nums = [p[0] for p in pnl_series]
    pnl_values = [p[1] for p in pnl_series]
    bet_infos = [p[2] for p in pnl_series]

    # Chart origin
    chart_x = padding
    chart_y = header_height + 20

    proj = make_projection(
        y_values=[float(p) for p in pnl_values],
        total_x=len(bet_nums),
        chart_x=chart_x,
        chart_y=chart_y,
        chart_width=chart_width,
        chart_height=chart_height,
    )

    def to_pixel(bet_num: int, pnl: int) -> tuple[int, int]:
        return proj.to_pixel(bet_num, pnl)

    zero_y = draw_zero_line(draw, proj)
    draw_y_axis_labels(draw, proj, min(pnl_values), max(pnl_values))
    draw_x_axis_labels(draw, proj, bet_nums)

    # Draw filled area under/over the line
    # Create overlay for transparency
    overlay = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    overlay_draw = ImageDraw.Draw(overlay)

    # Build polygon points for fill
    if len(pnl_series) > 1:
        # Positive area (above zero)
        pos_points = [(chart_x, zero_y)]
        for bet_num, pnl, _ in pnl_series:
            px, py = to_pixel(bet_num, pnl)
            if pnl >= 0:
                pos_points.append((px, py))
            else:
                pos_points.append((px, zero_y))
        pos_points.append((chart_x + chart_width, zero_y))
        if len(pos_points) > 2:
            overlay_draw.polygon(pos_points, fill=(87, 242, 135, 60))  # Green with alpha

        # Negative area (below zero)
        neg_points = [(chart_x, zero_y)]
        for bet_num, pnl, _ in pnl_series:
            px, py = to_pixel(bet_num, pnl)
            if pnl <= 0:
                neg_points.append((px, py))
            else:
                neg_points.append((px, zero_y))
        neg_points.append((chart_x + chart_width, zero_y))
        if len(neg_points) > 2:
            overlay_draw.polygon(neg_points, fill=(237, 66, 69, 60))  # Red with alpha

        img = Image.alpha_composite(img, overlay)
        draw = ImageDraw.Draw(img)

    # Draw the P&L line
    if len(pnl_series) > 1:
        line_points = [to_pixel(p[0], p[1]) for p in pnl_series]
        for i in range(len(line_points) - 1):
            # Color based on direction
            if pnl_values[i + 1] >= pnl_values[i]:
                color = DISCORD_GREEN
            else:
                color = DISCORD_RED
            draw.line([line_points[i], line_points[i + 1]], fill=color, width=2)

    # Draw bet/wheel markers
    # Calculate max bet size for scaling (only consider bets, not wheel spins)
    bet_sizes = [b["effective_bet"] for b in bet_infos if b.get("source", "bet") == "bet" and b["effective_bet"] > 0]
    max_bet_size = max(bet_sizes) if bet_sizes else 1

    # Calculate max double or nothing size for scaling
    don_sizes = [b["effective_bet"] for b in bet_infos if b.get("source") == "double_or_nothing" and b["effective_bet"] > 0]
    max_don_size = max(don_sizes) if don_sizes else 1

    for bet_num, pnl, info in pnl_series:
        px, py = to_pixel(bet_num, pnl)
        source = info.get("source", "bet")

        # Color based on outcome
        if info["outcome"] == "won":
            color = DISCORD_GREEN
        elif info["outcome"] == "neutral":
            color = DISCORD_GREY  # Neutral (wheel lose-a-turn)
        else:
            color = DISCORD_RED

        if source == "wheel":
            # Wheel icon: a circle with spokes (like a wheel of fortune)
            size = 6  # Fixed size for wheel icons
            # Outer circle
            draw.ellipse(
                [(px - size, py - size), (px + size, py + size)],
                fill=color,
                outline=DISCORD_WHITE,
            )
            # Draw spokes (4 lines through center)
            spoke_len = size - 1
            for angle in [0, 45, 90, 135]:
                rad = math.radians(angle)
                x1 = px + int(spoke_len * math.cos(rad))
                y1 = py + int(spoke_len * math.sin(rad))
                x2 = px - int(spoke_len * math.cos(rad))
                y2 = py - int(spoke_len * math.sin(rad))
                draw.line([(x1, y1), (x2, y2)], fill=DISCORD_WHITE, width=1)
        elif source == "double_or_nothing":
            # Star/burst marker for Double or Nothing
            # Size scaled by amount risked (5-10 pixels)
            size = 5 + int((info["effective_bet"] / max_don_size) * 5)
            # Draw 8-pointed star
            points = []
            for i in range(16):
                angle = math.radians(i * 22.5 - 90)  # Start from top
                # Alternate between outer and inner radius
                r = size if i % 2 == 0 else size * 0.4
                x = px + int(r * math.cos(angle))
                y = py + int(r * math.sin(angle))
                points.append((x, y))
            draw.polygon(points, fill=color, outline=DISCORD_WHITE)
        else:
            # Regular bet marker
            # Size based on bet amount (3-8 pixels)
            size = 3 + int((info["effective_bet"] / max_bet_size) * 5)

            # Diamond shape for leveraged bets, circle for normal
            if info["leverage"] > 1:
                # Diamond
                points = [
                    (px, py - size),
                    (px + size, py),
                    (px, py + size),
                    (px - size, py),
                ]
                draw.polygon(points, fill=color, outline=DISCORD_WHITE)
            else:
                # Circle
                draw.ellipse(
                    [(px - size, py - size), (px + size, py + size)],
                    fill=color,
                )

    # Annotate peak and trough
    peak_idx = pnl_values.index(max(pnl_values))
    trough_idx = pnl_values.index(min(pnl_values))

    peak_x, peak_y = to_pixel(bet_nums[peak_idx], pnl_values[peak_idx])
    trough_x, trough_y = to_pixel(bet_nums[trough_idx], pnl_values[trough_idx])

    # Peak annotation
    if pnl_values[peak_idx] > 0:
        peak_text = f"Peak: +{pnl_values[peak_idx]}"
        draw.text((peak_x + 5, peak_y - 15), peak_text, fill=DISCORD_GREEN, font=value_font)

    # Trough annotation
    if pnl_values[trough_idx] < 0:
        trough_text = f"Trough: {pnl_values[trough_idx]}"
        draw.text((trough_x + 5, trough_y + 5), trough_text, fill=DISCORD_RED, font=value_font)

    # Current position annotation
    curr_x, curr_y = to_pixel(bet_nums[-1], pnl_values[-1])
    curr_color = DISCORD_GREEN if pnl_values[-1] >= 0 else DISCORD_RED
    curr_text = f"Now: {pnl_values[-1]:+d}"
    draw.text((curr_x - 40, curr_y - 20), curr_text, fill=curr_color, font=label_font)

    # Draw legend
    legend_y = chart_y + chart_height + 22
    legend_font = _get_font(14)
    marker_size = 12

    # Win marker
    draw.ellipse(
        [(padding, legend_y), (padding + marker_size, legend_y + marker_size)],
        fill=DISCORD_GREEN,
    )
    draw.text((padding + marker_size + 5, legend_y - 1), "Win", fill=DISCORD_GREY, font=legend_font)

    # Loss marker
    lx_loss = padding + 70
    draw.ellipse(
        [(lx_loss, legend_y), (lx_loss + marker_size, legend_y + marker_size)],
        fill=DISCORD_RED,
    )
    draw.text((lx_loss + marker_size + 5, legend_y - 1), "Loss", fill=DISCORD_GREY, font=legend_font)

    # Leveraged marker (diamond)
    lx = padding + 150
    half = marker_size // 2
    draw.polygon(
        [
            (lx + half, legend_y),
            (lx + marker_size, legend_y + half),
            (lx + half, legend_y + marker_size),
            (lx, legend_y + half),
        ],
        fill=DISCORD_ACCENT,
        outline=DISCORD_WHITE,
    )
    draw.text((lx + marker_size + 5, legend_y - 1), "Leveraged", fill=DISCORD_GREY, font=legend_font)

    # Double or Nothing marker (star)
    lx_don = padding + 260
    center_x = lx_don + half
    center_y = legend_y + half
    star_size = half
    star_points = []
    for i in range(16):
        angle = math.radians(i * 22.5 - 90)
        r = star_size if i % 2 == 0 else star_size * 0.4
        x = center_x + int(r * math.cos(angle))
        y = center_y + int(r * math.sin(angle))
        star_points.append((x, y))
    draw.polygon(star_points, fill=DISCORD_ACCENT, outline=DISCORD_WHITE)
    draw.text((lx_don + marker_size + 5, legend_y - 1), "DoN", fill=DISCORD_GREY, font=legend_font)

    # Draw footer stats
    footer_y = height - footer_height + 5
    stat_font = _get_font(15)

    net_pnl = stats.get("net_pnl", 0)
    pnl_text = f"{net_pnl:+d}" if net_pnl != 0 else "0"

    footer_parts = [
        f"{stats.get('total_bets', 0)} bets",
        f"{stats.get('win_rate', 0):.0%} WR",
        f"{pnl_text} {'profit' if net_pnl >= 0 else 'loss'}",
        f"ROI {stats.get('roi', 0):+.1%}",
    ]

    # Draw footer stats centered
    footer_text = "  |  ".join(footer_parts)
    text_w = _get_text_size(stat_font, footer_text)[0]
    draw.text(((width - text_w) // 2, footer_y), footer_text, fill=DISCORD_WHITE, font=stat_font)

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp

