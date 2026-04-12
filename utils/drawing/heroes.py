"""Hero-centric renderers (performance chart, grid, image helpers)."""

from __future__ import annotations

import math
from io import BytesIO

from PIL import Image, ImageDraw

from utils.drawing._common import (
    DISCORD_BG,
    DISCORD_DARKER,
    DISCORD_GREEN,
    DISCORD_GREY,
    DISCORD_RED,
    DISCORD_WHITE,
    DISCORD_YELLOW,
    _get_font,
    _get_text_size,
)


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


