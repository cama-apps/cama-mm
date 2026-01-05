"""
Image generation utilities for Dota 2 stats visualization.
"""

import math
from io import BytesIO

from PIL import Image, ImageDraw, ImageFont

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

    # Draw bet markers
    max_bet_size = max(b["effective_bet"] for b in bet_infos) if bet_infos else 1
    for bet_num, pnl, info in pnl_series:
        px, py = to_pixel(bet_num, pnl)

        # Size based on bet amount (3-8 pixels)
        size = 3 + int((info["effective_bet"] / max_bet_size) * 5)

        # Color based on outcome
        if info["outcome"] == "won":
            color = DISCORD_GREEN
        else:
            color = DISCORD_RED

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
