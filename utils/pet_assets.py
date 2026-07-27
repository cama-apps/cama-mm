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


def _compose_versus(left_data: bytes, right_data: bytes) -> bytes:
    """Two pet cards face-to-face (right mirrored) with a VS slash."""
    with Image.open(io.BytesIO(left_data)) as left_src:
        left = left_src.convert("RGBA")
    with Image.open(io.BytesIO(right_data)) as right_src:
        right = right_src.convert("RGBA").transpose(Image.FLIP_LEFT_RIGHT)
    height = max(left.height, right.height)
    canvas = Image.new("RGBA", (left.width + right.width, height), (24, 20, 34, 255))
    canvas.alpha_composite(left, (0, (height - left.height) // 2))
    canvas.alpha_composite(right, (left.width, (height - right.height) // 2))
    draw = ImageDraw.Draw(canvas)
    font = get_font(64, bold=True)
    text = "VS"
    width = draw.textlength(text, font=font)
    x = (canvas.width - int(width)) // 2
    y = (height - 64) // 2
    for dx, dy in ((-2, -2), (2, -2), (-2, 2), (2, 2)):
        draw.text((x + dx, y + dy), text, font=font, fill=(20, 16, 28, 255))
    draw.text((x, y), text, font=font, fill=(255, 214, 68, 255))
    rendered = io.BytesIO()
    canvas.save(rendered, format="PNG", optimize=True)
    return rendered.getvalue()


def get_versus_card(
    species_a: str,
    stage_a: str,
    seed_a: int,
    accessory_a: str | None,
    species_b: str,
    stage_b: str,
    seed_b: int,
    accessory_b: str | None,
) -> discord.File | None:
    """Return a wide face-off splash for a brawl challenge, or None.

    Composes the two pets' regular card renders; if either render is
    unavailable the caller falls back to an embed without art.
    """
    try:
        cache_key = (
            f"versus_{species_a}_{stage_a}_{seed_a}_{accessory_a}"
            f"_{species_b}_{stage_b}_{seed_b}_{accessory_b}"
        )
        data = _bytes_cache.get(cache_key)
        if data is None:
            left = get_pet_card(
                species_a, stage_a, "happy", seed_a, accessory=accessory_a
            )
            right = get_pet_card(
                species_b, stage_b, "happy", seed_b, accessory=accessory_b
            )
            if left is None or right is None:
                return None
            data = _compose_versus(left.fp.read(), right.fp.read())
            _bytes_cache[cache_key] = data
        return _file_from_bytes(data, "pet_versus.png")
    except Exception as e:
        logger.warning("Versus card composition failed: %s", e)
        return None


def get_altar_card(name: str, seed: int) -> discord.File | None:
    """Sacrifice-rite art: authored altar backdrop, else the tombstone.

    An authored ``assets/pets/altar.png`` (or .gif) takes precedence when
    present; until it ships, the engraved tombstone keeps the rite somber.
    """
    asset_path = _find_asset(ASSETS_DIR, "altar")
    if asset_path:
        data = _load_cached_bytes(asset_path)
        if data:
            return _file_from_bytes(data, f"pet_altar{asset_path.suffix}")
    return get_tombstone_card(name, seed)


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
