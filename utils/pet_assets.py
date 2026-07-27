"""
Asset loader for cama pet art.

Fallback chain for pet cards:
  1. Full-card override on disk    (assets/pets/{species}_{stage}_{mood}.png)
  2. Hybrid layer composite        (pet_compositor: disk component packs
                                    wired together, procedural per missing slot)
  3. Fully procedural pixel art    (pet_drawing, if compositing itself fails)
  4. None                          (caller sends the embed without an image)

discord.File objects are single-use (the buffer is consumed on send), so
we cache raw *bytes* and mint a fresh File each call. Filenames are
deterministic so embeds can reference them via attachment:// URLs.
"""

from __future__ import annotations

import io
import logging
from pathlib import Path

import discord
from PIL import Image, ImageDraw

from utils.fonts import get_font

logger = logging.getLogger(__name__)

ASSETS_DIR = Path(__file__).resolve().parent.parent / "assets" / "pets"

_MAX_FILE_SIZE = 8 * 1024 * 1024  # 8 MB Discord limit

# Module-level byte cache: disk path or render key -> bytes. Bounded in
# practice: disk entries by the finite asset set, rendered entries by the
# live pet roster (seed is stable per pet) and the names of dead pets.
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


def _engrave_tombstone(data: bytes, name: str) -> bytes:
    """Write the memorial inscription onto the shipped blank tombstone."""
    with Image.open(io.BytesIO(data)) as source:
        image = source.convert("RGBA")
    draw = ImageDraw.Draw(image)
    name_font = get_font(20)
    shown = name.strip() or "Unnamed"
    max_width = 132
    if draw.textlength(shown, font=name_font) > max_width:
        while (
            len(shown) > 1
            and draw.textlength(shown + "...", font=name_font) > max_width
        ):
            shown = shown[:-1]
        shown += "..."
    for text, font, y in (("R.I.P.", get_font(26, bold=True), 126), (shown, name_font, 164)):
        width = draw.textlength(text, font=font)
        x = (image.width - width) // 2
        draw.text((x + 1, y + 1), text, font=font, fill=(190, 188, 210, 180))
        draw.text((x, y), text, font=font, fill=(64, 61, 83, 235))
    rendered = io.BytesIO()
    image.save(rendered, format="PNG", optimize=True)
    return rendered.getvalue()


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def get_pet_card(
    species_id: str, stage: str, mood: str, seed: int, accessory: str | None = None
) -> discord.File | None:
    """Return a discord.File for a pet portrait card.

    Fallback chain: full-card file on disk → hybrid layer composite →
    fully procedural render → None.
    Full-card naming: assets/pets/{species_id}_{stage}_{mood}.png (or .gif)
    """
    base_name = f"{species_id}_{stage}_{mood}"

    # 1. Full-card override on disk
    asset_path = _find_asset(ASSETS_DIR, base_name)
    if asset_path:
        data = _load_cached_bytes(asset_path)
        if data:
            return _file_from_bytes(data, f"pet_{base_name}{asset_path.suffix}")

    # 2. Hybrid composite (disk component packs + procedural per slot).
    # Cache key carries the discovered-component token so tests (and any
    # future hot-reload) never serve composites from a stale manifest.
    try:
        from utils import pet_compositor
        cache_key = (
            f"compose_{base_name}_{seed}_{accessory}_"
            f"{pet_compositor.manifest_token()}"
        )
        data = _bytes_cache.get(cache_key)
        if data is None:
            data = pet_compositor.compose_pet_card(
                species_id, stage, mood, seed, accessory=accessory
            ).getvalue()
            _bytes_cache[cache_key] = data
        return _file_from_bytes(data, f"pet_{base_name}.png")
    except Exception as e:
        logger.warning("Pet card compositing failed, using pure fallback: %s", e)

    # 3. Fully procedural last resort
    try:
        cache_key = f"render_{base_name}_{seed}_{accessory}"
        data = _bytes_cache.get(cache_key)
        if data is None:
            from utils.pet_drawing import render_pet_card
            data = render_pet_card(
                species_id, stage, mood, seed, accessory=accessory
            ).getvalue()
            _bytes_cache[cache_key] = data
        return _file_from_bytes(data, f"pet_{base_name}.png")
    except Exception as e:
        logger.debug("PIL pet card fallback failed: %s", e)
        return None


def get_egg_card(seed: int) -> discord.File | None:
    """Return a discord.File for the unhatched-egg card.

    Fallback chain: custom file on disk → PIL pixel art → None.
    """
    asset_path = _find_asset(ASSETS_DIR, "egg")
    if asset_path:
        data = _load_cached_bytes(asset_path)
        if data:
            return _file_from_bytes(data, f"pet_egg{asset_path.suffix}")

    try:
        cache_key = f"render_egg_{seed}"
        data = _bytes_cache.get(cache_key)
        if data is None:
            from utils.pet_drawing import render_egg_card
            data = render_egg_card(seed).getvalue()
            _bytes_cache[cache_key] = data
        return _file_from_bytes(data, "pet_egg.png")
    except Exception as e:
        logger.debug("PIL egg card fallback failed: %s", e)
        return None


def get_tombstone_card(name: str, seed: int) -> discord.File | None:
    """Return a discord.File for a pet tombstone card.

    Fallback chain: custom file on disk → PIL pixel art → None.
    The procedural render engraves ``name``, so its cache keys on the name.
    """
    asset_path = _find_asset(ASSETS_DIR, "tombstone")
    if asset_path:
        data = _load_cached_bytes(asset_path)
        if data:
            cache_key = f"engraved_tombstone_{asset_path}_{name}"
            engraved = _bytes_cache.get(cache_key)
            if engraved is None:
                try:
                    engraved = _engrave_tombstone(data, name)
                except (OSError, ValueError) as exc:
                    logger.warning("Tombstone engraving failed, using fallback: %s", exc)
                else:
                    _bytes_cache[cache_key] = engraved
            if engraved is not None:
                return _file_from_bytes(engraved, f"pet_tombstone{asset_path.suffix}")

    try:
        cache_key = f"render_tombstone_{name}"
        data = _bytes_cache.get(cache_key)
        if data is None:
            from utils.pet_drawing import render_tombstone_card
            data = render_tombstone_card(name).getvalue()
            _bytes_cache[cache_key] = data
        return _file_from_bytes(data, "pet_tombstone.png")
    except Exception as e:
        logger.debug("PIL tombstone card fallback failed: %s", e)
        return None
