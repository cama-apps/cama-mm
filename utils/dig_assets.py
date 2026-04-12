"""
Asset loader for dig minigame visual art.

Fallback chain:
  1. Custom art file on disk  (assets/dig/...)
  2. PIL-generated pixel art   (dig_drawing helpers)
  3. None                      (caller falls back to ASCII or no image)

discord.File objects are single-use (the buffer is consumed on send), so
we cache raw *bytes* and mint a fresh File each call.
"""

from __future__ import annotations

import io
import logging
from pathlib import Path

import discord
from PIL import Image

logger = logging.getLogger(__name__)

ASSETS_DIR = Path(__file__).resolve().parent.parent / "assets" / "dig"

_MAX_FILE_SIZE = 8 * 1024 * 1024  # 8 MB Discord limit

LAYER_SLUGS: dict[str, str] = {
    "Dirt": "dirt",
    "Stone": "stone",
    "Crystal": "crystal",
    "Magma": "magma",
    "Abyss": "abyss",
    "Fungal Depths": "fungal_depths",
    "Frozen Core": "frozen_core",
    "The Hollow": "the_hollow",
}

#: Asset-filename slug for each boss depth boundary. Lives here (not in
#: ``services.dig_constants``) so the utils → services layering stays clean.
BOSS_SLUGS: dict[int, str] = {
    25: "grothak", 50: "crystalia", 75: "magmus",
    100: "void_warden", 150: "sporeling", 200: "chronofrost", 275: "nameless",
}

#: Asset-filename slug for each pickaxe tier index (0 = wooden, 6 = void_touched).
PICKAXE_SLUGS: list[str] = [
    "wooden", "stone", "iron", "diamond", "obsidian", "frostforged", "void_touched",
]

# Module-level byte cache:  path-string -> bytes
# Bounded by the finite asset set (~29 entries: 21 boss + 8 layer images).
_bytes_cache: dict[str, bytes] = {}


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

def _find_asset(directory: Path, base_name: str) -> Path | None:
    """Return the first matching asset file (.gif then .png), or None."""
    for ext in ("gif", "png"):
        p = directory / f"{base_name}.{ext}"
        if p.is_file() and p.stat().st_size <= _MAX_FILE_SIZE:
            return p
    return None


def _load_cached_bytes(path: Path) -> bytes | None:
    """Load file bytes, caching for reuse."""
    key = str(path)
    if key in _bytes_cache:
        return _bytes_cache[key]
    try:
        data = path.read_bytes()
        if len(data) > _MAX_FILE_SIZE:
            return None
        _bytes_cache[key] = data
        return data
    except OSError:
        return None


def _file_from_bytes(data: bytes, filename: str) -> discord.File:
    """Create a fresh discord.File from cached bytes."""
    return discord.File(io.BytesIO(data), filename=filename)


def _file_from_buf(buf: io.BytesIO, filename: str) -> discord.File:
    """Create a discord.File from a PIL BytesIO buffer."""
    return discord.File(buf, filename=filename)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def get_boss_art(
    boundary: int,
    scene: str,
    layer_name: str,
) -> discord.File | None:
    """Return a discord.File for boss art (encounter / victory / defeat).

    Fallback chain: custom file on disk → PIL pixel art → None.
    """
    slug = BOSS_SLUGS.get(boundary)
    if not slug:
        return None

    base_name = f"{slug}_{scene}"

    # 1. Custom art on disk
    asset_path = _find_asset(ASSETS_DIR / "bosses", base_name)
    if asset_path:
        data = _load_cached_bytes(asset_path)
        if data:
            ext = asset_path.suffix  # .png or .gif
            return _file_from_bytes(data, f"boss_{scene}{ext}")

    # 2. PIL fallback
    try:
        if scene == "encounter":
            from utils.dig_drawing import draw_boss_scene
            buf = draw_boss_scene(layer_name, slug)
        else:
            from utils.dig_drawing import draw_boss_result_scene
            buf = draw_boss_result_scene(layer_name, slug, won=(scene == "victory"))
        return _file_from_buf(buf, f"boss_{scene}.png")
    except Exception as e:
        logger.debug("PIL boss art fallback failed: %s", e)
        return None


def get_layer_thumbnail(layer_name: str) -> discord.File | None:
    """Return a discord.File for a layer thumbnail.

    Fallback chain: custom file on disk → PIL pixel art → None.
    """
    slug = LAYER_SLUGS.get(layer_name)
    if not slug:
        return None

    # 1. Custom art on disk
    asset_path = _find_asset(ASSETS_DIR / "layers", slug)
    if asset_path:
        data = _load_cached_bytes(asset_path)
        if data:
            ext = asset_path.suffix
            return _file_from_bytes(data, f"layer_{slug}{ext}")

    # 2. PIL fallback
    try:
        from utils.dig_drawing import draw_layer_thumbnail
        buf = draw_layer_thumbnail(layer_name)
        return _file_from_buf(buf, f"layer_{slug}.png")
    except Exception as e:
        logger.debug("PIL layer thumbnail fallback failed: %s", e)
        return None


def get_event_art(event_id: str, layer_name: str) -> discord.File | None:
    """Return a discord.File for event art.

    Fallback chain: custom file on disk → PIL pixel art → None.
    Naming: assets/dig/events/{event_id}.png (or .gif)
    """
    # 1. Custom art on disk
    asset_path = _find_asset(ASSETS_DIR / "events", event_id)
    if asset_path:
        data = _load_cached_bytes(asset_path)
        if data:
            ext = asset_path.suffix
            return _file_from_bytes(data, f"event_{event_id}{ext}")

    # 2. PIL fallback
    try:
        from utils.dig_drawing import draw_event_scene, has_event_scene
        if has_event_scene(event_id):
            buf = draw_event_scene(layer_name, event_id)
            return _file_from_buf(buf, f"event_{event_id}.png")
    except Exception as e:
        logger.debug("PIL event art fallback failed: %s", e)
    return None


def get_item_art(item_id: str) -> discord.File | None:
    """Return a discord.File for an item icon. No PIL fallback."""
    asset_path = _find_asset(ASSETS_DIR / "items", item_id)
    if asset_path:
        data = _load_cached_bytes(asset_path)
        if data:
            ext = asset_path.suffix
            return _file_from_bytes(data, f"item_{item_id}{ext}")
    return None


def get_pickaxe_art(tier_index: int) -> discord.File | None:
    """Return a discord.File for a pickaxe tier icon. No PIL fallback."""
    if tier_index < 0 or tier_index >= len(PICKAXE_SLUGS):
        return None
    slug = PICKAXE_SLUGS[tier_index]
    asset_path = _find_asset(ASSETS_DIR / "pickaxes", slug)
    if asset_path:
        data = _load_cached_bytes(asset_path)
        if data:
            ext = asset_path.suffix
            return _file_from_bytes(data, f"pickaxe_{slug}{ext}")
    return None


def _load_icon_image(directory: Path, base_name: str) -> Image.Image | None:
    """Load an icon as a PIL Image for composition, with byte caching."""
    asset_path = _find_asset(directory, base_name)
    if not asset_path:
        return None
    data = _load_cached_bytes(asset_path)
    if not data:
        return None
    return Image.open(io.BytesIO(data)).convert("RGBA")


def compose_items_used(item_ids: list[str]) -> discord.File | None:
    """Compose consumed item icons into a compact horizontal strip for a thumbnail."""
    if not item_ids:
        return None

    icon_size = 48
    gap = 4
    icons: list[Image.Image] = []
    for item_id in item_ids:
        img = _load_icon_image(ASSETS_DIR / "items", item_id)
        if img:
            icons.append(img.resize((icon_size, icon_size), Image.Resampling.LANCZOS))

    if not icons:
        return None

    total_w = len(icons) * icon_size + (len(icons) - 1) * gap
    strip = Image.new("RGBA", (total_w, icon_size), (0, 0, 0, 0))
    x = 0
    for ic in icons:
        strip.paste(ic, (x, 0), ic)
        x += ic.width + gap

    buf = io.BytesIO()
    strip.save(buf, format="PNG", optimize=True)
    buf.seek(0)
    return _file_from_buf(buf, "items_used.png")


_SHOP_ITEM_IDS = [
    "dynamite", "hard_hat", "lantern", "reinforcement", "torch",
    "grappling_hook", "sonar_pulse", "depth_charge", "void_bait",
]


def compose_shop_grid() -> discord.File | None:
    """Compose a 3x3 grid of all shop item icons for the shop embed."""
    icon_size = 80
    gap = 6
    cols, rows = 3, 3
    grid_w = cols * icon_size + (cols - 1) * gap
    grid_h = rows * icon_size + (rows - 1) * gap
    grid = Image.new("RGBA", (grid_w, grid_h), (0, 0, 0, 0))

    placed = 0
    for item_id in _SHOP_ITEM_IDS:
        img = _load_icon_image(ASSETS_DIR / "items", item_id)
        if not img:
            continue
        img = img.resize((icon_size, icon_size), Image.Resampling.LANCZOS)
        row, col = divmod(placed, cols)
        x = col * (icon_size + gap)
        y = row * (icon_size + gap)
        grid.paste(img, (x, y), img)
        placed += 1

    if not placed:
        return None

    buf = io.BytesIO()
    grid.save(buf, format="PNG", optimize=True)
    buf.seek(0)
    return _file_from_buf(buf, "shop_grid.png")
