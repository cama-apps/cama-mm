"""Hero-centric renderers (performance chart, grid, image helpers)."""

from __future__ import annotations

import logging
import math
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from io import BytesIO
from pathlib import Path

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

logger = logging.getLogger(__name__)


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


# --- Hero grid layout constants ---
_GRID_MAX_HEROES = 60
_GRID_CELL_SIZE = 44
_GRID_PLAYER_LABEL_WIDTH = 120
_GRID_HERO_LABEL_HEIGHT = 90
_GRID_PADDING = 15
_GRID_LEGEND_HEIGHT = 80
_GRID_TITLE_HEIGHT = 30
_GRID_MIN_CIRCLE_RADIUS = 4
_GRID_MAX_CIRCLE_RADIUS = 18
_GRID_LABEL_REPEAT_INTERVAL = 10
_GRID_MAX_WIDTH = 3900


@dataclass(frozen=True)
class _HeroGridLayout:
    """Computed geometry for a hero grid render.

    Holds the capped hero list, image dimensions, repeat-band/column counts,
    and the four repeat-label coordinate helpers used throughout drawing.
    """

    hero_ids: list[int]
    num_players: int
    num_heroes: int
    n_extra_bands: int
    n_extra_cols: int
    width: int
    height: int
    grid_top: int
    count_bands_before: Callable[[int], int]
    count_cols_before: Callable[[int], int]
    player_row_y: Callable[[int], int]
    hero_col_x: Callable[[int], int]


def _compute_hero_grid_layout(
    player_ids: list[int],
    hero_ids: list[int],
) -> _HeroGridLayout:
    """Compute hero-grid geometry: hero capping, dimensions, coordinate helpers.

    Args:
        player_ids: Ordered player ids that will become grid rows.
        hero_ids: Heroes sorted most-popular-first (pre-capping).

    Returns:
        A frozen :class:`_HeroGridLayout` with the capped hero list, image
        dimensions, repeat band/column counts, and coordinate helper closures.
    """
    num_players = len(player_ids)
    num_heroes_raw = len(hero_ids)

    # Compute repeat band/column counts for dimension calculations
    n_extra_bands = (
        (num_players - 1) // _GRID_LABEL_REPEAT_INTERVAL
        if num_players > _GRID_LABEL_REPEAT_INTERVAL
        else 0
    )
    n_extra_cols = (
        (num_heroes_raw - 1) // _GRID_LABEL_REPEAT_INTERVAL
        if num_heroes_raw > _GRID_LABEL_REPEAT_INTERVAL
        else 0
    )

    extra_col_width = n_extra_cols * _GRID_PLAYER_LABEL_WIDTH
    max_heroes_by_width = (
        _GRID_MAX_WIDTH - _GRID_PADDING * 2 - _GRID_PLAYER_LABEL_WIDTH - extra_col_width
    ) // _GRID_CELL_SIZE
    num_heroes = min(num_heroes_raw, _GRID_MAX_HEROES, max_heroes_by_width)
    hero_ids = hero_ids[:num_heroes]

    # Recompute extra columns after capping heroes
    n_extra_cols = (
        (num_heroes - 1) // _GRID_LABEL_REPEAT_INTERVAL
        if num_heroes > _GRID_LABEL_REPEAT_INTERVAL
        else 0
    )

    # --- Image dimensions ---
    extra_band_height = n_extra_bands * _GRID_HERO_LABEL_HEIGHT
    extra_col_width = n_extra_cols * _GRID_PLAYER_LABEL_WIDTH
    width = (
        _GRID_PADDING + _GRID_PLAYER_LABEL_WIDTH + num_heroes * _GRID_CELL_SIZE
        + extra_col_width + _GRID_PADDING
    )
    height = (
        _GRID_PADDING + _GRID_TITLE_HEIGHT + _GRID_HERO_LABEL_HEIGHT
        + num_players * _GRID_CELL_SIZE + extra_band_height
        + _GRID_LEGEND_HEIGHT + _GRID_PADDING
    )

    grid_top = _GRID_PADDING + _GRID_TITLE_HEIGHT + _GRID_HERO_LABEL_HEIGHT

    # --- Repeat-label coordinate helpers ---
    def _count_bands_before(player_idx: int) -> int:
        """Number of hero-header repeat bands above this player row."""
        if num_players <= _GRID_LABEL_REPEAT_INTERVAL:
            return 0
        return player_idx // _GRID_LABEL_REPEAT_INTERVAL

    def _count_cols_before(hero_idx: int) -> int:
        """Number of player-label repeat columns left of this hero column."""
        if num_heroes <= _GRID_LABEL_REPEAT_INTERVAL:
            return 0
        return hero_idx // _GRID_LABEL_REPEAT_INTERVAL

    def _player_row_y(player_idx: int) -> int:
        return (
            grid_top + player_idx * _GRID_CELL_SIZE
            + _count_bands_before(player_idx) * _GRID_HERO_LABEL_HEIGHT
        )

    def _hero_col_x(hero_idx: int) -> int:
        return (
            _GRID_PADDING + _GRID_PLAYER_LABEL_WIDTH + hero_idx * _GRID_CELL_SIZE
            + _count_cols_before(hero_idx) * _GRID_PLAYER_LABEL_WIDTH
        )

    return _HeroGridLayout(
        hero_ids=hero_ids,
        num_players=num_players,
        num_heroes=num_heroes,
        n_extra_bands=n_extra_bands,
        n_extra_cols=n_extra_cols,
        width=width,
        height=height,
        grid_top=grid_top,
        count_bands_before=_count_bands_before,
        count_cols_before=_count_cols_before,
        player_row_y=_player_row_y,
        hero_col_x=_hero_col_x,
    )


def _draw_grid_size_legend(draw: ImageDraw.ImageDraw, legend_y: int) -> None:
    """Draw the "Size = games" circle-size legend row at ``legend_y``."""
    legend_font = _get_font(11)
    draw.text((_GRID_PADDING, legend_y), "Size = games:", fill=DISCORD_GREY, font=legend_font)
    size_x = _GRID_PADDING + 90
    for label, example_t in [("few", 0.05), ("some", 0.25), ("many", 1.0)]:
        r = int(round(
            _GRID_MIN_CIRCLE_RADIUS
            + (_GRID_MAX_CIRCLE_RADIUS - _GRID_MIN_CIRCLE_RADIUS) * math.sqrt(example_t)
        ))
        cy = legend_y + 8
        draw.ellipse(
            [(size_x - r, cy - r), (size_x + r, cy + r)],
            fill=DISCORD_GREY,
        )
        lw, _lh = _get_text_size(legend_font, label)
        draw.text((size_x + r + 6, legend_y), label, fill=DISCORD_GREY, font=legend_font)
        size_x += r + 6 + lw + 22


def _draw_grid_color_legend(draw: ImageDraw.ImageDraw, legend_y2: int) -> None:
    """Draw the "Color = WR" winrate-color legend row at ``legend_y2``."""
    legend_font = _get_font(11)
    draw.text((_GRID_PADDING, legend_y2), "Color = WR:", fill=DISCORD_GREY, font=legend_font)
    color_x = _GRID_PADDING + 82
    for label, clr in [("≥60%", DISCORD_GREEN), ("≥50%", "#7CB342"),
                        ("≥40%", DISCORD_YELLOW), ("<40%", DISCORD_RED)]:
        r = 6
        cy = legend_y2 + 7
        draw.ellipse([(color_x - r, cy - r), (color_x + r, cy + r)], fill=clr)
        lw, _lh = _get_text_size(legend_font, label)
        draw.text((color_x + r + 3, legend_y2), label, fill=DISCORD_GREY, font=legend_font)
        color_x += r + 3 + lw + 14


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

    # --- Compute layout geometry (hero capping, dimensions, coordinate helpers) ---
    layout = _compute_hero_grid_layout(player_ids, hero_ids)
    hero_ids = layout.hero_ids
    num_players = layout.num_players
    num_heroes = layout.num_heroes
    n_extra_bands = layout.n_extra_bands
    n_extra_cols = layout.n_extra_cols
    width = layout.width
    height = layout.height
    grid_top = layout.grid_top
    _player_row_y = layout.player_row_y
    _hero_col_x = layout.hero_col_x

    # Drawing-stage aliases for the shared grid constants
    CELL_SIZE = _GRID_CELL_SIZE
    PLAYER_LABEL_WIDTH = _GRID_PLAYER_LABEL_WIDTH
    PADDING = _GRID_PADDING
    MIN_CIRCLE_RADIUS = _GRID_MIN_CIRCLE_RADIUS
    MAX_CIRCLE_RADIUS = _GRID_MAX_CIRCLE_RADIUS
    LABEL_REPEAT_INTERVAL = _GRID_LABEL_REPEAT_INTERVAL

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

    # --- Draw legends ---
    legend_y = grid_bottom + 25
    _draw_grid_size_legend(draw, legend_y)
    _draw_grid_color_legend(draw, legend_y + 45)

    # --- Save ---
    fp = BytesIO()
    img.save(fp, format="PNG")
    fp.seek(0)
    return fp


# -------------------------------------------------------------------------
# Hero Image Caching for Scout Report
# -------------------------------------------------------------------------

# Original-size decoded images stay hot in memory, while the on-disk copy
# survives process restarts and avoids repeat CDN requests after deploys.
_hero_image_cache: dict[int, Image.Image] = {}
_HERO_IMAGE_DISK_CACHE_DIR = Path(".cache/scout/heroes")
_HERO_IMAGE_FETCH_WORKERS = 4


def _hero_image_disk_path(hero_id: int) -> Path:
    """Return the deterministic path for one original hero image."""
    return _HERO_IMAGE_DISK_CACHE_DIR / f"{hero_id}.png"


def _resized_hero_image(image: Image.Image, size: tuple[int, int]) -> Image.Image:
    """Return ``image`` at the requested size without mutating the cache."""
    if image.size == size:
        return image
    return image.resize(size, Image.Resampling.LANCZOS)


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

    cached = _hero_image_cache.get(hero_id)
    if cached is not None:
        return _resized_hero_image(cached, size)

    disk_path = _hero_image_disk_path(hero_id)
    if disk_path.exists():
        try:
            with Image.open(disk_path) as source:
                image = source.convert("RGBA")
                image.load()
            _hero_image_cache[hero_id] = image
            return _resized_hero_image(image, size)
        except Exception:
            # A partial/corrupt file should fall through to a fresh CDN copy.
            logger.debug(
                "Failed to load cached hero image for hero_id=%s",
                hero_id,
                exc_info=True,
            )

    # Fetch from CDN
    url = get_hero_image_url(hero_id)
    if not url:
        return None

    try:
        response = requests.get(url, timeout=5)
        response.raise_for_status()
        with Image.open(BytesIO(response.content)) as source:
            img = source.convert("RGBA")
            img.load()
        # Cache the original. Intentionally retained (not closed) for the
        # lifetime of the process so repeat lookups reuse the decoded image.
        _hero_image_cache[hero_id] = img
        try:
            disk_path.parent.mkdir(parents=True, exist_ok=True)
            disk_path.write_bytes(response.content)
        except OSError:
            # The memory cache is still useful in read-only deployments.
            logger.debug(
                "Failed to persist hero image for hero_id=%s",
                hero_id,
                exc_info=True,
            )
        return _resized_hero_image(img, size)
    except Exception:
        logger.debug("Failed to fetch hero image for hero_id=%s", hero_id, exc_info=True)
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
    unique_ids = list(dict.fromkeys(hero_ids))
    resolved: dict[int, Image.Image] = {}
    missing: list[int] = []
    for hero_id in unique_ids:
        cached = _hero_image_cache.get(hero_id)
        if cached is None:
            missing.append(hero_id)
        else:
            resolved[hero_id] = _resized_hero_image(cached, size)

    if missing:
        workers = min(_HERO_IMAGE_FETCH_WORKERS, len(missing))
        with ThreadPoolExecutor(max_workers=workers) as executor:
            images = executor.map(
                lambda hero_id: _fetch_hero_image(hero_id, size),
                missing,
            )
            for hero_id, image in zip(missing, images, strict=True):
                if image is not None:
                    resolved[hero_id] = image

    # Preserve the first-occurrence ordering of the old sequential path.
    return {hero_id: resolved[hero_id] for hero_id in unique_ids if hero_id in resolved}
