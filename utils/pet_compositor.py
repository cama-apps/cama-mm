"""Hybrid layer compositor for cama pet cards.

Wires pre-generated component images together with procedural drawing.
Each layer slot (utils.pet_drawing.SLOT_ORDER) resolves independently:

  1. species-scoped disk component   components/{stage}/{slot}/{species}_*.png
  2. shared disk component (tinted)  components/{stage}/{slot}/any_*.png
  3. procedural drawing of the slot  (pet_drawing.render_layer)

so component packs can land one slot at a time: an AI-generated creature
body over a procedural face, or a procedural body under an AI backdrop —
whatever exists on disk wins its slot, everything else stays code-drawn.

Component contract (also documented in prompts.md):
  - Full-canvas 512x288 RGBA PNGs with transparency; layers are stacked
    with alpha_composite in slot order, so registration is simply "draw
    the part where it belongs on the canvas".
  - The `face` slot is mood-keyed: {scope}_{mood}_{variant}.png with mood
    in happy/neutral/hungry. Other slots are mood-independent.
  - `any`-scoped parts in tintable slots should be authored in neutral
    grayscale; they are remapped to the species palette at compose time.
  - Multiple variants per scope are encouraged: the pet's id seeds which
    variant an individual pet gets, so every pet looks subtly unique.

Directory listings and file bytes are cached for the process lifetime
(same policy as utils/pet_assets.py — new component packs need a restart).
"""

from __future__ import annotations

import logging
from pathlib import Path

from PIL import Image

from utils.pet_drawing import (
    _DEFAULT_PALETTE,
    SLOT_ORDER,
    SPECIES_PALETTES,
    _slot_rng,
    assemble_card,
    render_layer,
)

logger = logging.getLogger(__name__)

COMPONENTS_DIR = Path(__file__).resolve().parent.parent / "assets" / "pets" / "components"

# Slots whose shared ("any"-scoped) parts get species-palette tinting.
TINTED_SLOTS = {"back", "creature", "detail"}
# Slots whose components are keyed by mood.
MOOD_SLOTS = {"face"}

_pool_cache: dict[tuple[str, str], list[Path]] = {}
_image_cache: dict[str, Image.Image] = {}


def clear_caches() -> None:
    """Test hook: forget discovered components and decoded images."""
    _pool_cache.clear()
    _image_cache.clear()


def _pool(stage: str, slot: str) -> list[Path]:
    key = (stage, slot)
    if key not in _pool_cache:
        directory = COMPONENTS_DIR / stage / slot
        try:
            _pool_cache[key] = sorted(directory.glob("*.png")) if directory.is_dir() else []
        except OSError:
            _pool_cache[key] = []
    return _pool_cache[key]


def manifest_token() -> int:
    """Stable token for the currently discovered component set, for use in
    byte-cache keys so composites re-render when the manifest differs."""
    names = []
    for stage in ("baby", "adult"):
        for slot in SLOT_ORDER:
            names.extend(p.name for p in _pool(stage, slot))
    return hash(tuple(names))


def _pick_component(
    slot: str, stage: str, species_id: str, mood: str, seed: int
) -> tuple[Path, bool] | None:
    """Choose a component for a slot: (path, needs_tint) or None.

    Species-scoped parts beat shared parts; within a scope the pet's seed
    picks the variant deterministically.
    """
    pool = _pool(stage, slot)
    if not pool:
        return None
    if slot in MOOD_SLOTS:
        species_prefix, shared_prefix = f"{species_id}_{mood}_", f"any_{mood}_"
    else:
        species_prefix, shared_prefix = f"{species_id}_", "any_"
    rng = _slot_rng(seed, f"pick:{slot}")
    scoped = [p for p in pool if p.name.startswith(species_prefix)]
    if scoped:
        return rng.choice(scoped), False
    shared = [p for p in pool if p.name.startswith(shared_prefix)]
    if shared:
        return rng.choice(shared), slot in TINTED_SLOTS
    return None


def _load_component(path: Path) -> Image.Image | None:
    key = str(path)
    if key not in _image_cache:
        try:
            _image_cache[key] = Image.open(path).convert("RGBA")
        except OSError as exc:
            logger.warning("Unreadable pet component %s: %s", path, exc)
            return None
    return _image_cache[key]


def _tint_layer(img: Image.Image, species_id: str) -> Image.Image:
    """Remap a neutral/grayscale part's luminance onto the species palette
    (dark -> mid -> light ramp), preserving alpha."""
    dark, mid, light, _accent = SPECIES_PALETTES.get(species_id, _DEFAULT_PALETTE)

    def ramp(channel: int) -> list[int]:
        lut = []
        for value in range(256):
            if value < 128:
                t = value / 127
                lo, hi = dark[channel], mid[channel]
            else:
                t = (value - 128) / 127
                lo, hi = mid[channel], light[channel]
            lut.append(round(lo + (hi - lo) * t))
        return lut

    luminance = img.convert("L")
    bands = tuple(luminance.point(ramp(c)) for c in range(3))
    return Image.merge("RGBA", (*bands, img.getchannel("A")))


def compose_pet_card(species_id: str, stage: str, mood: str, seed: int):
    """Compose a pet card from disk components with procedural fallback
    per slot. Returns io.BytesIO of the finished PNG."""
    layers = []
    for slot in SLOT_ORDER:
        layer: Image.Image | None = None
        picked = _pick_component(slot, stage, species_id, mood, seed)
        if picked is not None:
            path, needs_tint = picked
            component = _load_component(path)
            if component is not None:
                layer = _tint_layer(component, species_id) if needs_tint else component
        if layer is None:
            layer = render_layer(slot, species_id, stage, mood, seed)
        layers.append(layer)
    return assemble_card(layers)
