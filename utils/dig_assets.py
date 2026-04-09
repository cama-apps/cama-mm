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

from services.dig_constants import BOSS_SLUGS

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
