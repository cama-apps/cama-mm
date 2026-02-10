"""
Image generation utilities for Dota 2 stats visualization.
"""

import math
from io import BytesIO

import matplotlib.pyplot as plt
import numpy as np
from PIL import Image, ImageDraw, ImageFont
from scipy import stats

# Discord-like dark theme colors
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


def _get_font(size: int = 16) -> ImageFont.FreeTypeFont:
    """Get a font, falling back to default if custom fonts unavailable."""
    try:
        # Try to use DejaVu Sans which is commonly available on Linux
        return ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", size)
    except OSError:
        try:
            # Try Arial on Windows
            return ImageFont.truetype("arial.ttf", size)
        except OSError:
            # Fall back to default
            return ImageFont.load_default()


def _get_text_size(font: ImageFont.FreeTypeFont, text: str) -> tuple[int, int]:
    """Get text dimensions."""
    bbox = font.getbbox(text)
    return (bbox[2] - bbox[0], bbox[3] - bbox[1])


def draw_matches_table(
    matches: list[dict],
    hero_names: dict[int, str] | None = None,
) -> BytesIO:
    """
    Generate a PNG image of recent matches table.

    Args:
        matches: List of match dicts with keys: hero_id, kills, deaths, assists,
                 duration, won, match_id, game_mode (optional)
        hero_names: Optional dict mapping hero_id to hero name

    Returns:
        BytesIO containing the PNG image
    """
    if not matches:
        # Return empty image
        img = Image.new("RGBA", (400, 100), DISCORD_BG)
        draw = ImageDraw.Draw(img)
        font = _get_font(20)
        draw.text((20, 40), "No matches found", fill=DISCORD_GREY, font=font)
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Fonts
    header_font = _get_font(20)
    cell_font = _get_font(16)

    # Column definitions: (header, width, align)
    columns = [
        ("Hero", 120, "left"),
        ("K", 35, "center"),
        ("D", 35, "center"),
        ("A", 35, "center"),
        ("Result", 55, "center"),
        ("Duration", 70, "center"),
    ]

    # Calculate dimensions
    row_height = 36
    header_height = 32
    padding = 10
    total_width = sum(c[1] for c in columns) + padding * 2
    total_height = header_height + len(matches) * row_height + padding * 2

    # Create image
    img = Image.new("RGBA", (total_width, total_height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Draw header row
    x = padding
    y = padding
    for header, width, _ in columns:
        draw.text((x + 5, y + 8), header, fill=DISCORD_WHITE, font=header_font)
        x += width

    # Header underline
    draw.line(
        [(padding, y + header_height - 2), (total_width - padding, y + header_height - 2)],
        fill=DISCORD_ACCENT,
        width=2,
    )

    # Draw match rows
    y = padding + header_height
    for i, match in enumerate(matches):
        # Alternate row background
        if i % 2 == 1:
            draw.rectangle(
                [(padding, y), (total_width - padding, y + row_height)],
                fill=DISCORD_DARKER,
            )

        x = padding

        # Hero name
        hero_id = match.get("hero_id", 0)
        hero_name = "Unknown"
        if hero_names and hero_id in hero_names:
            hero_name = hero_names[hero_id]
        elif match.get("hero_name"):
            hero_name = match["hero_name"]

        # Truncate long hero names
        if len(hero_name) > 14:
            hero_name = hero_name[:12] + ".."

        draw.text((x + 5, y + 10), hero_name, fill=DISCORD_WHITE, font=cell_font)
        x += columns[0][1]

        # KDA
        kills = str(match.get("kills", 0))
        deaths = str(match.get("deaths", 0))
        assists = str(match.get("assists", 0))

        for val, (_, width, _) in zip([kills, deaths, assists], columns[1:4]):
            text_w = _get_text_size(cell_font, val)[0]
            draw.text((x + (width - text_w) // 2, y + 10), val, fill=DISCORD_WHITE, font=cell_font)
            x += width

        # Result
        won = match.get("won", match.get("radiant_win"))
        if isinstance(won, bool):
            result_text = "Win" if won else "Loss"
            result_color = DISCORD_GREEN if won else DISCORD_RED
        else:
            result_text = "?"
            result_color = DISCORD_GREY

        text_w = _get_text_size(cell_font, result_text)[0]
        draw.text(
            (x + (columns[4][1] - text_w) // 2, y + 10),
            result_text,
            fill=result_color,
            font=cell_font,
        )
        x += columns[4][1]

        # Duration
        duration = match.get("duration", 0)
        if duration:
            mins = duration // 60
            secs = duration % 60
            duration_text = f"{mins}:{secs:02d}"
        else:
            duration_text = "-"

        text_w = _get_text_size(cell_font, duration_text)[0]
        draw.text(
            (x + (columns[5][1] - text_w) // 2, y + 10),
            duration_text,
            fill=DISCORD_GREY,
            font=cell_font,
        )

        y += row_height

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp


def draw_role_graph(
    role_values: dict[str, float],
    title: str = "Role Distribution",
) -> BytesIO:
    """
    Generate a radar/polygon graph showing role distribution.

    Args:
        role_values: Dict mapping role names to values (0-100 scale)
        title: Title for the graph

    Returns:
        BytesIO containing the PNG image
    """
    # Image dimensions
    size = 400
    center = (size // 2, size // 2 + 15)  # Offset for title
    radius = 140

    # Create image
    img = Image.new("RGBA", (size, size), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Draw title
    title_font = _get_font(22)
    title_w = _get_text_size(title_font, title)[0]
    draw.text(((size - title_w) // 2, 8), title, fill=DISCORD_WHITE, font=title_font)

    # Use fixed role order for consistent positioning across graphs
    # Always include all roles for visual consistency (0 value for missing roles)
    roles = list(ROLE_ORDER)
    # Add any roles not in ROLE_ORDER (shouldn't happen, but be safe)
    for r in role_values:
        if r not in roles:
            roles.append(r)
    raw_values = [role_values.get(r, 0) for r in roles]

    # Auto-scale: find the max value and scale so max reaches ~90% of radius
    # This makes the graph visually meaningful even when values are small percentages
    max_val = max(raw_values) if raw_values else 1
    # Round up to a nice scale (next multiple of 5 or 10)
    if max_val <= 10:
        scale_max = 10
    elif max_val <= 25:
        scale_max = ((int(max_val) // 5) + 1) * 5  # Round to next 5
    else:
        scale_max = ((int(max_val) // 10) + 1) * 10  # Round to next 10

    values = [v / scale_max for v in raw_values]  # Normalize to 0-1 based on scale_max
    n = len(roles)

    if n < 3:
        # Not enough data for polygon
        label_font = _get_font(14)
        draw.text((size // 4, size // 2), "Not enough data", fill=DISCORD_GREY, font=label_font)
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Calculate polygon points for the background grid
    def get_points(r: float, scale: float = 1.0) -> list[tuple[float, float]]:
        points = []
        for i in range(n):
            angle = (2 * math.pi * i / n) - (math.pi / 2)  # Start from top
            px = center[0] + r * scale * math.cos(angle)
            py = center[1] + r * scale * math.sin(angle)
            points.append((px, py))
        return points

    # Draw grid circles (at 25%, 50%, 75%, 100% of scale_max)
    scale_font = _get_font(12)
    for pct in [0.25, 0.5, 0.75, 1.0]:
        grid_points = get_points(radius, pct)
        draw.polygon(grid_points, outline=DISCORD_DARKER)

        # Add scale label on right side of each ring
        label_val = int(scale_max * pct)
        label_text = f"{label_val}%"
        # Position slightly to the right of center
        label_x = center[0] + radius * pct + 3
        label_y = center[1] - 5
        draw.text((label_x, label_y), label_text, fill=DISCORD_GREY, font=scale_font)

    # Draw grid lines from center to each vertex
    outer_points = get_points(radius)
    for point in outer_points:
        draw.line([center, point], fill=DISCORD_DARKER, width=1)

    # Draw data polygon
    data_points = []
    for i, val in enumerate(values):
        angle = (2 * math.pi * i / n) - (math.pi / 2)
        px = center[0] + radius * val * math.cos(angle)
        py = center[1] + radius * val * math.sin(angle)
        data_points.append((px, py))

    # Draw filled polygon with transparency
    overlay = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    overlay_draw = ImageDraw.Draw(overlay)
    overlay_draw.polygon(data_points, fill=(88, 101, 242, 100))  # DISCORD_ACCENT with alpha
    img = Image.alpha_composite(img, overlay)
    draw = ImageDraw.Draw(img)

    # Draw polygon outline
    draw.polygon(data_points, outline=DISCORD_ACCENT)

    # Draw data points
    for point in data_points:
        r = 4
        draw.ellipse(
            [(point[0] - r, point[1] - r), (point[0] + r, point[1] + r)],
            fill=DISCORD_ACCENT,
        )

    # Draw labels
    label_font = _get_font(14)
    label_offset = 22
    for i, role in enumerate(roles):
        angle = (2 * math.pi * i / n) - (math.pi / 2)
        lx = center[0] + (radius + label_offset) * math.cos(angle)
        ly = center[1] + (radius + label_offset) * math.sin(angle)

        # Adjust label position based on angle
        text_w, text_h = _get_text_size(label_font, role)

        # Horizontal adjustment
        if lx < center[0] - 10:
            lx -= text_w
        elif abs(lx - center[0]) < 10:
            lx -= text_w // 2

        # Vertical adjustment
        if ly < center[1] - 10:
            ly -= text_h
        elif abs(ly - center[1]) < 10:
            ly -= text_h // 2

        # Draw label with value
        pct_text = f"{int(role_values.get(role, 0))}%"
        draw.text((lx, ly), role, fill=DISCORD_WHITE, font=label_font)
        draw.text((lx, ly + text_h), pct_text, fill=DISCORD_GREY, font=label_font)

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp


def draw_lane_distribution(lane_values: dict[str, float]) -> BytesIO:
    """
    Generate a horizontal bar chart for lane distribution.

    Args:
        lane_values: Dict mapping lane names to percentages (0-100)

    Returns:
        BytesIO containing the PNG image
    """
    # Image dimensions
    width = 350
    bar_height = 30
    padding = 15
    label_width = 80

    lanes = list(lane_values.keys())
    height = len(lanes) * (bar_height + 10) + padding * 2 + 30  # Extra for title

    # Create image
    img = Image.new("RGBA", (width, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Draw title
    title_font = _get_font(20)
    draw.text((padding, padding), "Lane Distribution", fill=DISCORD_WHITE, font=title_font)

    # Lane colors
    lane_colors = {
        "Safe Lane": "#4CAF50",
        "Mid": "#2196F3",
        "Off Lane": "#FF9800",
        "Jungle": "#9C27B0",
        "Roaming": "#E91E63",  # Pink for roaming/support
    }

    label_font = _get_font(16)
    value_font = _get_font(14)

    y = padding + 40
    bar_width = width - padding * 2 - label_width - 50

    for lane in lanes:
        value = lane_values.get(lane, 0)
        color = lane_colors.get(lane, DISCORD_ACCENT)

        # Draw label
        draw.text((padding, y + 7), lane, fill=DISCORD_WHITE, font=label_font)

        # Draw bar background
        bar_x = padding + label_width
        draw.rectangle(
            [(bar_x, y + 5), (bar_x + bar_width, y + bar_height - 5)],
            fill=DISCORD_DARKER,
        )

        # Draw bar fill
        fill_width = int(bar_width * value / 100)
        if fill_width > 0:
            draw.rectangle(
                [(bar_x, y + 5), (bar_x + fill_width, y + bar_height - 5)],
                fill=color,
            )

        # Draw percentage
        pct_text = f"{value:.0f}%"
        draw.text(
            (bar_x + bar_width + 8, y + 7),
            pct_text,
            fill=DISCORD_GREY,
            font=value_font,
        )

        y += bar_height + 10

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp


def draw_attribute_distribution(attr_values: dict[str, float]) -> BytesIO:
    """
    Generate a pie-chart style visualization for hero attribute distribution.

    Args:
        attr_values: Dict with keys 'str', 'agi', 'int', 'all' and percentage values

    Returns:
        BytesIO containing the PNG image
    """
    size = 300
    center = (size // 2, size // 2 + 20)
    radius = 80

    # Create image
    img = Image.new("RGBA", (size, size), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Draw title
    title_font = _get_font(20)
    title = "Hero Attributes"
    title_w = _get_text_size(title_font, title)[0]
    draw.text(((size - title_w) // 2, 10), title, fill=DISCORD_WHITE, font=title_font)

    # Attribute colors
    colors = {
        "str": "#E53935",  # Red
        "agi": "#43A047",  # Green
        "int": "#1E88E5",  # Blue
        "all": "#8E24AA",  # Purple
    }

    labels = {
        "str": "STR",
        "agi": "AGI",
        "int": "INT",
        "all": "UNI",
    }

    # Draw pie chart
    start_angle = -90
    for attr in ["str", "agi", "int", "all"]:
        value = attr_values.get(attr, 0)
        if value <= 0:
            continue

        sweep = value * 3.6  # Convert percentage to degrees
        end_angle = start_angle + sweep

        draw.pieslice(
            [
                (center[0] - radius, center[1] - radius),
                (center[0] + radius, center[1] + radius),
            ],
            start=start_angle,
            end=end_angle,
            fill=colors[attr],
            outline=DISCORD_BG,
        )
        start_angle = end_angle

    # Draw legend
    legend_font = _get_font(14)
    legend_y = size - 60
    legend_x = 20
    box_size = 14

    for attr in ["str", "agi", "int", "all"]:
        value = attr_values.get(attr, 0)
        if value <= 0:
            continue

        # Color box
        draw.rectangle(
            [(legend_x, legend_y), (legend_x + box_size, legend_y + box_size)],
            fill=colors[attr],
        )

        # Label
        label = f"{labels[attr]} {value:.0f}%"
        draw.text(
            (legend_x + box_size + 5, legend_y - 1), label, fill=DISCORD_WHITE, font=legend_font
        )

        legend_x += 70

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp


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

    # Signed log scale: sign(x) * log(1 + |x|) for better visualization of large swings
    def signed_log(x: float) -> float:
        """Apply signed log transformation to compress large values while preserving sign."""
        if x == 0:
            return 0.0
        sign = 1 if x > 0 else -1
        return sign * math.log1p(abs(x))

    def signed_log_inverse(y: float) -> float:
        """Inverse of signed_log for label generation."""
        if y == 0:
            return 0.0
        sign = 1 if y > 0 else -1
        return sign * (math.exp(abs(y)) - 1)

    # Transform P&L values to log scale for chart bounds
    log_pnl_values = [signed_log(p) for p in pnl_values]
    min_log_pnl = min(min(log_pnl_values), 0)
    max_log_pnl = max(max(log_pnl_values), 0)
    log_pnl_range = max(abs(max_log_pnl - min_log_pnl), 0.1)

    # Add 10% padding to range
    min_log_pnl -= log_pnl_range * 0.1
    max_log_pnl += log_pnl_range * 0.1
    log_pnl_range = max_log_pnl - min_log_pnl

    # Chart origin
    chart_x = padding
    chart_y = header_height + 20

    # Helper to convert data to pixel coordinates (using log scale for Y)
    def to_pixel(bet_num: int, pnl: int) -> tuple[int, int]:
        x = chart_x + int((bet_num - 1) / max(len(bet_nums) - 1, 1) * chart_width)
        log_pnl = signed_log(pnl)
        y = chart_y + int((max_log_pnl - log_pnl) / log_pnl_range * chart_height)
        return (x, y)

    # Draw zero line (using log scale position)
    zero_y = chart_y + int((max_log_pnl - 0) / log_pnl_range * chart_height)
    draw.line(
        [(chart_x, zero_y), (chart_x + chart_width, zero_y)],
        fill=DISCORD_GREY,
        width=1,
    )
    draw.text((chart_x - 25, zero_y - 6), "0", fill=DISCORD_GREY, font=value_font)

    # Draw Y-axis labels at log-spaced positions
    # Generate nice round values that span the actual data range
    actual_min = min(pnl_values)
    actual_max = max(pnl_values)

    # Generate label values at key points (powers of 10, nice round numbers)
    label_values = [0]
    for magnitude in [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000]:
        if magnitude <= abs(actual_max) * 1.2:
            label_values.append(magnitude)
        if -magnitude >= actual_min * 1.2:
            label_values.append(-magnitude)

    for pnl_val in sorted(set(label_values)):
        log_val = signed_log(pnl_val)
        # Skip if outside the visible range
        if log_val < min_log_pnl or log_val > max_log_pnl:
            continue
        y_pos = chart_y + int((max_log_pnl - log_val) / log_pnl_range * chart_height)
        if pnl_val == 0:
            continue  # Already drawn zero line
        label = f"{pnl_val:+d}"
        text_w = _get_text_size(value_font, label)[0]
        draw.text((chart_x - text_w - 8, y_pos - 6), label, fill=DISCORD_GREY, font=value_font)

    # Draw X-axis labels (bet numbers)
    x_step = max(len(bet_nums) // 5, 1)
    for i in range(0, len(bet_nums), x_step):
        x_pos, _ = to_pixel(bet_nums[i], 0)
        label = str(bet_nums[i])
        text_w = _get_text_size(value_font, label)[0]
        draw.text(
            (x_pos - text_w // 2, chart_y + chart_height + 5),
            label,
            fill=DISCORD_GREY,
            font=value_font,
        )

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
    pnl_color = DISCORD_GREEN if net_pnl >= 0 else DISCORD_RED
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


def draw_rating_distribution(
    ratings: list[float], avg_rating: float | None = None, median_rating: float | None = None
) -> BytesIO:
    """
    Generate a histogram with fitted normal distribution curve overlay.

    Args:
        ratings: List of player ratings
        avg_rating: Optional average rating to display
        median_rating: Optional median rating to display

    Returns:
        BytesIO containing the PNG image
    """
    if not ratings:
        # Return empty image if no data
        fig, ax = plt.subplots(figsize=(6.5, 4), facecolor="#36393F")
        ax.set_facecolor("#2F3136")
        ax.text(0.5, 0.5, "No rating data", ha="center", va="center", color="white", fontsize=14)
        ax.set_xticks([])
        ax.set_yticks([])
        fp = BytesIO()
        fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor="#36393F")
        plt.close(fig)
        fp.seek(0)
        return fp

    ratings_arr = np.array(ratings)

    # Calculate statistics
    mean = np.mean(ratings_arr)
    std = np.std(ratings_arr)
    skewness = stats.skew(ratings_arr)
    kurtosis = stats.kurtosis(ratings_arr)  # Excess kurtosis (0 = normal)

    # Shapiro-Wilk test for normality (only reliable for n < 5000)
    if len(ratings_arr) >= 3:
        if len(ratings_arr) <= 5000:
            shapiro_stat, shapiro_p = stats.shapiro(ratings_arr)
        else:
            # Use D'Agostino-Pearson for larger samples
            shapiro_stat, shapiro_p = stats.normaltest(ratings_arr)
    else:
        shapiro_stat, shapiro_p = None, None

    # Create figure with Discord-like dark theme
    fig, ax = plt.subplots(figsize=(6.5, 4), facecolor="#36393F")
    ax.set_facecolor("#2F3136")

    # Plot histogram with more granular bins (100-point bins)
    bin_width = 100
    min_rating = max(0, int(min(ratings_arr) // bin_width) * bin_width)
    max_rating = int(np.ceil(max(ratings_arr) / bin_width) * bin_width) + bin_width
    bins = np.arange(min_rating, max_rating + bin_width, bin_width)

    # Plot histogram (density=True to normalize for PDF overlay)
    n, bins_edges, patches = ax.hist(
        ratings_arr,
        bins=bins,
        density=True,
        alpha=0.7,
        color="#5865F2",
        edgecolor="#36393F",
        linewidth=0.5,
        label=f"Data (n={len(ratings)})",
    )

    # Fit and plot normal distribution curve
    x_range = np.linspace(min_rating, max_rating, 200)
    normal_pdf = stats.norm.pdf(x_range, mean, std)
    ax.plot(x_range, normal_pdf, color="#57F287", linewidth=2.5, label="Normal fit", linestyle="-")

    # Also show a kernel density estimate for comparison
    if len(ratings_arr) >= 5:
        kde = stats.gaussian_kde(ratings_arr)
        kde_pdf = kde(x_range)
        ax.plot(x_range, kde_pdf, color="#FEE75C", linewidth=2, label="KDE", linestyle="--", alpha=0.8)

    # Add vertical lines for mean and median
    ax.axvline(mean, color="#ED4245", linestyle="-", linewidth=1.5, alpha=0.8, label=f"Mean: {mean:.0f}")
    if median_rating is not None:
        ax.axvline(median_rating, color="#F47B67", linestyle="--", linewidth=1.5, alpha=0.8, label=f"Median: {median_rating:.0f}")

    # Style the plot
    ax.set_xlabel("Rating", color="#B9BBBE", fontsize=11)
    ax.set_ylabel("Density", color="#B9BBBE", fontsize=11)
    ax.tick_params(colors="#B9BBBE", labelsize=9)
    for spine in ax.spines.values():
        spine.set_color("#4F545C")

    # Title with stats
    title = f"Rating Distribution (n={len(ratings)})"
    ax.set_title(title, color="white", fontsize=13, fontweight="bold", pad=10)

    # Legend
    ax.legend(loc="upper right", facecolor="#2F3136", edgecolor="#4F545C", labelcolor="white", fontsize=8)

    # Add stats annotation box
    normality_text = ""
    if shapiro_p is not None:
        if shapiro_p > 0.05:
            normality_text = f"Normal (p={shapiro_p:.3f})"
        else:
            normality_text = f"Non-normal (p={shapiro_p:.3f})"

    stats_text = f"μ={mean:.0f}, σ={std:.0f}\nSkew={skewness:.2f}, Kurt={kurtosis:.2f}"
    if normality_text:
        stats_text += f"\n{normality_text}"

    ax.text(
        0.02, 0.98, stats_text,
        transform=ax.transAxes,
        fontsize=9,
        verticalalignment="top",
        color="#B9BBBE",
        family="monospace",
        bbox=dict(boxstyle="round,pad=0.3", facecolor="#2F3136", edgecolor="#4F545C", alpha=0.9),
    )

    plt.tight_layout()

    # Save to BytesIO
    fp = BytesIO()
    fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor="#36393F")
    plt.close(fig)
    fp.seek(0)
    return fp


def draw_calibration_curve(
    glicko_data: list[tuple[float, float, int]],
    openskill_data: list[tuple[float, float, int]],
) -> BytesIO:
    """
    Draw calibration curves comparing Glicko-2 and OpenSkill predictions.

    A well-calibrated system has points on the diagonal (predicted = actual).

    Args:
        glicko_data: List of (avg_predicted, actual_rate, count) tuples for Glicko-2
        openskill_data: List of (avg_predicted, actual_rate, count) tuples for OpenSkill

    Returns:
        BytesIO containing the PNG image
    """
    fig, ax = plt.subplots(figsize=(6.5, 5), facecolor=DISCORD_BG)
    ax.set_facecolor(DISCORD_DARKER)

    # Perfect calibration line
    ax.plot([0, 1], [0, 1], color=DISCORD_GREY, linestyle="--", linewidth=1.5,
            label="Perfect calibration", alpha=0.7)

    # Glicko-2 curve
    if glicko_data:
        g_predicted = [d[0] for d in glicko_data]
        g_actual = [d[1] for d in glicko_data]
        g_counts = [d[2] for d in glicko_data]
        # Size points by sample count
        sizes = [min(200, 20 + c * 3) for c in g_counts]
        ax.scatter(g_predicted, g_actual, s=sizes, c=DISCORD_ACCENT, alpha=0.8,
                   label="Glicko-2", edgecolors="white", linewidths=0.5)
        if len(g_predicted) > 1:
            ax.plot(g_predicted, g_actual, color=DISCORD_ACCENT, alpha=0.5, linewidth=1)

    # OpenSkill curve
    if openskill_data:
        o_predicted = [d[0] for d in openskill_data]
        o_actual = [d[1] for d in openskill_data]
        o_counts = [d[2] for d in openskill_data]
        sizes = [min(200, 20 + c * 3) for c in o_counts]
        ax.scatter(o_predicted, o_actual, s=sizes, c=DISCORD_GREEN, alpha=0.8,
                   label="OpenSkill", edgecolors="white", linewidths=0.5, marker="s")
        if len(o_predicted) > 1:
            ax.plot(o_predicted, o_actual, color=DISCORD_GREEN, alpha=0.5, linewidth=1)

    # Styling
    ax.set_xlabel("Predicted Win Probability", color=DISCORD_GREY, fontsize=11)
    ax.set_ylabel("Actual Win Rate", color=DISCORD_GREY, fontsize=11)
    ax.set_xlim(0, 1)
    ax.set_ylim(0, 1)
    ax.tick_params(colors=DISCORD_GREY, labelsize=9)
    for spine in ax.spines.values():
        spine.set_color("#4F545C")

    ax.set_title("Calibration Curve: Predicted vs Actual", color="white",
                 fontsize=13, fontweight="bold", pad=10)
    ax.legend(loc="lower right", facecolor=DISCORD_DARKER, edgecolor="#4F545C",
              labelcolor="white", fontsize=9)

    # Add grid for easier reading
    ax.grid(True, alpha=0.2, color=DISCORD_GREY)

    plt.tight_layout()

    fp = BytesIO()
    fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
    plt.close(fig)
    fp.seek(0)
    return fp


def draw_rating_comparison_chart(comparison_data: dict) -> BytesIO:
    """
    Draw a comparison chart showing Glicko-2 vs OpenSkill metrics.

    Args:
        comparison_data: Dict from RatingComparisonService.get_comparison_summary()

    Returns:
        BytesIO containing the PNG image
    """
    if "error" in comparison_data:
        # Return error image
        fig, ax = plt.subplots(figsize=(6.5, 4), facecolor=DISCORD_BG)
        ax.set_facecolor(DISCORD_DARKER)
        ax.text(0.5, 0.5, comparison_data["error"], ha="center", va="center",
                color="white", fontsize=14)
        ax.set_xticks([])
        ax.set_yticks([])
        fp = BytesIO()
        fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
        plt.close(fig)
        fp.seek(0)
        return fp

    glicko = comparison_data["glicko"]
    openskill = comparison_data["openskill"]

    fig, axes = plt.subplots(1, 3, figsize=(10, 4), facecolor=DISCORD_BG)

    metrics = [
        ("Brier Score\n(Lower = Better)", "brier_score", True),
        ("Accuracy\n(Higher = Better)", "accuracy", False),
        ("Log Loss\n(Lower = Better)", "log_loss", True),
    ]

    for ax, (title, key, lower_is_better) in zip(axes, metrics):
        ax.set_facecolor(DISCORD_DARKER)

        g_val = glicko[key]
        o_val = openskill[key]

        bars = ax.bar(
            ["Glicko-2", "OpenSkill"],
            [g_val, o_val],
            color=[DISCORD_ACCENT, DISCORD_GREEN],
            edgecolor="white",
            linewidth=0.5,
        )

        # Highlight winner
        if lower_is_better:
            winner_idx = 0 if g_val < o_val else 1
        else:
            winner_idx = 0 if g_val > o_val else 1
        bars[winner_idx].set_edgecolor(DISCORD_YELLOW)
        bars[winner_idx].set_linewidth(2)

        ax.set_title(title, color="white", fontsize=10, fontweight="bold")
        ax.tick_params(colors=DISCORD_GREY, labelsize=9)
        for spine in ax.spines.values():
            spine.set_color("#4F545C")

        # Add value labels on bars
        for bar, val in zip(bars, [g_val, o_val]):
            height = bar.get_height()
            ax.annotate(
                f"{val:.3f}",
                xy=(bar.get_x() + bar.get_width() / 2, height),
                xytext=(0, 3),
                textcoords="offset points",
                ha="center", va="bottom",
                color="white", fontsize=9,
            )

    fig.suptitle(
        f"Rating System Comparison ({comparison_data['matches_analyzed']} matches)",
        color="white", fontsize=12, fontweight="bold", y=1.02,
    )

    plt.tight_layout()

    fp = BytesIO()
    fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
    plt.close(fig)
    fp.seek(0)
    return fp


def draw_hero_performance_chart(
    hero_stats: list[dict],
    username: str,
    max_heroes: int = 8,
) -> BytesIO:
    """
    Generate a horizontal bar chart showing top heroes by games played.

    Bars are colored by winrate (green for high, red for low).

    Args:
        hero_stats: List of dicts with hero_id, games, wins (from get_player_hero_detailed_stats)
        username: Player's display name for title
        max_heroes: Maximum number of heroes to display (default 8)

    Returns:
        BytesIO containing the PNG image
    """
    from utils.hero_lookup import get_hero_name

    if not hero_stats:
        # Return empty image
        img = Image.new("RGBA", (450, 100), DISCORD_BG)
        draw = ImageDraw.Draw(img)
        font = _get_font(18)
        draw.text((20, 40), "No hero data available", fill=DISCORD_GREY, font=font)
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Limit heroes displayed
    stats = hero_stats[:max_heroes]

    # Image dimensions
    width = 450
    bar_height = 32
    padding = 15
    header_height = 35
    label_width = 110  # Space for hero name
    value_width = 70   # Space for winrate and games

    height = header_height + len(stats) * (bar_height + 6) + padding * 2

    # Create image
    img = Image.new("RGBA", (width, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Draw title
    title_font = _get_font(18)
    draw.text((padding, padding), f"Top Heroes: {username}", fill=DISCORD_WHITE, font=title_font)

    # Calculate max games for bar scaling
    max_games = max(s["games"] for s in stats) if stats else 1

    label_font = _get_font(14)
    value_font = _get_font(13)

    y = padding + header_height
    bar_max_width = width - padding * 2 - label_width - value_width - 10

    for stat in stats:
        hero_name = get_hero_name(stat["hero_id"])
        games = stat["games"]
        wins = stat["wins"]
        winrate = wins / games if games > 0 else 0

        # Truncate long hero names
        if len(hero_name) > 13:
            hero_name = hero_name[:11] + ".."

        # Draw hero name
        draw.text((padding, y + 8), hero_name, fill=DISCORD_WHITE, font=label_font)

        # Calculate bar dimensions
        bar_x = padding + label_width
        bar_fill_width = int(bar_max_width * games / max_games)
        bar_fill_width = max(bar_fill_width, 4)  # Minimum visible bar

        # Color based on winrate (gradient from red to green)
        if winrate >= 0.60:
            bar_color = DISCORD_GREEN
        elif winrate >= 0.50:
            # Yellow-green gradient
            bar_color = "#7CB342"  # Light green
        elif winrate >= 0.40:
            bar_color = DISCORD_YELLOW
        else:
            bar_color = DISCORD_RED

        # Draw bar background
        draw.rectangle(
            [(bar_x, y + 5), (bar_x + bar_max_width, y + bar_height - 5)],
            fill=DISCORD_DARKER,
        )

        # Draw bar fill
        draw.rectangle(
            [(bar_x, y + 5), (bar_x + bar_fill_width, y + bar_height - 5)],
            fill=bar_color,
        )

        # Draw winrate and games text
        wr_text = f"{winrate:.0%} ({games}g)"
        text_x = bar_x + bar_max_width + 8
        draw.text((text_x, y + 8), wr_text, fill=DISCORD_GREY, font=value_font)

        y += bar_height + 6

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp


def draw_prediction_over_time(match_data: list[dict], window: int = 20) -> BytesIO:
    """
    Draw rolling accuracy of predictions over time for both systems.

    Args:
        match_data: List of match dicts with prediction data (chronological)
        window: Rolling window size for smoothing

    Returns:
        BytesIO containing the PNG image
    """
    if len(match_data) < window:
        # Return error image
        fig, ax = plt.subplots(figsize=(8, 4), facecolor=DISCORD_BG)
        ax.set_facecolor(DISCORD_DARKER)
        ax.text(0.5, 0.5, f"Need at least {window} matches for trend analysis",
                ha="center", va="center", color="white", fontsize=14)
        ax.set_xticks([])
        ax.set_yticks([])
        fp = BytesIO()
        fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
        plt.close(fig)
        fp.seek(0)
        return fp

    fig, ax = plt.subplots(figsize=(8, 4), facecolor=DISCORD_BG)
    ax.set_facecolor(DISCORD_DARKER)

    # Calculate rolling accuracy
    n = len(match_data)
    glicko_rolling = []
    openskill_rolling = []
    x_vals = []

    for i in range(window, n + 1):
        window_data = match_data[i - window:i]
        g_correct = sum(1 for m in window_data if m["glicko_correct"])
        o_correct = sum(1 for m in window_data if m["openskill_correct"])
        glicko_rolling.append(g_correct / window)
        openskill_rolling.append(o_correct / window)
        x_vals.append(i)

    ax.plot(x_vals, glicko_rolling, color=DISCORD_ACCENT, linewidth=2,
            label=f"Glicko-2 ({window}-match rolling)")
    ax.plot(x_vals, openskill_rolling, color=DISCORD_GREEN, linewidth=2,
            label=f"OpenSkill ({window}-match rolling)")

    # 50% reference line (coin flip)
    ax.axhline(0.5, color=DISCORD_GREY, linestyle="--", alpha=0.5, label="Coin flip (50%)")

    ax.set_xlabel("Match Number", color=DISCORD_GREY, fontsize=11)
    ax.set_ylabel("Prediction Accuracy", color=DISCORD_GREY, fontsize=11)
    ax.set_ylim(0.3, 0.9)
    ax.tick_params(colors=DISCORD_GREY, labelsize=9)
    for spine in ax.spines.values():
        spine.set_color("#4F545C")

    ax.set_title("Prediction Accuracy Over Time", color="white",
                 fontsize=13, fontweight="bold", pad=10)
    ax.legend(loc="lower right", facecolor=DISCORD_DARKER, edgecolor="#4F545C",
              labelcolor="white", fontsize=9)
    ax.grid(True, alpha=0.2, color=DISCORD_GREY)

    plt.tight_layout()

    fp = BytesIO()
    fig.savefig(fp, format="PNG", dpi=100, bbox_inches="tight", facecolor=DISCORD_BG)
    plt.close(fig)
    fp.seek(0)
    return fp


def draw_hero_grid(
    grid_data: list[dict],
    player_names: dict[int, str],
    min_games: int = 2,
    title: str = "Hero Grid",
) -> BytesIO:
    """
    Generate a player x hero grid image with sized/colored circles.

    Rows are players (Y-axis), columns are heroes (X-axis).
    Circle size represents number of games played.
    Circle color represents win rate.

    Args:
        grid_data: List of dicts with discord_id, hero_id, games, wins
        player_names: Dict mapping discord_id -> display name (insertion order = row order)
        min_games: Minimum games on a hero (across any player) for it to appear as a column
        title: Title text for the image

    Returns:
        BytesIO containing the PNG image
    """
    from utils.hero_lookup import get_hero_short_name

    # Handle empty data
    if not grid_data or not player_names:
        img = Image.new("RGBA", (450, 100), DISCORD_BG)
        draw = ImageDraw.Draw(img)
        font = _get_font(18)
        draw.text((20, 40), "No hero data available", fill=DISCORD_GREY, font=font)
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # --- Data transformation ---
    # Pivot into {(discord_id, hero_id): (games, wins)}
    data = {}
    for row in grid_data:
        data[(row["discord_id"], row["hero_id"])] = (row["games"], row["wins"])

    # Determine player order from player_names key order, filtered to those with data
    player_ids = [pid for pid in player_names if any(
        pid == k[0] for k in data
    )]

    if not player_ids:
        img = Image.new("RGBA", (450, 100), DISCORD_BG)
        draw = ImageDraw.Draw(img)
        font = _get_font(18)
        draw.text((20, 40), "No hero data available", fill=DISCORD_GREY, font=font)
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Collect all hero_ids and compute per-hero max games across players
    hero_max_games: dict[int, int] = {}
    hero_total_games: dict[int, int] = {}
    for (pid, hid), (games, _wins) in data.items():
        if pid in player_names:
            hero_max_games[hid] = max(hero_max_games.get(hid, 0), games)
            hero_total_games[hid] = hero_total_games.get(hid, 0) + games

    # Filter heroes by min_games threshold
    hero_ids = [hid for hid, mx in hero_max_games.items() if mx >= min_games]

    if not hero_ids:
        img = Image.new("RGBA", (450, 100), DISCORD_BG)
        draw = ImageDraw.Draw(img)
        font = _get_font(18)
        draw.text((20, 40), "No heroes meet the minimum games threshold", fill=DISCORD_GREY, font=font)
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Sort heroes by total games descending (most popular first)
    hero_ids.sort(key=lambda hid: hero_total_games.get(hid, 0), reverse=True)

    # Cap heroes to keep image within Discord limits
    MAX_HEROES = 60
    CELL_SIZE = 44
    PLAYER_LABEL_WIDTH = 120
    HERO_LABEL_HEIGHT = 90
    PADDING = 15
    LEGEND_HEIGHT = 80
    TITLE_HEIGHT = 30
    MIN_CIRCLE_RADIUS = 4
    MAX_CIRCLE_RADIUS = 18
    LABEL_REPEAT_INTERVAL = 10

    num_players = len(player_ids)
    num_heroes_raw = len(hero_ids)

    # Compute repeat band/column counts for dimension calculations
    n_extra_bands = (num_players - 1) // LABEL_REPEAT_INTERVAL if num_players > LABEL_REPEAT_INTERVAL else 0
    n_extra_cols = (num_heroes_raw - 1) // LABEL_REPEAT_INTERVAL if num_heroes_raw > LABEL_REPEAT_INTERVAL else 0

    max_width = 3900
    extra_col_width = n_extra_cols * PLAYER_LABEL_WIDTH
    max_heroes_by_width = (max_width - PADDING * 2 - PLAYER_LABEL_WIDTH - extra_col_width) // CELL_SIZE
    num_heroes = min(num_heroes_raw, MAX_HEROES, max_heroes_by_width)
    hero_ids = hero_ids[:num_heroes]

    # Recompute extra columns after capping heroes
    n_extra_cols = (num_heroes - 1) // LABEL_REPEAT_INTERVAL if num_heroes > LABEL_REPEAT_INTERVAL else 0

    # --- Repeat-label coordinate helpers ---
    def _count_bands_before(player_idx: int) -> int:
        """Number of hero-header repeat bands above this player row."""
        if num_players <= LABEL_REPEAT_INTERVAL:
            return 0
        return player_idx // LABEL_REPEAT_INTERVAL

    def _count_cols_before(hero_idx: int) -> int:
        """Number of player-label repeat columns left of this hero column."""
        if num_heroes <= LABEL_REPEAT_INTERVAL:
            return 0
        return hero_idx // LABEL_REPEAT_INTERVAL

    def _player_row_y(player_idx: int) -> int:
        return grid_top + player_idx * CELL_SIZE + _count_bands_before(player_idx) * HERO_LABEL_HEIGHT

    def _hero_col_x(hero_idx: int) -> int:
        return PADDING + PLAYER_LABEL_WIDTH + hero_idx * CELL_SIZE + _count_cols_before(hero_idx) * PLAYER_LABEL_WIDTH

    # --- Image dimensions ---
    extra_band_height = n_extra_bands * HERO_LABEL_HEIGHT
    extra_col_width = n_extra_cols * PLAYER_LABEL_WIDTH
    width = PADDING + PLAYER_LABEL_WIDTH + num_heroes * CELL_SIZE + extra_col_width + PADDING
    height = PADDING + TITLE_HEIGHT + HERO_LABEL_HEIGHT + num_players * CELL_SIZE + extra_band_height + LEGEND_HEIGHT + PADDING

    grid_top = PADDING + TITLE_HEIGHT + HERO_LABEL_HEIGHT

    img = Image.new("RGBA", (width, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Global max games for circle scaling
    max_games_all = max(
        (data.get((pid, hid), (0, 0))[0] for pid in player_ids for hid in hero_ids),
        default=1,
    )
    max_games_all = max(max_games_all, 1)

    # --- Draw title ---
    title_font = _get_font(18)
    draw.text((PADDING, PADDING), title, fill=DISCORD_WHITE, font=title_font)

    # --- Helper: draw hero column headers at a given y_bottom ---
    label_font = _get_font(11)

    def _draw_hero_headers(y_bottom: int) -> None:
        for hero_idx, hero_id in enumerate(hero_ids):
            hero_name = get_hero_short_name(hero_id)
            tw, th = _get_text_size(label_font, hero_name)
            txt_img = Image.new("RGBA", (tw + 4, th + 4), (0, 0, 0, 0))
            txt_draw = ImageDraw.Draw(txt_img)
            txt_draw.text((2, 2), hero_name, fill=DISCORD_WHITE, font=label_font)
            rotated = txt_img.rotate(45, expand=True, resample=Image.BICUBIC)

            col_center_x = _hero_col_x(hero_idx) + CELL_SIZE // 2
            paste_x = col_center_x - rotated.width // 2
            paste_y = y_bottom - rotated.height - 2
            img.paste(rotated, (paste_x, paste_y), rotated)

    # --- Helper: draw player row labels at a given x_left ---
    name_font = _get_font(13)

    def _draw_player_labels(x_left: int) -> None:
        for player_idx, pid in enumerate(player_ids):
            row_y = _player_row_y(player_idx)
            name = player_names.get(pid, f"Player {pid}")
            if len(name) > 14:
                name = name[:12] + ".."
            _tw, th = _get_text_size(name_font, name)
            text_y = row_y + (CELL_SIZE - th) // 2
            draw.text((x_left, text_y), name, fill=DISCORD_WHITE, font=name_font)

    # --- Draw player row backgrounds (alternating) — must come before labels ---
    grid_right = _hero_col_x(num_heroes - 1) + CELL_SIZE if num_heroes > 0 else PADDING + PLAYER_LABEL_WIDTH

    for player_idx, pid in enumerate(player_ids):
        row_y = _player_row_y(player_idx)

        # Alternating row background
        if player_idx % 2 == 1:
            draw.rectangle(
                [(PADDING + PLAYER_LABEL_WIDTH, row_y),
                 (grid_right, row_y + CELL_SIZE)],
                fill=DISCORD_DARKER,
            )

    # --- Draw original hero column headers ---
    _draw_hero_headers(grid_top)

    # --- Draw repeat hero header bands ---
    for band_idx in range(1, n_extra_bands + 1):
        # This band appears just above player row (band_idx * LABEL_REPEAT_INTERVAL)
        band_player_idx = band_idx * LABEL_REPEAT_INTERVAL
        band_y_bottom = _player_row_y(band_player_idx)
        _draw_hero_headers(band_y_bottom)

    # --- Draw original player row labels ---
    _draw_player_labels(PADDING)

    grid_bottom = _player_row_y(num_players - 1) + CELL_SIZE if num_players > 0 else grid_top

    # --- Draw subtle grid lines ---
    grid_color = "#2A2D33"
    # Vertical lines (hero columns)
    for hero_idx in range(num_heroes + 1):
        if hero_idx < num_heroes:
            x = _hero_col_x(hero_idx)
        else:
            x = _hero_col_x(num_heroes - 1) + CELL_SIZE
        draw.line([(x, grid_top), (x, grid_bottom)], fill=grid_color)
    # Horizontal lines (player rows)
    grid_left = PADDING + PLAYER_LABEL_WIDTH
    for player_idx in range(num_players + 1):
        if player_idx < num_players:
            y = _player_row_y(player_idx)
        else:
            y = _player_row_y(num_players - 1) + CELL_SIZE
        draw.line(
            [(grid_left, y), (grid_right, y)],
            fill=grid_color,
        )

    # --- Draw circles ---
    count_font = _get_font(10)
    for player_idx, pid in enumerate(player_ids):
        for hero_idx, hid in enumerate(hero_ids):
            key = (pid, hid)
            if key not in data:
                continue

            games, wins = data[key]
            if games <= 0:
                continue

            winrate = wins / games

            # Circle radius: sqrt scaling so area is proportional to games
            t = min(games / max_games_all, 1.0)
            radius = MIN_CIRCLE_RADIUS + (MAX_CIRCLE_RADIUS - MIN_CIRCLE_RADIUS) * math.sqrt(t)
            radius = int(round(radius))

            # Color by winrate
            if winrate >= 0.60:
                color = DISCORD_GREEN
            elif winrate >= 0.50:
                color = "#7CB342"
            elif winrate >= 0.40:
                color = DISCORD_YELLOW
            else:
                color = DISCORD_RED

            cx = _hero_col_x(hero_idx) + CELL_SIZE // 2
            cy = _player_row_y(player_idx) + CELL_SIZE // 2

            draw.ellipse(
                [(cx - radius, cy - radius), (cx + radius, cy + radius)],
                fill=color,
            )

            # Draw game count inside larger circles
            if radius >= 10:
                count_text = str(games)
                ctw, cth = _get_text_size(count_font, count_text)
                draw.text(
                    (cx - ctw // 2, cy - cth // 2),
                    count_text,
                    fill=DISCORD_BG,
                    font=count_font,
                )

    # --- Draw repeat player label columns (on top of everything) ---
    separator_color = "#4E5058"
    for col_idx in range(1, n_extra_cols + 1):
        col_hero_idx = col_idx * LABEL_REPEAT_INTERVAL
        col_x_left = _hero_col_x(col_hero_idx) - PLAYER_LABEL_WIDTH
        # Solid background to clear everything in this column
        draw.rectangle(
            [(col_x_left, grid_top), (col_x_left + PLAYER_LABEL_WIDTH - 1, grid_bottom)],
            fill=DISCORD_BG,
        )
        _draw_player_labels(col_x_left)
        # Vertical separator lines on left and right edges
        draw.line(
            [(col_x_left - 1, grid_top), (col_x_left - 1, grid_bottom)],
            fill=separator_color, width=2,
        )
        draw.line(
            [(col_x_left + PLAYER_LABEL_WIDTH, grid_top),
             (col_x_left + PLAYER_LABEL_WIDTH, grid_bottom)],
            fill=separator_color, width=2,
        )

    # --- Draw legend ---
    legend_y = grid_bottom + 25
    legend_font = _get_font(11)

    # Size legend
    draw.text((PADDING, legend_y), "Size = games:", fill=DISCORD_GREY, font=legend_font)
    size_x = PADDING + 90
    for label, example_t in [("few", 0.05), ("some", 0.25), ("many", 1.0)]:
        r = int(round(MIN_CIRCLE_RADIUS + (MAX_CIRCLE_RADIUS - MIN_CIRCLE_RADIUS) * math.sqrt(example_t)))
        cy = legend_y + 8
        draw.ellipse(
            [(size_x - r, cy - r), (size_x + r, cy + r)],
            fill=DISCORD_GREY,
        )
        lw, _lh = _get_text_size(legend_font, label)
        draw.text((size_x + r + 6, legend_y), label, fill=DISCORD_GREY, font=legend_font)
        size_x += r + 6 + lw + 22

    # Color legend
    legend_y2 = legend_y + 45
    draw.text((PADDING, legend_y2), "Color = WR:", fill=DISCORD_GREY, font=legend_font)
    color_x = PADDING + 82
    for label, clr in [("\u226560%", DISCORD_GREEN), ("\u226550%", "#7CB342"),
                        ("\u226540%", DISCORD_YELLOW), ("<40%", DISCORD_RED)]:
        r = 6
        cy = legend_y2 + 7
        draw.ellipse([(color_x - r, cy - r), (color_x + r, cy + r)], fill=clr)
        lw, _lh = _get_text_size(legend_font, label)
        draw.text((color_x + r + 3, legend_y2), label, fill=DISCORD_GREY, font=legend_font)
        color_x += r + 3 + lw + 14

    # --- Save ---
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp


# -------------------------------------------------------------------------
# Hero Image Caching for Scout Report
# -------------------------------------------------------------------------

# Module-level cache for hero images
_hero_image_cache: dict[int, Image.Image] = {}


def _fetch_hero_image(hero_id: int, size: tuple[int, int] = (48, 27)) -> Image.Image | None:
    """
    Fetch hero image from Steam CDN with caching.

    Args:
        hero_id: Dota 2 hero ID
        size: Target size (width, height) for the image

    Returns:
        PIL Image resized to specified dimensions, or None if fetch fails
    """
    import requests
    from utils.hero_lookup import get_hero_image_url

    # Check cache first
    cache_key = hero_id
    if cache_key in _hero_image_cache:
        cached = _hero_image_cache[cache_key]
        # Resize if needed
        if cached.size != size:
            return cached.resize(size, Image.Resampling.LANCZOS)
        return cached

    # Fetch from CDN
    url = get_hero_image_url(hero_id)
    if not url:
        return None

    try:
        response = requests.get(url, timeout=5)
        response.raise_for_status()
        img = Image.open(BytesIO(response.content)).convert("RGBA")
        # Cache the original
        _hero_image_cache[cache_key] = img
        # Return resized
        return img.resize(size, Image.Resampling.LANCZOS)
    except Exception:
        return None


def _get_hero_images_batch(hero_ids: list[int], size: tuple[int, int] = (48, 27)) -> dict[int, Image.Image]:
    """
    Fetch multiple hero images, using cache where available.

    Args:
        hero_ids: List of hero IDs to fetch
        size: Target size for images

    Returns:
        Dict mapping hero_id -> PIL Image
    """
    result = {}
    for hero_id in hero_ids:
        img = _fetch_hero_image(hero_id, size)
        if img:
            result[hero_id] = img
    return result


# Role colors for scout report (Dota 2 positions 1-5)
POSITION_COLORS = {
    1: "#FF9800",  # Orange - Carry
    2: "#9C27B0",  # Purple - Mid
    3: "#4CAF50",  # Green - Offlane
    4: "#00BCD4",  # Cyan - Soft Support
    5: "#2196F3",  # Blue - Hard Support
}


def _lerp_color(c1: tuple, c2: tuple, t: float) -> tuple:
    """Linearly interpolate between two RGB tuples."""
    t = max(0.0, min(1.0, t))
    return tuple(int(c1[i] + (c2[i] - c1[i]) * t) for i in range(3))


def _heatmap_contest_rate(rate: float) -> tuple:
    """Heatmap for contest rate: grey (0%) -> amber (50%) -> red (100%)."""
    rate = max(0.0, min(1.0, rate))
    if rate < 0.5:
        return _lerp_color((150, 150, 150), (255, 180, 50), rate / 0.5)
    else:
        return _lerp_color((255, 180, 50), (255, 60, 60), (rate - 0.5) / 0.5)


def _heatmap_winrate(rate: float) -> tuple:
    """Heatmap for win rate: red (0%) -> yellow (50%) -> green (100%)."""
    rate = max(0.0, min(1.0, rate))
    if rate < 0.5:
        return _lerp_color((255, 60, 60), (255, 220, 50), rate / 0.5)
    else:
        return _lerp_color((255, 220, 50), (80, 220, 80), (rate - 0.5) / 0.5)


def draw_scout_report(
    scout_data: dict,
    player_names: list[str],
    title: str = "SCOUT REPORT",
) -> BytesIO:
    """
    Generate a visual scouting report with hero portraits.

    Shows aggregated hero stats for a team/group of players in a compact
    table format with hero portrait images.

    Layout (~320px width, mobile-friendly):
    +----------------------------------+
    |        SCOUT REPORT              |
    | Player1, Player2, ...            |
    +----------------------------------+
    | Hero  Tot  CR%  W-L  WR%         |
    +----------------------------------+
    | [IMG]  25  85%  18-7  72%        |
    | [IMG]  20  68%  12-8  60%        |
    | ...                              |
    +----------------------------------+

    Columns: Hero | Tot (W+L+Bans) | CR% (contest rate) | W-L | WR% (win rate)
    CR% and WR% use heatmap coloring.

    Args:
        scout_data: Dict from get_scout_data() with player_count, total_matches, and heroes list
        player_names: List of player display names (for header)
        title: Title text for the report

    Returns:
        BytesIO containing the PNG image
    """
    heroes = scout_data.get("heroes", [])
    total_matches = scout_data.get("total_matches", 0)

    # Handle empty data
    if not heroes:
        img = Image.new("RGBA", (320, 100), DISCORD_BG)
        draw = ImageDraw.Draw(img)
        font = _get_font(16)
        draw.text((20, 40), "No hero data available", fill=DISCORD_GREY, font=font)
        fp = BytesIO()
        img.save(fp, format="PNG")
        fp.seek(0)
        return fp

    # Dimensions
    WIDTH = 320
    PADDING = 12
    HEADER_HEIGHT = 50
    ROW_HEIGHT = 32
    HERO_IMG_WIDTH = 48
    HERO_IMG_HEIGHT = 27

    # Calculate height based on number of heroes
    num_heroes = len(heroes)
    height = PADDING + HEADER_HEIGHT + (num_heroes * ROW_HEIGHT) + PADDING

    # Create image
    img = Image.new("RGBA", (WIDTH, height), DISCORD_BG)
    draw = ImageDraw.Draw(img)

    # Fonts
    title_font = _get_font(14)
    player_font = _get_font(11)
    stat_font = _get_font(13)

    # --- Draw header ---
    # Title
    title_w = _get_text_size(title_font, title)[0]
    draw.text(((WIDTH - title_w) // 2, PADDING), title, fill=DISCORD_WHITE, font=title_font)

    # Player names (truncated)
    if player_names:
        names_text = ", ".join(player_names[:5])
        if len(player_names) > 5:
            names_text += f" +{len(player_names) - 5}"
        # Truncate if too long
        max_name_len = 38
        if len(names_text) > max_name_len:
            names_text = names_text[:max_name_len - 2] + ".."
        names_w = _get_text_size(player_font, names_text)[0]
        draw.text(
            ((WIDTH - names_w) // 2, PADDING + 22),
            names_text,
            fill=DISCORD_GREY,
            font=player_font,
        )

    # Fixed column positions for alignment
    # Layout: Hero | Tot | CR% | W-L | WR%
    COL_HERO_X = PADDING + 4
    COL_TOTAL_X = COL_HERO_X + HERO_IMG_WIDTH + 8   # 72
    COL_CR_X = COL_TOTAL_X + 34                      # 106
    COL_WL_X = COL_CR_X + 40                         # 146
    COL_WR_X = COL_WL_X + 50                         # 196

    # Column headers
    header_font = _get_font(11)
    header_y = PADDING + HEADER_HEIGHT - 18
    draw.text((COL_TOTAL_X, header_y), "Tot", fill=DISCORD_GREY, font=header_font)
    draw.text((COL_CR_X, header_y), "CR", fill=DISCORD_GREY, font=header_font)
    draw.text((COL_WL_X, header_y), "W-L", fill=DISCORD_GREY, font=header_font)
    draw.text((COL_WR_X, header_y), "WR", fill=DISCORD_GREY, font=header_font)

    # Header separator line
    sep_y = PADDING + HEADER_HEIGHT - 5
    draw.line([(PADDING, sep_y), (WIDTH - PADDING, sep_y)], fill=DISCORD_ACCENT, width=1)

    # --- Fetch hero images ---
    hero_ids = [h["hero_id"] for h in heroes]
    hero_images = _get_hero_images_batch(hero_ids, (HERO_IMG_WIDTH, HERO_IMG_HEIGHT))

    # --- Draw hero rows ---
    y = PADDING + HEADER_HEIGHT

    for i, hero in enumerate(heroes):
        hero_id = hero["hero_id"]
        wins = hero["wins"]
        losses = hero["losses"]
        bans = hero.get("bans", 0)
        games = wins + losses
        total = games + bans

        # Contest rate and win rate
        contest_rate = total / total_matches if total_matches > 0 else 0.0
        win_rate = wins / games if games > 0 else 0.0

        # Alternate row background
        if i % 2 == 1:
            draw.rectangle(
                [(PADDING, y), (WIDTH - PADDING, y + ROW_HEIGHT)],
                fill=DISCORD_DARKER,
            )

        stat_y = y + (ROW_HEIGHT - 15) // 2

        # Hero portrait (fixed position)
        hero_img = hero_images.get(hero_id)
        if hero_img:
            img_y = y + (ROW_HEIGHT - HERO_IMG_HEIGHT) // 2
            img.paste(hero_img, (COL_HERO_X, img_y), hero_img)
        else:
            img_y = y + (ROW_HEIGHT - HERO_IMG_HEIGHT) // 2
            draw.rectangle(
                [(COL_HERO_X, img_y), (COL_HERO_X + HERO_IMG_WIDTH, img_y + HERO_IMG_HEIGHT)],
                fill=DISCORD_DARKER,
                outline=DISCORD_GREY,
            )

        # Total count (right-aligned)
        total_text = str(total)
        tw = _get_text_size(stat_font, total_text)[0]
        draw.text(
            (max(COL_TOTAL_X, COL_TOTAL_X + 25 - tw), stat_y),
            total_text,
            fill=DISCORD_WHITE,
            font=stat_font,
        )

        # Contest rate % (heatmap colored, right-aligned)
        cr_text = f"{contest_rate * 100:.0f}%"
        cr_w = _get_text_size(stat_font, cr_text)[0]
        cr_color = _heatmap_contest_rate(contest_rate)
        draw.text(
            (max(COL_CR_X, COL_CR_X + 32 - cr_w), stat_y),
            cr_text,
            fill=cr_color,
            font=stat_font,
        )

        # W-L (green wins, grey dash, red losses)
        w_text = str(wins)
        dash_text = "-"
        l_text = str(losses)
        wl_full = f"{wins}-{losses}"
        wl_w = _get_text_size(stat_font, wl_full)[0]
        wl_x = max(COL_WL_X, COL_WL_X + 40 - wl_w)

        w_width = _get_text_size(stat_font, w_text)[0]
        dash_width = _get_text_size(stat_font, dash_text)[0]

        draw.text((wl_x, stat_y), w_text, fill=DISCORD_GREEN, font=stat_font)
        draw.text((wl_x + w_width, stat_y), dash_text, fill=DISCORD_GREY, font=stat_font)
        draw.text((wl_x + w_width + dash_width, stat_y), l_text, fill=DISCORD_RED, font=stat_font)

        # Win rate % (heatmap colored, right-aligned)
        if games > 0:
            wr_text = f"{win_rate * 100:.0f}%"
            wr_color = _heatmap_winrate(win_rate)
        else:
            wr_text = "-"
            wr_color = DISCORD_GREY
        wr_w = _get_text_size(stat_font, wr_text)[0]
        draw.text(
            (max(COL_WR_X, COL_WR_X + 32 - wr_w), stat_y),
            wr_text,
            fill=wr_color,
            font=stat_font,
        )

        y += ROW_HEIGHT

    # Save to BytesIO
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp
